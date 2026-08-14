//! Identify the running CPU by the values CPUID reports.
//!
//! Brand strings are never used for lookup. They are unreliable in general and
//! least reliable exactly where it matters: the development machine reports
//! "AMD Ryzen 7 Microsoft Surface (R) Edition", which appears in no vendor
//! database anywhere.

use loadbear_core::{CpuKey, Vendor};
use raw_cpuid::CpuId;

/// Read the CPU key from CPUID.
///
/// `raw-cpuid` already folds the extended family and model fields into
/// `family_id()` and `model_id()`, so they are used directly. Adding
/// `extended_family_id()` on top double counts: the first version of this
/// function did exactly that and produced family 31 model 192 on a part that
/// is family 23 model 96, which silently missed every database entry.
pub fn current_cpu_key() -> Option<CpuKey> {
    let cpuid = CpuId::new();
    let vendor = match cpuid.get_vendor_info()?.as_str() {
        "GenuineIntel" => Vendor::Intel,
        "AuthenticAMD" => Vendor::Amd,
        _ => Vendor::Other,
    };
    let f = cpuid.get_feature_info()?;
    Some(CpuKey {
        vendor,
        family: f.family_id() as u32,
        model: f.model_id() as u32,
        stepping: f.stepping_id() as u32,
    })
}

/// The brand string, for display only. Never for lookup.
pub fn brand_string() -> Option<String> {
    CpuId::new()
        .get_processor_brand_string()
        .map(|b| b.as_str().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_cpu_is_identifiable() {
        let key = current_cpu_key().expect("CPUID must be available on any supported machine");
        assert!(
            matches!(key.vendor, Vendor::Intel | Vendor::Amd),
            "expected a known vendor, got {:?}",
            key.vendor
        );
        assert!(key.family > 0, "family should be populated");
    }

    #[test]
    fn the_extended_encoding_is_not_applied_twice() {
        // raw-cpuid already folds the extended fields in. Applying them again
        // yields family 31 model 192 on a Zen 2 part instead of 23 and 96, and
        // the failure is silent: lookup just returns None and every check that
        // needs published data quietly stops working.
        let Some(key) = current_cpu_key() else { return };
        assert!(
            key.family < 64,
            "family {} looks double counted",
            key.family
        );
        assert!(key.model < 256, "model {} looks double counted", key.model);
    }

    #[test]
    fn a_brand_string_is_available_for_display() {
        let b = brand_string().expect("a brand string should be readable");
        assert!(!b.is_empty());
    }
}
