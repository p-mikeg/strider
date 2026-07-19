#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Regression: `load_elf` must not leak its backing bytes. It used to
//! `Box::leak` the whole file to fabricate a `'static` `object::File`, leaking
//! one file-sized buffer per call. This loops the loader well past its own
//! working set and asserts resident memory stays bounded.

use std::path::PathBuf;

fn elf_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.elf"))
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> u64 {
    // statm field 1 is resident pages.
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

#[cfg(target_os = "linux")]
#[test]
fn load_elf_does_not_leak_backing_bytes() {
    let path = elf_path("x64", "arithmetic");
    if !path.exists() {
        // Skip cleanly when fixtures aren't built.
        return;
    }
    let file_size = std::fs::metadata(&path).unwrap().len();

    // Warm up allocator arenas and one-time parse tables so the baseline is
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

    // A per-call leak would grow RSS by roughly ITERS * file_size; without one
    // the growth is allocator noise. Gating at a small multiple of a single
    // file separates the two.
    let budget = file_size.saturating_mul(8).max(4 * 1024 * 1024);
    assert!(
        growth < budget,
        "RSS grew {growth} bytes over {ITERS} load_elf calls (file {file_size} B, \
         budget {budget} B) — load_elf is leaking its backing bytes",
    );
}
