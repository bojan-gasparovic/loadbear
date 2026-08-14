//! LoadBear diagnosis engine.
//!
//! This crate turns observations into judgements and contains no operating
//! system calls. Every function here takes a normalized [`Reading`] plus
//! specification data and returns a verdict, which is what makes the whole of
//! LoadBear's product logic testable with synthetic input on any machine.
//!
//! The rule this crate exists to enforce: every judgement traces to a vendor
//! guarantee, a hardware bit, or the machine's own history. Nothing here may
//! invent a threshold.

pub mod spec;
pub mod types;
pub mod verdict;

pub use spec::{CpuKey, CpuSpec, SpecDb, SpecError, Vendor};
pub use verdict::{evaluate, Severity, Verdict, VerdictKind};
pub use types::{
    CpuReading, ProcessReading, Reading, Resource, StallSignal, ThrottleReason, ThrottleState,
};
