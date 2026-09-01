#[cfg(test)]
mod decompose_tests {
    use crate::mem_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

    use super::super::test_sp as sp;

    /// Collapses phis to the bare `InitialVar(sp) + k` terminal `decompose`
    /// recognises.  ConstantFold is left out so the deep-chain and memo tests
    /// keep their un-flattened structure.
    fn collapse_phis(fg: &mut strider_ir::Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(fg, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// The post-`ConstantFold` shape of `x - k`, i.e. `Add(x, IntConst(-k))`.
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
        collapse_phis(&mut fg);
        let live_sp = crate::test_support::return_value(fg.graph())?;
        let r = decompose(&fg, live_sp);
        assert!(matches!(r, Some(MemExpr { offset: 0, .. })));
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
        assert!(matches!(r, Some(MemExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
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
        assert!(matches!(r, Some(MemExpr { offset: -4, .. })));
        Ok(())
    }

    /// Machine address arithmetic is mod 2^bitwidth.  On a 32-bit target
    /// `(sp + 0x7FFFFFFF) + 0x7FFFFFFF` is `sp - 2`; accumulating in i128
    /// without reducing makes it `sp + 4294967294`, wrongly Disjoint from the
    /// slot it actually names.
    #[test]
    fn decompose_wraps_the_offset_to_the_address_width() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let big = b.build_int_const(0x7FFF_FFFFu64, ValueType::I32)?;
        let once = b.build_int_binary_operation(sp_val, big, IntBinaryOp::Add, ValueType::I32)?;
        let twice = b.build_int_binary_operation(once, big, IntBinaryOp::Add, ValueType::I32)?;
        b.build_return(Some(twice), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert_eq!(
            decompose(&fg, twice).map(|e| e.offset),
            Some(-2),
            "two 0x7FFFFFFF bumps of a 32-bit pointer land at sp - 2"
        );
        Ok(())
    }

    #[test]
    fn decompose_is_idempotent_and_committed_slot_round_trips() -> crate::Result<()> {
        // Committing a slot is what `StackOffsetDetect` does.
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
                Some(MemExpr { offset: -4, .. }),
                Some(MemExpr { offset: -4, .. })
            )
        ));
        if let Some(MemExpr { base, offset, .. }) = r1 {
            fg.side_tables_mut().set_stack_slot(addr, base, offset);
        }
        assert_eq!(
            fg.side_tables().memory_slot_resolved(addr).map(|(_, o)| o),
            Some(-4),
            "committed slot must round-trip"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
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

    /// A loop-carried `Phi(InitialVar(sp), Add(phi, -K))` puts a data cycle
    /// in the cone.  Every node in it must classify the same whatever the
    /// query order or memo sharing.
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

        b.set_region(entry);
        b.build_branch(loop_hdr)?;

        // Reading sp here yields a Phi joining the entry value and the
        // loop-carried decrement, so branching back closes a data cycle.
        b.set_region(loop_hdr);
        let sp_phi = b.read_variable(&sp)?;
        let sp_dec = sub_off(&mut b, sp_phi, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_dec)?;
        let keep_looping = b.build_boolean_const(true);
        b.build_if(keep_looping, loop_hdr, exit)?;

        // A genuinely non-SP address in the same cone.
        b.set_region(exit);
        let global = b.build_int_const(0x4000u64, ValueType::I32)?;
        b.build_return(Some(global), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        // Do NOT collapse phis: the multi-predecessor loop-header Phi has to
        // survive or the cone contains no cycle.

        let truth = |v: ValueId| -> Option<MemExpr> { decompose(&fg, v) };
        let t_phi = truth(sp_phi);
        let t_dec = truth(sp_dec);
        let t_global = truth(global);

        assert!(t_phi.is_none(), "loop-header Phi(sp) is not an SP terminal");
        assert!(
            t_dec.is_none(),
            "loop-carried Add over a Phi is not provable"
        );
        assert!(t_global.is_none(), "global address is not SP-rooted");

        // Every order through ONE shared memo must reproduce the ground truth.
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
        // Fabricating `offset == 0` here would be dangerous: on conventions
        // where `base_offset == 0` (AArch64/ARM AAPCS) callers would
        // read a non-SP-rooted phi as the first stack argument, or forward a
        // load over it.
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

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let sp_minus_4 = sub_off(&mut b, sp_a, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: a literal pretending to be a new SP, not SP-rooted.
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

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

    /// FreeBSD i386 10.0 prologue: `and $0xfffffff8, %esp` aligns the stack
    /// after the saved-register pushes, so all later stack arithmetic must
    /// anchor at the And's output rather than `InitialVar(sp)`.
    #[test]
    fn decompose_sp_and_with_alignment_mask_yields_opaque_base() -> crate::Result<()> {
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
        b.build_return(Some(aligned), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = decompose(&fg, aligned);
        // Offset 0 because alignment can shift the value by 0..7 bytes: no
        // constant delta to pin, only a stable `ValueId` later decompositions
        // can reference.
        let Some(MemExpr { base, offset, .. }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
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

    /// The local-frame reservation `sub $0x1d0, %esp` after the alignment
    /// must decompose to the same opaque base with a non-zero offset.
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
        let MemExpr {
            base: aligned_base,
            offset: aligned_off,
            ..
        } = aligned_dec;
        let MemExpr {
            base: post_sub_base,
            offset: post_sub_off,
            ..
        } = post_sub_dec;
        assert_eq!(
            aligned_base, post_sub_base,
            "post-Sub base must equal post-And base (opaque base shared)"
        );
        assert_eq!(aligned_off, 0);
        assert_eq!(post_sub_off, -0x1D0, "Sub by 0x1D0 shifts offset by -0x1D0");
        Ok(())
    }

    /// A pathologically deep nested-`And` chain must terminate without
    /// overflowing the thread stack.
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
        let r = decompose(&fg, current);
        assert!(matches!(r, Some(MemExpr { offset: 0, .. })));
        Ok(())
    }

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
        let MemExpr { offset, .. } = decompose(&fg, current)
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
    use crate::mem_analysis::store_value_byte_size;
    use crate::mem_analysis::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// The canonical entry-SP base `decompose` returns for a clean `sp + k`.
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

    fn store_alias_verdict(
        f: &Function,
        store: NodeId,
        load_class: AddrClass,
        load_size: i128,
        mode: bool,
        distinct_sp_bases_disjoint: bool,
    ) -> AliasVerdict {
        let store_size = store_value_byte_size(f, f.store_data(store));
        let store_class = classify_store_addr(f, store);
        let mut opt_options = crate::OptOptions::default();
        opt_options.assumptions.distinct_sp_bases_disjoint = distinct_sp_bases_disjoint;
        let options = MemOptions::incoming_args(mode, &opt_options);
        // The real width the store's own address carries. Passing `None` would
        // model IR the lifter cannot produce, and the wrap guard would then
        // never be exercised by any of these cases.
        let addr_bits = addr_bit_width(f, f.store_addr(store));
        alias_verdict(
            SizedAddr {
                class: load_class,
                size: load_size,
                addr_bits,
            },
            SizedAddr {
                class: store_class,
                size: store_size,
                addr_bits,
            },
            options,
        )
    }

    /// Leaves SP addresses as the bare `InitialVar(sp) + k` terminals the
    /// decomposer recognises.
    fn collapse(f: &mut Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(f, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// A `Store` at an alignment-masked base `(sp & mask) + 8` must not be
    /// proven disjoint from an entry-SP query just because the offsets do not
    /// overlap.  The bases differ by the runtime `sp mod align`, so comparing
    /// their offsets is meaningless.
    #[test]
    fn different_base_terminal_store_may_alias() {
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
            // Keeps the store and its SP-address phi reachable.
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
            AddrClass::StackRooted {
                base: query_base,
                offset: 0,
            },
            4,
            true,
            false,
        );
        assert_eq!(
            verdict,
            AliasVerdict::MayAlias,
            "store at an alignment-masked base must may-alias an entry-SP query \
             (different bases are not offset-comparable)"
        );
    }

    /// Under `distinct_sp_bases_disjoint` the same store is `Disjoint`:
    /// incoming-arg slots above the entry SP are assumed not to overlap frame
    /// locals at an alignment-masked SP.
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
            AddrClass::StackRooted {
                base: query_base,
                offset: 0,
            },
            4,
            true,
            true,
        );
        assert_eq!(
            verdict,
            AliasVerdict::Disjoint,
            "with distinct_sp_bases_disjoint, a different-base store is assumed disjoint"
        );
    }

    #[test]
    fn same_base_disjoint_offsets_is_disjoint() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Keeps the store and its SP-address phi reachable.
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
            AddrClass::StackRooted {
                base: query_base,
                offset: 0,
            },
            4,
            true,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Disjoint);
    }

    #[test]
    fn same_base_same_offset_is_match() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Keeps the store and its SP-address phi reachable.
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
            AddrClass::StackRooted {
                base: query_base,
                offset: 8,
            },
            4,
            true,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Match);
    }
}

#[cfg(test)]
mod ranges_tests {
    use crate::mem_analysis::ranges_disjoint;

    #[test]
    fn ranges_disjoint_returns_true_for_non_overlapping() {
        // Touching counts as disjoint.
        assert!(ranges_disjoint(0, 4, 4, 4));
        assert!(!ranges_disjoint(0, 4, 2, 4));
        assert!(!ranges_disjoint(0, 4, 0, 4));
        // Argument order does not matter.
        assert!(ranges_disjoint(4, 4, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_does_not_panic_and_is_conservative() {
        // With plain `+`, `off + i128::MAX` panics in debug and wraps in
        // release for any positive `off`.  Real SP-relative offsets are small,
        // so cover zero and modest magnitudes of both signs, on either side.
        for (wide, small) in [(0, 100), (-1000, 100), (1_000_000, -1_000_000), (1, 0)] {
            assert!(!ranges_disjoint(wide, i128::MAX, small, 4), "left {wide}");
            assert!(!ranges_disjoint(small, 4, wide, i128::MAX), "right {wide}");
        }
    }
}

#[cfg(test)]
mod cfg_tests {
    use crate::mem_analysis::*;
    use crate::{OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// After a rewrite leaves a store's raw address non-decomposable,
    /// the side-table still records it as `[sp+K]`, and `verdict` must
    /// classify it from that rather than falling back to `Anchor`.
    #[test]
    fn verdict_uses_stack_offset_ssot_for_nondecomposable_store() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let k = b.build_int_const(8u64, ValueType::I32)?;
            // `xor(sp, 8)` is not a recognised SP base, so it classifies as
            // `Anchor`, standing in for an address a rewrite folded into a
            // non-decomposable shape.
            let opaque =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Xor, ValueType::I32)?;
            let data = b.build_int_const(0x11u64, ValueType::I32)?;
            b.build_store(opaque, data, rsleigh::VnSpace::RAM)?;
            // The real `sp + 8`, decomposing to StackRooted(8).
            let load_addr =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::I32)?;
            let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .expect("build sp fn");

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

        // Stand in for `StackOffsetDetect`: record `[sp+8]` even though the raw
        // address folded to an opaque shape.
        let store_addr = f.store_addr(store);
        f.side_tables_mut().set_stack_slot(store_addr, entry_sp, 8);

        let cfg = MemAnalyzer::new(MemOptions::call_blocking(true));
        assert_eq!(
            cfg.verdict(&f, load, store),
            AliasVerdict::Match,
            "verdict must classify the store via the side-table SSoT (like def_clobbers), \
             so a non-decomposable store still verifies as an exact match"
        );
    }
}

#[cfg(test)]
mod heap_tests {
    use crate::mem_analysis::*;
    use strider_ir::node::{ValueId, ValueType};
    use strider_ir::{Function, FunctionBuilder, IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }
    }
    /// A return register distinct from SP, to receive an allocator's pointer.
    fn ret_reg() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x00,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }
    }
    /// A second call output register, e.g. a caller-saved reg the allocator
    /// clobbers.
    fn clobber_reg() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x08,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }
    }

    const MALLOC: u64 = 0x1000;

    fn builder() -> crate::Result<FunctionBuilder> {
        let sp = sp();
        RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .build_fn_single_region()
    }

    fn built(mut b: FunctionBuilder, allocators: &[u64]) -> crate::Result<Function> {
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.side_tables_mut()
            .set_noalias_allocators(allocators.iter().copied().collect());
        Ok(fg)
    }

    /// Like [`built`], but first runs `PhiCollapse`/`RegionCollapse` so a
    /// `read_variable(sp)` becomes the bare `InitialVar(sp)` that `decompose`
    /// recognises (matching the post-collapse state `LoadForward` runs in).
    fn built_collapsed(mut b: FunctionBuilder, allocators: &[u64]) -> crate::Result<Function> {
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::new(None))?;
        fg.side_tables_mut()
            .set_noalias_allocators(allocators.iter().copied().collect());
        Ok(fg)
    }

    /// A prologue: drops SP by `bytes`, so a slot at `entry_sp - k` for
    /// `k < bytes` sits in THIS frame and above the SP a callee receives.
    fn lower_sp(b: &mut FunctionBuilder, entry_sp: ValueId, bytes: i64) -> crate::Result<()> {
        let frame = b.build_int_const((-bytes) as u64, ValueType::I64)?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::I64)?;
        b.write_variable(&sp(), call_sp)?;
        Ok(())
    }

    fn alloc_call(b: &mut FunctionBuilder, target_addr: u64) -> crate::Result<ValueId> {
        let target = b.build_int_const(target_addr, ValueType::I64)?;
        let (_call, rets) = b.build_call(target, &[], &[ret_reg()], 0)?;
        Ok(rets[0])
    }

    #[test]
    fn decompose_heap_base_from_allocator_call() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let addr = b.build_int_binary_operation(p, eight, IntBinaryOp::Add, ValueType::I64)?;
        b.build_return(Some(addr), &[])?;
        let fg = built(b, &[MALLOC])?;
        let r = decompose(&fg, addr);
        assert!(
            matches!(r, Some(MemExpr { base, offset: 8, .. }) if base == p),
            "malloc()+8 must decompose to a heap base at the call's return value, got {r:?}"
        );
        Ok(())
    }

    /// SOUNDNESS: `(malloc() + 15) & -16`, the manual aligned-allocation idiom,
    /// leaves the masked pointer's offset to its base unknown.  The And/terminal
    /// path would hand back a Stack-kinded anchor, making an aligned heap
    /// pointer Disjoint from its own object, so it must go opaque.
    #[test]
    fn decompose_aligned_heap_pointer_is_opaque() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let fifteen = b.build_int_const(15u64, ValueType::I64)?;
        let bumped = b.build_int_binary_operation(p, fifteen, IntBinaryOp::Add, ValueType::I64)?;
        let mask = b.build_int_const(0xFFFF_FFFF_FFFF_FFF0u64, ValueType::I64)?;
        let aligned =
            b.build_int_binary_operation(bumped, mask, IntBinaryOp::And, ValueType::I64)?;
        b.build_return(Some(aligned), &[])?;
        let fg = built(b, &[MALLOC])?;
        assert!(
            decompose(&fg, aligned).is_none(),
            "an aligned heap pointer must be opaque, not a Stack base Disjoint from its object"
        );
        Ok(())
    }

    /// The same idiom with the raw base ALREADY committed to the memo, so the
    /// masked pointer's spine reaches it through the committed-verdict path
    /// rather than the allocator arm.  It must still go opaque.
    #[test]
    fn decompose_aligned_heap_pointer_is_opaque_when_base_memoized() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let fifteen = b.build_int_const(15u64, ValueType::I64)?;
        let bumped = b.build_int_binary_operation(p, fifteen, IntBinaryOp::Add, ValueType::I64)?;
        let mask = b.build_int_const(0xFFFF_FFFF_FFFF_FFF0u64, ValueType::I64)?;
        let aligned =
            b.build_int_binary_operation(bumped, mask, IntBinaryOp::And, ValueType::I64)?;
        b.build_return(Some(aligned), &[])?;
        let fg = built(b, &[MALLOC])?;
        // Warm the memo: commit the raw base as a heap slot.
        assert!(
            matches!(decompose(&fg, p), Some(MemExpr { base, offset: 0, .. }) if base == p),
            "raw malloc() return must decompose to its own heap base"
        );
        assert!(
            decompose(&fg, aligned).is_none(),
            "aligned heap pointer must be opaque even when its base is memoized as Heap"
        );
        Ok(())
    }

    #[test]
    fn non_allocator_call_return_is_opaque() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, 0x2000)?;
        b.build_return(Some(p), &[])?;
        let fg = built(b, &[MALLOC])?;
        assert!(
            decompose(&fg, p).is_none(),
            "a call to 0x2000 is not in the allocator set, so its return is opaque"
        );
        Ok(())
    }

    /// SOUNDNESS: only the return pointer (`outputs[2]`) is a base. A clobbered
    /// register the allocator also writes holds garbage, not a fresh pointer.
    #[test]
    fn clobbered_output_is_not_a_heap_base() -> crate::Result<()> {
        let mut b = builder()?;
        let target = b.build_int_const(MALLOC, ValueType::I64)?;
        let (_call, rets) = b.build_call(target, &[], &[ret_reg(), clobber_reg()], 0)?;
        let clobber = rets[1];
        b.build_return(Some(clobber), &[])?;
        let fg = built(b, &[MALLOC])?;
        assert!(
            decompose(&fg, clobber).is_none(),
            "a clobbered output of an allocator call is not a heap base"
        );
        Ok(())
    }

    #[test]
    fn indirect_call_target_is_opaque() -> crate::Result<()> {
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?; // a non-constant call target
        let (_call, rets) = b.build_call(sp_val, &[], &[ret_reg()], 0)?;
        let p = rets[0];
        b.build_return(Some(p), &[])?;
        let fg = built(b, &[MALLOC])?;
        assert!(
            decompose(&fg, p).is_none(),
            "a non-constant (indirect) call target can't be matched, so it stays opaque"
        );
        Ok(())
    }

    #[test]
    fn two_allocator_calls_are_distinct_bases() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let q = alloc_call(&mut b, MALLOC)?;
        b.build_return(Some(p), &[])?;
        let fg = built(b, &[MALLOC])?;
        let bp = decompose(&fg, p).expect("p is a heap base").base;
        let bq = decompose(&fg, q).expect("q is a heap base").base;
        assert_ne!(bp, bq, "two malloc calls must yield distinct heap bases");
        Ok(())
    }

    #[test]
    fn nested_add_accumulates_heap_offset() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let four = b.build_int_const(4u64, ValueType::I64)?;
        let inner = b.build_int_binary_operation(p, four, IntBinaryOp::Add, ValueType::I64)?;
        let four2 = b.build_int_const(4u64, ValueType::I64)?;
        let outer = b.build_int_binary_operation(inner, four2, IntBinaryOp::Add, ValueType::I64)?;
        b.build_return(Some(outer), &[])?;
        let fg = built(b, &[MALLOC])?;
        let r = decompose(&fg, outer);
        assert!(
            matches!(r, Some(MemExpr { base, offset: 8, .. }) if base == p),
            "(malloc()+4)+4 must decompose to the heap base at offset 8, got {r:?}"
        );
        Ok(())
    }

    /// Two allocations live at once never overlap.  `stack_global_disjoint` is
    /// off, so the verdict rests on the noalias guarantee alone.  Liveness is
    /// the scope of that guarantee: see
    /// [`a_reused_allocation_is_still_taken_as_disjoint`].
    #[test]
    fn two_heap_objects_are_disjoint() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(p, x, rsleigh::VnSpace::RAM)?;
        let q = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(q, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store node");
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("load node");
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::Disjoint,
            "two distinct heap allocations never overlap"
        );
        Ok(())
    }

    /// Pins a known limitation, not a guarantee.  Nothing models deallocation,
    /// so the second `malloc` is a distinct base even where the program freed
    /// the first and the allocator handed the same storage back.  A load from
    /// the stale pointer is then taken not to see the new object's stores.
    /// Reaching it requires a use-after-free in the analysed program.  Modelling
    /// deallocation would change this verdict to `MayAlias`.
    #[test]
    fn a_reused_allocation_is_still_taken_as_disjoint() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(p, x, rsleigh::VnSpace::RAM)?;
        // Where a `free(p)` would sit: unmodelled, so it changes nothing.
        let q = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(q, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store node");
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("load node");
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::Disjoint,
            "the reuse is invisible: distinct calls stay distinct bases"
        );
        Ok(())
    }

    #[test]
    fn heap_and_stack_are_disjoint() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let stack_addr =
            b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I64)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(stack_addr, x, rsleigh::VnSpace::RAM)?;
        let p = alloc_call(&mut b, MALLOC)?;
        let eight2 = b.build_int_const(8u64, ValueType::I64)?;
        let heap_addr =
            b.build_int_binary_operation(p, eight2, IntBinaryOp::Add, ValueType::I64)?;
        let loaded = b.build_load(heap_addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        // Collapse the trivial sp Phi so the stack address decomposes.
        let fg = built_collapsed(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("load");
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::Disjoint,
            "a heap object and a stack slot never overlap"
        );
        Ok(())
    }

    /// Within one allocation, non-overlapping byte ranges are disjoint, exactly
    /// the SP-rooted offset semantics.
    #[test]
    fn same_heap_object_uses_offset_ranges() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let at0 = b.build_int_binary_operation(p, zero, IntBinaryOp::Add, ValueType::I64)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(at0, x, rsleigh::VnSpace::RAM)?; // [p+0..8)
        let sixteen = b.build_int_const(16u64, ValueType::I64)?;
        let at16 = b.build_int_binary_operation(p, sixteen, IntBinaryOp::Add, ValueType::I64)?;
        let loaded = b.build_load(at16, rsleigh::VnSpace::RAM, ValueType::I64)?; // [p+16..24)
        b.build_return(Some(loaded), &[])?;
        let fg = built(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg.producer(loaded);
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::Disjoint,
            "[p+16) and [p+0) are disjoint ranges in the same allocation"
        );
        Ok(())
    }

    /// SOUNDNESS: a heap base against an opaque pointer must stay may-alias:
    /// the opaque pointer could be this very allocation, spilled and reloaded.
    #[test]
    fn heap_vs_opaque_may_alias() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        // An opaque store address: a pointer loaded from a global.
        let global = b.build_int_const(0x4000u64, ValueType::I64)?;
        let opaque = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::I64)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(opaque, x, rsleigh::VnSpace::RAM)?;
        let p = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg.producer(loaded);
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::MayAlias,
            "an opaque pointer could be this allocation laundered through memory"
        );
        Ok(())
    }

    /// A pure allocator does not touch this function's private frame, so a
    /// stack reload steps *through* the allocator call to its store.  The slot
    /// is BELOW the entry SP: the relaxation covers this frame only, the same
    /// bound `escape_analysis` forwards under.
    #[test]
    fn allocator_call_is_transparent_to_a_stack_slot() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot_off = b.build_int_const((-8i64) as u64, ValueType::I64)?;
        let store_addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(store_addr, x, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let load_addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built_collapsed(b, &[MALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("load");
        let mem = fg.node_inputs(load)[0];
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.nearest_clobber(&fg, load, mem),
            store,
            "the allocator call is transparent to the stack slot; the store is the nearest clobber"
        );
        Ok(())
    }

    /// Symmetric: a load *of the freshly allocated object* must stop at the
    /// allocator call; the call is that region's definition point.
    #[test]
    fn allocator_call_clobbers_its_own_allocation() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built(b, &[MALLOC])?;

        let call = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Call))
            .expect("call");
        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.nearest_clobber(&fg, load, mem),
            call,
            "a load of the fresh allocation stops at the allocator call, not past it"
        );
        Ok(())
    }

    /// End-to-end: a stack value spilled before a malloc still reloads through
    /// the full pipeline.
    #[test]
    fn stack_spill_forwards_across_allocator_end_to_end() -> crate::Result<()> {
        use strider_ir::IRViewer;
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot_off = b.build_int_const((-8i64) as u64, ValueType::I64)?;
        let addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let secret = b.build_int_const(0x99u64, ValueType::I64)?;
        b.build_store(addr, secret, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let addr2 =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let reload = b.build_load(addr2, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        let mut ctx = crate::OptCtx::new(None);
        ctx.options.assumptions.noalias_allocators = [MALLOC].into_iter().collect();
        crate::test_support::standard_test().run(&mut fg, &mut ctx)?;

        let ret = crate::test_support::return_value(fg.graph())?;
        assert_eq!(
            fg.int_const_u128(ret),
            Some(0x99),
            "the reload forwards its spilled value across the transparent malloc"
        );
        Ok(())
    }

    /// SOUNDNESS: listing an allocator must not buy a stack relaxation
    /// `escape_analysis` refuses.  Once a frame address is in memory the callee
    /// may hold it, so the spill does NOT forward across the allocator call
    /// either, even though nothing else changed.
    #[test]
    fn escaped_frame_does_not_forward_across_an_allocator() -> crate::Result<()> {
        use strider_ir::IRViewer;
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot_off = b.build_int_const((-8i64) as u64, ValueType::I64)?;
        let addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let secret = b.build_int_const(0x99u64, ValueType::I64)?;
        b.build_store(addr, secret, rsleigh::VnSpace::RAM)?;
        // The escape: the slot's own address enters memory, at a disjoint slot
        // the walk steps through either way.
        let leak_off = b.build_int_const((-32i64) as u64, ValueType::I64)?;
        let leak_slot =
            b.build_int_binary_operation(sp_val, leak_off, IntBinaryOp::Add, ValueType::I64)?;
        b.build_store(leak_slot, addr, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let addr2 =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::I64)?;
        let reload = b.build_load(addr2, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        let mut ctx = crate::OptCtx::new(None);
        ctx.options.assumptions.noalias_allocators = [MALLOC].into_iter().collect();
        crate::test_support::standard_test().run(&mut fg, &mut ctx)?;

        let ret = crate::test_support::return_value(fg.graph())?;
        assert_eq!(
            fg.int_const_u128(ret),
            None,
            "an escaped frame slot must not forward across the allocator call"
        );
        Ok(())
    }

    /// SOUNDNESS: `ret_and_clobber_vns` drops a return register the function
    /// does not track, which slides the first CLOBBER into `outputs[2]`.  A
    /// clobber holds garbage, not a fresh pointer; tagging it a heap base makes
    /// everything derived from it Disjoint from all stack, constant and other
    /// heap accesses.
    #[test]
    fn untracked_return_register_leaves_the_clobber_opaque() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(clobber_reg())
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let target = b.build_int_const(MALLOC, ValueType::I64)?;
        // The call emits only the clobber, so `outputs[2]` is that clobber.
        let (call, outs) = b.build_call(target, &[], &[clobber_reg()], 0)?;
        let clobber = outs[0];
        b.build_return(Some(clobber), &[])?;
        let mut fg = built(b, &[MALLOC])?;
        // A per-call convention declaring a return register this function does
        // not track: the declaration alone must not mint a base.
        let cc = fg.default_cc().clone();
        fg.side_tables_mut().set_call_cc(
            call,
            strider_target::BuiltCallingConvention {
                ret_val_regs: vec![ret_reg()],
                ..cc
            },
        );
        assert!(
            decompose(&fg, clobber).is_none(),
            "with no tracked return register the first output is a clobber, not a heap base"
        );
        Ok(())
    }

    /// SOUNDNESS: an allocator is still a callee, and a callee owns the
    /// outgoing stack-argument area the ABI hands it.  The i386 cdecl idiom
    /// `mov [esp], 16 / call malloc` puts the size argument in slot 0; malloc
    /// may scratch it, so a reload of that slot must not forward.
    #[test]
    fn allocator_does_not_forward_its_own_stack_argument_slot() -> crate::Result<()> {
        use strider_ir::IRViewer;
        let sp = sp();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let slot0 = b.build_int_binary_operation(sp_val, zero, IntBinaryOp::Add, ValueType::I64)?;
        let size_arg = b.build_int_const(16u64, ValueType::I64)?;
        b.build_store(slot0, size_arg, rsleigh::VnSpace::RAM)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let reload = b.build_load(slot0, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(reload), &[])?;
        let fg = built_collapsed(b, &[MALLOC])?;

        let load = fg.producer(reload);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "the allocator owns its own argument slot, so the walk must stop on it"
        );
        Ok(())
    }

    /// SOUNDNESS guard: a listed address whose CC declares no return register
    /// (a void callee wrongly configured as an allocator) must not mint a heap
    /// base from its first (clobber) output.
    #[test]
    fn callee_with_no_return_register_is_not_a_heap_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp) // a void callee.
            .stack_vn(sp)
            .build_fn_single_region()?;
        let p = alloc_call(&mut b, MALLOC)?;
        b.build_return(Some(p), &[])?;
        let fg = built(b, &[MALLOC])?;
        assert!(
            decompose(&fg, p).is_none(),
            "a callee with no return register must not be treated as an allocator"
        );
        Ok(())
    }

    #[test]
    fn empty_allocator_set_leaves_heap_opaque() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let addr = b.build_int_binary_operation(p, eight, IntBinaryOp::Add, ValueType::I64)?;
        b.build_return(Some(addr), &[])?;
        let fg = built(b, &[])?;
        assert!(
            decompose(&fg, addr).is_none(),
            "with an empty allocator set, a heap pointer must stay opaque (feature off)"
        );
        Ok(())
    }

    /// Pins the memory half of `preserves_all`: a call declared transparent to
    /// memory must not stop the walk.
    #[test]
    fn preserves_memory_call_does_not_clobber() -> crate::Result<()> {
        let load_across_call = |preserves_memory: bool| -> crate::Result<bool> {
            let mut b = builder()?;
            let global = b.build_int_const(0x3000u64, ValueType::I64)?;
            let stored = b.build_int_const(7u64, ValueType::I64)?;
            b.build_store(global, stored, rsleigh::VnSpace::RAM)?;
            let target = b.build_int_const(0x2000u64, ValueType::I64)?;
            let (call, _rets) = b.build_call(target, &[], &[ret_reg()], 0)?;
            let loaded = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            let mut fg = built(b, &[])?;

            let cc = fg.default_cc().clone();
            fg.side_tables_mut().set_call_cc(
                call,
                strider_target::BuiltCallingConvention {
                    preserves_memory,
                    ..cc
                },
            );

            let load = fg.producer(loaded);
            let mem = fg.node_inputs(load)[0];
            let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true));
            let clobber = analyzer.nearest_clobber(&fg, load, mem);
            Ok(matches!(fg.node_kind(clobber), NodeKind::Store(_)))
        };

        assert!(
            !load_across_call(false)?,
            "a plain Call must stop the walk (call_blocking)"
        );
        assert!(
            load_across_call(true)?,
            "preserves_memory must let the walk step through the Call to the Store"
        );
        Ok(())
    }

    /// `preserves_memory` is a per-`CallOther` ABI attribute, so an opaque
    /// user-op declared transparent to memory must not stop the walk either.
    #[test]
    fn preserves_memory_call_other_does_not_clobber() -> crate::Result<()> {
        let load_across_call_other = |preserves_memory: bool| -> crate::Result<bool> {
            let mut b = builder()?;
            let global = b.build_int_const(0x3000u64, ValueType::I64)?;
            let stored = b.build_int_const(7u64, ValueType::I64)?;
            b.build_store(global, stored, rsleigh::VnSpace::RAM)?;
            let (call_other, _rets) = b.build_call_other(0, &[], &[], true, false)?;
            let loaded = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            let mut fg = built(b, &[])?;

            let cc = fg.default_cc().clone();
            fg.side_tables_mut().set_call_cc(
                call_other,
                strider_target::BuiltCallingConvention {
                    preserves_memory,
                    ..cc
                },
            );

            let load = fg.producer(loaded);
            let mem = fg.node_inputs(load)[0];
            let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true));
            let clobber = analyzer.nearest_clobber(&fg, load, mem);
            Ok(matches!(fg.node_kind(clobber), NodeKind::Store(_)))
        };

        assert!(
            !load_across_call_other(false)?,
            "a plain CallOther must stop the walk (call_blocking)"
        );
        assert!(
            load_across_call_other(true)?,
            "preserves_memory must let the walk step through the CallOther"
        );
        Ok(())
    }

    /// SOUNDNESS: `frame_escape` proves only that no address of THIS frame
    /// escaped.  A slot at a positive offset from the entry SP is the CALLER's
    /// outgoing-argument block, which the caller may already have leaked a
    /// pointer into (`va_list`'s overflow area points straight at it), so a
    /// callee must be assumed able to write it.
    #[test]
    fn incoming_stack_arg_does_not_forward_across_a_call() -> crate::Result<()> {
        let sp_vn = sp();
        let stack_args = strider_target::StackArgs {
            base_offset: 8,
            increment: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp_vn)
            .tracked(ret_reg())
            .arg(sp_vn)
            .ret(ret_reg())
            .stack_vn(sp_vn)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp = b.read_variable(&sp_vn)?;
        // +16: the second incoming stack argument, above the entry SP, so in
        // the caller's frame.
        let off = b.build_int_const(16u64, ValueType::I64)?;
        let arg_slot = b.build_int_binary_operation(sp, off, IntBinaryOp::Add, ValueType::I64)?;
        let seed = b.build_int_const(7u64, ValueType::I64)?;
        b.build_store(arg_slot, seed, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x2000u64, ValueType::I64)?;
        b.build_call(target, &[], &[ret_reg()], 0)?;
        let loaded = b.build_load(arg_slot, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built_collapsed(b, &[])?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "a slot above the entry SP is the caller's memory, which the \
             private-frame proof does not cover; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// A second allocator, so the merge is over two different call sites like
    /// an inlined size-class dispatch.
    const KMEM_CACHE_ALLOC: u64 = 0x1800;

    /// `if (c) p = malloc() else p = kmem_cache_alloc()`, leaving `p` a `Phi`
    /// of two allocator returns in the join, with the join region open.
    /// Returns the phi and both arms.
    fn alloc_phi_diamond() -> crate::Result<(FunctionBuilder, ValueId, ValueId, ValueId)> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let c = b.create_region_all()?;
        let join = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let sp_val = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let cond =
            b.build_int_cmp_operation(sp_val, zero, strider_ir::IntCmpOp::Equal, ValueType::I64)?;
        b.build_if(cond, a, c)?;

        b.set_region(a);
        let pa = alloc_call(&mut b, MALLOC)?;
        b.write_variable(&ret_reg(), pa)?;
        b.build_branch(join)?;

        b.set_region(c);
        let pc = alloc_call(&mut b, KMEM_CACHE_ALLOC)?;
        b.write_variable(&ret_reg(), pc)?;
        b.build_branch(join)?;

        b.set_region(join);
        let phi = b.read_variable(&ret_reg())?;
        Ok((b, phi, pa, pc))
    }

    /// The motivating case: a struct allocated by an inlined size-class
    /// dispatch is a `Phi` of two allocator returns, and a store into it must
    /// not disqualify the incoming stack-argument slots.
    #[test]
    fn phi_of_two_allocations_is_disjoint_from_the_stack() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let (mut b, phi, _pa, _pc) = alloc_phi_diamond()?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(phi, x, rsleigh::VnSpace::RAM)?;
        let sp_val = b.read_variable(&sp())?;
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let slot = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I64)?;
        let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built_collapsed(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("load");
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::Disjoint,
            "a store through a phi of allocations cannot reach a stack slot"
        );
        Ok(())
    }

    /// SOUNDNESS: the phi may BE either arm at runtime, so it must not carry an
    /// identity of its own.  Classifying it as a fresh `HeapRooted` base would
    /// make it Disjoint from the very allocation it holds.
    #[test]
    fn phi_of_allocations_may_alias_its_own_arm() -> crate::Result<()> {
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        let (mut b, phi, pa, _pc) = alloc_phi_diamond()?;
        let x = b.build_int_const(0x11u64, ValueType::I64)?;
        b.build_store(pa, x, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(phi, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        let fg = built_collapsed(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let store = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let load = fg.producer(loaded);
        let cfg = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_eq!(
            cfg.verdict(&fg, load, store),
            AliasVerdict::MayAlias,
            "the phi may be exactly the allocation the store wrote"
        );
        Ok(())
    }

    /// One non-heap arm is enough to lose the guarantee: the pointer may be the
    /// stack slot at runtime.
    #[test]
    fn phi_with_a_stack_arm_is_opaque() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let c = b.create_region_all()?;
        let join = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let sp_val = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let cond =
            b.build_int_cmp_operation(sp_val, zero, strider_ir::IntCmpOp::Equal, ValueType::I64)?;
        b.build_if(cond, a, c)?;

        b.set_region(a);
        let pa = alloc_call(&mut b, MALLOC)?;
        b.write_variable(&ret_reg(), pa)?;
        b.build_branch(join)?;

        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let slot = b.build_int_binary_operation(sp_c, eight, IntBinaryOp::Add, ValueType::I64)?;
        b.write_variable(&ret_reg(), slot)?;
        b.build_branch(join)?;

        b.set_region(join);
        let phi = b.read_variable(&ret_reg())?;
        b.build_return(Some(phi), &[])?;
        let fg = built_collapsed(b, &[MALLOC])?;
        assert!(
            decompose(&fg, phi).is_none(),
            "a phi merging a stack pointer names no allocation"
        );
        Ok(())
    }

    /// A loop-carried pointer walk: the phi is one of its own arms' operands.
    /// The frontier must terminate and still see only heap terminals.
    #[test]
    fn self_referential_phi_terminates() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let header = b.create_region_all()?;
        let body = b.create_region_all()?;
        let exit = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let p0 = alloc_call(&mut b, MALLOC)?;
        b.write_variable(&ret_reg(), p0)?;
        b.build_branch(header)?;

        b.set_region(header);
        let phi = b.read_variable(&ret_reg())?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let cond =
            b.build_int_cmp_operation(phi, zero, strider_ir::IntCmpOp::Equal, ValueType::I64)?;
        b.build_if(cond, body, exit)?;

        b.set_region(body);
        let eight = b.build_int_const(8u64, ValueType::I64)?;
        let next = b.build_int_binary_operation(phi, eight, IntBinaryOp::Add, ValueType::I64)?;
        b.write_variable(&ret_reg(), next)?;
        b.build_branch(header)?;

        b.set_region(exit);
        b.build_return(Some(phi), &[])?;
        let fg = built_collapsed(b, &[MALLOC])?;
        assert!(
            matches!(
                decompose(&fg, phi),
                Some(MemExpr {
                    kind: MemKind::HeapOpaque,
                    ..
                })
            ),
            "every value the walking pointer takes is derived from the allocation"
        );
        Ok(())
    }

    /// `levels` nested joins, each merging the previous phi with a fresh
    /// allocation, then one query per phi from the outermost in.
    fn phi_chain_steps(levels: usize) -> crate::Result<u64> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret_reg())
            .arg(sp)
            .ret(ret_reg())
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let alts: Vec<_> = (0..levels)
            .map(|_| b.create_region_all())
            .collect::<crate::Result<_>>()?;
        let joins: Vec<_> = (0..levels)
            .map(|_| b.create_region_all())
            .collect::<crate::Result<_>>()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let p0 = alloc_call(&mut b, MALLOC)?;
        b.write_variable(&ret_reg(), p0)?;
        let zero = b.build_int_const(0u64, ValueType::I64)?;
        let cond =
            b.build_int_cmp_operation(p0, zero, strider_ir::IntCmpOp::Equal, ValueType::I64)?;
        b.build_if(cond, alts[0], joins[0])?;

        for level in 0..levels {
            b.set_region(alts[level]);
            let p = alloc_call(&mut b, MALLOC)?;
            b.write_variable(&ret_reg(), p)?;
            b.build_branch(joins[level])?;

            b.set_region(joins[level]);
            let phi = b.read_variable(&ret_reg())?;
            if level + 1 == levels {
                b.build_return(Some(phi), &[])?;
            } else {
                let c = b.build_int_cmp_operation(
                    phi,
                    zero,
                    strider_ir::IntCmpOp::Equal,
                    ValueType::I64,
                )?;
                b.build_if(c, alts[level + 1], joins[level + 1])?;
            }
        }
        let fg = built(b, &[MALLOC])?;

        let phis: Vec<ValueId> = {
            use strider_ir::IRViewer;
            use strider_ir::node::NodeKind;
            fg.graph()
                .all_node_ids()
                .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi))
                .filter_map(|n| fg.node_outputs(n).first().copied())
                .collect()
        };
        SPINE_STEPS.with(|c| c.set(0));
        for v in phis.iter().rev() {
            decompose(&fg, *v);
        }
        Ok(SPINE_STEPS.with(std::cell::Cell::get))
    }

    /// The frontier walk must commit a verdict for every phi it visits, or a
    /// consumer-first sweep re-walks the whole chain per query.
    #[test]
    fn decomposing_a_phi_chain_is_not_quadratic() -> crate::Result<()> {
        let small = phi_chain_steps(50)?;
        let big = phi_chain_steps(200)?;
        assert!(
            big <= small * 6,
            "quadrupling the phi chain must not multiply the walk by 16: 50 \
             levels took {small} steps, 200 levels took {big}"
        );
        Ok(())
    }
}

/// The outgoing-argument window is computed from inside a memory-SSA walk, so
/// it must not start a walk that can re-enter it.
#[cfg(test)]
mod arg_window_complexity {
    use crate::mem_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    /// A spill at `sp - 8`, `calls` back-to-back calls under a lowered SP, then
    /// its reload.  The nearest call's window scan runs into the call before
    /// it, so the reload stops there; the cost of getting that answer must not
    /// grow with the call count.
    fn walk_steps_for(calls: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let k = b.build_int_const((-8i64) as u64, ValueType::I32)?;
        let slot = b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(slot, v, rsleigh::VnSpace::RAM)?;
        let frame = b.build_int_const((-64i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(sp_val, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        for i in 0..calls {
            let target = b.build_int_const(0x1000 + i as u64 * 0x10, ValueType::I32)?;
            b.build_call(target, &[], &[], 0)?;
        }
        let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        WALK_STEPS.with(|c| c.set(0));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "the window scan cannot see past the call before the nearest one, \
             so the nearest one keeps the slot"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the call count may only double the work.
    #[test]
    fn window_probe_cost_is_linear_in_call_count() -> crate::Result<()> {
        let small = walk_steps_for(12)?;
        let big = walk_steps_for(24)?;
        assert!(
            big * 100 <= small * 260,
            "doubling the call count must not blow up the walk: 12 calls took \
             {small} steps, 24 calls took {big}"
        );
        Ok(())
    }

    /// `slots` stack-argument stores below a spill, all under a lowered SP, one
    /// call, then the spill's reload.  The reload probes the call's argument
    /// window, which walks the prefix of argument slots, so the cost of one
    /// window computation is measured against the number of slots in it.
    fn walk_steps_for_arg_slots(slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // The frame is one slot deeper than the arguments need, so the spill at
        // `sp - 4` is the window's last slot rather than the caller's memory.
        let frame_bytes = 4 * (slots as i64 + 1);
        let spill_off = b.build_int_const((-4i64) as u64, ValueType::I32)?;
        let spill_slot =
            b.build_int_binary_operation(sp_val, spill_off, IntBinaryOp::Add, ValueType::I32)?;
        let spilled = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(spill_slot, spilled, rsleigh::VnSpace::RAM)?;
        for i in 0..slots {
            let off = b.build_int_const((4 * i as i64 - frame_bytes) as u64, ValueType::I32)?;
            let slot =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let arg = b.build_int_const(0x100 + i as u64, ValueType::I32)?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
        }
        let frame = b.build_int_const((-frame_bytes) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(sp_val, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, &[], &[], 0)?;
        let loaded = b.build_load(spill_slot, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        WALK_STEPS.with(|c| c.set(0));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "the spill sits one slot above the last argument, so the window \
             reaches it and the call clobbers"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the slot count may only double the work: one reverse scan of
    /// the memory chain answers every slot, rather than one scan per slot.
    #[test]
    fn window_probe_cost_is_linear_in_stack_arg_slots() -> crate::Result<()> {
        let small = walk_steps_for_arg_slots(100)?;
        let big = walk_steps_for_arg_slots(200)?;
        assert!(
            big * 100 <= small * 260,
            "doubling the argument-slot count must not square the walk: 100 \
             slots took {small} steps, 200 slots took {big}"
        );
        Ok(())
    }

    /// `slots` argument stores under a lowered SP, a call below them that the
    /// relaxation-free window probe cannot see past, then `loads` distinct
    /// reloads of slots above the argument stores.  Every reload asks the same
    /// call the same question, so the whole prefix walk must be paid once.
    fn walk_steps_for_loads(loads: usize, slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Blinds the prefix at the first slot with no store, so the window
        // reaches every reload instead of ending below them.
        let blinder = b.build_int_const(0x2000u64, ValueType::I32)?;
        b.build_call(blinder, &[], &[], 0)?;
        // The reloads occupy the `loads` slots just above the arguments.
        let frame_bytes = 4 * (slots + loads + 1) as i64;
        for j in 0..slots {
            let off = b.build_int_const((4 * j as i64 - frame_bytes) as u64, ValueType::I32)?;
            let slot =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let arg = b.build_int_const(0x100 + j as u64, ValueType::I32)?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
        }
        let frame = b.build_int_const((-frame_bytes) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(sp_val, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, &[], &[], 0)?;
        let mut sum = None;
        for i in 0..loads {
            let off = b.build_int_const((-4 * (i as i64 + 1)) as u64, ValueType::I32)?;
            let addr =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let v = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            sum = Some(match sum {
                None => v,
                Some(acc) => {
                    b.build_int_binary_operation(acc, v, IntBinaryOp::Add, ValueType::I32)?
                }
            });
        }
        b.build_return(sum, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let mut octx = crate::OptCtx::new(None);
        octx.options.assumptions.escape_analysis = true;
        WALK_STEPS.with(|c| c.set(0));
        let result = crate::pipeline::run_one(&crate::LoadForward::default(), &mut fg, &mut octx)?;
        assert_eq!(
            result,
            crate::OptimizationResult::NoChange,
            "the window covers every reload, so the call keeps all of them"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// `slots` argument stores under a lowered SP, one call, then a reload of
    /// every one of those slots, probed in ascending order through one
    /// analyzer.  Each probe reaches one slot further than the last, so the
    /// memoised window never covers it and the prefix is rescanned; the total
    /// cost of the run is what this measures.
    fn walk_steps_for_ascending_probes(slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let frame_bytes = 4 * slots as i64;
        let mut slot_addrs = Vec::with_capacity(slots);
        for i in 0..slots {
            let off = b.build_int_const((4 * i as i64 - frame_bytes) as u64, ValueType::I32)?;
            let slot =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let arg = b.build_int_const(0x100 + i as u64, ValueType::I32)?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
            slot_addrs.push(slot);
        }
        let frame = b.build_int_const((-frame_bytes) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(sp_val, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, &[], &[], 0)?;
        let mut reloads = Vec::with_capacity(slots);
        let mut sum = None;
        for &addr in &slot_addrs {
            let v = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            reloads.push(v);
            sum = Some(match sum {
                None => v,
                Some(acc) => {
                    b.build_int_binary_operation(acc, v, IntBinaryOp::Add, ValueType::I32)?
                }
            });
        }
        b.build_return(sum, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        WALK_STEPS.with(|c| c.set(0));
        for value in reloads {
            let load = fg.producer(value);
            let mem = fg.node_inputs(load)[0];
            let clobber = analyzer.nearest_clobber(&fg, load, mem);
            assert!(
                matches!(fg.node_kind(clobber), NodeKind::Call),
                "an argument slot is the callee's, so the call keeps it"
            );
        }
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the length of an ascending run of probes may only double the
    /// work: each rescan doubles the window it walks, so the rescans are
    /// logarithmic in the range and their total is linear in it.
    #[test]
    fn window_probe_cost_is_linear_in_ascending_probes() -> crate::Result<()> {
        let small = walk_steps_for_ascending_probes(100)?;
        let big = walk_steps_for_ascending_probes(200)?;
        assert!(
            big * 100 <= small * 260,
            "doubling an ascending run of probes must not square the walk: 100 \
             probes took {small} steps, 200 took {big}"
        );
        Ok(())
    }

    /// Ten times the reloads may not cost ten times the work: the window
    /// belongs to the call, not to the load that asked for it.
    #[test]
    fn window_probe_cost_is_flat_in_load_count() -> crate::Result<()> {
        let few = walk_steps_for_loads(2, 100)?;
        let many = walk_steps_for_loads(20, 100)?;
        assert!(
            many * 100 <= few * 200,
            "ten times the loads must not multiply the walk by ten: 2 loads \
             took {few} steps, 20 loads took {many}"
        );
        Ok(())
    }
}

/// `decompose` walks the whole address spine, so it must commit a verdict for
/// every node it passes through, not just the query root.
#[cfg(test)]
mod spine_memo {
    use crate::mem_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    /// `sp - 4 - 4 - ...`, `len` links deep, left un-flattened (`ConstantFold`
    /// would collapse it).  The returned chain runs shallowest link first.
    fn add_chain(len: usize) -> crate::Result<(strider_ir::Function, Vec<ValueId>)> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let mut cur = b.read_variable(&sp)?;
        let mut chain = Vec::with_capacity(len);
        for _ in 0..len {
            let minus_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
            cur =
                b.build_int_binary_operation(cur, minus_four, IntBinaryOp::Add, ValueType::I32)?;
            chain.push(cur);
        }
        b.build_return(Some(cur), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::new(None))?;
        Ok((fg, chain))
    }

    /// Consumer-first: deepest link first, then up.  Every query after the
    /// first should land on a committed verdict.
    fn spine_steps(len: usize) -> crate::Result<u64> {
        let (fg, chain) = add_chain(len)?;
        SPINE_STEPS.with(|c| c.set(0));
        for (i, v) in chain.iter().rev().enumerate() {
            let want = -4 * (len - i) as i128;
            assert_eq!(
                decompose(&fg, *v).map(|e| e.offset),
                Some(want),
                "link {i} from the bottom is sp{want}"
            );
        }
        Ok(SPINE_STEPS.with(std::cell::Cell::get))
    }

    #[test]
    fn decomposing_a_chain_consumer_first_is_not_quadratic() -> crate::Result<()> {
        let small = spine_steps(200)?;
        let big = spine_steps(800)?;
        assert!(
            big <= small * 6,
            "quadrupling the spine must not multiply the walk by 16: 200 links \
             took {small} steps, 800 links took {big}"
        );
        Ok(())
    }
}

/// The outgoing-argument window is derived from a probe that runs with call
/// relaxations off, so it stops at an earlier call.  A stop the probe cannot
/// see through must extend the window, not end it, or a load inside the window
/// forwards across the call.
#[cfg(test)]
mod arg_window_visibility {
    use crate::mem_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    /// ```text
    /// store P -> sp+0     ; f's arg0
    /// call f              ; hides the sp+0 store from a relaxation-free probe
    /// store Q -> sp+4     ; g's arg1
    /// call g              ; may overwrite sp+4
    /// load  sp+4
    /// ```
    #[test]
    fn a_slot_hidden_behind_an_earlier_call_stays_in_the_window() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;

        let zero = b.build_int_const(0u64, ValueType::I32)?;
        let slot0 = b.build_int_binary_operation(sp_val, zero, IntBinaryOp::Add, ValueType::I32)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let slot4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;

        let p = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(slot0, p, rsleigh::VnSpace::RAM)?;
        let f = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(f, &[], &[], 0)?;

        let q = b.build_int_const(0x22u64, ValueType::I32)?;
        b.build_store(slot4, q, rsleigh::VnSpace::RAM)?;
        let g = b.build_int_const(0x2000u64, ValueType::I32)?;
        b.build_call(g, &[], &[], 0)?;

        let loaded = b.build_load(slot4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "sp+4 is g's own argument slot, so g clobbers it; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// ```text
    /// sp' = sp - 16       ; this function's frame
    /// store -> sp'+4      ; anchored at the second slot, but shadowed below
    /// store -> sp'+0      ; slot 0
    /// store -> sp'+2      ; straddles slots 0 and 1, and reaches the call first
    /// call f
    /// load  sp'+4
    /// ```
    ///
    /// The store reaching slot 1 is the straddling one, anchored below the
    /// slot, so the slot was never written as a slot and the prefix ends
    /// before it.  Reading the deeper `sp+4` store as the slot's own would
    /// widen the window over the load and stop the forward.
    #[test]
    fn a_slot_reached_by_a_store_anchored_below_it_ends_the_prefix() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mut addrs = Vec::new();
        for off in [-12i64, -16, -14] {
            let k = b.build_int_const(off as u64, ValueType::I32)?;
            addrs.push(b.build_int_binary_operation(
                sp_val,
                k,
                IntBinaryOp::Add,
                ValueType::I32,
            )?);
        }
        for (i, addr) in addrs.iter().enumerate() {
            let data = b.build_int_const(0x11 * (i as u64 + 1), ValueType::I32)?;
            b.build_store(*addr, data, rsleigh::VnSpace::RAM)?;
        }
        let slot4 = addrs[0];
        let frame = b.build_int_const((-16i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(sp_val, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, &[], &[], 0)?;
        let loaded = b.build_load(slot4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Store(_)),
            "the window ends below the load, so the reload steps through the \
             call to the straddling store; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// The shape above with the argument slots under this function's own SP
    /// adjustment, so only the earlier call can end the window.
    ///
    /// ```text
    /// sp' = sp - 64        ; this function's frame
    /// store P -> sp'+4     ; f's first stack argument
    /// call f               ; hides the sp'+4 store from a relaxation-free probe
    /// store Q -> sp'+8     ; g's second stack argument
    /// call g               ; may overwrite sp'+8
    /// load  sp'+8
    /// ```
    #[test]
    fn a_frame_relative_slot_behind_an_earlier_call_stays_in_the_window() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let frame = b.build_int_const((-64i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;

        let four = b.build_int_const(4u64, ValueType::I32)?;
        let arg0 = b.build_int_binary_operation(call_sp, four, IntBinaryOp::Add, ValueType::I32)?;
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let arg1 =
            b.build_int_binary_operation(call_sp, eight, IntBinaryOp::Add, ValueType::I32)?;

        let p = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(arg0, p, rsleigh::VnSpace::RAM)?;
        let f = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(f, &[], &[], 0)?;

        let q = b.build_int_const(0x22u64, ValueType::I32)?;
        b.build_store(arg1, q, rsleigh::VnSpace::RAM)?;
        let g = b.build_int_const(0x2000u64, ValueType::I32)?;
        b.build_call(g, &[], &[], 0)?;

        let loaded = b.build_load(arg1, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "the earlier call hides slot 0, so the window cannot be shown to \
             end below sp'+8, which g owns; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// The relaxation's motivating shape: a register value spilled to the
    /// stack TOP, exactly where outgoing slot 0 sits, and reloaded after the
    /// call.  The store is indistinguishable from an argument push, so the
    /// window opens over it and the reload is pinned unless the caller opts
    /// into `callee_preserves_stack_args`.
    ///
    /// ```text
    /// sp' = sp - 64        ; this function's frame
    /// store V -> sp'+0     ; scratch spill, or f's arg0
    /// call f
    /// load  sp'+0
    /// ```
    fn spill_at_the_stack_top_forwards(relaxed: bool) -> crate::Result<bool> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let frame = b.build_int_const((-64i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;

        let zero = b.build_int_const(0u64, ValueType::I32)?;
        let slot0 =
            b.build_int_binary_operation(call_sp, zero, IntBinaryOp::Add, ValueType::I32)?;
        let spilled = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(slot0, spilled, rsleigh::VnSpace::RAM)?;
        let f = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(slot0, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(
            MemOptions::call_blocking(true)
                .with_callee_preserves_stack_args(relaxed)
                .with_escape_analysis(true),
        );
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        Ok(matches!(fg.node_kind(clobber), NodeKind::Store(_)))
    }

    #[test]
    fn a_stack_top_spill_forwards_only_under_the_relaxation() -> crate::Result<()> {
        assert!(
            !spill_at_the_stack_top_forwards(false)?,
            "without the relaxation the spill sits in f's argument window, so \
             f keeps the slot"
        );
        assert!(
            spill_at_the_stack_top_forwards(true)?,
            "with the relaxation the window is empty, so the reload forwards \
             to the spill"
        );
        Ok(())
    }

    /// The counterpart the fail-closed reading must not swallow: a genuine
    /// local, below the incoming-argument bound and anchoring no
    /// outgoing-argument slot, still forwards across the call.
    #[test]
    fn a_local_below_the_argument_bound_still_forwards_across_a_call() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let minus_eight = b.build_int_const((-8i64) as u64, ValueType::I32)?;
        let local =
            b.build_int_binary_operation(entry_sp, minus_eight, IntBinaryOp::Add, ValueType::I32)?;
        let spilled = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(local, spilled, rsleigh::VnSpace::RAM)?;

        let frame = b.build_int_const((-64i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let f = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(local, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Store(_)),
            "a private-frame local above the call's SP and below the argument \
             bound still forwards; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// A store the window scan cannot place is a stop it cannot see through,
    /// so it must extend the window like any other blind stop.  Reading it as
    /// "no argument reaches this slot" ends the prefix below the argument
    /// stores it hides and hands the load a slot the ABI gives the callee.
    ///
    /// ```text
    /// store A0 -> sp-16    ; slot 0, the call SP
    /// store A1 -> sp-12    ; slot 1
    /// store A2 -> sp-8     ; slot 2, the probed slot
    /// g = load [0x4000]    ; opaque pointer
    /// store J -> [g]       ; the scan cannot place this
    /// call f               ; sp = sp-16
    /// load  sp-8
    /// ```
    #[test]
    fn an_unplaceable_store_does_not_end_the_argument_window() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let stack_args = strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(stack_args))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;

        let mut slots = Vec::new();
        for off in [-16i64, -12, -8] {
            let k = b.build_int_const(off as u64, ValueType::I32)?;
            slots.push(b.build_int_binary_operation(
                entry_sp,
                k,
                IntBinaryOp::Add,
                ValueType::I32,
            )?);
        }
        for (i, slot) in slots.iter().enumerate() {
            let arg = b.build_int_const(0x10 * (i as u64 + 1), ValueType::I32)?;
            b.build_store(*slot, arg, rsleigh::VnSpace::RAM)?;
        }

        let global = b.build_int_const(0x4000u64, ValueType::I32)?;
        let opaque = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::I32)?;
        let junk = b.build_int_const(0x99u64, ValueType::I32)?;
        b.build_store(opaque, junk, rsleigh::VnSpace::RAM)?;

        let frame = b.build_int_const((-16i64) as u64, ValueType::I32)?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::I32)?;
        b.write_variable(&sp, call_sp)?;
        let f = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(slots[2], rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut pipe = crate::OptimizerPipeline::new();
        pipe.add(crate::PhiCollapse);
        pipe.add(crate::RegionCollapse);
        pipe.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load = fg.producer(loaded);
        let mem = fg.node_inputs(load)[0];
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(true).with_escape_analysis(true));
        let clobber = analyzer.nearest_clobber(&fg, load, mem);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call),
            "sp-8 is the callee's third outgoing argument slot, which the \
             opaque store hides rather than disproves; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }
}

#[cfg(test)]
mod own_frame_tests {
    use crate::mem_analysis::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    /// `in_own_frame` reads a raw base, so it must recognise the alignment
    /// anchor `decompose` produces rather than take any `And` on trust.
    #[test]
    fn only_an_alignment_masked_sp_is_a_frame_base() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 0,
                increment: 4,
            }))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let align = b.build_int_const(0xFFFF_FFF0u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(entry_sp, align, IntBinaryOp::And, ValueType::I32)?;
        let low_bits = b.build_int_const(0xFu64, ValueType::I32)?;
        let extracted =
            b.build_int_binary_operation(entry_sp, low_bits, IntBinaryOp::And, ValueType::I32)?;
        b.build_store(aligned, extracted, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;

        assert!(
            in_own_frame(&fg, aligned, -8, 4),
            "an alignment-masked SP is this frame's base"
        );
        assert!(
            !in_own_frame(&fg, extracted, -8, 4),
            "a low-bit extraction of SP is a value, not a stack base"
        );
        Ok(())
    }

    /// The bound is on the access's END. A slot starting below the caller's
    /// outgoing-argument block but reaching over into it is not private: the
    /// callee owns those bytes and may write them back.
    #[test]
    fn an_access_reaching_over_the_argument_bound_is_not_private() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 4,
                increment: 4,
            }))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let align = b.build_int_const(0xFFFF_FFF0u64, ValueType::I32)?;
        let entry_sp =
            b.build_int_binary_operation(entry_sp, align, IntBinaryOp::And, ValueType::I32)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;

        assert!(
            in_own_frame(&fg, entry_sp, -8, 4),
            "a slot wholly below the bound is this frame's"
        );
        assert!(
            in_own_frame(&fg, entry_sp, 0, 4),
            "an access ending exactly at the bound is still this frame's"
        );
        assert!(
            !in_own_frame(&fg, entry_sp, 4, 4),
            "a slot wholly in the caller's block is not"
        );
        assert!(
            !in_own_frame(&fg, entry_sp, 2, 4),
            "an access straddling the bound reaches the caller's block"
        );
        assert!(
            !in_own_frame(&fg, entry_sp, 0, 8),
            "a wide access from below the bound reaches over it"
        );
        Ok(())
    }

    /// The alignment anchor is a mask of whatever SP-rooted expression the
    /// spine was on, not of `sp` itself. `(sp + K) & !0xF` with a POSITIVE K
    /// sits inside the CALLER's frame, so accepting it as this frame's base
    /// would let a spill forward past a store through a caller-supplied
    /// pointer. The bound is checked in the entry SP's coordinates, so the base
    /// has to be at or below it.
    #[test]
    fn an_alignment_anchor_above_the_entry_sp_is_not_this_frame() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 4,
                increment: 4,
            }))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::I32)?;

        let below = b.build_int_const((-0x20i64) as u64, ValueType::I32)?;
        let sp_below =
            b.build_int_binary_operation(entry_sp, below, IntBinaryOp::Add, ValueType::I32)?;
        let anchor_below =
            b.build_int_binary_operation(sp_below, mask, IntBinaryOp::And, ValueType::I32)?;

        let above = b.build_int_const(0x1000u64, ValueType::I32)?;
        let sp_above =
            b.build_int_binary_operation(entry_sp, above, IntBinaryOp::Add, ValueType::I32)?;
        let anchor_above =
            b.build_int_binary_operation(sp_above, mask, IntBinaryOp::And, ValueType::I32)?;

        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;

        assert!(
            in_own_frame(&fg, anchor_below, 0, 4),
            "an anchor below the entry SP is this frame's"
        );
        assert!(
            !in_own_frame(&fg, anchor_above, 0, 4),
            "an anchor ABOVE the entry SP is the caller's frame, not ours"
        );
        Ok(())
    }
}

#[cfg(test)]
mod comparability_tests {
    use crate::mem_analysis::*;

    fn probe(size: i128) -> SizedAddr {
        SizedAddr {
            class: AddrClass::Constant { addr: 0 },
            size,
            addr_bits: Some(32),
        }
    }

    /// A probe spanning more than half the address space leaves no distance
    /// short enough to tell the two ways round apart, so nothing compares.
    /// The window probe reaches this width from a `call_sp` and a load offset
    /// that reduced to opposite ends of the signed range.
    #[test]
    fn a_probe_wider_than_half_the_modulus_compares_nothing() {
        let wide = probe(3i128 << 30);
        assert!(
            !offsets_comparable(wide, probe(4), 0, 1000),
            "a span past the half-modulus rejects every pair"
        );
        assert!(
            !offsets_comparable(probe(4), wide, 0, 1000),
            "either operand's span counts"
        );
    }

    /// Each offset is reduced at its OWN address width, so a 32-bit reduction
    /// and a 64-bit one are values mod different powers of two. Their
    /// difference names no distance, and reading the bound off either width
    /// alone lets a range that wraps 2^32 look ~4 GiB clear of the load.
    #[test]
    fn two_offsets_reduced_at_different_widths_compare_nothing() {
        let narrow = SizedAddr {
            class: AddrClass::Constant { addr: 0 },
            size: 4,
            addr_bits: Some(32),
        };
        let wide = SizedAddr {
            class: AddrClass::Constant { addr: 0 },
            size: 8,
            addr_bits: Some(64),
        };
        // Adjacent under a single 64-bit bound.
        assert!(
            !offsets_comparable(wide, narrow, 0, -4),
            "a 64-bit and a 32-bit reduction live in different moduli"
        );
        assert!(
            !offsets_comparable(narrow, wide, 0, -4),
            "and neither order rescues them"
        );
    }

    #[test]
    fn an_ordinary_probe_still_compares_nearby_offsets() {
        assert!(offsets_comparable(probe(4), probe(4), 0, 1000));
    }
}

#[cfg(test)]
mod modular_offset_tests {
    use crate::mem_analysis::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
    use strider_ir_test_utils::RegisterSet;

    use super::super::test_sp as sp;

    /// Address arithmetic is mod 2^32 here, so `sp+0x7FFFFFFE` and
    /// `sp+0x80000000` name slots two bytes apart and their four-byte accesses
    /// overlap.  Reducing both into the signed range puts them at opposite ends
    /// of the integer order, where a non-modular interval test reads them as
    /// 2^32-2 apart.
    #[test]
    fn offsets_two_bytes_apart_across_the_wrap_may_alias() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let k_a = b.build_int_const(0x7FFF_FFFEu64, ValueType::I32)?;
        let a = b.build_int_binary_operation(sp_val, k_a, IntBinaryOp::Add, ValueType::I32)?;
        let k_b = b.build_int_const(0x8000_0000u64, ValueType::I32)?;
        let bb = b.build_int_binary_operation(sp_val, k_b, IntBinaryOp::Add, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(bb, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(a, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::new(None))?;

        let load_node = fg.producer(loaded);
        let store_node = fg
            .walk()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("store");
        let analyzer = MemAnalyzer::new(MemOptions::call_blocking(false));
        assert_ne!(
            analyzer.verdict(&fg, load_node, store_node),
            AliasVerdict::Disjoint,
            "the accesses are two bytes apart mod 2^32 and overlap",
        );
        Ok(())
    }
}
