use strider_ir::node::ValueType;
use strider_ir::{FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{CaptureExt, MatchPat, Matcher, int_const, store};
#[test]
fn bit_width_filters_store_by_data_width() {
    // Both stores are reachable via the memory chain ending at Return.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b.build_int_const(0x100u64, ValueType::I64).expect("addr1");
    let v32 = b.build_int_const(1u64, ValueType::I32).expect("v32");
    b.build_store(addr1, v32, rsleigh::VnSpace::RAM)
        .expect("u32 store");
    let addr2 = b.build_int_const(0x108u64, ValueType::I64).expect("addr2");
    let v64 = b.build_int_const(2u64, ValueType::I64).expect("v64");
    b.build_store(addr2, v64, rsleigh::VnSpace::RAM)
        .expect("u64 store");
    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::new(&function);
    let h32 = m
        .find_all(&store().addr(int_const(0x100u128)).bit_width(32).build())
        .unwrap();
    let h64 = m
        .find_all(&store().addr(int_const(0x108u128)).bit_width(64).build())
        .unwrap();
    assert_eq!(h32.len(), 1);
    assert_eq!(h64.len(), 1);
    let h32_wrong = m
        .find_all(&store().addr(int_const(0x100u128)).bit_width(64).build())
        .unwrap();
    let h64_wrong = m
        .find_all(&store().addr(int_const(0x108u128)).bit_width(32).build())
        .unwrap();
    assert_eq!(h32_wrong.len(), 0);
    assert_eq!(h64_wrong.len(), 0);
}

/// Output width and input width are different questions. `any_bool()`
/// (output width 1) catches anything producing a bool, comparisons included;
/// `bool_inputs(..)` (input width 1) catches only ops over booleans, so it
/// skips comparisons whose operands are 32-bit.
#[test]
fn output_width_and_input_width_distinguish_bool_ops_from_comparisons() {
    use strider_ir::{IntBinaryOp, IntCmpOp};
    use strider_pattern::{any_bool, anything, bool_and, bool_inputs, value_of_width};

    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let a = b.build_int_const(1u64, ValueType::I32).expect("a");
    let c = b.build_int_const(2u64, ValueType::I32).expect("c");
    // Two comparisons: wide inputs, I1 output.
    let cmp1 = b
        .build_int_cmp_operation(a, c, IntCmpOp::Equal, ValueType::I32)
        .expect("cmp1");
    let cmp2 = b
        .build_int_cmp_operation(a, c, IntCmpOp::Sless, ValueType::I32)
        .expect("cmp2");
    // Combined by a boolean AND: I1 in, I1 out.
    let and = b
        .build_int_binary_operation(cmp1, cmp2, IntBinaryOp::And, ValueType::I1)
        .expect("bool and");
    // Returning the AND keeps the comparisons (and their consts) reachable.
    b.build_return(Some(and), &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::new(&function);

    // Produces a bool: both comparisons plus the AND.
    assert_eq!(
        m.find_all(&any_bool().into_pattern()).unwrap().len(),
        3,
        "two comparisons and the boolean AND all produce I1"
    );

    // Operates on booleans: only the AND. Comparisons take I32 operands and
    // the consts have no value inputs at all.
    assert_eq!(
        m.find_all(&bool_inputs(anything()).into_pattern())
            .unwrap()
            .len(),
        1,
        "only the boolean AND consumes 1-bit operands"
    );

    // The two queries compose.
    assert_eq!(
        m.find_all(&bool_inputs(bool_and(anything(), anything())).into_pattern())
            .unwrap()
            .len(),
        1
    );

    assert!(
        !m.find_all(&value_of_width(32).into_pattern())
            .unwrap()
            .is_empty()
    );
}

/// The `bool_*` constructors match only an `I1` value output, never a
/// same-shaped wide integer op or const. The guard survives `.ordered()`, so
/// neither operand order reaches a wide `And` either.
#[test]
fn bool_ctors_require_i1_output() {
    use strider_ir::IntBinaryOp;
    use strider_pattern::{anything, bool_and, bool_binary, bool_const, int_and};

    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    // I64 And over an I64 IntConst(1); neither is a boolean.
    let x = b.build_int_const(0xFFu64, ValueType::I64).expect("x");
    let one = b.build_int_const(1u64, ValueType::I64).expect("one");
    let wide_and = b
        .build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::I64)
        .expect("wide and");
    b.build_return(Some(wide_and), &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::new(&function);
    assert_eq!(
        m.find_all(&bool_and(anything(), anything()).into_pattern())
            .unwrap()
            .len(),
        0,
        "bool_and is boolean-specific and must not match a 64-bit And"
    );
    assert_eq!(
        m.find_all(&bool_const(true).into_pattern()).unwrap().len(),
        0,
        "bool_const matches only an I1 constant, not a wide IntConst(1)"
    );
    for pat in [
        bool_binary(IntBinaryOp::And, anything(), anything()).into_pattern(),
        bool_binary(IntBinaryOp::And, anything(), anything())
            .ordered()
            .into_pattern(),
    ] {
        assert!(m.find_all(&pat).unwrap().is_empty());
    }
    // The width is the whole difference: the integer constructor matches it.
    assert_eq!(
        m.find_all(&int_and(anything(), anything()).into_pattern())
            .unwrap()
            .len(),
        1
    );
}

/// `inputs_of_width` is a constraint on the matched NODE's inputs, so wrapping
/// an alternation in it must not land the width on the arms' own outputs: an
/// `I1` compare is never 64 bits wide and the query would match nothing.
#[test]
fn inputs_of_width_over_an_alternation_still_constrains_the_inputs() {
    use strider_ir::IntCmpOp;
    use strider_pattern::{anything, inputs_of_width, int_eq, one_of};

    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let a = b.build_int_const(7u64, ValueType::I64).expect("a");
    let c = b.build_int_const(9u64, ValueType::I64).expect("c");
    let eq = b
        .build_int_cmp_operation(a, c, IntCmpOp::Equal, ValueType::I64)
        .expect("eq");
    b.build_return(Some(eq), &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::new(&function);
    let plain = m
        .find_all(&inputs_of_width(64, int_eq(anything(), anything())).into_pattern())
        .unwrap();
    assert_eq!(plain.len(), 1);
    let wrapped = m
        .find_all(&inputs_of_width(64, one_of![int_eq(anything(), anything())]).into_pattern())
        .unwrap();
    assert_eq!(wrapped.len(), 1, "the arm is not an operand");
    // The named width still has to be the right one.
    assert!(
        m.find_all(&inputs_of_width(32, one_of![int_eq(anything(), anything())]).into_pattern())
            .unwrap()
            .is_empty()
    );
}

/// The inputs the width is read off are the matched node's, not what its
/// operand sub-patterns bound: under `ignore_casts` an operand reached through
/// a cast is narrower than the input that feeds the node, and the node's input
/// is what `inputs_of_width` names.
#[test]
fn inputs_of_width_reads_the_input_not_the_operand_behind_a_cast() {
    use strider_ir::IntBinaryOp;
    use strider_pattern::{anything, inputs_of_width, int_add};

    let mut t = super::support::Tb::empty();
    let a = t.u32(1);
    let b = t.u32(2);
    // I32 add, widened to I64 and added to an I64 value.
    let narrow = t.int_bin_at(a, b, IntBinaryOp::Add, ValueType::I32);
    let wide = t.zext_to(narrow, ValueType::I64);
    let other = t.u64(9);
    let outer = t.add(wide, other);
    let function = t.ret_val(outer);

    let m = Matcher::new(&function);
    let pat = inputs_of_width(64, int_add(int_add(anything(), anything()), anything()))
        .into_pattern()
        .ignore_casts();
    assert_eq!(
        m.find_all(&pat).unwrap().len(),
        1,
        "the outer add's own inputs are both I64"
    );
}
