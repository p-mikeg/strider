use entity_utils::Worklist;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;

/// The producer of each input of `node`, in slot order, control and memory
/// inputs included.
pub(crate) fn input_producers_iter<V: IRViewer>(
    edit: &V,
    node: NodeId,
) -> impl Iterator<Item = NodeId> + '_ {
    edit.node_inputs(node).into_iter().map(|v| edit.producer(v))
}

pub(crate) trait PeepholePass {
    /// Per-sweep memo: built empty when a sweep starts, dropped when it ends.
    type Sweep: Default;

    /// Re-enqueue a rewritten root's consumers in the same sweep, so cascading
    /// folds fire now instead of in another fixed-point iteration.
    const PROPAGATE: bool = false;

    fn matches_kind(&self, kind: &NodeKind) -> bool;

    /// `root` matches [`Self::matches_kind`] and is live. Returns the value
    /// the root was redirected to, whose producer is re-examined; `None` when
    /// there is nothing to revisit.
    fn try_rewrite(
        &self,
        sweep: &mut Self::Sweep,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>>;

    /// Runs once after every sweep; `changed` when it rewrote something.
    fn after_sweep(&self, _edit: &mut crate::EditFunction<'_>, _changed: bool) -> Result<()> {
        Ok(())
    }
}

/// Drives a [`PeepholePass`] over the live graph to a local fixpoint. `true`
/// when some rewrite moved a use or killed a node.
pub(crate) fn run_peephole<P: PeepholePass>(
    pass: &P,
    edit: &mut crate::EditFunction<'_>,
    opt_ctx: &crate::pipeline::OptCtx<'_>,
) -> Result<bool> {
    let mut sweep = P::Sweep::default();
    // Operands first (reverse postorder).
    let seed: Vec<NodeId> = edit
        .live_reverse_postorder_filter(|k| pass.matches_kind(k))
        .collect();
    // Fresh `NodeId`s defeat the worklist's dedup, so a non-confluent rule
    // pair would spin forever; the budget makes it an error.
    let dequeue_budget = seed.len().saturating_mul(64).saturating_add(1024);
    let mut dequeued = 0usize;
    let mut work: Worklist<NodeId> = seed.into_iter().collect();
    let mut overall = false;
    // Snapshotted before `try_rewrite`, which empties `value_uses(old_out)`.
    let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
    while let Some(root) = work.dequeue() {
        dequeued += 1;
        if dequeued > dequeue_budget {
            anyhow::bail!(
                "peephole pass did not converge after {dequeue_budget} worklist \
                 dequeues; a rule pair is most likely undoing another's work"
            );
        }
        // An earlier rewrite of this sweep may have culled it.
        if !edit.is_live(root) {
            continue;
        }
        if P::PROPAGATE {
            consumers.clear();
            for &out in edit.node_outputs(root) {
                for (consumer, _) in edit.value_uses(out) {
                    if pass.matches_kind(edit.node_kind(consumer)) {
                        consumers.push(consumer);
                    }
                }
            }
        }
        let generation = edit.generation();
        let new_value = pass.try_rewrite(&mut sweep, edit, opt_ctx, root)?;
        if edit.generation() != generation {
            overall = true;
            // Unqueued, whether the fresh node fires this sweep would depend
            // on seed order.
            if let Some(n) = new_value.map(|v| edit.producer(v))
                && pass.matches_kind(edit.node_kind(n))
            {
                work.enqueue(n);
            }
            if P::PROPAGATE {
                for &consumer in &consumers {
                    work.enqueue(consumer);
                }
            }
        }
    }
    Ok(overall)
}

impl<P: PeepholePass + Sync> crate::pipeline::Optimizer for P {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &crate::pipeline::OptCtx<'_>,
    ) -> Result<()> {
        let changed = run_peephole(self, edit, opt_ctx)?;
        self.after_sweep(edit, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::make_empty_fn;

    use crate::Result;

    /// Records every `try_rewrite` and on match rewires the root's value to
    /// a fresh `IntConst(REPLACEMENT_K)`.
    struct ScriptedPass<const P: bool> {
        match_kind: fn(&NodeKind) -> bool,
        do_rewrite: bool,
        return_error: bool,
        visit_log: RefCell<Vec<u32>>,
        /// The first rewrite builds a fresh kind-matching node instead of an
        /// `IntConst`. Consumed on first use.
        create_matching_once: RefCell<bool>,
        /// Never consume `create_matching_once`, so every rewrite mints
        /// another matching node: a stand-in for a non-confluent rule pair.
        cascade_forever: bool,
        /// Killed on the first visit, which then rewrites nothing.
        kill_first: RefCell<Option<NodeId>>,
    }

    impl<const P: bool> ScriptedPass<P> {
        fn new(match_kind: fn(&NodeKind) -> bool, do_rewrite: bool) -> Self {
            Self {
                match_kind,
                do_rewrite,
                return_error: false,
                visit_log: RefCell::default(),
                create_matching_once: RefCell::new(false),
                cascade_forever: false,
                kill_first: RefCell::new(None),
            }
        }
    }

    const REPLACEMENT_K: u64 = 0xABCD_1234;

    impl<const P: bool> PeepholePass for ScriptedPass<P> {
        type Sweep = ();
        const PROPAGATE: bool = P;

        fn matches_kind(&self, k: &NodeKind) -> bool {
            (self.match_kind)(k)
        }
        fn try_rewrite(
            &self,
            _sweep: &mut (),
            edit: &mut crate::EditFunction<'_>,
            _opt_ctx: &crate::pipeline::OptCtx<'_>,
            root: NodeId,
        ) -> Result<Option<ValueId>> {
            use cranelift_entity::EntityRef;
            self.visit_log.borrow_mut().push(root.index() as u32);
            if let Some(victim) = self.kill_first.borrow_mut().take() {
                edit.kill_node(victim);
                return Ok(None);
            }
            if self.return_error {
                return Err(anyhow::anyhow!("scripted-pass forced error"));
            }
            if !self.do_rewrite {
                return Ok(None);
            }
            let kind = *edit.node_kind(root);
            if !(self.match_kind)(&kind) {
                return Ok(None);
            }
            let (root_value, ty) = edit.single_value_output(root)?;
            if *self.create_matching_once.borrow() {
                if !self.cascade_forever {
                    *self.create_matching_once.borrow_mut() = false;
                }
                // A key the dedup cache cannot collapse onto an existing node.
                let first = edit.node_inputs(root)[0];
                let second = if self.cascade_forever {
                    let visits = self.visit_log.borrow().len() as u64;
                    edit.build_int_const(REPLACEMENT_K + visits, ty)?
                } else {
                    first
                };
                let new_node = edit.create_node(
                    kind,
                    [first, second],
                    [strider_ir::node::ValueKind::Typed(ty)],
                );
                let [new_value] = edit.node_outputs_exact::<1>(new_node)?;
                edit.replace_value(root_value, new_value);
                return Ok(Some(new_value));
            }
            let new_value = edit.build_int_const(REPLACEMENT_K, ty)?;
            edit.replace_value(root_value, new_value);
            Ok(Some(new_value))
        }
    }

    /// `fn() -> u64 { return Add(11, 13); }`.
    fn add_two_consts() -> strider_ir::Function {
        make_empty_fn(|b| {
            let a = b.build_int_const(11u64, ValueType::int(64))?;
            let bb = b.build_int_const(13u64, ValueType::int(64))?;
            b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::int(64))
        })
        .unwrap()
    }

    /// `fn() -> u64 { return Add(Add(1, 2), 3); }`: the outer `Add` consumes
    /// the inner.
    fn nested_adds() -> strider_ir::Function {
        make_empty_fn(|b| {
            let a = b.build_int_const(1u64, ValueType::int(64))?;
            let ty = ValueType::int(64);
            let inner = b.build_const_binop(2u64, a, IntBinaryOp::Add, ty)?;
            b.build_const_binop(3u64, inner, IntBinaryOp::Add, ty)
        })
        .unwrap()
    }

    /// One [`run_peephole`] sweep of `pass` over `fg`.
    fn sweep<P: PeepholePass>(pass: &P, fg: &mut strider_ir::Function) -> Result<bool> {
        run_peephole(
            pass,
            &mut crate::EditFunction::new(fg),
            &crate::OptCtx::new(None),
        )
    }

    fn match_add(k: &NodeKind) -> bool {
        matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add))
    }
    fn match_nothing(_: &NodeKind) -> bool {
        false
    }

    #[test]
    fn run_peephole_pass_never_matches_returns_nochange() {
        // The graph has an Add but the kind filter rejects everything.
        let mut fg = add_two_consts();
        let pass = ScriptedPass::<true>::new(match_nothing, true);
        let r = sweep(&pass, &mut fg).unwrap();
        assert!(!r);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_rewrites_and_reports_changed() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass::<false>::new(match_add, true);
        let r = sweep(&pass, &mut fg).unwrap();
        assert!(r);
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
        // Without propagation each Add is visited once, at seed time.
        let mut fg = nested_adds();
        let pass = ScriptedPass::<false>::new(match_add, true);
        let _ = sweep(&pass, &mut fg).unwrap();
        let log = pass.visit_log.borrow().clone();
        assert_eq!(log.len(), 2, "exactly two visits, no re-enqueue: {log:?}");
    }

    #[test]
    fn run_peephole_with_propagate_true_reenqueues_consumers() {
        let mut fg = nested_adds();
        let pass = ScriptedPass::<true>::new(match_add, true);
        let r = sweep(&pass, &mut fg).unwrap();
        assert!(r);
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
        // The seeded `Add` is rewritten into a fresh `Add`, visited in the
        // same sweep.
        use cranelift_entity::EntityRef;
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            create_matching_once: RefCell::new(true),
            ..ScriptedPass::<true>::new(match_add, true)
        };
        let mut edit = crate::EditFunction::new(&mut fg);
        let octx = crate::pipeline::OptCtx::new(None);
        // The next id to be handed out is the new `Add`'s.
        let new_node_idx = edit.graph_ref().next_node_id().index() as u32;
        let r = run_peephole(&pass, &mut edit, &octx).unwrap();
        assert!(r);
        let log = pass.visit_log.borrow().clone();
        assert!(
            log.contains(&new_node_idx),
            "freshly-created matching node {new_node_idx} must be re-visited, log={log:?}",
        );
    }

    /// A seed an earlier visit of the same sweep killed is never handed to
    /// `try_rewrite`.
    #[test]
    fn run_peephole_skips_a_seed_killed_earlier_in_the_sweep() {
        use cranelift_entity::EntityRef;
        let mut fg = nested_adds();
        let outer = fg.producer(crate::test_support::return_value(fg.graph()).unwrap());
        // Seeds run operands first, so the inner Add's visit kills the outer.
        let pass = ScriptedPass {
            kill_first: RefCell::new(Some(outer)),
            ..ScriptedPass::<false>::new(match_add, false)
        };
        sweep(&pass, &mut fg).unwrap();
        let log = pass.visit_log.borrow().clone();
        assert_eq!(log.len(), 1, "only the inner Add is visited: {log:?}");
        assert!(
            !log.contains(&(outer.index() as u32)),
            "the killed Add was visited: {log:?}"
        );
    }

    #[test]
    fn run_peephole_propagates_pass_internal_error() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            return_error: true,
            ..ScriptedPass::<false>::new(match_add, false)
        };
        let r = sweep(&pass, &mut fg);
        assert!(r.is_err(), "errored pass must surface error");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.contains("scripted-pass forced error"),
            "error must propagate, got {msg}",
        );
    }

    /// A rule that mints a fresh matching node on every fire errors rather
    /// than spinning.
    #[test]
    fn run_peephole_bails_out_instead_of_spinning_forever() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            create_matching_once: RefCell::new(true),
            cascade_forever: true,
            ..ScriptedPass::<true>::new(match_add, true)
        };
        let err = sweep(&pass, &mut fg).expect_err("a non-confluent pass must error, not hang");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not converge"), "unexpected error: {msg}");
        assert!(
            msg.contains("dequeues"),
            "the bound must name its unit: {msg}"
        );
    }
}
