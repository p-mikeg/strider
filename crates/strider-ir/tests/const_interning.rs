//! Dedup is by stored representation, so two spellings of one value have to
//! reach the interner already normalised or they intern as unequal constants.

use strider_ir::ValueType;
use strider_ir_test_utils::RegisterSet;

fn function() -> strider_ir::Function {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    b.set_lift_addr(Some(0x10));
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

#[test]
fn limb_spellings_of_one_value_intern_to_one_id() {
    let mut f = function();
    let ty = ValueType::I256;
    // Short, exact, and zero-padded spellings of the same I256 value.
    let short = f.intern_int_const_limbs(&[0, 0, 5], ty);
    let exact = f.intern_int_const_limbs(&[0, 0, 5, 0], ty);
    let padded = f.intern_int_const_limbs(&[0, 0, 5, 0, 0, 0], ty);
    assert_eq!(short, exact, "a short spelling is the same constant");
    assert_eq!(exact, padded, "so is a zero-padded one");
}

#[test]
fn bits_above_the_declared_width_are_not_part_of_the_value() {
    let mut f = function();
    let ty = ValueType::I256;
    let zero = f.intern_int_const_limbs(&[0, 0, 0, 0], ty);
    // Bit 256, one past the type.
    let over = f.intern_int_const_limbs(&[0, 0, 0, 0, 1], ty);
    assert_eq!(
        zero, over,
        "a bit outside the type is not part of the value"
    );
}

#[test]
fn a_wide_spelling_that_fits_reads_back_as_a_scalar() {
    let mut f = function();
    let ty = ValueType::I128;
    // Limb 2 is outside I128, so before trimming this stored as `Wide` and
    // `int_const_u128` answered `None` for a value that is plainly 7.
    let wide = f.intern_int_const_limbs(&[7, 0, 1], ty);
    let scalar = f.intern_int_const_limbs(&[7, 0], ty);
    assert_eq!(wide, scalar, "the same I128 value however it is spelled");
}
