//! Vendor dispatch and graceful degradation for temperature.
//!
//! LoadBear ships no kernel driver, so an absent source is the state every user
//! starts in rather than an error. That is why [`WindowsTemperature::read`]
//! returns a reading instead of a `Result`: a caller must not be able to treat
//! missing temperature as a failure.
//!
//! # What is shipped and what is not
//!
//! The compiled PawnIO modules are embedded here. They are LGPL-2.1, and the
//! licence text travels with them in `modules/COPYING-PawnIO-Modules`.
//!
//! The driver and `PawnIOLib.dll` are **not** shipped. Both arrive with the
//! user's own PawnIO installation from pawnio.eu. That decision removed the
//! GPL-2.0 redistribution obligations that bundling the driver would have
//! carried.

use loadbear_core::{CpuKey, Vendor};

use crate::amd::{read_amd_temperature, TemperatureReading, TemperatureZone};
use crate::pawnio::{PawnIo, PawnIoError};
use crate::pm_table::PerCoreTemperature;

/// AMD Zen family 17h and later. Gates on family 0x17 to 0x1A in its `main()`.
const MODULE_AMD17: &[u8] = include_bytes!("../modules/AMDFamily17.bin");

/// Intel MSR access. Loaded but not yet consumed; see [`WindowsTemperature`].
const MODULE_INTEL_MSR: &[u8] = include_bytes!("../modules/IntelMSR.bin");

/// Where a user goes to enable temperature monitoring.
pub const PAWNIO_URL: &str = "https://pawnio.eu";

/// What the user can do about temperature being unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// The driver is not installed. Point at the upstream download.
    InstallDriver { url: &'static str },
    /// Present but unusable here, and nothing the user can do about it.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemperatureStatus {
    Available,
    Unavailable { reason: String, remedy: Remedy },
}

impl TemperatureStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Windows temperature source.
///
/// Opens the driver lazily and remembers failure, so a machine without PawnIO
/// does not attempt a device open on every sampling tick.
pub struct WindowsTemperature {
    device: Option<PawnIo>,
    /// Second executor for the SMU table. A PawnIO executor holds one module,
    /// and per-core temperature comes from a different module than the die
    /// reading does.
    per_core: Option<PerCoreTemperature>,
    cores: usize,
    status: TemperatureStatus,
    vendor: Vendor,
}

impl WindowsTemperature {
    /// Prepare a source for the given CPU.
    ///
    /// Does the open and the module load once. Everything after this is a read.
    pub fn new(key: Option<&CpuKey>) -> Self {
        let vendor = key.map(|k| k.vendor).unwrap_or(Vendor::Other);
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);

        let device = match PawnIo::open() {
            Ok(d) => d,
            Err(PawnIoError::NotInstalled) => {
                return Self {
                    device: None,
                    per_core: None,
                    cores,
                    status: TemperatureStatus::Unavailable {
                        reason: "LoadBear does not ship a kernel driver. Install PawnIO to \
                                 enable temperature monitoring."
                            .to_string(),
                        remedy: Remedy::InstallDriver { url: PAWNIO_URL },
                    },
                    vendor,
                }
            }
            Err(e) => {
                return Self {
                    device: None,
                    per_core: None,
                    cores,
                    status: TemperatureStatus::Unavailable {
                        reason: e.to_string(),
                        remedy: Remedy::None,
                    },
                    vendor,
                }
            }
        };

        let module = match vendor {
            Vendor::Amd => MODULE_AMD17,
            Vendor::Intel => MODULE_INTEL_MSR,
            Vendor::Other => {
                return Self {
                    device: None,
                    per_core: None,
                    cores,
                    status: TemperatureStatus::Unavailable {
                        reason: "This processor vendor is not supported for temperature."
                            .to_string(),
                        remedy: Remedy::None,
                    },
                    vendor,
                }
            }
        };

        if let Err(e) = device.load_module(module) {
            // The module's own main() rejects unsupported parts, so a load
            // failure here usually means this CPU is out of its range rather
            // than anything being broken.
            return Self {
                device: None,
                per_core: None,
                cores,
                status: TemperatureStatus::Unavailable {
                    reason: format!("No temperature module supports this processor. {e}"),
                    remedy: Remedy::None,
                },
                vendor,
            };
        }

        // Per-core is a bonus, not a requirement. A machine whose PM table
        // layout is unverified still reports a die temperature.
        let per_core = if vendor == Vendor::Amd {
            PerCoreTemperature::new().ok().filter(|p| p.is_supported())
        } else {
            None
        };

        Self {
            device: Some(device),
            per_core,
            cores,
            status: TemperatureStatus::Available,
            vendor,
        }
    }

    pub fn status(&self) -> &TemperatureStatus {
        &self.status
    }

    /// Read temperature, or return an empty reading.
    ///
    /// Never returns `Result`. Temperature is optional throughout LoadBear and
    /// the type signature is what makes that impossible to get wrong.
    pub fn read(&mut self) -> TemperatureReading {
        let Some(device) = &self.device else {
            return TemperatureReading::default();
        };

        match self.vendor {
            Vendor::Amd => {
                let mut r = read_amd_temperature(device).unwrap_or_default();
                if let Some(pc) = &self.per_core {
                    for (i, c) in pc.read(self.cores).into_iter().enumerate() {
                        r.zones.push(TemperatureZone {
                            label: format!("Core {i}"),
                            celsius: c,
                        });
                    }
                }
                r
            }
            // Intel reads land in LB-12. The module is loaded and the path is
            // reachable; it is simply not written yet, and saying so is better
            // than returning a number from nowhere.
            Vendor::Intel | Vendor::Other => TemperatureReading::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amd_key() -> CpuKey {
        CpuKey {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
        }
    }

    #[test]
    fn without_a_driver_it_reports_unavailable_and_returns_an_empty_reading() {
        let mut t = WindowsTemperature::new(Some(&amd_key()));
        let r = t.read();
        match t.status() {
            TemperatureStatus::Unavailable { .. } => {
                assert!(r.is_empty(), "no driver must mean no reading");
            }
            TemperatureStatus::Available => {
                // PawnIO is installed on this machine. Valid state.
            }
        }
    }

    #[test]
    fn a_missing_driver_offers_the_install_remedy_rather_than_only_an_apology() {
        let t = WindowsTemperature::new(Some(&amd_key()));
        if let TemperatureStatus::Unavailable { reason, remedy } = t.status() {
            assert!(!reason.contains("0x"), "no raw error codes: {reason}");
            if matches!(remedy, Remedy::InstallDriver { .. }) {
                assert_eq!(remedy, &Remedy::InstallDriver { url: PAWNIO_URL });
            }
        }
    }

    #[test]
    fn reading_repeatedly_without_a_driver_never_panics() {
        let mut t = WindowsTemperature::new(Some(&amd_key()));
        for _ in 0..20 {
            let _ = t.read();
        }
    }

    #[test]
    fn the_embedded_modules_are_present_and_non_trivial() {
        // A zero length blob would fail at load time with a confusing error.
        assert!(MODULE_AMD17.len() > 1000, "AMDFamily17 blob looks wrong");
        assert!(MODULE_INTEL_MSR.len() > 1000, "IntelMSR blob looks wrong");
    }
}
