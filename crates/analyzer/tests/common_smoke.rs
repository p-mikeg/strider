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
