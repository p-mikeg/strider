//! `PeepholePass` trait + generic driver for the kind-filtered, per-node
//! rewrite shape shared by most opt passes in this crate.
//!
//! A `PeepholePass` impl declares (a) which `NodeKind`s it cares about and
//! (b) how to attempt one rewrite at a given root.  The driver
//! ([`run_peephole`]) handles the worklist, kind-filtered seeding,
//! and (optionally) consumer re-enqueue on a successful rewrite.  Passes
//! that don't need cascading re-enqueue can override
//! [`PeepholePass::propagate_to_consumers`] to return `false`.
//!
//! `PeepholePass` is *below* the existing [`crate::pipeline::Optimizer`]
//! trait — concrete passes implement `PeepholePass` and provide a thin
//! `Optimizer` impl whose body is just `run_peephole(self, edit)`.  The
//! pipeline still consumes `dyn Optimizer` exactly as before.
//!
//! Passes that don't fit this shape (analytic passes, multi-stage passes
//! with a per-pass memo, etc.) keep their hand-written `Optimizer` impl.

use entity_utils::Worklist;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::OptimizationResult;

/// The producer node of each value input of `node`, in input-slot order.
///
/// Collapses the recurring `node_inputs(n).iter().map(|i| producer(i))`
/// micro-idiom (the input-producer cone walk) into one named helper.  Returns
/// an owned `Vec` rather than a borrowing iterator so callers may **mutate**
/// `edit` while iterating the producers (e.g. extend a fingerprint per producer)
/// — the borrow must end before the mutation.  Iterate-only callers that do not
/// touch `edit` in the loop body prefer the allocation-free
/// [`input_producers_iter`].
pub(crate) fn input_producers<V: IRViewer>(edit: &V, node: NodeId) -> Vec<NodeId> {
    input_producers_iter(edit, node).collect()
}

/// Borrowing-iterator counterpart of [`input_producers`]: yields each value
/// input's producer node without an intermediate `Vec`.  Holds an immutable
/// borrow of `edit` for the iterator's lifetime, so use it only where the loop
/// body does not also borrow `edit` mutably (the worklist / fingerprint-extend
/// callers keep the owned-`Vec` [`input_producers`] instead).
pub(crate) fn input_producers_iter<V: IRViewer>(
    edit: &V,
    node: NodeId,
) -> impl Iterator<Item = NodeId> + '_ {
    edit.node_inputs(node).into_iter().map(|v| edit.producer(v))
}

/// Outcome of a single [`PeepholePass::try_rewrite`] attempt at one root.
///
/// A rewrite reports not just *whether* it fired but also *which* node
/// (if any) it freshly created, so the driver can re-examine that node
/// for cascading folds without scanning the new-NodeId range.
pub(crate) enum PeepholeRewrite {
    /// Nothing matched — no change.
    NoChange,
    /// A rewrite fired.  `new_node` is `Some(n)` when the rewrite
    /// produced a FRESH node that should be re-examined for cascading
    /// folds; `None` for a pure redirect/collapse to an
    /// already-existing value.
    Changed { new_node: Option<NodeId> },
}

impl PeepholeRewrite {
    /// `Some(v)` → `Changed { new_node: Some(producer(v)) }`; `None` →
    /// `NoChange`.  Collapses the recurring "a rule returned an
    /// `Option<ValueId>` of the freshly-produced value" mapping.
    pub(crate) fn from_new_value(edit: &crate::EditFunction<'_>, v: Option<ValueId>) -> Self {
        v.map_or(PeepholeRewrite::NoChange, |new_value| {
            PeepholeRewrite::Changed {
                new_node: Some(edit.producer(new_value)),
            }
        })
    }

    /// `true` → `Changed { new_node: None }`; `false` → `NoChange`.
    /// Collapses the recurring "a rule reported whether it fired" mapping
    /// for passes that rewire to an already-existing value (no fresh node).
    pub(crate) fn from_changed(changed: bool) -> Self {
        if changed {
            PeepholeRewrite::Changed { new_node: None }
        } else {
            PeepholeRewrite::NoChange
        }
    }
}

/// The order [`run_peephole`] seeds its worklist in.  The order is not
/// universal: value-propagation passes (constant folding, known-bits) want
/// operands settled before their consumers, while canonicalization passes
/// that collapse a tree to a single node want to match the OUTERMOST shape
/// before a sub-rewrite can destroy it.
pub(crate) enum SeedOrder {
    /// Operands before consumers (defs-before-uses).  The default; right for
    /// value-propagation / cascading folds.
    ReversePostorder,
    /// Consumers before operands (uses-before-defs, top-down).  Right for
    /// canonicalization passes whose rules match an outer pattern that a
    /// bottom-up sub-rewrite would otherwise break first.
    Postorder,
}

/// A kind-filtered, per-node rewrite pass.  See module docs.
pub(crate) trait PeepholePass {
    /// Which `NodeKind`s does this pass care about?  Seeded into the
    /// worklist by [`run_peephole`] in [`Self::seed_order`].
    fn matches_kind(&self, kind: &NodeKind) -> bool;

    /// The order the seed worklist is built in.  Defaults to
    /// [`SeedOrder::ReversePostorder`] (operands before consumers); a
    /// collapse/canonicalization pass overrides it to
    /// [`SeedOrder::Postorder`] so it matches outer shapes first.
    fn seed_order(&self) -> SeedOrder {
        SeedOrder::ReversePostorder
    }

    /// Attempt to rewrite at `root`.  Returns
    /// [`PeepholeRewrite::Changed`] if a rewrite fired (the driver will
    /// re-enqueue consumers iff [`Self::propagate_to_consumers`] is
    /// `true`, and re-examine `new_node` if the rewrite reports one).
    ///
    /// # Errors
    /// Propagates the first error from the underlying rewrite.
    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite>;

    /// When `true`, the driver re-enqueues every consumer of `root`'s
    /// outputs after a successful rewrite, so cascading folds can
    /// re-fire in the same sweep (no need for a fresh fixed-point
    /// iteration).  Default `true` matches the `ConstantFold` shape.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}

/// Drive a [`PeepholePass`] over the reachable graph.
///
/// Seeds the worklist with every kind-matching reachable root, then
/// drains the worklist by calling `pass.try_rewrite` on each root.  On
/// [`PeepholeRewrite::Changed`], consumers of `root`'s outputs are
/// re-enqueued (subject to [`PeepholePass::propagate_to_consumers`]) so
/// cascading folds can fire in the same sweep, and — when the rewrite
/// reports a freshly-created `new_node` whose kind the pass cares about
/// — that node is enqueued too so it's re-examined within the same
/// sweep.  This drives the local fixpoint off what the rewrite *reports*
/// rather than scanning the new-NodeId range.
///
/// Consumers are snapshotted **before** `try_rewrite` runs because the
/// rewrite typically rewires uses to a replacement, leaving
/// `value_uses(old_out)` empty afterwards.  A `SmallVec<[NodeId; 8]>`
/// inlines the common case (~95% of IR nodes fan out to <=8 consumers)
/// to avoid heap allocation on the hot worklist path.
///
/// # Errors
/// Propagates the first error from `try_rewrite`.
pub(crate) fn run_peephole<P: PeepholePass>(
    pass: &P,
    edit: &mut crate::EditFunction<'_>,
    opt_ctx: &mut crate::pipeline::OptCtx<'_>,
) -> Result<OptimizationResult> {
    // Seed in the pass's chosen order, computed DIRECTLY for each variant —
    // no `reverse()` of an already-reversed sequence.  `ReversePostorder`
    // takes the global reverse-post-order (operands before consumers);
    // `Postorder` takes the global post-order (consumers before operands)
    // straight from the forward def→use post-order, NOT by reversing the RPO.
    //
    // `reverse_postorder_filter`/`postorder_filter` seed from the edit's CHEAP cached walk
    // (the O(1)-maintained `roots` + `live_nodes`), so there is no per-seed
    // `compute_full`.  The cached `roots` iterate in ascending-`NodeId` order,
    // which differs from `compute_full`'s preorder-discovery order; this is safe
    // because (a) the cached `live_nodes`/`roots` are kept exactly equal to the
    // entry-reachable set, and (b) `ConstantFold`'s AND-distribution rule was
    // made confluent (it fires only when it strictly simplifies), so any valid
    // RPO converges.
    let seed: Vec<NodeId> = match pass.seed_order() {
        SeedOrder::ReversePostorder => edit
            .reverse_postorder_filter(|k| pass.matches_kind(k))
            .collect(),
        SeedOrder::Postorder => edit.postorder_filter(|k| pass.matches_kind(k)).collect(),
    };
    let mut work: Worklist<NodeId> = seed.into_iter().collect();
    let mut overall = OptimizationResult::NoChange;
    let propagate = pass.propagate_to_consumers();
    // Reused per iteration to snapshot consumer NodeIds BEFORE running
    // the pass body.  After a rewrite, `value_uses(old_out)` is empty
    // (uses were rewired to the replacement), so capture consumers ahead.
    // Only consumers whose kind the pass cares about are snapshotted, so
    // `try_rewrite` is only ever handed a node matching `matches_kind`
    // (the same contract the seed walk establishes) — pass bodies don't
    // need a defensive kind re-check on entry.
    let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
    while let Some(root) = work.dequeue() {
        if propagate {
            consumers.clear();
            for &out in edit.node_outputs(root) {
                for (consumer, _) in edit.graph_ref().value_uses(out) {
                    if pass.matches_kind(edit.graph_ref().node_kind(consumer)) {
                        consumers.push(consumer);
                    }
                }
            }
        }
        let r = pass.try_rewrite(edit, opt_ctx, root)?;
        if let PeepholeRewrite::Changed { new_node } = r {
            overall = OptimizationResult::Changed;
            // Re-examine the node the rewrite reports it freshly created
            // (if any) whose kind the pass cares about.  A rewrite doesn't
            // only rewire consumers — it may build a fresh node (a folded
            // constant, a merged AND-mask, a new `Add`) that is itself
            // immediately rewritable by the same pass.  Without this,
            // whether it folds within one sweep depends on seed order; with
            // it, `run_peephole` reaches a local fixpoint over the pass's
            // rule set independent of seed order.
            if let Some(n) = new_node
                && pass.matches_kind(edit.graph_ref().node_kind(n))
            {
                work.enqueue(n);
            }
            if propagate {
                for &consumer in &consumers {
                    work.enqueue(consumer);
                }
            }
        }
    }
    Ok(overall)
}

/// Blanket [`Optimizer`](crate::pipeline::Optimizer) impl for every
/// `PeepholePass`: the `apply` body is always the same one-liner (hand the
/// pipeline's shared `EditFunction` to `run_peephole`), so a `PeepholePass`
/// type gets its `Optimizer` impl for free — no per-pass macro invocation.
/// `Clone + 'static` satisfies the `OptimizerClone` super-trait so the
/// pipeline can box-clone the pass.
impl<P: PeepholePass + Clone + 'static> crate::pipeline::Optimizer for P {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        run_peephole(self, edit, opt_ctx)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::cell::RefCell;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::make_empty_fn;

    use crate::error::Result;
    use crate::pipeline::OptimizationResult;

    /// A scriptable pass: matches on a configured kind predicate, records
    /// every `try_rewrite` invocation, and on match rewires the root's
    /// single value output to a fresh `IntConst(REPLACEMENT_K)`.  Used by
    /// the tests below to assert ordering / propagation behaviour without
    /// pulling in a real opt pass.
    struct ScriptedPass {
        match_kind: fn(&NodeKind) -> bool,
        do_rewrite: bool,
        propagate: bool,
        return_error: bool,
        visit_log: RefCell<Vec<u32>>,
        /// When `true`, the *first* successful rewrite creates a fresh
        /// kind-matching node (a duplicate `Add` reusing the root's two
        /// inputs) and rewires the root's output to it, instead of folding
        /// to an `IntConst`.  The flag is consumed on first use so the
        /// rewrite fires exactly once (no infinite cascade).  Used to prove
        /// that `run_peephole` re-enqueues newly-created kind-matching nodes.
        create_matching_once: RefCell<bool>,
    }

    const REPLACEMENT_K: u64 = 0xABCD_1234;

    impl PeepholePass for ScriptedPass {
        fn matches_kind(&self, k: &NodeKind) -> bool {
            (self.match_kind)(k)
        }
        fn try_rewrite(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
            root: NodeId,
        ) -> Result<PeepholeRewrite> {
            use cranelift_entity::EntityRef;
            self.visit_log.borrow_mut().push(root.index() as u32);
            if self.return_error {
                return Err(anyhow::anyhow!("scripted-pass forced error"));
            }
            if !self.do_rewrite {
                return Ok(PeepholeRewrite::NoChange);
            }
            let kind = *edit.node_kind(root);
            if !(self.match_kind)(&kind) {
                return Ok(PeepholeRewrite::NoChange);
            }
            let (root_value, ty) = edit.single_value_output(root)?;
            // When scripted to do so, the first rewrite builds a fresh
            // kind-matching node (a clone of the root `Add` reusing its two
            // value inputs) instead of folding to a constant.  The fresh
            // node should itself be re-visited by `run_peephole` — so the
            // rewrite REPORTS it as `new_node`.
            if *self.create_matching_once.borrow() {
                *self.create_matching_once.borrow_mut() = false;
                // Build a genuinely fresh `Add` with a distinct cacheable key
                // (the root's first input used twice) so the dedup cache
                // can't collapse it onto the already-seen root node.
                let first = edit.node_inputs(root)[0];
                let new_node = edit.create_node(
                    kind,
                    [first, first],
                    [strider_ir::node::ValueKind::Typed(ty)],
                );
                let [new_value] = edit.node_outputs_exact::<1>(new_node)?;
                edit.replace_value(root_value, new_value)?;
                return Ok(PeepholeRewrite::Changed {
                    new_node: Some(new_node),
                });
            }
            let new_value = edit.build_int_const(REPLACEMENT_K, ty)?;
            let new_node = edit.producer(new_value);
            edit.replace_value(root_value, new_value)?;
            Ok(PeepholeRewrite::Changed {
                new_node: Some(new_node),
            })
        }
        fn propagate_to_consumers(&self) -> bool {
            self.propagate
        }
    }

    /// `fn() -> u64 { return 7; }` — minimal reachable graph.
    fn one_const_fn() -> strider_ir::Function {
        make_empty_fn(|b| b.build_int_const(7u64, ValueType::I64)).unwrap()
    }

    /// `fn() -> u64 { return Add(11, 13); }`.
    fn add_two_consts() -> strider_ir::Function {
        make_empty_fn(|b| {
            let a = b.build_int_const(11u64, ValueType::I64)?;
            let bb = b.build_int_const(13u64, ValueType::I64)?;
            b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::I64)
        })
        .unwrap()
    }

    fn match_add(k: &NodeKind) -> bool {
        matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add))
    }
    fn match_nothing(_: &NodeKind) -> bool {
        false
    }

    #[test]
    fn run_peephole_on_minimal_graph_no_match() {
        let mut fg = one_const_fn();
        let pass = ScriptedPass {
            match_kind: match_nothing,
            do_rewrite: false,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::NoChange);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_pass_never_matches_returns_nochange() {
        // Graph has an Add but the pass kind-filter rejects everything.
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_nothing,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::NoChange);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_rewrites_and_reports_changed() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        assert!(!pass.visit_log.borrow().is_empty());

        // Return's value-input is now an IntConst (the rewrite replacement).
        let value = crate::test_support::return_value(fg.graph()).unwrap();
        let producer = fg.producer(value);
        assert!(
            matches!(fg.node_kind(producer), NodeKind::IntConst(_)),
            "Return's value input must be IntConst post-rewrite",
        );
    }

    #[test]
    fn run_peephole_with_propagate_false_skips_reenqueue() {
        // `Add(Add(1,2), 3)` — outer consumes inner.  With propagate=false
        // each Add is visited at most once (the seed-time visit).
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(1u64, ValueType::I64)?;
            let bb = b.build_int_const(2u64, ValueType::I64)?;
            let c = b.build_int_const(3u64, ValueType::I64)?;
            let inner = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::I64)?;
            b.build_int_binary_operation(inner, c, IntBinaryOp::Add, ValueType::I64)
        })
        .unwrap();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let _ = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        let log = pass.visit_log.borrow().clone();
        assert_eq!(log.len(), 2, "exactly two visits, no re-enqueue: {log:?}");
    }

    #[test]
    fn run_peephole_with_propagate_true_reenqueues_consumers() {
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(1u64, ValueType::I64)?;
            let bb = b.build_int_const(2u64, ValueType::I64)?;
            let c = b.build_int_const(3u64, ValueType::I64)?;
            let inner = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::I64)?;
            b.build_int_binary_operation(inner, c, IntBinaryOp::Add, ValueType::I64)
        })
        .unwrap();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        // Each Add visited at least once; propagate-true allows extra
        // re-enqueue visits.  The exact count depends on worklist dedup
        // policy, but the lower bound is 2.
        let log_len = pass.visit_log.borrow().len();
        assert!(
            log_len >= 2,
            "expected >=2 visits with propagate=true, got {log_len}"
        );
    }

    #[test]
    fn run_peephole_revisits_newly_created_matching_node() {
        // A rewrite that BUILDS a fresh kind-matching node (not just rewires
        // to a constant) must have that new node re-examined within the same
        // `run_peephole` sweep — otherwise whether it folds depends on seed
        // order.  Here the single seeded `Add` is rewritten into a brand-new
        // `Add`; the driver must enqueue and visit that new node.
        use cranelift_entity::EntityRef;
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(true),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        // The id the next created node will take == the new `Add`'s id.
        let new_node_idx = edit.graph_ref().next_node_id().index() as u32;
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        let log = pass.visit_log.borrow().clone();
        assert!(
            log.contains(&new_node_idx),
            "freshly-created matching node {new_node_idx} must be re-visited, log={log:?}",
        );
    }

    #[test]
    fn run_peephole_propagates_pass_internal_error() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: false,
            propagate: false,
            return_error: true,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx);
        assert!(r.is_err(), "errored pass must surface error");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.contains("scripted-pass forced error"),
            "error must propagate, got {msg}",
        );
    }
}
