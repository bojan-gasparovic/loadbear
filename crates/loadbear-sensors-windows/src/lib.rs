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

pub mod pawnio;

pub use pawnio::{PawnIo, PawnIoError};
