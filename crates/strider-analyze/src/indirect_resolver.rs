//! Concrete indirect-branch resolver that builds a per-site mini IR
//! and runs the strider-analyze opt pipeline to classify a
//! `BranchIndirect`'s target.
//!
//! Lives in `strider-analyze` (not `strider-lift::cfg`) to keep the dep
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
//! 2. Read the current `NodeOutputId` of `target_vn` and emit a
//!    `Return(target_value)` so the value is reachable from the entry.
//! 3. Run `ConstantFold + KnownBits + RedundantPhis` (and optionally
//!    [`crate::opt::LoadReadOnly`] when the caller passes a [`strider_ir::ReadOnlyMemory`])
//!    over the resulting [`strider_ir::Graph`].
//! 4. Inspect the producer of the post-fold target value:
//!    - `IntConst(k)` → [`ResolvedTargets::Single(k as u64)`].
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
//! in `crate::opt::indirect_branch_resolve` (jump-table arm,
//! stack-array arm) is the path that constructs
//! [`ResolvedTargets::Multiple`], routed through the strider
//! orchestrator's indirect-branch fixed-point loop after the stable
//! optimiser pipeline runs.

use strider_ir::ReadOnlyMemory;
use strider_lift::cfg::{RegionInstruction, ResolvedTargets, Result};
use strider_target::Endianness;

use crate::opt::{ConstantFold, KnownBits, OptimizerPipeline, RedundantPhis};

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
/// `rom` enables [`crate::opt::LoadReadOnly`]-equivalent folding inside
/// the mini-graph's optimizer pipeline so that loads from the binary's
/// `.rodata` / `.text` resolve to constants.  `None` skips the pass.
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
    let entry = fg.entry().ok_or_else(|| {
        anyhow::anyhow!("resolver mini-graph has not been built (entry is None)")
    })?;
    make_resolver_pipeline().run(&mut fg, entry)?;

    // If the caller supplied a ReadOnlyMemory, resolve constant-address
    // loads against it and re-run the core fold pipeline so the loaded
    // constants propagate.  Iterate to fixed point: each
    // `resolve_const_loads` sweep folds every Load whose address is
    // currently constant.  ConstantFold + KnownBits then propagate the
    // new `IntConst` outputs through any address-arithmetic chain (e.g.
    // `Add(loaded_const, K)`) so that chained `Load(Load(const_addr))`
    // shapes resolve in subsequent sweeps.
    if let Some(rom) = rom {
        while resolve_const_loads(&mut fg, rom)? {
            make_resolver_pipeline().run(&mut fg, entry)?;
        }
    }

    // Classify by inspecting the `Return` node's value-input (slot
    // index 2 — slots 0/1 are control/memory).  Looking at the
    // Return input rather than `target_value` directly is robust
    // against `replace_all_uses` rewires that orphan the original
    // NodeOutputId.
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
    let producer = fg.node_for_output(value_input);
    let kind = *fg.node_kind(producer);

    match kind {
        strider_ir::node::NodeKind::IntConst(k) => {
            // IntConst stores a u128.  `BranchIndirect` targets are always
            // machine pointers (≤ 64 bits on every supported arch); a
            // higher-bit constant (e.g. a 128-bit SIMD register used as a
            // target VN) is not a valid jump target, so defer rather than
            // silently truncate to a wrong address.  Mirrors the opt-time
            // `classify_anchor` policy via the shared helper.
            match crate::opt::indirect_branch_resolve::u128_to_branch_target(k) {
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
/// Propagates [`strider_ir::FunctionBuilder::new_raw`], pcode-lift, and
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
    let mut seen: rustc_hash::FxHashSet<rsleigh::Vn> =
        rustc_hash::FxHashSet::default();
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

    // Stand up a minimal FunctionBuilder.  No calling convention
    // plumbing — `new_raw` with empty arg/callee/ret slices, no stack
    // pointer, ret_stack_pop=0.  The mini-graph never emits Call or
    // Store nodes, so the convention is irrelevant.
    let mut builder = strider_ir::FunctionBuilder::new_raw(
        all_vns, &[], &[], &[], None, 0,
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
        let mut lifter = strider_lift::pcode_lift::ValueLifter::new(&mut builder, sleigh, endianness);
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

    // Read target_vn's current value into a NodeOutputId and emit a
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
        .ok_or_else(|| anyhow::anyhow!(
            "build_resolver_mini_graph: region has no instructions to anchor \
             the synthesised Return's asm-fingerprint against"
        ))?;
    builder.set_lift_addr(Some(branch_indirect_addr));
    let target_value = {
        let mut lifter = strider_lift::pcode_lift::ValueLifter::new(&mut builder, sleigh, endianness);
        lifter.read_vn(&target_vn)?
    };
    builder.build_return(Some(target_value), &[])?;
    builder.set_lift_addr(None);

    // Build the graph and run the resolver pipeline.  The pipeline is
    // rebuilt per invocation; most binaries have only a handful of
    // indirect branches, so the per-site construction cost (a handful
    // of small allocs) is dominated by the actual fold work.
    //
    // `LoadReadOnly` is NOT added to the pipeline by the caller: its
    // `Optimizer` impl requires `M: 'static` (the pipeline
    // stores passes as `Box<dyn Optimizer + 'static>`), and `rom`
    // is borrowed for an arbitrary lifetime.  The with-rom branch
    // upstream drives an inlined load-folder by hand — see
    // [`resolve_const_loads`].
    builder.build()
}

/// Builds a fresh fixed-point pipeline of `ConstantFold + KnownBits +
/// RedundantPhis` — the resolver runs this once initially and again
/// after the with-rom load-folding pass to propagate any constants
/// that loads exposed.
///
/// `RedundantPhis` IS needed: the mini-graph's lone region has a
/// single predecessor (the function entry), so the lifter creates
/// trivial `VarPhi(vn)` nodes for every variable read.  The
/// classifier's `LinkRegister` arm matches `InitialVar(lr_vn)` rather
/// than `VarPhi(lr_vn)` — without `RedundantPhis` collapsing the
/// trivial single-predecessor phi back to its `InitialVar` input, the
/// `bx lr` shape never resolves.
fn make_resolver_pipeline() -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(KnownBits);
    pipeline.add(RedundantPhis);
    pipeline
}

/// Inlined equivalent of [`crate::opt::LoadReadOnly::optimize`] that
/// takes a borrowed `&dyn ReadOnlyMemory` instead of an owned
/// `M: 'static`.
///
/// Walks every `Load(space)` node in `fg`, asks the read-only memory
/// for the constant value at the load's compile-time address, and
/// rewrites the load's value output to the resulting `IntConst`.
///
/// Why a copy: `OptimizerPipeline::add` requires `O: 'static` because
/// the pipeline stores passes as `Box<dyn Optimizer + 'static>`.  The
/// resolver's `rom` is borrowed for an arbitrary (non-'static)
/// lifetime so it can't be wrapped in `LoadReadOnly` and registered
/// directly.  Must stay in lockstep with [`crate::opt::LoadReadOnly`]'s impl.
fn resolve_const_loads(
    function: &mut strider_ir::Function,
    rom: &dyn ReadOnlyMemory,
) -> Result<bool> {
    let nodes: Vec<_> = function.walk().collect();
    let mut any_folded = false;
    for node_id in nodes {
        let kind = *function.node_kind(node_id);
        let strider_ir::node::NodeKind::Load(space) = kind else {
            continue;
        };
        // `ReadOnlyMemory` only models RAM; gate at the call site so
        // non-RAM Load nodes never reach the rom.  Mirrors
        // `crate::opt::LoadReadOnly::try_rewrite`.
        if space != rsleigh::VnSpace::RAM {
            continue;
        }
        let inputs = function.node_inputs(node_id);
        if inputs.len() < 2 {
            continue;
        }
        let addr_input = inputs[1];
        let Some(addr) = function.int_const_val(addr_input) else {
            continue;
        };
        let [data_out] = function.node_outputs_exact::<1>(node_id)?;
        let Some(ty) = function.output_kind(data_out).as_value() else {
            continue;
        };
        let size = ty.byte_size();
        // `ReadOnlyMemory::read` returns `Option<u64>` — bail on
        // wider loads (U80 / U128 / U256 / U512) rather than asking
        // the impl to truncate silently into a u64.
        if size > 8 {
            continue;
        }
        let Some(loaded) = rom.read(addr, size) else {
            continue;
        };
        let Some(masked) = ty.get_unsigned_int(u128::from(loaded)).and_then(|v| u64::try_from(v).ok()) else {
            continue;
        };
        let new_out = function.make_int_const(masked, ty)?;
        // Absorb the rewritten Load's asm-fingerprint into the new
        // IntConst so the Layer-C always-on check sees a non-empty
        // fingerprint on the freshly-introduced constant even after
        // the cache-hit dedup path.  `make_int_const` is the
        // low-level `Graph` method and does NOT stamp on its own.
        let new_node = function.node_for_output(new_out);
        let load_node = function.node_for_output(data_out);
        function.extend_asm_fingerprint_from(new_node, load_node);
        if function.replace_all_uses(data_out, new_out)? {
            any_folded = true;
        }
    }
    Ok(any_folded)
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
