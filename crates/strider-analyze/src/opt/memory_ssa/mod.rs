//! Unified backward memory-SSA walk shared by the SP-aware analyses.
//!
//! A memory-SSA walk starts at a `Load`'s memory-token input and walks
//! the memory-token chain backward looking for the nearest memory
//! definition (a `Store` / `Call` / `CallOther` memory output, or a
//! `MemPhi`) that **may alias** the location the load reads.  The
//! aliasing question is delegated to a pluggable [`MemorySSAWalker`]
//! oracle so the two consumers — `function_args` (does any def shadow a
//! stack-arg slot?) and the SP range-overlap analysis — can share the
//! traversal plumbing while supplying their own alias predicate.
//!
//! [`walk_memory_ssa`] returns the nearest clobbering memory output, or
//! `None` when the chain reaches `InitialMemory` (or any non-memory
//! producer) with no aliasing def on any path.
//!
//! ## Semantics
//!
//! Starting at the load's memory input, iterate the memory-token chain
//! backward (input slot 0 of `Load` / `Store` / `MemPhi`, the call's
//! memory input for `Call` / `CallOther`):
//!
//! * if a def `may_alias` the load → return it (the nearest clobber);
//! * if it does NOT alias and is not a phi → advance the cursor to that
//!   def's own memory input and continue;
//! * at a `MemPhi` → recurse into every predecessor's memory input.  If
//!   ANY predecessor reaches a clobber, the phi is the clobber boundary
//!   (returned as the clobber).  This OR-over-predecessors reduction is
//!   what a "does any path clobber?" consumer needs; a phi whose every
//!   predecessor is clean is itself clean.
//!
//! ## Cycle guard
//!
//! Loop-header `MemPhi`s feed their own region indirectly, so a naive
//! backward walk can revisit a node.  The walk carries an internal
//! `DenseEntitySet<NodeOutputId>` visited set updated at every node;
//! re-encountering a visited node short-circuits as "clean for this
//! edge" (returns `None` for that branch).  This folds the former
//! `GuardEveryNode` / `GuardPhiOnly` policies into one sound default:
//! guarding every node is strictly more conservative than guarding only
//! phis and never produces a wrong "no clobber" on a non-cyclic chain
//! (each non-phi node walks strictly backward to an earlier producer, so
//! the guard only ever fires at a genuine cycle).
//!
//! ## Stack safety
//!
//! The walk is iterative (explicit work stack), so it is heap-bounded,
//! not call-stack-bounded — a 10k-deep store prologue or a deep
//! phi fan-out costs O(1) host stack.

use entity_utils::DenseEntitySet;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

/// Pluggable aliasing oracle for the memory-SSA walk.
pub(crate) trait MemorySSAWalker {
    /// May the memory written by `mem_def` alias the location read by
    /// `load`?
    ///
    /// `mem_def` is never a `MemPhi` or `InitialMemory`: the walker
    /// handles phis structurally (recursing into predecessors) and
    /// treats `InitialMemory` as the clean chain root, so the oracle
    /// classifies every other producer it meets on the chain —
    /// `Store` / `Call` / `CallOther` and any opaque memory producer.
    /// A conservative oracle returns `true` for producers it cannot
    /// reason about.
    ///
    /// Returning `true` terminates the walk with `mem_def` as the
    /// nearest clobber; returning `false` advances the cursor past
    /// `mem_def` to its own memory input (or terminates the branch
    /// cleanly when the producer has no incoming memory edge).
    fn may_alias(&mut self, function: &Function, load: NodeOutputId, mem_def: NodeOutputId)
        -> bool;
}

/// Finds the nearest memory definition reachable backward from `load`'s
/// memory input that may alias `load` (per `walker`) — the clobber.
///
/// Returns `None` if the chain reaches `InitialMemory` (or any
/// non-memory / malformed producer) with no aliasing def on any path.
///
/// Walks only memory-token edges (input slot 0 of `Load` / `Store` /
/// `MemPhi`; the memory input of `Call` / `CallOther`) and recurses
/// per-predecessor at `MemPhi`.  See the module docs for the full
/// semantics and the cycle-guard contract.
pub(crate) fn walk_memory_ssa<W: MemorySSAWalker>(
    function: &Function,
    walker: &mut W,
    load: NodeOutputId,
    load_mem: NodeOutputId,
) -> Option<NodeOutputId> {
    let mut visited: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
    walk_from(function, walker, load, load_mem, &mut visited)
}

/// The memory-token input of a memory-chain node, if any.  Slot 0 for
/// `Store` / `Load` / `MemPhi`; the call's memory input (slot 1) for
/// `Call` / `CallOther`.  `None` for `InitialMemory` and anything that
/// does not carry an incoming memory edge.
fn prev_mem(function: &Function, node: NodeId) -> Option<NodeOutputId> {
    let inputs = function.node_inputs(node);
    match *function.node_kind(node) {
        NodeKind::Store(_) | NodeKind::Load(_) => inputs.into_iter().next(),
        NodeKind::Call | NodeKind::CallOther { .. } => inputs.into_iter().nth(1),
        _ => None,
    }
}

/// Iterative backward walk from a single memory cursor.  `MemPhi`
/// recursion is handled by an explicit work stack so the host call
/// stack stays O(1) regardless of chain depth or phi fan-out.
fn walk_from<W: MemorySSAWalker>(
    function: &Function,
    walker: &mut W,
    load: NodeOutputId,
    start_mem: NodeOutputId,
    visited: &mut DenseEntitySet<NodeOutputId>,
) -> Option<NodeOutputId> {
    // A linear cursor walk with MemPhi predecessors pushed onto an
    // explicit stack.  The first predecessor that reaches a clobber
    // wins (OR-over-paths), so we can return as soon as any branch
    // produces `Some`.
    let mut stack: Vec<NodeOutputId> = vec![start_mem];

    while let Some(mut cur) = stack.pop() {
        // Walk this branch linearly until it terminates, forks at a
        // MemPhi, or short-circuits on a cycle.
        loop {
            if !visited.insert(cur) {
                // Already visited on this walk — treat as clean for this
                // edge (a cycle contributes no new clobber).
                break;
            }
            let node = function.node_for_output(cur);
            match *function.node_kind(node) {
                NodeKind::InitialMemory => break,
                NodeKind::MemPhi => {
                    // Inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
                    // Fork: push every predecessor's memory input onto
                    // the work stack and continue the outer loop.  ANY
                    // predecessor reaching a clobber makes the phi a
                    // clobber boundary.
                    let inputs = function.node_inputs(node);
                    for pred in inputs.into_iter().skip(1) {
                        stack.push(pred);
                    }
                    break;
                }
                // Every other memory producer — `Store` / `Call` /
                // `CallOther` and any opaque kind — is classified by the
                // oracle.  A `true` verdict is the nearest clobber; a
                // `false` verdict advances to the producer's own memory
                // input, or terminates the branch cleanly when the
                // producer carries no incoming memory edge.
                _ => {
                    if walker.may_alias(function, load, cur) {
                        return Some(cur);
                    }
                    match prev_mem(function, node) {
                        Some(p) => cur = p,
                        None => break,
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests;
