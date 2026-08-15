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
    /// Package power below what the part is rated to draw, while under load.
    ///
    /// Separate from [`Self::PowerOutsideBand`] because it is a different
    /// event with a different remedy. Drawing too much is the chip exceeding
    /// its envelope; drawing too little under full load means the platform is
    /// not supplying what the processor is rated for, and the fix is outside
    /// the machine rather than inside it.
    PowerBelowRating,
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

/// How much room is left below the vendor's own limit.
///
/// # Why this is not a normal range
///
/// LoadBear does not invent one, and this is not one. Chassis, ambient and
/// cooling account for something like twenty degrees of variance that has
/// nothing to do with the processor, which is why no vendor publishes a normal
/// range and why a fixed "70 is warm, 90 is hot" scale is wrong on some
/// machines and useless on others.
///
/// What every vendor does publish is TjMax, the junction temperature the part
/// is specified to reach. Distance below that is a real quantity with a real
/// authority behind it, and it rescales itself for a part specified to 105 and
/// a part specified to 90 without anybody choosing a number.
///
/// [`ThermalBand::AtLimit`] begins exactly where [`VerdictKind::ThermalHeadroomLow`]
/// fires, so a tile turning red and a finding appearing are the same event
/// rather than two thresholds that can disagree.
///
/// Running at the limit is by design on modern parts. `AtLimit` means headroom
/// has been used up, which is information, not a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThermalBand {
    /// Plenty of room below the specified limit.
    Normal,
    /// Warming, still comfortably inside the specification.
    Warm,
    /// Approaching the limit.
    Hot,
    /// Headroom is gone. The same point the thermal verdict fires at.
    AtLimit,
}

/// Headroom above which nothing is remarkable.
const HEADROOM_NORMAL_C: f32 = 25.0;
/// Headroom above which the part is merely warm.
const HEADROOM_WARM_C: f32 = 15.0;

/// Which band a reading falls in, or `None` when the part publishes no limit.
///
/// `None` is a real answer and the interface shows it as an uncoloured tile.
/// AMD parts frequently report TjMax as zero from every source available, and
/// colouring against a guess would be exactly the invented range this avoids.
pub fn thermal_band(temp_c: f32, tjmax_c: Option<f32>) -> Option<ThermalBand> {
    let tjmax = tjmax_c?;
    if tjmax <= 0.0 {
        return None;
    }
    let headroom = tjmax - temp_c;
    Some(if headroom > HEADROOM_NORMAL_C {
        ThermalBand::Normal
    } else if headroom > HEADROOM_WARM_C {
        ThermalBand::Warm
    } else if headroom > THERMAL_HEADROOM_LOW_C {
        ThermalBand::Hot
    } else {
        ThermalBand::AtLimit
    })
}

/// Utilization below which the base clock guarantee cannot be tested.
///
/// This is not a threshold on a fault, and it can only ever make LoadBear
/// quieter. It exists because of how the underlying measurement is defined:
/// the all-core frequency is averaged across every logical processor including
/// idle ones, and an idle processor is clocked down because that is correct
/// behaviour rather than a failure. On a half-idle machine the average is
/// dragged below base by cores doing nothing wrong, so a `BelowBaseClock`
/// verdict there would be reporting power management working as designed.
///
/// Only when nearly every processor is being asked for work does the average
/// reflect what the working cores actually sustain, which is the thing the
/// vendor guarantees. The figure is deliberately conservative: the correctness
/// bar treats a confident wrong verdict as worse than a missed one.
const MIN_UTILIZATION_FOR_BASE_CLOCK_PCT: f32 = 80.0;

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

    // Utilization is required rather than optional here. Absent, the machine
    // cannot be said to be asking for performance, and the check is skipped so
    // that a backend which cannot measure demand yet stays silent instead of
    // flagging every idle machine it sees.
    let under_demand = reading
        .cpu
        .utilization_pct
        .is_some_and(|u| u >= MIN_UTILIZATION_FOR_BASE_CLOCK_PCT);

    // The base clock comes from the specification database when the processor
    // is in it, and from the machine itself when it is not. Reaching it only
    // through the database meant the strongest verdict LoadBear has stayed
    // silent on every processor nobody had hand-entered, which is nearly all
    // of them.
    let base = spec
        .map(|s| (s.base_mhz, format!("{} publishes", s.name)))
        .or_else(|| {
            reading
                .cpu
                .reported_base_mhz
                .map(|m| (m, "This processor reports".to_string()))
        });

    if let (Some(mhz), Some((base_mhz, authority)), true) =
        (reading.cpu.all_core_mhz, base, under_demand)
    {
        if mhz < base_mhz {
            let utilization = reading.cpu.utilization_pct.unwrap_or_default();
            verdicts.push(Verdict {
                kind: VerdictKind::BelowBaseClock,
                severity: Severity::OutOfSpec,
                detail: format!(
                    "Every processor is busy, and the clock is {mhz} MHz against a guaranteed {base_mhz} MHz. Busy and slow at the same time means something is holding the clock down rather than the machine running out of work."
                ),
                basis: format!(
                    "{authority} {base_mhz} MHz as its base clock, which is guaranteed at the rated TDP. Measured at {utilization:.0} percent utilization, so the processor is being asked for it."
                ),
            });
        }
    }

    // Everything below needs figures no machine reports about itself.
    let Some(spec) = spec else {
        return verdicts;
    };

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

        // Drawing far less than the part is rated for, while every processor
        // is busy, means something outside the processor is limiting it.
        //
        // Gated on demand for the same reason the clock check is: a machine
        // sitting idle draws almost nothing and that is correct behaviour, not
        // a starved platform.
        let floor = spec.ctdp_min_watts.unwrap_or(spec.tdp_watts) as f32;
        if under_demand && watts < floor {
            verdicts.push(Verdict {
                kind: VerdictKind::PowerBelowRating,
                severity: Severity::Degraded,
                detail: format!(
                    "Every processor is busy and the package is drawing {watts:.1} W, below the {floor:.0} W this processor is rated to draw. It is being supplied less power than it is built to use."
                ),
                basis: format!(
                    "{} publishes a configurable TDP floor of {floor:.0} W. Package power read from the processor's own energy counters.",
                    spec.name
                ),
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

    pub(super) fn spec() -> CpuSpec {
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
            source: "test fixture".to_string(),
        }
    }

    pub(super) fn reading(cpu: CpuReading) -> Reading {
        Reading {
            timestamp_ms: 0,
            stall: StallSignal {
                cpu: 0.0,
                memory: 0.0,
                io: 0.0,
            },
            cpu,
            processes: vec![],
            containers: vec![],
        }
    }

    /// A machine under real load and behaving correctly.
    ///
    /// Utilization is high because that is the only state in which the base
    /// clock guarantee can be tested at all.
    pub(super) fn healthy_cpu() -> CpuReading {
        CpuReading {
            all_core_mhz: Some(2400),
            reported_base_mhz: None,
            utilization_pct: Some(95.0),
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
    fn a_starved_platform_is_flagged_when_every_processor_is_busy() {
        // Measured on 2026-08-15. On an underpowered USB-C supply this machine
        // held its package at roughly 9.3 W against a 10 W configurable floor
        // and could not reach its 2000 MHz base clock. Swapping to the rated
        // charger raised the sustained all-core clock from about 1150 MHz to
        // about 1930, a difference of roughly sixty percent that the user had
        // no way of seeing.
        let mut cpu = healthy_cpu();
        cpu.package_watts = Some(9.3);
        cpu.utilization_pct = Some(100.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::PowerBelowRating)
            .expect("a part drawing under its floor at full load is being starved");
        assert!(v.detail.contains("9.3"));
        assert!(v.basis.contains("10 W"), "got basis: {}", v.basis);
    }

    #[test]
    fn an_idle_machine_drawing_little_power_is_not_starved() {
        // The obvious false positive. An idle processor draws almost nothing
        // and that is correct behaviour, not a supply problem.
        let mut cpu = healthy_cpu();
        cpu.package_watts = Some(2.0);
        cpu.utilization_pct = Some(4.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(!verdicts
            .iter()
            .any(|v| v.kind == VerdictKind::PowerBelowRating));
    }

    #[test]
    fn power_inside_the_band_under_load_is_not_flagged_either_way() {
        let mut cpu = healthy_cpu();
        cpu.package_watts = Some(15.0);
        cpu.utilization_pct = Some(100.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(!verdicts.iter().any(|v| matches!(
            v.kind,
            VerdictKind::PowerBelowRating | VerdictKind::PowerOutsideBand
        )));
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
    fn an_unknown_processor_is_still_held_to_the_base_clock_it_reports() {
        // The point of the whole exercise. Three processors are in the shipped
        // database, so reaching the base clock only through it meant the
        // strongest verdict LoadBear has said nothing on any other machine.
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.reported_base_mhz = Some(2000);
        let verdicts = evaluate(&reading(cpu), None);
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::BelowBaseClock)
            .expect("a machine that reports its own base clock can be judged against it");
        assert!(
            v.basis.contains("This processor reports"),
            "the weaker authority must be stated rather than implied: {}",
            v.basis
        );
    }

    #[test]
    fn a_published_figure_is_preferred_over_the_machines_own() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.reported_base_mhz = Some(1500);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::BelowBaseClock)
            .expect("must flag");
        assert!(
            v.basis.contains("AMD Ryzen 7 4980U publishes"),
            "got {}",
            v.basis
        );
        assert!(
            v.detail.contains("2000"),
            "the published figure wins: {}",
            v.detail
        );
    }

    #[test]
    fn a_processor_with_no_base_clock_from_anywhere_is_not_judged() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.reported_base_mhz = None;
        let verdicts = evaluate(&reading(cpu), None);
        assert!(
            !verdicts
                .iter()
                .any(|v| v.kind == VerdictKind::BelowBaseClock),
            "with no guarantee from either source there is nothing to judge against"
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
    fn an_idle_machine_clocked_below_base_is_not_out_of_spec() {
        // The regression this guards. Windows averages frequency across every
        // logical processor, so an idle machine reads far below base because
        // its cores are parked, which is power management working. Flagging it
        // would make the strongest verdict LoadBear has fire constantly on a
        // machine doing nothing at all.
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(900);
        cpu.utilization_pct = Some(3.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(
            !verdicts
                .iter()
                .any(|v| v.kind == VerdictKind::BelowBaseClock),
            "a guarantee about performance under load says nothing about an idle machine"
        );
    }

    #[test]
    fn a_partly_loaded_machine_clocked_below_base_is_not_out_of_spec() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.utilization_pct = Some(45.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(
            !verdicts
                .iter()
                .any(|v| v.kind == VerdictKind::BelowBaseClock),
            "idle cores drag the all-core average down, so this figure is not evidence of a fault"
        );
    }

    #[test]
    fn without_a_utilization_figure_the_base_clock_cannot_be_judged() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.utilization_pct = None;
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(
            !verdicts
                .iter()
                .any(|v| v.kind == VerdictKind::BelowBaseClock),
            "a backend that cannot measure demand must stay silent rather than assume load"
        );
    }

    #[test]
    fn the_basis_records_the_utilization_the_verdict_was_measured_at() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1400);
        cpu.utilization_pct = Some(97.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::BelowBaseClock)
            .expect("must flag below base clock under real load");
        assert!(
            v.basis.contains("97 percent"),
            "the demand the verdict rests on belongs in its basis, got: {}",
            v.basis
        );
    }

    #[test]
    fn missing_optional_readings_produce_no_verdicts_rather_than_panicking() {
        let cpu = CpuReading {
            all_core_mhz: None,
            reported_base_mhz: None,
            utilization_pct: None,
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

#[cfg(test)]
mod thermal_band_tests {
    use super::tests::{healthy_cpu, reading, spec};
    use super::*;

    const TJMAX: Option<f32> = Some(105.0);

    #[test]
    fn a_cool_processor_is_normal() {
        assert_eq!(thermal_band(45.0, TJMAX), Some(ThermalBand::Normal));
        assert_eq!(thermal_band(79.0, TJMAX), Some(ThermalBand::Normal));
    }

    #[test]
    fn the_bands_run_in_order_as_it_heats_up() {
        let at = |t| thermal_band(t, TJMAX).unwrap();
        assert_eq!(at(70.0), ThermalBand::Normal);
        assert_eq!(at(85.0), ThermalBand::Warm);
        assert_eq!(at(95.0), ThermalBand::Hot);
        assert_eq!(at(102.0), ThermalBand::AtLimit);
    }

    #[test]
    fn red_begins_exactly_where_the_thermal_verdict_fires() {
        // The two must not be able to disagree. A tile turning red while
        // nothing appears in the findings, or the reverse, would be the
        // interface contradicting itself about the same reading.
        let tjmax = 105.0;
        let boundary = tjmax - THERMAL_HEADROOM_LOW_C;

        assert_eq!(
            thermal_band(boundary, Some(tjmax)),
            Some(ThermalBand::AtLimit)
        );
        assert_eq!(
            thermal_band(boundary - 0.1, Some(tjmax)),
            Some(ThermalBand::Hot)
        );

        let mut cpu = healthy_cpu();
        cpu.package_temp_c = Some(boundary);
        cpu.tjmax_c = Some(tjmax);
        assert!(
            evaluate(&reading(cpu), Some(&spec()))
                .iter()
                .any(|v| v.kind == VerdictKind::ThermalHeadroomLow),
            "the verdict must fire at the same reading the tile turns red at"
        );
    }

    #[test]
    fn the_bands_rescale_to_the_part_rather_than_to_a_fixed_scale() {
        // 85 degrees is comfortable on a part specified to 105 and nearly out
        // of room on one specified to 95. A fixed scale gets one of them wrong.
        assert_eq!(thermal_band(85.0, Some(105.0)), Some(ThermalBand::Warm));
        assert_eq!(thermal_band(85.0, Some(95.0)), Some(ThermalBand::Hot));
    }

    #[test]
    fn without_a_published_limit_nothing_is_coloured() {
        assert_eq!(thermal_band(85.0, None), None);
        assert_eq!(
            thermal_band(85.0, Some(0.0)),
            None,
            "AMD parts report TjMax as zero from every source, and zero is not a limit"
        );
    }
}
