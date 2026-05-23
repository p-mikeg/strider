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

use common::dump_helpers::{
    lift_branch_snippet_x86_64, lift_ret_snippet_x86_64, ScratchDir, VIEWER_JSON_ANCHOR,
};

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

    let scratch = ScratchDir::new("dump-per-region");
    let tmp = scratch.path();
    strider_analyze::dump_per_region(
        &outcome.graph,
        exit_controls.iter().copied(),
        outcome.lift_generation(),
        cfg.sleigh(),
        tmp,
    )
    .expect("dump_per_region");

    let entries: Vec<_> = std::fs::read_dir(tmp)
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
}

/// Thicker per-region coverage: a 2-region conditional-branch snippet
/// (`test rax,rax ; jz +1 ; ret ; ret`) lifts into two distinct
/// regions, each ending in its own `ret`.  Verifies `dump_per_region`
/// emits a file for each region — not just the trivial single-region
/// case — and that each file carries the viewer JSON anchor.
#[test]
fn dump_per_region_emits_one_html_for_each_branch_region() {
    let (outcome, cfg) = lift_branch_snippet_x86_64();
    let exit_controls: Vec<_> = outcome.region_exit_controls().collect();
    assert!(
        exit_controls.len() >= 2,
        "conditional-branch snippet must produce at least 2 regions, got {}",
        exit_controls.len(),
    );

    let scratch = ScratchDir::new("dump-per-region-branch");
    let tmp = scratch.path();
    strider_analyze::dump_per_region(
        &outcome.graph,
        exit_controls.iter().copied(),
        outcome.lift_generation(),
        cfg.sleigh(),
        tmp,
    )
    .expect("dump_per_region");

    let entries: Vec<_> = std::fs::read_dir(tmp)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("region_") && name.ends_with(".html"))
        .collect();

    assert_eq!(
        entries.len(),
        exit_controls.len(),
        "one HTML file per region expected; got {entries:?} for {} regions",
        exit_controls.len(),
    );

    // Every emitted file must contain the viewer anchor — catches a
    // regression where a "second" file gets created but truncated /
    // empty / missing the embedded DOT JSON.
    for name in &entries {
        let html = std::fs::read_to_string(tmp.join(name)).expect("read html");
        assert!(
            html.contains(VIEWER_JSON_ANCHOR),
            "viewer JSON script missing from {name}",
        );
    }
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

    let scratch = ScratchDir::new("dump-per-region-stale");
    let err = strider_analyze::dump_per_region(
        &outcome.graph,
        exit_controls.iter().copied(),
        lift_gen,
        cfg.sleigh(),
        scratch.path(),
    )
    .expect_err("dump_per_region must reject post-compaction ids");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not match lift snapshot"),
        "unexpected error message: {msg}",
    );
}
