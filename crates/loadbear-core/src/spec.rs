use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("could not parse the embedded specification database: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vendor {
    Intel,
    Amd,
    Other,
}

/// Identifies a CPU model by the values CPUID reports.
///
/// This is the join key between a running machine and the specification
/// database. Brand strings are unreliable, particularly for OEM-exclusive
/// parts, so they are never used for lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CpuKey {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
}

/// Published limits and guarantees for one CPU model.
///
/// `base_mhz` is the load-bearing field. Both Intel and AMD commit that the
/// part sustains this frequency at its rated TDP, which makes a sustained
/// all-core clock below it objectively out of spec in any chassis at any
/// ambient temperature. It is the only performance figure here that is a
/// contractual guarantee rather than a best case.
///
/// `tjmax_c` is here because it frequently cannot be read from the chip. Intel
/// exposes it via MSR 0x1A2, but AMD does not, and the LB-02 spike confirmed it
/// reads as zero on the Ryzen 7 4980U from every available source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub name: String,
    pub base_mhz: u32,
    pub boost_mhz: Option<u32>,
    pub tdp_watts: u32,
    pub ctdp_min_watts: Option<u32>,
    pub ctdp_max_watts: Option<u32>,
    pub tjmax_c: Option<f32>,
    pub cores: u8,
    pub threads: u8,
    /// Where these figures came from.
    ///
    /// Required, and not decorative. `base_mhz` is the only contractual number
    /// in the product and a wrong one produces a permanent, plausible looking
    /// verdict on a healthy machine, which is the hardest kind of error to
    /// notice. An entry that cannot say where its figures came from cannot be
    /// checked by anybody later.
    #[serde(default)]
    pub source: String,
}

impl CpuSpec {
    pub fn key(&self) -> CpuKey {
        CpuKey {
            vendor: self.vendor,
            family: self.family,
            model: self.model,
            stepping: self.stepping,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SpecFile {
    entries: Vec<CpuSpec>,
}

/// What a processor could be, and what it can be held to.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecMatch {
    /// The guarantees to judge against. When more than one product shares this
    /// processor's identity, this is not any one of them: it is the weakest
    /// claim common to all of them.
    pub spec: CpuSpec,
    /// How many published products share the identity.
    pub candidates: usize,
}

impl SpecMatch {
    /// Whether exactly one product matches, so it can be named.
    pub fn is_exact(&self) -> bool {
        self.candidates == 1
    }

    /// How the processor should be described.
    ///
    /// Names the product only when the identity is unambiguous. Otherwise it
    /// says what was actually established, because "matched to Ryzen 7 4980U"
    /// on a 4800U is a claim LoadBear has no basis for and the user has no way
    /// to check.
    pub fn label(&self) -> String {
        if self.is_exact() {
            self.spec.name.clone()
        } else {
            format!(
                "{} cores / {} threads, one of {} models sharing this processor id",
                self.spec.cores, self.spec.threads, self.candidates
            )
        }
    }
}

/// Merge candidates into the weakest claim all of them support.
///
/// Each field is taken in the direction that makes a verdict *less* likely to
/// fire, so nothing is reported out of spec unless it is out of spec for every
/// product it could be.
fn conservative(candidates: &[&CpuSpec]) -> CpuSpec {
    let first = candidates[0].clone();
    if candidates.len() == 1 {
        return first;
    }

    let min_u32 = |f: fn(&CpuSpec) -> u32| candidates.iter().map(|s| f(s)).min().unwrap_or(0);

    CpuSpec {
        // Lowest guaranteed base: the clock verdict fires only below the
        // weakest promise on offer.
        base_mhz: min_u32(|s| s.base_mhz),
        boost_mhz: candidates.iter().filter_map(|s| s.boost_mhz).max(),
        tdp_watts: min_u32(|s| s.tdp_watts),
        // Widest power band, so neither power verdict fires on a boundary that
        // only some candidates set.
        ctdp_min_watts: candidates.iter().filter_map(|s| s.ctdp_min_watts).min(),
        ctdp_max_watts: candidates.iter().filter_map(|s| s.ctdp_max_watts).max(),
        // Highest junction limit, so thermal headroom is judged generously.
        tjmax_c: candidates
            .iter()
            .filter_map(|s| s.tjmax_c)
            .fold(None, |acc: Option<f32>, v| {
                Some(acc.map_or(v, |a| a.max(v)))
            }),
        name: format!("{} and {} similar", first.name, candidates.len() - 1),
        ..first
    }
}

#[derive(Debug, Clone)]
pub struct SpecDb {
    entries: Vec<CpuSpec>,
}

const EMBEDDED: &str = include_str!("../data/cpu-specs.json");

impl SpecDb {
    /// Load the database compiled into the binary.
    ///
    /// LoadBear works offline. There is no runtime network dependency.
    pub fn embedded() -> Result<Self, SpecError> {
        let file: SpecFile = serde_json::from_str(EMBEDDED)?;
        Ok(Self {
            entries: file.entries,
        })
    }

    /// Find the specification for a CPU.
    ///
    /// Returns `None` for parts that are not in the database, which is an
    /// expected and common outcome rather than an error. OEM-exclusive CPUs
    /// frequently have no published specification anywhere. Callers must fall
    /// back to values read from the chip itself.
    pub fn lookup(&self, key: &CpuKey) -> Option<&CpuSpec> {
        self.entries.iter().find(|spec| spec.key() == *key)
    }

    /// Resolve a processor to the guarantees it can be held to.
    ///
    /// # Why this is not a lookup
    ///
    /// CPUID does not identify a product. Family 23 model 96 stepping 1 is
    /// Renoir, and every Ryzen 4000U part shares it: the 4300U at a 2700 MHz
    /// base, the 4500U at 2300, the 4800U at 1800, the 4980U at 2000. Matching
    /// on the key alone and taking the first hit means a 4800U owner is told
    /// their base is 2700 and watches the strongest verdict LoadBear has fire
    /// forever on a healthy machine.
    ///
    /// Logical processor count separates most of a line, since those parts
    /// ship 4, 6, 8, 12 and 16 threads. It cannot separate every pair, so
    /// whatever remains is merged field by field into the weakest claim any
    /// candidate makes. A verdict then fires only when the machine is out of
    /// spec for *every* model it could possibly be, which is the same trade
    /// made everywhere else here: never a confident wrong answer, occasionally
    /// a missed one.
    pub fn resolve(&self, key: &CpuKey, threads: u8) -> Option<SpecMatch> {
        // Stepping is deliberately not matched. It is a silicon revision, and
        // no vendor publishes different base clocks, TDPs or junction limits
        // for one. Requiring it means an entry written from a machine with
        // stepping 1 silently fails to match the same product at stepping 0 or
        // 2, and the failure looks exactly like an unknown processor: every
        // published check quietly disabled, with nothing saying why.
        let by_key: Vec<&CpuSpec> = self
            .entries
            .iter()
            .filter(|s| s.vendor == key.vendor && s.family == key.family && s.model == key.model)
            .collect();
        if by_key.is_empty() {
            return None;
        }

        // Narrow by thread count only if it leaves something. A machine whose
        // affinity mask hides processors would otherwise match nothing at all,
        // and a wider candidate set is safe because merging is conservative.
        let narrowed: Vec<&CpuSpec> = by_key
            .iter()
            .copied()
            .filter(|s| s.threads == threads)
            .collect();
        let candidates = if narrowed.is_empty() {
            by_key
        } else {
            narrowed
        };

        Some(SpecMatch {
            spec: conservative(&candidates),
            candidates: candidates.len(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> SpecDb {
        SpecDb::embedded().expect("embedded database must parse")
    }

    #[test]
    fn looks_up_a_known_cpu_by_cpuid_key() {
        let db = db();
        let key = CpuKey {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
        };
        let spec = db.lookup(&key).expect("4980U must be present");
        assert_eq!(spec.base_mhz, 2000);
        assert_eq!(spec.cores, 8);
        assert_eq!(spec.threads, 16);
    }

    #[test]
    fn returns_none_for_an_unknown_cpu() {
        let db = db();
        let key = CpuKey {
            vendor: Vendor::Other,
            family: 999,
            model: 999,
            stepping: 0,
        };
        assert!(db.lookup(&key).is_none());
    }

    #[test]
    fn exposes_the_configurable_tdp_band_when_published() {
        let db = db();
        let key = CpuKey {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
        };
        let spec = db.lookup(&key).unwrap();
        assert_eq!(spec.ctdp_min_watts, Some(10));
        assert_eq!(spec.ctdp_max_watts, Some(25));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn renoir(name: &str, base: u32, cores: u8, threads: u8) -> CpuSpec {
        CpuSpec {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
            name: name.to_string(),
            base_mhz: base,
            boost_mhz: Some(4400),
            tdp_watts: 15,
            ctdp_min_watts: Some(10),
            ctdp_max_watts: Some(25),
            tjmax_c: Some(105.0),
            cores,
            threads,
            source: "test fixture".to_string(),
        }
    }

    /// The whole Ryzen 4000U line shares one CPUID.
    fn line() -> SpecDb {
        SpecDb {
            entries: vec![
                renoir("AMD Ryzen 3 4300U", 2700, 4, 4),
                renoir("AMD Ryzen 5 4500U", 2300, 6, 6),
                renoir("AMD Ryzen 5 4600U", 2100, 6, 12),
                renoir("AMD Ryzen 7 4700U", 2000, 8, 8),
                renoir("AMD Ryzen 7 4800U", 1800, 8, 16),
                renoir("AMD Ryzen 7 4980U", 2000, 8, 16),
            ],
        }
    }

    fn key() -> CpuKey {
        CpuKey {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
        }
    }

    #[test]
    fn thread_count_identifies_most_of_a_line_exactly() {
        let db = line();
        for (threads, name) in [
            (4u8, "AMD Ryzen 3 4300U"),
            (6, "AMD Ryzen 5 4500U"),
            (12, "AMD Ryzen 5 4600U"),
            (8, "AMD Ryzen 7 4700U"),
        ] {
            let m = db.resolve(&key(), threads).expect("must resolve");
            assert!(m.is_exact(), "{threads} threads should be unambiguous");
            assert_eq!(m.label(), name);
        }
    }

    #[test]
    fn an_ambiguous_pair_is_judged_against_the_weakest_promise() {
        // 4800U and 4980U are both 8C/16T with different base clocks. Picking
        // either one names a product the machine might not be; taking the
        // lower base means the clock verdict fires only when the machine is
        // out of spec whichever of the two it is.
        let m = line().resolve(&key(), 16).expect("must resolve");
        assert_eq!(m.candidates, 2);
        assert!(!m.is_exact());
        assert_eq!(m.spec.base_mhz, 1800, "the weaker of the two guarantees");
    }

    #[test]
    fn an_ambiguous_match_does_not_name_a_product() {
        // The complaint this fixes. "matched to AMD Ryzen 7 4980U" shown to a
        // 4800U owner is a claim with nothing behind it.
        let m = line().resolve(&key(), 16).unwrap();
        let label = m.label();
        assert!(!label.contains("4980U"), "must not pick a side: {label}");
        assert!(!label.contains("4800U"), "must not pick a side: {label}");
        assert!(
            label.contains("16 threads"),
            "should say what it does know: {label}"
        );
    }

    #[test]
    fn the_widest_power_band_and_highest_thermal_limit_are_taken() {
        // Every merged field must make a verdict less likely, never more.
        let mut db = line();
        db.entries[4].ctdp_min_watts = Some(12);
        db.entries[4].ctdp_max_watts = Some(20);
        db.entries[4].tjmax_c = Some(95.0);
        let m = db.resolve(&key(), 16).unwrap();
        assert_eq!(m.spec.ctdp_min_watts, Some(10), "lowest floor");
        assert_eq!(m.spec.ctdp_max_watts, Some(25), "highest ceiling");
        assert_eq!(m.spec.tjmax_c, Some(105.0), "highest junction limit");
    }

    #[test]
    fn a_hidden_processor_count_still_resolves_rather_than_failing() {
        // An affinity mask can make a machine report fewer threads than the
        // part has. Matching nothing would silently disable every published
        // check, so the key-only candidates are used and merged instead.
        let m = line().resolve(&key(), 3).expect("must still resolve");
        assert_eq!(m.candidates, 6);
        assert_eq!(m.spec.base_mhz, 1800, "weakest across the whole line");
    }

    #[test]
    fn a_different_silicon_revision_still_resolves() {
        // Steppings vary across a production run and vendors publish one set
        // of figures for the product regardless. Matching on it turns a known
        // processor into an unknown one for no reason.
        let other_stepping = CpuKey {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 0,
        };
        let m = line()
            .resolve(&other_stepping, 8)
            .expect("a stepping the database has never seen must still resolve");
        assert_eq!(m.label(), "AMD Ryzen 7 4700U");
    }

    #[test]
    fn an_unknown_processor_resolves_to_nothing() {
        let unknown = CpuKey {
            vendor: Vendor::Amd,
            family: 99,
            model: 1,
            stepping: 0,
        };
        assert!(line().resolve(&unknown, 16).is_none());
    }

    #[test]
    fn the_shipped_database_has_no_ambiguous_entries_it_cannot_separate() {
        // A guard on the data rather than the code. Adding two products that
        // share a CPUID *and* a thread count is allowed, but it silently costs
        // precision, so it should be a deliberate choice rather than a
        // surprise found later on somebody else's machine.
        let db = SpecDb::embedded().expect("must parse");
        let mut seen: Vec<(CpuKey, u8)> = Vec::new();
        for e in &db.entries {
            let id = (e.key(), e.threads);
            assert!(
                !seen.contains(&id),
                "{} shares a processor id and thread count with an earlier entry, \
                 so neither can be named exactly",
                e.name
            );
            seen.push(id);
        }
    }
}

#[cfg(test)]
mod data_tests {
    use super::*;

    /// Guards on the shipped data rather than the code.
    ///
    /// These catch the class of mistake that is otherwise invisible: a figure
    /// typed wrongly reads as a perfectly ordinary processor until somebody
    /// with that chip sees a verdict fire forever on a healthy machine.
    #[test]
    fn every_entry_records_where_its_figures_came_from() {
        for e in &SpecDb::embedded().unwrap().entries {
            assert!(
                !e.source.trim().is_empty(),
                "{} has no source. Every figure must be traceable to the vendor.",
                e.name
            );
        }
    }

    #[test]
    fn every_published_figure_is_physically_plausible() {
        for e in &SpecDb::embedded().unwrap().entries {
            assert!(
                (400..=6000).contains(&e.base_mhz),
                "{} has a base clock of {} MHz",
                e.name,
                e.base_mhz
            );
            if let Some(boost) = e.boost_mhz {
                assert!(
                    boost >= e.base_mhz,
                    "{} boosts to {boost} below its {} MHz base",
                    e.name,
                    e.base_mhz
                );
            }
            assert!(
                (1..=400).contains(&e.tdp_watts),
                "{} has a TDP of {} W",
                e.name,
                e.tdp_watts
            );
            if let (Some(lo), Some(hi)) = (e.ctdp_min_watts, e.ctdp_max_watts) {
                assert!(
                    lo <= hi,
                    "{} has a configurable band of {lo} to {hi} W",
                    e.name
                );
            }
            if let Some(t) = e.tjmax_c {
                assert!(
                    (60.0..=120.0).contains(&t),
                    "{} has a junction limit of {t} C",
                    e.name
                );
            }
            assert!(e.cores >= 1, "{} has no cores", e.name);
            assert!(
                e.threads >= e.cores,
                "{} has {} threads across {} cores",
                e.name,
                e.threads,
                e.cores
            );
        }
    }
}

/// Plausible range for anything claiming to be a base clock, in MHz.
const PLAUSIBLE_BASE_MHZ: std::ops::RangeInclusive<u32> = 400..=6000;

/// How far the two independent sources may disagree and still be believed.
///
/// They are both meant to be the same published figure, so any real
/// disagreement means one of them is not the base clock at all.
const BASE_AGREEMENT_MHZ: u32 = 50;

/// The base clock as the machine itself reports it, when it can be trusted.
///
/// # Why this exists
///
/// `base_mhz` is the load bearing number in the whole product, and reaching it
/// through the specification database means LoadBear has nothing to say on any
/// processor nobody has hand-entered. That is most of them.
///
/// The operating system already publishes the figure. Windows reports a
/// nominal frequency that the performance counters are expressed as a
/// percentage of, and it read exactly 2000 MHz on a Ryzen 7 4980U across every
/// sample at every load, matching the published base clock. So the database is
/// better treated as an enhancement, carrying the things the machine genuinely
/// cannot supply, rather than as a prerequisite for saying anything at all.
///
/// # Why it is cross-checked
///
/// On some systems the nominal figure is a marketing number rather than the
/// base clock. Many parts also carry the frequency in their CPUID brand
/// string, as in `i7-8550U CPU @ 1.80GHz`, which is a second and independent
/// statement of the same thing. Where both exist they must agree, and where
/// they do not, neither is trusted: a wrong base clock produces a permanent
/// verdict on a healthy machine, which is the failure this codebase exists to
/// avoid.
///
/// Where only the operating system offers a figure it is used on its own, as
/// on OEM parts whose brand string carries no frequency at all.
pub fn reported_base_mhz(os_nominal_mhz: u32, brand: Option<&str>) -> Option<u32> {
    if !PLAUSIBLE_BASE_MHZ.contains(&os_nominal_mhz) {
        return None;
    }
    match brand.and_then(base_from_brand) {
        Some(from_brand) => {
            let gap = os_nominal_mhz.abs_diff(from_brand);
            (gap <= BASE_AGREEMENT_MHZ).then_some(os_nominal_mhz)
        }
        None => Some(os_nominal_mhz),
    }
}

/// Pull a base frequency out of a CPUID brand string, if it states one.
///
/// Intel writes `Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz`. AMD usually does
/// not, and OEM exclusive parts frequently carry no model number either, so an
/// absent figure is ordinary rather than a failure.
pub fn base_from_brand(brand: &str) -> Option<u32> {
    let tail = brand.split('@').nth(1)?.trim();
    let digits: String = tail
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = digits.parse().ok()?;
    let lower = tail.to_ascii_lowercase();

    let mhz = if lower.contains("ghz") {
        (value * 1000.0).round() as u32
    } else if lower.contains("mhz") {
        value.round() as u32
    } else {
        return None;
    };

    PLAUSIBLE_BASE_MHZ.contains(&mhz).then_some(mhz)
}

#[cfg(test)]
mod reported_base_tests {
    use super::*;

    #[test]
    fn a_brand_string_frequency_is_read_in_either_unit() {
        assert_eq!(
            base_from_brand("Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz"),
            Some(1800)
        );
        assert_eq!(base_from_brand("Some CPU @ 2400MHz"), Some(2400));
    }

    #[test]
    fn a_brand_string_without_a_frequency_yields_nothing() {
        // The real one from the machine this was built on. OEM parts often
        // carry no model number and no frequency at all.
        assert_eq!(
            base_from_brand("AMD Ryzen 7 Microsoft Surface (R) Edition"),
            None
        );
        assert_eq!(
            base_from_brand("AMD Ryzen 7 4980U with Radeon Graphics"),
            None
        );
    }

    #[test]
    fn the_operating_systems_figure_is_used_when_it_is_the_only_one() {
        // Measured: Windows reported a nominal 2000 MHz on a Ryzen 7 4980U in
        // every sample at every load, matching the published base clock, on a
        // machine whose brand string states no frequency.
        assert_eq!(
            reported_base_mhz(2000, Some("AMD Ryzen 7 Microsoft Surface (R) Edition")),
            Some(2000)
        );
        assert_eq!(reported_base_mhz(2000, None), Some(2000));
    }

    #[test]
    fn two_sources_that_agree_are_believed() {
        assert_eq!(
            reported_base_mhz(1800, Some("Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz")),
            Some(1800)
        );
    }

    #[test]
    fn two_sources_that_disagree_are_both_discarded() {
        // One of them is not the base clock, and there is no way to tell which.
        // Believing either produces a permanent verdict on a healthy machine.
        assert_eq!(
            reported_base_mhz(3600, Some("Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz")),
            None
        );
    }

    #[test]
    fn an_implausible_figure_is_refused() {
        assert_eq!(reported_base_mhz(0, None), None);
        assert_eq!(reported_base_mhz(99_000, None), None);
    }
}
