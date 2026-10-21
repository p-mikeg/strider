//! Drives `Strider::analyze` against real ELF fixtures. `indirect_branch.rs`
//! pins the resolved ARM SET of the computed-goto fixture on every arch; this
//! file covers the loop's other outcomes: seating, deferral, oscillation and
//! the resolution knob.

mod common;

use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::IrWalkerEx;

fn run_orchestrator_on(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
) -> anyhow::Result<strider_ir::Function> {
    run_with_opts(arch, case, fn_name, Default::default(), true)
}

/// Every `RegionTerminator::Switch` arm set in `cfg`, sorted and deduplicated.
fn switch_arms(cfg: &strider_cfg::Cfg) -> Vec<Vec<u64>> {
    cfg.regions()
        .filter_map(|region| match &region.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => {
                let mut addrs: Vec<u64> = targets.iter().map(|t| t.addr).collect();
                addrs.sort_unstable();
                addrs.dedup();
                Some(addrs)
            }
            _ => None,
        })
        .collect()
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
fn orchestrator_resolves_jump_tables_x86() {
    assert_all_table_shapes_resolve(common::Arch::X86);
}

#[test]
fn orchestrator_resolves_jump_tables_x64() {
    assert_table_resolves(common::Arch::X64, "switch_value_range", "dispatch_unmasked");
    assert_table_resolves(common::Arch::X64, "switch_value_range", "dispatch_offset");
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
// which `value_range` then bounds like every other arch.
#[test]
fn orchestrator_resolves_jump_tables_ppc32() {
    assert_all_table_shapes_resolve(common::Arch::Ppc32be);
    assert_all_table_shapes_resolve(common::Arch::Ppc32le);
}

// ppc64 reaches its table through the TOC, whose pointer the ELFv2 prologue
// derives from the entry-address register (`r12`). With that register bound
// the masked and value_range-unmasked shapes resolve; the offset shape
// still defers on both endians.
#[test]
fn orchestrator_resolves_toc_relative_jump_tables_ppc64() {
    for arch in [common::Arch::Ppc64be, common::Arch::Ppc64le] {
        assert_table_resolves(arch, "switch", "dispatch_value");
        assert_table_resolves(arch, "switch_value_range", "dispatch_unmasked");
    }
}

// MIPS N64 accesses the jump table through the GOT relative to `gp` even
// under `-fno-pic` (`Load[gp + got_off]`). The abicalls contract binding
// `t9` to the entry address folds `gp` itself, but the GOT load of the
// table base still does not: `.got` is writable, so no read-only memory
// serves it. The branch must therefore DEFER (stay in
// `UnresolvedBranch`, placeholder retained) rather than
// resolve to a garbage target.

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
        result
            .function
            .has_kind(|k| matches!(k, strider_ir::node::NodeKind::Call { .. })),
        "{arch:?}/switch/main: `f` is called from one switch arm, so a missing Call \
         means arms were never decoded: the table resolved to a single target",
    );
    // The widest seated table is the inlined dispatch; a narrower one is a
    // different site (x86 seats a one-target `Switch` of its own).
    let widest = switch_arms(&result.cfg)
        .into_iter()
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

/// `run_orchestrator_on` with caller-supplied `AnalyzeOptions`.
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
    analyze_claiming(
        arch,
        case,
        fn_name,
        known,
        resolve,
        strider_opt::AssumptionOptions::none(),
    )
}

/// `analyze_with_opts` with a claim stated: every case in this file but one
/// resolves at the sound floor, so the claim lives at the call site that needs
/// it rather than in the shared helper.
fn analyze_claiming(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
    known: rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>,
    resolve: bool,
    assumptions: strider_opt::AssumptionOptions,
) -> anyhow::Result<strider_orchestrator::AnalyzeResult> {
    let (mut strider, cc, addr) = common::fixture_strider(arch, case, fn_name);
    let opts = strider_orchestrator::AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::Open {
                code_before_start: true,
            },
            known_targets: &known,
            ..Default::default()
        },
        resolve_indirect_branches: resolve,
        assumptions,
        ..Default::default()
    };
    strider.analyze(addr, &cc, &opts, None)
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
    let [addr] = off
        .incomplete()
        .of(strider_orchestrator::Reason::UnresolvedBranch)
        .collect::<Vec<_>>()[..]
    else {
        panic!(
            "expected exactly one unresolved dispatch, got {}",
            off.incomplete()
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
    let out = analyze_with_opts(common::Arch::X64, "switch", "dispatch_value", known, false)
        .expect("caller-seated target must not error");
    assert_eq!(
        count_indirect_branch_placeholders(&out.function),
        0,
        "the caller's answer must seat the dispatch, leaving no placeholder",
    );
    // The placeholder going away is not the claim: the claim is that THIS
    // address is the successor the cfg now carries.
    assert_eq!(
        switch_arms(&out.cfg),
        vec![vec![0x4011c0u64]],
        "the seated successor must be the address the caller invented",
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
        off.incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .count(),
        placeholders,
        "every surviving placeholder must be reported",
    );
}

/// A masked switch index carried around a loop through a phi.
///
/// The `& 7` alone admits 0..7 while the table gcc emitted holds six entries,
/// so the mask on its own names two addresses read past the table. The tighter
/// `i <= 5` lives on the loop back edge, below the header it re-enters, so no
/// guard node dominates the dispatch; the bound is carried by the edge instead,
/// for the phi arm arriving on it.
///
/// The arm set is spelled out because an EXTRA arm is an edge to code the
/// hardware never reaches: these six are `.rodata` 0x402008..0x402038 read off
/// the fixture.
#[test]
fn a_masked_loop_index_is_bounded_by_its_back_edge() {
    // The arms resolve at the floor; the claim is for this fixture's OWN
    // epilogue, whose return slot the floor cannot walk back to, so without it
    // the return is `UnresolvedBranch` and the last assertion
    // below cannot hold.
    let result = analyze_claiming(
        common::Arch::X64,
        "switch_masked_loop",
        "masked_loop_switch",
        Default::default(),
        true,
        strider_opt::AssumptionOptions {
            return_slots_survive_untracked_writes: true,
            ..strider_opt::AssumptionOptions::none()
        },
    )
    .expect("converges");
    assert_eq!(
        switch_arms(&result.cfg),
        vec![vec![
            0x40_1170, 0x40_1190, 0x40_1198, 0x40_11a0, 0x40_11a8, 0x40_11ad
        ]],
    );
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .next()
            .is_none(),
        "every arm is proven, so nothing is left to report",
    );
}

/// The same fixture on mips32, where the delay slot puts the ALREADY-SCALED
/// `(i & 7) << 2` on the back edge rather than the index.  The bound has to
/// cross the scaling, and the join with the entry arm's constant 0 has to keep
/// the spacing, or the six table slots read as twenty-one.
///
/// The site is still reported: the table base sits in a register the arms'
/// `Call` clobbers, so the selector stops deriving once the loop closes and
/// nothing re-proves the seated arms.
#[test]
fn a_scaled_masked_loop_index_seats_only_the_real_slots() {
    let result = analyze_with_opts(
        common::Arch::Mips32le,
        "switch_masked_loop",
        "masked_loop_switch",
        Default::default(),
        true,
    )
    .expect("converges");
    assert_eq!(
        switch_arms(&result.cfg),
        vec![vec![
            0x40_0734, 0x40_0760, 0x40_0768, 0x40_0770, 0x40_0778, 0x40_0780
        ]],
    );
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .next()
            .is_some()
    );
}

/// `switch_bitfield_mips64.S`: the `op` switch is guarded by `srl; sltiu 62`
/// and indexed by an `ext` of the same six bits, and the `rs` table follows
/// its 62 entries directly.  The arms are read off the fixture's tables: 18
/// for `op`, 11 for `rs`.  The field alone admits 64 slots, and seating them
/// would add `rs` entries 0 and 1 to `op`.
#[test]
fn a_guard_on_another_extract_of_the_index_bits_sizes_the_table() {
    let op: Vec<u64> = (0..18).map(|i| 0x1_2000_01e0 + 8 * i).collect();
    let rs: Vec<u64> = (0..11).map(|i| 0x1_2000_0270 + 8 * i).collect();
    for arch in [common::Arch::Mips64be, common::Arch::Mips64le] {
        let result = analyze_with_opts(
            arch,
            "switch_bitfield_mips64",
            "bitfield_dispatch",
            Default::default(),
            true,
        )
        .unwrap_or_else(|e| panic!("{arch:?}: must converge: {e:#}"));
        let mut arms = switch_arms(&result.cfg);
        arms.sort();
        assert_eq!(arms, vec![op.clone(), rs.clone()], "{arch:?}: {arms:#x?}");
        assert!(
            result.incomplete().is_empty(),
            "{arch:?}: {}",
            result.incomplete()
        );
    }
}

/// The switch arm sets of an analysis under the default claims, which must
/// converge with nothing incomplete: an -O0 frame's `ret` is proven only
/// with the incoming arguments surviving its calls.
fn complete_switch_arms(arch: common::Arch, case: &str, fn_name: &str) -> Vec<Vec<u64>> {
    let claims = strider_opt::AssumptionOptions::default();
    let result = analyze_claiming(arch, case, fn_name, Default::default(), true, claims)
        .unwrap_or_else(|e| panic!("{arch:?} {fn_name}: must converge: {e:#}"));
    assert!(
        result.incomplete().is_empty(),
        "{arch:?} {fn_name}: {}",
        result.incomplete()
    );
    switch_arms(&result.cfg)
}

/// `switch_phi_x64.S`'s `phi_table_slot` loads its target from `tbl`,
/// `tbl+8` or `tbl+0x20`, a phi of three addresses 8 and 24 apart.  The two
/// slots between the last two are never read; the range the three span names
/// them too.  Mutation: enumerate the range in `dispatch_domain` even where
/// `phi_constants` answers.
#[test]
fn a_phi_of_table_addresses_reads_only_its_own_slots() {
    assert_eq!(
        complete_switch_arms(common::Arch::X64, "switch_phi_x64", "phi_table_slot"),
        vec![vec![0x40_101f, 0x40_1025, 0x40_1037]],
    );
}

/// `switch_phi_index.c` at -O0 on x64: `goto *t[s]` with `s` one of 0, 3 and
/// 5, so `t[0]`, `t[3]` and `t[5]` are the arms, where the range 0..5 seats
/// all six.  Mutation: as above.
#[test]
fn a_phi_of_constant_indices_seats_only_the_indexed_arms() {
    assert_eq!(
        complete_switch_arms(common::Arch::X64, "switch_phi_index", "phi_index_dispatch"),
        vec![vec![0x40_11b9, 0x40_11f4, 0x40_121a]],
    );
}

/// `switch_phi_x64.S`'s `phi_label` jumps to one of three labels two
/// `cmovne`s pick: the target is itself a phi of constants, which no index
/// bounds.  The range of the three at their spacing also names the unused
/// label at `0x401060`.  Mutation: drop the arm of `dispatch_domain` that
/// takes the phi's constants where no index is bounded.
#[test]
fn a_jump_to_a_phi_of_labels_seats_each_label() {
    assert_eq!(
        complete_switch_arms(common::Arch::X64, "switch_phi_x64", "phi_label"),
        vec![vec![0x40_105a, 0x40_1066, 0x40_106c]],
    );
}

/// The same shape WITHOUT the mask resolves completely, which is what isolates
/// the bound as the cause rather than the loop or the phi.
#[test]
fn the_same_loop_switch_without_a_mask_resolves() {
    let result = analyze_with_opts(
        common::Arch::X64,
        "switch_value_range",
        "dispatch_unmasked",
        Default::default(),
        true,
    )
    .expect("converges");
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .next()
            .is_none(),
        "an unmasked guarded switch must still resolve every arm",
    );
}

/// Resolution off never calls the classifier and reports its site
/// unresolved; on, the classifier's answer resolves it. Mutation: classify
/// regardless of `resolve_indirect_branches`.
#[test]
fn resolution_off_skips_the_classifier_and_reports_the_site() {
    const BASE: u64 = 0x1000;
    let mut bytes = vec![0xc3u8; 0x40];
    bytes[..2].copy_from_slice(&[0xff, 0xe0]); // 0x1000: jmp rax
    for resolve in [false, true] {
        let calls = std::cell::Cell::new(0usize);
        let classify = |f: &strider_ir::Function,
                        _rom: Option<&dyn strider_orchestrator::opt::ReadOnlyMemory>|
         -> anyhow::Result<strider_orchestrator::opt::ClassifiedSites> {
            calls.set(calls.get() + 1);
            let mut found = strider_orchestrator::opt::ClassifiedSites::unresolved(f);
            found.resolved = found
                .sites
                .iter()
                .map(|&site| {
                    let ret = strider_cfg::ResolvedTarget::new(BASE + 0x10, None);
                    (site, strider_cfg::ResolvedTargets::Single(ret))
                })
                .collect();
            Ok(found)
        };
        let (mut strider, cc) =
            common::strider_over_bytes(common::Arch::X64, bytes.clone(), BASE, None);
        let opts = strider_orchestrator::AnalyzeOptions {
            resolve_indirect_branches: resolve,
            ..Default::default()
        };
        let result = strider
            .analyze_with_classifier(BASE, &cc, &opts, None, &classify)
            .expect("analyze");
        let reported = result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .any(|a| a.machine_addr.addr == BASE);
        assert_eq!(
            (calls.get() > 0, reported),
            (resolve, !resolve),
            "resolution {resolve}: (classified, reported) under {}",
            result.incomplete()
        );
    }
}
