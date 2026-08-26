//! Safe wrappers over the shim's memory readings.
//!
//! Two figures, for an app that wants to know how much room is left: device-wide free RAM
//! (pressure) and this process's own heap (a coarse "am I the one growing?" check). As with
//! [`crate::process`], the one `unsafe` block stays here so the callers above
//! (`#![forbid(unsafe_code)]`) never touch the raw ABI. On the host the shim functions return
//! `SHIM_ERR_NOT_READY`, so every reading fails cleanly under `cargo test` and a caller does
//! nothing rather than acting on a lie.
//!
//! There is deliberately no per-app RAM reading: Symbian has no public way to ask another
//! process how much it holds, so a caller acts on the device-wide figure plus a policy,
//! never on attributed consumption.

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Free device RAM, in KiB. The number to compare against a watermark of your own.
pub fn free_kb() -> Result<u32> {
    // SAFETY: no arguments; the shim reads HAL and returns KiB or a negative error.
    read(unsafe { sys::shim_mem_free_kb() })
}

/// Total device RAM, in KiB. Constant for a given handset; useful for a percentage.
pub fn total_kb() -> Result<u32> {
    // SAFETY: as above.
    read(unsafe { sys::shim_mem_total_kb() })
}

/// Bytes this process has allocated, in KiB.
pub fn heap_used_kb() -> Result<u32> {
    // SAFETY: as above; reads the current thread's allocator.
    read(unsafe { sys::shim_heap_used_kb() })
}

/// A reading is a non-negative KiB count or a negative Symbian error.
fn read(rc: i32) -> Result<u32> {
    if rc < 0 {
        Err(Error::from_code(rc))
    } else {
        Ok(rc as u32)
    }
}

/// The C libraries' heap: bytes committed to the process, and bytes live inside it.
///
/// A separate reading from [`heap_used_kb`], and the distinction matters: that one reads the
/// *calling thread's* allocator, which is the process heap, while the five vendored NetSurf
/// libraries allocate on a chunk heap of their own. Every memory figure this project logged was
/// therefore blind to the code most likely to be growing.
///
/// Returns `(committed, live)`. Both zero before anything has parsed.
pub fn c_heap() -> (u32, u32) {
    let mut size = 0i32;
    let mut allocated = 0i32;
    // SAFETY: two out-pointers into locals; the shim writes both or neither.
    unsafe { sys::shim_cheap_stats(&mut size, &mut allocated) };
    (size.max(0) as u32, allocated.max(0) as u32)
}

/// Give the C heap's unused tail back to the system, and report the bytes recovered.
///
/// `RHeap::Compress` cannot move live cells, so one allocation near the top pins everything under
/// it. The return value is what was actually recovered rather than a claim of success — which is
/// the difference between "the page was freed" and "the page was freed and the memory came back".
pub fn compress_c_heap() -> u32 {
    // SAFETY: no arguments; the shim is a no-op when the heap does not exist.
    unsafe { sys::shim_cheap_compress().max(0) as u32 }
}
