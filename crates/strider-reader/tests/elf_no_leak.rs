#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Regression: `load_elf` must NOT leak its backing bytes.  It used to
//! `Box::leak` the whole file to fabricate a `'static` `object::File`, so
//! every call leaked one file-sized buffer — RSS grew without bound in a
//! long-lived process (the strider-py `load_elf` path).  This loops the
//! loader far more times than its own working set and asserts resident
//! memory stays bounded.

use std::path::PathBuf;

fn elf_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.elf"))
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> u64 {
    // statm field 1 (resident pages) * page size.
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

#[cfg(target_os = "linux")]
#[test]
fn load_elf_does_not_leak_backing_bytes() {
    let path = elf_path("x64", "arithmetic");
    if !path.exists() {
        // Skip cleanly when fixtures aren't built (matches the loader tests).
        return;
    }
    let file_size = std::fs::metadata(&path).unwrap().len();

    // Warm up (allocator arenas, one-time parse tables) so the baseline is
    // steady before measuring.
    for _ in 0..20 {
        drop(strider_reader::load_elf(&path).expect("load_elf"));
    }
    let base = rss_bytes();

    const ITERS: u64 = 400;
    for _ in 0..ITERS {
        drop(strider_reader::load_elf(&path).expect("load_elf"));
    }
    let growth = rss_bytes().saturating_sub(base);

    // Pre-fix, each call leaks ~`file_size`, so growth ≈ ITERS * file_size.
    // Post-fix the bytes free on drop, so growth is allocator noise.  Gate at
    // a small multiple of ONE file so a genuine per-call leak trips it while
    // benign fragmentation does not.
    let budget = file_size.saturating_mul(8).max(4 * 1024 * 1024);
    assert!(
        growth < budget,
        "RSS grew {growth} bytes over {ITERS} load_elf calls (file {file_size} B, \
         budget {budget} B) — load_elf is leaking its backing bytes",
    );
}
