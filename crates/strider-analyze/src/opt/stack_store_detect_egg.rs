//! Egg-based `StackStoreDetect` rewriter — Phase 3 Task 3.5a.
//!
//! Built alongside the imperative [`crate::opt::StackStoreDetect`] — NOT
//! a replacement.  The parity test
//! `crates/strider-analyze/tests/stack_store_egg_parity.rs` proves both
//! produce structurally identical IR for the supported shapes.
//!
//! # Design
//!
//! Per-eclass [`StackOffset`] lattice computed by an `egg::Analysis::Data`
//! transfer function.  Each e-class is classified as either
//! [`StackOffset::Sp`] (value is the stack pointer), [`StackOffset::SpRelative`]
//! (value is `sp + K` for a constant `K`), or [`StackOffset::Other`].
//!
//! Phis are opaque leaves in the egraph (the adapter discards them by
//! construction) and are therefore handled imperatively as a post-walk:
//! after the egraph saturates, walk the strider graph and for each
//! `Store(addr)` whose addr e-class is `SpRelative(K)`, rewrite to
//! `StackStore { offset: K }`; if the addr is a `VarPhi(sp)` whose every
//! predecessor's e-class is `SpRelative(K_i)`, rewrite to
//! `StackStorePhi { offsets: [K_i, …] }`.
//!
//! The And-aligned-base shape (`and esp, 0xFFFFFFF8`) is **not** modelled by
//! this v2 port because the production v1 path treats the And output as a
//! distinct opaque base — modelling it identically would require tagging
//! e-classes with per-base identity, which adds complexity beyond the
//! Task 3.5a parity scope.  The parity test only covers shapes where the
//! base is `InitialVar(sp)`; fixtures exercising the And-aligned dance
//! continue to be served by v1 [`crate::opt::StackStoreDetect`].

use std::collections::BTreeSet;

use egg::{Analysis, DidMerge, EGraph, Id};
use strider_ir::egraph_adapter::{EGraphAdapter, StriderLang};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use strider_ir::IntBinaryOp;

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};
use crate::opt::sp_expr::int_const_signed;

// ── Lattice ──────────────────────────────────────────────────────────────────

/// Per-eclass stack-offset classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackOffset {
    /// The e-class is exactly the stack-pointer varnode's
    /// `InitialVar(sp)` output.
    Sp,
    /// The e-class is `sp + K` for a known constant offset `K`.
    SpRelative(i64),
    /// The e-class is a phi of SP-relative offsets (every contributor
    /// resolved to `sp + K_i`).  This variant is reserved for analyses
    /// over Phi nodes — Phis are opaque leaves in the egraph, so this
    /// variant is populated only by the imperative post-walk, not by
    /// `Analysis::make`.  Stored as a `BTreeSet<i64>` so per-pred
    /// duplicates collapse before they reach the StackStorePhi
    /// rewrite.
    SpPhiOf(BTreeSet<i64>),
    /// Everything else (constants, non-SP InitialVar, foreign
    /// arithmetic, etc.).  Cannot be classified as SP-relative.
    Other,
}

impl Default for StackOffset {
    fn default() -> Self {
        StackOffset::Other
    }
}

// ── Analysis ─────────────────────────────────────────────────────────────────

/// `egg::Analysis` impl computing [`StackOffset`] for each e-class.
#[derive(Clone, Copy)]
pub struct StackOffsetAnalysis;

impl Analysis<StriderLang> for StackOffsetAnalysis {
    type Data = StackOffset;

    fn make(egraph: &mut EGraph<StriderLang, Self>, enode: &StriderLang) -> Self::Data {
        use StriderLang as L;
        match enode {
            // Opaque leaf — the per-add `visit` callback patches the
            // strider-side identity (Sp / Other).  Default to Other
            // until then; on rebuild merges, the patched value will
            // win (Sp / SpRelative are strictly more informative than
            // Other and the merge rule prefers them).
            L::Opaque(_) => StackOffset::Other,
            L::IntConst(_, _) => StackOffset::Other,
            L::IntBin(IntBinaryOp::Add, _ty, [a, b]) => {
                // Add of SP-relative + constant = SP-relative shifted.
                // Add of Sp + constant = SP-relative(K).
                let l = egraph[*a].data.clone();
                let r = egraph[*b].data.clone();
                let l_const = enode_const(egraph, *a);
                let r_const = enode_const(egraph, *b);
                match (l, r) {
                    (StackOffset::Sp, _) => {
                        if let Some(k) = r_const {
                            StackOffset::SpRelative(k)
                        } else {
                            StackOffset::Other
                        }
                    }
                    (_, StackOffset::Sp) => {
                        if let Some(k) = l_const {
                            StackOffset::SpRelative(k)
                        } else {
                            StackOffset::Other
                        }
                    }
                    (StackOffset::SpRelative(off), _) => {
                        if let Some(k) = r_const {
                            StackOffset::SpRelative(off.wrapping_add(k))
                        } else {
                            StackOffset::Other
                        }
                    }
                    (_, StackOffset::SpRelative(off)) => {
                        if let Some(k) = l_const {
                            StackOffset::SpRelative(off.wrapping_add(k))
                        } else {
                            StackOffset::Other
                        }
                    }
                    _ => StackOffset::Other,
                }
            }
            // Every other variant is uninformative.  v1's decompose_sp
            // handles only Add (with Sub lowered to Add(_, Neg(_))) and
            // And-of-alignment-mask; we match Add only for the parity
            // scope (see module docs for the And caveat).
            _ => StackOffset::Other,
        }
    }

    fn merge(&mut self, a: &mut Self::Data, b: Self::Data) -> DidMerge {
        // Lattice ordering: Sp / SpRelative(K) > Other.  Merging two
        // SpRelative values with different offsets is a contradiction
        // (same e-class, two different offsets); preserve the existing
        // value and accept the new one as well — but per-eclass we
        // never expect both to fire simultaneously in a well-formed
        // graph, so in practice the merge keeps `a`'s value when it
        // already dominates.
        let prev = a.clone();
        match (&prev, &b) {
            (StackOffset::Other, _) if !matches!(b, StackOffset::Other) => {
                *a = b.clone();
            }
            (StackOffset::Sp, StackOffset::Sp) => {}
            (StackOffset::SpRelative(_), StackOffset::SpRelative(_)) => {
                // Contradiction — keep `a` to remain deterministic.
            }
            _ => {}
        }
        let a_changed = *a != prev;
        let b_changed = *a != b;
        DidMerge(a_changed, b_changed)
    }
}

/// Returns the `i64` value of `id`'s e-class if it contains an `IntConst`
/// e-node (using the sign-extended-from-declared-width interpretation
/// matching `int_const_signed` for `Add(_, IntConst(_))` decomposition).
///
/// Also recognises `IntUnaryOp::Neg(IntConst(K))` because the lifter
/// emits `Sub(a, K)` as `Add(a, Neg(K))`; this peephole lets the
/// analysis see `-K` before ConstantFold collapses the unary negation.
fn enode_const(egraph: &EGraph<StriderLang, StackOffsetAnalysis>, id: Id) -> Option<i64> {
    use StriderLang as L;
    for enode in egraph[id].nodes.iter() {
        match enode {
            L::IntConst(v, ty) => {
                let signed = ty.get_signed_int(*v & ty.bit_mask_u128())?;
                return i64::try_from(signed).ok();
            }
            L::IntUn(strider_ir::IntUnaryOp::Neg, ty, [inner]) => {
                for inner_enode in egraph[*inner].nodes.iter() {
                    if let L::IntConst(v, inner_ty) = inner_enode {
                        if inner_ty == ty {
                            let masked = v & ty.bit_mask_u128();
                            let neg = masked.wrapping_neg() & ty.bit_mask_u128();
                            if let Some(signed) = ty.get_signed_int(neg) {
                                if let Ok(v64) = i64::try_from(signed) {
                                    return Some(v64);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ── StackStoreDetectEgg pass ────────────────────────────────────────────────

/// Egg-informed-but-imperative StackStoreDetect.
pub struct StackStoreDetectEgg {
    /// Varnode for the stack pointer register.
    pub stack_ptr_vn: rsleigh::Vn,
}

impl StackStoreDetectEgg {
    /// Construct a new pass for the given stack-pointer varnode.
    #[must_use]
    pub fn new(stack_ptr_vn: rsleigh::Vn) -> Self {
        Self { stack_ptr_vn }
    }
}

impl OptimizerRaw for StackStoreDetectEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Step 1: collect all reachable Store nodes (we'll mutate the
        // graph below; we can't hold a live walk iterator across
        // mutations).
        let stores: Vec<NodeId> = strider_ir::walk::walk_graph(graph, entry)
            .filter(|&n| matches!(graph.node_kind(n), NodeKind::Store(_)))
            .collect();

        if stores.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Step 2: build the egraph and patch opaque leaves whose source
        // strider output is `InitialVar(sp)` to `StackOffset::Sp`.  The
        // egraph's adapter discards the strider node identity for opaque
        // leaves (it preserves only the `NodeOutputId`-derived payload),
        // so we patch using the per-add `visit` callback.  This
        // mirrors `KnownBitsEgg`'s pattern.
        let sp_vn = self.stack_ptr_vn;
        let graph_ref: &strider_ir::Graph = graph;
        let adapter: EGraphAdapter<StackOffsetAnalysis> =
            EGraphAdapter::from_graph_with_analysis_and_visit(
                graph_ref,
                entry,
                StackOffsetAnalysis,
                |egraph, _oid, kind, id| {
                    if let NodeKind::InitialVar(vn) = kind {
                        if *vn == sp_vn {
                            egraph.set_analysis_data(id, StackOffset::Sp);
                        }
                    }
                },
            );

        // Step 3: for each Store, classify the addr e-class.  Three
        // outcomes:
        //   * SpRelative(K) — rewrite to StackStore { offset: K }.
        //   * The addr is a VarPhi(sp) whose predecessors all classify
        //     as SP-relative — rewrite to StackStorePhi.
        //   * Otherwise — leave alone.
        let mut any_changed = false;
        for store_id in stores {
            let outcome = classify_store(graph, store_id, sp_vn, &adapter);
            match outcome {
                StoreOutcome::Terminal { space, offset, memory, data } => {
                    rewrite_terminal(graph, store_id, space, offset, memory, data)?;
                    any_changed = true;
                }
                StoreOutcome::Phi {
                    space,
                    phi_node,
                    offsets,
                    memory,
                    data,
                } => {
                    rewrite_phi(graph, store_id, space, phi_node, offsets, memory, data)?;
                    any_changed = true;
                }
                StoreOutcome::Skip => {}
            }
        }

        Ok(if any_changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

enum StoreOutcome {
    Terminal {
        space: rsleigh::VnSpace,
        offset: i64,
        memory: NodeOutputId,
        data: NodeOutputId,
    },
    Phi {
        space: rsleigh::VnSpace,
        phi_node: NodeId,
        offsets: Vec<i64>,
        memory: NodeOutputId,
        data: NodeOutputId,
    },
    Skip,
}

fn classify_store(
    graph: &strider_ir::Graph,
    store_id: NodeId,
    sp_vn: rsleigh::Vn,
    adapter: &EGraphAdapter<StackOffsetAnalysis>,
) -> StoreOutcome {
    let NodeKind::Store(space) = *graph.node_kind(store_id) else {
        return StoreOutcome::Skip;
    };
    let Ok([memory, addr, data]) = graph.node_inputs_exact::<3>(store_id) else {
        return StoreOutcome::Skip;
    };

    // Look up addr's e-class data.
    if let Some(&eclass) = adapter.output_to_eclass.get(&addr) {
        let canon = adapter.egraph.find(eclass);
        let data_kind = adapter.egraph[canon].data.clone();
        match data_kind {
            StackOffset::Sp => {
                return StoreOutcome::Terminal {
                    space,
                    offset: 0,
                    memory,
                    data,
                };
            }
            StackOffset::SpRelative(off) => {
                return StoreOutcome::Terminal {
                    space,
                    offset: off,
                    memory,
                    data,
                };
            }
            _ => {}
        }
    }

    // Not directly SP-relative — check if addr is a VarPhi(sp_vn) whose
    // predecessors are all SP-relative.
    let addr_node = graph.get_node_from_output(addr);
    if let NodeKind::VarPhi(vn) = *graph.node_kind(addr_node) {
        if vn == sp_vn {
            return classify_phi_addr(graph, store_id, addr_node, space, memory, data, adapter, sp_vn);
        }
    }
    StoreOutcome::Skip
}

fn classify_phi_addr(
    graph: &strider_ir::Graph,
    _store_id: NodeId,
    phi_node: NodeId,
    space: rsleigh::VnSpace,
    memory: NodeOutputId,
    data: NodeOutputId,
    adapter: &EGraphAdapter<StackOffsetAnalysis>,
    _sp_vn: rsleigh::Vn,
) -> StoreOutcome {
    // VarPhi inputs: [phi_token, pred_0, pred_1, ...].
    let inputs = graph.node_inputs(phi_node);
    if inputs.len() < 2 {
        return StoreOutcome::Skip;
    }
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    for pred in inputs.into_iter().skip(1) {
        let Some(&pred_eclass) = adapter.output_to_eclass.get(&pred) else {
            return StoreOutcome::Skip;
        };
        let canon = adapter.egraph.find(pred_eclass);
        match adapter.egraph[canon].data.clone() {
            StackOffset::Sp => offsets.push(0),
            StackOffset::SpRelative(off) => offsets.push(off),
            _ => return StoreOutcome::Skip,
        }
    }
    // If every offset is equal, the phi is degenerate — rewrite as a
    // plain StackStore.  Matches v1's `decompose_sp_phi` behaviour.
    if offsets.iter().all(|&o| o == offsets[0]) {
        return StoreOutcome::Terminal {
            space,
            offset: offsets[0],
            memory,
            data,
        };
    }
    StoreOutcome::Phi {
        space,
        phi_node,
        offsets,
        memory,
        data,
    }
}

fn rewrite_terminal(
    graph: &mut strider_ir::Graph,
    store_id: NodeId,
    space: rsleigh::VnSpace,
    offset: i64,
    memory: NodeOutputId,
    data: NodeOutputId,
) -> crate::opt::Result<()> {
    // Find a stable "base" to feed StackStore's second slot.  v1 uses
    // the InitialVar(sp) output (or the And node's output in the
    // alignment-dance case).  We use the Store's addr input as the base
    // — this preserves whatever stable producer the program already
    // committed to.  The actual address arithmetic is no longer
    // semantically active (StackStore reads `offset` directly), so the
    // base only needs to be a value output that survives reachability
    // analysis.
    let addr = graph.node_inputs(store_id)[1];
    let new_store = graph.create_node_attributed(
        NodeKind::StackStore { space, offset },
        [memory, addr, data],
        [NodeOutputKind::Memory],
        &[store_id],
    );
    let new_mem = graph.node_outputs(new_store).into_iter().next().expect("StackStore has one output");
    let [old_mem] = graph.node_outputs_exact::<1>(store_id)?;
    graph.replace_all_uses(old_mem, new_mem)?;
    graph.detach_node_inputs(store_id);
    Ok(())
}

fn rewrite_phi(
    graph: &mut strider_ir::Graph,
    store_id: NodeId,
    space: rsleigh::VnSpace,
    phi_node: NodeId,
    offsets: Vec<i64>,
    memory: NodeOutputId,
    data: NodeOutputId,
) -> crate::opt::Result<()> {
    // The VarPhi's inputs[0] is the dispatch token from the owning
    // ControlState — StackStorePhi consumes the same token so
    // RedundantPhis collapses it correctly.
    let phi_inputs = graph.node_inputs(phi_node);
    if phi_inputs.is_empty() {
        return Ok(());
    }
    let phi_token = phi_inputs[0];
    let new_store = graph.create_node_attributed(
        NodeKind::StackStorePhi { space },
        [phi_token, memory, data],
        [NodeOutputKind::Memory],
        &[store_id],
    );
    graph.set_stack_phi_offsets(new_store, offsets);
    let new_mem = graph.node_outputs(new_store).into_iter().next().expect("StackStorePhi has one output");
    let [old_mem] = graph.node_outputs_exact::<1>(store_id)?;
    graph.replace_all_uses(old_mem, new_mem)?;
    graph.detach_node_inputs(store_id);
    Ok(())
}

// `int_const_signed` is re-exported for symmetry with the v1 pass; the egg
// transfer function reads constants directly off the e-class's nodes.
#[doc(hidden)]
pub fn _unused_int_const_signed(g: &strider_ir::Graph, out: NodeOutputId) -> Option<i64> {
    int_const_signed(g, out)
}
