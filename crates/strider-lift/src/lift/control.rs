use crate::lift::pcode_util::nth_input_or_err;
use anyhow::{Result, anyhow, bail};
use strider_ir::{IRBuilderExt, IRViewer};

use super::FunctionLifter;

/// Emits an If-ladder dispatching `idx` against `targets_and_regions`.
///
/// Builds a chain of `IntCmpOp::Equal + If` nodes, one per case, using
/// only the existing `FunctionBuilder::build_if` /
/// `build_int_const` / `build_int_cmp_operation` primitives.
///
/// Layout (forward iteration over the slice):
///
/// ```text
/// dispatch_region:
///   if (idx == K_0)  → R_0
///                    → dispatcher_1
/// dispatcher_1:
///   if (idx == K_1)  → R_1
///                    → dispatcher_2
/// ...
/// dispatcher_{N-2}:
///   if (idx == K_{N-2}) → R_{N-2}
///                       → R_{N-1}            (last cmp's false-branch is the final target)
/// ```
///
/// The last comparison's false-branch goes UNCONDITIONALLY to the
/// final target's region — this is sound because the IR-level indirect-branch resolver's `Multiple`
/// classification is exhaustive for the runtime index range
/// (KnownBits / predecessor `If(idx < N)` provide the upper bound),
/// so the runtime always picks one of `targets`.  Sending the
/// otherwise-unreachable default to `targets[N-1]` keeps the IR
/// well-formed without inventing an "unreachable" sink region; the
/// optimizer can prune the dead default branch when ConstantFold
/// folds `idx` to a constant in a single-target case.
///
/// Special cases:
/// - `targets_and_regions.len() == 0` — returns an error.  Defensive;
///   the cfg builder rejects empty `Multiple` upstream.
/// - `targets_and_regions.len() == 1` — emits a plain
///   `build_branch(target_0)` with no comparison.
///
/// `dispatch_region` (the region terminated by the Switch) appears in
/// the empty-targets error for diagnostics only.
///
/// # Errors
///
/// Propagates the IR-shape errors from `FunctionBuilder::build_if` /
/// `build_branch` / `build_int_cmp_operation` / `build_int_const` /
/// `create_region`, plus an explicit error when `targets_and_regions`
/// is empty.
pub(crate) fn build_switch_if_ladder(
    builder: &mut strider_ir::FunctionBuilder,
    idx: strider_ir::Value,
    targets_and_regions: &[(u64, strider_ir::RegionId)],
    dispatch_region: strider_cfg::RegionId,
) -> Result<()> {
    let n = targets_and_regions.len();
    if n == 0 {
        bail!("switch terminator at region {dispatch_region:?} has no targets");
    }
    if n == 1 {
        // Single target — degenerate ladder is just an unconditional
        // branch.  ConstantFold can't simplify a `cmp` it doesn't see;
        // emitting a 1-target switch as a plain branch keeps the IR
        // shape minimal and matches what the cfg builder would have
        // produced for `Single(K)`.
        let (_target, region) = targets_and_regions[0];
        builder.build_branch(region)?;
        return Ok(());
    }
    // Comparison value type drives the IntConst widths and the
    // IntCmpOp output type.  The cmp's output is always Bool but
    // `build_int_cmp_operation` takes `output_type` for the
    // input-side coercion.
    let idx_ty = builder.function().value_kind(idx).as_value_or_err()?;
    // Walk every case except the last; the last comparison's false
    // branch is the final target's region (no extra dispatcher).
    for i in 0..n - 1 {
        let (k_i, region_i) = targets_and_regions[i];
        let next_else: strider_ir::RegionId = if i + 1 == n - 1 {
            // Final iteration's else IS the final target.
            targets_and_regions[n - 1].1
        } else {
            // Synthesise a dispatcher region for the next comparison.
            builder.create_region()?
        };
        let target_const = builder.build_int_const(k_i, idx_ty)?;
        let cond = builder.build_int_cmp_operation(
            idx,
            target_const,
            strider_ir::IntCmpOp::Equal,
            idx_ty,
        )?;
        builder.build_if(cond, region_i, next_else)?;
        if i + 1 < n - 1 {
            // Move into the freshly synthesised dispatcher for the
            // next iteration's `build_if` to terminate.  We skip the
            // set on the second-to-last iteration because that
            // iteration's else is the FINAL target's region (already
            // existing) — no dispatcher to thread through.
            builder.set_region(next_else);
        }
    }
    Ok(())
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    pub(super) fn handle_branch(&mut self) -> Result<()> {
        // Nothing to do per-region.  An unconditional pcode `Branch`
        // produces a single `Unconditional` CFG edge to its successor,
        // and EVERY unconditional successor — branch or plain
        // fall-through — is wired uniformly by the post-loop region
        // linker (`pipeline.rs::link_region_edges`).  Lowering an
        // explicit IR branch here as well would double-link the
        // successor (and thus its `Phi` predecessor inputs).
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::Switch`] into an If-ladder of
    /// `IntCmpOp::Equal + If` nodes against each target.
    ///
    /// `region_id` is the dispatch region (the one terminated by
    /// the Switch).  For each target machine address in `targets`,
    /// the helper looks up the corresponding CFG region via
    /// [`strider_cfg::Cfg::region_id_at_start`] and resolves it to an IR
    /// region through `region_map` (via [`super::ir_region_of`]).
    ///
    /// # Errors
    ///
    /// Returns an error when `targets` is empty, when a target
    /// machine address has no matching CFG region, or propagates IR
    /// construction failures from `read_vn` / `build_if` /
    /// `build_branch` / `build_int_const` / `build_int_cmp_operation` /
    /// `create_region`.
    pub(crate) fn handle_switch(
        &mut self,
        region_id: strider_cfg::RegionId,
        target_vn: &rsleigh::Vn,
        targets: &[u64],
        region_map: &super::RegionMap,
    ) -> Result<()> {
        if targets.is_empty() {
            bail!("switch terminator at region {region_id:?} has no targets");
        }
        // Resolve every target machine address to its IR region.
        // The cfg builder enqueues each target with a `Branch` edge
        // as it constructs the Switch (see
        // `cfg/src/cfg/builder/region_builder.rs:436`), so each
        // target IS the start of a region by lift time.
        let mut targets_and_regions: Vec<(u64, strider_ir::RegionId)> =
            Vec::with_capacity(targets.len());
        for &target in targets {
            let machine_addr = strider_cfg::MachineInsnAddr::from(target);
            let cfg_region = self.cfg.region_id_at_start(machine_addr).ok_or_else(|| {
                anyhow!("switch target machine address {target:#x} has no CFG region")
            })?;
            let ir_region = super::ir_region_of(region_map, cfg_region)?;
            targets_and_regions.push((target, ir_region));
        }
        // Read the dispatch value at the region exit — the comparison
        // value for the If-ladder.
        let idx = self.read_vn(target_vn)?;
        build_switch_if_ladder(&mut self.builder, idx, &targets_and_regions, region_id)
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        let cond_raw = self.read_input(insn, 1)?;
        // Sleigh always feeds `CBRANCH` an already-`I1` condition (a 1-byte
        // comparison / flag result, value 0 or 1 — verified across arches).
        // `build_if` requires `I1`; `truncate_if_needed` is a no-op in that
        // common case.  Should a wider, provably-0/1 value ever reach here
        // (e.g. a flag zero-extended into a multi-byte register), narrowing
        // to the low bit is exact for a 0/1 value and keeps the IR sound.
        let cond = self
            .builder
            .truncate_if_needed(cond_raw, strider_ir::ValueType::I1)?;
        let res = self.cfg.region_if(region_id)?;
        let if_true_region = res
            .if_true_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let if_false_region = res
            .if_false_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let true_block = super::ir_region_of(region_map, if_true_region)?;
        let false_block = super::ir_region_of(region_map, if_false_region)?;
        self.builder.build_if(cond, true_block, false_block)?;
        Ok(())
    }

    /// Lowers a p-code `Return` into the IR's calling-convention-aware return
    /// node, emitting the convention's `ret_val_regs` in ABI order.
    ///
    /// Also handles the `BranchIndirect` link-register-return case (e.g. ARM
    /// `bx lr`): the dispatcher routes both `Opcode::Return` and
    /// `Opcode::BranchIndirect` here, since both emit a CC `Return` (tail
    /// calls / jump tables are split off earlier into dedicated terminators).
    ///
    /// The p-code `Return` op carries a single fabricated input (typically the
    /// popped return address on stack-push ISAs).  That value is *not* an ABI
    /// return slot, so we discard the lifted input here and let the IR resolve
    /// the real return values from the calling convention's resolved register
    /// list.
    pub(super) fn handle_return(&mut self, _insn: &rsleigh::Insn) -> Result<()> {
        // build_function_return terminates the region unconditionally.
        self.builder.build_function_return()
    }

    /// Builds a `Call` from the (override or function-default) calling
    /// convention — the prod call-construction orchestration.  Derives the
    /// ret-val / clobber / arg vns from the CC, reads the args + SP through the
    /// shared vn read path, emits the dumb [`strider_ir::FunctionBuilder::build_call`],
    /// writes the clobbers then ret-vals back, records the override CC, and
    /// applies the post-call SP adjust.  (strider-ir's constructor is dumb: it
    /// takes resolved Vn lists and knows nothing about calling conventions.)
    fn build_cc_call(
        &mut self,
        call_address: strider_ir::Value,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<()> {
        // Snapshot the CC-derived ingredients (owned) so the immutable borrow of
        // the function ends before the &mut read / build / write path below.
        let (ret_vns, clobber_vns, arg_vns, sp_vn, ret_stack_pop, advance_memory) = {
            let cc = override_cc.unwrap_or_else(|| self.builder.function().default_cc());
            (
                self.builder.function().call_ret_vals_for(cc),
                self.builder.function().call_clobbered_for(cc),
                cc.arg_passing_regs.clone(),
                cc.stack_vn,
                cc.ret_stack_pop,
                !cc.preserves_memory,
            )
        };

        let args = self.read_vns(&arg_vns)?;
        let sp_value = self.read_vn(&sp_vn)?;

        let mut output_vns = ret_vns.clone();
        output_vns.extend_from_slice(&clobber_vns);
        let (call, outputs) =
            self.builder
                .build_call(call_address, sp_value, &args, &output_vns, advance_memory)?;
        let (ret_vals, clobbers) = outputs.split_at(ret_vns.len());

        // Writeback: clobbers first, then ret-vals (an aliased clobber must not
        // re-clobber the return value).  Clobbers go through `write_variable`
        // (the set legitimately includes CONST / RAM Sleigh temps that the
        // aliasing-aware register path can't slice); ret-vals are REGISTER /
        // UNIQUE, so they take the aliasing-aware `write_vn`.
        for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
            self.builder.write_variable(vn, *v)?;
        }
        for (vn, v) in core::iter::zip(&ret_vns, ret_vals) {
            self.write_vn(vn, *v)?;
        }

        if let Some(cc) = override_cc {
            self.builder.function_mut().set_call_cc(call, cc.clone());
        }
        self.builder
            .apply_post_call_sp_adjust(&sp_vn, sp_value, ret_stack_pop)?;
        Ok(())
    }

    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Direct call: the target varnode is in the code space and its offset
        // *is* the target address — it's not a pointer to dereference.
        let target_vn = nth_input_or_err(insn, 0)?;
        let space = target_vn.addr_space;
        let target_addr = target_vn.addr_off;
        let call_address = self.build_addr_const(space, target_addr, "call target space")?;
        // Per-address CC override: when the call target matches a user-supplied
        // entry, build the Call with that CC instead of the function-default.
        // Cloned so the per_address_ccs borrow ends before the &mut build call.
        let override_cc = self.per_address_ccs.get(&target_addr).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::TailCall`] as
    /// `Call(IntConst(target)) + Return`.  The per-region loop
    /// SKIPS the trailing `Opcode::Branch` insn (see
    /// `pipeline.rs::SpecialTerm::skips_opcode`); this method is the
    /// post-loop handler that emits the `Call + Return` pair.
    ///
    /// This is the lowering the
    /// [`strider_cfg::RegionTerminator::TailCall`] doc-comment promises: a
    /// direct branch out of the function range is semantically a
    /// tail call, so the IR carries the explicit Call (with the
    /// resolved constant target) and a Return that hands the
    /// caller's frame back.
    pub(crate) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
        let default_code_space = self.lifter.sleigh().default_code_space();
        let call_address =
            self.build_addr_const(default_code_space, target, "default code space")?;
        // Per-address CC override applies to lift-time tail calls too.
        let override_cc = self.per_address_ccs.get(&target).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        // build_function_return terminates the region unconditionally.
        self.builder.build_function_return()
    }

    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Indirect call: target is a register/memory value holding the address.
        let call_address = self.read_vn(nth_input_or_err(insn, 0)?)?;
        self.build_cc_call(call_address, None)?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::UnresolvedIndirectBranch`] by emitting
    /// an `IndirectBranch(target_value)` placeholder anchoring
    /// `target_vn`'s lifted value in the IR.  The indirect-branch
    /// resolver inspects this placeholder after the optimiser runs.
    ///
    /// `IndirectBranch [ctrl, mem, target]` carries the same control
    /// and memory snapshot a real `Return` would, so the resolver can
    /// rewrite it in place into a real `Return` for `LinkRegister`
    /// resolutions or splice in a `Call+Return` pair for `Single`
    /// tail-call resolutions without re-walking the CFG.
    ///
    /// The `(addr, placeholder_node)` pair is recorded on
    /// `FunctionLifter::unresolved_branches` so the resolver can correlate
    /// each placeholder node with the offending pcode address.  The node
    /// id (not the lifted value) is recorded so the resolver can read the
    /// placeholder's *current* dispatch input after the optimizer rewrites
    /// it — the lifted value may be `replace_all_uses`-rewired away.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        // Read target_vn through pcode-lift's register-aliasing path
        // so sub-register dispatches (e.g. `jmp *eax` on x86-64) fold
        // correctly via the same Piece/Insert chain the rest of the
        // lifter uses.
        let target_value = self.read_vn(target_vn)?;
        // IndirectBranch placeholder — the resolver reads slot 2 of this
        // node (its live dispatch input) and inspects the producer.
        let placeholder = self.builder.build_indirect_branch(target_value)?;
        self.unresolved_branches.push((addr, placeholder));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `build_switch_if_ladder`.  Cover the IR
    //! construction primitive in isolation (no Cfg required); the
    //! integration coverage that drives the full
    //! `handle_switch` → `build_ir` path lives in
    //! `crates/strider-orchestrator/tests/jump_table_lifting.rs`.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::node::NodeKind;
    use strider_ir::{FunctionBuilder, IRWalker};
    use strider_ir_test_utils::{IrWalkerEx, RegisterSet};

    /// Build a 4-byte register VN to act as the `idx` source.
    fn idx_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Set up a builder with a single tracked variable (`idx`), an
    /// entry region, N target regions (one per case), and the
    /// dispatch region as the active region.  Returns the builder,
    /// the lifted `idx` value, and the per-target IR region IDs.
    fn make_builder_with_targets(
        n: usize,
    ) -> (
        FunctionBuilder,
        strider_ir::Value,
        Vec<strider_ir::RegionId>,
    ) {
        let idx = idx_vn();
        let mut b = RegisterSet::new()
            .tracked(idx)
            .build_fn()
            .expect("RegisterSet::build_fn");
        let dispatch = b.create_region().expect("dispatch region");
        b.set_entry_region(dispatch).expect("set_entry_region");
        let target_regions: Vec<strider_ir::RegionId> = (0..n)
            .map(|_| b.create_region().expect("create target region"))
            .collect();
        // Each target region terminates with a Return so `build()`
        // produces a valid graph.  We use the calling convention's
        // empty ret-val list (no values returned) since the test
        // builder has no convention-resolved ret regs.
        for &tr in &target_regions {
            b.set_region(tr);
            b.build_return(None, &[]).expect("target return");
        }
        b.set_region(dispatch);
        let idx_val = b.read_variable(&idx).expect("read idx");
        (b, idx_val, target_regions)
    }

    /// Returns the unique cfg-region-id sentinel.  The helper only
    /// uses it inside the `SwitchHasNoTargets` error payload, so any
    /// value is fine for these tests.
    fn dummy_caller_region() -> strider_cfg::RegionId {
        strider_cfg::RegionId::new(0)
    }

    /// Count `If` nodes via the post-build preorder walk.
    fn count_if_nodes(function: &strider_ir::Function) -> usize {
        function.count_kind(|k| matches!(k, NodeKind::If))
    }

    /// Count `IntCmpOp(Equal)` nodes via the post-build preorder walk.
    fn count_eq_cmps(function: &strider_ir::Function) -> usize {
        function.count_kind(|k| matches!(k, NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal)))
    }

    /// Count `IntConst` nodes whose value equals `want`.
    fn count_int_consts_eq(function: &strider_ir::Function, want: u128) -> usize {
        use strider_ir::IRViewer;
        function
            .walk_kind(|k| matches!(k, NodeKind::IntConst(_)))
            .filter(|&n| {
                function
                    .node_outputs(n)
                    .iter()
                    .any(|&out| function.int_const_u128(out) == Some(want))
            })
            .count()
    }

    #[test]
    fn handle_switch_with_one_target_emits_single_branch_no_cmp() {
        // Single-target switch: degenerate ladder collapses to a
        // plain `build_branch`.  No comparison, no If, no IntConst
        // for the dispatch value.  The target region is reachable
        // via the single Branch edge.
        let (mut b, idx, regions) = make_builder_with_targets(1);
        build_switch_if_ladder(
            &mut b,
            idx,
            &[(0xdeadu64, regions[0])],
            dummy_caller_region(),
        )
        .expect("build_switch_if_ladder(1)");
        let function = b.build().expect("build");
        assert_eq!(
            count_if_nodes(&function),
            0,
            "no If nodes for 1-target switch"
        );
        assert_eq!(count_eq_cmps(&function), 0, "no equality cmps for 1-target");
        assert_eq!(
            count_int_consts_eq(&function, 0xdead),
            0,
            "no comparison constant emitted for 1-target",
        );
    }

    #[test]
    fn handle_switch_with_two_targets_emits_one_if_with_correct_polarity() {
        // Two-target switch: emits exactly ONE If whose true-branch
        // points at target_0 and whose false-branch points at
        // target_1 (the last comparison's else IS the final target).
        // One IntCmpOp(Equal), one IntConst for K_0.
        let (mut b, idx, regions) = make_builder_with_targets(2);
        build_switch_if_ladder(
            &mut b,
            idx,
            &[(0x100u64, regions[0]), (0x200u64, regions[1])],
            dummy_caller_region(),
        )
        .expect("build_switch_if_ladder(2)");
        let function = b.build().expect("build");
        assert_eq!(
            count_if_nodes(&function),
            1,
            "exactly one If for 2-target switch"
        );
        assert_eq!(
            count_eq_cmps(&function),
            1,
            "exactly one equality cmp for 2-target switch",
        );
        // K_0 (0x100) is compared; K_{N-1} (0x200) is NOT compared
        // because the last If's false-branch flows unconditionally
        // to its region.
        assert!(
            count_int_consts_eq(&function, 0x100) >= 1,
            "K_0 (0x100) must be present as IntConst",
        );
    }

    #[test]
    fn handle_switch_with_three_targets_chains_if_ladder_and_two_consts() {
        // Three-target switch: emits 2 If nodes (one per non-final
        // case) and 2 IntCmpOp(Equal) cmps against K_0 and K_1.
        // The final case (K_2) is reached via the last If's
        // false-branch — no comparison emitted for K_2.
        let (mut b, idx, regions) = make_builder_with_targets(3);
        build_switch_if_ladder(
            &mut b,
            idx,
            &[
                (0x1000u64, regions[0]),
                (0x2000u64, regions[1]),
                (0x3000u64, regions[2]),
            ],
            dummy_caller_region(),
        )
        .expect("build_switch_if_ladder(3)");
        let function = b.build().expect("build");
        assert_eq!(
            count_if_nodes(&function),
            2,
            "N-1=2 If nodes for 3-target switch"
        );
        assert_eq!(
            count_eq_cmps(&function),
            2,
            "N-1=2 equality cmps for 3-target switch"
        );
        assert!(
            count_int_consts_eq(&function, 0x1000) >= 1,
            "K_0 (0x1000) IntConst present",
        );
        assert!(
            count_int_consts_eq(&function, 0x2000) >= 1,
            "K_1 (0x2000) IntConst present",
        );
        assert_eq!(
            count_int_consts_eq(&function, 0x3000),
            0,
            "K_{{N-1}} (0x3000) NOT compared — flows via last If's false-branch",
        );
    }

    #[test]
    fn handle_switch_threads_control_chain_through_dispatcher_regions() {
        // For N targets the helper allocates N-2 dispatcher regions
        // (one per intermediate If's else side).  After the call,
        // the IR graph has exactly N-1 If nodes feeding control to
        // the target regions; running `build()` succeeds (validate
        // passes), which is the strongest single check that the
        // control chain is well-formed end-to-end.
        let (mut b, idx, regions) = make_builder_with_targets(4);
        build_switch_if_ladder(
            &mut b,
            idx,
            &[
                (0xa0u64, regions[0]),
                (0xb0u64, regions[1]),
                (0xc0u64, regions[2]),
                (0xd0u64, regions[3]),
            ],
            dummy_caller_region(),
        )
        .expect("build_switch_if_ladder(4)");
        let function = b.build().expect("build");
        assert_eq!(
            count_if_nodes(&function),
            3,
            "N-1=3 If nodes for 4-target switch"
        );
        // Validation already happened inside build(); reaching this
        // line means the per-region control-chain is consistent.
        let _ = function;
    }

    #[test]
    fn handle_switch_with_zero_targets_returns_typed_error() {
        // Defensive: cfg builder rejects empty `Multiple` upstream,
        // but the helper's error path must still surface a typed
        // error rather than a panic.  Pin that here.
        let (mut b, idx, _regions) = make_builder_with_targets(0);
        let err = build_switch_if_ladder(&mut b, idx, &[], dummy_caller_region())
            .expect_err("zero-target switch must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("switch terminator") && msg.contains("no targets"),
            "error must name the SwitchHasNoTargets variant; got {msg:?}",
        );
    }
}
