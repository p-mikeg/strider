//! Integration test: `strider_analyze::dump_per_region` emits one HTML
//! file per region in `AnalyzeOutcome::region_exit_controls()`.
//!
//! Uses the same minimal `ud2`-style snippet as
//! `bug_on_lifts_cleanly.rs` so the test has no fixture-build
//! dependency, and asserts the disk side effects (file count, names,
//! and presence of the vendored viewer JS that signals a successful
//! HTML render).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use strider_lift::cfg::{Builder, OptionsBuilder};
use strider_target::SleighArch;

mod common;

#[test]
fn dump_per_region_writes_one_html_per_region() {
    let strider = common::strider_x86_64();
    let arch = SleighArch::x86_64();

    // `ret` is a single-region function — the `analyze_cfg`'s outcome
    // carries one `RegionLiftHandles` entry, so `dump_per_region` must
    // emit exactly one HTML file.
    let bytes = vec![0xc3u8]; // ret
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let cfg = Builder::for_arch(&arch, sleigh, entry, OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    let exit_controls: Vec<_> = outcome.region_exit_controls().collect();
    assert!(
        !exit_controls.is_empty(),
        "expected at least one region for `ret`, got 0"
    );

    // Avoid the `tempfile` crate dep — a unique subdir under the
    // process's temp dir is sufficient for a per-test scratch space.
    let tmp = std::env::temp_dir().join(format!(
        "strider-dump-per-region-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).expect("create tmp dir");
    strider_analyze::dump_per_region(&outcome.graph, exit_controls.iter().copied(), cfg.sleigh(), &tmp)
        .expect("dump_per_region");

    let entries: Vec<_> = std::fs::read_dir(&tmp)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("region_") && name.ends_with(".html"))
        .collect();

    assert_eq!(
        entries.len(),
        exit_controls.len(),
        "exactly one HTML file per region (got {entries:?})"
    );

    // Sanity-check the first file: it must contain the JSON-embedded
    // DOT payload (the viewer's `<script type=\"application/json\">`
    // element).  We don't assert engine choice here — the smallest
    // region trivially stays below the sfdp threshold.
    let first = tmp.join(&entries[0]);
    let html = std::fs::read_to_string(&first).expect("read html");
    assert!(
        html.contains("<script type=\"application/json\" id=\"dot-src\">"),
        "viewer JSON script missing from {}",
        first.display()
    );

    // Clean up the scratch dir on success — leave it on panic so a
    // developer can inspect the offending HTML.
    let _ = std::fs::remove_dir_all(&tmp);
}
