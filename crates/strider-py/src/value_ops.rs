//! The value-op constructor vocabulary, stated once and emitted twice: over
//! `PyPat` for `strider.pattern` (matching) and over `PyTemplate` for
//! `strider.template` (building). Both modules expose the same 49 names with
//! the same arities and the same `PatRepr` variants.

/// One `#[pyfunction]` per row. `$ty` is the module's wrapper, `$verb` the
/// doc prefix, `$comm` the note a `binary_comm` row appends (empty where
/// operand order is not a matching question).
macro_rules! value_op {
    ($ty:ident, $verb:literal, $comm:literal, binary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, binary_comm $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc, $comm)]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, unary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, operand))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, binary_bare $name:ident, $repr:ident, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr(l, r))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, unary_bare $name:ident, $repr:ident, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr(operand))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, binary_named $name:ident, $repr:ident, $parse:path, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<$ty> {
            Ok(<$ty>::from_repr(PatRepr::$repr($parse(op)?, l, r)))
        }
    };
    ($ty:ident, $verb:literal, $comm:literal, unary_named $name:ident, $repr:ident, $parse:path, $doc:literal) => {
        #[doc = concat!($verb, ": ", $doc)]
        #[pyfunction]
        pub fn $name(op: &str, operand: Py<PyAny>) -> PyResult<$ty> {
            Ok(<$ty>::from_repr(PatRepr::$repr($parse(op)?, operand)))
        }
    };
}

/// Rows in, functions plus their registrar out. Split from [`value_ops`] so
/// the table is written once and walked twice.
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

/// The vocabulary. A row is `[shape name, PatRepr variant, op, doc]`; the
/// Python name is the Rust name.
macro_rules! value_ops {
    ($ty:ident, $verb:literal, $comm:literal) => {
        $crate::value_ops::emit_value_ops! { $ty, $verb, $comm,
            [binary_comm int_add, IntBinary, strider_ir::IntBinaryOp::Add,
                "`IntBinaryOp::Add` (`a + b`)."]
            [binary_bare int_sub, Sub,
                "integer subtraction `a - b`."]
            [binary_comm int_mul, IntBinary, strider_ir::IntBinaryOp::Mul,
                "`IntBinaryOp::Mul` (`a * b`)."]
            [binary int_div, IntBinary, strider_ir::IntBinaryOp::Div,
                "`IntBinaryOp::Div` (unsigned `a / b`)."]
            [binary int_sdiv, IntBinary, strider_ir::IntBinaryOp::Sdiv,
                "`IntBinaryOp::Sdiv` (signed `a / b`)."]
            [binary int_rem, IntBinary, strider_ir::IntBinaryOp::Rem,
                "`IntBinaryOp::Rem` (unsigned `a % b`)."]
            [binary int_srem, IntBinary, strider_ir::IntBinaryOp::Srem,
                "`IntBinaryOp::Srem` (signed `a % b`)."]
            [binary int_shl, IntBinary, strider_ir::IntBinaryOp::ShiftLeft,
                "`IntBinaryOp::ShiftLeft` (`a << b`)."]
            [binary int_shr, IntBinary, strider_ir::IntBinaryOp::ShiftRight,
                "`IntBinaryOp::ShiftRight` (`a >> b`)."]
            [binary int_sshr, IntBinary, strider_ir::IntBinaryOp::SShiftRight,
                "`IntBinaryOp::SShiftRight` (arithmetic `a >> b`)."]
            [binary_comm int_and, IntBinary, strider_ir::IntBinaryOp::And,
                "`IntBinaryOp::And` (`a & b`)."]
            [binary_comm int_or, IntBinary, strider_ir::IntBinaryOp::Or,
                "`IntBinaryOp::Or` (`a | b`)."]
            [binary_comm int_xor, IntBinary, strider_ir::IntBinaryOp::Xor,
                "`IntBinaryOp::Xor` (`a ^ b`)."]
            [binary_named int_cmp, IntCmp, crate::pattern::parse_int_cmp_op,
                "the `IntCmpOp` variant `op` names, e.g. `\"Equal\"` or `\"Sless\"`."]
            [binary_comm int_eq, IntCmp, strider_ir::IntCmpOp::Equal,
                "`IntCmpOp::Equal` (`a == b`)."]
            [binary int_lt, IntCmp, strider_ir::IntCmpOp::Less,
                "`IntCmpOp::Less` (unsigned `a < b`)."]
            [binary int_slt, IntCmp, strider_ir::IntCmpOp::Sless,
                "`IntCmpOp::Sless` (signed `a < b`)."]
            [binary_comm int_carry, IntCmp, strider_ir::IntCmpOp::Carry,
                "`IntCmpOp::Carry` (unsigned add carry-out)."]
            [binary_comm int_scarry, IntCmp, strider_ir::IntCmpOp::Scarry,
                "`IntCmpOp::Scarry` (signed add overflow)."]
            [binary int_sborrow, IntCmp, strider_ir::IntCmpOp::Sborrow,
                "`IntCmpOp::Sborrow` (signed subtract overflow)."]
            [unary int_neg, IntUnary, IntUnaryKind::Neg,
                "`IntUnaryOp::Neg`, two's-complement negation (`-x`)."]
            [unary_bare int_not, BitNot,
                "bitwise complement `~x`."]
            [unary int_popcount, IntUnary, IntUnaryKind::Popcount,
                "`Popcount`, the count of set bits."]
            [unary int_lzcount, IntUnary, IntUnaryKind::Lzcount,
                "`Lzcount`, the count of leading zero bits."]
            [binary_comm bool_and, BoolBinary, strider_ir::IntBinaryOp::And,
                "boolean `a && b` (`IntBinaryOp::And` at `I1`)."]
            [binary_comm bool_or, BoolBinary, strider_ir::IntBinaryOp::Or,
                "boolean `a || b` (`IntBinaryOp::Or` at `I1`)."]
            [binary_comm bool_xor, BoolBinary, strider_ir::IntBinaryOp::Xor,
                "boolean `a ^ b` (`IntBinaryOp::Xor` at `I1`)."]
            [unary_bare bool_not, BoolNot,
                "boolean negation `!x`."]
            [binary_comm float_add, FloatBinary, strider_ir::FloatBinaryOp::Add,
                "`FloatBinaryOp::Add` (`a + b`)."]
            [binary_bare float_sub, FloatSub,
                "float subtraction `a - b`."]
            [binary_comm float_mul, FloatBinary, strider_ir::FloatBinaryOp::Mul,
                "`FloatBinaryOp::Mul` (`a * b`)."]
            [binary float_div, FloatBinary, strider_ir::FloatBinaryOp::Div,
                "`FloatBinaryOp::Div` (`a / b`)."]
            [unary float_neg, FloatUnary, FloatUnaryKind::Neg,
                "`FloatUnaryOp::Neg` (`-x`)."]
            [unary float_abs, FloatUnary, FloatUnaryKind::Abs,
                "`FloatUnaryOp::Abs` (`fabs(x)`)."]
            [unary float_sqrt, FloatUnary, FloatUnaryKind::Sqrt,
                "`FloatUnaryOp::Sqrt` (`sqrt(x)`)."]
            [unary float_ceil, FloatUnary, FloatUnaryKind::Ceil,
                "`FloatUnaryOp::Ceil` (`ceil(x)`)."]
            [unary float_floor, FloatUnary, FloatUnaryKind::Floor,
                "`FloatUnaryOp::Floor` (`floor(x)`)."]
            [unary float_round, FloatUnary, FloatUnaryKind::Round,
                "`FloatUnaryOp::Round`, round to nearest with ties away from zero."]
            [binary_comm float_eq, FloatCmp, strider_ir::FloatCmpOp::Equal,
                "`FloatCmpOp::Equal` (`a == b`)."]
            [binary float_lt, FloatCmp, strider_ir::FloatCmpOp::Less,
                "`FloatCmpOp::Less` (`a < b`)."]
            [unary int_to_float, Cast, CastKind::IntToFloat,
                "`IntToFloat`, a signed integer to the nearest representable float."]
            [unary float_to_int, Cast, CastKind::FloatToInt,
                "`FloatToInt`, truncating toward zero."]
            [unary float_to_float, Cast, CastKind::FloatToFloat,
                "`FloatToFloat`, a float to float re-width."]
            [unary int_bits_to_float, Cast, CastKind::IntBitsToFloat,
                "`IntBitsToFloat`, reinterpreting int bits."]
            [unary float_bits_to_int, Cast, CastKind::FloatBitsToInt,
                "`FloatBitsToInt`, reinterpreting float bits."]
            [unary int_truncate, Cast, CastKind::Truncate,
                "`Truncate`, narrowing an integer."]
            [unary int_zero_extend, Cast, CastKind::ZeroExtend,
                "`Extend(ZeroExtend)`."]
            [unary int_sign_extend, Cast, CastKind::SignExtend,
                "`Extend(SignExtend)`."]
            [unary_named int_extend, Extend, crate::pattern::parse_extend_op,
                "the `Extend` variant `op` names: `\"zero\"` or `\"sign\"`."]
        }
    };
}

pub(crate) use {emit_value_ops, value_op, value_ops};
