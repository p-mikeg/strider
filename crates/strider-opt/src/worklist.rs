//! Shared per-pass worklist helpers used by every fold-style optimizer.
//!
//! Passes use [`entity_utils::Worklist<NodeId>`] directly (FIFO with
//! dedup-while-queued, backed by a [`DenseEntitySet<NodeId>`] for O(1)
//! membership keyed by `NodeId`'s u32 index).  This module retains the
//! kind-filtered seeding helper.

use entity_utils::Worklist;
use strider_ir::node::{NodeId, NodeKind};

/// Seeds a worklist with every node reachable from `ctx.entry()` whose
/// [`NodeKind`] satisfies `pred`, in **global reverse-post-order**
/// (entry-first, every producer before its consumers).
///
/// Replaces the recurring `ctx.walk_kind(...).collect::<Vec<_>>()`
/// followed by a `for node in collected { ... }` loop: the seeded
/// worklist gives the same one-shot iteration semantics (kind-filtered,
/// no re-enqueue unless a rule explicitly pushes consumers) without
/// allocating an intermediate `Vec`, and lets passes upgrade in place to
/// cascading rewrites by calling [`Worklist::enqueue`] on consumers.
///
/// Seeding in RPO is behaviour-preserving — the reachable SET is
/// identical to the pre-order walk, and the worklist is a fixpoint so
/// the result is order-independent — but it lets cascading folds settle
/// in fewer iterations because operands are processed before consumers.
pub(crate) fn seeded_kind<P>(ctx: &crate::EditFunction<'_>, pred: P) -> Worklist<NodeId>
where
    P: Fn(&NodeKind) -> bool,
{
    ctx.reverse_postorder_filter(pred).collect()
}
