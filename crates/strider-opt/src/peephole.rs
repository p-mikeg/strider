//! An impl declares which `NodeKind`s it cares about and how to attempt one
//! rewrite at a root; [`run_peephole`] owns the worklist, kind-filtered
//! seeding, and consumer re-enqueue.

use entity_utils::Worklist;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::OptimizationResult;

/// The producer node of each input of `node`, in input-slot order. Every
/// input: a `Load` yields its memory producer, a `Phi` its `Region`.
pub(crate) fn input_producers_iter<V: IRViewer>(
    edit: &V,
    node: NodeId,
) -> impl Iterator<Item = NodeId> + '_ {
    edit.node_inputs(node).into_iter().map(|v| edit.producer(v))
}

/// Outcome of one [`PeepholePass::try_rewrite`] attempt.
pub(crate) enum PeepholeRewrite {
    NoChange,
    /// `new_node` is `Some(n)` when the rewrite built a FRESH node worth
    /// re-examining for cascading folds; `None` for a pure redirect to an
    /// already-existing value.
    Changed {
        new_node: Option<NodeId>,
    },
}

impl PeepholeRewrite {
    pub(crate) fn from_new_value(edit: &crate::EditFunction<'_>, v: Option<ValueId>) -> Self {
        v.map_or(PeepholeRewrite::NoChange, |new_value| {
            PeepholeRewrite::Changed {
                new_node: Some(edit.producer(new_value)),
            }
        })
    }

    /// For a rewire to an existing value, creating no fresh node.
    pub(crate) fn from_changed(changed: bool) -> Self {
        if changed {
            PeepholeRewrite::Changed { new_node: None }
        } else {
            PeepholeRewrite::NoChange
        }
    }
}

/// The output of the FIRST rule to fire at `root`, or `None` if none does.
///
/// Later rules do not run: the first firing rule redirects every use of the
/// root's output, so a later match sits on a node nothing consumes and its
/// rewrite would report a value the caller must not re-enqueue.
///
/// # Errors
/// Propagates the first error returned by a rule.
pub(crate) fn first_matching_rule<R>(
    rules: &[R],
    edit: &mut crate::EditFunction<'_>,
    root: NodeId,
) -> Result<Option<ValueId>>
where
    R: for<'g> Fn(&mut crate::EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
{
    for rule in rules {
        if let Some(out) = rule(edit, root)? {
            return Ok(Some(out));
        }
    }
    Ok(None)
}

/// Worklist seed order.
pub(crate) enum SeedOrder {
    /// Operands before consumers. The default; what a value-propagation
    /// pass wants.
    ReversePostorder,
    /// Consumers before operands, so a tree-collapsing canonicalization
    /// matches the OUTERMOST shape before a sub-rewrite destroys it.
    Postorder,
}

pub(crate) trait PeepholePass {
    fn matches_kind(&self, kind: &NodeKind) -> bool;

    /// Drops any memo the pass carries: it was read off the graph as the
    /// previous sweep left it.
    fn start_sweep(&self) {}

    fn seed_order(&self) -> SeedOrder {
        SeedOrder::ReversePostorder
    }

    /// `root` always matches [`Self::matches_kind`].
    ///
    /// # Errors
    /// Propagates the first error from the underlying rewrite.
    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite>;

    /// When `true`, a successful rewrite re-enqueues every consumer of
    /// `root`'s outputs, so cascading folds fire in the same sweep instead
    /// of waiting for another fixed-point iteration.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}

/// Drives a [`PeepholePass`] over the reachable graph to a local fixpoint,
/// off what each rewrite reports rather than by scanning new NodeIds.
///
/// # Errors
/// Propagates the first error from `try_rewrite`.
pub(crate) fn run_peephole<P: PeepholePass>(
    pass: &P,
    edit: &mut crate::EditFunction<'_>,
    opt_ctx: &mut crate::pipeline::OptCtx<'_>,
) -> Result<OptimizationResult> {
    pass.start_sweep();
    // Seeds iterate in reverse-postorder (or postorder, per `seed_order`), not
    // discovery/preorder order. Safe because the cached live set stays exactly
    // the entry-reachable set, and every rule is confluent (fires only when it
    // strictly simplifies), so any valid order converges.
    let seed: Vec<NodeId> = match pass.seed_order() {
        SeedOrder::ReversePostorder => edit
            .reverse_postorder_filter(|k| pass.matches_kind(k))
            .collect(),
        SeedOrder::Postorder => edit.postorder_filter(|k| pass.matches_kind(k)).collect(),
    };
    // Fresh nodes get fresh `NodeId`s, which the worklist's dedup cannot
    // collapse, so a non-confluent rule pair spins forever; the budget turns
    // that into an error instead of a hang.
    let dequeue_budget = seed.len().saturating_mul(64).saturating_add(1024);
    let mut dequeued = 0usize;
    let mut work: Worklist<NodeId> = seed.into_iter().collect();
    let mut overall = OptimizationResult::NoChange;
    let propagate = pass.propagate_to_consumers();
    // Snapshotted before `try_rewrite` runs: a rewrite rewires uses to its
    // replacement, leaving `value_uses(old_out)` empty afterwards.
    // Kind-filtered here, so `try_rewrite` only ever sees a node matching
    // `matches_kind`.
    let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
    while let Some(root) = work.dequeue() {
        dequeued += 1;
        if dequeued > dequeue_budget {
            anyhow::bail!(
                "peephole pass did not converge after {dequeue_budget} worklist \
                 dequeues; a rule pair is most likely undoing another's work"
            );
        }
        if propagate {
            consumers.clear();
            for &out in edit.node_outputs(root) {
                for (consumer, _) in edit.value_uses(out) {
                    if pass.matches_kind(edit.node_kind(consumer)) {
                        consumers.push(consumer);
                    }
                }
            }
        }
        let r = pass.try_rewrite(edit, opt_ctx, root)?;
        if let PeepholeRewrite::Changed { new_node } = r {
            overall = OptimizationResult::Changed;
            // A fresh node (a folded constant, a merged AND-mask) the same pass
            // can rewrite again; unqueued, whether that fires in this sweep
            // would depend on seed order.
            if let Some(n) = new_node
                && pass.matches_kind(edit.node_kind(n))
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
    use super::*;
    use std::cell::RefCell;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::make_empty_fn;

    use crate::error::Result;
    use crate::pipeline::OptimizationResult;

    /// Records every `try_rewrite` invocation and on match rewires the
    /// root's single value output to a fresh `IntConst(REPLACEMENT_K)`.
    struct ScriptedPass {
        match_kind: fn(&NodeKind) -> bool,
        do_rewrite: bool,
        propagate: bool,
        return_error: bool,
        visit_log: RefCell<Vec<u32>>,
        /// The first successful rewrite builds a fresh kind-matching node
        /// instead of folding to an `IntConst`. Consumed on first use so it
        /// cannot cascade forever.
        create_matching_once: RefCell<bool>,
        /// Never consume `create_matching_once`, so every rewrite mints
        /// another matching node: a stand-in for a non-confluent rule pair.
        cascade_forever: bool,
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
            if *self.create_matching_once.borrow() {
                if !self.cascade_forever {
                    *self.create_matching_once.borrow_mut() = false;
                }
                // Distinct cacheable key (the root's first input used twice)
                // so the dedup cache can't collapse this onto the root node.
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

    /// `fn() -> u64 { return 7; }`, the minimal reachable graph.
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
            cascade_forever: false,
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::NoChange);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_pass_never_matches_returns_nochange() {
        // The graph has an Add but the kind filter rejects everything.
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_nothing,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(false),
            cascade_forever: false,
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
            cascade_forever: false,
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        assert!(!pass.visit_log.borrow().is_empty());

        let value = crate::test_support::return_value(fg.graph()).unwrap();
        let producer = fg.producer(value);
        assert!(
            matches!(fg.node_kind(producer), NodeKind::IntConst(_)),
            "Return's value input must be IntConst post-rewrite",
        );
    }

    #[test]
    fn run_peephole_with_propagate_false_skips_reenqueue() {
        // `Add(Add(1,2), 3)`: the outer consumes the inner. With
        // propagate=false each Add is visited once, at seed time.
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
            cascade_forever: false,
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
            cascade_forever: false,
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        // The exact count depends on worklist dedup policy; only the lower
        // bound of 2 is guaranteed.
        let log_len = pass.visit_log.borrow().len();
        assert!(
            log_len >= 2,
            "expected >=2 visits with propagate=true, got {log_len}"
        );
    }

    #[test]
    fn run_peephole_revisits_newly_created_matching_node() {
        // The seeded `Add` is rewritten into a brand-new `Add`, which the
        // driver must enqueue and visit in the same sweep; otherwise whether
        // it folds would depend on seed order.
        use cranelift_entity::EntityRef;
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(true),
            cascade_forever: false,
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        // The next id to be handed out is the new `Add`'s.
        let new_node_idx = edit.graph_ref().next_node_id().index() as u32;
        let r = run_peephole(&pass, &mut edit, &mut octx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        let log = pass.visit_log.borrow().clone();
        assert!(
            log.contains(&new_node_idx),
            "freshly-created matching node {new_node_idx} must be re-visited, log={log:?}",
        );
    }

    /// Once a rule fires, the rules after it must not even be tried.
    #[test]
    fn first_matching_rule_stops_at_the_first_fire() {
        use std::cell::Cell;
        let mut fg = add_two_consts();
        let add = fg
            .graph()
            .all_node_ids()
            .find(|&n| match_add(fg.node_kind(n)))
            .unwrap();
        let winner = crate::test_support::return_value(fg.graph()).unwrap();
        let later_tries = &Cell::new(0usize);

        type Rule<'a> =
            Box<dyn Fn(&mut crate::EditFunction<'_>, NodeId) -> Result<Option<ValueId>> + 'a>;
        let rules: Vec<Rule> = vec![
            Box::new(move |_edit, _root| Ok(Some(winner))),
            Box::new(|_edit, _root| Ok(None)),
            Box::new(move |_edit, _root| {
                later_tries.set(later_tries.get() + 1);
                Ok(Some(winner))
            }),
        ];

        let mut edit = crate::EditFunction::new(&mut fg);
        let got = first_matching_rule(&rules, &mut edit, add).unwrap();
        assert_eq!(got, Some(winner), "the first firing rule's output wins");
        assert_eq!(later_tries.get(), 0, "no rule may run after the first fire");
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
            cascade_forever: false,
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

    /// A rule that mints a fresh matching node on every fire never reaches a
    /// fixpoint. The worklist dedups by `NodeId`, which a fresh id defeats, so
    /// without a bound the driver spins and grows the arena forever.
    #[test]
    fn run_peephole_bails_out_instead_of_spinning_forever() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            match_kind: match_add,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
            create_matching_once: RefCell::new(true),
            cascade_forever: true,
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let mut octx = crate::pipeline::OptCtx::new(None);
        let err = run_peephole(&pass, &mut edit, &mut octx)
            .expect_err("a non-confluent pass must error, not hang");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not converge"), "unexpected error: {msg}");
        // The bound is on DEQUEUES, which on a propagating pass outnumber the
        // rewrites by a large factor.
        assert!(
            msg.contains("dequeues"),
            "the bound must name its unit: {msg}"
        );
    }
}
