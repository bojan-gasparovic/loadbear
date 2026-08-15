use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::types::{Resource, StallSignal};
use crate::verdict::{Severity, Verdict, VerdictKind};

/// How the machine is doing.
///
/// This is severity of state only. Whether the user is interrupted is decided
/// separately by the interruption contract. Strained without a notification is
/// a legitimate and common state: the machine is genuinely struggling, but the
/// cause is the build the user deliberately started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// Within spec, headroom available.
    Easy,
    /// Degraded, or evidence still accumulating.
    Braced,
    /// Sustained and out of spec.
    Strained,
}

/// Stall at or above this share of the window counts as degraded.
pub const STALL_BRACED: f32 = 0.40;

/// Stall at or above this share of the window counts as out of spec.
///
/// At this level the majority of the sampling window was spent waiting rather
/// than progressing, which is the definition of overloaded regardless of what
/// any published limit says.
const STALL_STRAINED: f32 = 0.80;

/// Why the tier is what it is.
///
/// Exists because a tier on its own is an assertion with nothing behind it. A
/// red tray icon and an empty findings panel is the shape of the complaint that
/// produced this type: the machine was being called strained by the stall
/// signal, which produces no verdict, so the interface had a colour and no
/// explanation to go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TierReason {
    /// Nothing is wrong.
    Clear,
    /// A published limit or a hardware bit was crossed.
    Verdict(VerdictKind),
    /// No limit was crossed, but this resource is spending the window waiting.
    Stall(Resource),
}

/// A tier and the reason for it, which travel together so they cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    pub tier: Tier,
    pub reason: TierReason,
}

/// Classify machine state from verdicts and stall together.
///
/// The worst input wins. Either signal can drive the tier on its own, because
/// each catches conditions the other misses. Verdicts catch a machine quietly
/// running below its guaranteed clock while feeling responsive. Stall catches
/// a machine paging itself to death without crossing any published limit.
///
/// **This is an instantaneous judgement about one window and is not the tier to
/// show anybody.** `Strained` is defined as sustained, and a single window
/// cannot establish that. Feed the result to a [`TierTracker`], which is what
/// applies the sustained requirement.
pub fn classify(verdicts: &[Verdict], stall: &StallSignal) -> Assessment {
    let worst_verdict = verdicts.iter().max_by_key(|v| v.severity);
    let from_verdicts = worst_verdict
        .map(|v| match v.severity {
            Severity::OutOfSpec => Tier::Strained,
            Severity::Degraded => Tier::Braced,
            Severity::Ok => Tier::Easy,
        })
        .unwrap_or(Tier::Easy);

    let (worst_resource, worst_stall) = stall.worst();
    let from_stall = if worst_stall >= STALL_STRAINED {
        Tier::Strained
    } else if worst_stall >= STALL_BRACED {
        Tier::Braced
    } else {
        Tier::Easy
    };

    if from_verdicts >= from_stall {
        Assessment {
            tier: from_verdicts,
            reason: match worst_verdict {
                Some(v) if from_verdicts > Tier::Easy => TierReason::Verdict(v.kind),
                _ => TierReason::Clear,
            },
        }
    } else {
        Assessment {
            tier: from_stall,
            reason: TierReason::Stall(worst_resource),
        }
    }
}

/// How long a worse state must hold continuously before it is adopted.
///
/// Strain is defined as sustained. A build that pins the machine for four
/// seconds is the user doing their job, and colouring the tray icon red for it
/// is a false alarm that teaches people to ignore the icon.
pub const ESCALATE_MS: u64 = 30_000;

/// How long recovery must hold before a better state is adopted.
///
/// Shorter than escalation but not instant, so that a momentary lull in the
/// middle of a heavy build does not flash the icon back to green and start the
/// thirty second escalation clock over.
pub const SETTLE_MS: u64 = 20_000;

/// Applies the sustained requirement that [`Tier::Strained`] has always claimed.
///
/// The rule, in one sentence each way:
///
/// - **Escalate** to the worst tier that *every* observation in the last
///   [`ESCALATE_MS`] supports.
/// - **De-escalate** to the best tier that *every* observation in the last
///   [`SETTLE_MS`] supports.
///
/// Stated that way, a burst cannot escalate anything, because the observations
/// on either side of it do not support the higher tier. It also means the tier
/// never moves on a single sample in either direction, which is what stops the
/// icon flickering.
///
/// Time is passed in rather than read from a clock, so the whole behaviour is
/// testable without waiting thirty seconds.
#[derive(Debug, Clone)]
pub struct TierTracker {
    escalate_ms: u64,
    settle_ms: u64,
    current: Assessment,
    history: VecDeque<(u64, Assessment)>,
}

impl Default for TierTracker {
    fn default() -> Self {
        Self::new(ESCALATE_MS, SETTLE_MS)
    }
}

impl TierTracker {
    pub fn new(escalate_ms: u64, settle_ms: u64) -> Self {
        Self {
            escalate_ms,
            settle_ms,
            current: Assessment {
                tier: Tier::Easy,
                reason: TierReason::Clear,
            },
            history: VecDeque::new(),
        }
    }

    /// Record one window's judgement and return the tier to actually show.
    pub fn observe(&mut self, assessment: Assessment, now_ms: u64) -> Assessment {
        self.history.push_back((now_ms, assessment));
        let horizon = now_ms.saturating_sub(self.escalate_ms.max(self.settle_ms));
        while self.history.front().is_some_and(|(t, _)| *t < horizon) {
            self.history.pop_front();
        }

        if let Some(worse) = self.supported_by_all(now_ms, self.escalate_ms, true) {
            if worse.tier > self.current.tier {
                self.current = worse;
                return self.current;
            }
        }
        if let Some(better) = self.supported_by_all(now_ms, self.settle_ms, false) {
            if better.tier < self.current.tier {
                self.current = better;
                return self.current;
            }
        }

        // The tier is unchanged, but if this window agrees with it, take its
        // reason. Otherwise the panel could keep naming a resource that
        // stopped being the worst one several windows ago.
        if assessment.tier == self.current.tier {
            self.current.reason = assessment.reason;
        }
        self.current
    }

    /// The worst, or best, tier that every observation across `window_ms`
    /// agrees on, or `None` when the window is not yet covered.
    ///
    /// Requiring full coverage is what makes a fresh start conservative: with
    /// ten seconds of history there is no answer about the last thirty, so
    /// nothing moves.
    fn supported_by_all(&self, now_ms: u64, window_ms: u64, worst: bool) -> Option<Assessment> {
        let cutoff = now_ms.saturating_sub(window_ms);
        let oldest = self.history.front()?.0;
        if oldest > cutoff {
            return None;
        }

        let mut chosen: Option<Assessment> = None;
        for (_, a) in self.history.iter().filter(|(t, _)| *t >= cutoff) {
            chosen = Some(match chosen {
                None => *a,
                Some(c) if worst && a.tier < c.tier => *a,
                Some(c) if !worst && a.tier > c.tier => *a,
                Some(c) => c,
            });
        }
        chosen
    }

    pub fn current(&self) -> Assessment {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StallSignal;
    use crate::verdict::{Severity, Verdict, VerdictKind};

    fn quiet() -> StallSignal {
        StallSignal {
            cpu: 0.02,
            memory: 0.0,
            io: 0.01,
        }
    }

    fn verdict(kind: VerdictKind, severity: Severity) -> Verdict {
        Verdict {
            kind,
            severity,
            detail: "test".to_string(),
            basis: "test".to_string(),
        }
    }

    #[test]
    fn no_verdicts_and_no_stall_is_easy() {
        assert_eq!(classify(&[], &quiet()).tier, Tier::Easy);
    }

    #[test]
    fn an_out_of_spec_verdict_is_strained() {
        let v = [verdict(VerdictKind::BelowBaseClock, Severity::OutOfSpec)];
        assert_eq!(classify(&v, &quiet()).tier, Tier::Strained);
    }

    #[test]
    fn a_degraded_verdict_alone_is_braced() {
        let v = [verdict(VerdictKind::ThermalHeadroomLow, Severity::Degraded)];
        assert_eq!(classify(&v, &quiet()).tier, Tier::Braced);
    }

    #[test]
    fn heavy_stall_alone_is_braced_even_with_no_verdicts() {
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.55,
            io: 0.20,
        };
        assert_eq!(classify(&[], &stall).tier, Tier::Braced);
    }

    #[test]
    fn severe_stall_alone_is_strained_even_with_no_verdicts() {
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.85,
            io: 0.20,
        };
        assert_eq!(
            classify(&[], &stall).tier,
            Tier::Strained,
            "a machine paging itself to death is strained whether or not any published limit was crossed"
        );
    }

    #[test]
    fn the_worst_input_wins() {
        let v = [verdict(VerdictKind::ThermalHeadroomLow, Severity::Degraded)];
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.85,
            io: 0.20,
        };
        assert_eq!(classify(&v, &stall).tier, Tier::Strained);
    }
}

#[cfg(test)]
mod tracker_tests {
    use super::*;
    use crate::types::StallSignal;

    const TICK: u64 = 1500;

    fn easy() -> Assessment {
        Assessment {
            tier: Tier::Easy,
            reason: TierReason::Clear,
        }
    }

    fn strained() -> Assessment {
        Assessment {
            tier: Tier::Strained,
            reason: TierReason::Stall(Resource::Memory),
        }
    }

    fn braced() -> Assessment {
        Assessment {
            tier: Tier::Braced,
            reason: TierReason::Stall(Resource::Cpu),
        }
    }

    /// Feed `count` identical windows and return the tier at the end.
    fn run(t: &mut TierTracker, a: Assessment, count: u64, from: u64) -> (Tier, u64) {
        let mut now = from;
        let mut last = t.current().tier;
        for _ in 0..count {
            now += TICK;
            last = t.observe(a, now).tier;
        }
        (last, now)
    }

    #[test]
    fn a_burst_never_escalates_anything() {
        // The complaint, as a test. Something runs hard for a few seconds on
        // an otherwise quiet machine and the tray icon goes red.
        let mut t = TierTracker::default();
        let (_, mut now) = run(&mut t, easy(), 40, 0);

        for _ in 0..3 {
            now += TICK;
            assert_eq!(
                t.observe(strained(), now).tier,
                Tier::Easy,
                "four seconds of load is a burst, not strain"
            );
        }
    }

    #[test]
    fn sustained_load_does_escalate() {
        // The other half. A tracker that never escalates is not a monitor.
        let mut t = TierTracker::default();
        let (_, now) = run(&mut t, easy(), 40, 0);
        let (tier, _) = run(&mut t, strained(), 40, now);
        assert_eq!(tier, Tier::Strained);
    }

    #[test]
    fn escalation_takes_at_least_the_full_window() {
        let mut t = TierTracker::default();
        let (_, mut now) = run(&mut t, easy(), 40, 0);
        let started = now;

        loop {
            now += TICK;
            if t.observe(strained(), now).tier == Tier::Strained {
                break;
            }
            assert!(
                now - started < ESCALATE_MS * 3,
                "escalation should happen eventually"
            );
        }
        assert!(
            now - started >= ESCALATE_MS,
            "escalated after {}ms, which is less than the {ESCALATE_MS}ms it is meant to require",
            now - started
        );
    }

    #[test]
    fn one_quiet_window_in_the_middle_restarts_the_clock() {
        let mut t = TierTracker::default();
        let (_, mut now) = run(&mut t, easy(), 40, 0);

        // Nearly enough strain to escalate.
        for _ in 0..18 {
            now += TICK;
            t.observe(strained(), now);
        }
        // One window disagrees, so the last thirty seconds no longer all
        // support Strained.
        now += TICK;
        t.observe(easy(), now);

        for _ in 0..5 {
            now += TICK;
            assert_eq!(
                t.observe(strained(), now).tier,
                Tier::Easy,
                "escalation requires every window in the period to agree"
            );
        }
    }

    #[test]
    fn recovery_is_not_instant_either() {
        let mut t = TierTracker::default();
        let (_, now) = run(&mut t, strained(), 40, 0);
        let (tier, mut now) = (t.current().tier, now);
        assert_eq!(tier, Tier::Strained);

        for _ in 0..3 {
            now += TICK;
            assert_eq!(
                t.observe(easy(), now).tier,
                Tier::Strained,
                "a momentary lull inside a heavy build must not flash the icon green"
            );
        }
    }

    #[test]
    fn recovery_does_eventually_happen() {
        // The other side of the previous test, and the more important one. A
        // tier that escalates and never comes back is a tier nobody trusts,
        // and a real run showed eighteen seconds of a quiet machine still
        // reading Strained, which is only correct if it ends.
        let mut t = TierTracker::default();
        let (_, mut now) = run(&mut t, strained(), 40, 0);
        assert_eq!(t.current().tier, Tier::Strained);

        let quiet_from = now;
        loop {
            now += TICK;
            if t.observe(easy(), now).tier == Tier::Easy {
                break;
            }
            assert!(
                now - quiet_from < SETTLE_MS * 4,
                "still Strained after {}ms of a completely quiet machine",
                now - quiet_from
            );
        }
        assert!(
            now - quiet_from >= SETTLE_MS,
            "recovered after {}ms, sooner than the {SETTLE_MS}ms it should require",
            now - quiet_from
        );
    }

    #[test]
    fn a_fresh_tracker_stays_calm_until_it_has_seen_enough() {
        // Startup must not inherit whatever the machine happened to be doing
        // in its first second.
        let mut t = TierTracker::default();
        assert_eq!(t.observe(strained(), TICK).tier, Tier::Easy);
        assert_eq!(t.observe(strained(), TICK * 2).tier, Tier::Easy);
    }

    #[test]
    fn an_intermediate_tier_is_reachable_without_passing_through_the_worst() {
        let mut t = TierTracker::default();
        let (_, now) = run(&mut t, easy(), 40, 0);
        let (tier, _) = run(&mut t, braced(), 40, now);
        assert_eq!(tier, Tier::Braced);
    }

    #[test]
    fn a_tier_driven_by_stall_carries_the_resource_that_drove_it() {
        // The empty findings panel, as a test. If the stall signal is what
        // turned the icon red, the interface has to be able to say so.
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.95,
            io: 0.20,
        };
        let a = classify(&[], &stall);
        assert_eq!(a.tier, Tier::Strained);
        assert_eq!(a.reason, TierReason::Stall(Resource::Memory));
    }

    #[test]
    fn a_tier_driven_by_a_verdict_names_the_verdict() {
        use crate::verdict::{Severity, Verdict, VerdictKind};
        let v = [Verdict {
            kind: VerdictKind::BelowBaseClock,
            severity: Severity::OutOfSpec,
            detail: "test".to_string(),
            basis: "test".to_string(),
        }];
        let a = classify(
            &v,
            &StallSignal {
                cpu: 0.0,
                memory: 0.0,
                io: 0.0,
            },
        );
        assert_eq!(a.reason, TierReason::Verdict(VerdictKind::BelowBaseClock));
    }

    #[test]
    fn a_clear_machine_gives_a_clear_reason() {
        let a = classify(
            &[],
            &StallSignal {
                cpu: 0.01,
                memory: 0.0,
                io: 0.0,
            },
        );
        assert_eq!(a.tier, Tier::Easy);
        assert_eq!(a.reason, TierReason::Clear);
    }
}
