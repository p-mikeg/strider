//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites and applies the in-place IR edits for the
//! resolutions that don't require a CFG rebuild.  Used as an
//! [`crate::Optimizer`] pass inside the standard fixed-point loop;
//! the strider orchestrator drives the outer loop (CFG rebuild,
//! cache invalidation, iteration cap).
//!
//! ## Submodules
//!
//! - [`classify`] — producer-shape classifier returning
//!   [`ResolvedTargets`] (`classify_anchor*` family).
//! - [`inplace`] — in-place IR edits for `LinkRegister` returns and
//!   `Single` tail calls (`apply_link_register`, `apply_tail_call`).
//! - [`jump_table`] — rodata jump-table arm.
//! - [`stack_array`] — stack-array-of-labels arm.
//!
//! ## Where [`ResolvedTargets`] lives
//!
//! Defined here in `opt` and re-exported as `cfg::ResolvedTargets`.
//! `cfg` already depends on `opt` (cfg's `indirect_resolve` mini-graph
//! runs the opt pipeline), so opt is the upstream crate where the type
//! must live; the reverse direction would form a dep cycle.

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use ir::Graph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use crate::pipeline::{OptimizationResult, Optimizer};
use crate::{ReadOnlyMemory, Result};

pub mod classify;
pub mod inplace;
pub mod jump_table;
pub mod stack_array;

/// Per-anchor enumeration cap, shared by both the rodata jump-table arm
/// (`jump_table::classify_jump_table`) and the stack-array-of-labels arm
/// (`stack_array::classify_stack_array`).
///
/// `u32::MAX + 1` if a known-bits mask were all-ones, so without this cap
/// a buggy KnownBits result could force iteration through 4 GiB of slots.
/// Real jump tables emitted by gcc/clang are bounded by the source-level
/// `switch` arm count, almost always well under 4096.  Tables larger than
/// this cap are unusual enough that we prefer `None` (defer to
/// `UnresolvedIndirectBranch`) over the pathological enumeration cost.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use classify::{
    classify_anchor, classify_anchor_with_rom, classify_anchor_with_rom_and_sp,
};
pub use inplace::{apply_link_register, apply_tail_call};
pub use jump_table::classify_jump_table;
pub use stack_array::classify_stack_array;

/// The set of statically-known targets of a single `BranchIndirect`.
///
/// The classifier's verdict on a placeholder anchor.  Re-exported from
/// `cfg` so callers that build `known_targets` maps for
/// [`cfg::Builder::with_known_targets`] use the same type the
/// classifier returns.
///
/// ## Variants
///
/// - [`Self::LinkRegister`] — the indirect branch is a return-via-LR
///   (typical on ARM/AArch64 with `bx lr`).  In-place edit: append the
///   ABI ret-val regs to the placeholder Return and we're done.
/// - [`Self::Single`] — the indirect branch resolves to exactly one
///   constant target.  In-place edit possible iff the target is a
///   tail call (out of function range); otherwise the orchestrator
///   does a CFG rebuild.
/// - [`Self::Multiple`] — the indirect branch resolves to a known set
///   of constant targets (jump table).  Always requires a CFG rebuild;
///   the opt pass leaves these alone for the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// The indirect branch dispatches to the link register's
    /// caller-provided value (i.e. a function return via LR).
    LinkRegister,
    /// The indirect branch resolves to exactly one constant target.
    Single(u64),
    /// The indirect branch resolves to a known set of constant
    /// targets.  Sorted-deduplicated by the classifier.
    ///
    /// **Invariant:** the inner `Vec` must be **non-empty**.  An
    /// empty `Multiple` would silently advertise zero runtime targets,
    /// making the dispatch site appear unreachable.  Use
    /// [`Self::multiple`] for the validating constructor that
    /// rejects empty input — the existing `Multiple(targets)`
    /// tuple-construct form is retained for pattern-matching and
    /// for callers that have already established non-emptiness.
    Multiple(Vec<u64>),
}

impl ResolvedTargets {
    /// Validating constructor for [`Self::Multiple`] (round 9 P5 /
    /// R9-2D M6).  Rejects empty `targets` so a future arm cannot
    /// silently produce an unreachable dispatch site.  The classifier
    /// arms (jump-table, stack-array, ValuePhi) already check
    /// `targets.is_empty()` and return `None` instead of constructing
    /// an empty `Multiple`; this constructor codifies the contract
    /// for any future arm.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `targets` is empty.
    pub fn multiple(targets: Vec<u64>) -> std::result::Result<Self, anyhow::Error> {
        if targets.is_empty() {
            return Err(anyhow::anyhow!(
                "ResolvedTargets::multiple: targets must be non-empty \
                 (an empty Multiple advertises zero runtime targets, \
                 making the dispatch site appear unreachable)"
            ));
        }
        Ok(Self::Multiple(targets))
    }
}

/// Opt pass that classifies indirect-branch placeholder anchors and
/// applies the in-place IR edits that don't require a CFG rebuild
/// (LinkRegister returns + Single tail calls).
///
/// `Multiple` and intra-function `Single` resolutions are LEFT for the
/// strider orchestrator to handle via a CFG rebuild — this pass cannot
/// rebuild the CFG because it doesn't own one.
///
/// ## Pass behavior
///
/// On each [`Optimizer::optimize`] invocation, the pass walks
/// [`Self::unresolved_anchors`], classifies each via
/// [`classify_anchor_with_rom_and_sp`], and applies in-place edits
/// for [`ResolvedTargets::LinkRegister`] and tail-call
/// [`ResolvedTargets::Single`].  The pass returns
/// [`OptimizationResult::Changed`] iff any in-place edit fired.
///
/// `is_tail_call` is a caller-supplied predicate so the pass doesn't
/// need to know the function's address range — that lives in strider.
pub struct IndirectBranchResolve {
    /// Calling-convention link-register varnode (`None` on x86 /
    /// x86_64 — those ABIs return via stack-pop and have no
    /// architectural LR).  Threaded into
    /// [`classify_anchor_with_rom_and_sp`].
    pub link_register_vn: Option<rsleigh::Vn>,
    /// Calling-convention stack-pointer varnode (`None` disables the
    /// stack-array arm).  Threaded into
    /// [`classify_anchor_with_rom_and_sp`].
    pub stack_ptr_vn: Option<rsleigh::Vn>,
    /// Read-only memory image (`None` disables the rodata
    /// jump-table arm).  Wrapped in `Arc` so the pass can clone it
    /// cheaply across the strider orchestrator's iterations.
    pub rom: Option<Arc<dyn ReadOnlyMemory>>,
    /// Anchor list — `(addr_for_diagnostics, value_output)` pairs
    /// pinned at lift time.  The strider orchestrator populates this
    /// from `outcome.unresolved_branches`.
    ///
    /// **Precondition:** each anchor must appear at most once.  The pass
    /// walks the list and applies in-place edits per entry; a duplicate
    /// would re-visit the same anchor, find the now-detached placeholder
    /// (because the first visit already rewrote it), and silently no-op
    /// — masking the duplicate.  The strider orchestrator populates this
    /// from a deduplicated source.
    ///
    /// Field is opaque: anchors are addr-tagged `NodeOutputId`s.  We
    /// store [`AnchorAddr`] rather than `cfg::PcodeInsnAddr` to keep
    /// the opt crate free of any cfg dependency.
    pub unresolved_anchors: Vec<(AnchorAddr, NodeOutputId)>,
    /// Per-anchor calling-convention context: argument-passing
    /// varnodes' CURRENT IR values + clobbered output kinds + return-
    /// value varnodes' CURRENT IR values, all read at the placeholder
    /// site BEFORE the optimizer runs.  The orchestrator populates
    /// this from the cache's `exit_vn_to_value` for the dispatch
    /// region; the pass threads them into the resulting Call/Return
    /// nodes.
    ///
    /// Defaulting to an empty map preserves back-compat with callers
    /// that haven't populated this field yet — the in-place editors
    /// emit a degenerate but well-typed Call/Return in that case.
    pub anchor_contexts: HashMap<AnchorAddr, AnchorCallingContext>,
    /// Predicate: `is_tail_call(target_addr)` returns `true` when
    /// `target_addr` lies outside the function's range (so a `Single`
    /// resolution can be applied in-place rather than via CFG rebuild).
    /// Boxed so the pass struct stays object-safe-friendly without
    /// making the predicate a generic.
    pub is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>,
}

/// Per-anchor calling-convention snapshot consumed by the in-place
/// editors.  See [`IndirectBranchResolve::anchor_contexts`].
#[derive(Debug, Clone, Default)]
pub struct AnchorCallingContext {
    /// IR `NodeOutputId`s for the calling convention's
    /// `arg_passing_vars` at the dispatch site.  Threaded as
    /// `inputs[3..]` to the resulting Call node (slots after control,
    /// memory, target).
    pub arg_passing_outputs: Vec<NodeOutputId>,
    /// `NodeOutputKind`s for the calling convention's clobbered
    /// varnodes at the dispatch site.  Threaded as the Call node's
    /// value outputs after `[Control, Memory]`.
    pub clobbered_kinds: Vec<NodeOutputKind>,
    /// IR `NodeOutputId`s for the calling convention's `ret_val_regs`
    /// at the dispatch site.  Threaded as the resulting Return node's
    /// inputs after `[control, memory, target_value]`
    /// (link-register case) or `[call_ctrl, call_mem]` (tail-call
    /// case).
    pub ret_val_outputs: Vec<NodeOutputId>,
}

/// Opaque address tag carried alongside each anchor — opaque to opt,
/// transparent to strider (which casts it back to its
/// `cfg::PcodeInsnAddr`).  Stored as a 128-bit packed value because
/// strider's `PcodeInsnAddr` is `(MachineInsnAddr { addr: u64 },
/// insn_index: u64)` — exactly 16 bytes.
///
/// We use a packed pair rather than `Box<dyn Any>` to avoid an
/// allocation and to keep the type cheaply `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnchorAddr {
    /// Machine address of the placeholder's BranchIndirect — opaque
    /// payload; opt does not interpret it.
    pub machine_addr: u64,
    /// Sub-machine pcode-insn index — opaque payload; opt does not
    /// interpret it.
    pub insn_index: u64,
}

impl IndirectBranchResolve {
    /// Construct a new pass with no anchors and no in-place predicate.
    /// The default predicate treats every target as in-function (i.e.
    /// no in-place edits for `Single`).  Callers typically reset both
    /// fields before invoking [`Optimizer::optimize`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            link_register_vn: None,
            stack_ptr_vn: None,
            rom: None,
            unresolved_anchors: Vec::new(),
            anchor_contexts: HashMap::new(),
            is_tail_call: Box::new(|_| false),
        }
    }

    /// Add an anchor and its calling-convention context atomically.
    /// Round 9 V5 (R9-2D H5): canonical builder method that
    /// populates `unresolved_anchors` and `anchor_contexts` in
    /// lockstep so a future caller cannot accidentally desynchronise
    /// them by populating one and forgetting the other.
    ///
    /// The strider orchestrator should use this method rather than
    /// `pass.unresolved_anchors.push((...)) + pass.anchor_contexts.insert(...)`
    /// — the lockstep contract is then type-enforced at the
    /// add-call boundary.  The fields remain `pub` for back-compat
    /// with existing test scaffolding.
    pub fn add_anchor(
        &mut self,
        addr: AnchorAddr,
        anchor_output: NodeOutputId,
        ctx: AnchorCallingContext,
    ) {
        self.unresolved_anchors.push((addr, anchor_output));
        self.anchor_contexts.insert(addr, ctx);
    }

    /// Clear all anchors and contexts atomically.  Mirrors
    /// [`Self::add_anchor`]'s lockstep contract on the reset path.
    pub fn clear_anchors(&mut self) {
        self.unresolved_anchors.clear();
        self.anchor_contexts.clear();
    }
}

impl Default for IndirectBranchResolve {
    fn default() -> Self {
        Self::new()
    }
}

impl Optimizer for IndirectBranchResolve {
    /// Classify each anchor against the current `graph`, then apply
    /// in-place edits for the LinkRegister + tail-call-Single subsets.
    ///
    /// Returns [`OptimizationResult::Changed`] iff any in-place edit
    /// modified the graph; otherwise [`OptimizationResult::NoChange`].
    /// `entry` is read but not mutated — it remains the function's
    /// entry node for the lifetime of the pass.
    ///
    /// # Errors
    ///
    /// Propagates failures from [`apply_link_register`] /
    /// [`apply_tail_call`].
    fn optimize(
        &self,
        graph: &mut Graph,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        // Wrap once over the whole loop: the classifier and the in-place
        // editors all operate on `&mut pattern::RewriteCtx<'_>`, and
        // `analyze_known_bits` is a per-call read-only analysis we want
        // to compute once and reuse across all anchors.
        crate::pipeline::with_rewrite_ctx(graph, entry, |fg| self.optimize_built(fg))
    }
}

impl IndirectBranchResolve {
    fn optimize_built(
        &self,
        fg: &mut pattern::RewriteCtx<'_>,
    ) -> Result<OptimizationResult> {
        // Cache the known-bits analysis up-front: classify_anchor's
        // jump-table and stack-array arms used to call analyze_known_bits
        // per anchor, paying the worklist cost N times for N anchors
        // even though the IR doesn't change between in-place edits
        // (the LinkRegister edit appends slots; the tail-call edit
        // detaches the placeholder and emits fresh nodes — neither
        // affects bounds on existing producers).
        let known = crate::analyze_known_bits(fg.as_view())?;
        let mut changed = false;
        for (addr, anchor_output) in &self.unresolved_anchors {
            let resolved = match classify::classify_anchor_with_rom_and_sp(
                fg.as_view(),
                *anchor_output,
                self.link_register_vn,
                self.rom.as_deref(),
                self.stack_ptr_vn,
                &known,
            ) {
                Some(r) => r,
                None => continue,
            };
            let Some(placeholder) =
                find_placeholder_return_for_anchor(fg.graph, *anchor_output)
            else {
                continue;
            };
            // Surface a missing anchor context as an Err — the orchestrator
            // populates `anchor_contexts` and `unresolved_anchors` in lockstep,
            // so a missing entry here means an upstream contract was broken.
            // Silently substituting an empty context would splice a `Return`
            // with zero return values and a `Call` with zero args/clobbers,
            // producing wrong IR.
            let ctx = self.anchor_contexts.get(addr).ok_or_else(|| {
                anyhow!(
                    "IndirectBranchResolve: missing AnchorCallingContext for anchor {addr:?}"
                )
            })?;
            match resolved {
                ResolvedTargets::LinkRegister => {
                    inplace::apply_link_register(fg, placeholder, &ctx.ret_val_outputs)?;
                    changed = true;
                }
                ResolvedTargets::Single(target) => {
                    if !(self.is_tail_call)(target) {
                        // Intra-function Single — orchestrator handles
                        // it via CFG rebuild.
                        continue;
                    }
                    let _new_return = inplace::apply_tail_call(
                        fg,
                        placeholder,
                        target,
                        &ctx.arg_passing_outputs,
                        &ctx.clobbered_kinds,
                        &ctx.ret_val_outputs,
                    )?;
                    changed = true;
                }
                ResolvedTargets::Multiple(_) => {
                    // Multiple always requires a CFG rebuild —
                    // orchestrator's territory.  Do nothing here.
                }
            }
        }
        if changed {
            Ok(OptimizationResult::Changed)
        } else {
            Ok(OptimizationResult::NoChange)
        }
    }
}

/// Walk the use-list of `anchor_output` and return the unique
/// 3-input `IndirectBranch` whose `target_value` input equals
/// `anchor_output` — the placeholder shape pinned at strider's lift
/// time.
///
/// Returns `None` when no such placeholder exists (e.g. an earlier
/// in-place edit already replaced it: `apply_tail_call` detaches the
/// node, and `apply_link_register` mutates the kind to
/// [`NodeKind::Return`]).  Public so strider's orchestrator can reuse
/// the same lookup for its own bookkeeping.
#[must_use]
pub fn find_placeholder_return_for_anchor(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer, _input_index) in graph.output_uses(anchor_output) {
        if !matches!(graph.node_kind(consumer), NodeKind::IndirectBranch) {
            continue;
        }
        if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)
            && val == anchor_output
        {
            return Some(consumer);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`IndirectBranchResolve`] as an [`Optimizer`] pass.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use ir::FunctionBuilder;
    use ir::node::NodeOutputType;

    fn fake_addr(machine: u64) -> AnchorAddr {
        AnchorAddr {
            machine_addr: machine,
            insn_index: 0,
        }
    }

    /// Build a placeholder graph: one region terminated by
    /// `IndirectBranch(ctrl, mem, IntConst(target))`.  The IntConst's
    /// NodeOutputId is the anchor.  Returns the graph + anchor +
    /// entry id.
    fn placeholder_graph_with_int_const(target: u64) -> (ir::Graph, NodeId, NodeOutputId) {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let anchor = b.build_int_const(target, NodeOutputType::U64).unwrap();
        b.build_indirect_branch(anchor).unwrap();
        let built = b.build().unwrap();
        let entry = built.entry;
        (built.graph, entry, anchor)
    }

    #[test]
    fn pass_does_nothing_when_no_anchors() -> Result<()> {
        // Vacuous case: no anchors → NoChange.
        let (mut graph, entry, _anchor) = placeholder_graph_with_int_const(0xc0de);
        let pass = IndirectBranchResolve::new();
        let result = pass.optimize(&mut graph, entry)?;
        assert_eq!(result, OptimizationResult::NoChange);
        Ok(())
    }

    #[test]
    fn pass_returns_no_change_when_no_anchor_classifies() -> Result<()> {
        // Classifier returns None for every anchor.
        // Construct: one anchor whose producer is an IntBinaryOp(Add),
        // which the classifier maps to None (not IntConst, not
        // InitialVar, not ValuePhi, not a Load shape with stack-array
        // ingredients).  Pass returns NoChange.
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let lhs = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
        let rhs = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
        let anchor = b
            .build_int_binary_operation(lhs, rhs, ir::IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        b.build_indirect_branch(anchor).unwrap();
        let mut built = b.build().unwrap();
        let entry = built.entry;

        // Locate the LIVE anchor on the post-build graph: the
        // build step doesn't run any optimization, so the IntBinaryOp
        // is still the IndirectBranch's value-input.
        let placeholder_inputs: Vec<_> = built
            .graph
            .node_inputs(
                built
                    .preorder()
                    .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
                    .unwrap(),
            )
            .into_iter()
            .collect();
        let live_anchor = placeholder_inputs[2];

        let mut pass = IndirectBranchResolve::new();
        pass.unresolved_anchors.push((fake_addr(0x1234), live_anchor));
        let result = pass.optimize(&mut built.graph, entry)?;
        assert_eq!(result, OptimizationResult::NoChange);
        Ok(())
    }

    #[test]
    fn pass_returns_changed_when_link_register_anchor_resolves() -> Result<()> {
        // InitialVar(lr) anchor resolves to LinkRegister and the
        // LinkRegister in-place edit fires.
        //
        // Pre-condition: run RedundantPhis to collapse the trivial
        // single-input VarPhi over `lr` → InitialVar(lr) directly.
        // The classifier only matches `InitialVar`; without
        // RedundantPhis, the anchor's producer would still be a
        // VarPhi and the classifier would defer.
        let lr_vn = rsleigh::Vn {
            addr_off: 0x4c,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![lr_vn], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let lr_in = b.read_variable(&lr_vn).unwrap();
        b.build_indirect_branch(lr_in).unwrap();
        let mut built = b.build().unwrap();
        let entry = built.entry;
        // Collapse the trivial VarPhi(lr) so the IndirectBranch's slot 2
        // input is `InitialVar(lr_vn)` directly.
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::RedundantPhis);
        p.run(&mut built.graph, entry)?;

        let placeholder_id = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
            .unwrap();
        let live_anchor: Vec<_> =
            built.graph.node_inputs(placeholder_id).into_iter().collect();
        let live_anchor = live_anchor[2];

        let mut pass = IndirectBranchResolve::new();
        pass.link_register_vn = Some(lr_vn);
        let anchor_addr = fake_addr(0x1234);
        pass.unresolved_anchors.push((anchor_addr, live_anchor));
        // Pair each unresolved anchor with a default (empty-ABI) calling
        // context — `IndirectBranchResolve::optimize` now requires the
        // two lists to be in lockstep.  The orchestrator populates both
        // from the same source; tests bypassing the orchestrator must do
        // the same.  Empty context is sound here: this test exercises
        // only the `LinkRegister` arm, which uses `ctx.ret_val_outputs`
        // (empty by default = no return values, valid IR).
        pass.anchor_contexts.insert(anchor_addr, AnchorCallingContext::default());
        let result = pass.optimize(&mut built.graph, entry)?;
        assert_eq!(result, OptimizationResult::Changed);
        Ok(())
    }

    #[test]
    fn pass_returns_changed_when_tail_call_anchor_resolves() -> Result<()> {
        // IntConst(K) anchor where K is OUT of the function's range
        // (per `is_tail_call`).  Pass applies the tail-call in-place
        // edit and returns Changed.
        let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

        let mut pass = IndirectBranchResolve::new();
        let anchor_addr = fake_addr(0x1000);
        pass.unresolved_anchors.push((anchor_addr, anchor));
        pass.anchor_contexts.insert(anchor_addr, AnchorCallingContext::default());
        // Treat 0xc0de as a tail call (out of function range).
        pass.is_tail_call = Box::new(|target| target == 0xc0de);
        let result = pass.optimize(&mut graph, entry)?;
        assert_eq!(result, OptimizationResult::Changed);
        // Post-edit, the graph must contain a Call node.
        let mut had_call = false;
        for nid in graph.all_node_ids() {
            if matches!(graph.node_kind(nid), NodeKind::Call) {
                had_call = true;
                break;
            }
        }
        assert!(had_call, "tail-call edit must materialise a Call node");
        Ok(())
    }

    #[test]
    fn pass_does_not_apply_in_place_for_intra_fn_single() -> Result<()> {
        // IntConst(K) anchor where K is in-range (NOT a tail call).
        // Pass leaves the graph alone — the orchestrator would handle
        // this via a CFG rebuild.
        let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

        let mut pass = IndirectBranchResolve::new();
        let anchor_addr = fake_addr(0x1000);
        pass.unresolved_anchors.push((anchor_addr, anchor));
        pass.anchor_contexts.insert(anchor_addr, AnchorCallingContext::default());
        // Always intra-function — no tail calls.
        pass.is_tail_call = Box::new(|_| false);
        let result = pass.optimize(&mut graph, entry)?;
        assert_eq!(result, OptimizationResult::NoChange);
        // Confirm: no Call node appears (the in-place edit didn't fire).
        for nid in graph.all_node_ids() {
            assert!(
                !matches!(graph.node_kind(nid), NodeKind::Call),
                "intra-fn Single must NOT produce a Call (orchestrator's job)",
            );
        }
        Ok(())
    }

    /// Regression for round8-2C H5: a missing `AnchorCallingContext` for
    /// an entry in `unresolved_anchors` MUST produce a typed `Err`
    /// rather than silently splicing an empty calling context.  The
    /// orchestrator populates both lists in lockstep — a mismatch is a
    /// contract violation that shouldn't be papered over.
    #[test]
    fn pass_errors_when_anchor_context_missing() {
        let lr_vn = rsleigh::Vn {
            addr_off: 0x4c,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![lr_vn], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let lr_in = b.read_variable(&lr_vn).unwrap();
        b.build_indirect_branch(lr_in).unwrap();
        let mut built = b.build().unwrap();
        let entry = built.entry;
        // Collapse the trivial VarPhi(lr) so the anchor is InitialVar.
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::RedundantPhis);
        p.run(&mut built.graph, entry).unwrap();

        let placeholder_id = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
            .unwrap();
        let live_anchor: Vec<_> =
            built.graph.node_inputs(placeholder_id).into_iter().collect();
        let live_anchor = live_anchor[2];

        let mut pass = IndirectBranchResolve::new();
        pass.link_register_vn = Some(lr_vn);
        // Populate `unresolved_anchors` but DELIBERATELY leave
        // `anchor_contexts` empty — the lockstep contract is broken.
        pass.unresolved_anchors.push((fake_addr(0x9999), live_anchor));
        let err = pass
            .optimize(&mut built.graph, entry)
            .expect_err("missing anchor context must propagate as Err");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("missing AnchorCallingContext"),
            "Err message must name the contract violation; got: {msg}"
        );
    }

    /// New resolver shape (round 8 follow-up): the classifier peels
    /// through `Truncate(IntConst(K))` to surface `Single(K & out_mask)`.
    /// Without this, a compiler that emits `MOV r4, #target; trunc r4
    /// to 32-bit; BX r4` would have its target_value anchored to a
    /// `Truncate` node and the resolver would fail closed.
    #[test]
    fn classify_truncate_of_int_const_resolves_to_single() {
        use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
        use crate::indirect_branch_resolve::classify::classify_anchor_with_rom_and_sp;

        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        // Build IntConst(0xC0DE_DEAD) at U64, then Truncate to U32 →
        // dispatch target is `0xDEAD` (low 32 bits = 0xC0DE_DEAD; the
        // declared output is U32 so the masked value fits).
        let const_64 = b
            .build_int_const(0xC0DE_DEADu64, NodeOutputType::U64)
            .unwrap();
        let trunc = b.body_mut().graph.create_node(
            NodeKind::Truncate,
            [const_64],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let trunc_out = b
            .body_mut()
            .graph
            .node_outputs(trunc)
            .into_iter()
            .next()
            .unwrap();
        b.build_indirect_branch(trunc_out).unwrap();
        let built = b.build().unwrap();

        let placeholder = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
            .unwrap();
        let inputs: Vec<_> = built.graph.node_inputs(placeholder).into_iter().collect();
        let anchor = inputs[2];
        let known = crate::analyze_known_bits((&built).into()).unwrap();
        let resolved = classify_anchor_with_rom_and_sp((&built).into(), anchor, None, None, None, &known);
        assert_eq!(
            resolved,
            Some(ResolvedTargets::Single(0xC0DE_DEAD)),
            "Truncate(IntConst(0xC0DE_DEAD), U32) must resolve to Single(0xC0DE_DEAD)"
        );
    }

    /// Sibling shape: `Extend(IntConst(K))` (e.g. zero-extend a 32-bit
    /// register holding a target into the 64-bit dispatch slot) must
    /// also resolve to `Single(K)`.
    #[test]
    fn classify_extend_of_int_const_resolves_to_single() {
        use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
        use crate::indirect_branch_resolve::classify::classify_anchor_with_rom_and_sp;

        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        // IntConst(0xDEAD) at U32 — zero-extended to U64 stays 0xDEAD.
        let const_32 = b.build_int_const(0xDEADu64, NodeOutputType::U32).unwrap();
        let ext = b.body_mut().graph.create_node(
            NodeKind::Extend(ir::ExtendOp::ZeroExtend),
            [const_32],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let ext_out = b
            .body_mut()
            .graph
            .node_outputs(ext)
            .into_iter()
            .next()
            .unwrap();
        b.build_indirect_branch(ext_out).unwrap();
        let built = b.build().unwrap();

        let placeholder = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
            .unwrap();
        let inputs: Vec<_> = built.graph.node_inputs(placeholder).into_iter().collect();
        let anchor = inputs[2];
        let known = crate::analyze_known_bits((&built).into()).unwrap();
        let resolved = classify_anchor_with_rom_and_sp((&built).into(), anchor, None, None, None, &known);
        assert_eq!(
            resolved,
            Some(ResolvedTargets::Single(0xDEAD)),
            "Extend(IntConst(0xDEAD), U64) must resolve to Single(0xDEAD)"
        );
    }

    /// Round 9 IMPORTANT (R9-1C Issue 1) regression: a hand-built
    /// `Extend(SignExtend, IntConst(neg_value, U32), U64)` shape — bypassing
    /// `ConstantFold` rule 6 and `extend_if_needed` — must classify to
    /// the *sign-extended* dispatch target, not the zero-extended one.
    /// Before the fix the classifier used `(*k) as u64` for both
    /// extension flavours, masking off the high bits for sign-negative
    /// narrow constants.
    #[test]
    fn classify_sign_extend_of_negative_int_const_resolves_to_sign_extended_single() {
        use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
        use crate::indirect_branch_resolve::classify::classify_anchor_with_rom_and_sp;

        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        // 32-bit value with the sign bit set: 0xFFFF_FFFF (= -1 in i32).
        // Sign-extension to U64 must produce 0xFFFF_FFFF_FFFF_FFFF.
        // We construct the SignExtend node directly (bypassing the
        // builder's eager fold) so the classifier's arm gets the live
        // shape rather than a constant-folded `IntConst(0xFFFF_FFFF_FFFF_FFFF)`.
        let const_32 = b.build_int_const(0xFFFF_FFFFu64, NodeOutputType::U32).unwrap();
        let sext = b.body_mut().graph.create_node(
            NodeKind::Extend(ir::ExtendOp::SignExtend),
            [const_32],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let sext_out = b
            .body_mut()
            .graph
            .node_outputs(sext)
            .into_iter()
            .next()
            .unwrap();
        b.build_indirect_branch(sext_out).unwrap();
        let built = b.build().unwrap();

        let placeholder = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::IndirectBranch))
            .unwrap();
        let inputs: Vec<_> = built.graph.node_inputs(placeholder).into_iter().collect();
        let anchor = inputs[2];
        let known = crate::analyze_known_bits((&built).into()).unwrap();
        let resolved = classify_anchor_with_rom_and_sp((&built).into(), anchor, None, None, None, &known);
        assert_eq!(
            resolved,
            Some(ResolvedTargets::Single(0xFFFF_FFFF_FFFF_FFFFu64)),
            "Extend(SignExtend, IntConst(0xFFFF_FFFF, U32), U64) must \
             sign-fill to Single(0xFFFF_FFFF_FFFF_FFFF), not Single(0xFFFF_FFFF)"
        );
    }
}
