//! Rewrite a boot.cfg blob with a different first-launch delay, keeping everything else.
//!
//! Usage: setdelay <in.cfg> <out.cfg> <delay_ms>
//!
//! Uses BootConfig::decode/encode so the CRC is recomputed correctly. Sets `first_delay_ms`
//! directly and encodes — encode() does not re-apply the 10 s home floor (only `add_home` does),
//! so this can write a value below it deliberately, to launch the home screen earlier in the boot.
use std::fs;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (inp, outp, ms) = (&a[1], &a[2], a[3].parse::<u32>().unwrap());
    let bytes = fs::read(inp).unwrap();
    let mut c = symbian_bootcfg::config::BootConfig::decode(&bytes).expect("decode");
    eprintln!("before: first_delay_ms={} entries={}", c.first_delay_ms, c.entries.len());
    for e in &c.entries { eprintln!("  entry uid=0x{:08x} '{}' delay={}ms", e.uid3, e.name, e.delay_ms); }
    c.first_delay_ms = ms;
    let out = c.encode();
    fs::write(outp, &out).unwrap();
    // round-trip check
    let back = symbian_bootcfg::config::BootConfig::decode(&out).expect("re-decode");
    eprintln!("after:  first_delay_ms={} ({} bytes) re-decode ok", back.first_delay_ms, out.len());
    assert_eq!(back.first_delay_ms, ms);
}
