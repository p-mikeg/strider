//! The build-side rewrite-RHS DSL, `strider.template`.
//!
//! Python's `strider.pattern` fuses match and build: a `Pat` compiles to
//! either a `Pattern` or a `Template`.  This module makes the split explicit
//! at the Python type level without duplicating the compile pipeline:
//! [`PyTemplate`] wraps the same `Rc<PatRepr>` a `Pat` does, but the free
//! functions here construct only the build-valid subset of variants.  No
//! `.when()`, no commutativity toggle, no `.ordered()`: those are match-only
//! concepts with no build-side meaning.

use pyo3::prelude::*;
#[allow(unused_imports)]
use pyo3_stub_gen::derive::gen_stub_pyclass;

use crate::pattern::{CastKind, FloatUnaryKind, IntUnaryKind, PatRepr, PyCapture};

/// A type-checked rewrite-RHS expression. Construct via the free functions in
/// `strider.template` (`var(c)`, `add(...)`, `int_const`, ...); pass as
/// `replace` to `Function.rewrite` / `rewrite_all`.
#[gen_stub_pyclass]
#[pyclass(name = "Template", module = "strider.template", unsendable)]
pub struct PyTemplate {
    pub(crate) repr: std::rc::Rc<PatRepr>,
}

impl PyTemplate {
    pub(crate) fn from_repr(repr: PatRepr) -> Self {
        Self {
            repr: std::rc::Rc::new(repr),
        }
    }

    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<strider_pattern::Template> {
        self.repr.to_template(py)
    }
}

#[pymethods]
impl PyTemplate {
    fn __repr__(&self) -> String {
        "Template(...)".to_string()
    }
}

macro_rules! tpl_fn {
    (binary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
            PyTemplate::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    (binary $name:ident = $py:literal, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py)]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
            PyTemplate::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    (unary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> PyTemplate {
            PyTemplate::from_repr(PatRepr::$repr($op, operand))
        }
    };
}

/// Substituted at rewrite time by the node bound to `c` on the matched LHS.
/// The one build-valid wildcard; there is no build-side `any()`.
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::Var(c.inner))
}

/// Build an `IntConst` whose stored value, masked to the output width, is
/// `value`. Bit-pattern equality; negatives use the sign-extended form.
#[pyfunction]
pub fn int_const(value: i128) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::IntConst(value as u128))
}

/// Build a signed `IntConst`. A `value` outside the `i64` range raises
/// `StriderError`.
#[pyfunction]
pub fn signed_int_const(value: i128) -> PyResult<PyTemplate> {
    let v = crate::pattern::checked_signed_i64(value)?;
    Ok(PyTemplate::from_repr(PatRepr::SignedIntConst(v)))
}

/// Build an `I1` boolean constant equal to `value`.
#[pyfunction]
pub fn bool_const(value: bool) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::BoolConst(value))
}

/// Build a `FloatConst` with raw bits `bits`.
#[pyfunction]
pub fn float_const(bits: u64) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::FloatConst(bits))
}

tpl_fn!(binary
    add, IntBinary, strider_ir::IntBinaryOp::Add,
    "Build: `IntBinaryOp::Add` (`a + b`)."
);
tpl_fn!(binary
    mul, IntBinary, strider_ir::IntBinaryOp::Mul,
    "Build: `IntBinaryOp::Mul` (`a * b`)."
);
tpl_fn!(binary div, IntBinary, strider_ir::IntBinaryOp::Div,
    "Build: `IntBinaryOp::Div` (unsigned `a / b`).");
tpl_fn!(binary sdiv, IntBinary, strider_ir::IntBinaryOp::Sdiv,
    "Build: `IntBinaryOp::Sdiv` (signed `a / b`).");
tpl_fn!(binary rem, IntBinary, strider_ir::IntBinaryOp::Rem,
    "Build: `IntBinaryOp::Rem` (unsigned `a % b`).");
tpl_fn!(binary srem, IntBinary, strider_ir::IntBinaryOp::Srem,
    "Build: `IntBinaryOp::Srem` (signed `a % b`).");
tpl_fn!(binary
    shl, IntBinary, strider_ir::IntBinaryOp::ShiftLeft,
    "Build: `IntBinaryOp::ShiftLeft` (`a << b`)."
);
tpl_fn!(binary
    shr, IntBinary, strider_ir::IntBinaryOp::ShiftRight,
    "Build: `IntBinaryOp::ShiftRight` (`a >> b`)."
);
tpl_fn!(binary
    sshr, IntBinary, strider_ir::IntBinaryOp::SShiftRight,
    "Build: `IntBinaryOp::SShiftRight` (arithmetic `a >> b`)."
);
tpl_fn!(binary
    and_ = "int_and", IntBinary, strider_ir::IntBinaryOp::And,
    "Build: `IntBinaryOp::And` (`a & b`)."
);
tpl_fn!(binary
    or_ = "int_or", IntBinary, strider_ir::IntBinaryOp::Or,
    "Build: `IntBinaryOp::Or` (`a | b`)."
);
tpl_fn!(binary
    xor = "int_xor", IntBinary, strider_ir::IntBinaryOp::Xor,
    "Build: `IntBinaryOp::Xor` (`a ^ b`)."
);

/// Build integer subtraction `a - b` (lowers to `Add(a, Neg(b))`, the
/// lifter-canonical shape).
#[pyfunction]
pub fn sub(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::Sub(l, r))
}

/// Build a specific `IntCmpOp` variant by name (e.g. `"Equal"`, `"Less"`,
/// `"Sless"`, `"Carry"`, `"Scarry"`, `"Sborrow"`).
#[pyfunction]
pub fn int_cmp(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyTemplate> {
    let cmp_op = crate::pattern::parse_int_cmp_op(op)?;
    Ok(PyTemplate::from_repr(PatRepr::IntCmp(cmp_op, l, r)))
}

tpl_fn!(binary
    int_eq, IntCmp, strider_ir::IntCmpOp::Equal,
    "Build: `IntCmpOp::Equal` (`a == b`)."
);
tpl_fn!(binary
    int_lt, IntCmp, strider_ir::IntCmpOp::Less,
    "Build: `IntCmpOp::Less` (unsigned `a < b`)."
);
tpl_fn!(binary
    int_slt, IntCmp, strider_ir::IntCmpOp::Sless,
    "Build: `IntCmpOp::Sless` (signed `a < b`)."
);
tpl_fn!(binary
    int_carry, IntCmp, strider_ir::IntCmpOp::Carry,
    "Build: `IntCmpOp::Carry` (unsigned add carry-out)."
);
tpl_fn!(binary
    int_scarry, IntCmp, strider_ir::IntCmpOp::Scarry,
    "Build: `IntCmpOp::Scarry` (signed add overflow)."
);
tpl_fn!(binary
    int_sborrow, IntCmp, strider_ir::IntCmpOp::Sborrow,
    "Build: `IntCmpOp::Sborrow` (signed subtract overflow)."
);

/// Build `IntUnaryOp::Neg`, two's-complement negation (`-x`).
#[pyfunction]
pub fn neg(operand: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::IntUnary(IntUnaryKind::Neg, operand))
}

/// Build bitwise complement (`~x`), i.e. `Xor(x, all_ones)`.
#[pyfunction(name = "int_not")]
pub fn bit_not(operand: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::BitNot(operand))
}

/// Build `Popcount`, the count of set bits.
#[pyfunction]
pub fn popcount(operand: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::IntUnary(IntUnaryKind::Popcount, operand))
}

/// Build `Lzcount`, the count of leading zero bits.
#[pyfunction]
pub fn lzcount(operand: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::IntUnary(IntUnaryKind::Lzcount, operand))
}

tpl_fn!(binary
    bool_and, BoolBinary, strider_ir::IntBinaryOp::And,
    "Build: boolean `a && b` (`IntBinaryOp::And` at `I1`)."
);
tpl_fn!(binary
    bool_or, BoolBinary, strider_ir::IntBinaryOp::Or,
    "Build: boolean `a || b` (`IntBinaryOp::Or` at `I1`)."
);
tpl_fn!(binary
    bool_xor, BoolBinary, strider_ir::IntBinaryOp::Xor,
    "Build: boolean `a ^ b` (`IntBinaryOp::Xor` at `I1`)."
);

/// Build boolean negation (`!x`), i.e. `Xor(x, IntConst(1)):I1`.
#[pyfunction]
pub fn bool_not(operand: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::BoolNot(operand))
}

tpl_fn!(binary
    float_add, FloatBinary, strider_ir::FloatBinaryOp::Add,
    "Build: `FloatBinaryOp::Add` (`a + b`)."
);
tpl_fn!(binary
    float_mul, FloatBinary, strider_ir::FloatBinaryOp::Mul,
    "Build: `FloatBinaryOp::Mul` (`a * b`)."
);
tpl_fn!(binary float_div, FloatBinary, strider_ir::FloatBinaryOp::Div,
    "Build: `FloatBinaryOp::Div` (`a / b`).");

/// Build float subtraction `a - b` (lowers to `FloatAdd(a, Neg(b))`).
#[pyfunction]
pub fn float_sub(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::FloatSub(l, r))
}

tpl_fn!(unary float_neg, FloatUnary, FloatUnaryKind::Neg,
    "Build: `FloatUnaryOp::Neg` (`-x`).");
tpl_fn!(unary float_abs, FloatUnary, FloatUnaryKind::Abs,
    "Build: `FloatUnaryOp::Abs` (`fabs(x)`).");
tpl_fn!(unary
    float_sqrt, FloatUnary, FloatUnaryKind::Sqrt,
    "Build: `FloatUnaryOp::Sqrt` (`sqrt(x)`)."
);
tpl_fn!(unary
    float_ceil, FloatUnary, FloatUnaryKind::Ceil,
    "Build: `FloatUnaryOp::Ceil` (`ceil(x)`)."
);
tpl_fn!(unary
    float_floor, FloatUnary, FloatUnaryKind::Floor,
    "Build: `FloatUnaryOp::Floor` (`floor(x)`)."
);
tpl_fn!(unary
    float_round, FloatUnary, FloatUnaryKind::Round,
    "Build: `FloatUnaryOp::Round` (round-to-nearest-even)."
);

/// Build: `FloatCmpOp::Equal` (`a == b`).
#[pyfunction]
pub fn float_eq(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::FloatCmp(strider_ir::FloatCmpOp::Equal, l, r))
}

/// Build: `FloatCmpOp::Less` (`a < b`).
#[pyfunction]
pub fn float_lt(l: Py<PyAny>, r: Py<PyAny>) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::FloatCmp(strider_ir::FloatCmpOp::Less, l, r))
}

tpl_fn!(unary
    int_to_float, Cast, CastKind::IntToFloat,
    "Build: `IntToFloat` — int→float conversion."
);
tpl_fn!(unary
    float_to_int, Cast, CastKind::FloatToInt,
    "Build: `FloatToInt` — float→int conversion."
);
tpl_fn!(unary
    float_to_float, Cast, CastKind::FloatToFloat,
    "Build: `FloatToFloat` — float→float re-width."
);
tpl_fn!(unary
    int_bits_to_float, Cast, CastKind::IntBitsToFloat,
    "Build: `IntBitsToFloat` — reinterpret int bits."
);
tpl_fn!(unary
    float_bits_to_int, Cast, CastKind::FloatBitsToInt,
    "Build: `FloatBitsToInt` — reinterpret float bits."
);
tpl_fn!(unary
    truncate, Cast, CastKind::Truncate,
    "Build: `Truncate` — narrow an integer."
);
tpl_fn!(unary zero_extend, Cast, CastKind::ZeroExtend, "Build: `Extend(ZeroExtend)`.");
tpl_fn!(unary sign_extend, Cast, CastKind::SignExtend, "Build: `Extend(SignExtend)`.");

/// `extend(op, operand)` where `op` is "zero" / "zero_extend" / "sign" /
/// "sign_extend".
#[pyfunction]
pub fn extend(op: &str, operand: Py<PyAny>) -> PyResult<PyTemplate> {
    let extend_op = crate::pattern::parse_extend_op(op)?;
    Ok(PyTemplate::from_repr(PatRepr::Extend(extend_op, operand)))
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTemplate>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, m)?)?;
        };
    }
    add_fn!(var);
    add_fn!(int_const);
    add_fn!(signed_int_const);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(add);
    add_fn!(sub);
    add_fn!(mul);
    add_fn!(div);
    add_fn!(sdiv);
    add_fn!(rem);
    add_fn!(srem);
    add_fn!(shl);
    add_fn!(shr);
    add_fn!(sshr);
    add_fn!(and_);
    add_fn!(or_);
    add_fn!(xor);
    add_fn!(int_cmp);
    add_fn!(int_eq);
    add_fn!(int_lt);
    add_fn!(int_slt);
    add_fn!(int_carry);
    add_fn!(int_scarry);
    add_fn!(int_sborrow);
    add_fn!(neg);
    add_fn!(bit_not);
    add_fn!(popcount);
    add_fn!(lzcount);
    add_fn!(bool_and);
    add_fn!(bool_or);
    add_fn!(bool_xor);
    add_fn!(bool_not);
    add_fn!(float_add);
    add_fn!(float_sub);
    add_fn!(float_mul);
    add_fn!(float_div);
    add_fn!(float_neg);
    add_fn!(float_abs);
    add_fn!(float_sqrt);
    add_fn!(float_ceil);
    add_fn!(float_floor);
    add_fn!(float_round);
    add_fn!(float_eq);
    add_fn!(float_lt);
    add_fn!(int_to_float);
    add_fn!(float_to_int);
    add_fn!(float_to_float);
    add_fn!(int_bits_to_float);
    add_fn!(float_bits_to_int);
    add_fn!(truncate);
    add_fn!(zero_extend);
    add_fn!(sign_extend);
    add_fn!(extend);
    Ok(())
}
