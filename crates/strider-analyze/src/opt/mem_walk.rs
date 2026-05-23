//! Shared backward memory-chain walker used by SP-aware analyses.
//!
//! Both `stack_load_forward::probe` and `function_args::mem_chain_is_dirty`
//! traverse the memory chain backward from a `mem: NodeOutputId`, treat
//! `StackStore` / `StackStorePhi` / `Store` as pass-through-or-terminate
//! steps (delegated to [`crate::opt::sp_expr::step_through_*`]), and treat
//! `MemPhi` as a fork where every predecessor must be walked and the
//! per-predecessor verdicts combined.  This module pulls the work-stack
//! plumbing, the cycle guard, and the MemPhi-fold continuation frame into
//! one place so the two passes only contribute their per-step classifier
//! and their verdict-combination policy.
//!
//! The walk is stack-safe at any chain depth (no recursion); a long
//! sequence of disjoint StackStores or non-aliasing Stores costs O(1)
//! heap per node.
//!
//! See [`MemChainStep`] for the classifier-trait shape and
//! [`walk_mem_chain`] for the driver entry point.
//!
//! Note that the closure's chosen per-step verdict — not the walker — owns
//! all rule semantics; the walker only orchestrates the traversal.
//!
//! Per the design constraint that opt-pass cleanup keeps walks O(n) over
//! reachable mem nodes, the walker uses an explicit `Vec<Frame>` work
//! stack and an `entity_utils::DenseEntitySet<NodeOutputId>` cycle guard.
//!
//! ## Scope: forking walks only
//!
//! Two other backward memory-chain walks live elsewhere in this crate
//! and deliberately do NOT use this primitive:
//!
//! * [`crate::opt::stack_store::call_args`]'s
//!   `collect_stack_args_in_chain_order` walks the chain leading into a
//!   `Call`, accumulating positional `StackStore` data outputs into a
//!   dense-prefix slot table.  It treats `MemPhi` as a chain-terminator
//!   (never branches), and its per-step decision depends on cross-step
//!   accumulated state (`prefix_top`, `anchor_base`, `anchor_space`,
//!   `chain_anchor_offset`, `is_first_store`).  A unified trait would
//!   force it to declare a `JoinPhi` arm that is dead code at every
//!   call site and a `combine_phi` impl that is never invoked.
//!
//! * [`crate::opt::stack_load_forward::find_stack_stored_value_at_offset`]
//!   walks the chain looking for one `StackStore` at a specific
//!   SP-relative offset.  It bails on `MemPhi` rather than branching,
//!   and it memoises EVERY prefix on the way back into a caller-
//!   supplied `StackStoredValueMemo` — the walker would have to expose
//!   its internal visited-set order back to the step impl to preserve
//!   that semantics, which leaks implementation.
//!
//! Both excluded walks are linear (non-forking) chains and therefore
//! never invoke this module's load-bearing machinery (the `Vec<Frame>`
//! work stack, the `JoinPhi` continuation frame, the results-stack
//! drain).  Folding them in would trade clarity for a never-taken
//! `JoinPhi` arm in every step impl plus an unmodelled per-prefix-memo
//! callback.  Keeping them local also lets each express its
//! termination-with-partial-result shape directly (`Vec<NodeOutputId>`
//! dense prefix, `Option<NodeOutputId>` keyed lookup) without an extra
//! `Verdict` layer.
//!
//! If a third forking walker arrives, fold it into [`MemChainStep`].
//! If a third linear walker arrives, consider extracting a tiny
//! `walk_mem_chain_linear` helper instead — the design constraints are
//! disjoint enough that one trait cannot serve both shapes cleanly.

use smallvec::SmallVec;
use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeOutputId};

use crate::opt::error::Result;

/// Per-step classifier for the memory-chain walk.
///
/// The walker calls [`Self::classify`] for each mem node it visits; the
/// classifier decides whether the node terminates the search with a
/// verdict, continues the walk via a single predecessor, or forks a
/// `MemPhi` join.  After every predecessor of a forked `MemPhi` has been
/// resolved, the walker calls [`Self::combine_phi`] with the collected
/// per-predecessor verdicts to produce a single verdict for the phi.
///
/// `cycle_verdict` is consulted when the walker re-encounters a mem node
/// already visited within this walk (cycle short-circuit).  The two
/// existing call sites disagree on policy:
///
/// * `mem_chain_is_dirty` treats revisits as clean (`false`) — every node
///   participates in the cycle set, so the second visit is silently
///   absorbed via [`CyclePolicy::GuardEveryNode`].
/// * `probe` only guards at `MemPhi` boundaries (other memory nodes walk
///   strictly backward to earlier producers and cannot self-cycle) and
///   treats a phi-cycle as a fail-closed verdict — see
///   [`CyclePolicy::GuardPhiOnly`].
pub(crate) trait MemChainStep {
    /// Per-step verdict (the analysis's success/failure value).
    type Verdict;

    /// Classifies one memory-chain node.  Called exactly once per
    /// non-cycle visit.  `mem` is the `NodeOutputId` we arrived via; the
    /// classifier may need both `node` (the producer) and `mem` (the
    /// specific output port carrying the token).
    fn classify(
        &mut self,
        graph: &Graph,
        mem: NodeOutputId,
        node: NodeId,
    ) -> Result<StepResult<Self::Verdict>>;

    /// Verdict for a node revisited within this walk.  Only consulted
    /// when the configured [`CyclePolicy`] fires on the current visit.
    fn cycle_verdict(&mut self) -> Self::Verdict;

    /// Combines per-predecessor verdicts of a `MemPhi` into a single
    /// verdict for the phi.  `phi_node` and `phi_token` are surfaced so
    /// the analysis can attach phi-level metadata (e.g. the
    /// `ResolveShape::Phi` token-and-preds union).
    fn combine_phi(
        &mut self,
        phi_node: NodeId,
        phi_token: NodeOutputId,
        preds: Vec<Self::Verdict>,
    ) -> Self::Verdict;
}

/// Outcome of classifying a single mem-chain node.
pub(crate) enum StepResult<V> {
    /// Terminal verdict for this branch — no further predecessors visited.
    Verdict(V),
    /// Continue walking via this single predecessor.
    Continue(NodeOutputId),
    /// `MemPhi` fork — visit every listed predecessor and combine the
    /// results via [`MemChainStep::combine_phi`].  `phi_node` and
    /// `phi_token` are echoed back into the combine call so analyses
    /// that need them don't have to look them up again.
    JoinPhi {
        phi_node: NodeId,
        phi_token: NodeOutputId,
        preds: SmallVec<[NodeOutputId; 4]>,
    },
}

/// Cycle-guard policy.  Different passes need different cycle behaviour:
/// some treat every revisit as silently clean, others only guard at
/// `MemPhi` boundaries and fail closed.
pub(crate) enum CyclePolicy {
    /// Update + check the cycle set on every node visit.  A revisit
    /// shorts to [`MemChainStep::cycle_verdict`].
    GuardEveryNode,
    /// Only update + check the cycle set when arriving at a `MemPhi`.
    /// Other memory nodes walk strictly backward to earlier producers
    /// and cannot self-cycle on a single path.
    GuardPhiOnly,
}

/// Drives the backward memory-chain walk.  Stack-safe at any chain depth
/// and any phi fan-out, including pathological 10k+ store prologues.
///
/// The caller owns the cycle-guard set so that nested walks within one
/// optimisation iteration can be sequenced without conflating their
/// visited sets — both call sites freshly construct one per
/// `(load, mem)` query.
///
/// `is_mem_phi` is a closure rather than a `NodeKind` constant because
/// the walker is generic over the IR `NodeKind` API surface — the only
/// kind the walker itself needs to look at is `MemPhi` (for the
/// phi-only cycle guard).  Keeping it parametric avoids hauling the
/// full `strider_ir::node::NodeKind` dependency into a "what kind is
/// this?" branch that only the existing passes really need.
pub(crate) fn walk_mem_chain<S: MemChainStep>(
    graph: &Graph,
    initial_mem: NodeOutputId,
    cycle_policy: CyclePolicy,
    seen: &mut entity_utils::DenseEntitySet<NodeOutputId>,
    is_mem_phi: impl Fn(NodeId) -> bool,
    step: &mut S,
) -> Result<S::Verdict> {
    /// Work-stack frame.  Either a fresh `Visit` of a mem node, or a
    /// `JoinPhi` continuation that combines K already-popped predecessor
    /// verdicts via `MemChainStep::combine_phi`.
    enum Frame {
        Visit(NodeOutputId),
        JoinPhi {
            phi_node: NodeId,
            phi_token: NodeOutputId,
            pred_count: usize,
        },
    }

    let mut work: Vec<Frame> = vec![Frame::Visit(initial_mem)];
    // Stack of per-branch verdicts; MemPhi joins drain the top
    // `pred_count` entries.
    let mut results: Vec<S::Verdict> = Vec::new();

    while let Some(frame) = work.pop() {
        match frame {
            Frame::JoinPhi {
                phi_node,
                phi_token,
                pred_count,
            } => {
                let drain_at = results.len() - pred_count;
                let preds: Vec<S::Verdict> = results.drain(drain_at..).collect();
                results.push(step.combine_phi(phi_node, phi_token, preds));
            }
            Frame::Visit(cur_mem) => {
                let node = graph.get_node_from_output(cur_mem);
                let guard_here = match cycle_policy {
                    CyclePolicy::GuardEveryNode => true,
                    CyclePolicy::GuardPhiOnly => is_mem_phi(node),
                };
                if guard_here && !seen.insert(cur_mem) {
                    results.push(step.cycle_verdict());
                    continue;
                }
                match step.classify(graph, cur_mem, node)? {
                    StepResult::Verdict(v) => {
                        results.push(v);
                    }
                    StepResult::Continue(next_mem) => {
                        work.push(Frame::Visit(next_mem));
                    }
                    StepResult::JoinPhi {
                        phi_node,
                        phi_token,
                        preds,
                    } => {
                        let pred_count = preds.len();
                        work.push(Frame::JoinPhi {
                            phi_node,
                            phi_token,
                            pred_count,
                        });
                        // Push preds in reverse order: the LIFO worklist
                        // then pops them in forward (slot-0, slot-1, …)
                        // order, so the per-pred verdicts accumulate on
                        // the `results` stack in the same order the
                        // original sequential walks produced them.
                        // Preserves byte-identical snapshots when the
                        // downstream `realize` step is order-sensitive
                        // about fresh `ValuePhi` / `Truncate` NodeIds.
                        for pred in preds.into_iter().rev() {
                            work.push(Frame::Visit(pred));
                        }
                    }
                }
            }
        }
    }

    // Walker invariant: exactly one final verdict for `initial_mem`.
    // Surface a violation as Err rather than silently returning a
    // default — a count != 1 is a walker bug, not an input property.
    if results.len() != 1 {
        return Err(anyhow::anyhow!(
            "walk_mem_chain: result-stack invariant broken — expected 1 final verdict, \
             got {} (walker bug)",
            results.len()
        ));
    }
    let Some(verdict) = results.pop() else {
        return Err(anyhow::anyhow!(
            "walk_mem_chain: result-stack pop failed after len==1 check (walker bug)"
        ));
    };
    Ok(verdict)
}
