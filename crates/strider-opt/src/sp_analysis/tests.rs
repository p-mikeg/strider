#[cfg(test)]
mod decompose_tests {
    use crate::sp_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Collapses the single-predecessor `read_variable(sp)` phi so an SP
    /// address becomes a bare `InitialVar(sp) + k` terminal — the shape
    /// `decompose` sees in production (it no longer looks through phis;
    /// the pipeline's `PhiCollapse` has run by then).  ConstantFold is
    /// intentionally NOT run here: these tests build the canonical
    /// `Add(_, IntConst(-K))` offset shape directly (via [`sub_off`]) and the
    /// deep-chain / memo tests need the un-collapsed structure.
    fn collapse_phis(fg: &mut strider_ir::Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(fg, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// Builds the canonical `Add(x, IntConst(-k))` — the post-`ConstantFold`
    /// shape of `x - k`.  The lifter emits `Add(x, Neg(IntConst(k)))`, but
    /// `ConstantFold` folds the `Neg` to a single negative `IntConst` before
    /// any SP-aware pass runs, so the decomposer only ever meets this shape
    /// (it does not peel `Neg` itself).
    fn sub_off(
        b: &mut strider_ir::FunctionBuilder,
        x: ValueId,
        k: i64,
        ty: ValueType,
    ) -> crate::Result<ValueId> {
        let neg_k = b.build_int_const((-k) as u64, ty)?;
        b.build_int_binary_operation(x, neg_k, IntBinaryOp::Add, ty)
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // `read_variable(sp)` wraps `InitialVar(sp)` in a single-predecessor
        // phi; PhiCollapse collapses it, so the live SP value (the Return's
        // value input) is the bare `InitialVar(sp)` that decomposes to
        // offset 0.  (Decomposing the now-detached phi output would be None.)
        collapse_phis(&mut fg);
        let live_sp = crate::test_support::return_value(fg.graph())?;
        let r = decompose(&fg, live_sp);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        let _ = sp_val;
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let addr = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = decompose(&fg, addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
        let addr =
            b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = decompose(&fg, addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_is_idempotent_and_committed_slot_round_trips() -> crate::Result<()> {
        // Read-only decompose is idempotent, and committing a slot (as
        // `StackOffsetDetect` does) round-trips through the cache.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let addr = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r1 = decompose(&fg, addr);
        let r2 = decompose(&fg, addr);
        assert!(matches!(
            (&r1, &r2),
            (
                Some(SpExpr { offset: -4, .. }),
                Some(SpExpr { offset: -4, .. })
            )
        ));
        if let Some(SpExpr { base, offset }) = r1 {
            fg.side_tables_mut().set_stack_slot(addr, base, offset);
        }
        assert_eq!(
            fg.side_tables().stack_slot_resolved(addr).map(|(_, o)| o),
            Some(-4),
            "committed slot must round-trip"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(decompose(&fg, c).is_none());
        Ok(())
    }

    #[test]
    fn decompose_walks_deep_offset_chain() -> crate::Result<()> {
        // The spine walk accumulates the constant offset down a deep
        // `sp - 4 - 8 - 12` chain, and each intermediate decomposes to its own
        // partial offset.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let s1 = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        let s2 = sub_off(&mut b, s1, 8, ValueType::I32)?;
        let s3 = sub_off(&mut b, s2, 12, ValueType::I32)?;
        b.build_return(Some(s3), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let off = |v| decompose(&fg, v).map(|e| e.offset);
        assert_eq!(off(s1), Some(-4), "s1 = sp - 4");
        assert_eq!(off(s2), Some(-12), "s2 = sp - 12");
        assert_eq!(off(s3), Some(-24), "s3 = sp - 24");
        Ok(())
    }

    /// Determinism guarantee under a cycle: a loop-carried SP expression
    /// `Phi(InitialVar(sp), Add(phi, -K))` contains a data cycle through the
    /// `Phi`.  Every node in the cone (the `Phi`, the loop-carried `Add`, and a
    /// genuinely-non-SP address) must classify identically regardless of which
    /// value is queried first or whether a memo is shared across queries — the
    /// path-independence that makes caching `None` sound.
    #[test]
    fn decompose_sp_cycle_classifies_identically_regardless_of_query_order() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let loop_hdr = b.create_region_all()?;
        let exit = b.create_region_all()?;
        b.set_entry_region_all(entry)?;

        // entry: sp is the incoming value; branch into the loop header.
        b.set_region(entry);
        b.build_branch(loop_hdr)?;

        // loop header: read sp (a Phi joining the entry value and the
        // loop-carried decremented value), then sp = sp - 4, and conditionally
        // branch back to the header — a data cycle through the sp Phi.
        b.set_region(loop_hdr);
        let sp_phi = b.read_variable(&sp)?;
        let sp_dec = sub_off(&mut b, sp_phi, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_dec)?;
        let keep_looping = b.build_boolean_const(true);
        b.build_if(keep_looping, loop_hdr, exit)?;

        // exit: a genuinely non-SP address in the same function cone.
        b.set_region(exit);
        let global = b.build_int_const(0x4000u64, ValueType::I32)?;
        b.build_return(Some(global), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        // NB: do NOT collapse phis here — we want the genuine loop-header Phi
        // (multi-predecessor) intact so the cone really contains a cycle.

        // Compute each verdict in isolation (fresh memo per query) — the
        // ground-truth graph verdict for each value.
        let truth = |v: ValueId| -> Option<SpExpr> { decompose(&fg, v) };
        let t_phi = truth(sp_phi);
        let t_dec = truth(sp_dec);
        let t_global = truth(global);

        // The cycle nodes are not provable SP terminals (the decomposer does
        // not look through phis), and the global is genuinely non-SP.
        assert!(t_phi.is_none(), "loop-header Phi(sp) is not an SP terminal");
        assert!(
            t_dec.is_none(),
            "loop-carried Add over a Phi is not provable"
        );
        assert!(t_global.is_none(), "global address is not SP-rooted");

        // Now query in every order through ONE shared memo and assert each
        // value's verdict matches its isolated ground truth — verdicts are
        // path-independent, so a cached `None` (or `Some`) is always correct.
        for order in [
            [sp_phi, sp_dec, global],
            [global, sp_dec, sp_phi],
            [sp_dec, global, sp_phi],
        ] {
            for v in order {
                let got = decompose(&fg, v);
                let want = if v == sp_phi {
                    t_phi
                } else if v == sp_dec {
                    t_dec
                } else {
                    t_global
                };
                assert_eq!(
                    got.map(|e| e.offset),
                    want.map(|e| e.offset),
                    "verdict for {v:?} must be query-order-independent"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn decompose_sp_phi_with_non_sp_pred_returns_none() -> crate::Result<()> {
        // A VarPhi(sp) whose predecessor value is NOT SP-rooted must
        // decompose to None.  Previously decompose_sp_phi fabricated a
        // Terminal{base: phi_output, offset: 0} on this path; callers
        // ignored `base` but trusted `offset == 0`, which on conventions
        // where stack_arg_offsets[0] == 0 (AArch64/ARM AAPCS) could
        // misclassify a non-SP-rooted phi as the first stack argument or
        // wrongly forward a load over it.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let bb = b.create_region_all()?;
        let c = b.create_region_all()?;
        b.set_entry_region_all(entry)?;

        // entry: if cond goto a else goto bb
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: sp = sp - 4 (SP-rooted)
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let sp_minus_4 = sub_off(&mut b, sp_a, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        // c: read sp.  The phi at c has two predecessor values: the SP-rooted
        // one from `a` and the bogus const from `bb`.  decompose must
        // refuse to claim "this is sp + K" for that phi.
        b.set_region(c);
        let sp_at_c = b.read_variable(&sp)?;
        b.build_return(Some(sp_at_c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let r = decompose(&fg, sp_at_c);
        assert!(
            r.is_none(),
            "expected None for VarPhi(sp) with a non-SP-rooted predecessor, got {r:?}"
        );
        Ok(())
    }

    /// FreeBSD i386 10.0 prologue: `and $0xfffffff8, %esp` aligns the
    /// stack to 8 bytes after the saved-register pushes.  All subsequent
    /// stack arithmetic is anchored at the And's output, not at
    /// `InitialVar(sp)`, so `decompose` must recognise the And and
    /// treat its output as a stable opaque base (offset 0) — otherwise
    /// every store after the alignment dance is a non-decomposable
    /// `Store(_)`, and `CallStackArgCollect` walks past the call's args
    /// as "non-aliasing".
    #[test]
    fn decompose_sp_and_with_alignment_mask_yields_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Simulate `and $0xfffffff8, %esp`.
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        b.build_return(Some(aligned), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = decompose(&fg, aligned);
        // The aligned output is a stable opaque base.  Offset = 0
        // because the alignment can shift the value by 0..7 bytes — we
        // can't pin a constant delta, but we *can* pin a stable
        // `ValueId` that subsequent decompositions reference.
        let Some(SpExpr { base, offset }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
        // Base must NOT be the InitialVar(sp) output — it's the And output.
        let base_node = fg.producer(base);
        assert!(
            matches!(
                *fg.node_kind(base_node),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            ),
            "And-aligned base must point to the And node, got {:?}",
            fg.node_kind(base_node)
        );
        Ok(())
    }

    /// Following the alignment dance, the function does
    /// `sub $0x1d0, %esp` (the local-frame reservation).  The post-Sub
    /// SP must decompose to the *same* opaque base (the And output),
    /// just with a non-zero offset.  Without this, every cdecl call
    /// site after the alignment dance has args at addresses that
    /// `decompose` cannot relate to each other, breaking
    /// `CallStackArgCollect`.
    #[test]
    fn decompose_sp_sub_after_and_chains_offset_through_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        let post_sub = sub_off(&mut b, aligned, 0x1D0, ValueType::I32)?;
        b.build_return(Some(post_sub), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let aligned_dec = decompose(&fg, aligned).expect("aligned must decompose");
        let post_sub_dec = decompose(&fg, post_sub).expect("post_sub must decompose");
        let SpExpr {
            base: aligned_base,
            offset: aligned_off,
        } = aligned_dec;
        let SpExpr {
            base: post_sub_base,
            offset: post_sub_off,
        } = post_sub_dec;
        assert_eq!(
            aligned_base, post_sub_base,
            "post-Sub base must equal post-And base (opaque base shared)"
        );
        assert_eq!(aligned_off, 0);
        assert_eq!(post_sub_off, -0x1D0, "Sub by 0x1D0 shifts offset by -0x1D0");
        Ok(())
    }

    /// Deep nested-`And` shape: the iterative `rpo` sweep re-bases at
    /// each level and resolves to an opaque base without recursion, so a
    /// pathologically deep chain terminates cleanly (no stack overflow,
    /// no recursion-depth budget) with an opaque `Terminal` base.
    #[test]
    fn decompose_sp_deep_and_chain_terminates_without_overflow() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        const N: usize = 6000;
        for _ in 0..N {
            current =
                b.build_int_binary_operation(current, mask, IntBinaryOp::And, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        // Iterative rpo sweep: the deep And chain re-bases at each level and
        // resolves to an opaque base without recursion, so no stack overflow.
        let r = decompose(&fg, current);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        Ok(())
    }

    /// Regression: `decompose`
    /// must not blow the thread stack on a deep `sp + K1 + K2 + ... + KN`
    /// chain.  The recursive form overflowed at ~4-8k nodes; the
    /// iterative form must walk a 5000-node chain without panic AND
    /// produce the correct cumulative offset.
    #[test]
    fn decompose_sp_does_not_stack_overflow_on_deep_chain() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        const N: usize = 5000;
        for _ in 0..N {
            let one = b.build_int_const(1u64, ValueType::I32)?;
            current =
                b.build_int_binary_operation(current, one, IntBinaryOp::Add, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let SpExpr { offset, .. } = decompose(&fg, current)
            .expect("5000-node chain must decompose without stack-overflowing");
        assert_eq!(
            offset, N as i128,
            "cumulative offset must equal N adds of +1"
        );
        Ok(())
    }
}

#[cfg(test)]
mod alias_tests {
    use crate::sp_analysis::store_value_byte_size;
    use crate::sp_analysis::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// The `InitialVar(sp)` output — the canonical entry-SP terminal base
    /// that `decompose` returns for any clean `sp + k` address.
    fn entry_sp_value(f: &Function, sp: rsleigh::Vn) -> ValueId {
        let node = f
            .graph()
            .all_node_ids()
            .find(
                |&n| matches!(*f.node_kind(n), NodeKind::InitialVar(id) if f.initial_vn(id) == sp),
            )
            .expect("InitialVar(sp) exists");
        f.node_outputs_exact::<1>(node)
            .expect("InitialVar has 1 output")[0]
    }

    fn only_store(f: &Function) -> NodeId {
        f.graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("one store")
    }

    /// Test composition of the store→load alias verdict: classify the store
    /// address, derive its size, then
    /// run the pure class-on-class [`alias_verdict`] — exactly what the
    /// production `SpMemWalker` Store arm does now that the bespoke
    /// `store_alias_verdict` method was dissolved into `classify_store_addr`.
    fn store_alias_verdict(
        f: &Function,
        store: NodeId,
        load_class: AddrClass,
        load_size: i128,
        mode: AliasMode,
        distinct_sp_bases_disjoint: bool,
    ) -> AliasVerdict {
        let store_size = store_value_byte_size(f, f.store_data(store));
        let store_class = classify_store_addr(f, store);
        let options = SpOptions::new(
            mode,
            MemAliasOptions {
                calls_clobber: false,
                assume_distinct_sp_bases_disjoint: distinct_sp_bases_disjoint,
            },
        );
        alias_verdict(
            SizedAddr {
                class: load_class,
                size: load_size,
            },
            SizedAddr {
                class: store_class,
                size: store_size,
            },
            options,
        )
    }

    /// Collapse the single-predecessor `read_variable(sp)` phi so SP
    /// addresses are bare `InitialVar(sp) + k` terminals — the shape these
    /// alias helpers see in production (the decomposer no longer looks
    /// through phis).
    fn collapse(f: &mut Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(f, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// Regression for the two-terminal base bug: a `Store` whose address is
    /// an *alignment-masked* SP base (`(sp & mask) + 8`) must NOT be proven
    /// disjoint from a query slot rooted at the *entry* SP just because
    /// their offsets don't overlap.  The two bases differ by the runtime
    /// alignment delta `sp mod align`, so the offset comparison is
    /// meaningless and the verdict must be may-alias (not `Disjoint`).
    #[test]
    fn different_base_terminal_store_may_alias() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            // aligned = sp & 0xFFFF_FFF8  (a distinct SP base)
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            // store at aligned + 8
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(
            verdict,
            AliasVerdict::MayAlias,
            "store at an alignment-masked base must may-alias an entry-SP query \
             (different bases are not offset-comparable)"
        );
    }

    /// With the `distinct_sp_bases_disjoint` opt-in (used by stack-arg
    /// detection), the SAME different-base store is instead treated as
    /// `Disjoint`: incoming-arg slots above the entry SP are assumed not to
    /// overlap frame locals rooted at an alignment-masked SP.
    #[test]
    fn different_base_terminal_store_disjoint_when_opted_in() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            true,
        );
        assert_eq!(
            verdict,
            AliasVerdict::Disjoint,
            "with distinct_sp_bases_disjoint, a different-base store is assumed disjoint"
        );
    }

    /// Sanity: same base, non-overlapping offsets are provably disjoint.
    #[test]
    fn same_base_disjoint_offsets_is_disjoint() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        // store at sp+8 (size 4) vs query at sp+0 (size 4): disjoint.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Disjoint);
    }

    /// Sanity: same base, same offset is an exact `Match`.
    #[test]
    fn same_base_same_offset_is_match() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        // store at sp+8 (size 4) vs query at sp+8 (size 4): exact match.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 8,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Match);
    }
}

#[cfg(test)]
mod ranges_tests {
    use crate::sp_analysis::ranges_disjoint;

    #[test]
    fn ranges_disjoint_returns_true_for_non_overlapping() {
        // Adjacent ranges are disjoint (touching is fine).
        assert!(ranges_disjoint(0, 4, 4, 4));
        // Overlapping ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 2, 4));
        // Identical ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 0, 4));
        // Reverse order — equally disjoint.
        assert!(ranges_disjoint(4, 4, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_left_does_not_panic_and_is_conservative() {
        // The three memory-chain walkers (CallStackArgCollect, load_forward::probe,
        // function_args::mem_chain_is_dirty) pass `i128::MAX` as a
        // soundness-pessimistic fallback when a Store's `value_byte_size` is
        // unknown. With plain `+`, `a_off + i128::MAX` would panic in debug and
        // wrap in release for any positive `a_off`. ranges_disjoint must saturate
        // cleanly and report "not disjoint" (false) for any reachable load offset
        // — the conservative verdict callers depend on. SP-relative offsets in
        // practice are small (kB range), so we cover zero, modestly-negative, and
        // modestly-positive a_off values.
        assert!(!ranges_disjoint(0, i128::MAX, 100, 4));
        assert!(!ranges_disjoint(-1000, i128::MAX, 100, 4));
        assert!(!ranges_disjoint(1_000_000, i128::MAX, -1_000_000, 4));
        // Even very large positive a_off (where `a_off + i128::MAX` would
        // overflow without saturation) must not panic and must report
        // "not disjoint".
        assert!(!ranges_disjoint(1, i128::MAX, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_right_does_not_panic_and_is_conservative() {
        // Symmetric: i128::MAX on the b-side must also saturate and report
        // "not disjoint" without panicking.
        assert!(!ranges_disjoint(100, 4, 0, i128::MAX));
        assert!(!ranges_disjoint(100, 4, -1000, i128::MAX));
        assert!(!ranges_disjoint(-1_000_000, 4, 1_000_000, i128::MAX));
        assert!(!ranges_disjoint(0, 4, 1, i128::MAX));
    }
}

#[cfg(test)]
mod cfg_tests {
    use crate::sp_analysis::*;
    use crate::{OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// Regression for the `verdict` / `def_clobbers` SSoT divergence: after a
    /// rewrite leaves a store's raw address non-decomposable, `stack_offsets`
    /// still records it as `[sp+K]`.  The memory-SSA walk stops at the store
    /// (its `def_clobbers` uses `classify_store_addr`, the SSoT), so the exact
    /// re-check in `verdict` must classify the store the SAME way — otherwise
    /// it falls back to `Anchor`, reports `MayAlias`, and `LoadForward`
    /// silently misses a legal forward.
    #[test]
    fn verdict_uses_stack_offset_ssot_for_nondecomposable_store() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let k = b.build_int_const(8u64, ValueType::I32)?;
            // Opaque store address: `xor(sp, 8)` is not a recognised SP base,
            // so `classify_addr` on it yields `Anchor` — standing in for an
            // address a later rewrite folded into a non-decomposable shape.
            let opaque =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Xor, ValueType::I32)?;
            let data = b.build_int_const(0x11u64, ValueType::I32)?;
            b.build_store(opaque, data, rsleigh::VnSpace::RAM)?;
            // Load at the real `sp + 8` (decomposable -> SpRooted(8)).
            let load_addr =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::I32)?;
            let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .expect("build sp fn");

        // Collapse the entry `read_variable(sp)` phi so SP addresses are bare
        // `InitialVar(sp) + k` terminals the decomposer recognises.
        let mut p = OptimizerPipeline::new();
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut f, &mut OptCtx::new(None)).expect("collapse");

        let store = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("store node");
        let load = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Load(_)))
            .expect("load node");
        let entry_sp = f.initial_var_value(&sp).expect("entry sp value");

        // Simulate `StackOffsetDetect`: the SSoT records the store's address
        // value as `[sp+8]` even though its raw address folded to an opaque
        // shape (the derived per-node `stack_offset(store)` reads it back).
        let store_addr = f.store_addr(store);
        f.side_tables_mut().set_stack_slot(store_addr, entry_sp, 8);

        let cfg = SpAnalyzer::new(SpOptions::call_blocking(AliasMode::StackGlobalDisjoint));
        assert_eq!(
            cfg.verdict(&f, load, store),
            AliasVerdict::Match,
            "verdict must classify the store via the stack_offsets SSoT (like def_clobbers), \
             so a non-decomposable store still verifies as an exact match"
        );
    }
}
