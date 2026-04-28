//! F5 — IR-level indirect-branch resolver.
//!
//! Originally implemented in `strider::indirect_resolve_tier2` (the
//! "tier-2" classifier).  F5 relocates the classification + in-place
//! edit logic into the `opt` crate so it can be invoked as an
//! [`crate::Optimizer`] pass inside the standard fixed-point loop.
//! The strider crate retains thin shims that delegate to this module
//! for back-compat with the existing tier-2 orchestrator.
//!
//! ## What moved here
//!
//! - `classify_anchor` / `classify_anchor_with_rom` /
//!   `classify_anchor_with_rom_and_sp` ([`classify`]) — producer-shape
//!   classifier returning [`BranchResolution`].
//! - `apply_link_register` / `apply_tail_call` ([`inplace`]) — in-place
//!   IR edits for resolutions that don't require a CFG rebuild.
//! - `classify_jump_table` ([`jump_table`]) — rodata jump-table arm.
//! - `classify_stack_array` ([`stack_array`]) — BUG-30 stack-array arm.
//!
//! ## What stays in strider
//!
//! - The orchestrator's outer fixed-point loop (CFG rebuild, cache
//!   invalidation, iteration cap).
//! - The `cfg::ResolvedTargets` ↔ [`BranchResolution`] conversion shims
//!   in `strider::indirect_resolve_tier2::*`.
//!
//! ## Why a local [`BranchResolution`] rather than reusing
//! [`cfg::ResolvedTargets`]
//!
//! The `cfg` crate already depends on `opt` (cfg's
//! `indirect_resolve` mini-graph runs the opt pipeline).  Adding a
//! reverse dep `opt` → `cfg` would create a cycle.  Instead, opt
//! owns its own [`BranchResolution`] enum — structurally identical to
//! `cfg::ResolvedTargets` — and strider's shims convert between the
//! two at the layer where both crates are visible.

#![allow(clippy::module_name_repetitions)]

use std::sync::Arc;

use ir::Graph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pipeline::{OptimizationResult, Optimizer};
use crate::{ReadOnlyMemory, Result};

pub mod classify;
pub mod inplace;
pub mod jump_table;
pub mod stack_array;

pub use classify::{
    classify_anchor, classify_anchor_with_rom, classify_anchor_with_rom_and_sp,
};
pub use inplace::{apply_link_register, apply_tail_call};
pub use jump_table::classify_jump_table;
pub use stack_array::classify_stack_array;

/// Opt-local mirror of `cfg::ResolvedTargets`.
///
/// Carries the classifier's verdict on a placeholder anchor.  Strider
/// shims convert this to/from `cfg::ResolvedTargets` at the boundary
/// where both crates are visible.
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
pub enum BranchResolution {
    /// The indirect branch dispatches to the link register's
    /// caller-provided value (i.e. a function return via LR).
    LinkRegister,
    /// The indirect branch resolves to exactly one constant target.
    Single(u64),
    /// The indirect branch resolves to a known set of constant
    /// targets.  Sorted-deduplicated by the classifier.
    Multiple(Vec<u64>),
}

/// F5 — opt pass that classifies indirect-branch placeholder anchors
/// and applies the in-place IR edits that don't require a CFG rebuild
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
/// for [`BranchResolution::LinkRegister`] and tail-call
/// [`BranchResolution::Single`].  The pass returns
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
    /// BUG-30 stack-array arm).  Threaded into
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
    /// Field is opaque: anchors are addr-tagged `NodeOutputId`s.  We
    /// store [`AnchorAddr`] rather than `cfg::PcodeInsnAddr` to keep
    /// the opt crate free of any cfg dependency.
    pub unresolved_anchors: Vec<(AnchorAddr, NodeOutputId)>,
    /// Predicate: `is_tail_call(target_addr)` returns `true` when
    /// `target_addr` lies outside the function's range (so a `Single`
    /// resolution can be applied in-place rather than via CFG rebuild).
    /// Boxed so the pass struct stays object-safe-friendly without
    /// making the predicate a generic.
    pub is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>,
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
            is_tail_call: Box::new(|_| false),
        }
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
    /// [`apply_tail_call`] (typically [`crate::ErrorKind::IrError`]).
    fn optimize(
        &self,
        graph: &mut Graph,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        let mut changed = false;
        for (_addr, anchor_output) in &self.unresolved_anchors {
            // Phase 5 — `classify_anchor_with_rom_and_sp` now takes
            // `&BuiltFunctionGraph` so it can drive `pattern::Matcher`
            // for the jump-table and stack-array shape matches.  The
            // pass owns only `&mut Graph`, so we wrap each call via
            // `with_built` (read-only access — the classifier itself
            // never mutates the graph; the in-place editors below run
            // on the restored `&mut Graph`).
            let resolved_opt = crate::pipeline::with_built(graph, entry, |fg| {
                classify::classify_anchor_with_rom_and_sp(
                    fg,
                    *anchor_output,
                    self.link_register_vn,
                    self.rom.as_deref(),
                    self.stack_ptr_vn,
                )
            });
            let Some(resolved) = resolved_opt else {
                continue;
            };
            // Locate the placeholder Return.  Skip anchors whose
            // placeholder has already been edited away in a previous
            // iteration of the same fixed-point loop.
            let Some(placeholder) =
                find_placeholder_return_for_anchor(graph, *anchor_output)
            else {
                continue;
            };
            match resolved {
                BranchResolution::LinkRegister => {
                    // Round-1 stub for `ret_val_outputs` mirrors the
                    // strider orchestrator's documented limitation
                    // (see orchestrator.rs::read_ret_val_outputs):
                    // the cache's `exit_vn_to_value` isn't yet wired
                    // to populate ABI return-value inputs.  Future
                    // rounds: thread the calling-convention's
                    // ret_val_regs through this pass via a new field
                    // on `IndirectBranchResolve` and supply them
                    // here.  Code-review H1: keeps the opt-pass and
                    // orchestrator paths aligned on this stub.
                    inplace::apply_link_register(graph, placeholder, &[])?;
                    changed = true;
                }
                BranchResolution::Single(target) => {
                    if !(self.is_tail_call)(target) {
                        // Intra-function Single — orchestrator handles
                        // it via CFG rebuild.
                        continue;
                    }
                    // Same Round-1 stub for ret_val_outputs as
                    // LinkRegister above.
                    let _new_return =
                        inplace::apply_tail_call(graph, placeholder, target, &[])?;
                    changed = true;
                }
                BranchResolution::Multiple(_) => {
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
/// 3-input Return whose value-input slot equals `anchor_output` —
/// the placeholder Return shape pinned at strider's lift time.
///
/// Returns `None` when no such Return exists (e.g. an earlier in-
/// place edit already replaced it).  Public so strider's orchestrator
/// can reuse the same lookup for its own bookkeeping.
#[must_use]
pub fn find_placeholder_return_for_anchor(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer, _input_index) in graph.output_uses(anchor_output) {
        if !matches!(graph.node_kind(consumer), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = graph.node_inputs(consumer).into_iter().collect();
        if inputs.len() == 3 && inputs[2] == anchor_output {
            return Some(consumer);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! F5 — unit tests for [`IndirectBranchResolve`] as an
    //! [`Optimizer`] pass.

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
    /// `Return(ctrl, mem, IntConst(target))`.  The IntConst's
    /// NodeOutputId is the anchor.  Returns the graph + anchor +
    /// entry id.
    fn placeholder_graph_with_int_const(target: u64) -> (ir::Graph, NodeId, NodeOutputId) {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let anchor = b.build_int_const(target, NodeOutputType::U64);
        b.build_return(Some(anchor), &[]).unwrap();
        let built = b.build().unwrap();
        let entry = built.entry;
        (built.graph, entry, anchor)
    }

    #[test]
    fn pass_does_nothing_when_no_anchors() -> Result<()> {
        // F5 unit 1 — vacuous case.  No anchors → NoChange.
        let (mut graph, entry, _anchor) = placeholder_graph_with_int_const(0xc0de);
        let pass = IndirectBranchResolve::new();
        let result = pass.optimize(&mut graph, entry)?;
        assert_eq!(result, OptimizationResult::NoChange);
        Ok(())
    }

    #[test]
    fn pass_returns_no_change_when_no_anchor_classifies() -> Result<()> {
        // F5 unit 2 — classifier returns None for every anchor.
        // Construct: one anchor whose producer is an IntBinaryOp(Add),
        // which the classifier maps to None (not IntConst, not
        // InitialVar, not ValuePhi, not a Load shape with stack-array
        // ingredients).  Pass returns NoChange.
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let lhs = b.build_int_const(1u64, NodeOutputType::U64);
        let rhs = b.build_int_const(2u64, NodeOutputType::U64);
        let anchor = b
            .build_int_binary_operation(lhs, rhs, ir::IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(anchor), &[]).unwrap();
        let mut built = b.build().unwrap();
        let entry = built.entry;

        // Locate the LIVE anchor on the post-build graph: the
        // build step doesn't run any optimization, so the IntBinaryOp
        // is still the Return's value-input.
        let placeholder_inputs: Vec<_> = built
            .graph
            .node_inputs(
                built
                    .preorder()
                    .find(|&n| matches!(built.graph.node_kind(n), NodeKind::Return))
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
        // F5 unit 3 — InitialVar(lr) anchor resolves to LinkRegister
        // and the LinkRegister in-place edit fires.
        //
        // Pre-condition: run RedundantPhis to collapse the trivial
        // single-input ControlPhi over `lr` → InitialVar(lr) directly.
        // The classifier only matches `InitialVar`; without
        // RedundantPhis, the anchor's producer would still be a
        // ControlPhi and the classifier would defer.
        let lr_vn = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x4c,
            },
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![lr_vn], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let lr_in = b.read_variable(&lr_vn).unwrap();
        b.build_return(Some(lr_in), &[]).unwrap();
        let mut built = b.build().unwrap();
        let entry = built.entry;
        // Collapse the trivial ControlPhi(lr) so the Return's slot 2
        // input is `InitialVar(lr_vn)` directly.
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::RedundantPhis);
        p.run(&mut built.graph, entry)?;

        let return_id = built
            .all_node_ids()
            .find(|&n| matches!(built.graph.node_kind(n), NodeKind::Return))
            .unwrap();
        let live_anchor: Vec<_> =
            built.graph.node_inputs(return_id).into_iter().collect();
        let live_anchor = live_anchor[2];

        let mut pass = IndirectBranchResolve::new();
        pass.link_register_vn = Some(lr_vn);
        pass.unresolved_anchors.push((fake_addr(0x1234), live_anchor));
        let result = pass.optimize(&mut built.graph, entry)?;
        assert_eq!(result, OptimizationResult::Changed);
        Ok(())
    }

    #[test]
    fn pass_returns_changed_when_tail_call_anchor_resolves() -> Result<()> {
        // F5 unit 4 — IntConst(K) anchor where K is OUT of the
        // function's range (per `is_tail_call`).  Pass applies the
        // tail-call in-place edit and returns Changed.
        let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

        let mut pass = IndirectBranchResolve::new();
        pass.unresolved_anchors.push((fake_addr(0x1000), anchor));
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
        // F5 unit 5 — IntConst(K) anchor where K is in-range (NOT a
        // tail call).  Pass leaves the graph alone — the orchestrator
        // would handle this via a CFG rebuild.
        let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

        let mut pass = IndirectBranchResolve::new();
        pass.unresolved_anchors.push((fake_addr(0x1000), anchor));
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
}
