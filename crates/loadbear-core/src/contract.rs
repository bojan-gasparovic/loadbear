use serde::{Deserialize, Serialize};

use crate::verdict::Verdict;

/// What kind of thing is responsible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CauseKind {
    Process { pid: u32 },
    Container { id: String },
    SystemService,
    PowerState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub label: String,
    pub kind: CauseKind,
}

/// What the user can actually do about it.
///
/// This deliberately does not require that something can be killed. Being
/// unable to end a process is not the same as there being nothing to do, and
/// the difference covers some of the highest-value findings available. Adding
/// an antivirus exclusion for a build directory is the clearest example.
///
/// Every variant names a specific action. A finding that cannot be stated as a
/// sentence ending in a thing the user does has no remediation and therefore
/// cannot notify. No variant may be added that means "something is happening".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Remediation {
    /// End a process or container the user started.
    Stop,
    /// Change an allocation, such as Docker Desktop memory or WSL2 config.
    ReconfigureLimit,
    /// Exclude a path from antivirus or indexing.
    AddExclusion,
    /// Postpone background work such as an update or an index rebuild.
    Defer,
    /// Plug in, or change the power profile.
    ChangePowerState,
    /// Physical intervention, such as clearing dust. Baseline-driven.
    Physical,
}

/// A verdict, what caused it, and what to do about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub verdict: Verdict,
    pub cause: Option<Cause>,
    pub remediation: Option<Remediation>,
}

impl Finding {
    /// Whether this finding has earned the right to interrupt.
    ///
    /// Requires both a named cause and a concrete action. Either one missing
    /// means the notification would amount to telling the user their machine
    /// is busy, which they already know.
    pub fn is_actionable(&self) -> bool {
        self.cause.is_some() && self.remediation.is_some()
    }
}
