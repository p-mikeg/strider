//! Opt-domain composite rewrites layered on top of `RewriteCtx`'s generic
//! primitives. These live in `opt` (not `strider-pattern`) because they encode
//! optimization-layer rewrite operations; `RewriteCtx` itself stays generic.
//!
//! Each composite is implemented purely in terms of `RewriteCtx`'s PUBLIC API
//! (the generic mutation primitives plus the two safe access primitives
//! `replace_all_uses` / `absorb_fingerprint`), so the opt layer never reaches
//! into `RewriteCtx`'s private `&mut Function`.

use strider_ir::node::{NodeId, UseId, ValueId};
use strider_pattern::RewriteCtx;

use crate::opt::error::Result;

/// Opt-domain composite rewrites on [`RewriteCtx`].
///
/// These compose the generic primitives `RewriteCtx` exposes into the
/// higher-level operations the optimizer needs (value replacement with
/// fingerprint absorption, single-input redirection, region-predecessor
/// removal).
pub(crate) trait OptRewrite {
    /// The single value-replacement primitive: redirect every use of `old`
    /// to `new`, after **absorbing** `old`'s producer asm-fingerprint into
    /// `new`'s producer (superset-only union).
    ///
    /// This is the one place that pairs fingerprint absorption with
    /// use-replacement — optimization passes call this instead of hand-writing
    /// the absorb + redirect pair, so the superset-only fingerprint contract has
    /// one implementation for value rewrites.
    ///
    /// Returns `true` iff at least one use was redirected.
    ///
    /// # Errors
    /// Propagates `RewriteCtx::replace_all_uses`'s error arm unchanged.
    fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool>;

    /// Redirect a single input slot from its current producer to `new`,
    /// absorbing the displaced producer's asm-fingerprint into `new`'s
    /// producer **iff** the redirect leaves the displaced producer with
    /// no remaining uses.
    ///
    /// The companion to [`Self::replace_value`] for the single-slot case:
    /// where `replace_value` redirects *every* use of a value,
    /// `redirect_input` rewires exactly one input edge. When the displaced
    /// producer becomes dead as a result, its contributing-asm history would
    /// otherwise be lost, so it is folded into the surviving consumer's new
    /// producer (superset-only union). When the displaced producer keeps other
    /// live uses, no absorption happens — those uses still explain its value via
    /// its own fingerprint, and contaminating `new`'s producer would violate the
    /// "fingerprint names the asm insns that contribute to this value" contract.
    fn redirect_input(&mut self, input_id: UseId, new: ValueId);

    /// Removes a batch of predecessor slots from a `Region` and the matching
    /// value slots from every `Phi`/`MemPhi` that consumes the Region's
    /// phi-token output — the single structural primitive for dropping dead
    /// control edges into a join.
    ///
    /// A `Region` produces `[control, phi_token]`; a `Phi`/`MemPhi` over it has
    /// inputs `[phi_token, val_pred0, val_pred1, …]`, so the value for Region
    /// predecessor `i` lives at phi input `i + 1`. Region/Phi nodes are exempt
    /// from the asm-fingerprint non-empty check, so no fingerprint work is needed.
    ///
    /// The caller passes ALL dead predecessor indices for the region at once;
    /// this method removes them highest-index-first internally so earlier
    /// removals never invalidate a later (lower) index — the caller does not
    /// need to pre-sort or remove one-by-one. Duplicate indices are deduped,
    /// and out-of-range indices are skipped per-node via bounds checks.
    ///
    /// # Errors
    /// Propagates `RewriteCtx::remove_node_input`'s error arm.
    fn remove_region_predecessors(&mut self, region: NodeId, pred_indices: &[u32]) -> Result<()>;
}

impl<'g> OptRewrite for RewriteCtx<'g> {
    fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool> {
        self.absorb_fingerprint(new, old);
        self.replace_all_uses(old, new)
    }

    fn redirect_input(&mut self, input_id: UseId, new: ValueId) {
        let old_out = self.graph_ref().input_output_id(input_id);
        let displaced_uses_before = self.graph_ref().value_uses(old_out).count();
        self.update_input(input_id, new);
        if displaced_uses_before == 1 {
            // `old_out` is the displaced producer's output; absorb its
            // fingerprint into `new`'s producer (superset-only union).
            self.absorb_fingerprint(new, old_out);
        }
    }

    fn remove_region_predecessors(&mut self, region: NodeId, pred_indices: &[u32]) -> Result<()> {
        debug_assert!(
            matches!(self.node_kind(region), strider_ir::node::NodeKind::Region),
            "remove_region_predecessors: node is not a Region",
        );
        if pred_indices.is_empty() {
            return Ok(());
        }
        // Highest-index-first, deduped: removing a higher slot never shifts a
        // lower one, so every remaining index stays valid across the batch.
        let mut indices: Vec<u32> = pred_indices.to_vec();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();

        // Collect the phi-token consumers once (the set of Phi/MemPhi nodes
        // doesn't change as we remove their value inputs).
        let phi_nodes: Vec<NodeId> = {
            let outputs = self.node_outputs(region);
            if outputs.len() >= 2 {
                let phi_out = outputs[1]; // ValueId: Copy
                self.graph_ref().value_uses(phi_out).map(|(n, _)| n).collect()
            } else {
                Vec::new()
            }
        };

        for pred_index in indices {
            let phi_input_idx = pred_index + 1;
            for &phi in &phi_nodes {
                if phi_input_idx < self.node_inputs(phi).len() as u32 {
                    self.remove_node_input(phi, phi_input_idx)?;
                }
            }
            if pred_index < self.node_inputs(region).len() as u32 {
                self.remove_node_input(region, pred_index)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]
mod tests {
    //! Verification for the opt-domain composite mutations
    //! ([`OptRewrite::replace_value`] and
    //! [`OptRewrite::remove_region_predecessors`]). These exercise the
    //! composites at their home on the `OptRewrite` extension trait; both
    //! build a *built* `Function` (entry set) so
    //! `RewriteCtx::try_for_built` succeeds.

    use super::OptRewrite;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{FunctionBuilder, IntBinaryOp};
    use strider_ir_test_utils::{reg_vn, RegisterSet};
    use strider_pattern::RewriteCtx;

    // ── replace_value ────────────────────────────────────────────────

    /// `replace_value` absorbs the old producer's asm-fingerprint into the
    /// new producer (superset union) and redirects every use of `old` to
    /// `new`.
    #[test]
    fn replace_value_absorbs_fingerprint_and_redirects_uses() {
        let mut b: FunctionBuilder = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");

        // old: IntConst(10) stamped with fingerprint 0xAA.
        b.set_lift_addr(Some(0xAA));
        let old_out = b.build_int_const(10u64, ValueType::I64).unwrap();
        // new: IntConst(20) stamped with fingerprint 0xBB.
        b.set_lift_addr(Some(0xBB));
        let new_out = b.build_int_const(20u64, ValueType::I64).unwrap();
        // sink: Add(old, old) — two uses of old_out.
        let sink = b
            .build_int_binary_operation(old_out, old_out, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sink), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_out);
        let sink_node = function.producer(sink);

        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        let changed = ctx.replace_value(old_out, new_out).unwrap();
        assert!(changed, "a live use existed → changed");

        // new_node absorbs old_node's fingerprint (superset) while keeping
        // its own.
        let fp = function.asm_fingerprint(new_node);
        assert!(fp.contains(&0xAA), "absorbed old's fingerprint 0xAA: {fp:?}");
        assert!(fp.contains(&0xBB), "kept new's own fingerprint 0xBB: {fp:?}");

        // sink now refers to new_out for all inputs.
        let sink_inputs: Vec<_> = function.node_inputs(sink_node).into_iter().collect();
        assert_eq!(
            sink_inputs,
            vec![new_out, new_out],
            "sink inputs must now point at new_out"
        );

        // old_out has no remaining uses.
        assert_eq!(
            function.graph().value_uses(old_out).count(),
            0,
            "old_out must have no remaining uses"
        );
    }

    /// With no uses to redirect, `replace_value` returns `false` but STILL
    /// absorbs the old producer's fingerprint into the new one.
    #[test]
    fn replace_value_no_uses_returns_false() {
        let mut b: FunctionBuilder = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");

        // old has fingerprint 0xAA but is wired to nothing.
        b.set_lift_addr(Some(0xAA));
        let old_out = b.build_int_const(1u64, ValueType::I64).unwrap();
        // new (the Return value) has fingerprint 0xBB.
        b.set_lift_addr(Some(0xBB));
        let new_out = b.build_int_const(2u64, ValueType::I64).unwrap();
        // Only `new_out` is used (by the Return); `old_out` is unused.
        b.build_return(Some(new_out), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_out);

        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        let changed = ctx.replace_value(old_out, new_out).unwrap();
        assert!(!changed, "no uses of old → changed must be false");

        // Fingerprint is still absorbed even with no uses redirected.
        let fp = function.asm_fingerprint(new_node);
        assert!(
            fp.contains(&0xAA),
            "fingerprint absorbed even when no uses redirected: {fp:?}"
        );
        assert!(fp.contains(&0xBB), "kept new's own fingerprint 0xBB: {fp:?}");
    }

    // ── remove_region_predecessors ────────────────────────────────────

    /// A 2-predecessor `Region` with a value `Phi` over it: removing
    /// predecessor 0 strips the first control slot from the Region AND the
    /// matching value slot (phi index 1) from the Phi, leaving 1 control
    /// input on the Region and `[token, surviving_value]` on the Phi.
    #[test]
    fn remove_region_predecessors_strips_ctrl_and_phi_slot() {
        // Build `if (true) { var = 1 } else { var = 2 }; return var;` so the
        // `join` Region has two control predecessors and a 2-value VarPhi.
        let var = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(var).arg(var).build_fn().unwrap();
        let entry = b.create_region().unwrap();
        let true_r = b.create_region().unwrap();
        let false_r = b.create_region().unwrap();
        let join = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, true_r, false_r).unwrap();

        b.set_region(true_r);
        let v_t = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.write_variable(&var, v_t).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(false_r);
        let v_f = b.build_int_const(2u64, ValueType::I64).unwrap();
        b.write_variable(&var, v_f).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(join);
        let merged = b.read_variable(&var).unwrap();
        b.build_return(Some(merged), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        // Locate the 2-value VarPhi at the join (inputs `[token, val0, val1]`)
        // and the Region it joins.  Filtering on input count = 3 skips any
        // single-predecessor VarPhi the builder may have produced for an
        // intermediate region.
        let phi = function
            .graph().all_node_ids()
            .find(|&n| {
                matches!(function.node_kind(n), NodeKind::Phi)
                    && function.phi_var_tag(n) == Some(var)
                    && function.node_inputs(n).len() == 3
            })
            .expect("2-value VarPhi at the join must exist");
        let phi_token = function.node_inputs(phi)[0];
        let region = function.producer(phi_token);
        assert!(
            matches!(function.node_kind(region), NodeKind::Region),
            "phi token producer must be the join Region"
        );

        // Sanity: two control predecessors, phi inputs [token, val0, val1].
        assert_eq!(
            function.node_inputs(region).len(),
            2,
            "join region starts with 2 control predecessors"
        );
        let pre_phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(pre_phi_inputs.len(), 3, "phi: [token, val0, val1]");
        // Capture pred-1's value (phi index 2) before removal.
        let pred1_val = pre_phi_inputs[2];

        // Act: remove predecessor 0 via the RewriteCtx.
        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        ctx.remove_region_predecessors(region, &[0])
            .expect("remove_region_predecessors must succeed");

        // Region drops to 1 control input.
        assert_eq!(
            function.node_inputs(region).len(),
            1,
            "region drops to 1 ctrl input"
        );

        // Phi must have exactly 2 inputs: [token, surviving value].
        let phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(phi_inputs.len(), 2, "phi: [token, surviving value]");
        assert_eq!(phi_inputs[1], pred1_val, "surviving slot is pred 1's value");
    }
}
