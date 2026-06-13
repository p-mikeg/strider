//! `DedupNodes` — merges structurally-identical cacheable value nodes that
//! a rewrite left un-deduplicated.
//!
//! The graph's construction cache merges structurally-identical cacheable
//! nodes at CREATION time (keyed on `(kind, inputs, output-kinds)`).  But a
//! rewrite that rewires a live node's inputs — most notably
//! [`crate::PhiCollapse`] redirecting a trivial `Phi` to its single
//! predecessor via `replace_all_uses` — can turn one node into a structural
//! twin of an existing node WITHOUT re-canonicalising it.  Two nodes then
//! compute the same value, and analyses keyed on node identity treat them as
//! unrelated.  Concretely, a range-check guard `cmp edi,7` and a table index
//! both read `Truncate(InitialVar(rdi))`, but as two separate `Truncate`
//! nodes (one per SSA phi the lifter introduced); after `PhiCollapse` rewires
//! both phis to the same `InitialVar`, the two `Truncate`s become identical
//! yet stay distinct, so [`crate::value_range`] never carries the guard's
//! bound to the index.
//!
//! This pass restores the invariant: walk reachable nodes in reverse
//! post-order (every producer precedes its consumers, so a node's inputs are
//! already canonical when it is visited) and, for each single-value-output
//! cacheable node, merge it into the first node sharing its structural key via
//! [`crate::EditFunction::replace_value`] (which absorbs the duplicate's
//! asm-fingerprint into the survivor, preserving the superset-only contract).
//!
//! ## Why it is sound
//!
//! Two nodes share a [`CseKey`] only when they have the same kind, the same
//! ordered input values, and the same output kind — i.e. the construction
//! cache itself would have deduplicated them.  Because a twin shares *all*
//! inputs with the survivor, merging it orphans nothing the survivor still
//! needs (no input subtree is detached), and the surviving node accumulates
//! the duplicate's uses, so it never becomes dead mid-pass.

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;
use strider_ir::node::{NodeKind, ValueId, ValueKind};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Merges structurally-identical cacheable single-value-output nodes that a
/// rewrite left un-deduplicated.
#[derive(Clone, Copy)]
pub struct DedupNodes;

/// Structural identity of a single-value-output node: kind, ordered value
/// inputs, and output kind.  Mirrors the graph's construction-time dedup key.
/// The output kind is load-bearing — a `Truncate` to `I8` and a `Truncate` of
/// the same input to `I32` share `(kind, inputs)` but are different values.
type CseKey = (NodeKind, Vec<ValueId>, ValueKind);

impl Optimizer for DedupNodes {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // Reverse post-order visits every producer before its consumers, so by
        // the time a node is keyed its input edges already point at the
        // canonical twin (an earlier merge's `replace_value` rewired them).
        let order = ctx.reverse_postorder();
        let mut seen: FxHashMap<CseKey, ValueId> = FxHashMap::default();
        let mut overall = OptimizationResult::NoChange;

        for node in order {
            let kind = *ctx.node_kind(node);
            // Restrict to what the construction cache would dedup AND to a
            // single VALUE output.  This excludes control (`If`) and
            // multi-output (`Region`) kinds, the non-cacheable phis / calls /
            // terminators, and memory-token producers (`Store`,
            // `InitialMemory`) whose lone output is not a value.
            if !kind.is_cacheable() {
                continue;
            }
            let &[out] = ctx.node_outputs(node) else {
                continue;
            };
            let out_kind = ctx.value_kind(out);
            if out_kind.as_value().is_none() {
                continue;
            }
            let inputs: Vec<ValueId> = ctx.node_inputs(node).into_iter().collect();
            let key = (kind, inputs, out_kind);

            match seen.get(&key) {
                // First occurrence of this shape becomes the canonical survivor.
                None => {
                    seen.insert(key, out);
                }
                // A structural twin: redirect every use of this duplicate onto
                // the canonical value (absorbing the duplicate's asm-fingerprint
                // into the survivor) and let the now-dead duplicate cull.
                Some(&canon) if canon != out => {
                    if ctx.replace_value(out, canon)? {
                        overall = OptimizationResult::Changed;
                    }
                }
                Some(_) => {}
            }
        }
        Ok(overall)
    }
}
