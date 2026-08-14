use serde::{Deserialize, Serialize};

use crate::spec::CpuSpec;
use crate::types::Reading;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Ok,
    Degraded,
    OutOfSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictKind {
    /// Sustained all-core frequency below the vendor-guaranteed base clock.
    BelowBaseClock,
    /// The hardware is asserting a throttle signal.
    Throttling,
    /// Package power outside the rated or configurable TDP band.
    PowerOutsideBand,
    /// Close to the junction temperature limit.
    ThermalHeadroomLow,
}

/// A single judgement about machine state.
///
/// `basis` records where the threshold came from and is not decorative. It
/// exists so the honesty rule survives into the user interface: if a verdict
/// cannot say what authority it rests on, it should not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub severity: Severity,
    pub detail: String,
    pub basis: String,
}

/// Headroom below TjMax, in degrees Celsius, at which headroom is called low.
///
/// This is not a health threshold and must not be presented as one. Modern
/// parts are designed to run at their thermal limit under load, and AMD states
/// this explicitly. It marks the point at which thermal headroom has stopped
/// being available, which is information, not a fault.
const THERMAL_HEADROOM_LOW_C: f32 = 5.0;

/// Evaluate every absolute check that the available data supports.
///
/// `spec` is optional because OEM-exclusive parts frequently have no published
/// specification. When it is absent, checks that depend on published figures
/// are skipped rather than guessed, and the checks that read straight from the
/// chip continue to work.
pub fn evaluate(reading: &Reading, spec: Option<&CpuSpec>) -> Vec<Verdict> {
    let mut verdicts = Vec::new();

    if reading.cpu.throttle.asserted {
        let reason = match reading.cpu.throttle.reason {
            Some(r) => format!("{r:?}"),
            None => "unspecified".to_string(),
        };
        verdicts.push(Verdict {
            kind: VerdictKind::Throttling,
            severity: Severity::OutOfSpec,
            detail: format!("The hardware is asserting a throttle signal. Reason: {reason}."),
            basis: "Hardware status bit, read directly. Not inferred from temperature.".to_string(),
        });
    }

    if let (Some(temp), Some(tjmax)) = (reading.cpu.package_temp_c, reading.cpu.tjmax_c) {
        let headroom = tjmax - temp;
        if headroom <= THERMAL_HEADROOM_LOW_C {
            verdicts.push(Verdict {
                kind: VerdictKind::ThermalHeadroomLow,
                severity: Severity::Degraded,
                detail: format!(
                    "{headroom:.1} degrees C of headroom below the limit of {tjmax:.0}."
                ),
                basis: "TjMax as reported by the processor or the specification database. Running at the limit is by design on modern parts.".to_string(),
            });
        }
    }

    let Some(spec) = spec else {
        return verdicts;
    };

    if let Some(mhz) = reading.cpu.all_core_mhz {
        if mhz < spec.base_mhz {
            verdicts.push(Verdict {
                kind: VerdictKind::BelowBaseClock,
                severity: Severity::OutOfSpec,
                detail: format!(
                    "All cores are sustaining {mhz} MHz against a guaranteed base of {} MHz.",
                    spec.base_mhz
                ),
                basis: format!(
                    "{} publishes {} MHz as the base clock, which the vendor guarantees at the rated TDP.",
                    spec.name, spec.base_mhz
                ),
            });
        }
    }

    if let Some(watts) = reading.cpu.package_watts {
        let ceiling = spec.ctdp_max_watts.unwrap_or(spec.tdp_watts) as f32;
        if watts > ceiling {
            verdicts.push(Verdict {
                kind: VerdictKind::PowerOutsideBand,
                severity: Severity::Degraded,
                detail: format!(
                    "Package power is {watts:.1} W against a ceiling of {ceiling:.0} W."
                ),
                basis: "Rated TDP and configurable TDP band as published for this model."
                    .to_string(),
            });
        }
    }

    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CpuSpec, Vendor};
    use crate::types::{CpuReading, Reading, StallSignal, ThrottleReason, ThrottleState};

    fn spec() -> CpuSpec {
        CpuSpec {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
            name: "AMD Ryzen 7 4980U".to_string(),
            base_mhz: 2000,
            boost_mhz: Some(4400),
            tdp_watts: 15,
            ctdp_min_watts: Some(10),
            ctdp_max_watts: Some(25),
            tjmax_c: Some(105.0),
            cores: 8,
            threads: 16,
        }
    }

    fn reading(cpu: CpuReading) -> Reading {
        Reading {
            timestamp_ms: 0,
            stall: StallSignal {
                cpu: 0.0,
                memory: 0.0,
                io: 0.0,
            },
            cpu,
            processes: vec![],
        }
    }

    fn healthy_cpu() -> CpuReading {
        CpuReading {
            all_core_mhz: Some(2400),
            package_watts: Some(15.0),
            package_temp_c: Some(70.0),
            tjmax_c: Some(105.0),
            throttle: ThrottleState {
                asserted: false,
                reason: None,
            },
        }
    }

    #[test]
    fn a_healthy_machine_produces_no_verdicts() {
        let verdicts = evaluate(&reading(healthy_cpu()), Some(&spec()));
        assert!(verdicts.is_empty(), "got {verdicts:?}");
    }

    #[test]
    fn sustained_clock_below_guaranteed_base_is_out_of_spec() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1600);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::BelowBaseClock)
            .expect("must flag below base clock");
        assert_eq!(v.severity, Severity::OutOfSpec);
    }

    #[test]
    fn clock_at_exactly_base_is_within_spec() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(2000);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(!verdicts
            .iter()
            .any(|v| v.kind == VerdictKind::BelowBaseClock));
    }

    #[test]
    fn an_asserted_throttle_is_reported_with_its_reason() {
        let mut cpu = healthy_cpu();
        cpu.throttle = ThrottleState {
            asserted: true,
            reason: Some(ThrottleReason::Thermal),
        };
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts.iter().any(|v| v.kind == VerdictKind::Throttling));
    }

    #[test]
    fn power_above_the_configurable_band_is_flagged() {
        let mut cpu = healthy_cpu();
        cpu.package_watts = Some(31.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts
            .iter()
            .any(|v| v.kind == VerdictKind::PowerOutsideBand));
    }

    #[test]
    fn low_thermal_headroom_is_degraded_not_out_of_spec() {
        let mut cpu = healthy_cpu();
        cpu.package_temp_c = Some(103.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::ThermalHeadroomLow)
            .expect("must flag low headroom");
        assert_eq!(
            v.severity,
            Severity::Degraded,
            "running at the thermal limit is by design on modern parts and is not a fault"
        );
    }

    #[test]
    fn without_a_spec_only_chip_sourced_verdicts_are_produced() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(800);
        cpu.throttle = ThrottleState {
            asserted: true,
            reason: Some(ThrottleReason::Power),
        };
        let verdicts = evaluate(&reading(cpu), None);
        assert!(verdicts.iter().any(|v| v.kind == VerdictKind::Throttling));
        assert!(
            !verdicts
                .iter()
                .any(|v| v.kind == VerdictKind::BelowBaseClock),
            "base clock cannot be judged without a published guarantee"
        );
    }

    #[test]
    fn missing_optional_readings_produce_no_verdicts_rather_than_panicking() {
        let cpu = CpuReading {
            all_core_mhz: None,
            package_watts: None,
            package_temp_c: None,
            tjmax_c: None,
            throttle: ThrottleState {
                asserted: false,
                reason: None,
            },
        };
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts.is_empty());
    }

    #[test]
    fn every_verdict_names_the_authority_its_threshold_rests_on() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1600);
        cpu.package_watts = Some(31.0);
        cpu.package_temp_c = Some(103.0);
        cpu.throttle = ThrottleState {
            asserted: true,
            reason: Some(ThrottleReason::Thermal),
        };
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert_eq!(verdicts.len(), 4, "all four checks should fire");
        for v in &verdicts {
            assert!(
                !v.basis.trim().is_empty(),
                "{:?} has no basis, which the honesty rule forbids",
                v.kind
            );
        }
    }
}
