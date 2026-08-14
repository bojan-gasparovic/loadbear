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
        Ok(Self { entries: file.entries })
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
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db.lookup(&key).expect("4980U must be present");
        assert_eq!(spec.base_mhz, 2000);
        assert_eq!(spec.cores, 8);
        assert_eq!(spec.threads, 16);
    }

    #[test]
    fn returns_none_for_an_unknown_cpu() {
        let db = db();
        let key = CpuKey { vendor: Vendor::Other, family: 999, model: 999, stepping: 0 };
        assert!(db.lookup(&key).is_none());
    }

    #[test]
    fn exposes_the_configurable_tdp_band_when_published() {
        let db = db();
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db.lookup(&key).unwrap();
        assert_eq!(spec.ctdp_min_watts, Some(10));
        assert_eq!(spec.ctdp_max_watts, Some(25));
    }
}
