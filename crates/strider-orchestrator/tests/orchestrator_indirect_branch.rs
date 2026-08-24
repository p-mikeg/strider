//! Drives `Strider::analyze` against real ELF fixtures. `indirect_branch.rs`
//! covers the same fixtures through `build_ir` + the classifier directly,
//! bypassing the resolve/re-lift loop.

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
    run_with_opts(arch, case, fn_name, Default::default(), true)
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
        "mips64 GOT-indirect table is unresolvable (gp unmodelled), so it must defer, \
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

fn count_kind(function: &strider_ir::Function, want: strider_ir::node::NodeKind) -> usize {
    function
        .walk()
        .filter(|nid| *function.node_kind(*nid) == want)
        .count()
}

/// A switch whose index is loop-carried and whose loop back-edge is reachable
/// only THROUGH a switch arm.  Resolution iteration 1 sees a CFG where the
/// header has one predecessor, so the index is the entry constant, the table
/// load folds to one literal, and the site seats as a single target with
/// nothing left unresolved.  Every arm but one is then never decoded.
///
/// x86/x64/mips lower the loop this way; ARM does not, which is why only these
/// regress.  `f` is called from exactly one arm, so its absence is the tell,
/// and `arms` pins the rest: a Call alone still passes with 7 of 8 arms gone.
///
/// `switch.c` inlines an 8-case dense switch into `main`, so the dispatch table
/// has one slot per case.  ARM lowers it to 7 distinct arm addresses (two cases
/// share a body), the others to 8.
fn assert_loop_carried_switch_reaches_every_arm(arch: common::Arch, arms: usize) {
    let result = analyze_with_opts(arch, "switch", "main", Default::default(), true)
        .unwrap_or_else(|e| panic!("{arch:?}/switch/main must converge: {e:#}"));
    // `arm/main` also contains a libgcc `mov pc, rN` division table that is
    // genuinely unresolvable and defers honestly, so the invariant under test
    // is that every arm is reached, not the placeholder count.
    assert!(
        count_kind(&result.function, strider_ir::node::NodeKind::Call) > 0,
        "{arch:?}/switch/main: `f` is called from one switch arm, so a missing Call \
         means arms were never decoded: the table resolved to a single target",
    );
    // The widest seated table is the inlined dispatch; a narrower one is a
    // different site (x86 seats a one-target `Switch` of its own).
    let widest = result
        .cfg
        .regions()
        .filter_map(|r| match &r.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => {
                let mut addrs: Vec<u64> = targets.iter().map(|t| t.addr).collect();
                addrs.sort_unstable();
                addrs.dedup();
                Some(addrs)
            }
            _ => None,
        })
        .max_by_key(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        widest.len(),
        arms,
        "{arch:?}/switch/main: the dispatch must seat every arm; got {widest:#x?}",
    );
}

#[test]
fn loop_carried_switch_reaches_every_arm() {
    for (arch, arms) in [
        (common::Arch::X64, 8),
        (common::Arch::X86, 8),
        (common::Arch::Mips32le, 8),
        (common::Arch::Mips32be, 8),
        (common::Arch::Arm, 7),
    ] {
        assert_loop_carried_switch_reaches_every_arm(arch, arms);
    }
}

/// `run_orchestrator_on` with caller-supplied `LiftOptions`.
fn run_with_opts(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
    known: rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>,
    resolve: bool,
) -> anyhow::Result<strider_ir::Function> {
    analyze_with_opts(arch, case, fn_name, known, resolve).map(|r| r.function)
}

/// As [`run_with_opts`], keeping the whole [`AnalyzeResult`].
fn analyze_with_opts(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
    known: rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>,
    resolve: bool,
) -> anyhow::Result<strider_orchestrator::AnalyzeResult> {
    let path = common::binary_path(arch, case);
    if !path.exists() {
        panic!("missing test binary {path:?}; run `make -C fixtures`");
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let sa = arch.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
    let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
    // The symbol's ARM-Thumb interworking bit IS the entry's ISA mode, and
    // `Lifter::build_cfg` masks it off for decoding itself, so it is passed
    // through rather than stripped here.
    let addr = obj.symbol_by_name(fn_name).expect("symbol").address();
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
    let regs = sleigh.regs().expect("regs");
    let cc = arch.cc().build(&regs).expect("cc");
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            allow_code_before_start_addr: true,
            known_targets: known,
            ..Default::default()
        },
        ..strider_orchestrator::LiftOptions::default()
    };
    let opt_opts = strider_orchestrator::opt::OptOptions {
        resolve_indirect_branches: resolve,
        ..Default::default()
    };
    let mut strider =
        strider_orchestrator::Strider::new(sa, sleigh, Some(rom)).expect("Strider::new");
    strider.analyze(addr, &cc, &lift_opts, &opt_opts, None)
}

#[test]
fn resolution_can_be_turned_off() {
    let on = run_with_opts(
        common::Arch::X64,
        "switch",
        "dispatch_value",
        Default::default(),
        true,
    )
    .expect("converges");
    assert_eq!(
        count_indirect_branch_placeholders(&on),
        0,
        "resolves by default"
    );

    let off = run_with_opts(
        common::Arch::X64,
        "switch",
        "dispatch_value",
        Default::default(),
        false,
    )
    .expect("converges with resolution off");
    assert!(
        count_indirect_branch_placeholders(&off) > 0,
        "resolution off must leave the dispatch a placeholder",
    );
}

#[test]
fn caller_supplied_targets_seat_with_resolution_off() {
    // The site the resolver would have found, handed in by the caller instead.
    let seated = run_with_opts(
        common::Arch::X64,
        "switch",
        "dispatch_value",
        Default::default(),
        true,
    )
    .expect("converges");
    assert_eq!(count_indirect_branch_placeholders(&seated), 0);

    // Taken from the report rather than hardcoded, so it cannot drift off the
    // real dispatch (a wrong address seats nothing and still returns Ok).
    let off = analyze_with_opts(
        common::Arch::X64,
        "switch",
        "dispatch_value",
        Default::default(),
        false,
    )
    .expect("converges with resolution off");
    let [addr] = off.unresolved_indirect_branches[..] else {
        panic!(
            "expected exactly one unresolved dispatch, got {:?}",
            off.unresolved_indirect_branches
        );
    };

    // `dispatch_value`'s own entry: an answer the caller invented, and a
    // mapped address so a seated edge would really decode.
    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        addr,
        strider_cfg::ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x4011c0, None)),
    );
    // Resolution off, so any seating here is the caller's doing.
    let out = run_with_opts(common::Arch::X64, "switch", "dispatch_value", known, false)
        .expect("caller-seated target must not error");
    assert_eq!(
        count_indirect_branch_placeholders(&out),
        0,
        "the caller's answer must seat the dispatch, leaving no placeholder",
    );
}

/// A placeholder the knob left in place is still a RESULT: the orchestrator
/// derives its report from the classifier's map, so skipping the pass entirely
/// publishes an empty list next to a live `IndirectBranch`.
#[test]
fn resolution_off_still_reports_the_surviving_placeholders() {
    let off = analyze_with_opts(
        common::Arch::X64,
        "switch",
        "dispatch_value",
        Default::default(),
        false,
    )
    .expect("converges with resolution off");
    let placeholders = count_indirect_branch_placeholders(&off.function);
    assert!(placeholders > 0, "resolution off must leave a placeholder");
    assert_eq!(
        off.unresolved_indirect_branches.len(),
        placeholders,
        "every surviving placeholder must be reported",
    );
}
