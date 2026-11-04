//! The value-op constructor vocabulary, emitted over `PyPat` for
//! `strider.pattern` and over `PyTemplate` for `strider.template`.

/// One `#[pyfunction]` per row, over the `bin` / `un` / `ex` / `Kind` in
/// scope. `$ty` is the module's wrapper, `$verb` the doc prefix, `$comm` the
/// note a `binary_comm` row appends.
macro_rules! value_op {
    ($ty:ident, $verb:literal, $comm:literal, binary $name:ident, $doc:literal) => {
        $crate::value_ops::value_op!(@bin $ty, $name, concat!($verb, ": ", $doc));
    };
    ($ty:ident, $verb:literal, $comm:literal, binary_comm $name:ident, $doc:literal) => {
        $crate::value_ops::value_op!(@bin $ty, $name, concat!($verb, ": ", $doc, $comm));
    };
    (@bin $ty:ident, $name:ident, $doc:expr) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            let make = |l, r| ex(strider_pattern::$name(l, r));
            let name = concat!(stringify!($name), "()");
            <$ty>::from(bin(name, Kind::Build, Box::new(make), l, r))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, unary $name:ident, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            let make = |x| ex(strider_pattern::$name(x));
            let name = concat!(stringify!($name), "()");
            <$ty>::from(un(name, Kind::Build, Box::new(make), operand))
        }
    };
    (
        $ty:ident, $verb:literal, $comm:literal,
        binary_named $name:ident, $parse:path, $doc:literal
    ) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<$ty> {
            let op = $parse(op)?;
            let make = move |l, r| ex(strider_pattern::$name(op, l, r));
            let name = concat!(stringify!($name), "()");
            Ok(<$ty>::from(bin(name, Kind::Build, Box::new(make), l, r)))
        }
    };
    (
        $ty:ident, $verb:literal, $comm:literal,
        unary_named $name:ident, $parse:path, $doc:literal
    ) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(op: &str, operand: Py<PyAny>) -> PyResult<$ty> {
            let op = $parse(op)?;
            let make = move |x| ex(strider_pattern::$name(op, x));
            let name = concat!(stringify!($name), "()");
            Ok(<$ty>::from(un(name, Kind::Build, Box::new(make), operand)))
        }
    };
}

/// The functions for each row, plus their registrar.
macro_rules! emit_value_ops {
    ($ty:ident, $verb:literal, $comm:literal, $([$kind:tt $name:ident $($rest:tt)*])*) => {
        $( $crate::value_ops::value_op!($ty, $verb, $comm, $kind $name $($rest)*); )*

        /// Bind every value-op constructor into `m`.
        pub fn register_value_ops(m: &Bound<'_, PyModule>) -> PyResult<()> {
            $( m.add_function(pyo3::wrap_pyfunction!($name, m)?)?; )*
            Ok(())
        }
    };
}

/// The vocabulary. A row is `[shape name, doc]`; the Python name is the name
/// of the `strider_pattern` factory it lowers through.
macro_rules! value_ops {
    ($ty:ident, $verb:literal, $comm:literal) => {
        $crate::value_ops::emit_value_ops! { $ty, $verb, $comm,
            [binary_comm int_add, "`IntBinaryOp::Add` (`a + b`)."]
            [binary int_sub, "integer subtraction `a - b`."]
            [binary_comm int_mul, "`IntBinaryOp::Mul` (`a * b`)."]
            [binary int_div, "`IntBinaryOp::Div` (unsigned `a / b`)."]
            [binary int_sdiv, "`IntBinaryOp::Sdiv` (signed `a / b`)."]
            [binary int_rem, "`IntBinaryOp::Rem` (unsigned `a % b`)."]
            [binary int_srem, "`IntBinaryOp::Srem` (signed `a % b`)."]
            [binary int_shl, "`IntBinaryOp::ShiftLeft` (`a << b`)."]
            [binary int_shr, "`IntBinaryOp::ShiftRight` (`a >> b`)."]
            [binary int_sshr, "`IntBinaryOp::SShiftRight` (arithmetic `a >> b`)."]
            [binary_comm int_and, "`IntBinaryOp::And` (`a & b`)."]
            [binary_comm int_or, "`IntBinaryOp::Or` (`a | b`)."]
            [binary_comm int_xor, "`IntBinaryOp::Xor` (`a ^ b`)."]
            [binary_named int_cmp, crate::pattern::parse_int_cmp_op,
                "the `IntCmpOp` variant `op` names, e.g. `\"Equal\"` or `\"Sless\"`."]
            [binary_comm int_eq, "`IntCmpOp::Equal` (`a == b`)."]
            [binary int_lt, "`IntCmpOp::Less` (unsigned `a < b`)."]
            [binary int_slt, "`IntCmpOp::Sless` (signed `a < b`)."]
            [binary_comm int_carry, "`IntCmpOp::Carry` (unsigned add carry-out)."]
            [binary_comm int_scarry, "`IntCmpOp::Scarry` (signed add overflow)."]
            [binary int_sborrow, "`IntCmpOp::Sborrow` (signed subtract overflow)."]
            [binary int_ne, "integer `a != b`, the lifter's `Xor(Equal(a, b), 1)`."]
            [binary int_le, "unsigned `a <= b`, the lifter's `Xor(Less(b, a), 1)`."]
            [binary int_sle, "signed `a <= b`, the lifter's `Xor(Sless(b, a), 1)`."]
            [unary int_neg, "`IntUnaryOp::Neg`, two's-complement negation (`-x`)."]
            [unary int_not, "bitwise complement `~x`."]
            [unary int_popcount, "`Popcount`, the count of set bits."]
            [unary int_lzcount, "`Lzcount`, the count of leading zero bits."]
            [binary_comm bool_and, "boolean `a && b` (`IntBinaryOp::And` at `I1`)."]
            [binary_comm bool_or, "boolean `a || b` (`IntBinaryOp::Or` at `I1`)."]
            [binary_comm bool_xor, "boolean `a ^ b` (`IntBinaryOp::Xor` at `I1`)."]
            [unary bool_not, "boolean negation `!x`."]
            [binary_comm float_add, "`FloatBinaryOp::Add` (`a + b`)."]
            [binary float_sub, "float subtraction `a - b`."]
            [binary_comm float_mul, "`FloatBinaryOp::Mul` (`a * b`)."]
            [binary float_div, "`FloatBinaryOp::Div` (`a / b`)."]
            [unary float_neg, "`FloatUnaryOp::Neg` (`-x`)."]
            [unary float_abs, "`FloatUnaryOp::Abs` (`fabs(x)`)."]
            [unary float_sqrt, "`FloatUnaryOp::Sqrt` (`sqrt(x)`)."]
            [unary float_ceil, "`FloatUnaryOp::Ceil` (`ceil(x)`)."]
            [unary float_floor, "`FloatUnaryOp::Floor` (`floor(x)`)."]
            [unary float_round, "`FloatUnaryOp::Round`, round to nearest with ties away from zero."]
            [binary_comm float_eq, "`FloatCmpOp::Equal` (`a == b`)."]
            [binary float_lt, "`FloatCmpOp::Less` (`a < b`)."]
            [binary float_ne, "float `a != b`, the lifter's `Xor(FloatEqual(a, b), 1)`."]
            [unary int_to_float,
                "`IntToFloat`, a signed integer to the nearest representable float."]
            [unary float_to_int, "`FloatToInt`, truncating toward zero."]
            [unary float_to_float, "`FloatToFloat`, a float to float re-width."]
            [unary int_bits_to_float, "`IntBitsToFloat`, reinterpreting int bits."]
            [unary float_bits_to_int, "`FloatBitsToInt`, reinterpreting float bits."]
            [unary int_truncate, "`Truncate`, narrowing an integer."]
            [unary int_zero_extend, "`Extend(ZeroExtend)`."]
            [unary int_sign_extend, "`Extend(SignExtend)`."]
            [unary_named int_extend, crate::pattern::parse_extend_op,
                "the `Extend` variant `op` names: `\"ZeroExtend\"` or `\"SignExtend\"`."]
        }
    };
}

pub(crate) use {emit_value_ops, value_op, value_ops};
