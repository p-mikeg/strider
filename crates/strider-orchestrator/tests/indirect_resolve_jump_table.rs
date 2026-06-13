//! Integration tests for the jump-table classifier.
//!
//! Each test builds a synthetic `Graph` via the fixture
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
//!     `build_ir` (which returns an `LiftOutcome` carrying the
//!     `unresolved_branches` list) and builds the same IR shapes.
//!   * This file proves the classifier shape + bound + read mechanics
//!     work in isolation; real-binary integration tests for jump
//!     tables live in `indirect_branch.rs` and `jump_table_lifting.rs`.
//!
//! The rom is a toy `TableRom` that returns successive 4-byte values
//! at fixed offsets; it stands in for the ELF's `.rodata` view that
//! production callers wire into the optimiser's `OptCtx` for
//! `LoadReadOnly`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use strider_cfg::ResolvedTargets;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::AliasMode;
use strider_orchestrator::opt::analyze_known_bits;
use strider_orchestrator::opt::classify_anchor;
use strider_orchestrator::opt::value_range::compute_value_ranges;

/// Test helper: recomputes `analyze_known_bits`, dominators, and ranges, then
/// calls `classify_anchor` on the fixture's sole `IndirectBranch` placeholder
/// (the classifier derives the dispatch anchor from the branch's slot-2 input).
fn classify_anchor_with_rom(
    view: &strider_ir::Function,
    rom: Option<&dyn strider_orchestrator::opt::ReadOnlyMemory>,
) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = analyze_known_bits(view)?;
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    let branch = view
        .walk()
        .find(|&n| matches!(view.node_kind(n), NodeKind::IndirectBranch))
        .expect("fixture has an IndirectBranch placeholder");
    Ok(classify_anchor(view, branch, rom, &mut ranges, AliasMode::StackGlobalDisjoint))
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

impl TableRom {
    fn resolve(&self, addr: u64, size: usize) -> Option<u64> {
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

impl strider_orchestrator::opt::ReadOnlyMemory for TableRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let size = buf.len();
        let value = self
            .resolve(addr, size)
            .ok_or_else(|| anyhow::anyhow!("TableRom: unmapped {addr:#x}"))?;
        buf.copy_from_slice(&value.to_le_bytes()[..size]);
        Ok(())
    }
}

/// Same as `TableRom` but only the first `cutoff` entries can be
/// read; subsequent reads return `None`.  Drives the partial-read
/// soundness test.
struct PartialRom {
    inner: TableRom,
    cutoff: usize,
}

impl strider_orchestrator::opt::ReadOnlyMemory for PartialRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        if addr < self.inner.base {
            anyhow::bail!("PartialRom: below base");
        }
        let offset = addr - self.inner.base;
        if self.inner.stride == 0 {
            anyhow::bail!("PartialRom: zero stride");
        }
        let idx = (offset / self.inner.stride) as usize;
        if idx >= self.cutoff {
            anyhow::bail!("PartialRom: past cutoff");
        }
        self.inner.read(addr, buf)
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
    let (function, _anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
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
    let (function, _anchor) = build_jump_table_predecessor_if_scenario(base, stride, bound);
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
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
/// index could be any I32 value; we'd be enumerating an arbitrary
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
    let (function, _anchor) = build_jump_table_unbounded_scenario(base, stride);
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
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
    let (function, _anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom(&function, None)
    .expect("classify_anchor_with_rom");
    assert_eq!(result, None);
}

/// Bounded shape, rom covers some but not all entries.  Classifier
/// must return None — a partial Multiple would induce a CFG missing
/// real edges.  See the soundness rationale in `table.rs`'s module docs.
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
    let (function, _anchor) = build_jump_table_known_bits_scenario(base, stride, idx_mask);
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
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
    let (function, _anchor) = build_jump_table_predecessor_if_scenario(base, stride, bound);
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "bound = 0 must return None — Multiple([]) would wrongly imply \
         the branch is dead code",
    );
}

// ── End-to-end byte-driven guard-polarity tests ──────────────────────────────
//
// The FunctionBuilder fixtures above hand-build the guard in a region-built
// shape.  These tests instead drive real x86-64 bytes through
// `Strider::analyze` (default pipeline, rom wired), pinning the end-to-end
// contract for a compiler-style range-checked rodata jump table in both
// guard polarities:
//
//   * jb polarity — `cmp rdi, N; jb .dispatch` (branch TOWARD the
//     dispatch on true).  Post-pipeline cond is the directly-recognised
//     `Less(rdi, IntConst(N))` with the dispatch on the true edge.
//   * ja polarity — `cmp rdi, N-1; ja .default` (branch AWAY on true;
//     the shape gcc/clang emit for `switch`).
//
// Both resolve through the default pipeline.  The fixes that make them
// resolve, for reference (each previously a distinct gap):
//
//   1. (both polarities) `RegionCollapse` removes the single-predecessor
//      dispatch Region, so the If's control feeds the `IndirectBranch`
//      placeholder directly.  `compute_value_ranges` keys each guard by the
//      unique control consumer of the guarded If-edge (the placeholder
//      post-collapse, the dispatch Region pre-collapse) and the classifier
//      queries the range at the placeholder node — both control nodes in the
//      dominator tree — so the guard's dominance survives the collapse.
//   2. (both polarities) the guard is modelled on BOTH If edges and for a
//      constant on EITHER operand of `Less`/`Sless`, so the `ja` bound
//      (`idx <= N` on the false edge of `N < idx`, or after `IfCondInversion`
//      the true edge of the canonical compare) is recognised.
//   3. (ja only) `FlagCmpCanonicalize` rule 14 folds the constant-`cmp` `ja`
//      `CF||ZF` tree: `ConstantFold` pre-folds `Neg(IntConst(N))` to
//      `IntConst(-N)`, leaving `Or(Less(rdi, N), Equal(Add(rdi, -N), 0))`,
//      which rule 14 recognises directly (gated on `-N ≡ M` at width) and
//      rewrites to `¬Less(N, rdi)` (= `rdi <= N`).  `IfCondInversion` then
//      strips the negation onto the false edge, leaving the bound on the
//      dispatch edge for `compute_value_ranges` to read.

/// Analyze an x86-64 snippet at `base` with `rom` wired for the
/// jump-table classifier, default pipeline + default options.
fn analyze_x64_snippet_with_rom(
    bytes: Vec<u8>,
    base: u64,
    rom: TableRom,
) -> strider_orchestrator::AnalyzeResult {
    let arch = strider_target::SleighArch::x86_64();
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .expect("build cc");
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(Box::new(rom)))
        .expect("Strider::new");
    strider
        .analyze(
            base,
            &cc,
            &strider_orchestrator::LiftOptions::default(),
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .expect("analyze")
}

/// Asserts the analyze result fully resolved the table dispatch: nothing
/// in `unresolved_indirect_branches` and no `IndirectBranch` placeholder
/// left in the returned IR.
fn assert_dispatch_resolved(result: &strider_orchestrator::AnalyzeResult, what: &str) {
    use strider_ir::{IRViewer, IRWalker};
    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "{what}: jump table must resolve; unresolved: {:?}",
        result.unresolved_indirect_branches,
    );
    let placeholders = result
        .function
        .walk()
        .filter(|&n| {
            matches!(
                result.function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        placeholders, 0,
        "{what}: no IndirectBranch placeholder may survive resolution",
    );
}

const SNIPPET_BASE: u64 = 0x1000;
const TABLE_BASE: u64 = 0x2000;
const TABLE_TARGETS: [u64; 4] = [0x1010, 0x1011, 0x1012, 0x1013];

fn table_rom() -> TableRom {
    TableRom {
        base: TABLE_BASE,
        stride: 8,
        entries: TABLE_TARGETS.to_vec(),
        size: 8,
    }
}

/// Control: guard branches TOWARD the dispatch on true.
///
/// ```text
/// 0x1000: 48 83 ff 04              cmp  rdi, 4
/// 0x1004: 72 03                    jb   0x1009          ; idx < 4 → dispatch
/// 0x1006: 31 c0                    xor  eax, eax        ; default
/// 0x1008: c3                       ret
/// 0x1009: ff 24 fd 00 20 00 00     jmp  [rdi*8 + 0x2000]
/// 0x1010..0x1013: c3 ×4            table targets
/// ```
#[test]
fn jb_guarded_rodata_jump_table_resolves() {
    let mut bytes: Vec<u8> = vec![
        0x48, 0x83, 0xff, 0x04, // cmp rdi, 4
        0x72, 0x03, // jb +3 (0x1009)
        0x31, 0xc0, // xor eax, eax
        0xc3, // ret
        0xff, 0x24, 0xfd, 0x00, 0x20, 0x00, 0x00, // jmp [rdi*8 + 0x2000]
        0xc3, 0xc3, 0xc3, 0xc3, // four ret targets at 0x1010..0x1013
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 32));
    let result = analyze_x64_snippet_with_rom(bytes, SNIPPET_BASE, table_rom());
    assert_dispatch_resolved(&result, "jb-guarded (branch-toward-dispatch)");
}

/// Compiler-style polarity: guard branches AWAY on true (`ja .default`).
/// `FlagCmpCanonicalize` rule 14 folds the constant-`cmp` `CF||ZF` flag tree
/// `Or(Less(rdi, 3), Equal(Add(rdi, -3), 0))` to `rdi <= 3`, and after
/// `IfCondInversion` the dispatch sits on the post-pipeline If's TRUE edge
/// with the bound the range analysis reads (see the section comment).
///
/// ```text
/// 0x1000: 48 83 ff 03              cmp  rdi, 3
/// 0x1004: 77 07                    ja   0x100d          ; idx > 3 → default
/// 0x1006: ff 24 fd 00 20 00 00     jmp  [rdi*8 + 0x2000]
/// 0x100d: 31 c0                    xor  eax, eax        ; default
/// 0x100f: c3                       ret
/// 0x1010..0x1013: c3 ×4            table targets
/// ```
///
/// Aspirational contract: the dispatch must resolve to exactly the four
/// table targets, same as the jb-polarity control above.
#[test]
fn ja_guarded_rodata_jump_table_resolves() {
    let mut bytes: Vec<u8> = vec![
        0x48, 0x83, 0xff, 0x03, // cmp rdi, 3
        0x77, 0x07, // ja +7 (0x100d)
        0xff, 0x24, 0xfd, 0x00, 0x20, 0x00, 0x00, // jmp [rdi*8 + 0x2000]
        0x31, 0xc0, // xor eax, eax
        0xc3, // ret
        0xc3, 0xc3, 0xc3, 0xc3, // four ret targets at 0x1010..0x1013
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 32));
    let result = analyze_x64_snippet_with_rom(bytes, SNIPPET_BASE, table_rom());
    assert_dispatch_resolved(&result, "ja-guarded (branch-away-on-true)");
}

/// `Load` whose address shape isn't `IntAdd(IntConst, IntMul(idx,
/// IntConst))` — e.g. `Load(IntConst(addr))` for a simple global
/// read.  The classifier's Load arm must fall through to None: it's
/// neither a jump-table dispatch nor a recognised non-Load shape, so
/// no resolution is sound.
#[test]
fn non_jump_table_load_shape_falls_through() {
    // Build a `Load(IntConst(addr))` — no IntAdd, no IntMul.
    let (function, _anchor) = build_non_jump_table_load_scenario();
    // Provide a rom anyway so the test pins that the falsethrough
    // is structural, not rom-driven: even with an idle rom the
    // classifier still returns None for the wrong shape.
    let rom = TableRom {
        base: 0,
        stride: 4,
        entries: vec![0x100, 0x200],
        size: 4,
    };
    let result = classify_anchor_with_rom(&function, Some(&rom))
    .expect("classify_anchor_with_rom");
    assert_eq!(
        result, None,
        "non-jump-table Load shape must fall through to None, NOT \
         attempt rom enumeration",
    );
}
