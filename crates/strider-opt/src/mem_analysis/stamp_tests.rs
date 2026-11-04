use crate::test_support::{CF_RP, run_passes};
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::{IrBuilderEx, IrWalkerEx};
use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

fn stamped_count(function: &Function) -> usize {
    function
        .graph()
        .all_node_ids()
        .filter(|&n| function.stack_offset(n).is_some())
        .count()
}

fn run(function: &mut Function) {
    // ConstantFold folds the lowered `Add(_, Neg(K))` to `Add(_, IntConst(-K))`
    // and PhiCollapse drops the read_variable(sp) phi, giving the production
    // shape the post-pass sees.
    run_passes(function, CF_RP).expect("canonicalize must not error");
    super::stamp_all(&mut crate::EditFunction::new(function));
}

/// Builds `store [sp-4] = 0x42; load [sp-4]; return loaded`.
fn stack_store_load_return() -> Function {
    make_sp_fn(stack_vn_x86(), |b, sp_v| {
        let four = b.build_int_const(4u64, ValueType::int(32))?;
        let addr = b.build_sub_as_add_neg(sp_v, four, ValueType::int(32))?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap()
}

type Addr = fn(&mut FunctionBuilder, ValueId) -> crate::Result<ValueId>;

/// `store [addr(sp)] = 0x42; return 0`, run: the function and its store.
fn store_at(addr: Addr) -> (Function, NodeId) {
    let mut f = make_sp_fn(stack_vn_x86(), |b, sp_v| {
        let addr = addr(b, sp_v)?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let zero = b.build_int_const(0u64, ValueType::int(32))?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();
    run(&mut f);
    let store = f.sole_kind(|k| matches!(k, NodeKind::Store(_)));
    (f, store)
}

#[test]
fn sp_relative_store_and_load_get_offset_stamped() {
    let mut f = stack_store_load_return();
    run(&mut f);
    let store = f.sole_kind(|k| matches!(k, NodeKind::Store(_)));
    let load = f.sole_kind(|k| matches!(k, NodeKind::Load(_)));
    for node in [store, load] {
        assert_eq!(f.stack_offset(node).map(|(_, off)| off), Some(-4));
    }
}

#[test]
fn non_sp_relative_store_leaves_side_table_untouched() {
    let (f, _) = store_at(|b, _| b.build_int_const(0x1000u64, ValueType::int(32)));
    assert_eq!(stamped_count(&f), 0);
}

/// An alignment-masked base is a stack access in a different coordinate system
/// from entry-SP, so its offset must not be conflated with an entry-SP one.
#[test]
fn alignment_masked_base_store_is_stamped_with_aligned_base() {
    // `and $0xfffffff8, %esp`, then a store at that aligned base.
    let (f, store) = store_at(|b, sp| {
        b.build_const_binop(0xFFFF_FFF8u64, sp, IntBinaryOp::And, ValueType::int(32))
    });
    let (base, offset) = f
        .stack_offset(store)
        .expect("aligned store must be stamped");
    assert_eq!(offset, 0, "store at the aligned base directly => offset 0");
    assert!(
        matches!(
            f.node_kind(f.producer(base)),
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "recorded base must be the alignment `And` node, not InitialVar(sp)"
    );
}

/// The decomposition walks the whole chain, not just the outermost Add, and
/// sums signed: `((sp + 8) + 16) - 4` stamps `+20`, `(sp + 8) - 12` stamps -4.
#[test]
fn a_nested_chain_stamps_its_summed_offset() {
    let rows: [(Addr, i128); 2] = [
        (
            |b, sp| {
                let a1 = b.build_const_binop(8u64, sp, IntBinaryOp::Add, ValueType::int(32))?;
                let a2 = b.build_const_binop(16u64, a1, IntBinaryOp::Add, ValueType::int(32))?;
                let four = b.build_int_const(4u64, ValueType::int(32))?;
                b.build_sub_as_add_neg(a2, four, ValueType::int(32))
            },
            20,
        ),
        (
            |b, sp| {
                let a1 = b.build_const_binop(8u64, sp, IntBinaryOp::Add, ValueType::int(32))?;
                let twelve = b.build_int_const(12u64, ValueType::int(32))?;
                b.build_sub_as_add_neg(a1, twelve, ValueType::int(32))
            },
            -4,
        ),
    ];
    for (addr, want) in rows {
        let (f, store) = store_at(addr);
        assert_eq!(f.stack_offset(store).map(|(_, off)| off), Some(want));
    }
}

#[test]
fn rerun_after_first_pass_is_idempotent() {
    let mut f = stack_store_load_return();
    run(&mut f);
    let after_first = stamped_count(&f);
    assert!(
        after_first > 0,
        "first run must stamp the SP-relative accesses"
    );
    run(&mut f);
    assert_eq!(
        stamped_count(&f),
        after_first,
        "re-run must be idempotent: no new stamps"
    );
}

#[test]
fn post_pass_function_validates() {
    let mut f = stack_store_load_return();
    run(&mut f);
    strider_ir::validate::validate(&f).expect("IR must validate");
}

/// The region filters (`stack_only` / `heap_only` / `non_stack`) read the
/// decomposition memo, which every changing pass drains. A pipeline with no
/// post-pass must still leave it filled, or every region filter matches
/// nothing.
#[test]
fn a_pipeline_without_post_passes_still_stamps_the_decomposition() {
    let mut function = stack_store_load_return();
    run_passes(&mut function, CF_RP).expect("must not error");
    assert!(
        stamped_count(&function) > 0,
        "no address was stamped, so every stack_only() query would come back empty"
    );
}
