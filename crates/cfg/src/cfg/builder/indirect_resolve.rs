//! Lazy mini-IR resolver for `BranchIndirect` targets.
//!
//! When [`crate::Builder`] encounters a `BranchIndirect` opcode it has two
//! independent jobs: terminate the current region and decide whether the
//! branch targets the function's link register (`Return`), an in-range
//! constant (`Branch` to that address), or an out-of-range constant
//! (`TailCall { target }`).  This module's [`resolve_indirect_target`]
//! answers the *target* question.
//!
//! The resolver runs **only** on `BranchIndirect` encounter — regions
//! without a `BranchIndirect` never trigger the work below.
//!
//! ## Algorithm
//!
//! 1. Build a stand-alone, single-block IR graph for the region's
//!    value-producing pcode instructions.
//! 2. Read the current `NodeOutputId` of `target_vn` and emit a
//!    `Return(target_value)` so the value is reachable from the entry.
//! 3. Run `ConstantFold + KnownBits + RedundantPhis` (and optionally
//!    [`opt::LoadReadOnly`] when the caller passes a [`ReadOnlyMemory`])
//!    over the resulting [`ir::BuiltFunctionGraph`].
//! 4. Inspect the producer of the post-fold target value:
//!    - `IntConst(k)` → [`ResolvedTargets::Single(k as u64)`].
//!    - `InitialVar(vn)` where `vn == cc_link_register_vn` →
//!      [`ResolvedTargets::LinkRegister`].
//!    - anything else → `Ok(None)`.  Unclassifiable targets are not
//!      errors at this layer: callers (region_builder) defer the
//!      branch via [`crate::RegionTerminator::UnresolvedIndirectBranch`]
//!      and the strider-level fixed-point loop runs tier-2 resolution
//!      against the optimised IR.  Genuine errors (builder/opt
//!      failures, malformed graph) still propagate.
//!
//! The mini graph never contains calls, branches, or stores — control-flow
//! opcodes (which make [`pcode_lift::ValueLifter::lift`] return
//! `Ok(false)`) terminate lifting at the `BranchIndirect` itself.  The
//! omitted opt passes (`StackStoreDetect`, `CallStackArgCollect`,
//! `FunctionArgDetect`, …) all assume call/store nodes that we never emit
//! here, so leaving them out keeps the pipeline minimal.
//!
//! ## Multi-target / jump tables
//!
//! [`ResolvedTargets::Multiple`] is reserved for the future jump-table
//! resolver and is not constructed by this round; the variant exists so
//! adding jump-table support later is purely additive.

use opt::ReadOnlyMemory;

use crate::cfg::types::RegionInstruction;
use crate::error::Result;

/// Re-export of the canonical [`opt::ResolvedTargets`].  Kept under the
/// `cfg::ResolvedTargets` path so the strider orchestrator can build
/// `known_targets` maps without importing both crates' types — the
/// enum is defined in `opt` because cfg → opt is the workspace dep
/// direction (cfg's mini-graph runs the opt pipeline) and a reverse
/// dep would form a cycle.
pub use opt::ResolvedTargets;

/// Resolves the target of a `BranchIndirect` against `region_insns`.
///
/// `region_insns` is the *current* region's pcode instructions in
/// program order, **including the trailing `BranchIndirect`** — the
/// resolver naturally stops lifting at the first opcode
/// [`pcode_lift::ValueLifter::lift`] returns `Ok(false)` for, which
/// covers every control-flow / call / store op the caller is responsible
/// for.
///
/// `target_vn` is the varnode that the `BranchIndirect`'s
/// `inputs[0]` names (i.e. the dispatch destination, almost always the
/// register being jumped through on a real ISA).
///
/// `cc_link_register_vn` is the calling convention's link register
/// varnode, as exposed by
/// [`target::BuiltCallingConvention::link_register_vn`].  When the
/// resolver finds the target is the function-entry value of this
/// varnode, it returns [`ResolvedTargets::LinkRegister`].  `None` on
/// stack-push ISAs (x86, x86_64) where there is no architectural link
/// register; in that case the LinkRegister classification is impossible
/// and the resolver falls through to the unresolved-error path.
///
/// `rom` enables [`opt::LoadReadOnly`] inside the mini-graph's
/// optimizer pipeline so that loads from the binary's `.rodata` /
/// `.text` resolve to constants.  `None` skips the pass.
///
/// # Errors
///
/// Propagates errors from the underlying mini-graph build, opt run,
/// or pcode lifting.  Failure to classify the target itself is *not*
/// an error and returns `Ok(None)` — the caller stamps the offending
/// pcode address onto a
/// [`crate::RegionTerminator::UnresolvedIndirectBranch`] so the
/// strider-level fixed-point loop can attempt tier-2 resolution
/// against the optimised IR.
pub(super) fn resolve_indirect_target<R: rsleigh::MemReader>(
    region_insns: &[RegionInstruction],
    target_vn: rsleigh::Vn,
    sleigh: &rsleigh::Sleigh<R>,
    cc_link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: target::Endianness,
) -> Result<Option<ResolvedTargets>> {
    // ── Step 1: collect every varnode the region touches so the IR
    //    builder can pre-declare them.  Includes target_vn so we can
    //    always read its value, even on regions that don't otherwise
    //    write through it (e.g. `bx lr` after no prior writes to lr).
    //    Includes cc_link_register_vn for the same reason: the LR-target
    //    classification needs a tracked InitialVar(lr) to show up.
    let mut seen: std::collections::HashSet<rsleigh::Vn> =
        std::collections::HashSet::new();
    let mut all_vns: Vec<rsleigh::Vn> = Vec::new();
    let push_vn = |vn: rsleigh::Vn,
                       seen: &mut std::collections::HashSet<rsleigh::Vn>,
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
    // Determinism: sort by (space-shortcut, offset, size) like
    // strider's `find_all_unique_vns` so VarId numbering inside
    // FunctionBuilder is reproducible across runs (HashSet iteration
    // order would otherwise depend on the random hasher seed).
    all_vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));

    // ── Step 2: stand up a minimal FunctionBuilder.  No calling
    //    convention plumbing — `new_raw` with empty arg/callee/ret slices,
    //    no stack pointer, ret_stack_pop=0.  The mini-graph never emits
    //    Call or Store nodes, so the convention is irrelevant.
    let mut builder = ir::FunctionBuilder::new_raw(
        all_vns, &[], &[], &[], None, 0,
    )?;
    let region = builder.create_region()?;
    builder.set_entry_region(region)?;
    builder.set_region(region);

    // ── Step 3: lift every value-producing insn.  Stop at the first
    //    `Ok(false)` — that is the BranchIndirect (or any other
    //    control-flow / call / store opcode the lifter rejects).
    {
        let mut lifter = pcode_lift::ValueLifter::new(&mut builder, sleigh, endianness);
        for ri in region_insns {
            if !lifter.lift(&ri.insn)? {
                break;
            }
        }
    }

    // ── Step 4: read target_vn's current value into a NodeOutputId and
    //    emit a Return so the value is reachable from the function entry.
    //    `read_vn` uses pcode-lift's register-aliasing logic, so a
    //    sub-register target (`jmp *eax` on x86_64) folds correctly via
    //    KnownBits even though we tracked `rax`.
    let target_value = {
        let mut lifter = pcode_lift::ValueLifter::new(&mut builder, sleigh, endianness);
        lifter.read_vn(&target_vn)?
    };
    builder.build_return(Some(target_value), &[])?;

    // ── Step 5: build the graph and run the resolver pipeline.
    //
    // TODO(perf): the pipeline is rebuilt per resolver invocation.  The
    // user explicitly deferred caching (plan Q4) until measured — most
    // binaries have only a handful of indirect branches, so the
    // construction cost is in the noise.  Revisit if profiling shows
    // otherwise.
    let mut fg = builder.build()?;

    // Run the core fixed-point pipeline.  `LoadReadOnly` is *not* added
    // here because its `Optimizer` impl requires `M: 'static` (the
    // pipeline stores passes as `Box<dyn Optimizer + 'static>`), and
    // `rom: Option<&dyn ReadOnlyMemory>` is borrowed for an arbitrary
    // (non-'static) lifetime.  We instead drive `LoadReadOnly` by hand
    // alongside the pipeline below, which keeps the per-resolver
    // pipeline construction borrow-free.
    {
        let mut pipeline = opt::OptimizerPipeline::new();
        pipeline.add(opt::ConstantFold);
        pipeline.add(opt::KnownBits);
        pipeline.add(opt::RedundantPhis);
        pipeline.run_on_built(&mut fg)?;
    }

    // If the caller supplied a ReadOnlyMemory, resolve constant-address
    // loads against it and re-run the core fold pipeline so the loaded
    // constants propagate.
    //
    // We can't add an `opt::LoadReadOnly` to the pipeline above because
    // its `Optimizer` impl requires `M: 'static` (`Box<dyn Optimizer +
    // 'static>` storage) and our `rom: &dyn ReadOnlyMemory` is borrowed
    // for an arbitrary lifetime.  Inlining the load-folding loop here
    // (it's ~20 lines) keeps the pipeline construction borrow-free and
    // matches `opt::LoadReadOnly::optimize` line-for-line.
    if let Some(rom) = rom {
        resolve_const_loads(&mut fg, rom)?;
        let mut pipeline = opt::OptimizerPipeline::new();
        pipeline.add(opt::ConstantFold);
        pipeline.add(opt::KnownBits);
        pipeline.add(opt::RedundantPhis);
        pipeline.run_on_built(&mut fg)?;
    }

    // Classify by inspecting the `Return` node's value-input (slot
    // index 2 — slots 0/1 are control/memory).  Looking at the
    // Return input rather than `target_value` directly is robust
    // against `replace_all_uses` rewires that orphan the original
    // NodeOutputId.
    //
    // The Return is a fixed graph-construction invariant of step 4
    // (`build_return(Some(value), &[])`); a missing or duplicate
    // Return is therefore an internal bug, not a "can't classify"
    // outcome, and propagates as an error.
    let return_node = find_unique_return(&fg)?;
    let inputs = fg.graph.node_inputs(return_node);
    // Layout: [control, memory, value].  `build_return` above passed
    // `Some(value)` and `&[]`, so slot 2 is always present.
    let &value_input = inputs.get(2).ok_or_else(|| {
        anyhow::anyhow!("indirect_resolve mini-graph Return has no value input slot")
    })?;
    let producer = fg.graph.get_node_from_output(value_input);
    let kind = *fg.graph.node_kind(producer);

    match kind {
        ir::node::NodeKind::IntConst(k) => {
            // IntConst stores a u128.  `BranchIndirect` targets are
            // always machine pointers (≤ 64 bits on every supported
            // arch), but a higher-bit constant could in principle slip
            // through — e.g. a 128-bit SIMD register used as a target
            // VN.  Mask to 64 bits since virtual-address space is 64-bit
            // and any extra bits are garbage from the resolver's
            // perspective.
            #[allow(clippy::cast_possible_truncation)]
            let truncated = k as u64;
            Ok(Some(ResolvedTargets::Single(truncated)))
        }
        ir::node::NodeKind::InitialVar(vn) if Some(vn) == cc_link_register_vn => {
            Ok(Some(ResolvedTargets::LinkRegister))
        }
        // Unclassifiable producer is not an error: defer to the
        // strider-level outer loop's tier-2 resolver.
        _ => Ok(None),
    }
}

/// Inlined equivalent of [`opt::LoadReadOnly::optimize`] that takes a
/// borrowed `&dyn ReadOnlyMemory` instead of an owned `M: 'static`.
///
/// Walks every `Load(space)` node in `fg`, asks the read-only memory
/// for the constant value at the load's compile-time address, and
/// rewrites the load's value output to the resulting `IntConst`.
///
/// Mirrors the LoadReadOnly impl line-for-line; kept in sync via
/// `crates/opt/src/load_readonly/mod.rs`'s test suite (the
/// optimizer-side tests would catch any divergence in shared behaviour).
fn resolve_const_loads(
    fg: &mut ir::BuiltFunctionGraph,
    rom: &dyn ReadOnlyMemory,
) -> Result<()> {
    let nodes: Vec<_> = fg.preorder().collect();
    for node_id in nodes {
        let kind = *fg.graph.node_kind(node_id);
        let ir::node::NodeKind::Load(space) = kind else {
            continue;
        };
        let inputs = fg.graph.node_inputs(node_id);
        if inputs.len() < 2 {
            continue;
        }
        let addr_input = inputs[1];
        let Some(addr) = fg.int_const_val(addr_input) else {
            continue;
        };
        let [data_out] = fg.graph.node_outputs_exact::<1>(node_id)?;
        let Some(ty) = fg.graph.output_kind(data_out).as_value() else {
            continue;
        };
        let size = ty.byte_size();
        let Some(loaded) = rom.read(space, addr, size) else {
            continue;
        };
        let Some(masked) = ty.get_unsigned_int(u128::from(loaded)).and_then(|v| u64::try_from(v).ok()) else {
            continue;
        };
        let new_out = fg.make_int_const(masked, ty)?;
        fg.replace_all_uses(data_out, new_out)?;
    }
    Ok(())
}

/// Locates the unique `Return` node in `fg`.
///
/// The mini-graph builder emits exactly one `Return` (in
/// [`resolve_indirect_target`] step 4).  Optimization passes never
/// add or remove `Return` nodes, so this is well-defined post-fold.
/// Zero or more than one Return signals a graph-construction bug in
/// this module and propagates as an error.
///
/// Iterates the full reachable graph; with one Return the early-exit
/// is immediate after the second hit (which itself indicates a bug).
fn find_unique_return(fg: &ir::BuiltFunctionGraph) -> Result<ir::node::NodeId> {
    let mut found: Option<ir::node::NodeId> = None;
    for node_id in fg.preorder() {
        if matches!(fg.graph.node_kind(node_id), ir::node::NodeKind::Return) {
            if found.is_some() {
                return Err(anyhow::anyhow!(
                    "indirect_resolve mini-graph contains more than one Return node"
                ));
            }
            found = Some(node_id);
        }
    }
    found.ok_or_else(|| {
        anyhow::anyhow!("indirect_resolve mini-graph contains no Return node")
    })
}

#[doc(hidden)]
pub mod test_api {
    //! Test-only re-export of the resolver so unit tests in
    //! `crates/cfg/tests/indirect_resolve.rs` can call into it without
    //! exposing the helper to downstream crates.

    use super::resolve_indirect_target;
    use crate::cfg::types::RegionInstruction;
    use crate::error::Result;
    use opt::ReadOnlyMemory;

    pub use super::ResolvedTargets;

    /// Test-only forwarder for [`super::resolve_indirect_target`].
    ///
    /// Returns `Ok(None)` when the resolver cannot classify the
    /// target; genuine builder / opt errors still propagate via the
    /// `Result`.
    ///
    /// # Errors
    /// Propagates whatever the underlying resolver returns: builder
    /// failures, opt failures, malformed pcode-lift inputs.
    /// Unclassifiable targets surface as `Ok(None)`.
    pub fn resolve_indirect_target_for_test<R: rsleigh::MemReader>(
        region_insns: &[RegionInstruction],
        target_vn: rsleigh::Vn,
        sleigh: &rsleigh::Sleigh<R>,
        cc_link_register_vn: Option<rsleigh::Vn>,
        rom: Option<&dyn ReadOnlyMemory>,
        endianness: target::Endianness,
    ) -> Result<Option<ResolvedTargets>> {
        resolve_indirect_target(
            region_insns,
            target_vn,
            sleigh,
            cc_link_register_vn,
            rom,
            endianness,
        )
    }
}
