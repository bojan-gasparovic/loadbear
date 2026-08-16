//! Windows sensor backend for LoadBear.
//!
//! This is the only crate permitted to touch a driver. It depends on
//! `loadbear-core` for types and never the other way round, which is what keeps
//! the diagnosis layer testable on any machine with no hardware involved.
//!
//! # Temperature is optional
//!
//! LoadBear ships no kernel driver. Temperature reaches it through PawnIO,
//! which the user installs themselves, so an absent driver is the state every
//! user starts in rather than an error. Everything else LoadBear measures on
//! Windows, including the sustained all-core clock behind the `BelowBaseClock`
//! verdict, reads from unprivileged performance counters.

pub mod amd;
pub mod baseline;
pub mod counters;
pub mod cpuid;
pub mod docker;
pub mod installer;
pub mod intel;
pub mod mapping;
pub mod pawnio;
pub mod pm_table;
pub mod power;
pub mod presentation;
pub mod processes;
pub mod service_control;
pub mod shared;
pub mod temperature;
pub mod topology;

pub use amd::{
    decode_ccd_temp, decode_die_temp, is_plausible_celsius, read_amd_temperature, PciGuard,
    TemperatureReading, TemperatureZone,
};
pub use docker::read_containers;
pub use installer::{install, InstallError};
pub use mapping::{MappingError, TemperaturePublisher, TemperatureReader};
pub use pawnio::{PawnIo, PawnIoError};
pub use pm_table::PerCoreTemperature;
pub use power::PackagePower;
pub use processes::ProcessSampler;
pub use service_control::{install_and_start, is_running, ServiceError};
pub use shared::{SharedTemperature, MAPPING_NAME};
pub use temperature::{Remedy, TemperatureStatus, WindowsTemperature, PAWNIO_URL};
