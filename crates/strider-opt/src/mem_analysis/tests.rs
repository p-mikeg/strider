use super::alias::{Loc, alias_verdict, classify_addr};
use super::decompose::{SPINE_STEPS, is_alignment_mask};
use super::frame::in_own_frame;
use super::index::StoreIndex;
use super::query::Calls;
use super::ring::addr_bit_width;
use super::walker::MemWalker;
use super::*;
use crate::test_support::{STANDARD, run_passes};
use strider_ir::node::ValueId;
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, FunctionBuilder, IRViewer};
use strider_ir_test_utils::IrWalkerEx;

/// The pairwise verdict between two nodes, which production reads off the
/// climb ([`MemQuery::reaching`]) rather than re-deriving.  Distinct
/// address spaces never alias even at the same numeric offset.
fn verdict(policy: Policy, f: &Function, load_node: NodeId, store_node: NodeId) -> AliasVerdict {
    alias_verdict(
        Loc::read_by(f, load_node),
        Loc::written_by(f, store_node),
        policy,
    )
}

/// The fixtures below build exactly one `Store`.
fn only_store(f: &Function) -> NodeId {
    f.sole_kind(|k| matches!(k, NodeKind::Store(_)))
}

/// What `CallStackArgCollect` runs under over `f`, claiming
/// `stack_global_disjoint` on it when asked.  Adds to `f`'s claims, so call it
/// before installing a stand-in memo, which a new claim drops.
fn call_blocking(f: &mut Function, stack_global_disjoint: bool) -> Policy {
    debug_assert!(
        stack_global_disjoint || !f.assumptions().stack_global_disjoint,
        "the fixture already claims what this call leaves off"
    );
    f.assume(&crate::AssumptionOptions {
        stack_global_disjoint,
        ..crate::AssumptionOptions::none()
    });
    Policy::strict(f)
}

/// What `LoadForward` runs under over `f`, claiming `stack_global_disjoint`
/// and `escape_analysis` on it, and `callee_preserves_stack_args` when asked.
/// Adds to `f`'s claims, so call it before installing a stand-in memo.
fn escaping(f: &mut Function, callee_preserves_stack_args: bool) -> Policy {
    debug_assert!(
        callee_preserves_stack_args || !f.assumptions().callee_preserves_stack_args,
        "the fixture already claims what this call leaves off"
    );
    f.assume(&crate::AssumptionOptions {
        stack_global_disjoint: true,
        escape_analysis: true,
        callee_preserves_stack_args,
        ..crate::AssumptionOptions::none()
    });
    Policy::forward(f)
}

/// Returns `loaded`, builds, and collapses the region phis: the function and
/// the `Load` that produced `loaded`.
fn finish(mut b: FunctionBuilder, loaded: ValueId) -> crate::Result<(Function, NodeId)> {
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    collapse_phis(&mut fg);
    let load = fg.producer(loaded);
    Ok((fg, load))
}

/// The nearest clobber of `load` under `policy`, from a fresh query.
fn nearest(f: &Function, load: NodeId, policy: Policy) -> NodeId {
    MemQuery::default().reaching(f, load, policy).node()
}

/// [`nearest`] under [`escaping`].
fn reload_clobber(f: &mut Function, load: NodeId, callee_preserves_stack_args: bool) -> NodeId {
    let policy = escaping(f, callee_preserves_stack_args);
    nearest(f, load, policy)
}

/// The floor plus `allocators` listed as pure allocators.
fn allocating(allocators: &[u64]) -> crate::AssumptionOptions {
    crate::AssumptionOptions {
        noalias_allocators: allocators.iter().copied().collect(),
        ..crate::AssumptionOptions::none()
    }
}

mod decompose_tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86 as sp};

    /// `body` over a one-region frame of `sp`, its last value returned: the
    /// collapsed function and the values `body` names.
    fn sp_fn<const N: usize>(
        body: impl FnOnce(&mut FunctionBuilder, ValueId) -> crate::Result<[ValueId; N]>,
    ) -> crate::Result<(Function, [ValueId; N])> {
        let sp = sp();
        let mut b = sp_arg_frame(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let values = body(&mut b, sp_val)?;
        b.build_return(values.last().copied(), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        Ok((fg, values))
    }

    /// The post-`ConstantFold` shape of `x - k`, i.e. `Add(x, IntConst(-k))`.
    fn sub_off(b: &mut FunctionBuilder, x: ValueId, k: i64) -> crate::Result<ValueId> {
        crate::test_support::at(b, x, -k, ValueType::int(32))
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let (fg, _) = sp_fn(|_, sp_val| Ok([sp_val]))?;
        let r = decompose(&fg, fg.returned());
        assert!(matches!(r, Some(MemExpr { offset: 0, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let (fg, [addr]) = sp_fn(|b, sp_val| Ok([sub_off(b, sp_val, 4)?]))?;
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
        let (fg, [twice]) = sp_fn(|b, sp_val| {
            let big = b.build_int_const(0x7FFF_FFFFu64, ValueType::int(32))?;
            let once =
                b.build_int_binary_operation(sp_val, big, IntBinaryOp::Add, ValueType::int(32))?;
            Ok([b.build_int_binary_operation(once, big, IntBinaryOp::Add, ValueType::int(32))?])
        })?;
        assert_eq!(
            decompose(&fg, twice).map(|e| e.offset),
            Some(-2),
            "two 0x7FFFFFFF bumps of a 32-bit pointer land at sp - 2"
        );
        Ok(())
    }

    /// At 128 bits the same wrap is exact on the `i128` carrier: two
    /// `i128::MAX` bumps land at `sp - 2`.
    #[test]
    fn decompose_wraps_a_128_bit_offset() -> crate::Result<()> {
        let sp = strider_ir_test_utils::reg_vn(0x20, 16);
        let ty = ValueType::int(128);
        let mut b = sp_arg_frame(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let big = b.build_int_const(i128::MAX as u128, ty)?;
        let once = b.build_int_binary_operation(sp_val, big, IntBinaryOp::Add, ty)?;
        let twice = b.build_int_binary_operation(once, big, IntBinaryOp::Add, ty)?;
        b.build_return(Some(twice), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert_eq!(decompose(&fg, twice).map(|e| e.offset), Some(-2));
        Ok(())
    }

    #[test]
    fn decompose_is_idempotent_and_committed_slot_round_trips() -> crate::Result<()> {
        // Committing a slot is what a run's closing stamp does.
        let (fg, [addr]) = sp_fn(|b, sp_val| Ok([sub_off(b, sp_val, 4)?]))?;
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
            fg.set_stack_slot(addr, base, offset);
        }
        assert_eq!(
            fg.memory_slot_resolved(addr).map(|(_, o)| o),
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
        let c = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(decompose(&fg, c).is_none());
        Ok(())
    }

    #[test]
    fn decompose_walks_deep_offset_chain() -> crate::Result<()> {
        let (fg, [s1, s2, s3]) = sp_fn(|b, sp_val| {
            let s1 = sub_off(b, sp_val, 4)?;
            let s2 = sub_off(b, s1, 8)?;
            Ok([s1, s2, sub_off(b, s2, 12)?])
        })?;

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
        let build = || -> crate::Result<(Function, [ValueId; 3])> {
            let mut b = sp_arg_frame(sp).build_fn()?;
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
            let sp_dec = sub_off(&mut b, sp_phi, 4)?;
            b.write_variable(&sp, sp_dec)?;
            let keep_looping = b.build_boolean_const(true);
            b.build_if(keep_looping, loop_hdr, exit)?;

            // A genuinely non-SP address in the same cone.
            b.set_region(exit);
            let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
            b.build_return(Some(global), &[])?;
            b.set_lift_addr(None);
            // Do NOT collapse phis: the multi-predecessor loop-header Phi has
            // to survive or the cone contains no cycle.
            Ok((b.build()?, [sp_phi, sp_dec, global]))
        };

        let (fg, values) = build()?;
        let truth = values.map(|v| decompose(&fg, v).map(|e| e.offset));
        assert!(
            truth[0].is_none(),
            "loop-header Phi(sp) is not an SP terminal"
        );
        assert!(
            truth[1].is_none(),
            "loop-carried Add over a Phi is not provable"
        );
        assert!(truth[2].is_none(), "global address is not SP-rooted");

        // Each order starts from a cold memo, so the first query of each walks.
        for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
            let (fg, values) = build()?;
            for i in order {
                assert_eq!(
                    decompose(&fg, values[i]).map(|e| e.offset),
                    truth[i],
                    "verdict for {:?} must be query-order-independent",
                    values[i]
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
        let mut b = sp_arg_frame(sp).build_fn()?;
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
        let sp_minus_4 = sub_off(&mut b, sp_a, 4)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: a literal pretending to be a new SP, not SP-rooted.
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, ValueType::int(32))?;
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
        let (fg, [aligned]) = sp_fn(|b, sp_val| {
            Ok([b.build_const_binop(
                0xFFFF_FFF8u64,
                sp_val,
                IntBinaryOp::And,
                ValueType::int(32),
            )?])
        })?;
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
        let (fg, [aligned, post_sub]) = sp_fn(|b, sp_val| {
            let aligned =
                b.build_const_binop(0xFFFF_FFF8u64, sp_val, IntBinaryOp::And, ValueType::int(32))?;
            Ok([aligned, sub_off(b, aligned, 0x1D0)?])
        })?;
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
        const N: usize = 6000;
        let (fg, [current]) = sp_fn(|b, mut current| {
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::int(32))?;
            for _ in 0..N {
                current = b.build_int_binary_operation(
                    current,
                    mask,
                    IntBinaryOp::And,
                    ValueType::int(32),
                )?;
            }
            Ok([current])
        })?;
        let r = decompose(&fg, current);
        assert!(matches!(r, Some(MemExpr { offset: 0, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_does_not_stack_overflow_on_deep_chain() -> crate::Result<()> {
        const N: usize = 5000;
        let (fg, [current]) = sp_fn(|b, mut current| {
            for _ in 0..N {
                current =
                    b.build_const_binop(1u64, current, IntBinaryOp::Add, ValueType::int(32))?;
            }
            Ok([current])
        })?;
        let MemExpr { offset, .. } = decompose(&fg, current)
            .expect("5000-node chain must decompose without stack-overflowing");
        assert_eq!(
            offset, N as i128,
            "cumulative offset must equal N adds of +1"
        );
        Ok(())
    }
}

mod alias_tests {
    use super::*;
    use crate::mem_analysis::store_value_byte_size;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    use super::only_store;

    /// The verdict of `f`'s store against a 4-byte entry-SP load at
    /// `load_offset`, under `stack_global_disjoint` and, when asked,
    /// `distinct_sp_bases_disjoint`.
    fn store_alias_verdict(
        f: &Function,
        load_offset: i128,
        distinct_sp_bases_disjoint: bool,
    ) -> AliasVerdict {
        let store = only_store(f);
        let store_size = store_value_byte_size(f, f.store_data(store));
        let mut claimed = f.clone();
        claimed.assume(&crate::AssumptionOptions {
            stack_global_disjoint: true,
            distinct_sp_bases_disjoint,
            ..crate::AssumptionOptions::default()
        });
        let f = &claimed;
        let options = Policy::incoming_args(f);
        let base = f
            .initial_var_value(&stack_vn_x86())
            .expect("InitialVar(sp) exists");
        let store_class = classify_addr(f, f.store_addr(store));
        // The real width the store's own address carries, so the wrap guard is
        // exercised at the width the lifter produces.
        let bits = addr_bit_width(f, f.store_addr(store));
        let at = |class, size| Loc {
            class,
            size,
            bits,
            space: rsleigh::VnSpace::RAM,
        };
        alias_verdict(
            at(
                AddrClass::StackRooted {
                    base,
                    offset: load_offset,
                },
                4,
            ),
            at(store_class, store_size),
            options,
        )
    }

    /// `store [base + 8]` plus a load from the same address, so the store and
    /// its SP-rooted address stay reachable.  `align` masks the base to 8
    /// bytes first, giving a base the decomposer cannot offset-compare with
    /// the entry SP.
    fn store_at_base_plus_eight(align: bool) -> Function {
        let mut f = make_sp_fn(stack_vn_x86(), |b, sp_val| {
            let base = if align {
                b.build_const_binop(0xFFFF_FFF8u64, sp_val, IntBinaryOp::And, ValueType::int(32))?
            } else {
                sp_val
            };
            let store_addr =
                b.build_const_binop(8u64, base, IntBinaryOp::Add, ValueType::int(32))?;
            let data = b.build_int_const(0xAAu64, ValueType::int(32))?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse_phis(&mut f);
        f
    }

    /// A `Store` at an alignment-masked base `(sp & mask) + 8` must not be
    /// proven disjoint from an entry-SP query just because the offsets do not
    /// overlap: the bases differ by the runtime `sp mod align`, so comparing
    /// their offsets is meaningless.  Under `distinct_sp_bases_disjoint` it is
    /// `Disjoint`: incoming-arg slots above the entry SP are assumed not to
    /// overlap frame locals at an alignment-masked SP.  On one base the
    /// offsets decide.
    #[test]
    fn store_alias_verdicts() {
        for (why, align, load_offset, distinct, want) in [
            (
                "a different base may alias",
                true,
                0,
                false,
                AliasVerdict::MayAlias,
            ),
            (
                "distinct_sp_bases_disjoint",
                true,
                0,
                true,
                AliasVerdict::Disjoint,
            ),
            (
                "one base, disjoint offsets",
                false,
                0,
                false,
                AliasVerdict::Disjoint,
            ),
            ("one base, one offset", false, 8, false, AliasVerdict::Match),
        ] {
            let f = store_at_base_plus_eight(align);
            assert_eq!(
                store_alias_verdict(&f, load_offset, distinct),
                want,
                "{why}"
            );
        }
    }
}

/// A store in one space never reaches a load in another, however the two
/// addresses compare.
///
/// Mutation: probe RAM whatever the load's space.
#[test]
fn a_ram_store_does_not_reach_a_register_space_probe() -> crate::Result<()> {
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    let mut load = None;
    let mut f =
        strider_ir_test_utils::make_sp_fn(strider_ir_test_utils::stack_vn_x86(), |b, sp| {
            let slot =
                b.build_const_binop((-8i64) as u64, sp, IntBinaryOp::Add, ValueType::int(32))?;
            let data = b.build_int_const(7u64, ValueType::int(32))?;
            b.build_store(slot, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(slot, rsleigh::VnSpace::REGISTER, ValueType::int(32))?;
            load = Some(loaded);
            b.build_return(Some(loaded), &[])
        })?;
    collapse_phis(&mut f);
    let load = f.producer(load.expect("built"));
    let MemExpr { base, offset, .. } = decompose(&f, f.load_addr(load)).expect("sp - 8");
    let query = MemQuery::default();
    let clobber = query.nearest_sp_clobber(
        &f,
        Policy::strict(&f),
        f.load_mem(load),
        base,
        offset,
        4,
        rsleigh::VnSpace::REGISTER,
    );
    let hit = super::query::anchored_store(&f, clobber, base);
    assert!(
        hit.is_none(),
        "the RAM store is not the register-space slot's"
    );
    Ok(())
}

mod cfg_tests {
    use super::*;
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
            let k = b.build_int_const(8u64, ValueType::int(32))?;
            // `xor(sp, 8)` is not a recognised SP base, so it classifies as
            // `Anchor`, standing in for an address a rewrite folded into a
            // non-decomposable shape.
            let opaque =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Xor, ValueType::int(32))?;
            let data = b.build_int_const(0x11u64, ValueType::int(32))?;
            b.build_store(opaque, data, rsleigh::VnSpace::RAM)?;
            // The real `sp + 8`, decomposing to StackRooted(8).
            let load_addr =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::int(32))?;
            let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .expect("build sp fn");

        collapse_phis(&mut f);

        let store = super::only_store(&f);
        let load = f.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let entry_sp = f.initial_var_value(&sp).expect("entry sp value");

        // Claimed before the stand-in below, which a new claim would drop.
        let policy = super::call_blocking(&mut f, true);
        // Stand in for the run's closing stamp: record `[sp+8]` even though the raw
        // address folded to an opaque shape.
        let store_addr = f.store_addr(store);
        f.set_stack_slot(store_addr, entry_sp, 8);

        assert_eq!(
            verdict(policy, &f, load, store),
            AliasVerdict::Match,
            "verdict must classify the store via the side-table SSoT (like the walker), \
             so a non-decomposable store still verifies as an exact match"
        );
    }
}

mod heap_tests {
    use super::*;
    use strider_ir::node::{ValueId, ValueType};
    use strider_ir::{Function, FunctionBuilder, IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, reg_vn, stack_args_at, stack_vn_x86_64 as sp};

    /// A return register distinct from SP, to receive an allocator's pointer.
    fn ret_reg() -> rsleigh::Vn {
        reg_vn(0x00, 8)
    }

    /// A second call output register, e.g. a caller-saved reg the allocator
    /// clobbers.
    fn clobber_reg() -> rsleigh::Vn {
        reg_vn(0x08, 8)
    }

    const MALLOC: u64 = 0x1000;

    /// SP as the argument-passing stack pointer and [`ret_reg`] as the return
    /// register.
    fn regs() -> RegisterSet {
        RegisterSet::new()
            .tracked(sp())
            .tracked(ret_reg())
            .arg(sp())
            .ret(ret_reg())
            .stack_vn(sp())
    }

    fn builder() -> crate::Result<FunctionBuilder> {
        regs().build_fn_single_region()
    }

    /// The function, claiming `allocators`.
    fn built(mut b: FunctionBuilder, allocators: &[u64]) -> crate::Result<Function> {
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&super::allocating(allocators));
        Ok(fg)
    }

    /// Like [`built`], but first runs `PhiCollapse`/`RegionCollapse` so a
    /// `read_variable(sp)` becomes the bare `InitialVar(sp)` that `decompose`
    /// recognises (matching the post-collapse state `LoadForward` runs in).
    fn built_collapsed(mut b: FunctionBuilder, allocators: &[u64]) -> crate::Result<Function> {
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        // The collapse ran at the floor; these queries run under `allocators`.
        fg.assume(&super::allocating(allocators));
        Ok(fg)
    }

    /// A prologue: drops SP by `bytes`, so a slot at `entry_sp - k` for
    /// `k < bytes` sits in THIS frame and above the SP a callee receives.
    fn lower_sp(b: &mut FunctionBuilder, entry_sp: ValueId, bytes: i64) -> crate::Result<()> {
        let call_sp = b.build_const_binop(
            (-bytes) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.write_variable(&sp(), call_sp)?;
        Ok(())
    }

    fn alloc_call(b: &mut FunctionBuilder, target_addr: u64) -> crate::Result<ValueId> {
        let target = b.build_int_const(target_addr, ValueType::int(64))?;
        let (_call, rets) = b.build_call(target, &[], &[ret_reg()], 0)?;
        Ok(rets[0])
    }

    #[test]
    fn decompose_heap_base_from_allocator_call() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let addr = b.build_const_binop(8u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        b.build_return(Some(addr), &[])?;
        let fg = built(b, &[MALLOC])?;
        let r = decompose(&fg, addr);
        assert!(
            matches!(r, Some(MemExpr { base, offset: 8, .. }) if base == p),
            "malloc()+8 must decompose to a heap base at the call's return value, got {r:?}"
        );
        Ok(())
    }

    /// The memo is the verdict `strider-pattern`'s region filters read, so an
    /// address rooted at a call the configured set does not list is committed
    /// `NotMemory`: `store().non_stack()` keeps the store, which it drops for an
    /// address left unanswered.
    #[test]
    fn an_unlisted_call_return_is_committed_not_memory_for_non_stack() -> crate::Result<()> {
        const UNLISTED: u64 = 0x2000;
        let mut b = builder()?;
        let p = alloc_call(&mut b, UNLISTED)?;
        let addr = b.build_const_binop(8u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let data = b.build_int_const(0x2au64, ValueType::int(64))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        let fg = built(b, &[MALLOC])?;

        assert!(
            decompose(&fg, p).is_none(),
            "the call is not in the allocator set, so its return is opaque"
        );
        assert!(decompose(&fg, addr).is_none());
        assert!(
            matches!(fg.memory_class(addr), strider_ir::MemDecomp::NotMemory),
            "the negative verdict must reach the memo"
        );
        let hits = strider_pattern::Matcher::new(&fg)
            .find_all(&strider_pattern::store().non_stack().build())
            .expect("the query compiles");
        assert_eq!(
            hits.len(),
            1,
            "non_stack keeps a store through an unlisted call return"
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
        let bumped = b.build_const_binop(15u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let aligned = b.build_const_binop(
            0xFFFF_FFFF_FFFF_FFF0u64,
            bumped,
            IntBinaryOp::And,
            ValueType::int(64),
        )?;
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
        let bumped = b.build_const_binop(15u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let aligned = b.build_const_binop(
            0xFFFF_FFFF_FFFF_FFF0u64,
            bumped,
            IntBinaryOp::And,
            ValueType::int(64),
        )?;
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

    /// SOUNDNESS: only the return pointer (`outputs[2]`) is a base. A clobbered
    /// register the allocator also writes holds garbage, not a fresh pointer.
    #[test]
    fn clobbered_output_is_not_a_heap_base() -> crate::Result<()> {
        let mut b = builder()?;
        let target = b.build_int_const(MALLOC, ValueType::int(64))?;
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
        let inner = b.build_const_binop(4u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let outer = b.build_const_binop(4u64, inner, IntBinaryOp::Add, ValueType::int(64))?;
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
    /// off, so the verdict rests on the noalias guarantee alone.
    ///
    /// LIVENESS is the scope of that guarantee, and the same graph pins the
    /// limitation: nothing models deallocation, so a second `malloc` is a
    /// distinct base even where the program freed the first and the allocator
    /// handed the same storage back.  A load from the stale pointer is then
    /// taken not to see the new object's stores.  Reaching that needs a
    /// use-after-free in the analysed program; modelling deallocation would
    /// make this verdict `MayAlias`.
    #[test]
    fn two_heap_objects_are_disjoint() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let x = b.build_int_const(0x11u64, ValueType::int(64))?;
        b.build_store(p, x, rsleigh::VnSpace::RAM)?;
        let q = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(q, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built(b, &[MALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
            AliasVerdict::Disjoint,
            "two distinct heap allocations never overlap"
        );
        Ok(())
    }

    #[test]
    fn heap_and_stack_are_disjoint() -> crate::Result<()> {
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let stack_addr = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
        let x = b.build_int_const(0x11u64, ValueType::int(64))?;
        b.build_store(stack_addr, x, rsleigh::VnSpace::RAM)?;
        let p = alloc_call(&mut b, MALLOC)?;
        let heap_addr = b.build_const_binop(8u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let loaded = b.build_load(heap_addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        // Collapse the trivial sp Phi so the stack address decomposes.
        let mut fg = built_collapsed(b, &[MALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
            AliasVerdict::Disjoint,
            "a heap object and a stack slot never overlap"
        );
        Ok(())
    }

    /// Within one allocation, non-overlapping byte ranges are disjoint, exactly
    /// the SP-rooted offset semantics.
    #[test]
    fn same_heap_object_uses_offset_ranges() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let at0 = b.build_const_binop(0u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let x = b.build_int_const(0x11u64, ValueType::int(64))?;
        b.build_store(at0, x, rsleigh::VnSpace::RAM)?; // [p+0..8)
        let at16 = b.build_const_binop(16u64, p, IntBinaryOp::Add, ValueType::int(64))?;
        let loaded = b.build_load(at16, rsleigh::VnSpace::RAM, ValueType::int(64))?; // [p+16..24)
        b.build_return(Some(loaded), &[])?;
        let mut fg = built(b, &[MALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.producer(loaded);
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
            AliasVerdict::Disjoint,
            "[p+16) and [p+0) are disjoint ranges in the same allocation"
        );
        Ok(())
    }

    /// SOUNDNESS: a heap base against an opaque pointer must stay may-alias:
    /// the opaque pointer could be this very allocation, spilled and reloaded.
    #[test]
    fn heap_vs_opaque_may_alias() -> crate::Result<()> {
        let mut b = builder()?;
        // An opaque store address: a pointer loaded from a global.
        let global = b.build_int_const(0x4000u64, ValueType::int(64))?;
        let opaque = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let x = b.build_int_const(0x11u64, ValueType::int(64))?;
        b.build_store(opaque, x, rsleigh::VnSpace::RAM)?;
        let p = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built(b, &[MALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.producer(loaded);
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
            AliasVerdict::MayAlias,
            "an opaque pointer could be this allocation laundered through memory"
        );
        Ok(())
    }

    /// Symmetric: a load *of the freshly allocated object* must stop at the
    /// allocator call; the call is that region's definition point.
    #[test]
    fn allocator_call_clobbers_its_own_allocation() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built(b, &[MALLOC])?;

        let call = fg.first_kind(|k| matches!(k, NodeKind::Call { .. }));
        let load = fg.producer(loaded);
        let cfg = MemQuery::default();
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            cfg.reaching(&fg, load, policy).node(),
            call,
            "a load of the fresh allocation stops at the allocator call, not past it"
        );
        Ok(())
    }

    /// End-to-end: a stack value spilled before a malloc still reloads through
    /// the full pipeline.
    #[test]
    fn stack_spill_forwards_across_allocator_end_to_end() -> crate::Result<()> {
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot_off = b.build_int_const((-8i64) as u64, ValueType::int(64))?;
        let addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::int(64))?;
        let secret = b.build_int_const(0x99u64, ValueType::int(64))?;
        b.build_store(addr, secret, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let addr2 =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::int(64))?;
        let reload = b.build_load(addr2, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        fg.assume(&super::allocating(&[MALLOC]));
        run_passes(&mut fg, STANDARD)?;

        let ret = fg.returned();
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
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot_off = b.build_int_const((-8i64) as u64, ValueType::int(64))?;
        let addr =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::int(64))?;
        let secret = b.build_int_const(0x99u64, ValueType::int(64))?;
        b.build_store(addr, secret, rsleigh::VnSpace::RAM)?;
        // The escape: the slot's own address enters memory, at a disjoint slot
        // the walk steps through either way.
        let leak_slot = b.build_const_binop(
            (-32i64) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.build_store(leak_slot, addr, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let addr2 =
            b.build_int_binary_operation(sp_val, slot_off, IntBinaryOp::Add, ValueType::int(64))?;
        let reload = b.build_load(addr2, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        fg.assume(&super::allocating(&[MALLOC]));
        run_passes(&mut fg, STANDARD)?;

        let ret = fg.returned();
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
        let target = b.build_int_const(MALLOC, ValueType::int(64))?;
        // The call emits only the clobber, so `outputs[2]` is that clobber.
        let (call, outs) = b.build_call(target, &[], &[clobber_reg()], 0)?;
        let clobber = outs[0];
        b.build_return(Some(clobber), &[])?;
        let mut fg = built(b, &[MALLOC])?;
        // A per-call convention declaring a return register this function does
        // not track: the declaration alone must not mint a base.
        let cc = fg.default_cc().clone();
        fg.set_call_cc(
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

    /// SOUNDNESS: a second return register the function tracks, when the first
    /// is not, leads the call's outputs.  It is the high half of a pair, not
    /// the pointer, so it must not become a heap base either.
    ///
    /// Mutation: accept any surviving return register.
    #[test]
    fn untracked_first_return_register_leaves_the_second_opaque() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(clobber_reg())
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let target = b.build_int_const(MALLOC, ValueType::int(64))?;
        let (call, outs) = b.build_call(target, &[], &[clobber_reg()], 0)?;
        let second = outs[0];
        b.build_return(Some(second), &[])?;
        let mut fg = built(b, &[MALLOC])?;
        let cc = fg.default_cc().clone();
        fg.set_call_cc(
            call,
            strider_target::BuiltCallingConvention {
                ret_val_regs: vec![ret_reg(), clobber_reg()],
                ..cc
            },
        );
        assert!(
            decompose(&fg, second).is_none(),
            "only the first declared return register carries the pointer"
        );
        Ok(())
    }

    /// SOUNDNESS: an allocator is still a callee, and a callee owns the
    /// outgoing stack-argument area the ABI hands it.  The i386 cdecl idiom
    /// `mov [esp], 16 / call malloc` puts the size argument in slot 0; malloc
    /// may scratch it, so a reload of that slot must not forward.
    #[test]
    fn allocator_does_not_forward_its_own_stack_argument_slot() -> crate::Result<()> {
        let sp = sp();
        let mut b = regs()
            .stack_args(stack_args_at(0, 8))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let slot0 = b.build_const_binop(0u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
        let size_arg = b.build_int_const(16u64, ValueType::int(64))?;
        b.build_store(slot0, size_arg, rsleigh::VnSpace::RAM)?;
        let _p = alloc_call(&mut b, MALLOC)?;
        let reload = b.build_load(slot0, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(reload), &[])?;
        let mut fg = built_collapsed(b, &[MALLOC])?;

        let load = fg.producer(reload);
        let policy = super::call_blocking(&mut fg, true);
        let clobber = super::nearest(&fg, load, policy);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
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
        let addr = b.build_const_binop(8u64, p, IntBinaryOp::Add, ValueType::int(64))?;
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
            let global = b.build_int_const(0x3000u64, ValueType::int(64))?;
            let stored = b.build_int_const(7u64, ValueType::int(64))?;
            b.build_store(global, stored, rsleigh::VnSpace::RAM)?;
            let target = b.build_int_const(0x2000u64, ValueType::int(64))?;
            let (call, _rets) = b.build_call(target, &[], &[ret_reg()], 0)?;
            let loaded = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(64))?;
            b.build_return(Some(loaded), &[])?;
            let mut fg = built(b, &[])?;

            let cc = fg.default_cc().clone();
            fg.set_call_cc(
                call,
                strider_target::BuiltCallingConvention {
                    preserves_memory,
                    ..cc
                },
            );

            let load = fg.producer(loaded);
            let policy = super::call_blocking(&mut fg, true);
            let clobber = super::nearest(&fg, load, policy);
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

    /// A `CallOther` reaches the memory chain only when its user-op ABI row
    /// declared `clobbers_memory`, and no convention describes that op:
    /// `get_cc` on one answers with the ANALYSED function's own convention.
    /// So a `preserves_memory` convention must not open the walk.
    #[test]
    fn call_other_clobbers_under_a_preserves_memory_convention() -> crate::Result<()> {
        let load_across_call_other = |preserves_memory: bool| -> crate::Result<bool> {
            let cc = strider_target::BuiltCallingConvention {
                arg_passing_regs: vec![sp()],
                arg_passing_regs_float: Vec::new(),
                callee_saved_regs: Vec::new(),
                ret_val_regs: vec![ret_reg()],
                ret_val_regs_float: Vec::new(),
                stack_ptr: sp(),
                stack_args: None,
                ret_stack_pop: 0,
                link_register: None,
                preserves_memory,
                preserves_all_registers: false,
                entry_address: None,
                reg_args_adjoin_stack_args: false,
            };
            let mut b = FunctionBuilder::new(
                vec![sp(), ret_reg()],
                cc,
                strider_target::Endianness::Little,
            )?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            let region = b.create_region_all()?;
            b.set_entry_region_all(region)?;
            b.set_region(region);

            let global = b.build_int_const(0x3000u64, ValueType::int(64))?;
            let stored = b.build_int_const(7u64, ValueType::int(64))?;
            b.build_store(global, stored, rsleigh::VnSpace::RAM)?;
            b.build_call_other(0, &[], &[], true, false)?;
            let loaded = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(64))?;
            b.build_return(Some(loaded), &[])?;
            let mut fg = built(b, &[])?;

            let load = fg.producer(loaded);
            let policy = super::call_blocking(&mut fg, true);
            let clobber = super::nearest(&fg, load, policy);
            Ok(matches!(fg.node_kind(clobber), NodeKind::CallOther { .. }))
        };

        for preserves_memory in [false, true] {
            assert!(
                load_across_call_other(preserves_memory)?,
                "a memory-clobbering user-op must stop the walk \
                 (preserves_memory={preserves_memory})"
            );
        }
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
        let mut b = regs()
            .stack_args(stack_args_at(8, 8))
            .build_fn_single_region()?;
        let sp = b.read_variable(&sp_vn)?;
        // +16: the second incoming stack argument, above the entry SP, so in
        // the caller's frame.
        let arg_slot = b.build_const_binop(16u64, sp, IntBinaryOp::Add, ValueType::int(64))?;
        let seed = b.build_int_const(7u64, ValueType::int(64))?;
        b.build_store(arg_slot, seed, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x2000u64, ValueType::int(64))?;
        b.build_call(target, &[], &[ret_reg()], 0)?;
        let loaded = b.build_load(arg_slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built_collapsed(b, &[])?;

        let load = fg.producer(loaded);
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
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
    /// `store_a` stores that constant through arm `a`'s allocation, where it is
    /// defined.
    fn alloc_phi_diamond(
        store_a: Option<u64>,
    ) -> crate::Result<(FunctionBuilder, ValueId, ValueId, ValueId)> {
        let sp = sp();
        let mut b = regs().build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let c = b.create_region_all()?;
        let join = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let sp_val = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        let cond = b.build_int_cmp_operation(
            sp_val,
            zero,
            strider_ir::IntCmpOp::Equal,
            ValueType::int(64),
        )?;
        b.build_if(cond, a, c)?;

        b.set_region(a);
        let pa = alloc_call(&mut b, MALLOC)?;
        if let Some(v) = store_a {
            let x = b.build_int_const(v, ValueType::int(64))?;
            b.build_store(pa, x, rsleigh::VnSpace::RAM)?;
        }
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
        let (mut b, phi, _pa, _pc) = alloc_phi_diamond(None)?;
        let x = b.build_int_const(0x11u64, ValueType::int(64))?;
        b.build_store(phi, x, rsleigh::VnSpace::RAM)?;
        let sp_val = b.read_variable(&sp())?;
        let slot = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
        let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built_collapsed(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
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
        let (mut b, phi, _pa, _pc) = alloc_phi_diamond(Some(0x11))?;
        let loaded = b.build_load(phi, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = built_collapsed(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let store = super::only_store(&fg);
        let load = fg.producer(loaded);
        let policy = super::call_blocking(&mut fg, false);
        assert_eq!(
            verdict(policy, &fg, load, store),
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
        let mut b = regs().build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let c = b.create_region_all()?;
        let join = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();

        b.set_region(entry);
        let sp_val = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        let cond = b.build_int_cmp_operation(
            sp_val,
            zero,
            strider_ir::IntCmpOp::Equal,
            ValueType::int(64),
        )?;
        b.build_if(cond, a, c)?;

        b.set_region(a);
        let pa = alloc_call(&mut b, MALLOC)?;
        b.write_variable(&ret_reg(), pa)?;
        b.build_branch(join)?;

        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let slot = b.build_const_binop(8u64, sp_c, IntBinaryOp::Add, ValueType::int(64))?;
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

    /// `p = malloc(); while p == 0 { p += 8 }; return p`, collapsed: the
    /// returned phi is the allocation's walking pointer, one of its own arms'
    /// operands.
    fn walking_pointer() -> crate::Result<(Function, ValueId)> {
        let mut b = regs().build_fn()?;
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
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        let cond =
            b.build_int_cmp_operation(phi, zero, strider_ir::IntCmpOp::Equal, ValueType::int(64))?;
        b.build_if(cond, body, exit)?;

        b.set_region(body);
        let next = b.build_const_binop(8u64, phi, IntBinaryOp::Add, ValueType::int(64))?;
        b.write_variable(&ret_reg(), next)?;
        b.build_branch(header)?;

        b.set_region(exit);
        b.build_return(Some(phi), &[])?;
        let fg = built_collapsed(b, &[MALLOC])?;
        Ok((fg, phi))
    }

    /// The frontier of a loop-carried pointer walk terminates and still sees
    /// only heap terminals.
    #[test]
    fn self_referential_phi_terminates() -> crate::Result<()> {
        let (fg, phi) = walking_pointer()?;
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
        let mut b = regs().build_fn()?;
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
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        let cond =
            b.build_int_cmp_operation(p0, zero, strider_ir::IntCmpOp::Equal, ValueType::int(64))?;
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
                    ValueType::int(64),
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

    /// Runs `LoadForward` over a function with two live allocations under the
    /// given allocator set.
    fn run_load_forward(mut b: FunctionBuilder, allocators: &[u64]) -> crate::Result<Function> {
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&super::allocating(allocators));
        run_passes(
            &mut fg,
            &[
                &crate::PhiCollapse,
                &crate::RegionCollapse,
                &crate::LoadForward,
            ],
        )?;
        Ok(fg)
    }

    /// The memory-edge rewire honours heap identity but no call relaxation:
    /// the edge steps past the store to the OTHER allocation and stops at the
    /// call that made it.
    #[test]
    fn narrowing_carries_the_allocator_set_up_to_the_call() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let q = alloc_call(&mut b, KMEM_CACHE_ALLOC)?;
        let two = b.build_int_const(2u64, ValueType::int(64))?;
        b.build_store(q, two, rsleigh::VnSpace::RAM)?;
        // No store to `p`'s object, so the load survives to be inspected.
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let fg = run_load_forward(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let clobber = fg.producer(fg.node_inputs(load)[0]);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "the load must narrow past the store to the other allocation onto \
             its call, got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// The forwarding half still reads the set: the load steps past a store to
    /// a DIFFERENT allocation and takes its own object's value.
    #[test]
    fn forwarding_reads_the_allocator_set() -> crate::Result<()> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let q = alloc_call(&mut b, KMEM_CACHE_ALLOC)?;
        let one = b.build_int_const(1u64, ValueType::int(64))?;
        b.build_store(p, one, rsleigh::VnSpace::RAM)?;
        let two = b.build_int_const(2u64, ValueType::int(64))?;
        b.build_store(q, two, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        let fg = run_load_forward(b, &[MALLOC, KMEM_CACHE_ALLOC])?;

        let ret = fg.returned();
        assert_eq!(
            fg.int_const_u128(ret),
            Some(1),
            "the load must forward past the store to the other allocation"
        );
        Ok(())
    }

    /// One store through a `MALLOC` allocation, unoptimized.
    fn one_heap_store() -> crate::Result<Function> {
        let mut b = builder()?;
        let p = alloc_call(&mut b, MALLOC)?;
        let one = b.build_int_const(1u64, ValueType::int(64))?;
        b.build_store(p, one, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()
    }

    /// How many stores `store().heap_only()` matches.
    fn heap_stores(fg: &Function) -> usize {
        strider_pattern::Matcher::new(fg)
            .find_all(&strider_pattern::store().heap_only().build())
            .expect("the query compiles")
            .len()
    }

    /// `apply_rules_count` re-derives the memory classes under the function's
    /// allocator set, so a heap store stays `heap_only`, also after the
    /// compaction `analyze` ends with and a clone.
    #[test]
    fn a_rewrite_keeps_the_runs_heap_classes() -> crate::Result<()> {
        let mut fg = one_heap_store()?;
        fg.assume(&super::allocating(&[MALLOC]));
        crate::default_pipeline().run(&mut fg, &crate::OptCtx::new(None))?;
        assert_eq!(heap_stores(&fg), 1, "the run classifies the store as heap");
        fg.compact();
        let mut fg = fg.clone();
        let no_rules: &[crate::BoxedRule] = &[];
        crate::apply_rules_count(&mut crate::EditFunction::new(&mut fg), no_rules)?;
        assert_eq!(
            heap_stores(&fg),
            1,
            "a rewrite keeps the function's allocator set"
        );
        Ok(())
    }

    /// Claims accumulate: a later run without the allocator keeps the claim
    /// in force, so the store stays heap.
    #[test]
    fn a_later_run_without_the_allocator_keeps_its_heap_classes() -> crate::Result<()> {
        let mut fg = one_heap_store()?;
        fg.assume(&super::allocating(&[MALLOC]));
        crate::default_pipeline().run(&mut fg, &crate::OptCtx::new(None))?;
        assert_eq!(
            heap_stores(&fg),
            1,
            "the listed run classifies the store as heap"
        );
        crate::default_pipeline().run(&mut fg, &crate::OptCtx::new(None))?;
        assert_eq!(heap_stores(&fg), 1, "a later run withdraws no claim");
        assert!(
            fg.assumptions().noalias_allocators.contains(&MALLOC),
            "the allocator stays listed"
        );
        Ok(())
    }

    /// Each policy honours exactly the claims its constructor names, read off
    /// six verdicts: a spill
    /// reloaded across an unlisted call and one across a listed allocator,
    /// two allocations, a stack slot against a global, an entry-SP slot
    /// against an alignment-masked one, and, in a second function whose ABI
    /// passes stack arguments, the allocator's argument slot reloaded.
    #[test]
    fn each_purpose_honours_its_own_claims() -> crate::Result<()> {
        use crate::AssumptionOptions;
        let sp_slot = |b: &mut FunctionBuilder, sp_val, off: i64| -> crate::Result<ValueId> {
            let off = b.build_int_const(off as u64, ValueType::int(64))?;
            b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::int(64))
        };
        let mut b = builder()?;
        let sp_val = b.read_variable(&sp())?;
        let slot = sp_slot(&mut b, sp_val, -8)?;
        let seven = b.build_int_const(7u64, ValueType::int(64))?;
        b.build_store(slot, seven, rsleigh::VnSpace::RAM)?;
        lower_sp(&mut b, sp_val, 64)?;
        let unlisted = b.build_int_const(0x2000u64, ValueType::int(64))?;
        b.build_call(unlisted, &[], &[ret_reg()], 0)?;
        let reload = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let local = sp_slot(&mut b, sp_val, -16)?;
        let nine = b.build_int_const(9u64, ValueType::int(64))?;
        b.build_store(local, nine, rsleigh::VnSpace::RAM)?;
        let p = alloc_call(&mut b, MALLOC)?;
        let local_reload = b.build_load(local, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let q = alloc_call(&mut b, MALLOC)?;
        let two = b.build_int_const(2u64, ValueType::int(64))?;
        b.build_store(q, two, rsleigh::VnSpace::RAM)?;
        let from_p = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let global = b.build_int_const(0x9000u64, ValueType::int(64))?;
        let from_global = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let aligned = b.build_const_binop(
            (-16i64) as u64,
            sp_val,
            IntBinaryOp::And,
            ValueType::int(64),
        )?;
        let aligned_slot = sp_slot(&mut b, aligned, 8)?;
        let five = b.build_int_const(5u64, ValueType::int(64))?;
        b.build_store(aligned_slot, five, rsleigh::VnSpace::RAM)?;
        let incoming = sp_slot(&mut b, sp_val, 8)?;
        let from_incoming = b.build_load(incoming, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let mut sum = reload;
        for v in [local_reload, from_p, from_global, from_incoming] {
            sum = b.build_int_binary_operation(sum, v, IntBinaryOp::Add, ValueType::int(64))?;
        }
        b.build_return(Some(sum), &[])?;
        let fg = built_collapsed(b, &[])?;

        // An earlier call on the chain would blind the allocator's argument
        // window, so its argument slot gets a function of its own.
        let mut b = regs()
            .stack_args(stack_args_at(0, 8))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp())?;
        lower_sp(&mut b, sp_val, 64)?;
        // Slot 0 of the allocator's stack arguments.
        let arg_slot = sp_slot(&mut b, sp_val, -64)?;
        let eleven = b.build_int_const(11u64, ValueType::int(64))?;
        b.build_store(arg_slot, eleven, rsleigh::VnSpace::RAM)?;
        alloc_call(&mut b, MALLOC)?;
        let arg_reload = b.build_load(arg_slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(arg_reload), &[])?;
        let fa = built_collapsed(b, &[])?;

        let store_of = |fg: &Function, data: u128| {
            fg.graph()
                .all_node_ids()
                .find(|&n| {
                    matches!(fg.node_kind(n), NodeKind::Store(_))
                        && fg.int_const_u128(fg.store_data(n)) == Some(data)
                })
                .expect("the store")
        };
        let (spill, local_spill, to_q, to_aligned) = (
            store_of(&fg, 7),
            store_of(&fg, 9),
            store_of(&fg, 2),
            store_of(&fg, 5),
        );
        let [reload, local_reload, from_p, from_global, from_incoming] =
            [reload, local_reload, from_p, from_global, from_incoming].map(|v| fg.producer(v));
        let (arg_spill, arg_reload) = (store_of(&fa, 11), fa.producer(arg_reload));
        let forwards = |fg: &Function, policy: Policy, load: NodeId, spill: NodeId| {
            MemQuery::default().reaching(fg, load, policy).node() == spill
        };
        let none = AssumptionOptions::none;
        let toggles = [
            none(),
            AssumptionOptions {
                stack_global_disjoint: true,
                ..none()
            },
            AssumptionOptions {
                assume_incoming_args_survive_calls: true,
                ..none()
            },
            AssumptionOptions {
                distinct_sp_bases_disjoint: true,
                ..none()
            },
            AssumptionOptions {
                callee_preserves_stack_args: true,
                ..none()
            },
            AssumptionOptions {
                noalias_allocators: [MALLOC].into_iter().collect(),
                ..none()
            },
            AssumptionOptions {
                escape_analysis: true,
                ..none()
            },
            AssumptionOptions {
                return_slots_survive_untracked_writes: true,
                ..none()
            },
            // `callee_preserves_stack_args` is inert without a relaxation
            // that consults the argument window.
            AssumptionOptions {
                noalias_allocators: [MALLOC].into_iter().collect(),
                callee_preserves_stack_args: true,
                ..none()
            },
        ];
        for a in &toggles {
            // Claims only accumulate, so each toggle reads its own copy.
            let (mut fg, mut fa) = (fg.clone(), fa.clone());
            for f in [&mut fg, &mut fa] {
                f.assume(a);
            }
            type PolicyOf = fn(&Function) -> Policy;
            let policies: [(&str, PolicyOf); 3] = [
                ("forward", Policy::forward),
                ("incoming_args", Policy::incoming_args),
                ("strict", Policy::strict),
            ];
            for (purpose, policy_of) in policies {
                let (policy, policy_a) = (policy_of(&fg), policy_of(&fa));
                let heap = verdict(policy, &fg, from_p, to_q) == AliasVerdict::Disjoint;
                let global = verdict(policy, &fg, from_global, spill) == AliasVerdict::Disjoint;
                let sp_bases =
                    verdict(policy, &fg, from_incoming, to_aligned) == AliasVerdict::Disjoint;
                let no_call_blocks =
                    purpose == "incoming_args" && a.assume_incoming_args_survive_calls;
                let escapes = purpose == "forward" && a.escape_analysis;
                let allocator = purpose == "forward" && !a.noalias_allocators.is_empty();
                assert_eq!(
                    forwards(&fg, policy, reload, spill),
                    escapes || no_call_blocks,
                    "{purpose} forwarding across the call under {a:?}"
                );
                assert_eq!(
                    forwards(&fg, policy, local_reload, local_spill),
                    allocator || escapes || no_call_blocks,
                    "{purpose} forwarding across the allocator under {a:?}"
                );
                assert_eq!(
                    forwards(&fa, policy_a, arg_reload, arg_spill),
                    (purpose == "forward"
                        && a.callee_preserves_stack_args
                        && (allocator || escapes))
                        || no_call_blocks,
                    "{purpose} forwarding the allocator's argument slot under {a:?}"
                );
                assert_eq!(
                    sp_bases,
                    purpose == "incoming_args" && a.distinct_sp_bases_disjoint,
                    "{purpose} distinct-SP-base disjointness under {a:?}"
                );
                assert_eq!(
                    heap,
                    !a.noalias_allocators.is_empty(),
                    "{purpose} heap disjointness under {a:?}"
                );
                assert_eq!(
                    global, a.stack_global_disjoint,
                    "{purpose} stack-global disjointness under {a:?}"
                );
            }
        }
        Ok(())
    }
}

/// The window is placed by adding an ABI-declared offset to a decomposed SP,
/// neither of which this crate bounds.
mod arg_window_bounds {
    use super::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{IrWalkerEx, sp_frame, stack_args_at};

    fn saturating_args() -> Option<strider_target::StackArgs> {
        stack_args_at(i128::MAX, 8)
    }

    /// The first slot sits past the carrier, so placing the window saturates
    /// instead of overflowing.
    #[test]
    fn a_window_base_past_the_carrier_saturates() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(saturating_args())
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // A call SP ABOVE the entry SP, so the declared base offset is added to
        // a positive displacement.
        let call_sp = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // Collapses `read_variable(sp)` to the bare `InitialVar(sp)` the call's
        // SP has to decompose to for the window to be placed at all.
        collapse_phis(&mut fg);

        let call = fg.sole_kind(|k| matches!(k, NodeKind::Call { .. }));
        let query = MemQuery::default();
        let policy = super::call_blocking(&mut fg, false);
        let MemExpr {
            base, offset: 8, ..
        } = decompose(&fg, fg.node_inputs(call)[3]).expect("the call's SP is the entry SP plus 8")
        else {
            panic!("the call's SP is the entry SP plus 8")
        };
        let stores = StoreIndex::build(&fg, &crate::mem_ssa::MemLayout::build(&fg));
        let walker = MemWalker {
            query: &query,
            policy,
            loc: Loc {
                class: AddrClass::StackRooted { base, offset: -8 },
                size: 4,
                bits: 32,
                space: rsleigh::VnSpace::RAM,
            },
            stores: &stores,
            private_frame: std::cell::OnceCell::new(),
        };
        assert!(
            walker.in_outgoing_arg_area(&fg, call),
            "a window that starts past the carrier covers everything below it"
        );
        Ok(())
    }

    /// Same bound one level in: the prefix scan steps slot offsets off the same
    /// SP.
    #[test]
    fn a_slot_offset_past_the_carrier_saturates() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(saturating_args())
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let one = b.build_int_const(1u64, ValueType::int(32))?;
        b.build_store(sp_val, one, rsleigh::VnSpace::RAM)?;
        let call_sp = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let call = fg.sole_kind(|k| matches!(k, NodeKind::Call { .. }));
        let prefix = ArgPrefix::scan(&fg, &MemQuery::default(), call)
            .expect("the call's SP is the entry SP plus 8");
        assert!(
            !prefix.covers(-8, -4),
            "a window whose first slot saturates owns nothing below it"
        );
        Ok(())
    }
}

/// The outgoing-argument window and `CallStackArgCollect` step the same
/// argument-slot prefix off the same `StackArgs`, which
/// [`MemWalker::in_outgoing_arg_area`] states but nothing checked.
mod window_matches_collection {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_frame, stack_args_at};

    /// Two argument pushes behind an earlier call, which hides the slot above
    /// them: the window keeps both anchored slots and everything from the
    /// hidden one up.
    #[test]
    fn a_blinded_prefix_keeps_the_slots_below_the_blind() -> crate::Result<()> {
        use strider_ir::IRWalker;
        let sp = strider_ir_test_utils::stack_vn_x86();
        let args = stack_args_at(0, 4).expect("a declared stack-arg layout");
        let mut b = sp_frame(sp)
            .stack_args(Some(args))
            .build_fn_single_region()?;
        let sp0 = b.read_variable(&sp)?;
        let blinder = b.build_int_const(0x2000u64, ValueType::int(32))?;
        b.build_call(blinder, &[], &[], 0)?;
        for k in 0..2u64 {
            let addr = b.build_const_binop(k * 4, sp0, IntBinaryOp::Add, ValueType::int(32))?;
            let value = b.build_int_const(0x10 + k, ValueType::int(32))?;
            b.build_store(addr, value, rsleigh::VnSpace::RAM)?;
        }
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let call = fg
            .walk_kind(|k| matches!(k, NodeKind::Call { .. }))
            .find(|&c| {
                fg.int_const_u128(fg.node_inputs(c)[NodeKind::CALL_TARGET_SLOT]) == Some(0x1000)
            })
            .expect("the probed call");
        let window =
            ArgPrefix::scan(&fg, &MemQuery::default(), call).expect("the window is placed");
        for k in 0..4 {
            assert!(
                window.covers(k * 4, k * 4 + 4),
                "slot {k} is an argument or past the blind, so the callee owns it"
            );
        }
        Ok(())
    }

    /// Two argument pushes and a spill in the slot right above them.  Once
    /// lowered to memory the spill is indistinguishable from a third argument,
    /// so BOTH prefixes have to take it: a window ending below the collection
    /// would let a load forward out of a slot the callee owns.
    #[test]
    fn a_spill_above_the_arguments_ends_both_prefixes_alike() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let args = stack_args_at(0, 4).expect("a declared stack-arg layout");
        let mut b = sp_frame(sp)
            .stack_args(Some(args))
            .build_fn_single_region()?;
        let sp0 = b.read_variable(&sp)?;
        for k in 0..3u64 {
            let addr = b.build_const_binop(k * 4, sp0, IntBinaryOp::Add, ValueType::int(32))?;
            let value = b.build_int_const(0x10 + k, ValueType::int(32))?;
            b.build_store(addr, value, rsleigh::VnSpace::RAM)?;
        }
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call_cc(target, None)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        let mut pipeline = crate::test_support::pipeline(crate::test_support::CF_RP);
        pipeline.add_post_pass(&crate::CallStackArgCollect);
        pipeline.run(&mut fg, &crate::OptCtx::new(None))?;

        let call = fg.sole_kind(|k| matches!(k, NodeKind::Call { .. }));
        let collected = fg.node_inputs(call).len() - NodeKind::Call { cc: None }.input_head_len();
        assert_eq!(collected, 3, "the spill is collected as one more argument");

        let sp_value = fg.node_inputs(call)[NodeKind::CALL_SP_SLOT];
        let MemExpr {
            offset: sp_offset, ..
        } = decompose(&fg, sp_value).expect("the call's SP is the entry SP");
        let slot = |k: i128| sp_offset + args.base_offset + k * args.increment;
        let window =
            ArgPrefix::scan(&fg, &MemQuery::default(), call).expect("the window is placed");
        for k in 0..collected as i128 {
            assert!(
                window.covers(slot(k), slot(k + 1)),
                "slot {k} is collected, so the callee owns it"
            );
        }
        assert!(
            !window.covers(slot(collected as i128), slot(collected as i128 + 1)),
            "the window ends where the collection stops, not above it"
        );
        Ok(())
    }
}

/// The outgoing-argument window is computed from inside a memory-SSA walk, so
/// it must not start a walk that can re-enter it.
mod arg_window_complexity {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_frame, stack_args_at};

    /// A spill at `sp - 8`, `calls` back-to-back calls under a lowered SP, then
    /// its reload.  The nearest call's window scan runs into the call before
    /// it, so the reload stops there; the cost of getting that answer must not
    /// grow with the call count.
    fn walk_steps_for(calls: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let slot =
            b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let v = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(slot, v, rsleigh::VnSpace::RAM)?;
        let call_sp = b.build_const_binop(
            (-64i64) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        for i in 0..calls {
            let target = b.build_int_const(0x1000 + i as u64 * 0x10, ValueType::int(32))?;
            b.build_call(target, &[], &[], 0)?;
        }
        let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        crate::mem_analysis::collapse_phis(&mut fg);

        let load = fg.producer(loaded);
        let query = MemQuery::default();
        let policy = super::escaping(&mut fg, false);
        WALK_STEPS.with(|c| c.set(0));
        let clobber = query.reaching(&fg, load, policy).node();
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "the window scan cannot see past the call before the nearest one, \
             so the nearest one keeps the slot"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the call count may only double the work.
    #[test]
    fn window_probe_cost_is_linear_in_call_count() -> crate::Result<()> {
        crate::test_support::assert_near_linear("calls", 12, walk_steps_for, 260)
    }

    /// `slots` stack-argument stores below a spill, all under a lowered SP, one
    /// call, then the spill's reload.  The reload probes the call's argument
    /// window, which walks the prefix of argument slots, so the cost of one
    /// window computation is measured against the number of slots in it.
    fn walk_steps_for_arg_slots(slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // The frame is one slot deeper than the arguments need, so the spill at
        // `sp - 4` is the window's last slot rather than the caller's memory.
        let frame_bytes = 4 * (slots as i64 + 1);
        let spill_slot =
            b.build_const_binop((-4i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let spilled = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(spill_slot, spilled, rsleigh::VnSpace::RAM)?;
        for i in 0..slots {
            let slot = b.build_const_binop(
                (4 * i as i64 - frame_bytes) as u64,
                sp_val,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?;
            let arg = b.build_int_const(0x100 + i as u64, ValueType::int(32))?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
        }
        let call_sp = b.build_const_binop(
            (-frame_bytes) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        let loaded = b.build_load(spill_slot, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let query = MemQuery::default();
        let policy = super::escaping(&mut fg, false);
        WALK_STEPS.with(|c| c.set(0));
        let clobber = query.reaching(&fg, load, policy).node();
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "the spill sits one slot above the last argument, so the window \
             reaches it and the call clobbers"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the slot count may only double the work: one reverse scan of
    /// the memory chain answers every slot, rather than one scan per slot.
    #[test]
    fn window_probe_cost_is_linear_in_stack_arg_slots() -> crate::Result<()> {
        crate::test_support::assert_near_linear(
            "argument slots",
            100,
            walk_steps_for_arg_slots,
            260,
        )
    }

    /// `slots` argument stores under a lowered SP, a call below them that the
    /// relaxation-free window probe cannot see past, then `loads` distinct
    /// reloads of slots above the argument stores.  Every reload asks the same
    /// call the same question, so the whole prefix walk must be paid once.
    fn walk_steps_for_loads(loads: usize, slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Blinds the prefix at the first slot with no store, so the window
        // reaches every reload instead of ending below them.
        let blinder = b.build_int_const(0x2000u64, ValueType::int(32))?;
        b.build_call(blinder, &[], &[], 0)?;
        // The reloads occupy the `loads` slots just above the arguments.
        let frame_bytes = 4 * (slots + loads + 1) as i64;
        for j in 0..slots {
            let slot = b.build_const_binop(
                (4 * j as i64 - frame_bytes) as u64,
                sp_val,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?;
            let arg = b.build_int_const(0x100 + j as u64, ValueType::int(32))?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
        }
        let call_sp = b.build_const_binop(
            (-frame_bytes) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        let mut sum = None;
        for i in 0..loads {
            let addr = b.build_const_binop(
                (-4 * (i as i64 + 1)) as u64,
                sp_val,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?;
            let v = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
            sum = Some(match sum {
                None => v,
                Some(acc) => {
                    b.build_int_binary_operation(acc, v, IntBinaryOp::Add, ValueType::int(32))?
                }
            });
        }
        b.build_return(sum, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        fg.assume(&crate::AssumptionOptions {
            escape_analysis: true,
            ..crate::AssumptionOptions::none()
        });
        WALK_STEPS.with(|c| c.set(0));
        let result = crate::test_support::run_pass(&crate::LoadForward, &mut fg)?;
        assert!(
            !result,
            "the window covers every reload, so the call keeps all of them"
        );
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// `slots` argument stores under a lowered SP, one call, then a reload of
    /// every one of those slots, probed in ascending order through one
    /// query.  Each probe reaches one slot further than the last; the total
    /// cost of the run is what this measures.
    fn walk_steps_for_ascending_probes(slots: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let frame_bytes = 4 * slots as i64;
        let mut slot_addrs = Vec::with_capacity(slots);
        for i in 0..slots {
            let slot = b.build_const_binop(
                (4 * i as i64 - frame_bytes) as u64,
                sp_val,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?;
            let arg = b.build_int_const(0x100 + i as u64, ValueType::int(32))?;
            b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
            slot_addrs.push(slot);
        }
        let call_sp = b.build_const_binop(
            (-frame_bytes) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        let mut reloads = Vec::with_capacity(slots);
        let mut sum = None;
        for &addr in &slot_addrs {
            let v = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
            reloads.push(v);
            sum = Some(match sum {
                None => v,
                Some(acc) => {
                    b.build_int_binary_operation(acc, v, IntBinaryOp::Add, ValueType::int(32))?
                }
            });
        }
        b.build_return(sum, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let query = MemQuery::default();
        let policy = super::escaping(&mut fg, false);
        WALK_STEPS.with(|c| c.set(0));
        for value in reloads {
            let load = fg.producer(value);
            let clobber = query.reaching(&fg, load, policy).node();
            assert!(
                matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
                "an argument slot is the callee's, so the call keeps it"
            );
        }
        Ok(WALK_STEPS.with(std::cell::Cell::get))
    }

    /// Doubling the length of an ascending run of probes may only double the
    /// work: the prefix is scanned once, whatever the probe reaches.
    #[test]
    fn window_probe_cost_is_linear_in_ascending_probes() -> crate::Result<()> {
        crate::test_support::assert_near_linear(
            "ascending probes",
            100,
            walk_steps_for_ascending_probes,
            260,
        )
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

/// The argument window of a call behind an irreducible memory graph.
mod arg_window_irreducible {
    use super::*;

    fn window_steps(k: usize) -> crate::Result<u64> {
        let mut fg = crate::test_support::memory_shapes::irreducible_stack_args(k)?;
        assert!(
            !crate::mem_ssa::MemLayout::build(&fg).is_reducible(),
            "the memory graph is irreducible"
        );
        fg.assume(&crate::AssumptionOptions {
            escape_analysis: true,
            ..crate::AssumptionOptions::none()
        });
        crate::test_support::steps(&[&WALK_STEPS, &crate::mem_ssa::CLIMB_STEPS], || {
            let changed = crate::test_support::run_pass(&crate::LoadForward, &mut fg)?;
            assert!(
                !changed,
                "the reload's slot is the callee's, so the call keeps it"
            );
            Ok(())
        })
    }

    /// Doubling the argument count may only double the work of reading the
    /// window.
    #[test]
    fn window_probe_cost_is_linear_in_the_slot_count() -> crate::Result<()> {
        crate::test_support::assert_near_linear("arguments", 100, window_steps, 260)
    }

    /// The same behind a merge whose arms disagree on the reloaded slot, which
    /// every argument slot below it reads past by its own climb.
    #[test]
    fn window_probe_behind_a_merge_is_linear_in_the_slot_count() -> crate::Result<()> {
        crate::test_support::assert_near_linear(
            "arguments",
            100,
            |k| {
                let mut fg =
                    crate::test_support::memory_shapes::irreducible_stack_args_merging(k, Some(k))?;
                fg.assume(&crate::AssumptionOptions {
                    escape_analysis: true,
                    ..crate::AssumptionOptions::none()
                });
                crate::test_support::steps(&[&WALK_STEPS, &crate::mem_ssa::CLIMB_STEPS], || {
                    let changed = crate::test_support::run_pass(&crate::LoadForward, &mut fg)?;
                    assert!(!changed, "the arms disagree on the reloaded slot");
                    Ok(())
                })
            },
            260,
        )
    }
}

/// `decompose` walks the whole address spine, so it must commit a verdict for
/// every node it passes through, not just the query root.
mod spine_memo {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::sp_arg_frame;

    /// `sp - 4 - 4 - ...`, `len` links deep, left un-flattened (`ConstantFold`
    /// would collapse it).  The returned chain runs shallowest link first.
    fn add_chain(len: usize) -> crate::Result<(strider_ir::Function, Vec<ValueId>)> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_arg_frame(sp).build_fn_single_region()?;
        let mut cur = b.read_variable(&sp)?;
        let mut chain = Vec::with_capacity(len);
        for _ in 0..len {
            let minus_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::int(32))?;
            cur = b.build_int_binary_operation(
                cur,
                minus_four,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?;
            chain.push(cur);
        }
        b.build_return(Some(cur), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
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

    /// `levels` alignment masks, each over the last one less 4, queried from
    /// the outermost in: each query's spine runs through every mask below it.
    fn nested_mask_steps(levels: usize) -> crate::Result<u64> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_arg_frame(sp).build_fn_single_region()?;
        let mut cur = b.read_variable(&sp)?;
        let mut chain = Vec::with_capacity(levels);
        for _ in 0..levels {
            let below =
                b.build_const_binop(0xFFFF_FFFCu64, cur, IntBinaryOp::Add, ValueType::int(32))?;
            let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::int(32))?;
            cur =
                b.build_int_binary_operation(below, mask, IntBinaryOp::And, ValueType::int(32))?;
            chain.push(cur);
        }
        b.build_return(Some(cur), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        SPINE_STEPS.with(|c| c.set(0));
        for v in chain.iter().rev() {
            assert!(
                matches!(
                    decompose(&fg, *v),
                    Some(MemExpr { base, offset: 0, kind: MemKind::Stack }) if base == *v
                ),
                "each mask is its own base"
            );
        }
        Ok(SPINE_STEPS.with(std::cell::Cell::get))
    }

    #[test]
    fn decomposing_nested_masks_outermost_first_is_not_quadratic() -> crate::Result<()> {
        crate::test_support::assert_near_linear("masks", 100, nested_mask_steps, 260)
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
mod arg_window_visibility {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_frame, stack_args_at};

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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;

        let slot0 = b.build_const_binop(0u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let slot4 = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;

        let p = b.build_int_const(0x11u64, ValueType::int(32))?;
        b.build_store(slot0, p, rsleigh::VnSpace::RAM)?;
        let f = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(f, &[], &[], 0)?;

        let q = b.build_int_const(0x22u64, ValueType::int(32))?;
        b.build_store(slot4, q, rsleigh::VnSpace::RAM)?;
        let g = b.build_int_const(0x2000u64, ValueType::int(32))?;
        b.build_call(g, &[], &[], 0)?;

        let loaded = b.build_load(slot4, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mut addrs = Vec::new();
        for off in [-12i64, -16, -14] {
            let k = b.build_int_const(off as u64, ValueType::int(32))?;
            addrs.push(b.build_int_binary_operation(
                sp_val,
                k,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?);
        }
        for (i, addr) in addrs.iter().enumerate() {
            let data = b.build_int_const(0x11 * (i as u64 + 1), ValueType::int(32))?;
            b.build_store(*addr, data, rsleigh::VnSpace::RAM)?;
        }
        let slot4 = addrs[0];
        let call_sp = b.build_const_binop(
            (-16i64) as u64,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        let loaded = b.build_load(slot4, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-64i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;

        let arg0 = b.build_const_binop(4u64, call_sp, IntBinaryOp::Add, ValueType::int(32))?;
        let arg1 = b.build_const_binop(8u64, call_sp, IntBinaryOp::Add, ValueType::int(32))?;

        let p = b.build_int_const(0x11u64, ValueType::int(32))?;
        b.build_store(arg0, p, rsleigh::VnSpace::RAM)?;
        let f = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(f, &[], &[], 0)?;

        let q = b.build_int_const(0x22u64, ValueType::int(32))?;
        b.build_store(arg1, q, rsleigh::VnSpace::RAM)?;
        let g = b.build_int_const(0x2000u64, ValueType::int(32))?;
        b.build_call(g, &[], &[], 0)?;

        let loaded = b.build_load(arg1, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "the earlier call hides slot 0, so the window cannot be shown to \
             end below sp'+8, which g owns; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// The relaxation's motivating shape: whether the reload in
    /// [`crate::test_support::memory_shapes::stack_top_spill`] has the spill as
    /// its nearest clobber.
    fn spill_at_the_stack_top_forwards(relaxed: bool) -> crate::Result<bool> {
        let (b, loaded) = crate::test_support::memory_shapes::stack_top_spill()?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, relaxed);
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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let local = b.build_const_binop(
            (-8i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        let spilled = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(local, spilled, rsleigh::VnSpace::RAM)?;

        let frame = b.build_int_const((-64i64) as u64, ValueType::int(32))?;
        let call_sp =
            b.build_int_binary_operation(entry_sp, frame, IntBinaryOp::Add, ValueType::int(32))?;
        b.write_variable(&sp, call_sp)?;
        let f = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(local, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;

        let mut slots = Vec::new();
        for off in [-16i64, -12, -8] {
            let k = b.build_int_const(off as u64, ValueType::int(32))?;
            slots.push(b.build_int_binary_operation(
                entry_sp,
                k,
                IntBinaryOp::Add,
                ValueType::int(32),
            )?);
        }
        for (i, slot) in slots.iter().enumerate() {
            let arg = b.build_int_const(0x10 * (i as u64 + 1), ValueType::int(32))?;
            b.build_store(*slot, arg, rsleigh::VnSpace::RAM)?;
        }

        let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
        let opaque = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let junk = b.build_int_const(0x99u64, ValueType::int(32))?;
        b.build_store(opaque, junk, rsleigh::VnSpace::RAM)?;

        let call_sp = b.build_const_binop(
            (-16i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let f = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(slots[2], rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "sp-8 is the callee's third outgoing argument slot, which the \
             opaque store hides rather than disproves; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// An argument narrower than its ABI slot still hands the callee the whole
    /// slot.  A window built from STORE extents rather than slot extents leaves
    /// the slot's tail out, so a probe there reads as "not an argument" and the
    /// load forwards across a callee that owns those bytes.
    ///
    /// ```text
    /// sp' = sp - 32        ; x86-64 SysV, base_offset 8 / increment 8
    /// store.4 A -> sp'+8   ; `mov [rsp+8], eax`, a 4-byte 7th integer argument
    /// call f
    /// load.4  sp'+12       ; the tail of that same slot
    /// ```
    #[test]
    fn a_sub_slot_argument_owns_its_whole_slot() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86_64();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(8, 8))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-32i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.write_variable(&sp, call_sp)?;

        let slot0 = b.build_const_binop(8u64, call_sp, IntBinaryOp::Add, ValueType::int(64))?;
        let arg = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(slot0, arg, rsleigh::VnSpace::RAM)?;

        let f = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.build_call(f, &[], &[], 0)?;

        let tail = b.build_const_binop(12u64, call_sp, IntBinaryOp::Add, ValueType::int(64))?;
        let loaded = b.build_load(tail, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "the upper 4 bytes of argument slot 0 are the callee's, so the \
             load must stop at the call; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// A big-endian argument narrower than its slot is right-justified in it,
    /// so the caller never writes the slot's first byte.  The slot is still the
    /// callee's, and so is every slot above it.
    ///
    /// ```text
    /// sp' = sp - 32        ; base_offset 8 / increment 8
    /// store.4 A -> sp'+12  ; PPC64 `stfs`, right-justified in slot 0
    /// call f
    /// load.4  sp'+12
    /// ```
    ///
    /// Mutation: anchor only a store covering the slot's first byte.
    #[test]
    fn a_right_justified_sub_slot_argument_owns_its_slot() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86_64();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(8, 8))
            .byte_order(strider_target::Endianness::Big)
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-32i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.write_variable(&sp, call_sp)?;

        let arg_at = b.build_const_binop(12u64, call_sp, IntBinaryOp::Add, ValueType::int(64))?;
        let arg = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(arg_at, arg, rsleigh::VnSpace::RAM)?;

        let f = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.build_call(f, &[], &[], 0)?;

        let loaded = b.build_load(arg_at, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "argument slot 0 is the callee's, so the load must stop at the \
             call; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// The same right-justified argument behind a merge that only one path
    /// writes the next slot on: the per-slot probe past the merge finds slot
    /// 0 unwritten at its first byte and must look at the rest of it.
    ///
    /// ```text
    /// sp' = sp - 32
    /// store.4 A -> sp'+12  ; slot 0, right-justified
    /// if (..) store.8 B -> sp'+16   ; slot 1 on one path
    /// call f
    /// load.8  sp'+16
    /// ```
    ///
    /// Mutation: probe only the slot's first byte past the merge.
    #[test]
    fn a_right_justified_argument_behind_a_merge_owns_its_slot() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86_64();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(8, 8))
            .byte_order(strider_target::Endianness::Big)
            .build_fn()?;
        let (entry, then_r, merge) = (
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
        );
        b.set_entry_region_all(entry)?;
        b.set_region(entry);
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-32i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.write_variable(&sp, call_sp)?;
        let at = |b: &mut strider_ir::FunctionBuilder, off: u64| {
            let k = b.build_int_const(off, ValueType::int(64))?;
            b.build_int_binary_operation(call_sp, k, IntBinaryOp::Add, ValueType::int(64))
        };
        let slot0_tail = at(&mut b, 12)?;
        let a = b.build_int_const(0xAu64, ValueType::int(32))?;
        b.build_store(slot0_tail, a, rsleigh::VnSpace::RAM)?;
        let slot1 = at(&mut b, 16)?;
        let cond = b.build_boolean_const(true);
        b.build_if(cond, then_r, merge)?;

        b.set_region(then_r);
        let bv = b.build_int_const(0xBu64, ValueType::int(64))?;
        b.build_store(slot1, bv, rsleigh::VnSpace::RAM)?;
        b.build_branch(merge)?;

        b.set_region(merge);
        let f = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.build_call(f, &[], &[], 0)?;
        let loaded = b.build_load(slot1, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert!(
            matches!(fg.node_kind(clobber), NodeKind::Call { .. }),
            "slot 0 holds an argument, so slot 1 is the callee's too; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }

    /// Stores inside a slot whose first byte an earlier call hides still
    /// leave the window blind from the slot up: one of them reaching into
    /// the next slot is no argument ending there.
    ///
    /// ```text
    /// sp' = sp - 32        ; base_offset 8 / increment 8
    /// call g
    /// store.4 A -> sp'+10  ; inside slot 0
    /// store.4 B -> sp'+14  ; inside slot 0, into slot 1
    /// call f
    /// load.8  sp'+16
    /// ```
    ///
    /// Mutation: read a slot an inside store anchors as anchored whatever
    /// hides its first byte.
    #[test]
    fn inside_stores_past_a_hidden_first_byte_keep_the_window_blind() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86_64();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(8, 8))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-32i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.write_variable(&sp, call_sp)?;
        let at = |b: &mut strider_ir::FunctionBuilder, off: u64| {
            let k = b.build_int_const(off, ValueType::int(64))?;
            b.build_int_binary_operation(call_sp, k, IntBinaryOp::Add, ValueType::int(64))
        };
        let g = b.build_int_const(0x2000u64, ValueType::int(64))?;
        b.build_call(g, &[], &[], 0)?;
        for (off, v) in [(10u64, 0xAu64), (14, 0xB)] {
            let addr = at(&mut b, off)?;
            let data = b.build_int_const(v, ValueType::int(32))?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        }
        let f = b.build_int_const(0x1000u64, ValueType::int(64))?;
        let (call_f, _) = b.build_call(f, &[], &[], 0)?;
        let slot1 = at(&mut b, 16)?;
        let loaded = b.build_load(slot1, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let (mut fg, load) = finish(b, loaded)?;
        let clobber = super::reload_clobber(&mut fg, load, false);
        assert_eq!(
            clobber,
            call_f,
            "slot 1 may be the callee's, so the load must stop at the call; got {:?}",
            fg.node_kind(clobber),
        );
        Ok(())
    }
}

mod own_frame_tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_frame, stack_args_at};

    /// `in_own_frame` reads a raw base, so it must recognise the alignment
    /// anchor `decompose` produces rather than take any `And` on trust.
    #[test]
    fn only_an_alignment_masked_sp_is_a_frame_base() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let aligned = b.build_const_binop(
            0xFFFF_FFF0u64,
            entry_sp,
            IntBinaryOp::And,
            ValueType::int(32),
        )?;
        let extracted =
            b.build_const_binop(0xFu64, entry_sp, IntBinaryOp::And, ValueType::int(32))?;
        b.build_store(aligned, extracted, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        crate::mem_analysis::collapse_phis(&mut fg);

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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let align = b.build_int_const(0xFFFF_FFF0u64, ValueType::int(32))?;
        let entry_sp =
            b.build_int_binary_operation(entry_sp, align, IntBinaryOp::And, ValueType::int(32))?;
        b.build_store(entry_sp, align, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        crate::mem_analysis::collapse_phis(&mut fg);

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

    /// Every unknown in the entry-SP walk answers `false`; a `Phi` with no arm
    /// is one of them.  It names no allocation either: a frontier with no
    /// definition is not heap memory.
    #[test]
    fn a_phi_with_no_readable_arm_is_not_this_frame() -> crate::Result<()> {
        use strider_ir::IRWalker;
        use strider_ir::node::{NodeKind, ValueKind};

        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::int(32))?;
        let anchor =
            b.build_int_binary_operation(entry_sp, mask, IntBinaryOp::And, ValueType::int(32))?;
        b.build_store(anchor, mask, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        let region = fg
            .walk_kind(|k| matches!(k, NodeKind::Region))
            .next()
            .expect("the fixture has one region");
        let token = fg.node_outputs(region)[1];
        let phi = strider_ir_test_utils::sentinel_node(
            &mut fg,
            NodeKind::Phi,
            [token],
            [ValueKind::Typed(ValueType::int(32))],
        );
        let phi_value = fg.node_outputs(phi)[0];
        let masked = fg.node_input_id_at(fg.producer(anchor), 0)?;
        fg.graph_mut().update_input(masked, phi_value);

        assert!(
            !in_own_frame(&fg, anchor, -8, 4),
            "an anchor over a phi with no readable arm has an unbounded \
             displacement, so it is not this frame's base"
        );
        assert!(
            super::decompose::decompose(&fg, phi_value).is_none(),
            "a phi with no arm names no allocation"
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
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::int(32))?;

        let below = b.build_int_const((-0x20i64) as u64, ValueType::int(32))?;
        let sp_below =
            b.build_int_binary_operation(entry_sp, below, IntBinaryOp::Add, ValueType::int(32))?;
        let anchor_below =
            b.build_int_binary_operation(sp_below, mask, IntBinaryOp::And, ValueType::int(32))?;

        let sp_above =
            b.build_const_binop(0x1000u64, entry_sp, IntBinaryOp::Add, ValueType::int(32))?;
        let anchor_above =
            b.build_int_binary_operation(sp_above, mask, IntBinaryOp::And, ValueType::int(32))?;
        b.build_store(anchor_below, mask, rsleigh::VnSpace::RAM)?;
        b.build_store(anchor_above, mask, rsleigh::VnSpace::RAM)?;

        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        crate::mem_analysis::collapse_phis(&mut fg);

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

mod comparability_tests {
    use super::*;

    /// The verdict of two offsets from one base, or two constants.
    fn offset_range_verdict(load: Loc, store: Loc) -> AliasVerdict {
        let policy = Policy {
            stack_global_disjoint: false,
            distinct_sp_bases_disjoint: false,
            calls: Calls::Block,
        };
        alias_verdict(load, store, policy)
    }

    /// Each offset is reduced at its OWN address width, so a 32-bit reduction
    /// and a 64-bit one are values mod different powers of two. Their
    /// difference names no distance, and reading the bound off either width
    /// alone lets a range that wraps 2^32 look ~4 GiB clear of the load.
    #[test]
    fn two_offsets_reduced_at_different_widths_compare_nothing() {
        let narrow = Loc {
            class: AddrClass::Constant { addr: 0 },
            size: 4,
            bits: 32,
            space: rsleigh::VnSpace::RAM,
        };
        let wide = Loc {
            class: AddrClass::Constant { addr: -4 },
            size: 8,
            bits: 64,
            space: rsleigh::VnSpace::RAM,
        };
        // Adjacent under a single 64-bit bound.
        for (load, store) in [(wide, narrow), (narrow, wide)] {
            assert_eq!(
                offset_range_verdict(load, store),
                AliasVerdict::MayAlias,
                "a 64-bit and a 32-bit reduction live in different moduli"
            );
        }
    }

    /// Global addresses compare on the ring too: a constant just below 2^32
    /// and one just above 0 are eight bytes apart.  The integer order a raw
    /// address once took read them ~4 GiB apart and so refused them as
    /// incomparable.
    ///
    /// Mutation: compare constants in integer order within half the modulus.
    #[test]
    fn constants_across_the_wrap_compare_on_the_ring() -> crate::Result<()> {
        use strider_ir::IRBuilderExt;
        use strider_ir::node::ValueType;
        let mut addrs = Vec::new();
        let f = strider_ir_test_utils::make_empty_fn(|b| {
            for a in [0xFFFF_FFFCu64, 0x4, 0x0] {
                addrs.push(b.build_int_const(a, ValueType::int(32))?);
            }
            b.build_int_const(0u64, ValueType::int(32))
        })?;
        let at = |addr: ValueId| Loc {
            class: classify_addr(&f, addr),
            size: 4,
            bits: 32,
            space: rsleigh::VnSpace::RAM,
        };
        assert_eq!(
            offset_range_verdict(at(addrs[0]), at(addrs[1])),
            AliasVerdict::Disjoint,
            "0xFFFFFFFC and 0x4 are eight bytes apart"
        );
        let wide = Loc {
            size: 8,
            ..at(addrs[0])
        };
        assert_eq!(
            offset_range_verdict(wide, at(addrs[2])),
            AliasVerdict::MayAlias,
            "an 8-byte access at 0xFFFFFFFC wraps onto 0"
        );
        Ok(())
    }

    /// An address wider than the `i128` carrier has no reduction, so it is
    /// an `Anchor` whatever it decomposes to.
    #[test]
    fn an_address_past_the_carrier_is_an_anchor() -> crate::Result<()> {
        use strider_ir::IRBuilderExt;
        use strider_ir::node::ValueType;
        let mut addr = None;
        let f = strider_ir_test_utils::make_empty_fn(|b| {
            addr = Some(b.build_int_const(0x1000u64, ValueType::int(256))?);
            b.build_int_const(0u64, ValueType::int(32))
        })?;
        let addr = addr.expect("built");
        assert_eq!(classify_addr(&f, addr), AddrClass::Anchor { value: addr });
        Ok(())
    }

    /// The ring places a pair the integer bound refuses: six bytes apart mod
    /// 2^32, but at opposite ends of the signed range.
    #[test]
    fn the_ring_separates_a_pair_across_the_wrap() {
        let base = ValueId::from_u32(0);
        let at = |offset: i128| Loc {
            class: AddrClass::StackRooted { base, offset },
            size: 4,
            bits: 32,
            space: rsleigh::VnSpace::RAM,
        };
        assert_eq!(
            offset_range_verdict(at(0x7FFF_FFFA), at(-0x7FFF_FFFC)),
            AliasVerdict::Disjoint,
            "`sp + 0x7FFFFFFA` and `sp + 0x80000004` are six bytes apart"
        );
        assert_eq!(
            offset_range_verdict(at(0x7FFF_FFFE), at(-0x8000_0000)),
            AliasVerdict::MayAlias,
            "`sp + 0x7FFFFFFE` and `sp + 0x80000000` are two bytes apart, so \
             the four-byte accesses overlap"
        );
    }
}

mod modular_offset_tests {
    use super::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IRWalker, IntBinaryOp};
    use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86 as sp};

    /// Address arithmetic is mod 2^32 here, so `sp+0x7FFFFFFE` and
    /// `sp+0x80000000` name slots two bytes apart and their four-byte accesses
    /// overlap.  Reducing both into the signed range puts them at opposite ends
    /// of the integer order, where a non-modular interval test reads them as
    /// 2^32-2 apart.
    #[test]
    fn offsets_two_bytes_apart_across_the_wrap_may_alias() -> crate::Result<()> {
        let sp = sp();
        let mut b = sp_arg_frame(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let a =
            b.build_const_binop(0x7FFF_FFFEu64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let bb =
            b.build_const_binop(0x8000_0000u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(bb, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(a, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let (mut fg, load_node) = finish(b, loaded)?;
        let store_node = fg
            .walk_kind(|k| matches!(k, NodeKind::Store(_)))
            .next()
            .expect("store");
        let policy = super::call_blocking(&mut fg, false);
        assert_ne!(
            verdict(policy, &fg, load_node, store_node),
            AliasVerdict::Disjoint,
            "the accesses are two bytes apart mod 2^32 and overlap",
        );
        Ok(())
    }
}

/// An alignment mask rounds an address DOWN, so its 1-run has to reach the top
/// of the width it is applied at. A run that stops short is a bit-extraction
/// or a truncation, and neither yields a stack address; anchoring a stack
/// base on one lets `stack_global_disjoint` call it disjoint from a constant
/// address it may equal.
#[test]
fn only_a_top_reaching_run_is_an_alignment_mask() {
    for (m, width) in [
        (0xFFFF_FFFF_FFFF_FFF0u128, 64),
        (0xFFFF_FFF0, 32),
        (!0xFu128, 128),
    ] {
        assert!(is_alignment_mask(m, width), "{m:#x} at {width} rounds down");
    }
    for (m, width, why) in [
        (0x10u128, 64, "one bit, not a run to the top"),
        (0xF0, 64, "a run, but nowhere near the top"),
        (0xFFFF_FFF0, 64, "reaches the top of 32, not of 64"),
        (0xF, 64, "no low zero run: a bit-extraction"),
        (0, 64, "no alignment effect"),
        (u128::MAX, 64, "all ones: no alignment effect"),
        (
            0x8000_0000,
            32,
            "top-reaching, but no ABI aligns a frame to 2 GiB",
        ),
        (!0xFFFFu128, 128, "a 64 KiB alignment is a bit-manipulation"),
    ] {
        assert!(!is_alignment_mask(m, width), "{m:#x} at {width}: {why}");
    }
}

mod store_index {
    use super::*;
    use crate::mem_ssa::{MemLayout, Shape};
    use strider_ir::node::ValueType;
    use strider_ir::{FunctionBuilder, IRBuilderExt, IRWalker, IntBinaryOp, IntCmpOp};
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, reg_vn, stack_vn_x86 as sp};

    use crate::test_support::Rng;

    /// The listed allocator every chain calls.
    const ALLOC: u64 = 0x3000;

    /// The allocator's return register, disjoint from the SP.
    fn ret_reg() -> rsleigh::Vn {
        reg_vn(0x00, 4)
    }

    fn alloc(b: &mut FunctionBuilder) -> crate::Result<ValueId> {
        let target = b.build_int_const(ALLOC, ValueType::int(32))?;
        let (_call, rets) = b.build_call(target, &[], &[ret_reg()], 0)?;
        Ok(rets[0])
    }

    /// A random run of stores through every address class, calls and an
    /// opaque user-op, plus the addresses a probe can take. The heap classes
    /// come from `ALLOC`: a `Phi` of two allocations, whose identity is
    /// unknown, and allocations in the run itself. One store in six is in
    /// register space. Two arms add `k` to an SP or heap base plus `2^31 - 4`,
    /// so their offsets straddle the top of the signed 32-bit range. One
    /// constant arm straddles 0 and `2^32`, and one is an `I64` address, at or
    /// near 0 or numerically equal to an `I32` one.
    fn random_chain(seed: u64) -> crate::Result<(Function, Vec<ValueId>)> {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let sp = sp();
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(ret_reg())
            .ret(ret_reg())
            .build_fn()?;
        let entry = b.create_region_all()?;
        let left = b.create_region_all()?;
        let right = b.create_region_all()?;
        let join = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

        b.set_region(entry);
        let entry_sp = b.read_variable(&sp)?;
        let zero = b.build_int_const(0u64, ValueType::int(32))?;
        let cond =
            b.build_int_cmp_operation(entry_sp, zero, IntCmpOp::Equal, ValueType::int(32))?;
        b.build_if(cond, left, right)?;
        for arm in [left, right] {
            b.set_region(arm);
            let p = alloc(&mut b)?;
            b.write_variable(&ret_reg(), p)?;
            b.build_branch(join)?;
        }

        b.set_region(join);
        let merged = b.read_variable(&ret_reg())?;
        let first = b.build_int_const(1u64, ValueType::int(32))?;
        b.build_store(merged, first, rsleigh::VnSpace::RAM)?;
        let mut heaps = vec![merged];
        let sp_value = b.read_variable(&sp)?;
        let aligned = b.build_const_binop(
            0xFFFF_FFF0u64,
            sp_value,
            IntBinaryOp::And,
            ValueType::int(32),
        )?;
        let near_top = b.build_int_const(0x7FFF_FFFCu64, ValueType::int(32))?;
        let sp_top =
            b.build_int_binary_operation(sp_value, near_top, IntBinaryOp::Add, ValueType::int(32))?;
        let cell = b.build_int_const(0x9000u64, ValueType::int(32))?;
        let opaque = b.build_load(cell, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        let mut addresses = vec![opaque, merged];
        for _ in 0..40 {
            let k = rng.below(24) as i64 - 12;
            let addr = match rng.below(13) {
                0 | 1 => {
                    let k = b.build_int_const(k as u64, ValueType::int(32))?;
                    b.build_int_binary_operation(sp_value, k, IntBinaryOp::Add, ValueType::int(32))?
                }
                2 => {
                    let k = b.build_int_const(k as u64, ValueType::int(32))?;
                    b.build_int_binary_operation(aligned, k, IntBinaryOp::Add, ValueType::int(32))?
                }
                3 => b.build_int_const((0x1000 + k) as u64, ValueType::int(32))?,
                4 => {
                    b.build_int_const((0xFFFF_F000u64).wrapping_add(k as u64), ValueType::int(32))?
                }
                5 => opaque,
                6 => {
                    let base = heaps[rng.below(heaps.len())];
                    let k = b.build_int_const(k as u64, ValueType::int(32))?;
                    b.build_int_binary_operation(base, k, IntBinaryOp::Add, ValueType::int(32))?
                }
                7 => {
                    let p = alloc(&mut b)?;
                    heaps.push(p);
                    addresses.push(p);
                    continue;
                }
                8 => {
                    let k = b.build_int_const(k as u64, ValueType::int(32))?;
                    b.build_int_binary_operation(sp_top, k, IntBinaryOp::Add, ValueType::int(32))?
                }
                9 => {
                    let base = heaps[rng.below(heaps.len())];
                    let top = b.build_int_binary_operation(
                        base,
                        near_top,
                        IntBinaryOp::Add,
                        ValueType::int(32),
                    )?;
                    let k = b.build_int_const(k as u64, ValueType::int(32))?;
                    b.build_int_binary_operation(top, k, IntBinaryOp::Add, ValueType::int(32))?
                }
                10 => b.build_int_const(k as u64 & 0xFFFF_FFFF, ValueType::int(32))?,
                11 => {
                    let at = if rng.below(2) == 0 { k } else { 0x1000 + k };
                    b.build_int_const(at as u64, ValueType::int(64))?
                }
                _ => {
                    if rng.below(2) == 0 {
                        let target = b.build_int_const(0x2000u64, ValueType::int(32))?;
                        b.build_call(target, &[], &[], 0)?;
                    } else {
                        b.build_call_other(0, &[], &[], true, false)?;
                    }
                    continue;
                }
            };
            addresses.push(addr);
            let ty = [
                ValueType::int(8),
                ValueType::int(16),
                ValueType::int(32),
                ValueType::int(64),
            ][rng.below(4)];
            let data = b.build_int_const(rng.next_u64() % 100, ty)?;
            let space = if rng.below(6) == 0 {
                rsleigh::VnSpace::REGISTER
            } else {
                rsleigh::VnSpace::RAM
            };
            b.build_store(addr, data, space)?;
        }
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        super::super::collapse_phis(&mut fg);
        // The collapse ran at the floor; the probes run under `{ALLOC}`.
        fg.assume(&super::allocating(&[ALLOC]));
        Ok((fg, addresses))
    }

    /// Over every prefix of the chain, the index offers a def at least as high
    /// as the highest one the walker says clobbers, for every class of probe,
    /// both spaces, and every setting the candidates read.
    #[test]
    fn the_index_never_hides_a_clobber() -> crate::Result<()> {
        let mut heap_near_top = 0;
        for seed in 0..60 {
            let (fg, addresses) = random_chain(seed)?;
            let layout = MemLayout::build(&fg);
            let query = MemQuery::default();
            heap_near_top += addresses
                .iter()
                .filter(|&&a| {
                    matches!(
                        classify_addr(&fg, a),
                        AddrClass::HeapRooted { offset, .. } if offset.abs() >= 1 << 30
                    )
                })
                .count();
            let stores = StoreIndex::build(&fg, &layout);
            let mut runs = vec![Policy::strict(&fg)];
            for sgd in [false, true] {
                for dsbd in [false, true] {
                    let relaxed = Policy {
                        stack_global_disjoint: sgd,
                        distinct_sp_bases_disjoint: dsbd,
                        ..Policy::strict(&fg)
                    };
                    runs.push(relaxed);
                    runs.push(Policy {
                        calls: Calls::Pass,
                        ..relaxed
                    });
                }
            }
            for &policy in &runs {
                for (i, &addr) in addresses.iter().enumerate() {
                    let space = if i % 4 == 3 {
                        rsleigh::VnSpace::REGISTER
                    } else {
                        rsleigh::VnSpace::RAM
                    };
                    for size in [1, 4, 8] {
                        let probe = Loc {
                            class: classify_addr(&fg, addr),
                            size,
                            bits: addr_bit_width(&fg, addr),
                            space,
                        };
                        let mut walker = MemWalker {
                            query: &query,
                            policy,
                            loc: probe,
                            stores: &stores,
                            private_frame: std::cell::OnceCell::new(),
                        };
                        let mut highest = None;
                        for pos in 0..layout.len() as u32 {
                            if layout.shape_at(pos) == Shape::Def
                                && crate::mem_ssa::ClobberProbe::clobbers(
                                    &mut walker,
                                    &fg,
                                    layout.node_at(pos),
                                )
                            {
                                highest = Some(pos);
                            }
                            let offered = stores.candidate(&probe, policy, 0, pos);
                            assert!(
                                offered >= highest,
                                "seed {seed}: {probe:?} in {space:?} up to {pos} offered \
                                 {offered:?} below the clobber at {highest:?}"
                            );
                            if let Some(h) = highest {
                                assert!(
                                    stores
                                        .candidate(&probe, policy, h, pos)
                                        .is_some_and(|c| c >= h),
                                    "seed {seed}: {probe:?} in {space:?} in {h}..={pos} \
                                     hid the clobber"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(heap_near_top > 0, "no heap probe straddles the top");
        Ok(())
    }

    /// One query serves every policy over one index, which is a function of
    /// the graph alone, so a policy asked second answers what a query of its
    /// own answers.
    #[test]
    fn a_shared_query_answers_what_its_own_would() -> crate::Result<()> {
        let mut probed = 0usize;
        for seed in 0..20 {
            let (fg, addresses) = random_chain(seed)?;
            let strict = Policy::strict(&fg);
            let shared = MemQuery::default();
            let ret = fg
                .walk_kind(|k| matches!(k, NodeKind::Return))
                .next()
                .expect("the chain ends at a Return");
            let mem_start = fg.node_inputs(ret)[NodeKind::CTRL_MEM_SLOT];
            let stack_probes: Vec<(ValueId, i128)> = addresses
                .iter()
                .filter_map(|&addr| match classify_addr(&fg, addr) {
                    AddrClass::StackRooted { base, offset } => Some((base, offset)),
                    _ => None,
                })
                .collect();
            // Another policy queries first, so the strict one reads an index
            // built for it.
            let relaxed = Policy::strict(&fg);
            for &(base, offset) in &stack_probes {
                shared.nearest_sp_clobber(
                    &fg,
                    relaxed,
                    mem_start,
                    base,
                    offset,
                    4,
                    rsleigh::VnSpace::RAM,
                );
            }
            let fresh = MemQuery::default();
            for &(base, offset) in &stack_probes {
                for size in [1, 4, 8] {
                    probed += 1;
                    assert_eq!(
                        shared.nearest_sp_clobber(
                            &fg,
                            strict,
                            mem_start,
                            base,
                            offset,
                            size,
                            rsleigh::VnSpace::RAM
                        ),
                        fresh.nearest_sp_clobber(
                            &fg,
                            strict,
                            mem_start,
                            base,
                            offset,
                            size,
                            rsleigh::VnSpace::RAM
                        ),
                        "seed {seed}: the shared index moved the answer at {offset}"
                    );
                }
            }
        }
        assert!(probed > 0, "no stack-rooted probe ran");
        Ok(())
    }
}

mod refresh_tests {
    use strider_ir::node::{IntBinaryOp, NodeKind, ValueType};
    use strider_ir::{Function, IRBuilderExt, IRViewer, IRWalker, MemDecomp};
    use strider_ir_test_utils::{IrWalkerEx, RegisterSet, reg_vn, stack_vn_x86_64};

    const MALLOC: u64 = 0x1000;

    /// A heap store, a stack store and a global store, run through `pipeline`
    /// with `MALLOC` listed.  The one load is forwarded away, so once it is
    /// gone no pass decomposes what is left.
    fn optimized(pipeline: &crate::OptimizerPipeline) -> crate::Result<Function> {
        let sp = stack_vn_x86_64();
        let ret = reg_vn(0, 8);
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(ret)
            .arg(sp)
            .ret(ret)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let target = b.build_int_const(MALLOC, ValueType::int(64))?;
        let (_call, rets) = b.build_call(target, &[], &[ret], 0)?;
        let one = b.build_int_const(1u64, ValueType::int(64))?;
        b.build_store(rets[0], one, rsleigh::VnSpace::RAM)?;
        let slot =
            b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
        let two = b.build_int_const(2u64, ValueType::int(64))?;
        b.build_store(slot, two, rsleigh::VnSpace::RAM)?;
        let reload = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let global = b.build_int_const(0x9000u64, ValueType::int(64))?;
        b.build_store(global, reload, rsleigh::VnSpace::RAM)?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&super::allocating(&[MALLOC]));
        pipeline.run(&mut fg, &crate::OptCtx::new(None))?;
        Ok(fg)
    }

    /// Every live `Load` / `Store` address, with its memory class.
    fn classes(fg: &Function) -> Vec<MemDecomp> {
        fg.walk_kind(|k| matches!(k, NodeKind::Load(_) | NodeKind::Store(_)))
            .map(|n| fg.memory_class(fg.node_inputs(n)[1]))
            .collect()
    }

    /// The class of the store of `1`, the heap one.
    fn heap_store_class(fg: &Function) -> MemDecomp {
        let store = fg
            .walk_kind(|k| matches!(k, NodeKind::Store(_)))
            .find(|&n| fg.int_const_u128(fg.store_data(n)) == Some(1))
            .expect("the heap store");
        fg.memory_class(fg.store_addr(store))
    }

    /// No live access is left unclassified after a run or after a rewrite.
    #[test]
    fn every_access_is_classified_after_a_run_and_a_rewrite() -> crate::Result<()> {
        let mut fg = optimized(&crate::default_pipeline())?;
        let ran = classes(&fg);
        assert_eq!(ran.len(), 3, "the three stores survive, the load does not");
        assert!(!ran.contains(&MemDecomp::Unknown), "after the run: {ran:?}");
        assert!(matches!(heap_store_class(&fg), MemDecomp::Heap(_)));
        let no_rules: &[crate::BoxedRule] = &[];
        crate::apply_rules_count(&mut crate::EditFunction::new(&mut fg), no_rules)?;
        let rewritten = classes(&fg);
        assert!(
            !rewritten.contains(&MemDecomp::Unknown),
            "after a rewrite: {rewritten:?}"
        );
        assert!(matches!(heap_store_class(&fg), MemDecomp::Heap(_)));
        Ok(())
    }

    /// An edit outside a pipeline leaves the class stale until a refresh.
    #[test]
    fn refresh_rederives_an_edited_address() -> crate::Result<()> {
        let sp = stack_vn_x86_64();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let addr = b.build_const_binop(
                (-16i64) as u64,
                sp_val,
                IntBinaryOp::Add,
                ValueType::int(64),
            )?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;
        crate::default_pipeline().run(&mut fg, &crate::OptCtx::new(None))?;
        let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
        let addr = fg.load_addr(load);
        let minus_16 = fg
            .value_inputs(fg.producer(addr))
            .find(|&v| fg.int_const_i128(v) == Some(-16))
            .expect("the slot offset");
        let offset = |fg: &Function| fg.stack_offset(load).map(|(_, k)| k);
        assert_eq!(offset(&fg), Some(-16));
        let mut edit = crate::EditFunction::new(&mut fg);
        let minus_8 = edit.build_int_const((-8i64) as u64, ValueType::int(64))?;
        edit.replace_value(minus_16, minus_8);
        assert_eq!(
            offset(edit.function()),
            Some(-16),
            "stale before the refresh"
        );
        crate::refresh_memory_classes(&mut edit);
        assert_eq!(offset(edit.function()), Some(-8));
        Ok(())
    }

    /// A post-pass appended to the default pipeline that reads the memory
    /// classes finds every live access classified once it refreshes them.
    #[test]
    fn an_appended_post_pass_reads_the_classes_after_a_refresh() -> crate::Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        /// Stores one more than the unclassified accesses it saw, so 0 means
        /// it never ran.
        #[derive(Clone)]
        struct ReadsClasses(Arc<AtomicUsize>);
        impl crate::Optimizer for ReadsClasses {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &crate::OptCtx<'_>,
            ) -> crate::Result<()> {
                crate::refresh_memory_classes(edit);
                let unknown = classes(edit.function())
                    .into_iter()
                    .filter(|&c| c == MemDecomp::Unknown)
                    .count();
                self.0.store(unknown + 1, Ordering::Relaxed);
                Ok(())
            }
        }
        let seen = Arc::new(AtomicUsize::new(0));
        let mut pipeline = crate::default_pipeline();
        // A pass is held `&'static`; the counter leaks one box.
        pipeline.add_post_pass(Box::leak(Box::new(ReadsClasses(Arc::clone(&seen)))));
        optimized(&pipeline)?;
        assert_eq!(
            seen.load(Ordering::Relaxed),
            1,
            "the appended pass ran and found every access classified"
        );
        Ok(())
    }
}

/// An address width the ring cannot reduce in makes every overlap test
/// answer "may alias": the fallback is the conservative direction, and it
/// must stay that way at each of the three `None` meanings.
#[test]
fn an_unreducible_address_width_may_always_alias() {
    use super::ring::ring_overlap;
    use strider_ir::node::ValueType;
    for ty in [None, Some(ValueType::float(64)), Some(ValueType::int(256))] {
        assert!(
            ring_overlap(ty, 0, 1, 0x1000, 1),
            "{ty:?} must not prove disjointness"
        );
    }
    assert!(
        !ring_overlap(Some(ValueType::int(32)), 0, 1, 0x1000, 1),
        "a tracked width still separates"
    );
}

/// `LoadForward` keeps the outgoing-argument-window memo across its forwards.
/// That rests on the direction the staleness runs in: a store address
/// the window scan could not place was an `Anchor`, which blinds the scan, so
/// a window scanned before the forward covers at least what one scanned after
/// it covers.  Covering more only keeps a load from forwarding across the
/// call.
mod stale_window_is_conservative {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRWalker, IntBinaryOp};
    use strider_ir_test_utils::{sp_frame, stack_args_at};

    /// A spill, its reload, and one argument store addressed `sp + reload`:
    /// unplaceable until the reload forwards to the constant it spilled.
    #[test]
    fn a_window_scanned_before_a_forward_covers_what_one_after_it_does() -> crate::Result<()> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let args = stack_args_at(0, 4).expect("a declared stack-arg layout");
        let mut b = sp_frame(sp)
            .stack_args(Some(args))
            .build_fn_single_region()?;
        let sp0 = b.read_variable(&sp)?;
        let i32_ty = ValueType::int(32);
        let spill_addr = b.build_const_binop((-16i64) as u64, sp0, IntBinaryOp::Add, i32_ty)?;
        let zero = b.build_int_const(0u64, i32_ty)?;
        b.build_store(spill_addr, zero, rsleigh::VnSpace::RAM)?;
        let reload = b.build_load(spill_addr, rsleigh::VnSpace::RAM, i32_ty)?;
        let arg_addr = b.build_int_binary_operation(sp0, reload, IntBinaryOp::Add, i32_ty)?;
        let arg = b.build_int_const(0x11u64, i32_ty)?;
        b.build_store(arg_addr, arg, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x1000u64, i32_ty)?;
        b.build_call_cc(target, None)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // Collapses `read_variable(sp)` onto the bare `InitialVar`, without
        // which the call's SP does not decompose and no window is placed.
        collapse_phis(&mut fg);

        super::escaping(&mut fg, false);
        let probes: Vec<(i128, i128)> = (0..4).map(|k| (k * 4, k * 4 + 4)).collect();
        let before = window_answers(&fg, &probes);

        // What the forward does to the argument store's address, which the
        // pipeline's own invalidation then reclassifies.
        let mut edit = crate::EditFunction::new(&mut fg);
        assert!(edit.replace_value(reload, zero), "the reload forwards");
        edit.clean();
        fg.invalidate_analyses();
        let after = window_answers(&fg, &probes);

        assert_ne!(
            before, after,
            "the forward has to move an answer, or the fixture proves nothing"
        );
        for (i, (&was, &now)) in before.iter().zip(&after).enumerate() {
            assert!(
                was || !now,
                "slot {i} is the callee's after the forward but not before it"
            );
        }
        Ok(())
    }

    /// Whether the call's window covers each probe, scanned fresh off the
    /// graph as it stands.
    fn window_answers(fg: &Function, probes: &[(i128, i128)]) -> Vec<bool> {
        let call = fg
            .walk_kind(|k| matches!(k, NodeKind::Call { .. }))
            .next()
            .expect("the fixture has one call");
        let window = ArgPrefix::scan(fg, &MemQuery::default(), call).expect("the window is placed");
        probes
            .iter()
            .map(|&(lo, hi)| window.covers(lo, hi))
            .collect()
    }
}
