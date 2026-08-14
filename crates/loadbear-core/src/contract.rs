use serde::{Deserialize, Serialize};

use crate::tier::Tier;
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

/// Decides whether a finding may interrupt the user.
///
/// Notification fatigue is what kills tools in this category. A tool that pops
/// during a routine build is muted the same day and never heard from again, so
/// this gate is deliberately restrictive and stays quiet through plenty of
/// genuinely heavy load.
///
/// Time is passed in rather than read from the clock, which is what makes the
/// sustained window testable without waiting five minutes.
#[derive(Debug, Clone)]
pub struct NotificationGate {
    sustained_ms: u64,
    strained_since_ms: Option<u64>,
    notified_this_episode: bool,
}

impl NotificationGate {
    /// `sustained_ms` is how long a condition must hold continuously before it
    /// may escalate. The design specifies five minutes as a deliberate first
    /// guess to be tuned against real use, not a derived constant.
    pub fn new(sustained_ms: u64) -> Self {
        Self {
            sustained_ms,
            strained_since_ms: None,
            notified_this_episode: false,
        }
    }

    /// Returns true exactly once per episode, when all three conditions of the
    /// interruption contract are satisfied: sustained, diagnosable, actionable.
    pub fn evaluate(&mut self, tier: Tier, finding: Option<&Finding>, now_ms: u64) -> bool {
        if tier != Tier::Strained {
            self.strained_since_ms = None;
            self.notified_this_episode = false;
            return false;
        }

        let since = *self.strained_since_ms.get_or_insert(now_ms);

        if self.notified_this_episode {
            return false;
        }

        if now_ms.saturating_sub(since) < self.sustained_ms {
            return false;
        }

        let Some(finding) = finding else {
            return false;
        };

        if !finding.is_actionable() {
            return false;
        }

        self.notified_this_episode = true;
        true
    }

    /// Whether an episode of strain is currently being tracked.
    pub fn is_tracking(&self) -> bool {
        self.strained_since_ms.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::Tier;
    use crate::verdict::{Severity, Verdict, VerdictKind};

    const FIVE_MIN: u64 = 5 * 60 * 1000;

    fn actionable_finding() -> Finding {
        Finding {
            verdict: Verdict {
                kind: VerdictKind::BelowBaseClock,
                severity: Severity::OutOfSpec,
                detail: "test".to_string(),
                basis: "test".to_string(),
            },
            cause: Some(Cause {
                label: "Docker Desktop".to_string(),
                kind: CauseKind::Process { pid: 4242 },
            }),
            remediation: Some(Remediation::ReconfigureLimit),
        }
    }

    fn undiagnosed_finding() -> Finding {
        Finding {
            verdict: actionable_finding().verdict,
            cause: None,
            remediation: None,
        }
    }

    #[test]
    fn a_spike_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0));
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 90_000));
    }

    #[test]
    fn a_sustained_actionable_finding_notifies_once_the_window_elapses() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0));
        assert!(gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN));
    }

    #[test]
    fn it_does_not_notify_twice_for_one_episode() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0);
        assert!(gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN));
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN + 1000));
    }

    #[test]
    fn recovering_resets_the_window() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0);
        gate.evaluate(Tier::Easy, None, 60_000);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN),
            "the clock restarts after recovery, so this is only one minute of strain"
        );
    }

    #[test]
    fn an_undiagnosed_finding_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&undiagnosed_finding()), 0);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&undiagnosed_finding()), FIVE_MIN),
            "a finding with no attributed cause cannot satisfy the actionable condition"
        );
    }

    #[test]
    fn strain_with_nothing_to_do_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        let mut finding = actionable_finding();
        finding.remediation = None;
        gate.evaluate(Tier::Strained, Some(&finding), 0);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&finding), FIVE_MIN),
            "compiling is slow is not worth an interruption"
        );
    }

    #[test]
    fn braced_never_notifies_however_long_it_lasts() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Braced, Some(&actionable_finding()), 0);
        assert!(!gate.evaluate(Tier::Braced, Some(&actionable_finding()), FIVE_MIN * 10));
    }
}
