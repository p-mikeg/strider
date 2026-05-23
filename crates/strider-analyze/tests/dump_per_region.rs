//! Integration test: `strider_analyze::dump_per_region` emits one HTML
//! file per region in `AnalyzeOutcome::region_exit_controls()`.
//!
//! Uses the same minimal `ud2`-style snippet as
//! `bug_on_lifts_cleanly.rs` so the test has no fixture-build
//! dependency, and asserts the disk side effects (file count, names,
//! and presence of the vendored viewer JS that signals a successful
//! HTML render).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::dump_helpers::{lift_ret_snippet_x86_64, unique_tmp_dir, VIEWER_JSON_ANCHOR};

#[test]
fn dump_per_region_writes_one_html_per_region() {
    // `ret` is a single-region function — the `analyze_cfg`'s outcome
    // carries one `RegionLiftHandles` entry, so `dump_per_region` must
    // emit exactly one HTML file.
    let (outcome, cfg) = lift_ret_snippet_x86_64();
    let exit_controls: Vec<_> = outcome.region_exit_controls().collect();
    assert!(
        !exit_controls.is_empty(),
        "expected at least one region for `ret`, got 0"
    );

    let tmp = unique_tmp_dir("dump-per-region");
    strider_analyze::dump_per_region(
        &outcome.graph,
        exit_controls.iter().copied(),
        outcome.lift_generation(),
        cfg.sleigh(),
        &tmp,
    )
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
    // DOT payload.  We don't assert engine choice here — the smallest
    // region trivially stays below the sfdp threshold.
    let first = tmp.join(&entries[0]);
    let html = std::fs::read_to_string(&first).expect("read html");
    assert!(
        html.contains(VIEWER_JSON_ANCHOR),
        "viewer JSON script missing from {}",
        first.display()
    );

    // Clean up the scratch dir on success — leave it on panic so a
    // developer can inspect the offending HTML.
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Pins the stale-id detection added alongside `Graph::generation`.
/// Lifting captures a generation snapshot in `AnalyzeOutcome`; a
/// subsequent `Graph::compact` bumps the live generation and
/// invalidates the `region_exit_controls` ids.  `dump_per_region` must
/// surface a typed error rather than silently rendering the wrong
/// region.
#[test]
fn dump_per_region_rejects_post_compaction() {
    let (mut outcome, cfg) = lift_ret_snippet_x86_64();
    let exit_controls: Vec<_> = outcome.region_exit_controls().collect();
    assert!(!exit_controls.is_empty());

    let lift_gen = outcome.lift_generation();
    outcome.graph.compact().expect("compact");
    assert_ne!(
        outcome.graph.generation(),
        lift_gen,
        "compact must bump generation",
    );

    let tmp = unique_tmp_dir("dump-per-region-stale");
    let err = strider_analyze::dump_per_region(
        &outcome.graph,
        exit_controls.iter().copied(),
        lift_gen,
        cfg.sleigh(),
        &tmp,
    )
    .expect_err("dump_per_region must reject post-compaction ids");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not match lift snapshot"),
        "unexpected error message: {msg}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
