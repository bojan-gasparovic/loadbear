//! Step-by-step probe of the temperature path.
//!
//! Reports where it fails rather than just that it failed, because "no
//! temperature" has at least four distinct causes and they need different fixes.

use loadbear_sensors_windows::amd::{decode_ccd_temp, decode_die_temp, is_plausible_celsius};
use loadbear_sensors_windows::cpuid::current_cpu_key;
use loadbear_sensors_windows::pawnio::PawnIo;

const MODULE_AMD17: &[u8] = include_bytes!("../../modules/AMDFamily17.bin");
const SMN_TEMP_BASE: u64 = 0x0005_9800;
const RENOIR_CCD_OFFSET: u64 = 0x154;

fn main() {
    println!();

    print!("1. CPU identified ......... ");
    let Some(key) = current_cpu_key() else {
        println!("FAILED, CPUID unavailable");
        return;
    };
    println!(
        "ok  vendor {:?}, family {}, model {}, stepping {}",
        key.vendor, key.family, key.model, key.stepping
    );

    print!("2. PawnIOLib.dll loads .... ");
    let pawn = match PawnIo::open() {
        Ok(p) => {
            println!("ok");
            p
        }
        Err(e) => {
            println!("FAILED  {e}");
            println!();
            println!("   The driver service can be running while the library is still");
            println!("   unreachable. PawnIOLib.dll installs to C:\\Program Files\\PawnIO,");
            println!("   which is not on the default DLL search path.");
            return;
        }
    };

    print!("3. AMDFamily17 loads ...... ");
    if let Err(e) = pawn.load_module(MODULE_AMD17) {
        println!("FAILED  {e}");
        println!();
        println!("   The module's own main() rejects anything outside family 0x17 to 0x1A.");
        return;
    }
    println!("ok");

    print!("4. SMN read returns ....... ");
    let die = match pawn.execute("ioctl_read_smn", &[SMN_TEMP_BASE], 1) {
        Ok(v) => {
            println!("ok  raw 0x{:08X}", v.first().copied().unwrap_or(0));
            v
        }
        Err(e) => {
            println!("FAILED  {e}");
            return;
        }
    };

    println!();
    if let Some(&raw) = die.first() {
        let c = decode_die_temp(raw as u32);
        println!(
            "   Die temperature: {c:.1} C  ({})",
            if is_plausible_celsius(c) {
                "plausible"
            } else {
                "IMPLAUSIBLE, the register path is wrong"
            }
        );
    }

    let mut found = 0;
    for ccd in 0..8u64 {
        let offset = SMN_TEMP_BASE + RENOIR_CCD_OFFSET + ccd * 4;
        if let Ok(out) = pawn.execute("ioctl_read_smn", &[offset], 1) {
            if let Some(&raw) = out.first() {
                if let Some(c) = decode_ccd_temp(raw as u32) {
                    println!("   CCD{ccd}: {c:.1} C");
                    found += 1;
                }
            }
        }
    }
    if found == 0 {
        println!("   No CCD sensors reported a valid bit, which is expected on a single-die part.");
    }
    println!();
}
