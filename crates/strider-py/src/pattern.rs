//! Anywhere a sub-pattern is accepted, a string works too: it interns to a
//! `Capture` so `add("x", "x")` back-references. The intern table is global
//! per process.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};
#[allow(unused_imports)]
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use strider_ir::node::ValueType as T;
use strider_pattern as sp;
use strider_pattern::matcher::{MatcherBuilder, PatValueRef};
use strider_pattern::template::{TemplateBuilder, TmplValueRef};
use strider_pattern::{
    Capture, CaptureExt, JoinConstraint, MatchPat, MemPat, Pattern, Template, TemplatePat,
    template as tpl,
};

use crate::errors::into_strider_err;

/// Binds a matched node so its value, op variant or fingerprint can be read
/// back from the `Match`. Each `Capture()` is globally unique.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: Capture,
}

#[pymethods]
impl PyCapture {
    /// A fresh, globally-unique capture variable.
    #[new]
    fn new() -> Self {
        Self {
            inner: Capture::new(),
        }
    }

    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    fn __hash__(&self) -> isize {
        self.inner.id() as i64 as isize
    }
}

// Same string in the same process interns to the same Capture, so
// `add("x", "x")` aliases and `add("x", "y")` does not. "_" and "any_" are
// reserved.
fn intern_table() -> &'static Mutex<HashMap<String, Capture>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, Capture>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn intern_str(name: &str) -> PyResult<Capture> {
    if name == "_" || name == "any_" {
        return Err(into_strider_err(anyhow::anyhow!(
            "{name:?} is reserved (use anything() / var() / _ explicitly)"
        )));
    }
    let mut table = intern_table()
        .lock()
        .map_err(|_| into_strider_err(anyhow::anyhow!("intern table lock poisoned")))?;
    Ok(*table.entry(name.to_string()).or_insert_with(Capture::new))
}

pub(crate) struct DynMatch(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>);

impl MatchPat for DynMatch {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

pub(crate) struct DynTemplate(pub(crate) Box<dyn FnOnce(&mut TemplateBuilder) -> TmplValueRef>);

impl TemplatePat for DynTemplate {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        (self.0)(b)
    }
}

/// Type-erased sub-pattern yielding a memory token.
pub(crate) struct DynMem(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>);

impl MemPat for DynMem {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum IntUnaryKind {
    Neg,
    Popcount,
    Lzcount,
}

#[derive(Clone, Copy)]
pub(crate) enum FloatUnaryKind {
    Neg,
    Abs,
    Sqrt,
    Ceil,
    Floor,
    Round,
}

#[derive(Clone, Copy)]
pub(crate) enum CastKind {
    Truncate,
    ZeroExtend,
    SignExtend,
    IntToFloat,
    FloatToInt,
    IntBitsToFloat,
    FloatBitsToInt,
    FloatToFloat,
}

/// The recursive shape of a `PyPat`; recompiled fresh on every query.
///
/// Match-only variants have no rewrite-RHS form and error from
/// `compile_template`.
pub(crate) enum PatRepr {
    Any,
    Var(Capture),
    ValueOfWidth(u32),
    InputsOfWidth(u32, Py<PyAny>),
    IntConst(u128),
    SignedIntConst(i64),
    IntConstAnyOf(Vec<u64>),
    BoolConst(bool),
    FloatConst(u64),
    AnyIntConst(Option<Capture>),
    AnyBoolConst(Option<Capture>),
    AnyFloatConst(Option<Capture>),
    InitialVar,
    InitialVarFor(rsleigh::Vn),
    /// Alternation. Match-only: a rewrite RHS must build one concrete shape.
    OneOf(Vec<Py<PyAny>>),
    IntBinary(strider_ir::IntBinaryOp, Py<PyAny>, Py<PyAny>),
    /// Lowers to `add(l, neg(r))`.
    Sub(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(x, all_ones)`.
    BitNot(Py<PyAny>),
    IntUnary(IntUnaryKind, Py<PyAny>),
    Cast(CastKind, Py<PyAny>),
    Extend(strider_ir::ExtendOp, Py<PyAny>),
    IntCmp(strider_ir::IntCmpOp, Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_eq(l, r), 1)`.
    IntNe(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_lt(r, l), 1)`. Note the swapped operands.
    IntLe(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_slt(r, l), 1)`. Note the swapped operands.
    IntSle(Py<PyAny>, Py<PyAny>),
    FloatBinary(strider_ir::FloatBinaryOp, Py<PyAny>, Py<PyAny>),
    FloatSub(Py<PyAny>, Py<PyAny>),
    FloatUnary(FloatUnaryKind, Py<PyAny>),
    FloatCmp(strider_ir::FloatCmpOp, Py<PyAny>, Py<PyAny>),
    FloatNe(Py<PyAny>, Py<PyAny>),
    FloatLe(Py<PyAny>, Py<PyAny>),
    FloatIsNan(Py<PyAny>),
    /// Boolean ops are `IntBinaryOp` at `I1`.
    BoolBinary(strider_ir::IntBinaryOp, Py<PyAny>, Py<PyAny>),
    /// Lowers to `xor(x, 1)`.
    BoolNot(Py<PyAny>),
    IntBinAny(Capture, Py<PyAny>, Py<PyAny>),
    IntUnAny(Capture, Py<PyAny>),
    IntCmpAny(Capture, Py<PyAny>, Py<PyAny>),
    BoolBinAny(Capture, Py<PyAny>, Py<PyAny>),
    FloatBinAny(Capture, Py<PyAny>, Py<PyAny>),
    FloatUnAny(Capture, Py<PyAny>),
    FloatCmpAny(Capture, Py<PyAny>, Py<PyAny>),
    Captured(Rc<PatRepr>, Capture),
    Guarded(Rc<PatRepr>, Py<PyAny>),
    OfWidth(Rc<PatRepr>, u32),
    ValueTy(Rc<PatRepr>, strider_ir::node::ValueType),
    /// A finished control / variadic [`Pattern`] from a control builder's
    /// `.into_pat()`. One-shot: consumed when queried, and not nestable as a
    /// value operand.
    Finished(Box<std::cell::RefCell<Option<Pattern>>>),
}

/// A finished pattern. Reusable: one `Pat` can drive many `find_all` /
/// rewrite calls.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Pat", module = "strider.pattern", unsendable)]
pub struct PyPat {
    pub(crate) repr: Rc<PatRepr>,
}

impl PyPat {
    pub(crate) fn from_repr(repr: PatRepr) -> Self {
        Self {
            repr: Rc::new(repr),
        }
    }
}

// An operand can be a `PyPat`, a `PyCapture`, a `str`, or a typed builder.
// The downcast happens eagerly during the recursive walk so the emitted
// `FnOnce` closes over owned data only, never a `Bound` re-borrowed across
// the GIL.
pub(crate) fn compile_operand_match(ob: &Bound<'_, PyAny>) -> PyResult<DynMatch> {
    // Every nested-operand recursion funnels through here, including the
    // pure typed-builder path (`load(addr=load(...))`) that never touches
    // `compile_repr_*`. Guarding here turns a deep-nesting Rust stack
    // overflow into a clean `StriderError`.
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    if let Ok(p) = ob.downcast::<PyPat>() {
        return p.borrow().repr.compile_match(py);
    }
    if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
        let cap = c.inner;
        return Ok(DynMatch(Box::new(move |b| {
            mc(strider_pattern::var(cap), b)
        })));
    }
    if let Ok(s) = ob.downcast::<PyString>() {
        let name = s.to_string();
        if name == "_" || name == "any_" {
            return Ok(DynMatch(Box::new(|b| strider_pattern::any().compile(b))));
        }
        let cap = intern_str(&name)?;
        return Ok(DynMatch(Box::new(move |b| {
            mc(strider_pattern::var(cap), b)
        })));
    }
    if let Ok(b) = ob.downcast::<PyLoadPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyFunctionArgPat>() {
        return Ok(b.borrow().compile_value());
    }
    if let Ok(b) = ob.downcast::<PyPhiPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyIntBinaryPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyFloatBinaryPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyBoolBinaryPat>() {
        return b.borrow().compile_value(py);
    }
    // Call / CallOther nest via their value outputs, loose by default (any
    // value output); `.res()` narrows to the declared result.
    if let Ok(b) = ob.downcast::<PyCallPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyCallOtherPat>() {
        return b.borrow().compile_value(py);
    }
    // `entry()` / `region()` produce control, wiring that edge into whatever
    // control slot they are passed to, e.g. `region().any_input(entry())`.
    if let Ok(b) = ob.downcast::<PyEntryPat>() {
        return b.borrow().compile_value(py);
    }
    if let Ok(b) = ob.downcast::<PyRegionPat>() {
        return b.borrow().compile_value(py);
    }
    Err(into_strider_err(anyhow::anyhow!(
        "expected a value pattern (Pat / Capture / str / value builder); \
         a control / variadic builder (store / ret / if / mem_phi) \
         cannot be nested as a value operand"
    )))
}

pub(crate) fn compile_operand_template(ob: &Bound<'_, PyAny>) -> PyResult<DynTemplate> {
    // Same recursion bound as `compile_operand_match`.
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    if let Ok(t) = ob.downcast::<crate::template::PyTemplate>() {
        return t.borrow().repr.compile_template(py);
    }
    // Back-compat: a bare `Pat` is still accepted as a nested RHS operand.
    // Only its build-valid subset compiles; match-only shapes error.
    if let Ok(p) = ob.downcast::<PyPat>() {
        return p.borrow().repr.compile_template(py);
    }
    if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
        let cap = c.inner;
        return Ok(DynTemplate(Box::new(move |b| template_var(b, cap))));
    }
    if let Ok(s) = ob.downcast::<PyString>() {
        let name = s.to_string();
        if name == "_" || name == "any_" {
            return Err(rhs_error("any_"));
        }
        let cap = intern_str(&name)?;
        return Ok(DynTemplate(Box::new(move |b| template_var(b, cap))));
    }
    Err(rhs_error("control / variadic builder"))
}

pub(crate) fn compile_operand_mem(ob: &Bound<'_, PyAny>) -> PyResult<DynMem> {
    // Same recursion bound as `compile_operand_match`.
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    if let Ok(b) = ob.downcast::<PyStorePat>() {
        return b.borrow().compile_mem(py);
    }
    if let Ok(b) = ob.downcast::<PyMemPhiPat>() {
        return b.borrow().compile_mem(py);
    }
    if let Ok(b) = ob.downcast::<PyCallPat>() {
        return b.borrow().compile_mem(py);
    }
    if let Ok(b) = ob.downcast::<PyCallOtherPat>() {
        return b.borrow().compile_mem(py);
    }
    // A value operand here would build a pattern that can never match: the
    // matcher's `output_ok` rejects a value output against a memory slot.
    // Reject up front rather than silently returning a dead pattern.
    Err(into_strider_err(anyhow::anyhow!(
        "a memory-input slot (`mem_in`) requires a memory producer — \
         store() / mem_phi() / call() / call_other(); got a value operand \
         ({})",
        operand_kind_name(ob)
    )))
}

fn operand_kind_name(ob: &Bound<'_, PyAny>) -> String {
    ob.get_type()
        .name()
        .map_or_else(|_| "value".to_string(), |n| n.to_string())
}

pub(crate) fn template_var(b: &mut TemplateBuilder, cap: Capture) -> TmplValueRef {
    tc(strider_pattern::var(cap), b)
}

// `mc` / `tc` disambiguate `.compile`: under a single-trait bound only that
// trait's method is in scope, so callers avoid spelling out UFCS.
fn mc<P: MatchPat>(p: P, b: &mut MatcherBuilder) -> PatValueRef {
    p.compile(b)
}

fn tc<P: TemplatePat>(p: P, b: &mut TemplateBuilder) -> TmplValueRef {
    p.compile(b)
}

fn rhs_error(kind: &str) -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "cannot use {kind} as a rewrite RHS — the RHS must be a buildable \
         value expression"
    ))
}

/// Case-insensitive: `"i1"` / `"I64"` / `"f32"`.
fn parse_value_ty(name: &str) -> PyResult<strider_ir::node::ValueType> {
    let ty = match name.to_ascii_lowercase().as_str() {
        "i1" => T::I1,
        "i8" => T::I8,
        "i16" => T::I16,
        "i32" => T::I32,
        "i48" => T::I48,
        "i64" => T::I64,
        "i80" => T::I80,
        "i128" => T::I128,
        "i256" => T::I256,
        "i512" => T::I512,
        "f32" => T::F32,
        "f64" => T::F64,
        "f80" => T::F80,
        other => {
            return Err(into_strider_err(anyhow::anyhow!(
                "unknown output type {other:?} — expected one of i1, i8, i16, \
                 i32, i48, i64, i80, i128, i256, i512, f32, f64, f80"
            )));
        }
    };
    Ok(ty)
}

impl PatRepr {
    pub(crate) fn compile_match(&self, py: Python<'_>) -> PyResult<DynMatch> {
        compile_repr_match(self, py)
    }

    /// Errors if this representation is match-only.
    pub(crate) fn compile_template(&self, py: Python<'_>) -> PyResult<DynTemplate> {
        compile_repr_template(self, py)
    }
}

// Compiling is native recursion mirroring the Python pattern tree's depth, so
// a pathologically deep pattern would overflow the Rust stack and abort the
// process. CPython's own recursion limit normally caps pattern construction
// first; this counter is the backstop that turns the abort into an exception.

/// Well above any realistic hand-written pattern.
const MAX_PATTERN_NESTING: u32 = 512;

thread_local! {
    static COMPILE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct DepthGuard;

impl DepthGuard {
    fn enter() -> PyResult<Self> {
        COMPILE_DEPTH.with(|d| {
            let next = d.get() + 1;
            if next > MAX_PATTERN_NESTING {
                return Err(into_strider_err(anyhow::anyhow!(
                    "pattern nesting too deep (max {MAX_PATTERN_NESTING})"
                )));
            }
            d.set(next);
            Ok(Self)
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        COMPILE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

fn op_match(py: Python<'_>, ob: &Py<PyAny>) -> PyResult<DynMatch> {
    compile_operand_match(ob.bind(py))
}

#[allow(clippy::too_many_lines)]
fn compile_repr_match(repr: &PatRepr, py: Python<'_>) -> PyResult<DynMatch> {
    // Operands compile eagerly while the GIL and the `Bound`s are held, so the
    // boxed closure owns only child shims and outlives those borrows. These
    // macros stamp out that skeleton: `m_binop` / `m_unop` carry a leading op
    // value, `m_bin` / `m_un` do not, and the `*_any` pair appends `.capture`.
    macro_rules! m_binop {
        ($f:path, $op:ident, $l:ident, $r:ident) => {{
            let op = *$op;
            let l = op_match(py, $l)?;
            let r = op_match(py, $r)?;
            DynMatch(Box::new(move |b| mc($f(op, l, r), b)))
        }};
    }
    macro_rules! m_unop {
        ($f:path, $op:ident, $x:ident) => {{
            let op = *$op;
            let x = op_match(py, $x)?;
            DynMatch(Box::new(move |b| mc($f(op, x), b)))
        }};
    }
    macro_rules! m_bin {
        ($f:path, $l:ident, $r:ident) => {{
            let l = op_match(py, $l)?;
            let r = op_match(py, $r)?;
            DynMatch(Box::new(move |b| mc($f(l, r), b)))
        }};
    }
    macro_rules! m_un {
        ($f:path, $x:ident) => {{
            let x = op_match(py, $x)?;
            DynMatch(Box::new(move |b| mc($f(x), b)))
        }};
    }
    macro_rules! m_bin_any {
        ($f:path, $c:ident, $l:ident, $r:ident) => {{
            let c = *$c;
            let l = op_match(py, $l)?;
            let r = op_match(py, $r)?;
            DynMatch(Box::new(move |b| mc($f(l, r).capture(c), b)))
        }};
    }
    macro_rules! m_un_any {
        ($f:path, $c:ident, $x:ident) => {{
            let c = *$c;
            let x = op_match(py, $x)?;
            DynMatch(Box::new(move |b| mc($f(x).capture(c), b)))
        }};
    }
    let _depth = DepthGuard::enter()?;
    Ok(match repr {
        PatRepr::Any => DynMatch(Box::new(|b| mc(sp::any(), b))),
        PatRepr::Var(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| mc(sp::var(c), b)))
        }
        PatRepr::ValueOfWidth(n) => {
            let n = *n;
            DynMatch(Box::new(move |b| mc(sp::value_of_width(n), b)))
        }
        PatRepr::InputsOfWidth(n, inner) => {
            let n = *n;
            let i = op_match(py, inner)?;
            DynMatch(Box::new(move |b| mc(sp::inputs_of_width(n, i), b)))
        }
        PatRepr::IntConst(v) => {
            let v = *v;
            DynMatch(Box::new(move |b| mc(sp::int_const(v), b)))
        }
        PatRepr::SignedIntConst(v) => {
            let v = *v;
            DynMatch(Box::new(move |b| mc(sp::signed_int_const(v), b)))
        }
        PatRepr::IntConstAnyOf(vals) => {
            let vals = vals.clone();
            DynMatch(Box::new(move |b| mc(sp::int_const_any_of(vals), b)))
        }
        PatRepr::BoolConst(v) => {
            let v = *v;
            DynMatch(Box::new(move |b| mc(sp::bool_const(v), b)))
        }
        PatRepr::FloatConst(bits) => {
            let bits = *bits;
            DynMatch(Box::new(move |b| mc(sp::float_const(bits), b)))
        }
        PatRepr::AnyIntConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_int_const().capture(c), b),
                None => mc(sp::any_int_const(), b),
            }))
        }
        PatRepr::AnyBoolConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_bool_const().capture(c), b),
                None => mc(sp::any_bool_const(), b),
            }))
        }
        PatRepr::AnyFloatConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_float_const().capture(c), b),
                None => mc(sp::any_float_const(), b),
            }))
        }
        PatRepr::InitialVar => DynMatch(Box::new(|b| mc(sp::initial_var(), b))),
        PatRepr::InitialVarFor(vn) => {
            let vn = *vn;
            DynMatch(Box::new(move |b| mc(sp::initial_var_for(vn), b)))
        }
        PatRepr::OneOf(alts) => {
            // A DynMatch's inner box IS a `BoxedAlt`, so unwrapping `.0` feeds
            // `sp::OneOf` directly.
            let boxed: Vec<sp::BoxedAlt> = alts
                .iter()
                .map(|a| op_match(py, a).map(|d| d.0))
                .collect::<PyResult<_>>()?;
            DynMatch(Box::new(move |b| mc(sp::OneOf::new(boxed), b)))
        }
        PatRepr::IntBinary(op, l, r) => m_binop!(sp::int_binary, op, l, r),
        PatRepr::Sub(l, r) => m_bin!(sp::sub, l, r),
        PatRepr::BitNot(x) => m_un!(sp::bit_not, x),
        PatRepr::IntUnary(kind, x) => {
            let kind = *kind;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| match kind {
                IntUnaryKind::Neg => mc(sp::neg(x), b),
                IntUnaryKind::Popcount => mc(sp::popcount(x), b),
                IntUnaryKind::Lzcount => mc(sp::lzcount(x), b),
            }))
        }
        PatRepr::Cast(kind, x) => {
            let kind = *kind;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| cast_match(kind, x, b)))
        }
        PatRepr::Extend(op, x) => m_unop!(sp::extend, op, x),
        PatRepr::IntCmp(op, l, r) => m_binop!(sp::int_cmp, op, l, r),
        PatRepr::IntNe(l, r) => m_bin!(sp::int_ne, l, r),
        PatRepr::IntLe(l, r) => m_bin!(sp::int_le, l, r),
        PatRepr::IntSle(l, r) => m_bin!(sp::int_sle, l, r),
        PatRepr::FloatBinary(op, l, r) => m_binop!(sp::float_binary, op, l, r),
        PatRepr::FloatSub(l, r) => m_bin!(sp::float_sub, l, r),
        PatRepr::FloatUnary(kind, x) => {
            let kind = *kind;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| match kind {
                FloatUnaryKind::Neg => mc(sp::float_neg(x), b),
                FloatUnaryKind::Abs => mc(sp::float_abs(x), b),
                FloatUnaryKind::Sqrt => mc(sp::float_sqrt(x), b),
                FloatUnaryKind::Ceil => mc(sp::float_ceil(x), b),
                FloatUnaryKind::Floor => mc(sp::float_floor(x), b),
                FloatUnaryKind::Round => mc(sp::float_round(x), b),
            }))
        }
        PatRepr::FloatCmp(op, l, r) => m_binop!(sp::float_cmp, op, l, r),
        PatRepr::FloatNe(l, r) => m_bin!(sp::float_ne, l, r),
        PatRepr::FloatLe(l, r) => m_bin!(sp::float_le, l, r),
        PatRepr::FloatIsNan(x) => m_un!(sp::float_is_nan, x),
        PatRepr::BoolBinary(op, l, r) => m_binop!(sp::bool_binary, op, l, r),
        PatRepr::BoolNot(x) => m_un!(sp::bool_not, x),
        PatRepr::IntBinAny(c, l, r) => m_bin_any!(sp::int_binary_any, c, l, r),
        PatRepr::IntUnAny(c, x) => m_un_any!(sp::int_unary_any, c, x),
        PatRepr::IntCmpAny(c, l, r) => m_bin_any!(sp::int_cmp_any, c, l, r),
        PatRepr::BoolBinAny(c, l, r) => m_bin_any!(sp::bool_bin_any, c, l, r),
        PatRepr::FloatBinAny(c, l, r) => m_bin_any!(sp::float_binary_any, c, l, r),
        PatRepr::FloatUnAny(c, x) => m_un_any!(sp::float_unary_any, c, x),
        PatRepr::FloatCmpAny(c, l, r) => m_bin_any!(sp::float_cmp_any, c, l, r),
        PatRepr::Captured(inner, c) => {
            let c = *c;
            let inner = compile_repr_match(inner, py)?;
            DynMatch(Box::new(move |b| mc(inner.capture(c), b)))
        }
        PatRepr::Guarded(inner, f) => {
            let inner = compile_repr_match(inner, py)?;
            let f = f.clone_ref(py);
            DynMatch(Box::new(move |b| mc(wrap_when(inner, f), b)))
        }
        PatRepr::OfWidth(inner, n) => {
            let n = *n;
            let inner = compile_repr_match(inner, py)?;
            DynMatch(Box::new(move |b| mc(inner.of_width(n), b)))
        }
        PatRepr::ValueTy(inner, ty) => {
            let ty = *ty;
            let inner = compile_repr_match(inner, py)?;
            DynMatch(Box::new(move |b| mc(inner.value_ty(ty), b)))
        }
        PatRepr::Finished(_) => {
            return Err(into_strider_err(anyhow::anyhow!(
                "a finished control / variadic pattern cannot be nested as a \
                 value operand"
            )));
        }
    })
}

fn cast_match(kind: CastKind, x: DynMatch, b: &mut MatcherBuilder) -> PatValueRef {
    match kind {
        CastKind::Truncate => mc(sp::truncate(x), b),
        CastKind::ZeroExtend => mc(sp::zero_extend(x), b),
        CastKind::SignExtend => mc(sp::sign_extend(x), b),
        CastKind::IntToFloat => mc(sp::int_to_float(x), b),
        CastKind::FloatToInt => mc(sp::float_to_int(x), b),
        CastKind::IntBitsToFloat => mc(sp::int_bits_to_float(x), b),
        CastKind::FloatBitsToInt => mc(sp::float_bits_to_int(x), b),
        CastKind::FloatToFloat => mc(sp::float_to_float(x), b),
    }
}

fn op_tpl(py: Python<'_>, ob: &Py<PyAny>) -> PyResult<DynTemplate> {
    compile_operand_template(ob.bind(py))
}

#[allow(clippy::too_many_lines)]
fn compile_repr_template(repr: &PatRepr, py: Python<'_>) -> PyResult<DynTemplate> {
    // Composite ops go through the `TemplatePat`-bounded `tpl::` twins: a
    // `DynTemplate` operand is `TemplatePat`, not `MatchPat`, so it cannot
    // feed the match-side factories. Leaves stay on the dual-trait builders.
    // The macros mirror the match side.
    macro_rules! t_binop {
        ($f:path, $op:ident, $l:ident, $r:ident) => {{
            let op = *$op;
            let l = op_tpl(py, $l)?;
            let r = op_tpl(py, $r)?;
            DynTemplate(Box::new(move |b| tc($f(op, l, r), b)))
        }};
    }
    macro_rules! t_unop {
        ($f:path, $op:ident, $x:ident) => {{
            let op = *$op;
            let x = op_tpl(py, $x)?;
            DynTemplate(Box::new(move |b| tc($f(op, x), b)))
        }};
    }
    macro_rules! t_bin {
        ($f:path, $l:ident, $r:ident) => {{
            let l = op_tpl(py, $l)?;
            let r = op_tpl(py, $r)?;
            DynTemplate(Box::new(move |b| tc($f(l, r), b)))
        }};
    }
    macro_rules! t_un {
        ($f:path, $x:ident) => {{
            let x = op_tpl(py, $x)?;
            DynTemplate(Box::new(move |b| tc($f(x), b)))
        }};
    }
    let _depth = DepthGuard::enter()?;
    Ok(match repr {
        PatRepr::Var(c) => {
            let c = *c;
            DynTemplate(Box::new(move |b| template_var(b, c)))
        }
        PatRepr::IntConst(v) => {
            let v = *v;
            DynTemplate(Box::new(move |b| tc(sp::int_const(v), b)))
        }
        PatRepr::SignedIntConst(v) => {
            let v = *v;
            DynTemplate(Box::new(move |b| tc(sp::signed_int_const(v), b)))
        }
        PatRepr::BoolConst(v) => {
            let v = *v;
            DynTemplate(Box::new(move |b| tc(sp::bool_const(v), b)))
        }
        PatRepr::FloatConst(bits) => {
            let bits = *bits;
            DynTemplate(Box::new(move |b| tc(sp::float_const(bits), b)))
        }
        PatRepr::IntBinary(op, l, r) => t_binop!(tpl::int_binary, op, l, r),
        PatRepr::Sub(l, r) => t_bin!(tpl::sub, l, r),
        PatRepr::BitNot(x) => t_un!(tpl::bit_not, x),
        PatRepr::IntUnary(kind, x) => {
            let kind = *kind;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| match kind {
                IntUnaryKind::Neg => tc(tpl::neg(x), b),
                IntUnaryKind::Popcount => tc(tpl::popcount(x), b),
                IntUnaryKind::Lzcount => tc(tpl::lzcount(x), b),
            }))
        }
        PatRepr::Cast(kind, x) => {
            let kind = *kind;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| cast_tpl(kind, x, b)))
        }
        PatRepr::Extend(op, x) => t_unop!(tpl::extend, op, x),
        PatRepr::IntCmp(op, l, r) => t_binop!(tpl::int_cmp, op, l, r),
        PatRepr::FloatBinary(op, l, r) => t_binop!(tpl::float_binary, op, l, r),
        PatRepr::FloatSub(l, r) => t_bin!(tpl::float_sub, l, r),
        PatRepr::FloatUnary(kind, x) => {
            let kind = *kind;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| match kind {
                FloatUnaryKind::Neg => tc(tpl::float_neg(x), b),
                FloatUnaryKind::Abs => tc(tpl::float_abs(x), b),
                FloatUnaryKind::Sqrt => tc(tpl::float_sqrt(x), b),
                FloatUnaryKind::Ceil => tc(tpl::float_ceil(x), b),
                FloatUnaryKind::Floor => tc(tpl::float_floor(x), b),
                FloatUnaryKind::Round => tc(tpl::float_round(x), b),
            }))
        }
        PatRepr::FloatCmp(op, l, r) => t_binop!(tpl::float_cmp, op, l, r),
        PatRepr::BoolBinary(op, l, r) => t_binop!(tpl::bool_binary, op, l, r),
        PatRepr::BoolNot(x) => t_un!(tpl::bool_not, x),
        PatRepr::Captured(_inner, c) => {
            // On a RHS a capture resolves to the matched LHS value, replacing
            // whatever it wrapped, so `inner` is deliberately never built.
            let c = *c;
            DynTemplate(Box::new(move |b| b.capture(c)))
        }
        PatRepr::Any => return Err(rhs_error("any")),
        PatRepr::ValueOfWidth(_) => return Err(rhs_error("value_of_width")),
        PatRepr::InputsOfWidth(..) => return Err(rhs_error("inputs_of_width")),
        PatRepr::IntConstAnyOf(_) => return Err(rhs_error("int_const_any_of")),
        PatRepr::AnyIntConst(_) => return Err(rhs_error("any_int_const")),
        PatRepr::AnyBoolConst(_) => return Err(rhs_error("any_bool_const")),
        PatRepr::AnyFloatConst(_) => return Err(rhs_error("any_float_const")),
        PatRepr::InitialVar => return Err(rhs_error("initial_var")),
        PatRepr::InitialVarFor(_) => return Err(rhs_error("initial_var_for")),
        PatRepr::OneOf(_) => return Err(rhs_error("one_of")),
        PatRepr::IntNe(..) => return Err(rhs_error("int_ne")),
        PatRepr::IntLe(..) => return Err(rhs_error("int_le")),
        PatRepr::IntSle(..) => return Err(rhs_error("int_sle")),
        PatRepr::FloatNe(..) => return Err(rhs_error("float_ne")),
        PatRepr::FloatLe(..) => return Err(rhs_error("float_le")),
        PatRepr::FloatIsNan(_) => return Err(rhs_error("float_is_nan")),
        PatRepr::IntBinAny(..) => return Err(rhs_error("int_bin_any")),
        PatRepr::IntUnAny(..) => return Err(rhs_error("int_un_any")),
        PatRepr::IntCmpAny(..) => return Err(rhs_error("int_cmp_any")),
        PatRepr::BoolBinAny(..) => return Err(rhs_error("bool_bin_any")),
        PatRepr::FloatBinAny(..) => return Err(rhs_error("float_bin_any")),
        PatRepr::FloatUnAny(..) => return Err(rhs_error("float_un_any")),
        PatRepr::FloatCmpAny(..) => return Err(rhs_error("float_cmp_any")),
        PatRepr::Guarded(..) => return Err(rhs_error(".when()")),
        PatRepr::OfWidth(..) => return Err(rhs_error(".of_width()")),
        PatRepr::ValueTy(..) => return Err(rhs_error(".value_ty()")),
        PatRepr::Finished(_) => return Err(rhs_error("control / variadic builder")),
    })
}

fn cast_tpl(kind: CastKind, x: DynTemplate, b: &mut TemplateBuilder) -> TmplValueRef {
    match kind {
        CastKind::Truncate => tc(tpl::truncate(x), b),
        CastKind::ZeroExtend => tc(tpl::zero_extend(x), b),
        CastKind::SignExtend => tc(tpl::sign_extend(x), b),
        CastKind::IntToFloat => tc(tpl::int_to_float(x), b),
        CastKind::FloatToInt => tc(tpl::float_to_int(x), b),
        CastKind::IntBitsToFloat => tc(tpl::int_bits_to_float(x), b),
        CastKind::FloatBitsToInt => tc(tpl::float_bits_to_int(x), b),
        CastKind::FloatToFloat => tc(tpl::float_to_float(x), b),
    }
}

impl PatRepr {
    pub(crate) fn to_pattern(&self, py: Python<'_>) -> PyResult<Pattern> {
        if let PatRepr::Finished(cell) = self {
            return cell.borrow_mut().take().ok_or_else(|| {
                into_strider_err(anyhow::anyhow!(
                    "this control / variadic pattern was already consumed by a \
                     prior query — rebuild it for each find/rewrite call"
                ))
            });
        }
        Ok(self.compile_match(py)?.into_pattern())
    }

    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<Template> {
        Ok(self.compile_template(py)?.into_template())
    }
}

/// Polymorphic input for builder field methods and `Function.find_all`:
/// a `Pat`, a `Capture`, a string (interned to a `Capture`), or any typed
/// builder that finalises to a pattern.
#[derive(FromPyObject)]
pub enum PatLike<'py> {
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
    CallPat(Bound<'py, PyCallPat>),
    CallOtherPat(Bound<'py, PyCallOtherPat>),
    RetPat(Bound<'py, PyRetPat>),
    IfPat(Bound<'py, PyIfPat>),
    LoadPat(Bound<'py, PyLoadPat>),
    StorePat(Bound<'py, PyStorePat>),
    PhiPat(Bound<'py, PyPhiPat>),
    MemPhiPat(Bound<'py, PyMemPhiPat>),
    FunctionArgPat(Bound<'py, PyFunctionArgPat>),
    IntBinaryPat(Bound<'py, PyIntBinaryPat>),
    FloatBinaryPat(Bound<'py, PyFloatBinaryPat>),
    BoolBinaryPat(Bound<'py, PyBoolBinaryPat>),
    IndirectBranchPat(Bound<'py, PyIndirectBranchPat>),
    UnreachablePat(Bound<'py, PyUnreachablePat>),
    SwitchPat(Bound<'py, PySwitchPat>),
    EntryPat(Bound<'py, PyEntryPat>),
    RegionPat(Bound<'py, PyRegionPat>),
}

/// One pattern, or a list matched as a join: every pattern runs, captures
/// unify on shared `Capture` objects, and each result collapses to one
/// merged `Match`.
// Variant order matters: `Single` is tried first so a bare capture-name
// string, itself a sequence, is read as one pattern.
#[derive(FromPyObject)]
pub enum PatQuery<'py> {
    Single(PatLike<'py>),
    Many(Vec<PatLike<'py>>),
}

impl PatQuery<'_> {
    pub(crate) fn to_patterns(&self, py: Python<'_>) -> PyResult<Vec<Pattern>> {
        match self {
            PatQuery::Single(p) => Ok(vec![p.to_pattern(py)?]),
            PatQuery::Many(ps) => ps.iter().map(|p| p.to_pattern(py)).collect(),
        }
    }
}

impl pyo3_stub_gen::PyStubType for PatQuery<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        pyo3_stub_gen::TypeInfo::with_module("strider.pattern.PatLike", "strider.pattern".into())
    }
}

// Hand-written so pyo3-stub-gen emits the canonical `PatLike` type alias.
impl pyo3_stub_gen::PyStubType for PatLike<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        pyo3_stub_gen::TypeInfo::with_module("strider.pattern.PatLike", "strider.pattern".into())
    }
}

impl PatLike<'_> {
    pub(crate) fn to_pattern(&self, py: Python<'_>) -> PyResult<Pattern> {
        match self {
            PatLike::Pat(p) => p.borrow().repr.to_pattern(py),
            PatLike::Capture(c) => Ok(strider_pattern::var(c.borrow().inner).into_pattern()),
            PatLike::Str(s) => {
                let name = s.to_string();
                if name == "_" || name == "any_" {
                    Ok(strider_pattern::any().into_pattern())
                } else {
                    Ok(strider_pattern::var(intern_str(&name)?).into_pattern())
                }
            }
            PatLike::CallPat(b) => b.borrow().build_pattern_py(py),
            PatLike::CallOtherPat(b) => b.borrow().build_pattern_py(py),
            PatLike::RetPat(b) => b.borrow().build_pattern_py(py),
            PatLike::IfPat(b) => b.borrow().build_pattern_py(py),
            PatLike::LoadPat(b) => b.borrow().build_pattern_py(py),
            PatLike::StorePat(b) => b.borrow().build_pattern_py(py),
            PatLike::PhiPat(b) => b.borrow().build_pattern_py(py),
            PatLike::MemPhiPat(b) => b.borrow().build_pattern_py(py),
            PatLike::FunctionArgPat(b) => b.borrow().build_pattern_py(py),
            PatLike::IntBinaryPat(b) => b.borrow().build_pattern_py(py),
            PatLike::FloatBinaryPat(b) => b.borrow().build_pattern_py(py),
            PatLike::BoolBinaryPat(b) => b.borrow().build_pattern_py(py),
            PatLike::IndirectBranchPat(b) => b.borrow().build_pattern_py(py),
            PatLike::UnreachablePat(b) => b.borrow().build_pattern_py(py),
            PatLike::SwitchPat(b) => b.borrow().build_pattern_py(py),
            PatLike::EntryPat(b) => b.borrow().build_pattern_py(py),
            PatLike::RegionPat(b) => b.borrow().build_pattern_py(py),
        }
    }
}

/// Rewrite-RHS input, separate from the match-side `PatLike` so
/// `Function.rewrite` / `rewrite_all` type `replace` against the build-valid
/// `Template`. A bare `Pat` is accepted for back-compat, but only its
/// build-valid subset compiles: a match-only `Pat` such as `anything()`
/// raises.
#[derive(FromPyObject)]
pub enum TemplateLike<'py> {
    Template(Bound<'py, crate::template::PyTemplate>),
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
}

// Hand-written so pyo3-stub-gen emits the canonical `Template` type.
impl pyo3_stub_gen::PyStubType for TemplateLike<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        pyo3_stub_gen::TypeInfo::with_module("strider.template.Template", "strider.template".into())
    }
}

impl TemplateLike<'_> {
    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<Template> {
        match self {
            TemplateLike::Template(t) => t.borrow().to_template(py),
            TemplateLike::Pat(p) => p.borrow().repr.to_template(py),
            TemplateLike::Capture(c) => Ok(DynTemplate(Box::new({
                let cap = c.borrow().inner;
                move |b| template_var(b, cap)
            }))
            .into_template()),
            TemplateLike::Str(s) => {
                let name = s.to_string();
                if name == "_" || name == "any_" {
                    Err(rhs_error("any_"))
                } else {
                    let cap = intern_str(&name)?;
                    Ok(DynTemplate(Box::new(move |b| template_var(b, cap))).into_template())
                }
            }
        }
    }
}

// A `.when()` predicate raising KeyboardInterrupt or SystemExit stashes the
// error here instead of propagating: the matcher must finish its walk, and
// returning to CPython with an exception still set trips its "returned a
// result with an exception set" guard. Stashing also protects the error from
// being destroyed by the next predicate call. The outer find boundary drains
// the cell and re-raises.
thread_local! {
    static PENDING_CONTROL_FLOW: std::cell::Cell<Option<PyErr>> =
        const { std::cell::Cell::new(None) };
}

pub(crate) fn take_pending_control_flow() -> Option<PyErr> {
    PENDING_CONTROL_FLOW.with(std::cell::Cell::take)
}

pub(crate) fn peek_pending_control_flow() -> bool {
    PENDING_CONTROL_FLOW.with(|cell| {
        let t = cell.take();
        let pending = t.is_some();
        cell.set(t);
        pending
    })
}

pub(crate) fn stash_pending_control_flow(e: PyErr) {
    PENDING_CONTROL_FLOW.with(|cell| cell.set(Some(e)));
}

// A `.when(f)` predicate is attached at pattern-build time, before any
// `Function` exists, and the same pattern can run against several of them, so
// the closure cannot capture a `Py<PyFunction>`. `Function::run_query` pushes
// the function plus its generation here instead, and `run_when_predicate`
// peeks the top to build the `Match` it hands the callback.
//
// A stack rather than one slot: a predicate may itself issue a nested query,
// which must not clobber the outer entry. Both entries share the live query's
// `Rc<RefCell<Function>>`, so a `Match` accessor re-borrowing it under
// `run_query`'s own read guard is a same-thread recursive READ lock. That is
// only safe because no write lock is ever taken outside `try_write_inner`.
thread_local! {
    static CURRENT_QUERY_FUNCTION: std::cell::RefCell<Vec<(Py<crate::function::PyFunction>, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Must be paired with [`pop_current_query_function`] once the query, and
/// every `.when()` predicate it invokes, has finished.
pub(crate) fn push_current_query_function(
    function: Py<crate::function::PyFunction>,
    generation: u64,
) {
    CURRENT_QUERY_FUNCTION.with(|c| c.borrow_mut().push((function, generation)));
}

pub(crate) fn pop_current_query_function() {
    CURRENT_QUERY_FUNCTION.with(|c| {
        c.borrow_mut().pop();
    });
}

fn current_query_function(py: Python<'_>) -> Option<(Py<crate::function::PyFunction>, u64)> {
    CURRENT_QUERY_FUNCTION.with(|c| {
        c.borrow()
            .last()
            .map(|(function, generation)| (function.clone_ref(py), *generation))
    })
}

/// Runs a Python `.when()` predicate, returning whether to keep the match.
/// `node` becomes the `Match.root` the predicate sees. Control-flow
/// exceptions are stashed for the outer boundary; an ordinary predicate
/// exception goes to stderr and counts as no-match.
fn run_when_predicate(
    node: strider_ir::node::NodeId,
    bindings: &strider_pattern::Bindings,
    py_func: &PyObject,
) -> bool {
    Python::with_gil(|py| {
        if peek_pending_control_flow() {
            return false;
        }
        let Some((function, generation)) = current_query_function(py) else {
            // Unreachable: every query path pushes an entry before running the
            // matcher. Fail closed rather than panic into the matcher's stack.
            eprintln!(
                "strider .when(): no active query function on this thread (internal \
                 error) — treating as no-match"
            );
            return false;
        };
        let py_match = match Py::new(
            py,
            crate::matcher::PyMatch {
                inner: vec![strider_pattern::Match::from_root(node, bindings.clone())],
                function,
                generation,
            },
        ) {
            Ok(p) => p,
            Err(e) => {
                stash_pending_control_flow(e);
                return false;
            }
        };
        let args = PyTuple::new_bound(py, [py_match]);
        let result = py_func.call_bound(py, args, None);
        match result {
            Ok(obj) => match obj.extract::<bool>(py) {
                Ok(b) => b,
                Err(e) => {
                    stash_pending_control_flow(e);
                    false
                }
            },
            Err(e) => {
                let is_control_flow = {
                    let t = e.get_type_bound(py);
                    t.is_subclass_of::<pyo3::exceptions::PyKeyboardInterrupt>()
                        .unwrap_or(false)
                        || t.is_subclass_of::<pyo3::exceptions::PySystemExit>()
                            .unwrap_or(false)
                };
                if is_control_flow {
                    stash_pending_control_flow(e);
                } else {
                    eprintln!("strider .when() predicate raised — treating as no-match: {e}");
                }
                false
            }
        }
    })
}

/// Attaches the predicate as a full `PostMatchFn`, which keeps the matched
/// `NodeId` the predicate needs for a real `Match.root`.
pub(crate) fn wrap_when<P: MatchPat + 'static>(inner: P, py_func: PyObject) -> impl MatchPat {
    DynMatch(Box::new(move |b: &mut MatcherBuilder| {
        let o = inner.compile(b);
        b.set_post_match(
            o,
            Box::new(move |_matcher, node, _ty, bindings| {
                run_when_predicate(node, bindings, &py_func)
            }),
        );
        o
    }))
}

/// For the node-rooted control / variadic builders, which finalise straight
/// to a `Pattern` and so have no `MatchPat` form for [`wrap_when`] to wrap.
pub(crate) fn make_root_post_match(py_func: PyObject) -> strider_pattern::PostMatchFn {
    Box::new(move |_matcher, node, _ty, bindings| run_when_predicate(node, bindings, &py_func))
}

fn apply_when_to_pattern(py: Python<'_>, common: &CommonState, pat: Pattern) -> Pattern {
    match common.when.as_ref() {
        Some(f) => pat.with_root_post_match(make_root_post_match(f.clone_ref(py))),
        None => pat,
    }
}

/// Selects which value-passthrough cast node kinds the matcher walks through
/// transparently. Build from the classmethods, combine with `|`, and pass as
/// `find_all(pat, ignore_casts_mask=...)`.
#[pyclass(name = "CastMask", module = "strider.pattern", frozen)]
#[derive(Clone, Copy)]
pub struct PyCastMask {
    pub(crate) inner: strider_pattern::CastMask,
}

macro_rules! forall_castmask {
    ($name:ident => $value:ident) => {
        #[pymethods]
        impl PyCastMask {
            #[doc = concat!(
                                        "Mask selecting the `", stringify!($value),
                                        "` value-passthrough cast for the matcher to walk through."
                                    )]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self {
                    inner: strider_pattern::CastMask::$value,
                }
            }
        }
    };
    ($name:ident => fn $value:ident) => {
        #[pymethods]
        impl PyCastMask {
            #[doc = concat!(
                                        "`CastMask::", stringify!($value), "()` — ",
                                        "the all-casts (`all`) / no-casts (`empty`) mask."
                                    )]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self {
                    inner: strider_pattern::CastMask::$value(),
                }
            }
        }
    };
}

forall_castmask!(zero_extend => ZERO_EXTEND);
forall_castmask!(sign_extend => SIGN_EXTEND);
forall_castmask!(extend => EXTEND);
forall_castmask!(truncate => TRUNCATE);
forall_castmask!(int_bits_to_float => INT_BITS_TO_FLOAT);
forall_castmask!(float_bits_to_int => FLOAT_BITS_TO_INT);
forall_castmask!(all => fn all);
forall_castmask!(none => fn empty);

#[pymethods]
impl PyCastMask {
    fn __or__(&self, other: &Self) -> Self {
        Self {
            inner: self.inner | other.inner,
        }
    }
    fn __and__(&self, other: &Self) -> Self {
        Self {
            inner: self.inner & other.inner,
        }
    }
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
    fn __hash__(&self) -> u64 {
        u64::from(self.inner.bits())
    }
    /// The mask as a raw integer bitset.
    fn bits(&self) -> u32 {
        self.inner.bits()
    }
    fn __repr__(&self) -> String {
        format!("CastMask(0b{:08b})", self.inner.bits())
    }
}

#[pymethods]
impl PyPat {
    /// Capture this pattern's matched node under `c`. Returns a new `Pat`.
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        PyPat::from_repr(PatRepr::Captured(Rc::clone(&self.repr), c.inner))
    }

    /// Capture this pattern under a string name (auto-interned).
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        let c = intern_str(name)?;
        Ok(PyPat::from_repr(PatRepr::Captured(
            Rc::clone(&self.repr),
            c,
        )))
    }

    /// Attach a Python predicate that runs after this pattern matches.
    /// Returning `False` (or raising) fails the match.
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_repr(PatRepr::Guarded(Rc::clone(&self.repr), f))
    }

    /// Constrain the matched node's value output to exactly `n` bits.
    /// Match-only.
    fn of_width(&self, n: u32) -> PyPat {
        PyPat::from_repr(PatRepr::OfWidth(Rc::clone(&self.repr), n))
    }

    /// Constrain the matched node's value output to the type named by `ty`,
    /// e.g. `"i1"`, `"i64"`, `"f32"`. Match-only.
    fn value_ty(&self, ty: &str) -> PyResult<PyPat> {
        let t = parse_value_ty(ty)?;
        Ok(PyPat::from_repr(PatRepr::ValueTy(Rc::clone(&self.repr), t)))
    }

    /// Sugar for `.of_width(1)`, a boolean output. Match-only.
    fn bool_valued(&self) -> PyPat {
        PyPat::from_repr(PatRepr::OfWidth(Rc::clone(&self.repr), 1))
    }

    /// Force commutative binary ops not to try the swapped operand order.
    /// Only valid on a typed builder (`int_binary(...).ordered()` etc.);
    /// on a finalized `Pat` this raises.
    fn ordered(&self) -> PyResult<PyPat> {
        Err(into_strider_err(anyhow::anyhow!(
            "Pat.ordered() has no effect on a finalized Pat — \
             use int_binary(op, l, r).ordered() / bool_binary(op, l, r).ordered() / \
             float_binary(op, l, r).ordered() to force left-to-right matching"
        )))
    }

    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

/// Wildcard: matches any node without binding it.
#[pyfunction(name = "anything")]
pub fn any_() -> PyPat {
    PyPat::from_repr(PatRepr::Any)
}

/// Wildcard that binds the matched node to capture `c`.
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_repr(PatRepr::Var(c.inner))
}

/// Match any value output exactly `n` bits wide.
#[pyfunction]
pub fn value_of_width(n: u32) -> PyPat {
    PyPat::from_repr(PatRepr::ValueOfWidth(n))
}

/// Match any 1-bit (`I1`) value output.
#[pyfunction]
pub fn bool_value() -> PyPat {
    PyPat::from_repr(PatRepr::ValueOfWidth(1))
}

/// Match `inner` and require all value inputs to be `n` bits wide.
#[pyfunction]
pub fn inputs_of_width(n: u32, inner: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::InputsOfWidth(n, inner))
}

/// Match `inner` whose value inputs are all 1-bit (`I1`).
#[pyfunction]
pub fn bool_inputs(inner: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::InputsOfWidth(1, inner))
}

/// Match an `IntConst` whose stored value, masked to the output width, equals
/// `value`. Bit-pattern equality: a negative `value` uses its sign-extended
/// form. For the cross-width signed form use `signed_int_const`.
#[pyfunction]
pub fn int_const(value: i128) -> PyPat {
    PyPat::from_repr(PatRepr::IntConst(value as u128))
}

/// Match a signed `IntConst` across width encodings (exact, sign-extended,
/// zero-extended-narrow). More permissive than `int_const`.
///
/// A `value` outside `i64` range raises.
#[pyfunction]
pub fn signed_int_const(value: i128) -> PyResult<PyPat> {
    let v = checked_signed_i64(value)?;
    Ok(PyPat::from_repr(PatRepr::SignedIntConst(v)))
}

/// Match an `IntConst` equal (masked) to any of `values`. An empty list
/// vacuously fails. Match-only.
///
/// An element outside `u64` range raises.
#[pyfunction]
pub fn int_const_any_of(values: Vec<i128>) -> PyResult<PyPat> {
    let u64_values: Vec<u64> = values
        .into_iter()
        .map(|v| {
            u64::try_from(v).map_err(|_| {
                into_strider_err(anyhow::anyhow!(
                    "int_const_any_of element {v} does not fit in u64 (the core candidate width)"
                ))
            })
        })
        .collect::<PyResult<_>>()?;
    Ok(PyPat::from_repr(PatRepr::IntConstAnyOf(u64_values)))
}

/// Match an `I1` boolean constant equal to `value`.
#[pyfunction]
pub fn bool_const(value: bool) -> PyPat {
    PyPat::from_repr(PatRepr::BoolConst(value))
}

/// Match a `FloatConst` whose raw bits equal `bits`.
#[pyfunction]
pub fn float_const(bits: u64) -> PyPat {
    PyPat::from_repr(PatRepr::FloatConst(bits))
}

/// Match any `IntConst`, optionally binding its value to `c`. With no
/// capture it is a purely structural constraint.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_int_const(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyIntConst(c.map(|c| c.inner)))
}

/// Match any `I1` boolean constant, optionally binding it to `c`.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_bool_const(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyBoolConst(c.map(|c| c.inner)))
}

/// Match any `FloatConst`, optionally binding it to `c`.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_float_const(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloatConst(c.map(|c| c.inner)))
}

/// Match any `InitialVar` node.
#[pyfunction]
pub fn initial_var() -> PyPat {
    PyPat::from_repr(PatRepr::InitialVar)
}

/// Match `InitialVar(vn)` for a specific varnode.
#[pyfunction]
pub fn initial_var_for(vn: crate::sleigh::PyVn) -> PyPat {
    PyPat::from_repr(PatRepr::InitialVarFor(vn.inner))
}

/// Alternation: match a value if any listed sub-pattern matches it. Good for
/// an optional wrapper, e.g. an address that may or may not be masked.
/// Match-only. Requires at least one alternative.
///
/// **Order the alternatives most-specific first.** They are tried in order,
/// first match wins, so a permissive alternative ahead of a narrower one
/// shadows it, and because the shadowing arm still matches the query returns
/// the wrong binding rather than failing. `anything()` and `var(c)` match ANY
/// node, including the operator a later arm was meant to catch:
///
/// ```python
/// # WRONG: var(base) also matches the Add, so `off` never binds and every
/// # `base + K` load silently looks like a bare `base`.
/// load(addr=one_of([var(base), add(var(base), any_int_const(off))]))
///
/// # RIGHT: specific shape first, bare fallback last.
/// load(addr=one_of([add(var(base), any_int_const(off)), var(base)]))
/// ```
///
/// Captures under an alternative that did not fire are left UNBOUND, not
/// defaulted, so `Match.has(c)` (or a `None` from `Match.const_uint(c)`)
/// tells you which arm matched and lets you pick your own default:
///
/// ```python
/// offset = h.const_uint(off) if h.has(off) else 0
/// ```
#[pyfunction]
pub fn one_of(patterns: Vec<Py<PyAny>>) -> PyResult<PyPat> {
    if patterns.is_empty() {
        return Err(into_strider_err(anyhow::anyhow!(
            "one_of requires at least one alternative"
        )));
    }
    Ok(PyPat::from_repr(PatRepr::OneOf(patterns)))
}

/// Match any node, subject to a Python predicate. Shorthand for
/// `anything().when(f)`.
#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    PyPat::from_repr(PatRepr::Guarded(Rc::new(PatRepr::Any), f))
}

// The canonical spelling each parser accepts is the op variant's `Debug`
// output, the same string `crate::matcher::op_name` emits. Canonical names
// come from an exhaustive `match`, so adding a variant in strider-ir is a
// compile error here instead of a silent desync.

/// Tries exact, then alias, then case-insensitive on both.
fn lookup_op<Op: Copy>(
    variants: &[Op],
    canonical: impl Fn(Op) -> &'static str,
    aliases: &[(&str, Op)],
    name: &str,
    op_kind: &str,
) -> PyResult<Op> {
    if let Some(&op) = variants.iter().find(|&&op| canonical(op) == name) {
        return Ok(op);
    }
    if let Some(&(_, op)) = aliases.iter().find(|(n, _)| *n == name) {
        return Ok(op);
    }
    if let Some(&op) = variants
        .iter()
        .find(|&&op| canonical(op).eq_ignore_ascii_case(name))
    {
        return Ok(op);
    }
    if let Some(&(_, op)) = aliases.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Ok(op);
    }
    Err(into_strider_err(anyhow::anyhow!(
        "unknown {op_kind} variant {name:?}"
    )))
}

// The optional `_ => unreachable` arm lets a curated subset (the boolean ops)
// list only the variants it accepts and still match exhaustively. The leading
// `$vis` token sets per-invocation visibility; only `parse_int_cmp_op` has a
// consumer outside this module.
macro_rules! op_parser {
    (
        $vis:vis $fn:ident, $ty:ty, $op_kind:literal,
        variants = [$($variant:ident),+ $(,)?],
        $(rest_unreachable = $msg:literal,)?
        aliases = [$(($alias:literal, $aop:ident)),* $(,)?] $(,)?
    ) => {
        $vis fn $fn(name: &str) -> PyResult<$ty> {
            use $ty as Op;
            static VARIANTS: &[$ty] = &[$(Op::$variant),+];
            fn canonical(op: $ty) -> &'static str {
                match op {
                    $(Op::$variant => stringify!($variant),)+
                    $(_ => unreachable!($msg, ),)?
                }
            }
            static ALIASES: &[(&str, $ty)] = &[$(($alias, Op::$aop)),*];
            lookup_op(VARIANTS, canonical, ALIASES, name, $op_kind)
        }
    };
}

op_parser!(
    pub(crate) parse_int_cmp_op,
    strider_ir::IntCmpOp,
    "IntCmpOp",
    variants = [Equal, Less, Sless, Carry, Scarry, Sborrow],
    aliases = [("eq", Equal), ("lt", Less), ("slt", Sless)]
);

op_parser!(
    parse_int_binary_op,
    strider_ir::IntBinaryOp,
    "IntBinaryOp",
    variants = [
        Add,
        And,
        Or,
        Xor,
        Div,
        Sdiv,
        Rem,
        Srem,
        ShiftRight,
        SShiftRight,
        ShiftLeft,
        Mul
    ],
    aliases = [
        ("shl", ShiftLeft),
        ("shr", ShiftRight),
        ("sshr", SShiftRight)
    ]
);

op_parser!(
    parse_bool_binary_op,
    strider_ir::IntBinaryOp,
    "boolean binary op",
    variants = [And, Or, Xor],
    rest_unreachable = "non-boolean IntBinaryOp in bool table",
    aliases = []
);

op_parser!(
    parse_float_binary_op,
    strider_ir::FloatBinaryOp,
    "FloatBinaryOp",
    variants = [Add, Mul, Div],
    aliases = []
);

pub(crate) fn parse_extend_op(op: &str) -> PyResult<strider_ir::ExtendOp> {
    match op {
        "zero" | "zero_extend" | "ZeroExtend" => Ok(strider_ir::ExtendOp::ZeroExtend),
        "sign" | "sign_extend" | "SignExtend" => Ok(strider_ir::ExtendOp::SignExtend),
        other => Err(into_strider_err(anyhow::anyhow!(
            "unknown extend op {other:?} (expected 'zero' or 'sign')"
        ))),
    }
}

pub(crate) fn checked_signed_i64(value: i128) -> PyResult<i64> {
    i64::try_from(value).map_err(|_| {
        into_strider_err(anyhow::anyhow!(
            "signed_int_const value {value} does not fit in i64 (the core signed-const width)"
        ))
    })
}

// Every value-op builder is a thunk wrapping its operands in one `PatRepr`
// variant. The arms vary only by arity and by whether the Python name differs
// from the Rust ident (`and_` exposed as `int_and`, and friends).
macro_rules! pat_fn {
    (binary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    (binary $name:ident = $py:literal, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py)]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    (unary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::$repr($op, operand))
        }
    };
}

pat_fn!(binary
    add, IntBinary, strider_ir::IntBinaryOp::Add,
    "Pattern: `IntBinaryOp::Add` (`a + b`). Commutative."
);
pat_fn!(binary
    mul, IntBinary, strider_ir::IntBinaryOp::Mul,
    "Pattern: `IntBinaryOp::Mul` (`a * b`). Commutative."
);
pat_fn!(binary div, IntBinary, strider_ir::IntBinaryOp::Div,
    "Pattern: `IntBinaryOp::Div` (unsigned `a / b`).");
pat_fn!(binary sdiv, IntBinary, strider_ir::IntBinaryOp::Sdiv,
    "Pattern: `IntBinaryOp::Sdiv` (signed `a / b`).");
pat_fn!(binary rem, IntBinary, strider_ir::IntBinaryOp::Rem,
    "Pattern: `IntBinaryOp::Rem` (unsigned `a % b`).");
pat_fn!(binary srem, IntBinary, strider_ir::IntBinaryOp::Srem,
    "Pattern: `IntBinaryOp::Srem` (signed `a % b`).");
pat_fn!(binary
    shl, IntBinary, strider_ir::IntBinaryOp::ShiftLeft,
    "Pattern: `IntBinaryOp::ShiftLeft` (`a << b`)."
);
pat_fn!(binary
    shr, IntBinary, strider_ir::IntBinaryOp::ShiftRight,
    "Pattern: `IntBinaryOp::ShiftRight` (`a >> b`)."
);
pat_fn!(binary
    sshr, IntBinary, strider_ir::IntBinaryOp::SShiftRight,
    "Pattern: `IntBinaryOp::SShiftRight` (arithmetic `a >> b`)."
);
pat_fn!(binary
    and_ = "int_and", IntBinary, strider_ir::IntBinaryOp::And,
    "Pattern: `IntBinaryOp::And` (`a & b`). Commutative."
);
pat_fn!(binary
    or_ = "int_or", IntBinary, strider_ir::IntBinaryOp::Or,
    "Pattern: `IntBinaryOp::Or` (`a | b`). Commutative."
);
pat_fn!(binary
    xor = "int_xor", IntBinary, strider_ir::IntBinaryOp::Xor,
    "Pattern: `IntBinaryOp::Xor` (`a ^ b`). Commutative."
);

/// Pattern: integer subtraction `a - b` (lifter-canonical `Add(a, Neg(b))`).
#[pyfunction]
pub fn sub(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::Sub(l, r))
}

pat_fn!(binary
    int_eq, IntCmp, strider_ir::IntCmpOp::Equal,
    "Pattern: `IntCmpOp::Equal` (`a == b`). Commutative."
);
pat_fn!(binary
    int_lt, IntCmp, strider_ir::IntCmpOp::Less,
    "Pattern: `IntCmpOp::Less` (unsigned `a < b`)."
);
pat_fn!(binary
    int_slt, IntCmp, strider_ir::IntCmpOp::Sless,
    "Pattern: `IntCmpOp::Sless` (signed `a < b`)."
);
pat_fn!(binary
    int_carry, IntCmp, strider_ir::IntCmpOp::Carry,
    "Pattern: `IntCmpOp::Carry` (unsigned add carry-out). Commutative."
);
pat_fn!(binary
    int_scarry, IntCmp, strider_ir::IntCmpOp::Scarry,
    "Pattern: `IntCmpOp::Scarry` (signed add overflow). Commutative."
);
pat_fn!(binary
    int_sborrow, IntCmp, strider_ir::IntCmpOp::Sborrow,
    "Pattern: `IntCmpOp::Sborrow` (signed subtract overflow)."
);

/// Pattern: integer `a != b` (lifter-canonical `Xor(IntEqual(a, b), 1)`).
#[pyfunction]
pub fn int_ne(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntNe(l, r))
}

/// Pattern: unsigned `a <= b` (lifter-canonical `Xor(IntLess(b, a), 1)`).
#[pyfunction]
pub fn int_le(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntLe(l, r))
}

/// Pattern: signed `a <= b` (lifter-canonical `Xor(Sless(b, a), 1)`).
#[pyfunction]
pub fn int_sle(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntSle(l, r))
}

/// Match a specific `IntCmpOp` variant by name.
#[pyfunction]
pub fn int_cmp(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyPat> {
    let cmp_op = parse_int_cmp_op(op)?;
    Ok(PyPat::from_repr(PatRepr::IntCmp(cmp_op, l, r)))
}

/// Pattern: `IntUnaryOp::Neg`, two's-complement negation (`-x`).
#[pyfunction]
pub fn neg(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Neg, operand))
}

/// Pattern: bitwise complement `~x`, lifted as `Xor(x, all_ones)`.
#[pyfunction(name = "int_not")]
pub fn bit_not(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BitNot(operand))
}

/// Pattern: `Popcount`, the count of set bits.
#[pyfunction]
pub fn popcount(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Popcount, operand))
}

/// Pattern: `Lzcount`, the count of leading zero bits.
#[pyfunction]
pub fn lzcount(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Lzcount, operand))
}

pat_fn!(binary
    bool_and, BoolBinary, strider_ir::IntBinaryOp::And,
    "Pattern: boolean `a && b` (`IntBinaryOp::And` at `I1`). Commutative."
);
pat_fn!(binary
    bool_or, BoolBinary, strider_ir::IntBinaryOp::Or,
    "Pattern: boolean `a || b` (`IntBinaryOp::Or` at `I1`). Commutative."
);
pat_fn!(binary
    bool_xor, BoolBinary, strider_ir::IntBinaryOp::Xor,
    "Pattern: boolean `a ^ b` (`IntBinaryOp::Xor` at `I1`). Commutative."
);

/// Pattern: boolean negation `!x`, lifted as `Xor(x, IntConst(1)):I1`.
#[pyfunction]
pub fn bool_not(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BoolNot(operand))
}

pat_fn!(binary
    float_add, FloatBinary, strider_ir::FloatBinaryOp::Add,
    "Pattern: `FloatBinaryOp::Add` (`a + b`). Commutative."
);
pat_fn!(binary
    float_mul, FloatBinary, strider_ir::FloatBinaryOp::Mul,
    "Pattern: `FloatBinaryOp::Mul` (`a * b`). Commutative."
);
pat_fn!(binary float_div, FloatBinary, strider_ir::FloatBinaryOp::Div,
    "Pattern: `FloatBinaryOp::Div` (`a / b`).");

/// Pattern: float subtraction `a - b` (lifter-canonical `FloatAdd(a, Neg(b))`).
#[pyfunction]
pub fn float_sub(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatSub(l, r))
}

pat_fn!(unary float_neg, FloatUnary, FloatUnaryKind::Neg,
    "Pattern: `FloatUnaryOp::Neg` (`-x`).");
pat_fn!(unary float_abs, FloatUnary, FloatUnaryKind::Abs,
    "Pattern: `FloatUnaryOp::Abs` (`fabs(x)`).");
pat_fn!(unary
    float_sqrt, FloatUnary, FloatUnaryKind::Sqrt,
    "Pattern: `FloatUnaryOp::Sqrt` (`sqrt(x)`)."
);
pat_fn!(unary
    float_ceil, FloatUnary, FloatUnaryKind::Ceil,
    "Pattern: `FloatUnaryOp::Ceil` (`ceil(x)`)."
);
pat_fn!(unary
    float_floor, FloatUnary, FloatUnaryKind::Floor,
    "Pattern: `FloatUnaryOp::Floor` (`floor(x)`)."
);
pat_fn!(unary
    float_round, FloatUnary, FloatUnaryKind::Round,
    "Pattern: `FloatUnaryOp::Round` (round-to-nearest-even)."
);

/// Pattern: `x` is NaN, the IEEE 754 self-inequality `Xor(FloatEqual(x, x), 1)`.
#[pyfunction]
pub fn float_is_nan(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatIsNan(operand))
}

/// Pattern: `FloatCmpOp::Equal` (`a == b`). Commutative.
#[pyfunction]
pub fn float_eq(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatCmp(strider_ir::FloatCmpOp::Equal, l, r))
}

/// Pattern: `FloatCmpOp::Less` (`a < b`).
#[pyfunction]
pub fn float_lt(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatCmp(strider_ir::FloatCmpOp::Less, l, r))
}

/// Pattern: float `a != b` (lifter-canonical `Xor(FloatEqual(a, b), 1)`).
#[pyfunction]
pub fn float_ne(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatNe(l, r))
}

/// Pattern: float `a <= b` (NaN-aware `Or(FloatLess(a, b), FloatEqual(a, b))`).
#[pyfunction]
pub fn float_le(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatLe(l, r))
}

pat_fn!(unary
    int_to_float, Cast, CastKind::IntToFloat,
    "Pattern: `IntToFloat` — int→float conversion."
);
pat_fn!(unary
    float_to_int, Cast, CastKind::FloatToInt,
    "Pattern: `FloatToInt` — float→int conversion."
);
pat_fn!(unary
    float_to_float, Cast, CastKind::FloatToFloat,
    "Pattern: `FloatToFloat` — float→float re-width."
);
pat_fn!(unary
    int_bits_to_float, Cast, CastKind::IntBitsToFloat,
    "Pattern: `IntBitsToFloat` — reinterpret int bits."
);
pat_fn!(unary
    float_bits_to_int, Cast, CastKind::FloatBitsToInt,
    "Pattern: `FloatBitsToInt` — reinterpret float bits."
);
pat_fn!(unary
    truncate, Cast, CastKind::Truncate,
    "Pattern: `Truncate` — narrow an integer."
);
pat_fn!(unary zero_extend, Cast, CastKind::ZeroExtend, "Pattern: `Extend(ZeroExtend)`.");
pat_fn!(unary sign_extend, Cast, CastKind::SignExtend, "Pattern: `Extend(SignExtend)`.");

/// `extend(op, operand)` where `op` is "zero" / "zero_extend" / "sign" /
/// "sign_extend".
#[pyfunction]
pub fn extend(op: &str, operand: Py<PyAny>) -> PyResult<PyPat> {
    let extend_op = parse_extend_op(op)?;
    Ok(PyPat::from_repr(PatRepr::Extend(extend_op, operand)))
}

/// Match any `IntBinaryOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn int_bin_any(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntBinAny(c.inner, l, r))
}

/// Match any `IntUnaryOp` over `operand` and bind the op variant to `c`.
#[pyfunction]
pub fn int_un_any(c: PyRef<'_, PyCapture>, operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnAny(c.inner, operand))
}

/// Match any `IntCmpOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn int_cmp_any(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntCmpAny(c.inner, l, r))
}

/// Match any boolean binary op over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn bool_bin_any(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BoolBinAny(c.inner, l, r))
}

/// Match any `FloatBinaryOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn float_bin_any(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatBinAny(c.inner, l, r))
}

/// Match any `FloatUnaryOp` over `operand` and bind the op variant to `c`.
#[pyfunction]
pub fn float_un_any(c: PyRef<'_, PyCapture>, operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatUnAny(c.inner, operand))
}

/// Match any `FloatCmpOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn float_cmp_any(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatCmpAny(c.inner, l, r))
}

// A typed builder accumulates field state and replays it onto the core builder
// at finalise time. `.when()` reaches the core two ways: value-rooted builders
// wrap it via `wrap_when` at the `MatchPat` level, node-rooted ones attach it
// as a root post-match guard on the finished `Pattern`.

/// Leaves the original in place so the builder stays reusable.
fn clone_opt(py: Python<'_>, slot: &Option<Py<PyAny>>) -> Option<Py<PyAny>> {
    slot.as_ref().map(|p| p.clone_ref(py))
}

#[derive(Default)]
struct CommonState {
    capture: Option<Capture>,
    when: Option<PyObject>,
}

macro_rules! builder_common_methods {
    ($ty:ty) => {
        #[gen_stub_pymethods]
        #[pymethods]
        impl $ty {
            /// Capture the matched node under the given `Capture`.
            fn capture<'py>(slf: PyRef<'py, Self>, c: PyRef<'py, PyCapture>) -> PyRef<'py, Self> {
                slf.common.borrow_mut().capture = Some(c.inner);
                slf
            }
            /// Capture under a string name, auto-interned.
            fn cap<'py>(slf: PyRef<'py, Self>, name: &str) -> PyResult<PyRef<'py, Self>> {
                let c = intern_str(name)?;
                slf.common.borrow_mut().capture = Some(c);
                Ok(slf)
            }
            /// Attach a Python predicate that runs after the match.
            fn when(slf: PyRef<'_, Self>, f: PyObject) -> PyRef<'_, Self> {
                slf.common.borrow_mut().when = Some(f);
                slf
            }
            /// Finalise into a `Pat`.
            fn into_pat(&self, py: Python<'_>) -> PyResult<PyPat> {
                let pat = self.build_pattern_py(py)?;
                Ok(PyPat::from_repr(PatRepr::Finished(Box::new(
                    std::cell::RefCell::new(Some(pat)),
                ))))
            }
        }
    };
}

// Every node-rooted builder is the same skeleton over a different operand
// field-set, generated here from a compact spec so the `.when()` wiring and
// capture handling live in ONE place.
//
// The three root flavors differ only in how `build_pattern_py` and the
// nestable-compile methods derive from `core_builder`:
//
//   * `value` produces a value and exposes `compile_value`, so it nests as a
//     value operand. `.when()` rides on `wrap_when`.
//   * `mem` produces a memory token and exposes `compile_mem` for a `mem_in`
//     slot. `.when()` is applied by `apply_when_to_pattern`.
//   * `node` is node-rooted only: same build as `mem`, no nestable compile.
//
// Field kinds: `pat` / `mem` (one operand slot), `multi_match` / `multi_mem`
// (indexed operand vectors), `scalar` (Copy), `scalar_clone`, `scalar_inner`
// (a Py wrapper stored via `.inner`), and `flag` (no-arg bool setter).

macro_rules! node_builder {
    // A macro call cannot sit in struct-field position, so the field decls
    // accumulate here and the `*Inner` struct is emitted once the list runs
    // out.
    (@members $inner:ident [ $($acc:tt)* ] ) => {
        #[derive(Default)]
        struct $inner {
            $($acc)*
        }
    };
    (@members $inner:ident [ $($acc:tt)* ] { pat $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<Py<PyAny>>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { mem $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<Py<PyAny>>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { multi_pat $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Vec<Py<PyAny>>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { multi_match $name:ident($idx:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Vec<($idx, Py<PyAny>)>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { multi_mem $name:ident($idx:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Vec<($idx, Py<PyAny>)>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar_clone $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar_inner $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { flag $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: bool, ] $($rest)*);
    };

    (@apply $self:ident, $py:ident, $b:ident, { pat $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = clone_opt($py, &$self.inner.borrow().$name) {
            $b = $b.$m(compile_operand_match(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { mem $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = clone_opt($py, &$self.inner.borrow().$name) {
            $b = $b.$m(compile_operand_mem(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { multi_pat $name:ident: $m:ident = $doc:literal }) => {
        let __items: Vec<Py<PyAny>> = $self
            .inner
            .borrow()
            .$name
            .iter()
            .map(|p| p.clone_ref($py))
            .collect();
        for __p in __items {
            $b = $b.$m(compile_operand_match(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { multi_match $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        let __items: Vec<($idx, Py<PyAny>)> = $self
            .inner
            .borrow()
            .$name
            .iter()
            .map(|(i, p)| (*i, p.clone_ref($py)))
            .collect();
        for (__idx, __p) in __items {
            $b = $b.$m(__idx, compile_operand_match(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { multi_mem $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        let __items: Vec<($idx, Py<PyAny>)> = $self
            .inner
            .borrow()
            .$name
            .iter()
            .map(|(i, p)| (*i, p.clone_ref($py)))
            .collect();
        for (__idx, __p) in __items {
            $b = $b.$m(__idx, compile_operand_mem(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar $name:ident($set:ty => $store:ty): $m:ident = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar_clone $name:ident($set:ty => $store:ty): $m:ident = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name.clone() {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar_inner $name:ident($set:ty => $store:ty): $m:ident = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { flag $name:ident: $m:ident = $doc:literal }) => {
        if $self.inner.borrow().$name {
            $b = $b.$m();
        }
    };

    // pyo3's proc-macro cannot see through a nested macro item, so the setters
    // cannot be `node_builder!(@setter ...)` calls inside `#[pymethods]`.
    // Instead the field list is munched one head at a time into an
    // accumulator, and the whole impl is emitted once the list empties.
    (@setters $ty:ident [ $($acc:tt)* ] ) => {
        #[gen_stub_pymethods]
        #[pymethods]
        impl $ty {
            $($acc)*
        }
    };
    (@setters $ty:ident [ $($acc:tt)* ] { pat $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some(p);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { mem $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some(p);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { multi_pat $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $m<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push(p);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { multi_match $name:ident($idx:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $m<'py>(slf: PyRef<'py, Self>, idx: $idx, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push((idx, p));
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { multi_mem $name:ident($idx:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $m<'py>(slf: PyRef<'py, Self>, idx: $idx, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push((idx, p));
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, v: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some(v);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar_clone $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, v: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some(v);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar_inner $name:ident($set:ty => $store:ty): $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, v: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some(v.inner);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { flag $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = true;
                slf
            }
        ] $($rest)*);
    };

    (@flavor value $py_name:literal $core:path) => {
        /// Compile as a value operand (`MatchPat`), honouring `.when()`.
        fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let b = self.core_builder(py)?;
            let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
            Ok(match when {
                Some(f) => DynMatch(Box::new(move |mb| wrap_when(b, f).compile(mb))),
                None => DynMatch(Box::new(move |mb| b.compile(mb))),
            })
        }

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            Ok(self.compile_value(py)?.into_pattern())
        }
    };
    (@flavor mem $py_name:literal $core:path) => {
        /// Compile as a memory-token producer for a `mem_in` slot.
        fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
            let b = self.core_builder(py)?;
            Ok(DynMem(Box::new(move |mb| b.compile_mem(mb))))
        }

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };
    (@flavor mem_value $py_name:literal $core:path) => {
        /// Compile as a memory-token producer for a `mem_in` slot.
        fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
            let b = self.core_builder(py)?;
            Ok(DynMem(Box::new(move |mb| b.compile_mem(mb))))
        }

        /// Nest as a value operand, e.g. `add(x, call_other().name("f"))`.
        /// Loose by default: any value output, narrowed by `.res()`.
        fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let b = self.core_builder(py)?;
            let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
            Ok(match when {
                Some(f) => DynMatch(Box::new(move |mb| wrap_when(b, f).compile(mb))),
                None => DynMatch(Box::new(move |mb| b.compile(mb))),
            })
        }

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };
    (@flavor node $py_name:literal $core:path) => {
        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };
    // Node-rooted build with a `compile_value` that rejects value nesting: the
    // core `*Pat` only offers `.build()`.
    (@flavor value_err $py_name:literal $core:path) => {
        fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let _ = py;
            Err(into_strider_err(anyhow::anyhow!(concat!(
                $py_name,
                " cannot be nested as a value operand"
            ))))
        }

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };

    (
        ty: $ty:ident,
        inner: $inner:ident,
        py_name: $py_name:literal,
        doc: $doc:literal,
        core: $core:path,
        core_ty: $core_ty:ty,
        root: $root:ident,
        fields: [ $( $field:tt ),* $(,)? ] $(,)?
    ) => {
        node_builder!(@members $inner [] $($field)*);

        #[doc = $doc]
        #[gen_stub_pyclass]
        #[pyclass(name = $py_name, module = "strider.pattern", unsendable)]
        pub struct $ty {
            inner: std::cell::RefCell<$inner>,
            common: std::cell::RefCell<CommonState>,
        }

        impl $ty {
            fn new() -> Self {
                Self {
                    inner: std::cell::RefCell::new($inner::default()),
                    common: std::cell::RefCell::new(CommonState::default()),
                }
            }

            /// Applies every set field plus `common.capture`. `.when()` is
            /// applied per root flavor instead.
            fn core_builder(&self, py: Python<'_>) -> PyResult<$core_ty> {
                // A field-less builder such as `EntryPat` never touches
                // `self.inner` or `py`, so reference both unconditionally to
                // keep that case off `dead_code` / `unused_variables`.
                let _ = &self.inner;
                let _ = py;
                let mut b = $core();
                $( node_builder!(@apply self, py, b, $field); )*
                if let Some(c) = self.common.borrow().capture {
                    b = b.capture(c);
                }
                Ok(b)
            }

            node_builder!(@flavor $root $py_name $core);
        }

        node_builder!(@setters $ty [] $($field)*);
        builder_common_methods!($ty);
    };
}

node_builder! {
    ty: PyLoadPat,
    inner: LoadInner,
    py_name: "LoadPat",
    doc: "Typed builder for `Load` node patterns. Chain `.addr(p)`, \
          `.space(s)`, `.mem_in(m)`, `.bit_width(n)`, `.stack_only()`.",
    core: strider_pattern::load,
    core_ty: strider_pattern::LoadPat,
    root: value,
    fields: [
        { scalar_inner space(crate::sleigh::PyVnSpace => rsleigh::VnSpace): space
            = "Restrict the match to a specific memory space." },
        { pat addr: addr = "Constrain the load's address operand." },
        { mem mem_in: mem_in
            = "Constrain the load's memory predecessor (a memory-producing sub-pattern)." },
        { multi_pat any_input: any_input
            = "Require SOME input of the Load to match `p` (candidates: mem, addr). \
               A typed sub only binds addr; a kind-unconstrained sub (var/anything) \
               can also bind the memory edge. Repeatable." },
        { scalar bit_width(u32 => u32): bit_width = "Filter loads by value width in bits." },
        { scalar stack_offset(i128 => i128): stack_offset
            = "Match only loads whose address decomposes to exactly `sp + k`." },
        { flag stack_only: stack_only
            = "Reject matches where the SP-relative offset is unknown." },
    ],
}

/// Start a `Load` pattern builder, optionally pre-setting the address.
#[pyfunction]
#[pyo3(signature = (addr=None))]
pub fn load(addr: Option<Py<PyAny>>) -> PyLoadPat {
    let b = PyLoadPat::new();
    if let Some(a) = addr {
        b.inner.borrow_mut().addr = Some(a);
    }
    b
}

node_builder! {
    ty: PyStorePat,
    inner: StoreInner,
    py_name: "StorePat",
    doc: "Typed builder for `Store` node patterns.",
    core: strider_pattern::store,
    core_ty: strider_pattern::StorePat,
    root: mem,
    fields: [
        { pat addr: addr = "Constrain the store's address operand." },
        { pat data: data = "Constrain the store's stored-value operand." },
        { scalar_inner space(crate::sleigh::PyVnSpace => rsleigh::VnSpace): space
            = "Restrict the match to a specific memory space." },
        { mem mem_in: mem_in = "Constrain the store's memory predecessor." },
        { multi_pat any_input: any_input
            = "Require SOME input of the Store to match `p` (candidates: mem, addr, \
               data). A typed sub only binds addr/data; a kind-unconstrained sub \
               (var/anything) can also bind the memory edge. Repeatable." },
        { scalar bit_width(u32 => u32): bit_width = "Filter stores by data width in bits." },
        { scalar stack_offset(i128 => i128): stack_offset
            = "Match only stores whose address decomposes to exactly `sp + k`." },
        { flag stack_only: stack_only
            = "Reject matches where the SP-relative offset is unknown." },
    ],
}

/// Start a `Store` pattern builder, optionally pre-setting addr / data.
#[pyfunction]
#[pyo3(signature = (addr=None, data=None))]
pub fn store(addr: Option<Py<PyAny>>, data: Option<Py<PyAny>>) -> PyStorePat {
    let b = PyStorePat::new();
    {
        let mut inner = b.inner.borrow_mut();
        inner.addr = addr;
        inner.data = data;
    }
    b
}

#[derive(Default)]
struct CallInner {
    target: Option<Py<PyAny>>,
    args: Vec<(usize, Py<PyAny>)>,
    mem: Option<Py<PyAny>>,
    any_input: Vec<Py<PyAny>>,
    /// Pins a nested value operand to the declared result output, excluding
    /// caller-saved clobber outputs.
    res: bool,
    outputs: Vec<OutputSpecPy>,
}

/// One `.output(j)` sibling-output constraint.
#[derive(Clone, Copy)]
struct OutputSpecPy {
    slot: usize,
    capture: Option<Capture>,
    width: Option<u32>,
    ty: Option<T>,
}

/// Typed builder for `Call` node patterns. Chain `.at(addr)`, `.at_any(addrs)`,
/// `.target(p)`, `.arg(idx, p)`, `.mem(m)`.
#[gen_stub_pyclass]
#[pyclass(name = "CallPat", module = "strider.pattern", unsendable)]
pub struct PyCallPat {
    inner: std::cell::RefCell<CallInner>,
    common: std::cell::RefCell<CommonState>,
    /// Carried apart from `inner.target` so the literal-address forms and a
    /// `target` Pat cannot clash.
    at_target: std::cell::RefCell<Option<CallTarget>>,
}

enum CallTarget {
    At(u64),
    AtAny(Vec<u64>),
}

impl PyCallPat {
    fn new() -> Self {
        Self {
            inner: std::cell::RefCell::new(CallInner::default()),
            common: std::cell::RefCell::new(CommonState::default()),
            at_target: std::cell::RefCell::new(None),
        }
    }

    fn core_builder(&self, py: Python<'_>) -> PyResult<strider_pattern::CallPat> {
        let mut b = strider_pattern::call();
        // A literal address wins over a Pat target when both are set.
        match &*self.at_target.borrow() {
            Some(CallTarget::At(a)) => b = b.at(*a),
            Some(CallTarget::AtAny(addrs)) => b = b.at_any(addrs.clone()),
            None => {
                if let Some(t) = clone_opt(py, &self.inner.borrow().target) {
                    b = b.target(compile_operand_match(t.bind(py))?);
                }
            }
        }
        let args: Vec<(usize, Py<PyAny>)> = self
            .inner
            .borrow()
            .args
            .iter()
            .map(|(i, p)| (*i, p.clone_ref(py)))
            .collect();
        for (idx, p) in args {
            b = b.arg(idx, compile_operand_match(p.bind(py))?);
        }
        if let Some(m) = clone_opt(py, &self.inner.borrow().mem) {
            b = b.mem(compile_operand_mem(m.bind(py))?);
        }
        let any_inputs: Vec<Py<PyAny>> = self
            .inner
            .borrow()
            .any_input
            .iter()
            .map(|p| p.clone_ref(py))
            .collect();
        for p in any_inputs {
            b = b.any_input(compile_operand_match(p.bind(py))?);
        }
        if self.inner.borrow().res {
            b = b.res();
        }
        let outputs: Vec<OutputSpecPy> = self.inner.borrow().outputs.clone();
        for spec in outputs {
            if let Some(c) = spec.capture {
                b = b.output(spec.slot).capture(c);
            } else if let Some(w) = spec.width {
                b = b.output(spec.slot).of_width(w);
            } else if let Some(t) = spec.ty {
                b = b.output(spec.slot).of_type(t);
            }
        }
        if let Some(c) = self.common.borrow().capture {
            b = b.capture(c);
        }
        Ok(b)
    }

    fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
        let b = self.core_builder(py)?;
        Ok(DynMem(Box::new(move |mb| b.compile_mem(mb))))
    }

    /// Loose by default: any value output, narrowed by `.res()`.
    fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
        let b = self.core_builder(py)?;
        let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
        Ok(match when {
            Some(f) => DynMatch(Box::new(move |mb| wrap_when(b, f).compile(mb))),
            None => DynMatch(Box::new(move |mb| b.compile(mb))),
        })
    }

    fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
        let pat = self.core_builder(py)?.build();
        Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCallPat {
    /// Constrain the call target with an arbitrary pattern.
    fn target<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().target = Some(p);
        slf
    }
    /// Constrain the call target to the literal address `addr`.
    fn at(slf: PyRef<'_, Self>, addr: u64) -> PyRef<'_, Self> {
        *slf.at_target.borrow_mut() = Some(CallTarget::At(addr));
        slf
    }
    /// Constrain the call target to any address in `addrs`.
    fn at_any(slf: PyRef<'_, Self>, addrs: Vec<u64>) -> PyRef<'_, Self> {
        *slf.at_target.borrow_mut() = Some(CallTarget::AtAny(addrs));
        slf
    }
    /// Constrain positional argument `idx` (0-based).
    fn arg<'py>(slf: PyRef<'py, Self>, idx: usize, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().args.push((idx, p));
        slf
    }
    /// Constrain the call's memory predecessor.
    fn mem<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().mem = Some(p);
        slf
    }
    /// Require SOME input of the Call to match `p`; candidates are ctrl, mem,
    /// target, sp and each arg. A typed sub only binds a value input; a
    /// kind-unconstrained sub can also bind the control or memory edge.
    /// Repeatable.
    fn any_input<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().any_input.push(p);
        slf
    }
    /// When nested as a value operand, pin it to the declared result output,
    /// excluding caller-saved clobbers.
    fn res(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf.inner.borrow_mut().res = true;
        slf
    }

    /// Bind or constrain the value at raw output `slot`. Slots are
    /// `[Control(0), Memory(1), result(2), ...clobbers]`, so `output(2)` is
    /// the first return value. Returns a terminal builder taking one of
    /// `.capture(c)`, `.of_width(w)`, `.of_type("i64")`.
    ///
    /// This names the output value itself; it does not recurse into whatever
    /// consumes that output.
    fn output(slf: Bound<'_, Self>, slot: usize) -> PyCallOutput {
        PyCallOutput {
            parent: slf.unbind(),
            slot,
        }
    }
}
builder_common_methods!(PyCallPat);

/// Returned by `CallPat.output(slot)`. Each terminal commits one constraint
/// onto the parent `CallPat` and hands it back so the chain continues.
#[gen_stub_pyclass]
#[pyclass(name = "CallOutputPat", module = "strider.pattern", unsendable)]
pub struct PyCallOutput {
    parent: Py<PyCallPat>,
    slot: usize,
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCallOutput {
    /// Bind the sibling output's value to `c`.
    fn capture(&self, py: Python<'_>, c: PyRef<'_, PyCapture>) -> Py<PyCallPat> {
        self.parent
            .borrow(py)
            .inner
            .borrow_mut()
            .outputs
            .push(OutputSpecPy {
                slot: self.slot,
                capture: Some(c.inner),
                width: None,
                ty: None,
            });
        self.parent.clone_ref(py)
    }

    /// Constrain the sibling output to bit width `bits`.
    fn of_width(&self, py: Python<'_>, bits: u32) -> Py<PyCallPat> {
        self.parent
            .borrow(py)
            .inner
            .borrow_mut()
            .outputs
            .push(OutputSpecPy {
                slot: self.slot,
                capture: None,
                width: Some(bits),
                ty: None,
            });
        self.parent.clone_ref(py)
    }

    /// Constrain the sibling output to the type named by `ty`, e.g. `"i64"`.
    fn of_type(&self, py: Python<'_>, ty: &str) -> PyResult<Py<PyCallPat>> {
        let t = parse_value_ty(ty)?;
        self.parent
            .borrow(py)
            .inner
            .borrow_mut()
            .outputs
            .push(OutputSpecPy {
                slot: self.slot,
                capture: None,
                width: None,
                ty: Some(t),
            });
        Ok(self.parent.clone_ref(py))
    }
}

/// Start a `Call` pattern builder, optionally pinning the target to `at`.
#[pyfunction]
#[pyo3(signature = (at=None))]
pub fn call(at: Option<u64>) -> PyCallPat {
    let b = PyCallPat::new();
    if let Some(addr) = at {
        *b.at_target.borrow_mut() = Some(CallTarget::At(addr));
    }
    b
}

node_builder! {
    ty: PyCallOtherPat,
    inner: CallOtherInner,
    py_name: "CallOtherPat",
    doc: "Typed builder for `CallOther` node patterns.",
    core: strider_pattern::call_other,
    core_ty: strider_pattern::CallOtherPat,
    root: mem_value,
    fields: [
        { scalar user_op_id(u64 => u64): user_op_id
            = "Constrain the matched node's user-op id." },
        { scalar_clone name(String => String): name
            = "Constrain the matched node's user-op name." },
        { pat ctrl: ctrl
            = "Match the CallOther's control predecessor (`inputs[0]`)." },
        { mem mem: mem
            = "Match the CallOther's memory predecessor (`inputs[1]`); \
               takes a memory producer (store / mem_phi / call / call_other)." },
        { multi_match args(usize): arg
            = "Constrain raw `inputs[idx]` of the matched CallOther." },
        { multi_pat any_input: any_input
            = "Require SOME input of the CallOther to match `p`, without pinning \
               which slot. Repeatable." },
        { flag res: res
            = "When nested as a value operand, pin it to the declared result \
               output (excludes implicit-write clobber outputs)." },
    ],
}

/// Start a `CallOther` pattern builder.
#[pyfunction]
pub fn call_other() -> PyCallOtherPat {
    PyCallOtherPat::new()
}

node_builder! {
    ty: PyRetPat,
    inner: RetInner,
    py_name: "RetPat",
    doc: "Typed builder for `Return` node patterns.",
    core: strider_pattern::ret,
    core_ty: strider_pattern::RetPat,
    root: node,
    fields: [
        { pat preceded_by: preceded_by
            = "Match `p` against the Return's direct ctrl predecessor." },
        { multi_match ret_vals(usize): ret_val = "Constrain return value at position `idx`." },
        { multi_pat any_input: any_input
            = "Require SOME input of the Return to match `p`, without pinning \
               which slot. Repeatable." },
    ],
}

/// Start a `Return` pattern builder.
#[pyfunction]
pub fn ret() -> PyRetPat {
    PyRetPat::new()
}

node_builder! {
    ty: PyIndirectBranchPat,
    inner: IndirectBranchInner,
    py_name: "IndirectBranchPat",
    doc: "Typed builder for `IndirectBranch` node patterns.",
    core: strider_pattern::indirect_branch,
    core_ty: strider_pattern::IndirectBranchPat,
    root: node,
    fields: [
        { pat target: target
            = "Constrain the dispatch target (`inputs[2]`)." },
        { pat preceded_by: preceded_by
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
        { mem mem: mem
            = "Constrain the node's memory predecessor (`inputs[1]`)." },
        { multi_pat any_input: any_input
            = "Require SOME input of the IndirectBranch to match `p`, without pinning \
               which slot. Repeatable." },
    ],
}

/// Start an `IndirectBranch` pattern builder.
#[pyfunction]
pub fn indirect_branch() -> PyIndirectBranchPat {
    PyIndirectBranchPat::new()
}

node_builder! {
    ty: PyUnreachablePat,
    inner: UnreachableInner,
    py_name: "UnreachablePat",
    doc: "Typed builder for `Unreachable` node patterns.",
    core: strider_pattern::unreachable,
    core_ty: strider_pattern::UnreachablePat,
    root: node,
    fields: [
        { pat preceded_by: preceded_by
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
        { multi_pat any_input: any_input
            = "Require SOME input of the Unreachable to match `p`, without pinning \
               which slot. Repeatable." },
    ],
}

/// Start an `Unreachable` pattern builder.
#[pyfunction]
pub fn unreachable() -> PyUnreachablePat {
    PyUnreachablePat::new()
}

node_builder! {
    ty: PySwitchPat,
    inner: SwitchInner,
    py_name: "SwitchPat",
    doc: "Typed builder for `Switch` node patterns.",
    core: strider_pattern::switch,
    core_ty: strider_pattern::SwitchPat,
    root: node,
    fields: [
        { pat address: address
            = "Constrain the dispatch address (`inputs[1]`)." },
        { pat preceded_by: preceded_by
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
        { multi_pat any_input: any_input
            = "Require SOME input of the Switch to match `p`, without pinning \
               which slot. Repeatable." },
    ],
}

/// Start a `Switch` pattern builder.
#[pyfunction]
pub fn switch() -> PySwitchPat {
    PySwitchPat::new()
}

#[derive(Default)]
struct IfInner {
    cond: Option<Py<PyAny>>,
    true_branch: Option<Py<PyAny>>,
    false_branch: Option<Py<PyAny>>,
    capture_true: Option<Capture>,
    capture_false: Option<Capture>,
}

/// Typed builder for `If` node patterns.
#[gen_stub_pyclass]
#[pyclass(name = "IfPat", module = "strider.pattern", unsendable)]
pub struct PyIfPat {
    inner: std::cell::RefCell<IfInner>,
    common: std::cell::RefCell<CommonState>,
}

impl PyIfPat {
    fn new() -> Self {
        Self {
            inner: std::cell::RefCell::new(IfInner::default()),
            common: std::cell::RefCell::new(CommonState::default()),
        }
    }

    fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
        let mut b = strider_pattern::if_node();
        if let Some(c) = clone_opt(py, &self.inner.borrow().cond) {
            b = b.cond(compile_operand_match(c.bind(py))?);
        }
        // Branches forward-walk to the single consumer and match a finished
        // Pattern there.
        if let Some(t) = clone_opt(py, &self.inner.borrow().true_branch) {
            let pat = pattern_for_operand(t.bind(py))?;
            b = b.with_true(pat);
        }
        if let Some(f) = clone_opt(py, &self.inner.borrow().false_branch) {
            let pat = pattern_for_operand(f.bind(py))?;
            b = b.with_false(pat);
        }
        if let Some(c) = self.common.borrow().capture {
            b = b.capture(c);
        }
        if let Some(c) = self.inner.borrow().capture_true {
            b = b.capture_true(c);
        }
        if let Some(c) = self.inner.borrow().capture_false {
            b = b.capture_false(c);
        }
        Ok(apply_when_to_pattern(py, &self.common.borrow(), b.build()))
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyIfPat {
    /// Constrain the If's condition operand.
    fn cond<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().cond = Some(p);
        slf
    }
    /// Match the unique consumer of the If's true output.
    fn true_branch<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().true_branch = Some(p);
        slf
    }
    /// Match the unique consumer of the If's false output.
    fn false_branch<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().false_branch = Some(p);
        slf
    }
    /// Bind the If's true control-output value to `c`, the edge operand
    /// `dominates` / `phi_input_from_edge` take.
    fn capture_true<'py>(slf: PyRef<'py, Self>, c: PyRef<'py, PyCapture>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().capture_true = Some(c.inner);
        slf
    }
    /// Bind the If's false control-output value to `c`. See `capture_true`.
    fn capture_false<'py>(slf: PyRef<'py, Self>, c: PyRef<'py, PyCapture>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().capture_false = Some(c.inner);
        slf
    }
}
builder_common_methods!(PyIfPat);

fn pattern_for_operand(ob: &Bound<'_, PyAny>) -> PyResult<Pattern> {
    let py = ob.py();
    let like = ob.extract::<PatLike<'_>>()?;
    like.to_pattern(py)
}

/// Start an `If` pattern builder, optionally pre-setting the condition.
#[pyfunction(name = "if_else")]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<Py<PyAny>>) -> PyIfPat {
    let b = PyIfPat::new();
    if let Some(c) = cond {
        b.inner.borrow_mut().cond = Some(c);
    }
    b
}

// These live in the `strider.pattern.constraints` submodule: a pattern
// describes graph SHAPE, a constraint is a relational predicate over captures
// evaluated post-join.

/// A CFG relation between captured entities, passed to
/// `Function.find_all([...], constraints=[...])` to filter joined tuples.
/// Construct via `dominates` / `phi_input_from_edge`, and negate with `negate`.
#[gen_stub_pyclass]
#[pyclass(
    name = "JoinConstraint",
    module = "strider.pattern.constraints",
    frozen
)]
pub struct PyJoinConstraint {
    pub(crate) inner: JoinConstraint,
}

/// `a` dominates `b`: every path from entry to `b` passes through `a`.
/// Operands are captured nodes (or an `If` branch-edge capture).
#[pyfunction]
pub fn dominates(a: PyRef<'_, PyCapture>, b: PyRef<'_, PyCapture>) -> PyJoinConstraint {
    PyJoinConstraint {
        inner: JoinConstraint::Dominates {
            dominator: a.inner,
            dominated: b.inner,
        },
    }
}

/// The value merged into `phi` from the branch `edge` is `value`. `edge` binds
/// an `If`'s `capture_true` / `capture_false` value, and `value` is bound by
/// another pattern in the same `find_all` list.
#[pyfunction]
pub fn phi_input_from_edge(
    phi: PyRef<'_, PyCapture>,
    edge: PyRef<'_, PyCapture>,
    value: PyRef<'_, PyCapture>,
) -> PyJoinConstraint {
    PyJoinConstraint {
        inner: JoinConstraint::PhiInputFromEdge {
            phi: phi.inner,
            edge: edge.inner,
            value: value.inner,
        },
    }
}

/// The negation of `c`: a match survives only if `c` does not hold.
#[pyfunction]
pub fn negate(c: PyRef<'_, PyJoinConstraint>) -> PyJoinConstraint {
    PyJoinConstraint {
        inner: JoinConstraint::Not(Box::new(c.inner.clone())),
    }
}

/// A constraint that passes when ANY of the listed constraints passes. An
/// empty list passes nothing.
///
/// The top-level `constraints=[...]` list is already an AND, so `any_of` is
/// how you express OR.
#[pyfunction]
pub fn any_of(constraints: Vec<PyRef<'_, PyJoinConstraint>>) -> PyJoinConstraint {
    PyJoinConstraint {
        inner: JoinConstraint::Or(constraints.iter().map(|c| c.inner.clone()).collect()),
    }
}

/// A constraint that passes only when EVERY listed constraint passes. An
/// empty list passes everything. Use it to AND constraints inside an `any_of`
/// (the top-level list does not nest).
#[pyfunction]
pub fn all_of(constraints: Vec<PyRef<'_, PyJoinConstraint>>) -> PyJoinConstraint {
    PyJoinConstraint {
        inner: JoinConstraint::And(constraints.iter().map(|c| c.inner.clone()).collect()),
    }
}

node_builder! {
    ty: PyPhiPat,
    inner: PhiInner,
    py_name: "PhiPat",
    doc: "Typed builder for tagged-`Phi` patterns.",
    core: strider_pattern::phi,
    core_ty: strider_pattern::PhiPat,
    root: value,
    fields: [
        { scalar_inner for_vn(crate::sleigh::PyVn => rsleigh::Vn): for_vn
            = "Restrict the match to phi nodes for varnode `vn`." },
        { multi_match inputs(usize): input
            = "Constrain the value arriving from predecessor slot `idx`." },
        { multi_pat any_input: any_input
            = "Require SOME data input of the phi to match `p`, without pinning \
               which predecessor slot (a phi's incoming values are usually \
               order-irrelevant). Repeatable: each call adds a constraint bound \
               to a DISTINCT input slot. Captures inside `p` bind out normally." },
        { pat phi_token: phi_token
            = "Constrain the phi's ownership edge — the PhiToken input at raw \
               slot 0 (the owning Region's PhiToken output). Unlike `.input(i, \
               p)`, which shifts by +1 past this slot, this targets slot 0 \
               directly. A typed sub can never bind it (PhiToken falls outside \
               the value domain a typed sub matches); use var()/anything() to \
               bind the edge." },
    ],
}

/// Start a tagged-`Phi` pattern builder.
#[pyfunction]
pub fn phi() -> PyPhiPat {
    PyPhiPat::new()
}

/// Match a tagged `Phi` for a specific varnode.
#[pyfunction]
pub fn phi_for(vn: crate::sleigh::PyVn) -> PyPhiPat {
    let b = PyPhiPat::new();
    b.inner.borrow_mut().for_vn = Some(vn.inner);
    b
}

node_builder! {
    ty: PyMemPhiPat,
    inner: MemPhiInner,
    py_name: "MemPhiPat",
    doc: "Typed builder for `MemPhi` patterns.",
    core: strider_pattern::mem_phi,
    core_ty: strider_pattern::MemPhiPat,
    root: mem,
    fields: [
        { multi_mem inputs(usize): input
            = "Constrain the memory token arriving from predecessor slot `idx`." },
        { multi_pat any_input: any_input
            = "Require SOME input of the MemPhi to match `p`, without pinning \
               which slot (candidates: PhiToken at slot 0, each memory \
               predecessor). A typed value sub can never bind a Memory or \
               PhiToken edge — only a kind-unconstrained sub (var/anything) \
               reaches them. Repeatable." },
        { pat phi_token: phi_token
            = "Constrain the MemPhi's ownership edge — the PhiToken input at \
               raw slot 0 (the owning Region's PhiToken output). Unlike \
               `.input(i, p)`, which shifts by +1 past this slot, this targets \
               slot 0 directly. See PhiPat.phi_token for the value-phi \
               analogue." },
    ],
}

/// Start a `MemPhi` pattern builder.
#[pyfunction]
pub fn mem_phi() -> PyMemPhiPat {
    PyMemPhiPat::new()
}

node_builder! {
    ty: PyEntryPat,
    inner: EntryInner,
    py_name: "EntryPat",
    doc: "Typed builder for the function's unique `Entry` node pattern. \
          `Entry` has no inputs and one control output — the function's \
          initial control edge. Nests as a control operand, e.g. \
          `region().any_input(entry())`.",
    core: strider_pattern::entry,
    core_ty: strider_pattern::EntryPat,
    root: value,
    fields: [],
}

/// Matches the function's unique `Entry` node.
#[pyfunction]
pub fn entry() -> PyEntryPat {
    PyEntryPat::new()
}

node_builder! {
    ty: PyRegionPat,
    inner: RegionInner,
    py_name: "RegionPat",
    doc: "Typed builder for `Region` (CFG-merge) node patterns. Chain \
          `.input(idx, p)` / `.any_input(p)` to constrain a control \
          predecessor. Nests as a control operand, e.g. \
          `region().any_input(region())`.",
    core: strider_pattern::region,
    core_ty: strider_pattern::RegionPat,
    root: value,
    fields: [
        { multi_match inputs(usize): input
            = "Constrain predecessor `idx`'s control edge (raw input slot \
               `idx` — Region has no fixed prefix ahead of its variadic \
               tail). The sub-pattern must be control-rooted (entry() / \
               region()) or an untyped wildcard (var/anything) — a typed \
               value sub can never bind a Control edge." },
        { multi_pat any_input: any_input
            = "Require SOME predecessor of the Region to match `p`, \
               without pinning which slot. Every Region input is Control, \
               so only an untyped wildcard or another control-rooted \
               pattern reaches one; a typed value sub matches nothing. \
               Repeatable." },
    ],
}

/// Matches any CFG-merge `Region` node.
#[pyfunction]
pub fn region() -> PyRegionPat {
    PyRegionPat::new()
}

/// Typed builder for `FunctionArg` carrier patterns. Chain `.index(i)`,
/// `.source_register(vn)`, `.source_stack(space, offset)`.
#[gen_stub_pyclass]
#[pyclass(name = "FunctionArgPat", module = "strider.pattern", unsendable)]
pub struct PyFunctionArgPat {
    source: std::cell::RefCell<Option<strider_ir::node::FunctionArgSource>>,
    index: std::cell::RefCell<Option<u32>>,
    common: std::cell::RefCell<CommonState>,
}

impl PyFunctionArgPat {
    fn new() -> Self {
        Self {
            source: std::cell::RefCell::new(None),
            index: std::cell::RefCell::new(None),
            common: std::cell::RefCell::new(CommonState::default()),
        }
    }

    fn core_builder(&self) -> strider_pattern::FunctionArgPat {
        let mut b = strider_pattern::function_arg_any();
        if let Some(s) = *self.source.borrow() {
            b = b.source(s);
        }
        if let Some(i) = *self.index.borrow() {
            b = b.index(i);
        }
        if let Some(c) = self.common.borrow().capture {
            b = b.capture(c);
        }
        b
    }

    fn compile_value(&self) -> DynMatch {
        let b = self.core_builder();
        DynMatch(Box::new(move |mb| b.compile(mb)))
    }

    fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
        let pat = self.core_builder().build();
        Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyFunctionArgPat {
    /// Constrain the match to argument at ABI position `i`.
    fn index(slf: PyRef<'_, Self>, i: u32) -> PyRef<'_, Self> {
        slf.index.replace(Some(i));
        slf
    }
    /// Constrain to an argument sourced from register varnode `vn`.
    fn source_register(slf: PyRef<'_, Self>, vn: crate::sleigh::PyVn) -> PyRef<'_, Self> {
        slf.source
            .replace(Some(strider_ir::node::FunctionArgSource::Register(
                vn.inner,
            )));
        slf
    }
    /// Constrain to an argument sourced from the stack at `(space, offset)`.
    fn source_stack(
        slf: PyRef<'_, Self>,
        space: crate::sleigh::PyVnSpace,
        offset: i128,
    ) -> PyRef<'_, Self> {
        slf.source
            .replace(Some(strider_ir::node::FunctionArgSource::Stack {
                space: space.inner,
                offset,
            }));
        slf
    }
}

builder_common_methods!(PyFunctionArgPat);

/// Start a `FunctionArg` pattern builder at index `i`.
#[pyfunction]
pub fn function_arg(i: u32) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.index.replace(Some(i));
    b
}

/// Start a `FunctionArg` pattern builder matching any index.
#[pyfunction]
pub fn function_arg_any() -> PyFunctionArgPat {
    PyFunctionArgPat::new()
}

/// Match a `FunctionArg` whose source is a specific register.
#[pyfunction]
pub fn function_arg_reg(vn: crate::sleigh::PyVn) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source
        .replace(Some(strider_ir::node::FunctionArgSource::Register(
            vn.inner,
        )));
    b
}

/// Match a `FunctionArg` whose source is a specific stack slot.
#[pyfunction]
pub fn function_arg_stack(space: crate::sleigh::PyVnSpace, offset: i128) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source
        .replace(Some(strider_ir::node::FunctionArgSource::Stack {
            space: space.inner,
            offset,
        }));
    b
}

macro_rules! binary_op_builder {
    (
        $ty:ident, $py_name:literal, $core:ident, $op_ty:ty,
        $doc:literal
    ) => {
        #[doc = $doc]
        #[gen_stub_pyclass]
        #[pyclass(name = $py_name, module = "strider.pattern", unsendable)]
        pub struct $ty {
            op: $op_ty,
            lhs: Py<PyAny>,
            rhs: Py<PyAny>,
            ordered: std::cell::Cell<bool>,
            common: std::cell::RefCell<CommonState>,
        }

        impl $ty {
            fn new(op: $op_ty, lhs: Py<PyAny>, rhs: Py<PyAny>) -> Self {
                Self {
                    op,
                    lhs,
                    rhs,
                    ordered: std::cell::Cell::new(false),
                    common: std::cell::RefCell::new(CommonState::default()),
                }
            }

            fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
                let op = self.op;
                let l = compile_operand_match(self.lhs.bind(py))?;
                let r = compile_operand_match(self.rhs.bind(py))?;
                let ordered = self.ordered.get();
                let capture = self.common.borrow().capture;
                let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
                Ok(DynMatch(Box::new(move |mb| {
                    let pat = strider_pattern::$core(op, l, r);
                    // `.ordered()` pins commutativity on the root and must be
                    // applied before capture / when. The branch exists because
                    // `pat.ordered()` is a distinct type, so each arm needs its
                    // own monomorphic `pat`.
                    if ordered {
                        apply_cap_when(mb, pat.ordered(), capture, when)
                    } else {
                        apply_cap_when(mb, pat, capture, when)
                    }
                })))
            }

            fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
                Ok(self.compile_value(py)?.into_pattern())
            }
        }

        #[gen_stub_pymethods]
        #[pymethods]
        impl $ty {
            /// Force left-to-right operand matching, disabling commutativity.
            ///
            /// Chainable, not terminal: the result stays lazy and nests as a
            /// value operand just like the bare commutative builder.
            fn ordered(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
                slf.ordered.set(true);
                slf
            }
        }

        builder_common_methods!($ty);
    };
}

fn apply_cap_when<P: MatchPat + 'static>(
    mb: &mut MatcherBuilder,
    pat: P,
    capture: Option<Capture>,
    when: Option<PyObject>,
) -> PatValueRef {
    match (capture, when) {
        (Some(c), Some(f)) => wrap_when(pat.capture(c), f).compile(mb),
        (Some(c), None) => pat.capture(c).compile(mb),
        (None, Some(f)) => wrap_when(pat, f).compile(mb),
        (None, None) => pat.compile(mb),
    }
}

binary_op_builder!(
    PyIntBinaryPat,
    "IntBinaryPat",
    int_binary,
    strider_ir::IntBinaryOp,
    "Typed builder for an integer binary-op pattern."
);
binary_op_builder!(
    PyFloatBinaryPat,
    "FloatBinaryPat",
    float_binary,
    strider_ir::FloatBinaryOp,
    "Typed builder for a float binary-op pattern."
);
binary_op_builder!(
    PyBoolBinaryPat,
    "BoolBinaryPat",
    bool_binary,
    strider_ir::IntBinaryOp,
    "Typed builder for a boolean binary-op pattern (`IntBinaryOp` at `I1`)."
);

/// Build an `IntBinaryOp` pattern builder for the named `op`.
#[pyfunction]
pub fn int_binary(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyIntBinaryPat> {
    Ok(PyIntBinaryPat::new(parse_int_binary_op(op)?, l, r))
}

/// Build a boolean binary pattern builder for the named `op`.
#[pyfunction]
pub fn bool_binary(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyBoolBinaryPat> {
    Ok(PyBoolBinaryPat::new(parse_bool_binary_op(op)?, l, r))
}

/// Build a `FloatBinaryOp` pattern builder for the named `op`.
#[pyfunction]
pub fn float_binary(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyFloatBinaryPat> {
    Ok(PyFloatBinaryPat::new(parse_float_binary_op(op)?, l, r))
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCapture>()?;
    m.add_class::<PyPat>()?;
    m.add_class::<PyIntBinaryPat>()?;
    m.add_class::<PyFloatBinaryPat>()?;
    m.add_class::<PyBoolBinaryPat>()?;
    m.add_class::<PyCallPat>()?;
    m.add_class::<PyCallOutput>()?;
    m.add_class::<PyCallOtherPat>()?;
    m.add_class::<PyRetPat>()?;
    m.add_class::<PyIndirectBranchPat>()?;
    m.add_class::<PyUnreachablePat>()?;
    m.add_class::<PySwitchPat>()?;
    m.add_class::<PyIfPat>()?;
    m.add_class::<PyLoadPat>()?;
    m.add_class::<PyStorePat>()?;
    m.add_class::<PyPhiPat>()?;
    m.add_class::<PyMemPhiPat>()?;
    m.add_class::<PyFunctionArgPat>()?;
    m.add_class::<PyEntryPat>()?;
    m.add_class::<PyRegionPat>()?;
    m.add_class::<PyCastMask>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, m)?)?;
        };
    }
    add_fn!(any_);
    add_fn!(var);
    add_fn!(value_of_width);
    add_fn!(bool_value);
    add_fn!(inputs_of_width);
    add_fn!(bool_inputs);
    add_fn!(int_const);
    add_fn!(signed_int_const);
    add_fn!(int_const_any_of);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(any_int_const);
    add_fn!(any_bool_const);
    add_fn!(any_float_const);
    add_fn!(initial_var);
    add_fn!(initial_var_for);
    add_fn!(one_of);
    add_fn!(function_arg);
    add_fn!(function_arg_any);
    add_fn!(function_arg_reg);
    add_fn!(function_arg_stack);
    add_fn!(phi);
    add_fn!(phi_for);
    add_fn!(mem_phi);
    add_fn!(entry);
    add_fn!(region);
    add_fn!(predicate);
    add_fn!(int_cmp);
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
    add_fn!(int_eq);
    add_fn!(int_ne);
    add_fn!(int_lt);
    add_fn!(int_le);
    add_fn!(int_slt);
    add_fn!(int_sle);
    add_fn!(int_carry);
    add_fn!(int_scarry);
    add_fn!(int_sborrow);
    add_fn!(neg);
    add_fn!(bit_not);
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
    add_fn!(float_is_nan);
    add_fn!(float_eq);
    add_fn!(float_ne);
    add_fn!(float_lt);
    add_fn!(float_le);
    add_fn!(int_to_float);
    add_fn!(float_to_int);
    add_fn!(float_to_float);
    add_fn!(int_bits_to_float);
    add_fn!(float_bits_to_int);
    add_fn!(truncate);
    add_fn!(popcount);
    add_fn!(lzcount);
    add_fn!(zero_extend);
    add_fn!(sign_extend);
    add_fn!(extend);
    add_fn!(load);
    add_fn!(store);
    add_fn!(call);
    add_fn!(call_other);
    add_fn!(ret);
    add_fn!(indirect_branch);
    add_fn!(unreachable);
    add_fn!(switch);
    add_fn!(if_);
    add_fn!(int_binary);
    add_fn!(bool_binary);
    add_fn!(float_binary);
    add_fn!(int_bin_any);
    add_fn!(int_un_any);
    add_fn!(int_cmp_any);
    add_fn!(bool_bin_any);
    add_fn!(float_bin_any);
    add_fn!(float_un_any);
    add_fn!(float_cmp_any);

    register_constraints(py, m)?;
    Ok(())
}

/// `parent` must be the `pattern` module so the `sys.modules` key is the full
/// dotted path. Without that, `from strider.pattern import constraints` fails
/// even though attribute access works.
fn register_constraints(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "constraints")?;
    m.add_class::<PyJoinConstraint>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, &m)?)?;
        };
    }
    add_fn!(dominates);
    add_fn!(phi_input_from_edge);
    add_fn!(negate);
    add_fn!(any_of);
    add_fn!(all_of);

    parent.add_submodule(&m)?;
    // The `sys.modules` entry that `import strider.pattern.constraints` needs
    // is inserted by the package `__init__.py`, which owns every dotted-path
    // registration.
    Ok(())
}
