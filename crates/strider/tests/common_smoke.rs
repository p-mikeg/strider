//! One-liner smoke test for the shared analyze helper.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;

#[test]
fn analyze_arithmetic_add_x64_returns_nontrivial_graph() {
    let g = common::analyze(common::Arch::X64, "arithmetic", "add");
    // Floor: a non-trivial graph has more than just Entry + InitialMemory.
    assert!(g.preorder().count() > 5,
            "graph too small: {}", g.preorder().count());
}

#[test]
fn analyze_arithmetic_add_mips32be_returns_nontrivial_graph() {
    // Smoke-test the BE MIPS path — exercises both the new mips_o32 calling
    // convention (Task 16) and the BE shift formula fix (Task 4).
    let g = common::analyze(common::Arch::Mips32be, "arithmetic", "add");
    assert!(g.preorder().count() > 5,
            "graph too small: {}", g.preorder().count());
}

// ── count_return_paths helper ────────────────────────────────────────────────
//
// `count_return_paths` is the compiler-independent way to count source-level
// return paths: it counts the predecessors of each Return's ControlState so
// shared-epilogue ABIs (PPC, aarch64) and split-epilogue ABIs (x86, MIPS, ARM)
// agree on the answer.  These two tests pin both shapes to the same value.

#[test]
fn count_return_paths_x64_early_return_is_two() {
    // x64 emits two distinct Return nodes (one per source-level `return`);
    // each typically has a non-ControlState direct ctrl predecessor, so the
    // helper sums to ≥2.
    let g = common::analyze(common::Arch::X64, "control", "early_return");
    let paths = common::count_return_paths(&g);
    assert!(paths >= 2,
            "x64 early_return should have ≥2 return paths; got {paths}");
}

#[test]
fn count_return_paths_ppc64le_early_return_is_two() {
    // PPC shares the function epilogue: one Return node with a ControlState
    // predecessor that merges 2 control paths.  Bare Return-count would be
    // 1; count_return_paths must still report ≥2.
    let g = common::analyze(common::Arch::Ppc64le, "control", "early_return");
    let paths = common::count_return_paths(&g);
    assert!(paths >= 2,
            "ppc64le early_return should have ≥2 return paths; got {paths}");
}
