//! The value-op constructor vocabulary, emitted over `PyPat` for
//! `strider.pattern` and over `PyTemplate` for `strider.template`.

/// A row's docstring: `$verb` then `$doc`, plus the row's note in the
/// `pattern` module only.
macro_rules! op_doc {
    ($verb:literal, pattern, $doc:literal $(, $note:literal)?) => {
        concat!($verb, " ", $doc $(, $note)?)
    };
    ($verb:literal, $mode:ident, $doc:literal $(, $note:literal)?) => {
        concat!($verb, " ", $doc)
    };
}

/// One `#[pyfunction]` per row, over the `bin` / `un` / `ex` / `Kind` in
/// scope. `$ty` is the module's wrapper, `$verb` the doc prefix, `$mode`
/// the module (`pattern` or `template`).
macro_rules! value_op {
    ($ty:ident, $verb:literal, $mode:ident, binary $name:ident, $($doc:literal),+) => {
        #[doc = $crate::value_ops::op_doc!($verb, $mode, $($doc),+)]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            let make = |l, r| ex(strider_pattern::$name(l, r));
            let name = concat!(stringify!($name), "()");
            <$ty>::from(bin(name, Kind::Build, Box::new(make), l, r))
        }
    };
    ($ty:ident, $verb:literal, $mode:ident, unary $name:ident, $doc:literal) => {
        #[doc = $crate::value_ops::op_doc!($verb, $mode, $doc)]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            let make = |x| ex(strider_pattern::$name(x));
            let name = concat!(stringify!($name), "()");
            <$ty>::from(un(name, Kind::Build, Box::new(make), operand))
        }
    };
    (
        $ty:ident, $verb:literal, $mode:ident,
        binary_named $name:ident, $parse:path, $doc:literal
    ) => {
        #[doc = $crate::value_ops::op_doc!($verb, $mode, $doc)]
        #[pyfunction]
        pub fn $name(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<$ty> {
            let op = $parse(op)?;
            let make = move |l, r| ex(strider_pattern::$name(op, l, r));
            let name = concat!(stringify!($name), "()");
            Ok(<$ty>::from(bin(name, Kind::Build, Box::new(make), l, r)))
        }
    };
    (
        $ty:ident, $verb:literal, $mode:ident,
        unary_named $name:ident, $parse:path, $doc:literal
    ) => {
        #[doc = $crate::value_ops::op_doc!($verb, $mode, $doc)]
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
    ($ty:ident, $verb:literal, $mode:ident, $([$kind:tt $name:ident $($rest:tt)*])*) => {
        $( $crate::value_ops::value_op!($ty, $verb, $mode, $kind $name $($rest)*); )*

        /// Bind every value-op constructor into `m`.
        pub fn register_value_ops(m: &Bound<'_, PyModule>) -> PyResult<()> {
            $( m.add_function(pyo3::wrap_pyfunction!($name, m)?)?; )*
            Ok(())
        }
    };
}

/// The vocabulary. A row is `[shape name, doc]` plus an optional note only
/// `strider.pattern` appends; the Python name is the name of the
/// `strider_pattern` factory it lowers through.
macro_rules! value_ops {
    ($ty:ident, $verb:literal, $mode:ident) => {
        $crate::value_ops::emit_value_ops! { $ty, $verb, $mode,
            [binary int_add, "integer addition.", " Commutative."]
            [binary int_sub, "integer subtraction."]
            [binary int_mul, "integer multiplication.", " Commutative."]
            [binary int_div, "unsigned integer division."]
            [binary int_sdiv, "signed integer division."]
            [binary int_rem, "unsigned integer remainder."]
            [binary int_srem, "signed integer remainder."]
            [binary int_shl, "a left shift."]
            [binary int_shr, "a logical (zero-filling) right shift."]
            [binary int_sshr, "an arithmetic (sign-filling) right shift."]
            [binary int_and, "a bitwise AND.", " Commutative."]
            [binary int_or, "a bitwise OR.", " Commutative."]
            [binary int_xor, "a bitwise XOR.", " Commutative."]
            [binary_named int_cmp, crate::pattern::parse_int_cmp_op, "a named integer comparison, e.g. `\"Equal\"`, `\"Less\"`, `\"Sless\"`, `\"Carry\"`, `\"Scarry\"`, `\"Sborrow\"`."]
            [binary int_eq, "an integer equality test.", " Commutative."]
            [binary int_lt, "an unsigned less-than test."]
            [binary int_slt, "a signed less-than test."]
            [binary int_carry, "an unsigned-addition carry test.", " Commutative."]
            [binary int_scarry, "a signed-addition overflow test.", " Commutative."]
            [binary int_sborrow, "a signed-subtraction overflow test."]
            [binary int_ne, "an integer inequality test, the lifter's `Xor(Equal(l, r), 1)`.", " `.ordered()` pins the `Equal`."]
            [binary int_le, "an unsigned less-or-equal test, the lifter's `Xor(Less(r, l), 1)`."]
            [binary int_sle, "a signed less-or-equal test, the lifter's `Xor(Sless(r, l), 1)`."]
            [unary int_neg, "arithmetic negation (`-x`)."]
            [unary int_not, "bitwise complement (`~x`)."]
            [unary int_popcount, "a set-bit count."]
            [unary int_lzcount, "a leading-zero count."]
            [binary bool_and, "a logical AND on 1-bit values.", " Commutative."]
            [binary bool_or, "a logical OR on 1-bit values.", " Commutative."]
            [binary bool_xor, "a logical XOR on 1-bit values.", " Commutative."]
            [unary bool_not, "a logical NOT on a 1-bit value."]
            [binary float_add, "float addition.", " Commutative."]
            [binary float_sub, "float subtraction."]
            [binary float_mul, "float multiplication.", " Commutative."]
            [binary float_div, "float division."]
            [unary float_neg, "float negation."]
            [unary float_abs, "float absolute value."]
            [unary float_sqrt, "a float square root."]
            [unary float_ceil, "a round-toward-positive-infinity."]
            [unary float_floor, "a round-toward-negative-infinity."]
            [unary float_round, "a round to nearest, ties away from zero."]
            [binary float_eq, "a float equality test.", " Commutative."]
            [binary float_lt, "a float less-than test."]
            [binary float_ne, "a float inequality test, the lifter's `Xor(FloatEqual(l, r), 1)`.", " `.ordered()` pins the `FloatEqual`."]
            [unary int_to_float, "a signed-integer conversion to the nearest representable float."]
            [unary float_to_int, "a float-to-integer conversion, truncating toward zero."]
            [unary float_to_float, "a float-to-float reprecision."]
            [unary int_bits_to_float, "a same-width reinterpretation of integer bits as a float."]
            [unary float_bits_to_int, "a same-width reinterpretation of float bits as an integer."]
            [unary int_truncate, "a narrowing to the output width, keeping the low bits."]
            [unary int_zero_extend, "a widening that fills the new high bits with zero."]
            [unary int_sign_extend, "a widening that replicates the sign bit."]
            [unary_named int_extend, crate::pattern::parse_extend_op, "a widening of the kind named by `op`: `\"ZeroExtend\"` or `\"SignExtend\"`."]
        }
    };
}

pub(crate) use {emit_value_ops, op_doc, value_op, value_ops};
