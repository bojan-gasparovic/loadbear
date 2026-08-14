use serde::{Deserialize, Serialize};

use crate::types::StallSignal;
use crate::verdict::{Severity, Verdict};

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
const STALL_BRACED: f32 = 0.40;

/// Stall at or above this share of the window counts as out of spec.
///
/// At this level the majority of the sampling window was spent waiting rather
/// than progressing, which is the definition of overloaded regardless of what
/// any published limit says.
const STALL_STRAINED: f32 = 0.80;

/// Classify machine state from verdicts and stall together.
///
/// The worst input wins. Either signal can drive the tier on its own, because
/// each catches conditions the other misses. Verdicts catch a machine quietly
/// running below its guaranteed clock while feeling responsive. Stall catches
/// a machine paging itself to death without crossing any published limit.
pub fn classify(verdicts: &[Verdict], stall: &StallSignal) -> Tier {
    let from_verdicts = verdicts
        .iter()
        .map(|v| match v.severity {
            Severity::OutOfSpec => Tier::Strained,
            Severity::Degraded => Tier::Braced,
            Severity::Ok => Tier::Easy,
        })
        .max()
        .unwrap_or(Tier::Easy);

    let (_, worst_stall) = stall.worst();
    let from_stall = if worst_stall >= STALL_STRAINED {
        Tier::Strained
    } else if worst_stall >= STALL_BRACED {
        Tier::Braced
    } else {
        Tier::Easy
    };

    from_verdicts.max(from_stall)
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
        assert_eq!(classify(&[], &quiet()), Tier::Easy);
    }

    #[test]
    fn an_out_of_spec_verdict_is_strained() {
        let v = [verdict(VerdictKind::BelowBaseClock, Severity::OutOfSpec)];
        assert_eq!(classify(&v, &quiet()), Tier::Strained);
    }

    #[test]
    fn a_degraded_verdict_alone_is_braced() {
        let v = [verdict(VerdictKind::ThermalHeadroomLow, Severity::Degraded)];
        assert_eq!(classify(&v, &quiet()), Tier::Braced);
    }

    #[test]
    fn heavy_stall_alone_is_braced_even_with_no_verdicts() {
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.55,
            io: 0.20,
        };
        assert_eq!(classify(&[], &stall), Tier::Braced);
    }

    #[test]
    fn severe_stall_alone_is_strained_even_with_no_verdicts() {
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.85,
            io: 0.20,
        };
        assert_eq!(
            classify(&[], &stall),
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
        assert_eq!(classify(&v, &stall), Tier::Strained);
    }
}
