use anyhow::{anyhow, bail, Result};

use super::super::PerRegionDriver;

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
/// `caller_region` appears in the empty-targets error for diagnostics
/// only.
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
    caller_region: strider_lift::cfg::RegionId,
) -> Result<()> {
    let n = targets_and_regions.len();
    if n == 0 {
        bail!("switch terminator at region {caller_region:?} has no targets");
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
    let idx_ty = builder.body().graph.output_kind(idx).as_value_or_err()?;
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

impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R> {
    pub(super) fn handle_branch(
        &mut self,
        region_id: strider_lift::cfg::RegionId,
        region_lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
    ) -> Result<()> {
        // Most unconditional p-code `Branch` ops correspond to a `Branch`
        // CFG edge, which we lower into an explicit IR branch.  The cfg
        // builder reclassifies a `Branch` whose target is the next
        // machine instruction (clang -O0 idiom on aarch64be / ppc32le)
        // as a `Fallthrough` edge.  In that case the IR-level
        // fallthrough linker in `pipeline.rs` wires the edge using
        // `cur_ctrl` / `cur_memory`; we skip the explicit IR branch
        // here to avoid double-linking the successor.
        if let Some(branch_region) = self.cfg.region_branch(region_id)? {
            let dest_block = region_lookup(branch_region)?;
            self.builder.build_branch(dest_block)?;
            return Ok(());
        }
        if self.cfg.region_fallthrough(region_id)?.is_some() {
            // Fallthrough successor — leave to the post-loop linker.
            return Ok(());
        }
        Err(anyhow!("invalid region index {region_id:?}"))
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_lift::cfg::RegionTerminator::Switch`] into an If-ladder of
    /// `IntCmpOp::Equal + If` nodes against each target.
    ///
    /// `region_id` is the dispatch region (the one terminated by
    /// the Switch).  For each target machine address in `targets`,
    /// the helper looks up the corresponding CFG region via
    /// [`strider_lift::cfg::Cfg::region_id_at_start`] and resolves it to an IR
    /// region through `region_lookup`.
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
        region_id: strider_lift::cfg::RegionId,
        target_vn: &rsleigh::Vn,
        targets: &[u64],
        target_value: Option<strider_ir::Value>,
        region_lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
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
            let machine_addr = strider_lift::cfg::MachineInsnAddr::new(target);
            let cfg_region = self
                .cfg
                .region_id_at_start(machine_addr)
                .ok_or_else(|| anyhow!("switch target machine address {target:#x} has no CFG region"))?;
            let ir_region = region_lookup(cfg_region)?;
            targets_and_regions.push((target, ir_region));
        }
        // Prefer the orchestrator's pinned NodeOutputId when available
        // — the dispatch must compare the SAME value the IR-level indirect-branch resolver classified.
        // Falls back to a fresh `read_vn` when the cfg builder didn't
        // populate `target_value`.
        let idx = match target_value {
            Some(v) => v,
            None => self.read_vn(target_vn)?,
        };
        build_switch_if_ladder(&mut self.builder, idx, &targets_and_regions, region_id)
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: strider_lift::cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
    ) -> Result<()> {
        let cond_raw = self.read_vn(&insn.inputs[1])?;
        // Most archs feed `If` a Bool-typed flag-register or compare result,
        // but a few lift conditional branches off an integer varnode (e.g.
        // ARM's status flags are written as integers when the analyzer's
        // write-side coercion stores them as the variable's declared U8).
        // `build_if` requires Bool, so coerce here at the read site.
        let cond = self.builder.convert_to_bool_if_needed(cond_raw)?;
        let res = self.cfg.region_if(region_id)?;
        let if_true_region = res
            .if_true_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let if_false_region = res
            .if_false_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let true_block = region_lookup(if_true_region)?;
        let false_block = region_lookup(if_false_region)?;
        self.builder.build_if(cond, true_block, false_block)?;
        Ok(())
    }

    /// Lowers a p-code `Return` into the IR's calling-convention-aware return
    /// node, emitting the convention's `ret_val_regs` in ABI order.
    ///
    /// The p-code `Return` op carries a single fabricated input (typically the
    /// popped return address on stack-push ISAs).  That value is *not* an ABI
    /// return slot, so we discard the lifted input here and let the IR resolve
    /// the real return values from the calling convention's resolved register
    /// list.
    pub(super) fn handle_return(&mut self, _insn: &rsleigh::Insn) -> Result<()> {
        let ret_regs = self.builder.ret_val_vars().to_vec();
        self.builder.build_return(None, &ret_regs)?;
        Ok(())
    }

    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Direct call: the target varnode is in the code space and its offset
        // *is* the target address — it's not a pointer to dereference.
        let target_vn = &insn.inputs[0];
        let space = target_vn.addr_space;
        let space_info = self
            .cfg
            .sleigh()
            .space_info(space)
            .ok_or_else(|| anyhow::anyhow!("no space info for call target space {space:?}"))?;
        let target_addr = target_vn.addr_off;
        let call_address = self
            .builder
            .build_int_const(target_addr, space_info.addr_size().try_into()?)?;
        // Per-address CC override: when the call target matches a
        // user-supplied entry, build the Call with that CC instead of
        // the function-default.  Convert the rich
        // `target::BuiltCallingConvention` to the thin
        // `strider_ir::FunctionBuilderCC` slice the builder consumes (see V6
        // back-edge fix, Phase 1 Task 1.3c).
        let override_cc = self.per_address_ccs.get(&target_addr).map(strider_ir::FunctionBuilderCC::from);
        self.builder
            .build_call_with_cc(call_address, override_cc.as_ref())
            .map(|_| ())?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_lift::cfg::RegionTerminator::TailCall`] as
    /// `Call(IntConst(target)) + Return`.  The per-region loop
    /// SKIPS the trailing `Opcode::Branch` insn (see
    /// `pipeline.rs::SpecialTerm::skips_opcode`); this method is the
    /// post-loop handler that emits the `Call + Return` pair.
    ///
    /// This is the lowering the
    /// [`strider_lift::cfg::RegionTerminator::TailCall`] doc-comment promises: a
    /// direct branch out of the function range is semantically a
    /// tail call, so the IR carries the explicit Call (with the
    /// resolved constant target) and a Return that hands the
    /// caller's frame back.
    pub(crate) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
        let default_code_space = self.cfg.sleigh().default_code_space();
        let space_info = self
            .cfg
            .sleigh()
            .space_info(default_code_space)
            .ok_or_else(|| {
                anyhow::anyhow!("no space info for default code space {default_code_space:?}")
            })?;
        let call_address = self
            .builder
            .build_int_const(target, space_info.addr_size().try_into()?)?;
        // Per-address CC override applies to lift-time tail calls too.
        // Convert from the rich `BuiltCallingConvention` to the thin
        // `FunctionBuilderCC` slice (V6 fix, see `handle_call`).
        let override_cc = self.per_address_ccs.get(&target).map(strider_ir::FunctionBuilderCC::from);
        self.builder
            .build_call_with_cc(call_address, override_cc.as_ref())
            .map(|_| ())?;
        let ret_regs = self.builder.ret_val_vars().to_vec();
        self.builder.build_return(None, &ret_regs)?;
        Ok(())
    }

    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Indirect call: target is a register/memory value holding the address.
        let call_address = self.read_vn(&insn.inputs[0])?;
        self.builder.build_call(call_address)?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch`] by emitting
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
    /// The `(addr, target_value)` pair is recorded on
    /// `PerRegionDriver::unresolved_branches` so the resolver can correlate
    /// each placeholder with the offending pcode address.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: strider_lift::cfg::PcodeInsnAddr,
    ) -> Result<()> {
        // Read target_vn through pcode-lift's register-aliasing path
        // so sub-register dispatches (e.g. `jmp *eax` on x86-64) fold
        // correctly via the same Piece/Insert chain the rest of the
        // lifter uses.
        let target_value = self.read_vn(target_vn)?;
        // IndirectBranch placeholder — the resolver reads target_value
        // at slot 2 of this node and inspects its producer.
        self.builder.build_indirect_branch(target_value)?;
        self.unresolved_branches.push((addr, target_value));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `build_switch_if_ladder`.  Cover the IR
    //! construction primitive in isolation (no Cfg required); the
    //! integration coverage that drives the full
    //! `handle_switch` → `analyze_cfg` path lives in
    //! `crates/strider/tests/jump_table_lifting.rs`.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
    use strider_ir::FunctionBuilder;

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
    ) -> (FunctionBuilder, strider_ir::Value, Vec<strider_ir::RegionId>) {
        let idx = idx_vn();
        let mut b = FunctionBuilder::new_raw(vec![idx], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let dispatch = b.create_region().expect("dispatch region");
        b.set_entry_region(dispatch).expect("set_entry_region");
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    fn dummy_caller_region() -> strider_lift::cfg::RegionId {
        strider_lift::cfg::RegionId::new(0)
    }

    /// Count `If` nodes via the post-build preorder walk.
    fn count_if_nodes(g: &strider_ir::BuiltFunctionGraph) -> usize {
        g.preorder()
            .filter(|nid| matches!(g.graph.node_kind(*nid), NodeKind::If))
            .count()
    }

    /// Count `IntCmpOp(Equal)` nodes via the post-build preorder walk.
    fn count_eq_cmps(g: &strider_ir::BuiltFunctionGraph) -> usize {
        g.preorder()
            .filter(|nid| {
                matches!(
                    g.graph.node_kind(*nid),
                    NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal),
                )
            })
            .count()
    }

    /// Count `IntConst` nodes whose value equals `want`.
    fn count_int_consts_eq(g: &strider_ir::BuiltFunctionGraph, want: u64) -> usize {
        g.preorder()
            .filter(|nid| {
                matches!(
                    g.graph.node_kind(*nid),
                    NodeKind::IntConst(c) if *c == u128::from(want),
                )
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
        build_switch_if_ladder(&mut b, idx, &[(0xdeadu64, regions[0])], dummy_caller_region())
            .expect("build_switch_if_ladder(1)");
        let g = b.build().expect("build");
        assert_eq!(count_if_nodes(&g), 0, "no If nodes for 1-target switch");
        assert_eq!(count_eq_cmps(&g), 0, "no equality cmps for 1-target");
        assert_eq!(
            count_int_consts_eq(&g, 0xdead),
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
        let g = b.build().expect("build");
        assert_eq!(count_if_nodes(&g), 1, "exactly one If for 2-target switch");
        assert_eq!(
            count_eq_cmps(&g),
            1,
            "exactly one equality cmp for 2-target switch",
        );
        // K_0 (0x100) is compared; K_{N-1} (0x200) is NOT compared
        // because the last If's false-branch flows unconditionally
        // to its region.
        assert!(
            count_int_consts_eq(&g, 0x100) >= 1,
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
        let g = b.build().expect("build");
        assert_eq!(count_if_nodes(&g), 2, "N-1=2 If nodes for 3-target switch");
        assert_eq!(count_eq_cmps(&g), 2, "N-1=2 equality cmps for 3-target switch");
        assert!(
            count_int_consts_eq(&g, 0x1000) >= 1,
            "K_0 (0x1000) IntConst present",
        );
        assert!(
            count_int_consts_eq(&g, 0x2000) >= 1,
            "K_1 (0x2000) IntConst present",
        );
        assert_eq!(
            count_int_consts_eq(&g, 0x3000),
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
        let g = b.build().expect("build");
        assert_eq!(count_if_nodes(&g), 3, "N-1=3 If nodes for 4-target switch");
        // Validation already happened inside build(); reaching this
        // line means the per-region control-chain is consistent.
        let _ = g;
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
