//! Drives `strider_orchestrator::Strider::analyze` end-to-end against real
//! ELF fixtures, the same path the Python `strider.run(...)` binding takes
//! (unlike `indirect_branch.rs`, which calls `build_ir` + the classifier
//! directly, bypassing the orchestrator's resolve/re-lift loop).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use object::{Object, ObjectSymbol};
use strider_ir::{IRViewer, IRWalker};

fn run_orchestrator_on(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
) -> anyhow::Result<strider_ir::Function> {
    let path = common::binary_path(arch, case);
    if !path.exists() {
        panic!("missing test binary {path:?}; run `make -C fixtures`");
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let sleigh_arch = arch.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj.symbol_by_name(fn_name).expect("symbol").address();
    let addr = match arch {
        common::Arch::Arm | common::Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };

    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));

    let regs = sleigh.regs().expect("regs");
    let cc = arch.cc().build(&regs).expect("build cc");
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..strider_orchestrator::LiftOptions::default()
    };
    let mut strider =
        strider_orchestrator::Strider::new(sleigh_arch, sleigh, Some(rom)).expect("Strider::new");
    strider
        .analyze(
            addr,
            &cc,
            &lift_opts,
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .map(|r| r.function)
}

#[test]
fn orchestrator_resolves_indirect_branch_x86() {
    let function = run_orchestrator_on(
        common::Arch::X86,
        "indirect_branch",
        "indirect_branch_resolved",
    )
    .expect("orchestrator must converge");
    assert!(function.graph().all_node_ids().count() > 0);
}

fn count_indirect_branch_placeholders(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| {
            matches!(
                function.node_kind(*nid),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count()
}

fn count_if_nodes(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| matches!(function.node_kind(*nid), strider_ir::node::NodeKind::If))
        .count()
}

#[test]
fn orchestrator_resolves_switch_jump_table_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch", "dispatch_value")
        .expect("orchestrator must converge on switch fixture");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "switch jump table must lower to switch edges"
    );
}

/// `switch_sparse.c`'s labels are far apart, so the compiler emits a
/// `cmp/je` chain instead of an indexed jump: the lifted IR never has an
/// `IndirectBranch` at all.
#[test]
fn orchestrator_sparse_switch_is_if_chain_x64() {
    let function = run_orchestrator_on(common::Arch::X64, "switch_sparse", "sparse_dispatch")
        .expect("orchestrator must converge on sparse switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "a sparse switch has no indirect branch to resolve",
    );
    assert!(
        count_if_nodes(&function) >= 2,
        "a sparse switch must lower to a real comparison chain of If nodes",
    );
}

// `switch_value_range.c` has no explicit `& mask`, so the classifier can't
// lean on `KnownBits` for the index bound (the way `switch.c` does); it
// must walk the compiler's `cmp; ja` range-check `If` via `value_range`.
//
// The dense (`dispatch_unmasked`) shape resolves on both x86 and x64. x64
// only works because `clean()`'s incremental re-canonicalization merges
// the duplicate `Truncate(rdi)` nodes a phi-collapse left behind, putting
// the guard on the same `Truncate(rdi)` node the 64-bit index is
// `ZeroExtend(..)`-wrapped from; the cone walk reaches that inner guarded
// Truncate directly. `value_range` does not bound the outer `ZeroExtend`.
//
// The offset-base (`dispatch_offset`) cases (labels starting at a nonzero
// base) lower to a compound range check
// `If(Or(Less(k-K, N), Equal(k-last, 0)))` ("k-K is in the low range OR k
// is the last case"). `FlagCmpCanonicalize` rule 15 recognises that
// disjunction as `(k-K) <= N` and rewrites to the canonical `<=` shape,
// which `value_range` then bounds.

#[test]
fn orchestrator_resolves_unmasked_switch_via_value_range_x86() {
    let function =
        run_orchestrator_on(common::Arch::X86, "switch_value_range", "dispatch_unmasked")
            .expect("orchestrator must converge on unmasked switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "unmasked jump table must resolve via the value_range If-bound",
    );
}

#[test]
fn orchestrator_resolves_unmasked_switch_via_value_range_x64() {
    let function =
        run_orchestrator_on(common::Arch::X64, "switch_value_range", "dispatch_unmasked")
            .expect("orchestrator must converge on unmasked switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "x64 unmasked table resolves via CSE (dedup Truncate) so the cone walk reaches the inner guarded Truncate(rdi)",
    );
}

#[test]
fn orchestrator_resolves_offset_switch_via_value_range_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch_value_range", "dispatch_offset")
        .expect("orchestrator must converge on offset switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "offset jump table must resolve via the value_range If-bound",
    );
}

#[test]
fn orchestrator_resolves_offset_switch_via_value_range_x64() {
    let function = run_orchestrator_on(common::Arch::X64, "switch_value_range", "dispatch_offset")
        .expect("orchestrator must converge on offset switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "x64 offset jump table must resolve via the value_range If-bound",
    );
}

// The general clone+optimise classifier resolves the masked (`switch`),
// value_range-bounded unmasked, and offset jump tables on every non-x86
// arch too: addressing differs per compiler, but the optimiser folds it
// once the index is pinned.

fn assert_table_resolves(arch: common::Arch, case: &str, fn_name: &str) {
    let function = run_orchestrator_on(arch, case, fn_name)
        .unwrap_or_else(|e| panic!("{arch:?}/{case}/{fn_name} must converge: {e:#}"));
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "{arch:?}/{case}/{fn_name}: jump table must lower to concrete switch edges",
    );
}

/// Resolves all three table shapes (masked / value_range unmasked / offset)
/// on `arch`. Endianness only changes rodata byte-decoding, not the lifted
/// IR shape, so both endians are covered where a big-endian variant exists.
fn assert_all_table_shapes_resolve(arch: common::Arch) {
    assert_table_resolves(arch, "switch", "dispatch_value");
    assert_table_resolves(arch, "switch_value_range", "dispatch_unmasked");
    assert_table_resolves(arch, "switch_value_range", "dispatch_offset");
}

#[test]
fn orchestrator_resolves_jump_tables_aarch64() {
    assert_all_table_shapes_resolve(common::Arch::Aarch64);
    assert_all_table_shapes_resolve(common::Arch::Aarch64Be);
}

#[test]
fn orchestrator_resolves_jump_tables_arm() {
    assert_all_table_shapes_resolve(common::Arch::Arm);
    assert_all_table_shapes_resolve(common::Arch::ArmBe);
}

#[test]
fn orchestrator_resolves_jump_tables_thumb() {
    assert_all_table_shapes_resolve(common::Arch::ArmThumb);
}

#[test]
fn orchestrator_resolves_jump_tables_mips32() {
    assert_all_table_shapes_resolve(common::Arch::Mips32le);
    assert_all_table_shapes_resolve(common::Arch::Mips32be);
}

// PowerPC compares via the condition register: `cmpwi` packs LT/GT/EQ/SO
// into a CR field and the branch extracts one bit, so the range-check
// guard is `Truncate(ShiftRight(cr_pack, k)):I1`. `FlagCmpCanonicalize`
// rewrites that to the bare comparison at the tested bit (each
// `ShiftLeft(ZeroExtend(I1), pos)` term provably sets only bit `pos`),
// which `value_range` then bounds like every other arch. (ppc64's N64 ABI
// routes the table base/index through 64-bit register-aliasing + TOC
// indirection that isn't modelled yet, so only ppc32 is covered here,
// both endians.)
#[test]
fn orchestrator_resolves_jump_tables_ppc32() {
    assert_all_table_shapes_resolve(common::Arch::Ppc32be);
    assert_all_table_shapes_resolve(common::Arch::Ppc32le);
}

// MIPS N64 accesses the jump table through the GOT relative to `gp` even
// under `-fno-pic` (`Load[gp + got_off]`), and `gp` is an unresolved
// runtime input (`InitialVar`) in the lifted IR. Pinning the index never
// folds the table base, so the branch must DEFER (stay in
// `unresolved_indirect_branches`, placeholder retained) rather than
// resolve to a garbage target. Fixing this needs MIPS N64 gp-setup
// modelling + applied GOT relocations.

#[test]
fn orchestrator_mips64_pic_jump_table_defers_not_errors() {
    let function = run_orchestrator_on(
        common::Arch::Mips64le,
        "switch_value_range",
        "dispatch_unmasked",
    )
    .expect("mips64 PIC table must DEFER (converge with a placeholder), not error");
    assert!(
        count_indirect_branch_placeholders(&function) > 0,
        "mips64 GOT-indirect table is unresolvable (gp unmodelled) — it must defer, \
         leaving the IndirectBranch placeholder, not mis-resolve to a bogus target",
    );
}

#[test]
fn orchestrator_mips64_sparse_switch_is_if_chain() {
    let function = run_orchestrator_on(common::Arch::Mips64le, "switch_sparse", "sparse_dispatch")
        .expect("orchestrator must converge on mips64 sparse switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "a sparse switch has no table (an if-chain) so it resolves on mips64 too",
    );
}
