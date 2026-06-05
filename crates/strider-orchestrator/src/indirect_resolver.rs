//! Concrete indirect-branch resolver that builds a per-site mini IR
//! and runs the strider-opt optimizer pipeline to classify a
//! `BranchIndirect`'s target.
//!
//! Lives in `strider-orchestrator` (not `strider-lift::cfg`) to keep the dep
//! direction forward: the cfg-time mini-IR resolver calls into the
//! optimizer pipeline, so it sits in the analyze layer.  The cfg builder
//! hands every unresolved `BranchIndirect` to the installed
//! [`strider_lift::cfg::IndirectResolverFn`] closure (see
//! [`strider_lift::cfg::Builder::with_indirect_resolver`]); the canonical
//! implementation is the [`resolve_indirect_target`] free function
//! below, which callers wrap in a closure.
//!
//! ## Algorithm
//!
//! 1. Build a stand-alone, single-block IR graph for the region's
//!    value-producing pcode instructions.
//! 2. Read the current `ValueId` of `target_vn` and emit a
//!    `Return(target_value)` so the value is reachable from the entry.
//! 3. Run `ConstantFold + KnownBits + LoadReadOnly + PhiCollapse +
//!    RegionCollapse` over the resulting [`strider_ir::Graph`].
//!    [`strider_opt::LoadReadOnly`]
//!    short-circuits when the caller's [`strider_opt::OptCtx`] carries
//!    no rom, so the pipeline shape is identical with or without one.
//! 4. Inspect the producer of the post-fold target value:
//!    - `IntConst(k)` → [`ResolvedTargets::Single`] when `k` fits a `u64`
//!      (via `u128_to_branch_target`); a constant with high bits set
//!      defers (`Ok(None)`) rather than truncating to a wrong address.
//!    - `InitialVar(vn)` where `vn == cc_link_register_vn` →
//!      [`ResolvedTargets::LinkRegister`].
//!    - anything else → `Ok(None)`.  Unclassifiable targets are not
//!      errors at this layer: callers (region_builder) defer the
//!      branch via [`strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch`]
//!      and the strider-level fixed-point loop runs IR-level
//!      indirect-branch resolution against the optimised IR.
//!
//! ## Multi-target / jump tables
//!
//! This cfg-time mini-graph resolver only ever returns `Single` /
//! `LinkRegister` / `None` — never `Multiple`.  The IR-level resolver
//! in `strider_opt::indirect_branch_resolve` (jump-table arm,
//! stack-array arm) is the path that constructs
//! [`ResolvedTargets::Multiple`], routed through the strider
//! orchestrator's indirect-branch fixed-point loop after the stable
//! optimiser pipeline runs.

use strider_ir::ReadOnlyMemory;
use strider_ir::{IRViewer, IRWalker};
use strider_lift::cfg::{RegionInstruction, ResolvedTargets, Result};
use strider_target::Endianness;

use strider_opt::{
    ConstantFold, KnownBits, LoadReadOnly, OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse,
};

/// Resolves the target of a `BranchIndirect` against `region_insns`.
///
/// `region_insns` is the *current* region's pcode instructions in
/// program order, **including the trailing `BranchIndirect`** — the
/// resolver naturally stops lifting at the first opcode
/// `strider_lift::pcode_lift::ValueLifter::lift` returns `Ok(false)` for, which
/// covers every control-flow / call / store op the caller is responsible
/// for.
///
/// `target_vn` is the varnode that the `BranchIndirect`'s
/// `inputs[0]` names (i.e. the dispatch destination, almost always the
/// register being jumped through on a real ISA).
///
/// `cc_link_register_vn` is the calling convention's link register
/// varnode.  When the resolver finds the target is the function-entry
/// value of this varnode, it returns [`ResolvedTargets::LinkRegister`].
/// `None` on stack-push ISAs (x86, x86_64) where there is no
/// architectural link register; in that case the LinkRegister
/// classification is impossible and the resolver falls through to the
/// unresolved-error path.
///
/// `rom` enables [`strider_opt::LoadReadOnly`] folding inside the
/// mini-graph's optimizer pipeline so that loads from the binary's
/// `.rodata` / `.text` resolve to constants.  `None` leaves
/// `LoadReadOnly` in the pipeline as a no-op (it short-circuits when
/// the caller's [`strider_opt::OptCtx`] carries no rom).
///
/// # Errors
///
/// Propagates errors from the underlying mini-graph build, opt run,
/// or pcode lifting.  Failure to classify the target itself is *not*
/// an error and returns `Ok(None)` — the caller stamps the offending
/// pcode address onto a
/// [`strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch`] so the
/// strider-level fixed-point loop can attempt IR-level indirect-branch resolution
/// against the optimised IR.
pub fn resolve_indirect_target<R: rsleigh::MemReader>(
    region_insns: &[RegionInstruction],
    target_vn: rsleigh::Vn,
    sleigh: &rsleigh::Sleigh<R>,
    cc_link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: Endianness,
) -> Result<Option<ResolvedTargets>> {
    let mut fg = build_resolver_mini_graph(
        region_insns,
        target_vn,
        sleigh,
        cc_link_register_vn,
        endianness,
    )?;
    // The resolver pipeline carries `LoadReadOnly` unconditionally; the
    // pass short-circuits when `ctx.rom` is `None`, so a caller that
    // didn't supply a rom pays only a single reachable-walk per
    // fixed-point iteration.  When a rom IS available, the pipeline's
    // shared fixed-point loop drives `ConstantFold` / `KnownBits` /
    // `LoadReadOnly` / `PhiCollapse` / `RegionCollapse` to convergence
    // in one pass — chained `Load(Load(const_addr))` shapes resolve as
    // each load's address fold exposes the next.
    // The mini-graph was built (above) with this run's `endianness`, so the
    // rom-consuming passes read it back off the function — no need to carry
    // it on the context.
    let mut ctx = match rom {
        Some(rom) => OptCtx::with_rom(rom),
        None => OptCtx::empty(),
    };
    make_resolver_pipeline().run(&mut fg, &mut ctx)?;

    // Classify by inspecting the `Return` node's value-input (slot
    // index 2 — slots 0/1 are control/memory).  Looking at the
    // Return input rather than `target_value` directly is robust
    // against `replace_all_uses` rewires that orphan the original
    // ValueId.
    //
    // The Return is a fixed graph-construction invariant of the
    // mini-graph builder above (`build_return(Some(value), &[])`);
    // a missing or duplicate Return is therefore an internal bug,
    // not a "can't classify" outcome, and propagates as an error.
    let return_node = find_unique_return(&fg)?;
    let inputs = fg.node_inputs(return_node);
    // Layout: [control, memory, value].  `build_return` above passed
    // `Some(value)` and `&[]`, so slot 2 is always present.
    let &value_input = inputs.get(2).ok_or_else(|| {
        anyhow::anyhow!("indirect_resolve mini-graph Return has no value input slot")
    })?;
    let producer = fg.producer(value_input);
    let kind = *fg.node_kind(producer);

    match kind {
        strider_ir::node::NodeKind::IntConst(_) => {
            // IntConst stores a u128.  `BranchIndirect` targets are always
            // machine pointers (≤ 64 bits on every supported arch); a
            // higher-bit constant (e.g. a 128-bit SIMD register used as a
            // target VN) is not a valid jump target, so defer rather than
            // silently truncate to a wrong address.  Mirrors the opt-time
            // `classify_anchor` policy via the shared helper.
            let k = fg.int_const_u128(value_input).ok_or_else(|| {
                anyhow::anyhow!("indirect_resolve: IntConst node has no integer output value")
            })?;
            match strider_opt::indirect_branch_resolve::u128_to_branch_target(k) {
                Some(target) => Ok(Some(ResolvedTargets::Single(target))),
                None => Ok(None),
            }
        }
        strider_ir::node::NodeKind::InitialVar(vn) if Some(vn) == cc_link_register_vn => {
            Ok(Some(ResolvedTargets::LinkRegister))
        }
        // Unclassifiable producer is not an error: defer to the
        // strider-level outer loop's indirect-branch resolver.
        _ => Ok(None),
    }
}

/// Builds the resolver's mini-IR graph and emits `Return(target_value)`
/// so the dispatch target is reachable from the entry.  Hoisted out of
/// [`resolve_indirect_target`] so the test API can drive it and inspect
/// the resulting [`strider_ir::Graph`] (e.g. for asm-fingerprint
/// validation in tests).
///
/// Each pcode insn is lifted under a
/// [`strider_ir::FunctionBuilder::set_lift_addr`] context naming the insn's
/// parent machine address, so every IR node born during the lift
/// carries the contributor address required by the Layer-C
/// asm-fingerprint invariant.  The post-loop `read_vn` /
/// `build_return` pair is attributed to the last region instruction's
/// machine address — that is the `BranchIndirect` insn whose target
/// the mini-graph is built to resolve, the natural cause of the
/// emitted Return / its operand reads.
///
/// Returns `Ok` with the built graph immediately after `builder.build()`
/// — optimizer passes run by the caller (which already absorbs
/// fingerprints from rewritten nodes via the standard contract).
///
/// # Errors
///
/// Propagates [`strider_ir::FunctionBuilder::new`], pcode-lift, and
/// `build_return` / `build` failures.
pub fn build_resolver_mini_graph<R: rsleigh::MemReader>(
    region_insns: &[RegionInstruction],
    target_vn: rsleigh::Vn,
    sleigh: &rsleigh::Sleigh<R>,
    cc_link_register_vn: Option<rsleigh::Vn>,
    endianness: Endianness,
) -> Result<strider_ir::Function> {
    // Collect every varnode the region touches plus `target_vn` and
    // `cc_link_register_vn` so the IR builder can pre-declare them.
    // Including `target_vn` lets us read its value even on regions
    // that never write through it (e.g. `bx lr` with no prior writes
    // to lr); including the link register lets the LR-target
    // classification's `InitialVar(lr)` show up.
    let mut seen: rustc_hash::FxHashSet<rsleigh::Vn> = rustc_hash::FxHashSet::default();
    let mut all_vns: Vec<rsleigh::Vn> = Vec::new();
    let push_vn = |vn: rsleigh::Vn,
                   seen: &mut rustc_hash::FxHashSet<rsleigh::Vn>,
                   all: &mut Vec<rsleigh::Vn>| {
        if seen.insert(vn) {
            all.push(vn);
        }
    };
    for ri in region_insns {
        for vn in ri.insn.all_vns() {
            push_vn(vn, &mut seen, &mut all_vns);
        }
    }
    push_vn(target_vn, &mut seen, &mut all_vns);
    if let Some(lr) = cc_link_register_vn {
        push_vn(lr, &mut seen, &mut all_vns);
    }
    // Determinism: sort by (space-shortcut, offset, size) so VarId
    // numbering inside FunctionBuilder is reproducible across runs
    // (HashSet iteration order would otherwise depend on the random
    // hasher seed).
    all_vns.sort_unstable_by_key(strider_lift::pcode_lift::vn_sort_key);

    // Stand up a minimal FunctionBuilder under the trivial (default)
    // calling convention.  The mini-graph never emits Call or Store nodes,
    // so the convention is irrelevant — only the tracked-var set and the
    // endianness matter for the lift.
    let mut builder = strider_ir::FunctionBuilder::new(
        all_vns,
        &strider_target::BuiltCallingConvention::default(),
        endianness,
    )?;
    let region = builder.create_region()?;
    builder.set_entry_region(region)?;
    builder.set_region(region);

    // Lift every value-producing insn.  Stop at the first `Ok(false)`
    // — that is the BranchIndirect (or any other control-flow / call
    // / store opcode the lifter rejects).
    //
    // Each pcode insn's lift is wrapped in a `set_lift_addr` pair
    // mirroring the per-region driver so every IR node produced
    // inherits the parent machine instruction's address as an
    // asm-fingerprint contributor.  Without this, the Layer-C
    // fingerprint check flags every lifted node in the mini-graph.
    {
        let mut lifter = strider_lift::pcode_lift::ValueLifter::new(&mut builder, sleigh);
        for ri in region_insns {
            let machine_addr = ri.addr.machine_addr.addr;
            lifter.builder.set_lift_addr(Some(machine_addr));
            let lifted_or_err = lifter.lift(&ri.insn);
            lifter.builder.set_lift_addr(None);
            if !lifted_or_err? {
                break;
            }
        }
    }

    // Read target_vn's current value into a ValueId and emit a
    // Return so the value is reachable from the function entry.
    // `read_vn` uses pcode-lift's register-aliasing logic, so a
    // sub-register target (`jmp *eax` on x86_64) folds correctly via
    // KnownBits even though we tracked `rax`.
    //
    // Attribute these nodes (the value reads and the synthesised
    // Return) to the BranchIndirect's machine address — the dispatch
    // insn is the natural cause of the resolver's Return-anchored
    // value lift.  The BranchIndirect is the final entry in
    // `region_insns` (the module-doc invariant); fall back to the
    // first insn's address when the region is degenerate, which only
    // happens in synthetic tests.  An empty region is structurally
    // unreachable in production callers and would stamp the
    // synthesised Return with a bogus `{0}` fingerprint — surface a
    // typed error instead of silently lying.
    let branch_indirect_addr = region_insns
        .last()
        .or_else(|| region_insns.first())
        .map(|ri| ri.addr.machine_addr.addr)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "build_resolver_mini_graph: region has no instructions to anchor \
             the synthesised Return's asm-fingerprint against"
            )
        })?;
    builder.set_lift_addr(Some(branch_indirect_addr));
    let target_value = {
        let mut lifter = strider_lift::pcode_lift::ValueLifter::new(&mut builder, sleigh);
        lifter.read_vn(&target_vn)?
    };
    // build_return terminates the region unconditionally.
    builder.build_return(Some(target_value), &[])?;
    builder.set_lift_addr(None);

    // Build the graph and run the resolver pipeline.  The pipeline is
    // rebuilt per invocation; most binaries have only a handful of
    // indirect branches, so the per-site construction cost (a handful
    // of small allocs) is dominated by the actual fold work.
    builder.build()
}

/// Builds a fresh fixed-point pipeline of `ConstantFold + KnownBits +
/// LoadReadOnly + PhiCollapse + RegionCollapse` for the cfg-time mini-IR
/// resolver.
///
/// `LoadReadOnly` is always present: the pass short-circuits to
/// `NoChange` when the caller's [`OptCtx`] carries no rom, so a
/// rom-less resolver run pays only one reachable-walk per fixed-point
/// iteration and never reaches the rom-dispatch path.  When a rom
/// IS supplied via [`OptCtx::with_rom`], the pipeline's shared
/// fixed-point loop interleaves `ConstantFold` / `KnownBits` /
/// `LoadReadOnly` / `PhiCollapse` / `RegionCollapse` to convergence —
/// chained `Load(Load(const_addr))` shapes resolve as each load's
/// address fold exposes the next, without a separate outer-loop scaffold.
///
/// `PhiCollapse` IS needed: the mini-graph's lone region has a
/// single predecessor (the function entry), so the lifter creates
/// trivial `VarPhi(vn)` nodes for every variable read.  The
/// classifier's `LinkRegister` arm matches `InitialVar(lr_vn)` rather
/// than `VarPhi(lr_vn)` — without `PhiCollapse` collapsing the
/// trivial single-predecessor phi back to its `InitialVar` input, the
/// `bx lr` shape never resolves.  `RegionCollapse` is paired with it
/// for symmetry (it is the control-side companion), though the mini
/// resolver rarely needs the Region rewrite.
fn make_resolver_pipeline() -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(KnownBits);
    pipeline.add(LoadReadOnly);
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline
}

/// Locates the unique `Return` node in `fg`.
///
/// The mini-graph builder emits exactly one `Return` (via the
/// `build_return` call inside [`resolve_indirect_target`]).
/// Optimization passes never add or remove `Return` nodes, so this
/// is well-defined post-fold.
/// Zero or more than one Return signals a graph-construction bug in
/// this module and propagates as an error.
fn find_unique_return(function: &strider_ir::Function) -> Result<strider_ir::node::NodeId> {
    let mut iter = function.walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Return));
    let first = iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("indirect_resolve mini-graph contains no Return node"))?;
    if iter.next().is_some() {
        return Err(anyhow::anyhow!(
            "indirect_resolve mini-graph contains more than one Return node"
        ));
    }
    Ok(first)
}
