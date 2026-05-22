//! Integration tests for the jump-table classifier.
//!
//! Each test builds a synthetic `BuiltFunctionGraph` via the fixture
//! helpers in `common::indirect_resolve_helpers`, runs the stable optimiser
//! subset (matching what intermediate orchestrator iterations will
//! see), then invokes [`classify_anchor`] on the placeholder
//! Return's value-input.  The fixtures are FunctionBuilder-driven
//! rather than going through real arch bytes + the cfg builder
//! because:
//!
//!   * Real jump-table-shaped bytes that lift cleanly through Sleigh
//!     are arch-specific and verbose; the FunctionBuilder API
//!     reproduces the shape directly.
//!   * The classifier under test is graph-shape-driven, not
//!     CFG-driven; every byte path eventually goes through
//!     `analyze_cfg` (which returns an `AnalyzeOutcome` carrying the
//!     `unresolved_branches` list) and builds the same IR shapes.
//!   * This file proves the classifier shape + bound + read mechanics
//!     work in isolation; real-binary integration tests for jump
//!     tables live in `indirect_branch.rs` and `jump_table_lifting.rs`.
//!
//! The rom is a toy `TableRom` that returns successive 4-byte values
//! at fixed offsets; it stands in for the ELF's `.rodata` view that
//! production callers wire through `OptionsBuilder::set_read_only_memory`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use strider_analyze::opt::indirect_branch_resolve::{ResolvedTargets, classify_anchor};
use strider_analyze::opt::analyze_known_bits;
use strider_analyze::pattern::RewriteCtxView;

/// Test helper: recomputes `analyze_known_bits` and calls
/// `classify_anchor` with the supplied rom and no SP varnode.  Mirrors
/// the production single-anchor convenience these tests used to call
/// directly.
fn classify_anchor_with_rom(
    view: RewriteCtxView<'_>,
    anchor: strider_ir::node::NodeOutputId,
    lr: Option<rsleigh::Vn>,
    rom: Option<&dyn strider_analyze::opt::ReadOnlyMemory>,
) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = analyze_known_bits(view)?;
    Ok(classify_anchor(view, anchor, lr, rom, None, &known))
}

use common::indirect_resolve_helpers::{
    build_jump_table_known_bits_scenario, build_jump_table_predecessor_if_scenario,
    build_jump_table_unbounded_scenario, build_non_jump_table_load_scenario,
};

/// Toy `ReadOnlyMemory` impl: returns successive 4-byte values at
/// `base + i * stride`, capped at `entries.len()`.  Reads outside
/// the configured table range return None.
///
/// Used by the integration tests to provide the rom that the
/// classifier reads jump-table entries from.
struct TableRom {
    base: u64,
    stride: u64,
    entries: Vec<u64>,
    size: usize,
}

impl strider_analyze::opt::ReadOnlyMemory for TableRom {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        if size != self.size {
            return None;
        }
        if addr < self.base {
            return None;
        }
        let offset = addr - self.base;
        if self.stride == 0 {
            return None;
        }
        if !offset.is_multiple_of(self.stride) {
            return None;
        }
        let idx = (offset / self.stride) as usize;
        self.entries.get(idx).copied()
    }
}

/// Same as `TableRom` but only the first `cutoff` entries can be
/// read; subsequent reads return `None`.  Drives the partial-read
/// soundness test.
struct PartialRom {
    inner: TableRom,
    cutoff: usize,
}

impl strider_analyze::opt::ReadOnlyMemory for PartialRom {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        if addr < self.inner.base {
            return None;
        }
        let offset = addr - self.inner.base;
        if self.inner.stride == 0 {
            return None;
        }
        let idx = (offset / self.inner.stride) as usize;
        if idx >= self.cutoff {
            return None;
        }
        self.inner.read(addr, size)
    }
}

/// Spec catalogue test #13 (headline).
///
/// `idx & 0x7; load[base + idx*stride]; jmp *target` →
/// `Multiple([table[0], …, table[7]])`.
///
/// `KnownBits` proves `idx <= 0x7` from the AND-mask, so the bound
/// is 8.  The classifier reads 8 entries from rom and produces a
/// canonical (sorted, deduplicated) Multiple.
#[test]
fn jump_table_known_bits_bound_resolves_to_multiple() {
    let base = 0x4000;
    let stride = 4;
    let idx_mask = 0x7u64;
    let entries = vec![0x100, 0x200, 0x300, 0x400, 0x500, 0x600, 0x700, 0x800];
    let rom = TableRom {
        base,
        stride,
        entries: entries.clone(),
        size: 4,
    };
    let (graph, anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts, entries,
                "Multiple targets must equal table entries in canonical order",
            );
        }
        other => panic!("expected Multiple([0x100..0x800]); got {other:?}"),
    }
}

/// Spec catalogue test #14.
///
/// `if (idx < 4) { jmp *table[idx] }` → `Multiple([table[0], …,
/// table[3]])`.  Bound proved via the predecessor-If walk.
#[test]
fn jump_table_predecessor_if_bound_resolves_to_multiple() {
    let base = 0x5000;
    let stride = 4;
    let bound = 4u64;
    let entries = vec![0x100, 0x200, 0x300, 0x400];
    let rom = TableRom {
        base,
        stride,
        entries: entries.clone(),
        size: 4,
    };
    let (graph, anchor) = build_jump_table_predecessor_if_scenario(base, stride, bound);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, entries);
        }
        other => panic!("expected Multiple([0x100..0x400]); got {other:?}"),
    }
}

/// Same shape as the KnownBits test but the index has no AND-mask
/// and no predecessor If — it's just a register read.  The classifier
/// must return None.  A Multiple here would be unsound: the runtime
/// index could be any U32 value; we'd be enumerating an arbitrary
/// prefix of memory as "targets".
#[test]
fn jump_table_unbounded_idx_returns_none() {
    let base = 0x6000;
    let stride = 4;
    let rom = TableRom {
        base,
        stride,
        entries: vec![0x100, 0x200, 0x300, 0x400],
        size: 4,
    };
    let (graph, anchor) = build_jump_table_unbounded_scenario(base, stride);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "unbounded idx must NOT classify to Multiple — the orchestrator \
         will defer or surface UnresolvedIndirectBranch at fixed point",
    );
}

/// Bounded shape (KnownBits-provable), but `rom = None`.  Without rom
/// we can't read entries, so the classifier returns None.  The
/// orchestrator might later try again once a rom is wired (in
/// production it always is).
#[test]
fn jump_table_no_rom_returns_none() {
    let base = 0x7000;
    let stride = 4;
    let idx_mask = 0x3u64;
    let (graph, anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, /* rom */ None).expect("classify_anchor_with_rom");
    assert_eq!(result, None);
}

/// Bounded shape, rom covers some but not all entries.  Classifier
/// must return None — a partial Multiple would induce a CFG missing
/// real edges.  See the soundness rationale in `jump_table.rs`'s
/// `read_table_entries` doc comment.
#[test]
fn jump_table_partial_rom_returns_none() {
    let base = 0x8000;
    let stride = 4;
    let idx_mask = 0x7u64; // bound = 8
    let rom = PartialRom {
        inner: TableRom {
            base,
            stride,
            entries: vec![0x100, 0x200, 0x300, 0x400, 0x500, 0x600, 0x700, 0x800],
            size: 4,
        },
        // Rom serves the first 4 of 8 entries.
        cutoff: 4,
    };
    let (graph, anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "partial rom read must fail closed (None), NOT produce a partial \
         Multiple — that would silently omit real runtime targets",
    );
}

/// Spec catalogue test #zero-entries edge case.
///
/// Pinned semantic: when the proved bound is zero (`bound == 0`),
/// the classifier returns `None` rather than a `Multiple([])`.
/// Rationale:
///
///   * `Multiple([])` would tell the orchestrator "the indirect
///     branch has zero reachable targets" — i.e. the branch is dead
///     code.  But our classifier proves an *upper* bound; an upper
///     bound of zero only holds if the branch is actually
///     unreachable, and we have no separate proof of unreachability.
///     Misclassifying a reachable branch as dead would silently
///     truncate the CFG.
///   * `None` is the conservative answer: defer.  The orchestrator
///     either tries again with a stronger bound or surfaces
///     `UnresolvedIndirectBranch` at fixed point.
///
/// Trigger: a predecessor-If with bound = 0 (i.e. `if (idx < 0)`).
/// In real code this is dead-by-construction, but the classifier
/// must still answer cleanly.
#[test]
fn jump_table_zero_bound_returns_none() {
    let base = 0xa000;
    let stride = 4;
    let bound = 0u64; // `idx < 0` — never true for unsigned idx
    let rom = TableRom {
        base,
        stride,
        entries: vec![0x100, 0x200, 0x300, 0x400],
        size: 4,
    };
    let (graph, anchor) = build_jump_table_predecessor_if_scenario(base, stride, bound);
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "bound = 0 must return None — Multiple([]) would wrongly imply \
         the branch is dead code",
    );
}

/// `Load` whose address shape isn't `IntAdd(IntConst, IntMul(idx,
/// IntConst))` — e.g. `Load(IntConst(addr))` for a simple global
/// read.  The classifier's Load arm must fall through to None: it's
/// neither a jump-table dispatch nor a recognised non-Load shape, so
/// no resolution is sound.
#[test]
fn non_jump_table_load_shape_falls_through() {
    // Build a `Load(IntConst(addr))` — no IntAdd, no IntMul.
    let (graph, anchor) = build_non_jump_table_load_scenario();
    // Provide a rom anyway so the test pins that the falsethrough
    // is structural, not rom-driven: even with an idle rom the
    // classifier still returns None for the wrong shape.
    let rom = TableRom {
        base: 0,
        stride: 4,
        entries: vec![0x100, 0x200],
        size: 4,
    };
    let result = classify_anchor_with_rom((&graph).into(), anchor, None, Some(&rom)).expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "non-jump-table Load shape must fall through to None, NOT \
         attempt rom enumeration",
    );
}
