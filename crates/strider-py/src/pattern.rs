//! `strider.pattern` submodule.
//!
//! Bridges Python's runtime pattern recursion onto the compile-time-
//! generic `strider_pattern` core. The core's value-op constructors are
//! `fn add<L: MatchPat, R: MatchPat>(l, r) -> Add<L, R>` etc. — there is
//! no `impl MatchPat for Pattern` and no splice primitive, so a finished
//! `Pattern` cannot be nested into a parent value builder. We bridge with
//! **type-erased shims** entirely inside strider-py:
//!
//! * [`DynMatch`] wraps a `Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>`
//!   and implements [`strider_pattern::MatchPat`].
//! * [`DynTemplate`] wraps a `Box<dyn FnOnce(&mut TemplateBuilder) -> TmplValueRef>`
//!   and implements [`strider_pattern::TemplatePat`].
//! * [`DynMem`] wraps a memory-producing sub-pattern compiler implementing
//!   [`strider_pattern::node_builders::MemPat`].
//!
//! Each Python pattern object produces a `DynMatch` (for matching) or a
//! `DynTemplate` (for a rewrite RHS) by recursing its operands into shims
//! and threading them into the existing typed free functions (`add`,
//! `sub`, `int_cmp`, `truncate`, …) — which already encode the correct
//! `KindSpec` / output-type / lowerings (e.g. `sub` → `add(l, neg(r))`).
//! Sealing happens only at the top level: `.into_pattern()` for `find`,
//! `.into_template()` for a rewrite RHS.
//!
//! String-keyed captures: any free function that accepts a sub-pattern
//! also accepts a string; the string is interned to a `Capture` at the
//! point the outermost pattern is compiled, so back-references
//! (`add("x", "x")`) work. The intern table is global per process.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};
#[allow(unused_imports)]
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use strider_pattern::matcher::{MatcherBuilder, PatValueRef};
use strider_pattern::MemPat;
use strider_pattern::{CaptureExt, MatchPat};
use strider_pattern::template::{TemplateBuilder, TmplValueRef};
use strider_pattern::TemplatePat;
use strider_pattern::{Capture, Pattern, Template};

use crate::errors::into_strider_err;

// ── Capture ──────────────────────────────────────────────────────────────

/// An opaque capture variable that binds a matched node so its value /
/// op-variant / fingerprint can be read back from the `Match`.  Each
/// `Capture()` call produces a globally unique id; pass it to `var(c)`,
/// `any_int_const(c)`, `.capture(c)`, etc.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: Capture,
}

#[pymethods]
impl PyCapture {
    /// Create a fresh, globally-unique capture variable for binding a
    /// matched node.  Retrieve the binding after a match via
    /// `Match[c]` / `Match.uint(c)` / etc.
    #[new]
    fn new() -> Self {
        Self {
            inner: Capture::new(),
        }
    }

    /// `Capture(<id>)`.
    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    /// Hash on the capture's globally-unique id (stable per instance).
    fn __hash__(&self) -> isize {
        self.inner.id() as i64 as isize
    }
}

// ── String → Capture interning ───────────────────────────────────────────
//
// `add("x", "x")` aliases (same string in the same Python process → same
// Capture) and `add("x", "y")` doesn't.  The reserved names "_" and
// "any_" raise StriderError when used as regular capture strings.

fn intern_table() -> &'static Mutex<HashMap<String, Capture>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, Capture>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn intern_str(name: &str) -> PyResult<Capture> {
    if name == "_" || name == "any_" {
        return Err(into_strider_err(anyhow::anyhow!(
            "{name:?} is reserved (use any_() / var() / _ explicitly)"
        )));
    }
    let mut table = intern_table()
        .lock()
        .map_err(|_| into_strider_err(anyhow::anyhow!("intern table lock poisoned")))?;
    Ok(*table.entry(name.to_string()).or_insert_with(Capture::new))
}

// ── Type-erased shims (the bridge to the static-generic core) ────────────

/// A match-side type-erased pattern: lowers onto the imperative
/// [`MatcherBuilder`] via a boxed `FnOnce`. Implements [`MatchPat`] so it
/// can be threaded into the core's typed free functions (`add(dyn_l,
/// dyn_r)`, …).
pub(crate) struct DynMatch(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>);

impl MatchPat for DynMatch {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

/// A build-side type-erased template: lowers onto the imperative
/// [`TemplateBuilder`] via a boxed `FnOnce`. Implements [`TemplatePat`]
/// so it can be threaded into the core's typed free functions on the
/// template side.
pub(crate) struct DynTemplate(pub(crate) Box<dyn FnOnce(&mut TemplateBuilder) -> TmplValueRef>);

impl TemplatePat for DynTemplate {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        (self.0)(b)
    }
}

/// A memory-producing type-erased sub-pattern: lowers onto the
/// [`MatcherBuilder`] returning its memory-token output. Implements
/// [`MemPat`] so it can feed `load().mem_in(...)` / `call().mem(...)`.
pub(crate) struct DynMem(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>);

impl MemPat for DynMem {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

// ── PyPat — the central re-finalisable pattern wrapper ───────────────────

/// The fixed integer-unary kinds we expose by name.
#[derive(Clone, Copy)]
pub(crate) enum IntUnaryKind {
    Neg,
    Popcount,
    Lzcount,
}

/// The fixed float-unary kinds we expose by name.
#[derive(Clone, Copy)]
pub(crate) enum FloatUnaryKind {
    Neg,
    Abs,
    Sqrt,
    Ceil,
    Floor,
    Round,
}

/// The cast / coercion kinds we expose by name.
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

/// The recursive shape of a `PyPat`. Stores operands as data extracted
/// eagerly during construction (captures, consts) plus re-finalisable
/// child references (`Py<PyAny>`), so a fresh `DynMatch` / `DynTemplate`
/// can be re-emitted on each compile — the same `PyPat` can drive many
/// `find_all` / `find_one` / `find_joined` calls.
///
/// `match_only` variants return a `StriderError` from `compile_template`:
/// they have no rewrite-RHS form.
pub(crate) enum PatRepr {
    /// Wildcard `any()` — match-only.
    Any,
    /// `var(c)` — matches any node, binds it to `c`. Buildable.
    Var(Capture),
    /// `value_of_width(n)` — match-only.
    ValueOfWidth(u32),
    /// `inputs_of_width(n, inner)` — match-only.
    InputsOfWidth(u32, Py<PyAny>),
    /// `int_const(v)` (bit-pattern). Buildable.
    IntConst(u128),
    /// `signed_int_const(v)`. Buildable.
    SignedIntConst(i64),
    /// `int_const_any_of([...])` — match-only.
    IntConstAnyOf(Vec<u64>),
    /// `bool_const(b)`. Buildable.
    BoolConst(bool),
    /// `float_const(bits)`. Buildable.
    FloatConst(u64),
    /// `any_int_const(c)` — match-only.
    AnyIntConst(Capture),
    /// `any_bool_const(c)` — match-only.
    AnyBoolConst(Capture),
    /// `any_float_const(c)` — match-only.
    AnyFloatConst(Capture),
    /// `initial_var()` — match-only.
    InitialVar,
    /// `initial_var_for(vn)` — match-only.
    InitialVarFor(rsleigh::Vn),
    /// A fixed integer binary op (`add`, `mul`, …). Buildable.
    IntBinary(strider_ir::IntBinaryOp, Py<PyAny>, Py<PyAny>),
    /// A subtraction `sub(l, r)` → `add(l, neg(r))`. Buildable.
    Sub(Py<PyAny>, Py<PyAny>),
    /// A bitwise complement `bit_not(x)` → `xor(x, all_ones)`. Buildable.
    BitNot(Py<PyAny>),
    /// A fixed integer unary op (`neg`, `popcount`, `lzcount`). Buildable.
    IntUnary(IntUnaryKind, Py<PyAny>),
    /// A cast / coercion op. Buildable.
    Cast(CastKind, Py<PyAny>),
    /// `extend(op, inner)`. Buildable.
    Extend(strider_ir::ExtendOp, Py<PyAny>),
    /// A fixed integer comparison (`int_eq`, `int_lt`, …). Buildable.
    IntCmp(strider_ir::IntCmpOp, Py<PyAny>, Py<PyAny>),
    /// `int_le(l, r)` → `xor(int_lt(r, l), 1)`. Buildable.
    IntLe(Py<PyAny>, Py<PyAny>),
    /// `int_sle(l, r)` → `xor(int_slt(r, l), 1)`. Buildable.
    IntSle(Py<PyAny>, Py<PyAny>),
    /// A fixed float binary op. Buildable.
    FloatBinary(strider_ir::FloatBinaryOp, Py<PyAny>, Py<PyAny>),
    /// `float_sub(l, r)`. Buildable.
    FloatSub(Py<PyAny>, Py<PyAny>),
    /// A fixed float unary op. Buildable.
    FloatUnary(FloatUnaryKind, Py<PyAny>),
    /// A fixed float comparison. Buildable.
    FloatCmp(strider_ir::FloatCmpOp, Py<PyAny>, Py<PyAny>),
    /// `float_ne(l, r)`. Buildable.
    FloatNe(Py<PyAny>, Py<PyAny>),
    /// `float_le(l, r)`. Buildable.
    FloatLe(Py<PyAny>, Py<PyAny>),
    /// `float_is_nan(x)`. Buildable.
    FloatIsNan(Py<PyAny>),
    /// A fixed boolean binary op (`bool_and`, …). Buildable.
    BoolBinary(strider_ir::IntBinaryOp, Py<PyAny>, Py<PyAny>),
    /// `bool_not(x)` → `xor(x, 1)`. Buildable.
    BoolNot(Py<PyAny>),
    /// `int_bin_any(c, l, r)` — match-only.
    IntBinAny(Capture, Py<PyAny>, Py<PyAny>),
    /// `int_un_any(c, x)` — match-only.
    IntUnAny(Capture, Py<PyAny>),
    /// `int_cmp_any(c, l, r)` — match-only.
    IntCmpAny(Capture, Py<PyAny>, Py<PyAny>),
    /// `bool_bin_any(c, l, r)` — match-only.
    BoolBinAny(Capture, Py<PyAny>, Py<PyAny>),
    /// `float_bin_any(c, l, r)` — match-only.
    FloatBinAny(Capture, Py<PyAny>, Py<PyAny>),
    /// `float_un_any(c, x)` — match-only.
    FloatUnAny(Capture, Py<PyAny>),
    /// `float_cmp_any(c, l, r)` — match-only.
    FloatCmpAny(Capture, Py<PyAny>, Py<PyAny>),
    /// `.capture(c)` wrapping an inner pattern.
    Captured(Rc<PatRepr>, Capture),
    /// `.when(f)` wrapping an inner pattern — match-only.
    Guarded(Rc<PatRepr>, Py<PyAny>),
    /// `.of_width(n)` wrapping an inner pattern — constrains the matched
    /// node's value output to `n` bits. Match-only.
    OfWidth(Rc<PatRepr>, u32),
    /// `.value_ty(ty)` wrapping an inner pattern — constrains the matched
    /// node's value output to an exact type. Match-only.
    ValueTy(Rc<PatRepr>, strider_ir::node::ValueType),
    /// A finished control / variadic [`Pattern`] (from a control
    /// builder's `.into_pat()`). One-shot: consumed when the pattern is
    /// queried. Cannot be nested as a value operand (the core exposes no
    /// splice primitive). Match-only.
    ///
    /// Boxed because a `Pattern` is large (~256 bytes); keeping it behind a
    /// `Box` keeps `PatRepr`'s other (small) variants from inheriting that
    /// size.
    Finished(Box<std::cell::RefCell<Option<Pattern>>>),
}

/// Opaque wrapper around a re-finalisable pattern representation.
///
/// `PyPat` holds an `Rc<PatRepr>` so it can be compiled to a fresh
/// `Pattern` / `Template` on each query — the same `PyPat` can drive
/// multiple `find_all` / rewrite calls. Refcounting (`Rc`) is local to
/// strider-py; the strider-pattern core stays single-threaded with no
/// `Arc` / `Rc` of its own.
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

// ── Operand dispatch ─────────────────────────────────────────────────────
//
// A `Bound<PyAny>` operand can be any pattern-like object: a `PyPat`, a
// `PyCapture`, a `str`, or one of the value-producing typed builders.
// `compile_operand_*` downcasts it and produces the right shim eagerly
// during the recursive walk (so the resulting `FnOnce` captures only
// owned data — no `Bound` re-borrow across the GIL).

/// Build a match-side `DynMatch` from any value-producing pattern-like
/// Python object.
pub(crate) fn compile_operand_match(ob: &Bound<'_, PyAny>) -> PyResult<DynMatch> {
    let py = ob.py();
    if let Ok(p) = ob.downcast::<PyPat>() {
        return p.borrow().repr.compile_match(py);
    }
    if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
        let cap = c.inner;
        return Ok(DynMatch(Box::new(move |b| mc(strider_pattern::var(cap), b))));
    }
    if let Ok(s) = ob.downcast::<PyString>() {
        let name = s.to_string();
        if name == "_" || name == "any_" {
            return Ok(DynMatch(Box::new(|b| strider_pattern::any().compile(b))));
        }
        let cap = intern_str(&name)?;
        return Ok(DynMatch(Box::new(move |b| mc(strider_pattern::var(cap), b))));
    }
    // Value-producing typed builders nest directly via MatchPat.
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
    Err(into_strider_err(anyhow::anyhow!(
        "expected a value pattern (Pat / Capture / str / value builder); \
         a control / variadic builder (call / store / ret / if / mem_phi) \
         cannot be nested as a value operand"
    )))
}

/// Build a build-side `DynTemplate` from any pattern-like Python object,
/// or a `StriderError` for match-only operands.
pub(crate) fn compile_operand_template(ob: &Bound<'_, PyAny>) -> PyResult<DynTemplate> {
    let py = ob.py();
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

/// Build a memory-producing `DynMem` from any pattern-like Python object
/// (a `LoadPat` / `StorePat` / `MemPhiPat` / `CallPat` / `CallOtherPat`).
pub(crate) fn compile_operand_mem(ob: &Bound<'_, PyAny>) -> PyResult<DynMem> {
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
    // A memory-input slot requires a memory-token producer. Wiring a
    // value operand (a bare `load()`, a `Pat`, a capture, a string) here
    // would build a pattern that can never match a real IR memory chain
    // (the matcher's `output_ok` rejects a value output against a
    // memory-kind slot), so reject it up front instead of silently
    // building a dead pattern.
    Err(into_strider_err(anyhow::anyhow!(
        "a memory-input slot (`mem_in`) requires a memory producer — \
         store() / mem_phi() / call() / call_other(); got a value operand \
         ({})",
        operand_kind_name(ob)
    )))
}

/// A short human-readable name for an operand's Python type, used in the
/// `mem_in` rejection message.
fn operand_kind_name(ob: &Bound<'_, PyAny>) -> String {
    ob.get_type()
        .name()
        .map_or_else(|_| "value".to_string(), |n| n.to_string())
}

/// Emit a `var(c)`-equivalent capture-only node on the template side.
fn template_var(b: &mut TemplateBuilder, cap: Capture) -> TmplValueRef {
    tc(strider_pattern::var(cap), b)
}

/// Disambiguating match-side compile helper (only `MatchPat::compile` is in
/// scope under the `P: MatchPat` bound, so `.compile` is unambiguous here).
fn mc<P: MatchPat>(p: P, b: &mut MatcherBuilder) -> PatValueRef {
    p.compile(b)
}

/// Disambiguating build-side compile helper.
fn tc<P: TemplatePat>(p: P, b: &mut TemplateBuilder) -> TmplValueRef {
    p.compile(b)
}

fn rhs_error(kind: &str) -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "cannot use {kind} as a rewrite RHS — the RHS must be a buildable \
         value expression"
    ))
}

/// Parse a `ValueType` from its (case-insensitive) name, e.g.
/// `"i1"` / `"I64"` / `"f32"`. Used by `Pat.value_ty(...)`.
fn parse_value_ty(name: &str) -> PyResult<strider_ir::node::ValueType> {
    use strider_ir::node::ValueType as T;
    let ty = match name.to_ascii_lowercase().as_str() {
        "i1" => T::I1,
        "i8" => T::I8,
        "i16" => T::I16,
        "i32" => T::I32,
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
                 i32, i64, i80, i128, i256, i512, f32, f64, f80"
            )));
        }
    };
    Ok(ty)
}

impl PatRepr {
    /// Build a fresh match-side `DynMatch` for this representation.
    pub(crate) fn compile_match(&self, py: Python<'_>) -> PyResult<DynMatch> {
        compile_repr_match(self, py)
    }

    /// Build a fresh build-side `DynTemplate`, or a `StriderError` if this
    /// representation is match-only.
    pub(crate) fn compile_template(&self, py: Python<'_>) -> PyResult<DynTemplate> {
        compile_repr_template(self, py)
    }
}

// ── Native-recursion depth guard ─────────────────────────────────────────
//
// `compile_repr_match` / `compile_repr_template` are native (Rust-stack)
// recursion that mirrors the nesting depth of the Python pattern tree. A
// pathologically deep pattern (`add(add(add(…)))` thousands deep) would
// overflow the Rust stack and abort the process — a worse failure mode
// than a clean exception. CPython's own recursion limit usually caps
// pattern *construction* long before this, so this is belt-and-suspenders
// that converts an abort into a clean `StriderError`. The bound is checked
// via a thread-local depth counter incremented by an RAII guard at each
// recursion entry, so it covers both the direct recursion (`Captured` /
// `Guarded`) and the indirect recursion through nested `Pat` operands.

/// Generous nesting bound; well above any realistic hand-written pattern.
const MAX_PATTERN_NESTING: u32 = 512;

thread_local! {
    static COMPILE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII guard that bumps the thread-local compile depth on construction and
/// restores it on drop. Construction fails past [`MAX_PATTERN_NESTING`].
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

// ── PatRepr recursion (match side) ───────────────────────────────────────
//
// Each arm recurses its operands into `DynMatch` shims (eagerly, while the
// GIL + the operand `Bound`s are held) and threads them into the core's
// typed free function, which encodes the correct KindSpec / output type /
// lowering. The resulting `DynMatch` closure owns only the child shims, so
// it survives past the `Bound` borrows.

fn op_match(py: Python<'_>, ob: &Py<PyAny>) -> PyResult<DynMatch> {
    compile_operand_match(ob.bind(py))
}

#[allow(clippy::too_many_lines)]
fn compile_repr_match(repr: &PatRepr, py: Python<'_>) -> PyResult<DynMatch> {
    use strider_pattern as sp;
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
            DynMatch(Box::new(move |b| mc(sp::any_int_const().capture(c), b)))
        }
        PatRepr::AnyBoolConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| mc(sp::any_bool_const().capture(c), b)))
        }
        PatRepr::AnyFloatConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| mc(sp::any_float_const().capture(c), b)))
        }
        PatRepr::InitialVar => DynMatch(Box::new(|b| mc(sp::initial_var(), b))),
        PatRepr::InitialVarFor(vn) => {
            let vn = *vn;
            DynMatch(Box::new(move |b| mc(sp::initial_var_for(vn), b)))
        }
        PatRepr::IntBinary(op, l, r) => {
            let op = *op;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_binary(op, l, r), b)))
        }
        PatRepr::Sub(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::sub(l, r), b)))
        }
        PatRepr::BitNot(x) => {
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::bit_not(x), b)))
        }
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
        PatRepr::Extend(op, x) => {
            let op = *op;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::extend(op, x), b)))
        }
        PatRepr::IntCmp(op, l, r) => {
            let op = *op;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_cmp(op, l, r), b)))
        }
        PatRepr::IntLe(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_le(l, r), b)))
        }
        PatRepr::IntSle(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_sle(l, r), b)))
        }
        PatRepr::FloatBinary(op, l, r) => {
            let op = *op;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_binary(op, l, r), b)))
        }
        PatRepr::FloatSub(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_sub(l, r), b)))
        }
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
        PatRepr::FloatCmp(op, l, r) => {
            let op = *op;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_cmp(op, l, r), b)))
        }
        PatRepr::FloatNe(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_ne(l, r), b)))
        }
        PatRepr::FloatLe(l, r) => {
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_le(l, r), b)))
        }
        PatRepr::FloatIsNan(x) => {
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::float_is_nan(x), b)))
        }
        PatRepr::BoolBinary(op, l, r) => {
            let op = *op;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::bool_binary(op, l, r), b)))
        }
        PatRepr::BoolNot(x) => {
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::bool_not(x), b)))
        }
        PatRepr::IntBinAny(c, l, r) => {
            let c = *c;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_binary_any(l, r).capture(c), b)))
        }
        PatRepr::IntUnAny(c, x) => {
            let c = *c;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::int_unary_any(x).capture(c), b)))
        }
        PatRepr::IntCmpAny(c, l, r) => {
            let c = *c;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::int_cmp_any(l, r).capture(c), b)))
        }
        PatRepr::BoolBinAny(c, l, r) => {
            let c = *c;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::bool_bin_any(l, r).capture(c), b)))
        }
        PatRepr::FloatBinAny(c, l, r) => {
            let c = *c;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_binary_any(l, r).capture(c), b)))
        }
        PatRepr::FloatUnAny(c, x) => {
            let c = *c;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| mc(sp::float_unary_any(x).capture(c), b)))
        }
        PatRepr::FloatCmpAny(c, l, r) => {
            let c = *c;
            let l = op_match(py, l)?;
            let r = op_match(py, r)?;
            DynMatch(Box::new(move |b| mc(sp::float_cmp_any(l, r).capture(c), b)))
        }
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
    use strider_pattern as sp;
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

// ── PatRepr recursion (template / build side) ────────────────────────────

fn op_tpl(py: Python<'_>, ob: &Py<PyAny>) -> PyResult<DynTemplate> {
    compile_operand_template(ob.bind(py))
}

#[allow(clippy::too_many_lines)]
fn compile_repr_template(repr: &PatRepr, py: Python<'_>) -> PyResult<DynTemplate> {
    use strider_pattern as sp;
    // Composite RHS ops build through the TemplatePat-bounded `template::`
    // twins (a `DynTemplate` operand is `TemplatePat`, not `MatchPat`, so it
    // can't feed the bare match-side factories). Leaves (`int_const`,
    // `var`, …) stay on the dual-trait bare builders.
    use strider_pattern::template as tpl;
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
        PatRepr::IntBinary(op, l, r) => {
            let op = *op;
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::int_binary(op, l, r), b)))
        }
        PatRepr::Sub(l, r) => {
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::sub(l, r), b)))
        }
        PatRepr::BitNot(x) => {
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| tc(tpl::bit_not(x), b)))
        }
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
        PatRepr::Extend(op, x) => {
            let op = *op;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| tc(tpl::extend(op, x), b)))
        }
        PatRepr::IntCmp(op, l, r) => {
            let op = *op;
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::int_cmp(op, l, r), b)))
        }
        PatRepr::FloatBinary(op, l, r) => {
            let op = *op;
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::float_binary(op, l, r), b)))
        }
        PatRepr::FloatSub(l, r) => {
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::float_sub(l, r), b)))
        }
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
        PatRepr::FloatCmp(op, l, r) => {
            let op = *op;
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::float_cmp(op, l, r), b)))
        }
        PatRepr::BoolBinary(op, l, r) => {
            let op = *op;
            let l = op_tpl(py, l)?;
            let r = op_tpl(py, r)?;
            DynTemplate(Box::new(move |b| tc(tpl::bool_binary(op, l, r), b)))
        }
        PatRepr::BoolNot(x) => {
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| tc(tpl::bool_not(x), b)))
        }
        PatRepr::Captured(_inner, c) => {
            // On a template RHS a capture resolves to the matched LHS
            // value, *replacing* whatever it wrapped — so `inner` is not
            // built; a capture is a fresh leaf.
            let c = *c;
            DynTemplate(Box::new(move |b| b.capture(c)))
        }
        // Match-only kinds: no buildable RHS form.
        PatRepr::Any => return Err(rhs_error("any")),
        PatRepr::ValueOfWidth(_) => return Err(rhs_error("value_of_width")),
        PatRepr::InputsOfWidth(..) => return Err(rhs_error("inputs_of_width")),
        PatRepr::IntConstAnyOf(_) => return Err(rhs_error("int_const_any_of")),
        PatRepr::AnyIntConst(_) => return Err(rhs_error("any_int_const")),
        PatRepr::AnyBoolConst(_) => return Err(rhs_error("any_bool_const")),
        PatRepr::AnyFloatConst(_) => return Err(rhs_error("any_float_const")),
        PatRepr::InitialVar => return Err(rhs_error("initial_var")),
        PatRepr::InitialVarFor(_) => return Err(rhs_error("initial_var_for")),
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
    use strider_pattern::template as tpl;
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

// ── Top-level sealing: PatRepr → Pattern / Template ──────────────────────

impl PatRepr {
    /// Seal this representation into a finished match [`Pattern`].
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

    /// Seal this representation into a finished build [`Template`], or a
    /// `StriderError` if it is match-only.
    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<Template> {
        Ok(self.compile_template(py)?.into_template())
    }
}

// ── PatLike — the polymorphic boundary input ─────────────────────────────

/// Polymorphic input for builder field methods and `Graph.find_all`.
/// Accepts a `Pat`, a `Capture`, a string (which interns to a Capture),
/// or any of the typed builders that finalise to a pattern.
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
}

// Manual `PyStubType` impl so `pyo3-stub-gen`'s proc-macros translate
// `PatLike` parameters to the canonical `PatLike` Python type alias.
impl pyo3_stub_gen::PyStubType for PatLike<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        pyo3_stub_gen::TypeInfo::with_module("strider.pattern.PatLike", "strider.pattern".into())
    }
}

impl PatLike<'_> {
    /// Seal into a finished match [`Pattern`].
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
        }
    }

    /// Seal into a finished build [`Template`], or a `StriderError` for a
    /// match-only input.
    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<Template> {
        match self {
            PatLike::Pat(p) => p.borrow().repr.to_template(py),
            PatLike::Capture(c) => {
                Ok(DynTemplate(Box::new({
                    let cap = c.borrow().inner;
                    move |b| template_var(b, cap)
                }))
                .into_template())
            }
            PatLike::Str(s) => {
                let name = s.to_string();
                if name == "_" || name == "any_" {
                    Err(rhs_error("any_"))
                } else {
                    let cap = intern_str(&name)?;
                    Ok(DynTemplate(Box::new(move |b| template_var(b, cap))).into_template())
                }
            }
            // Every control / variadic builder is match-only on the RHS.
            _ => Err(rhs_error("control / variadic builder")),
        }
    }
}

// ── Pending control-flow exception (KeyboardInterrupt / SystemExit) ──────
//
// When a `.when()` predicate raises a control-flow exception, we stash it
// in a thread-local cell so the matcher can finish its walk without
// CPython tripping its "returned a result with an exception set" guard;
// the outer find boundary drains the cell and surfaces the PyErr.

thread_local! {
    static PENDING_CONTROL_FLOW: std::cell::Cell<Option<PyErr>> =
        const { std::cell::Cell::new(None) };
}

/// Drain the thread-local pending-control-flow slot, if any.
pub(crate) fn take_pending_control_flow() -> Option<PyErr> {
    PENDING_CONTROL_FLOW.with(std::cell::Cell::take)
}

/// Peek at the pending-control-flow cell without draining it.
pub(crate) fn peek_pending_control_flow() -> bool {
    PENDING_CONTROL_FLOW.with(|cell| {
        let t = cell.take();
        let pending = t.is_some();
        cell.set(t);
        pending
    })
}

/// Stash a control-flow PyErr in the pending cell.
pub(crate) fn stash_pending_control_flow(e: PyErr) {
    PENDING_CONTROL_FLOW.with(|cell| cell.set(Some(e)));
}

// ── PyPartialMatch — proxy passed to .when predicates ────────────────────

/// Transient read-only view of the captures bound so far, passed to a
/// `.when(...)` / `predicate(...)` Python callback.
#[pyclass(name = "PartialMatch", module = "strider.pattern", unsendable)]
pub struct PyPartialMatch {
    bindings: strider_pattern::Bindings,
    function_ptr: Mutex<Option<*const strider_ir::Function>>,
}

impl PyPartialMatch {
    fn new(bindings: strider_pattern::Bindings, function: &strider_ir::Function) -> Self {
        Self {
            bindings,
            function_ptr: Mutex::new(Some(function as *const _)),
        }
    }

    fn clear_graph_ptr(&self) {
        let mut g = self
            .function_ptr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = None;
    }

    fn with_function<R>(&self, f: impl FnOnce(&strider_ir::Function) -> R) -> Option<R> {
        let guard = self
            .function_ptr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ptr = (*guard)?;
        // SAFETY: `ptr` was set to a valid `&Function` by the matcher and
        // only cleared after the predicate returns; the Mutex guard
        // prevents the cleanup from racing this call.
        let function_ref = unsafe { &*ptr };
        Some(f(function_ref))
    }

    fn capture_from_key(&self, key: &CaptureKeyOwned) -> PyResult<Capture> {
        match key {
            CaptureKeyOwned::Capture(c) => Ok(*c),
            CaptureKeyOwned::Str(s) => intern_str(s.as_str()),
        }
    }
}

/// Owned variant of a capture key (no `Bound` lifetime).
enum CaptureKeyOwned {
    Capture(Capture),
    Str(String),
}

impl<'py> FromPyObject<'py> for CaptureKeyOwned {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
            return Ok(CaptureKeyOwned::Capture(c.inner));
        }
        if let Ok(s) = ob.extract::<String>() {
            return Ok(CaptureKeyOwned::Str(s));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected Capture or str",
        ))
    }
}

#[pymethods]
impl PyPartialMatch {
    /// The capture's value as an unsigned `int`, or `None`.
    fn uint(&self, key: CaptureKeyOwned) -> PyResult<Option<u128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_function(|f| self.bindings.get_uint(cap, f))
            .flatten())
    }

    /// The capture's value as a signed `int`, or `None`.
    #[pyo3(name = "int")]
    fn int_(&self, key: CaptureKeyOwned) -> PyResult<Option<i128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_function(|f| self.bindings.get_int(cap, f))
            .flatten())
    }

    /// The capture's value as a `bool`, or `None`.
    #[pyo3(name = "bool")]
    fn bool_(&self, key: CaptureKeyOwned) -> PyResult<Option<bool>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_function(|f| self.bindings.get_bool(cap, f))
            .flatten())
    }

    /// The capture's value as raw float bits (`u64`), or `None`.
    fn float_bits(&self, key: CaptureKeyOwned) -> PyResult<Option<u64>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_function(|f| self.bindings.get_float_bits(cap, f.graph()))
            .flatten())
    }

    /// True if the capture has a binding so far in this partial match.
    fn has(&self, key: CaptureKeyOwned) -> PyResult<bool> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.bindings.is_bound(cap))
    }

    /// Look up a capture by key (Python `m[c]`).
    fn __getitem__(&self, py: Python<'_>, key: CaptureKeyOwned) -> PyResult<PyObject> {
        let cap = self.capture_from_key(&key)?;
        if let Some(Some(v)) = self.with_function(|f| self.bindings.get_uint(cap, f)) {
            return Ok(v.into_py(py));
        }
        if let Some(Some(b)) = self.with_function(|f| self.bindings.get_bool(cap, f)) {
            return Ok(b.into_py(py));
        }
        if let Some(Some(fl)) = self.with_function(|f| self.bindings.get_float_bits(cap, f.graph())) {
            return Ok(fl.into_py(py));
        }
        Ok(py.None())
    }

    /// Whether `c` is bound in this partial match (Python `c in m`).
    fn __contains__(&self, key: CaptureKeyOwned) -> PyResult<bool> {
        self.has(key)
    }
}

/// Build a `when_match` closure that calls a Python predicate with a
/// transient `PyPartialMatch` proxy. Control-flow exceptions
/// (`KeyboardInterrupt` / `SystemExit`) are stashed for the outer
/// boundary to re-raise; ordinary predicate exceptions are surfaced to
/// stderr and treated as no-match.
/// Invoke a Python `.when()` predicate against the current match
/// bindings, returning whether the match should be kept. Control-flow
/// exceptions (`KeyboardInterrupt` / `SystemExit`) are stashed for the
/// outer boundary to re-raise; ordinary predicate exceptions are
/// surfaced to stderr and treated as no-match.
///
/// Shared by both the `MatchPat`-level [`wrap_when`] (value-rooted
/// builders) and the finished-`Pattern` root guard [`make_root_post_match`]
/// (control / variadic builders) so the two paths behave identically.
fn run_when_predicate(
    matcher: &strider_pattern::Matcher,
    bindings: &strider_pattern::Bindings,
    py_func: &PyObject,
) -> bool {
    Python::with_gil(|py| {
        if peek_pending_control_flow() {
            return false;
        }
        let proxy = PyPartialMatch::new(bindings.clone(), matcher.function());
        let py_proxy = match Py::new(py, proxy) {
            Ok(p) => p,
            Err(e) => {
                stash_pending_control_flow(e);
                return false;
            }
        };
        let args = PyTuple::new_bound(py, [py_proxy.clone_ref(py)]);
        let result = py_func.call_bound(py, args, None);
        if let Ok(proxy_ref) = py_proxy.try_borrow(py) {
            proxy_ref.clear_graph_ptr();
        }
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

pub(crate) fn wrap_when<P: MatchPat + 'static>(inner: P, py_func: PyObject) -> impl MatchPat {
    inner.when_match(move |matcher, _ty, bindings| {
        run_when_predicate(matcher, bindings, &py_func)
    })
}

/// Build a finished-`Pattern` root [`PostMatchFn`] from a Python `.when()`
/// predicate. Used by the node-rooted control / variadic builders
/// (`call` / `store` / `ret` / `if` / `call_other` / `phi` / `mem_phi` /
/// `function_arg`), which finalise straight to a `Pattern` and so have no
/// `MatchPat` form for [`wrap_when`] to wrap.
pub(crate) fn make_root_post_match(py_func: PyObject) -> strider_pattern::PostMatchFn {
    Box::new(move |matcher, _node, _ty, bindings| {
        run_when_predicate(matcher, bindings, &py_func)
    })
}

/// If `common` carries a `.when()` predicate, attach it as a root
/// post-match guard on `pat`. Otherwise return `pat` unchanged. This is
/// how the node-rooted control / variadic builders honour `.when()`.
fn apply_when_to_pattern(py: Python<'_>, common: &CommonState, pat: Pattern) -> Pattern {
    match common.when.as_ref() {
        Some(f) => pat.with_root_post_match(make_root_post_match(f.clone_ref(py))),
        None => pat,
    }
}

// ── CastMask ─────────────────────────────────────────────────────────────

/// `CastMask` — bitset selecting which value-passthrough cast
/// `NodeKind`s the matcher walks through transparently. Construct via the
/// classmethods; combine with `|`. Pass to
/// `Graph.find_all(pat, ignore_casts_mask=...)`.
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
                Self { inner: strider_pattern::CastMask::$value }
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
                Self { inner: strider_pattern::CastMask::$value() }
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
    /// Alias for `none()` — mirrors Rust's `CastMask::empty()`.
    #[classmethod]
    fn empty(cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self::none(cls)
    }
    /// Union of two masks (`a | b`).
    fn __or__(&self, other: &Self) -> Self {
        Self {
            inner: self.inner | other.inner,
        }
    }
    /// Intersection of two masks (`a & b`).
    fn __and__(&self, other: &Self) -> Self {
        Self {
            inner: self.inner & other.inner,
        }
    }
    /// Equality on the underlying bitset.
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
    /// Hash on the underlying bits.
    fn __hash__(&self) -> u64 {
        u64::from(self.inner.bits())
    }
    /// The raw bitset value as a `u32`.
    fn bits(&self) -> u32 {
        self.inner.bits()
    }
    /// `CastMask(0b........)` showing the raw bits.
    fn __repr__(&self) -> String {
        format!("CastMask(0b{:08b})", self.inner.bits())
    }
}

// ── PyPat methods ────────────────────────────────────────────────────────

#[pymethods]
impl PyPat {
    /// Capture this pattern's matched node under `c`. Returns a new `Pat`.
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        PyPat::from_repr(PatRepr::Captured(Rc::clone(&self.repr), c.inner))
    }

    /// Capture this pattern under a string name (auto-interned).
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        let c = intern_str(name)?;
        Ok(PyPat::from_repr(PatRepr::Captured(Rc::clone(&self.repr), c)))
    }

    /// Attach a Python predicate that runs after this pattern matches.
    /// Returning `False` (or raising) fails the match.
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_repr(PatRepr::Guarded(Rc::clone(&self.repr), f))
    }

    /// Constrain the matched node's value output to exactly `n` bits.
    /// Returns a new `Pat`. Match-only.
    fn of_width(&self, n: u32) -> PyPat {
        PyPat::from_repr(PatRepr::OfWidth(Rc::clone(&self.repr), n))
    }

    /// Constrain the matched node's value output to the exact type named
    /// by `ty` (e.g. `"i1"`, `"i64"`, `"f32"`). Returns a new `Pat`.
    /// Match-only.
    fn value_ty(&self, ty: &str) -> PyResult<PyPat> {
        let t = parse_value_ty(ty)?;
        Ok(PyPat::from_repr(PatRepr::ValueTy(Rc::clone(&self.repr), t)))
    }

    /// Constrain the matched node's value output to a boolean (1-bit
    /// `I1`). Sugar for `.of_width(1)`. Match-only.
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

    /// Opaque `Pat(...)` repr.
    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

// ── Free constructors ────────────────────────────────────────────────────

/// Wildcard: matches any node without binding it.
#[pyfunction]
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

/// Match any boolean value — any 1-bit (`I1`) value output.
#[pyfunction]
pub fn bool_value() -> PyPat {
    PyPat::from_repr(PatRepr::ValueOfWidth(1))
}

/// Match `inner` and require all value inputs to be `n` bits wide.
#[pyfunction]
pub fn inputs_of_width(n: u32, inner: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::InputsOfWidth(n, inner))
}

/// Match `inner` whose value inputs are all booleans (1-bit `I1`).
#[pyfunction]
pub fn bool_inputs(inner: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::InputsOfWidth(1, inner))
}

/// Match an `IntConst` whose stored value (masked to output width) equals
/// `value` (bit-pattern equality; negative values use the sign-extended
/// form). Use `signed_int_const` for the cross-width signed form.
#[pyfunction]
pub fn int_const(value: i128) -> PyPat {
    PyPat::from_repr(PatRepr::IntConst(value as u128))
}

/// Match a signed `IntConst` across width encodings (exact / sign- /
/// zero-extended-narrow). More permissive than `int_const`.
///
/// The core signed matcher carries an `i64`; a `value` outside the `i64` range
/// is rejected with `StriderError` rather than silently truncated.
#[pyfunction]
pub fn signed_int_const(value: i128) -> PyResult<PyPat> {
    let v = i64::try_from(value).map_err(|_| {
        into_strider_err(anyhow::anyhow!(
            "signed_int_const value {value} does not fit in i64 (the core signed-const width)"
        ))
    })?;
    Ok(PyPat::from_repr(PatRepr::SignedIntConst(v)))
}

/// Match an `IntConst` whose value is any in `values` (masked). An empty
/// list vacuously fails. Match-only.
///
/// The core carries each candidate as a `u64`; an element outside the `u64`
/// range is rejected with `StriderError` rather than silently truncated.
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

/// Match any `IntConst` and bind its value to `c`.
#[pyfunction]
pub fn any_int_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyIntConst(c.inner))
}

/// Match any `I1` boolean constant and bind it to `c`.
#[pyfunction]
pub fn any_bool_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyBoolConst(c.inner))
}

/// Match any `FloatConst` and bind it to `c`.
#[pyfunction]
pub fn any_float_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloatConst(c.inner))
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

/// Match any node, subject to a Python predicate. Shorthand for
/// `any_().when(f)`.
#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    PyPat::from_repr(PatRepr::Guarded(Rc::new(PatRepr::Any), f))
}

// ── Op-name parsing helpers ──────────────────────────────────────────────

fn lookup_op<Op: Copy>(table: &[(&str, Op)], name: &str, op_kind: &str) -> PyResult<Op> {
    if let Some(&(_, op)) = table.iter().find(|(n, _)| *n == name) {
        return Ok(op);
    }
    let lowered = name.to_ascii_lowercase();
    if let Some(&(_, op)) = table.iter().find(|(n, _)| n.eq_ignore_ascii_case(&lowered)) {
        return Ok(op);
    }
    Err(into_strider_err(anyhow::anyhow!(
        "unknown {op_kind} variant {name:?}"
    )))
}

fn parse_int_cmp_op(name: &str) -> PyResult<strider_ir::IntCmpOp> {
    use strider_ir::IntCmpOp::*;
    static TABLE: &[(&str, strider_ir::IntCmpOp)] = &[
        ("Equal", Equal),
        ("Less", Less),
        ("Sless", Sless),
        ("Carry", Carry),
        ("Scarry", Scarry),
        ("Sborrow", Sborrow),
        ("eq", Equal),
        ("lt", Less),
        ("slt", Sless),
    ];
    lookup_op(TABLE, name, "IntCmpOp")
}

fn parse_int_binary_op(name: &str) -> PyResult<strider_ir::IntBinaryOp> {
    use strider_ir::IntBinaryOp::*;
    static TABLE: &[(&str, strider_ir::IntBinaryOp)] = &[
        ("Add", Add),
        ("Mul", Mul),
        ("Div", Div),
        ("Sdiv", Sdiv),
        ("Rem", Rem),
        ("Srem", Srem),
        ("And", And),
        ("Or", Or),
        ("Xor", Xor),
        ("ShiftLeft", ShiftLeft),
        ("ShiftRight", ShiftRight),
        ("SShiftRight", SShiftRight),
        ("shl", ShiftLeft),
        ("shr", ShiftRight),
        ("sshr", SShiftRight),
    ];
    lookup_op(TABLE, name, "IntBinaryOp")
}

fn parse_bool_binary_op(name: &str) -> PyResult<strider_ir::IntBinaryOp> {
    use strider_ir::IntBinaryOp::*;
    static TABLE: &[(&str, strider_ir::IntBinaryOp)] = &[("And", And), ("Or", Or), ("Xor", Xor)];
    lookup_op(TABLE, name, "boolean binary op")
}

fn parse_float_binary_op(name: &str) -> PyResult<strider_ir::FloatBinaryOp> {
    use strider_ir::FloatBinaryOp::*;
    static TABLE: &[(&str, strider_ir::FloatBinaryOp)] =
        &[("Add", Add), ("Mul", Mul), ("Div", Div)];
    lookup_op(TABLE, name, "FloatBinaryOp")
}

// ── Integer binary ops ───────────────────────────────────────────────────

macro_rules! int_binop {
    ($name:ident, $py:literal, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py)]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::IntBinary(strider_ir::IntBinaryOp::$variant, l, r))
        }
    };
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::IntBinary(strider_ir::IntBinaryOp::$variant, l, r))
        }
    };
}

int_binop!(add, Add, "Pattern: `IntBinaryOp::Add` (`a + b`). Commutative.");
int_binop!(mul, Mul, "Pattern: `IntBinaryOp::Mul` (`a * b`). Commutative.");
int_binop!(div, Div, "Pattern: `IntBinaryOp::Div` (unsigned `a / b`).");
int_binop!(sdiv, Sdiv, "Pattern: `IntBinaryOp::Sdiv` (signed `a / b`).");
int_binop!(rem, Rem, "Pattern: `IntBinaryOp::Rem` (unsigned `a % b`).");
int_binop!(srem, Srem, "Pattern: `IntBinaryOp::Srem` (signed `a % b`).");
int_binop!(shl, ShiftLeft, "Pattern: `IntBinaryOp::ShiftLeft` (`a << b`).");
int_binop!(shr, ShiftRight, "Pattern: `IntBinaryOp::ShiftRight` (`a >> b`).");
int_binop!(sshr, SShiftRight, "Pattern: `IntBinaryOp::SShiftRight` (arithmetic `a >> b`).");
int_binop!(and_, "and_", And, "Pattern: `IntBinaryOp::And` (`a & b`). Commutative.");
int_binop!(or_, "or_", Or, "Pattern: `IntBinaryOp::Or` (`a | b`). Commutative.");
int_binop!(xor, Xor, "Pattern: `IntBinaryOp::Xor` (`a ^ b`). Commutative.");

/// Pattern: integer subtraction `a - b` (lifter-canonical `Add(a, Neg(b))`).
#[pyfunction]
pub fn sub(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::Sub(l, r))
}

// ── Integer comparisons ──────────────────────────────────────────────────

macro_rules! int_cmpop {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::IntCmp(strider_ir::IntCmpOp::$variant, l, r))
        }
    };
}

int_cmpop!(int_eq, Equal, "Pattern: `IntCmpOp::Equal` (`a == b`). Commutative.");
int_cmpop!(int_lt, Less, "Pattern: `IntCmpOp::Less` (unsigned `a < b`).");
int_cmpop!(int_slt, Sless, "Pattern: `IntCmpOp::Sless` (signed `a < b`).");
int_cmpop!(int_carry, Carry, "Pattern: `IntCmpOp::Carry` (unsigned add carry-out). Commutative.");
int_cmpop!(int_scarry, Scarry, "Pattern: `IntCmpOp::Scarry` (signed add overflow). Commutative.");
int_cmpop!(int_sborrow, Sborrow, "Pattern: `IntCmpOp::Sborrow` (signed subtract overflow).");

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

/// Match a specific `IntCmpOp` variant by name. Returns a finalised `Pat`.
#[pyfunction]
pub fn int_cmp(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<PyPat> {
    let cmp_op = parse_int_cmp_op(op)?;
    Ok(PyPat::from_repr(PatRepr::IntCmp(cmp_op, l, r)))
}

// ── Integer unary ops ────────────────────────────────────────────────────

/// Pattern: `IntUnaryOp::Neg` — two's-complement negation (`-x`).
#[pyfunction]
pub fn neg(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Neg, operand))
}

/// Pattern: bitwise complement (`~x`) — `Xor(x, all_ones)`.
#[pyfunction]
pub fn bit_not(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BitNot(operand))
}

/// Pattern: bitwise complement (`~x`). Alias for `bit_not`.
#[pyfunction(name = "not_")]
pub fn not_(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BitNot(operand))
}

/// Pattern: `Popcount` — count of set bits.
#[pyfunction]
pub fn popcount(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Popcount, operand))
}

/// Pattern: `Lzcount` — count of leading zero bits.
#[pyfunction]
pub fn lzcount(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntUnary(IntUnaryKind::Lzcount, operand))
}

// ── Bool ops ─────────────────────────────────────────────────────────────

macro_rules! bool_binop {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::BoolBinary(strider_ir::IntBinaryOp::$variant, l, r))
        }
    };
}

bool_binop!(bool_and, And, "Pattern: boolean `a && b` (`IntBinaryOp::And` at `I1`). Commutative.");
bool_binop!(bool_or, Or, "Pattern: boolean `a || b` (`IntBinaryOp::Or` at `I1`). Commutative.");
bool_binop!(bool_xor, Xor, "Pattern: boolean `a ^ b` (`IntBinaryOp::Xor` at `I1`). Commutative.");

/// Pattern: boolean negation (`!x`) — `Xor(x, IntConst(1)):I1`.
#[pyfunction]
pub fn bool_not(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::BoolNot(operand))
}

// ── Float binary / unary / cmp ───────────────────────────────────────────

macro_rules! float_binop {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::FloatBinary(strider_ir::FloatBinaryOp::$variant, l, r))
        }
    };
}

float_binop!(float_add, Add, "Pattern: `FloatBinaryOp::Add` (`a + b`). Commutative.");
float_binop!(float_mul, Mul, "Pattern: `FloatBinaryOp::Mul` (`a * b`). Commutative.");
float_binop!(float_div, Div, "Pattern: `FloatBinaryOp::Div` (`a / b`).");

/// Pattern: float subtraction `a - b` (lifter-canonical `FloatAdd(a, Neg(b))`).
#[pyfunction]
pub fn float_sub(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatSub(l, r))
}

macro_rules! float_unop {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::FloatUnary(FloatUnaryKind::$variant, operand))
        }
    };
}

float_unop!(float_neg, Neg, "Pattern: `FloatUnaryOp::Neg` (`-x`).");
float_unop!(float_abs, Abs, "Pattern: `FloatUnaryOp::Abs` (`fabs(x)`).");
float_unop!(float_sqrt, Sqrt, "Pattern: `FloatUnaryOp::Sqrt` (`sqrt(x)`).");
float_unop!(float_ceil, Ceil, "Pattern: `FloatUnaryOp::Ceil` (`ceil(x)`).");
float_unop!(float_floor, Floor, "Pattern: `FloatUnaryOp::Floor` (`floor(x)`).");
float_unop!(float_round, Round, "Pattern: `FloatUnaryOp::Round` (round-to-nearest-even).");

/// Pattern: `x` is NaN — IEEE 754 self-inequality `Xor(FloatEqual(x, x), 1)`.
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

// ── Conversions / casts ──────────────────────────────────────────────────

macro_rules! cast_fn {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> PyPat {
            PyPat::from_repr(PatRepr::Cast(CastKind::$variant, operand))
        }
    };
}

cast_fn!(int_to_float, IntToFloat, "Pattern: `IntToFloat` — int→float conversion.");
cast_fn!(float_to_int, FloatToInt, "Pattern: `FloatToInt` — float→int conversion.");
cast_fn!(float_to_float, FloatToFloat, "Pattern: `FloatToFloat` — float→float re-width.");
cast_fn!(int_bits_to_float, IntBitsToFloat, "Pattern: `IntBitsToFloat` — reinterpret int bits.");
cast_fn!(float_bits_to_int, FloatBitsToInt, "Pattern: `FloatBitsToInt` — reinterpret float bits.");
cast_fn!(truncate, Truncate, "Pattern: `Truncate` — narrow an integer.");
cast_fn!(zero_extend, ZeroExtend, "Pattern: `Extend(ZeroExtend)`.");
cast_fn!(sign_extend, SignExtend, "Pattern: `Extend(SignExtend)`.");

/// `extend(op, operand)` where `op` is "zero" / "zero_extend" / "sign" /
/// "sign_extend".
#[pyfunction]
pub fn extend(op: &str, operand: Py<PyAny>) -> PyResult<PyPat> {
    let extend_op = match op {
        "zero" | "zero_extend" | "ZeroExtend" => strider_ir::ExtendOp::ZeroExtend,
        "sign" | "sign_extend" | "SignExtend" => strider_ir::ExtendOp::SignExtend,
        other => {
            return Err(into_strider_err(anyhow::anyhow!(
                "unknown extend op {other:?} (expected 'zero' or 'sign')"
            )))
        }
    };
    Ok(PyPat::from_repr(PatRepr::Extend(extend_op, operand)))
}

// ── Variant-agnostic constructors ────────────────────────────────────────

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

// ── Typed builders (hand-written) ────────────────────────────────────────
//
// Each typed builder accumulates field state in a `RefCell<*Inner>` and
// replays it onto the matching `strider_pattern` core builder at finalise
// time. Pat-operand fields are stored as re-finalisable `Py<PyAny>`
// references (compiled to a fresh `DynMatch` / `DynMem` per build), so the
// same builder can drive multiple queries.
//
// `.capture(c)` / `.cap(name)` / `.when(f)` return the same builder (so
// further chaining stays typed); `.into_pat()` (or passing the builder
// directly as a `PatLike`) seals it into a `Pat`. The universal `when`
// predicate is wired uniformly: value-rooted builders wrap it via
// `wrap_when` at the `MatchPat` level, and node-rooted control builders
// attach it as a root post-match guard on the finished `Pattern` via
// `apply_when_to_pattern` (using the core's `with_root_post_match`).

/// Clone an optional `Py<PyAny>` operand for a fresh compile, leaving the
/// original behind so the builder stays reusable.
fn clone_opt(py: Python<'_>, slot: &Option<Py<PyAny>>) -> Option<Py<PyAny>> {
    slot.as_ref().map(|p| p.clone_ref(py))
}

/// The shared capture / when state every builder carries.
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
            /// Capture under a string name (auto-interned).
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

// ── node_builder! — the shared node-pattern builder skeleton ─────────────
//
// Every node-rooted Python pattern builder is the same skeleton over a
// different operand field-set: a `#[pyclass]` wrapping an `*Inner` of
// `Option<…>` / `Vec<…>` operand slots plus a shared `CommonState`, a
// `core_builder` that applies the set fields + `common.capture` onto the
// core `strider_pattern::*Pat`, a root-kind compiler, a `build_pattern_py`,
// one-line `#[pymethods]` slot setters, the `builder_common_methods!`
// block, and the `.into_pat()` / `.when()` plumbing. `node_builder!`
// generates all of that from a compact spec so the `.when()` wiring (via
// `apply_when_to_pattern`) and capture handling live in ONE place.
//
// The three ROOT FLAVORS differ only in how `build_pattern_py` and the
// nestable-compile methods are derived from `core_builder`:
//
//   * `value` — the node produces a value; exposes `compile_value`
//     (`DynMatch`, `.into_pattern()` sealed) so it can nest as a value
//     operand. `.when()` is honoured by `wrap_when` on the MatchPat.
//   * `mem`   — the node produces a memory token; exposes `compile_mem`
//     (`DynMem`) so it can feed a `mem_in` slot. `build_pattern_py` seals
//     `core_builder().build()` and applies `.when()` via
//     `apply_when_to_pattern`.
//   * `node`  — node-rooted only; same `build_pattern_py` as `mem` but no
//     nestable compile method.
//
// Field kinds (`@field`): `pat`/`mem` (single `Option<Py<PyAny>>` operand,
// compiled via match / mem), `multi_match`/`multi_mem` (a `Vec<(usize,
// Py<PyAny>)>` of indexed operands), `scalar` (Copy, plain),
// `scalar_clone` (e.g. `String`), `scalar_inner` (a Py wrapper stored via
// `.inner`), and `flag` (a `bool` toggled by a no-arg setter).

macro_rules! node_builder {
    // ── struct members via a TT-muncher (a macro call can't sit in a ────
    // struct field position, so accumulate the field decls and emit the
    // `*Inner` struct once when the field list is exhausted).
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

    // ── per-field: apply onto the core builder `b` inside core_builder ──
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

    // ── #[pymethods] setters via a TT-muncher ───────────────────────────
    //
    // The setters can't be `node_builder!(@setter …)` calls inside the
    // `#[pymethods]` impl — pyo3's proc-macro can't see through a nested
    // macro item — so `@setters` munches the field list one head at a time,
    // appending each field's concrete setter tokens to an accumulator, and
    // emits the whole `#[pymethods] impl` once when the list is empty.

    // Base case: list exhausted — emit the accumulated setters in one impl.
    (@setters $ty:ident [ $($acc:tt)* ] ) => {
        #[gen_stub_pymethods]
        #[pymethods]
        impl $ty {
            $($acc)*
        }
    };
    // One recursive arm per field kind: append the setter, recurse on rest.
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

    // ── per-flavor: the nestable-compile + build_pattern_py methods ─────
    (@flavor value $core:path) => {
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
    (@flavor mem $core:path) => {
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
    (@flavor node $core:path) => {
        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };
    // `value_err` — node-rooted build, but exposes a `compile_value` that
    // rejects value nesting (the core `*Pat` only offers `.build()`).
    (@flavor value_err $core:path) => {
        /// Value nesting isn't supported for this builder (the core `*Pat`
        /// only offers a node-rooted `.build()`).
        fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let _ = py;
            Err(into_strider_err(anyhow::anyhow!(
                "phi() cannot be nested as a value operand"
            )))
        }

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };

    // ── entry point ─────────────────────────────────────────────────────
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

            /// Build a core `*Pat` with all set fields + `common.capture`
            /// applied (the `.when()` predicate is applied per root flavor).
            fn core_builder(&self, py: Python<'_>) -> PyResult<$core_ty> {
                let mut b = $core();
                $( node_builder!(@apply self, py, b, $field); )*
                if let Some(c) = self.common.borrow().capture {
                    b = b.capture(c);
                }
                Ok(b)
            }

            node_builder!(@flavor $root $core);
        }

        node_builder!(@setters $ty [] $($field)*);
        builder_common_methods!($ty);
    };
}

// ── LoadPat (value-rooted) ───────────────────────────────────────────────

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
        { scalar bit_width(u32 => u32): bit_width = "Filter loads by value width in bits." },
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

// ── StorePat (memory-rooted node) ────────────────────────────────────────

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
        { scalar bit_width(u32 => u32): bit_width = "Filter stores by data width in bits." },
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

// ── CallPat (node-rooted; also memory producer) ──────────────────────────

#[derive(Default)]
struct CallInner {
    target: Option<Py<PyAny>>,
    args: Vec<(usize, Py<PyAny>)>,
    mem: Option<Py<PyAny>>,
}

/// Typed builder for `Call` node patterns. Chain `.at(addr)`,
/// `.at_any(addrs)`, `.target(p)`, `.arg(idx, p)`, `.mem(m)`.
#[gen_stub_pyclass]
#[pyclass(name = "CallPat", module = "strider.pattern", unsendable)]
pub struct PyCallPat {
    inner: std::cell::RefCell<CallInner>,
    common: std::cell::RefCell<CommonState>,
    /// `Some(true)` => `.at(addr)` literal target; carried separately so
    /// `target` Pat and the literal-address forms don't clash.
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
        // Literal-address target takes precedence over a Pat target if both
        // are set (matches the old `at` overriding `target`).
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
        if let Some(c) = self.common.borrow().capture {
            b = b.capture(c);
        }
        Ok(b)
    }

    fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
        let b = self.core_builder(py)?;
        Ok(DynMem(Box::new(move |mb| b.compile_mem(mb))))
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
}
builder_common_methods!(PyCallPat);

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

// ── CallOtherPat (node-rooted; also memory producer) ─────────────────────

node_builder! {
    ty: PyCallOtherPat,
    inner: CallOtherInner,
    py_name: "CallOtherPat",
    doc: "Typed builder for `CallOther` node patterns.",
    core: strider_pattern::call_other,
    core_ty: strider_pattern::CallOtherPat,
    root: mem,
    fields: [
        { scalar user_op_id(u64 => u64): user_op_id
            = "Constrain the matched node's user-op id." },
        { scalar_clone name(String => String): name
            = "Constrain the matched node's user-op name." },
        { multi_match args(usize): arg
            = "Constrain raw `inputs[idx]` of the matched CallOther." },
    ],
}

// `ctrl` / `mem` are convenience aliases that push onto the same `args`
// vec at the fixed control / memory input slots — they don't fit the
// uniform per-slot setter shape, so they stay hand-written.
#[gen_stub_pymethods]
#[pymethods]
impl PyCallOtherPat {
    /// Convenience: match `inputs[0]` (control predecessor).
    fn ctrl<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().args.push((0, p));
        slf
    }
    /// Convenience: match `inputs[1]` (memory predecessor).
    fn mem<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().args.push((1, p));
        slf
    }
}

/// Start a `CallOther` pattern builder.
#[pyfunction]
pub fn call_other() -> PyCallOtherPat {
    PyCallOtherPat::new()
}

// ── RetPat (node-rooted) ─────────────────────────────────────────────────

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
    ],
}

/// Start a `Return` pattern builder.
#[pyfunction]
pub fn ret() -> PyRetPat {
    PyRetPat::new()
}

// ── IfPat (node-rooted; branches take finished Patterns) ─────────────────

#[derive(Default)]
struct IfInner {
    cond: Option<Py<PyAny>>,
    true_branch: Option<Py<PyAny>>,
    false_branch: Option<Py<PyAny>>,
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
}
builder_common_methods!(PyIfPat);

/// Build a finished `Pattern` from any pattern-like operand (used by the
/// `If` branch slots, which forward-walk and match a node-rooted Pattern).
fn pattern_for_operand(ob: &Bound<'_, PyAny>) -> PyResult<Pattern> {
    let py = ob.py();
    let like = ob.extract::<PatLike<'_>>()?;
    like.to_pattern(py)
}

/// Start an `If` pattern builder, optionally pre-setting the condition.
#[pyfunction]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<Py<PyAny>>) -> PyIfPat {
    let b = PyIfPat::new();
    if let Some(c) = cond {
        b.inner.borrow_mut().cond = Some(c);
    }
    b
}

// ── PhiPat (node-rooted; rejects value nesting) ──────────────────────────

node_builder! {
    ty: PyPhiPat,
    inner: PhiInner,
    py_name: "PhiPat",
    doc: "Typed builder for tagged-`Phi` patterns.",
    core: strider_pattern::phi,
    core_ty: strider_pattern::PhiPat,
    root: value_err,
    fields: [
        { scalar_inner for_vn(crate::sleigh::PyVn => rsleigh::Vn): for_vn
            = "Restrict the match to phi nodes for varnode `vn`." },
        { multi_match inputs(usize): input
            = "Constrain the value arriving from predecessor slot `idx`." },
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

// ── MemPhiPat (memory-rooted node) ───────────────────────────────────────

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
    ],
}

/// Start a `MemPhi` pattern builder.
#[pyfunction]
pub fn mem_phi() -> PyMemPhiPat {
    PyMemPhiPat::new()
}

// ── FunctionArgPat (value-rooted) ────────────────────────────────────────

/// Typed builder for `FunctionArg` carrier patterns. Chain `.index(i)` /
/// `.source_register(vn)` / `.source_stack(space, offset)`.
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
            .replace(Some(strider_ir::node::FunctionArgSource::Register(vn.inner)));
        slf
    }
    /// Constrain to an argument sourced from the stack at `(space, offset)`.
    fn source_stack(
        slf: PyRef<'_, Self>,
        space: crate::sleigh::PyVnSpace,
        offset: i64,
    ) -> PyRef<'_, Self> {
        slf.source
            .replace(Some(strider_ir::node::FunctionArgSource::Stack {
                space: space.inner,
                offset,
            }));
        slf
    }
}

// The shared `capture` / `cap` / `when` / `into_pat` methods come from the
// common-methods macro (a second `#[pymethods]` block — pyo3 allows several
// per class).
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
    b.source.replace(Some(
        strider_ir::node::FunctionArgSource::Register(vn.inner),
    ));
    b
}

/// Match a `FunctionArg` whose source is a specific stack slot.
#[pyfunction]
pub fn function_arg_stack(space: crate::sleigh::PyVnSpace, offset: i64) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source
        .replace(Some(strider_ir::node::FunctionArgSource::Stack {
            space: space.inner,
            offset,
        }));
    b
}

// ── Binary-op builders (chainable .ordered()) ────────────────────────────

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
                    // Build the chained combinators, then compile. We branch
                    // on the option combinations to keep the static types
                    // monomorphic per arm.
                    compile_binary_chain(mb, pat, ordered, capture, when)
                })))
            }

            fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
                Ok(self.compile_value(py)?.into_pattern())
            }
        }

        #[gen_stub_pymethods]
        #[pymethods]
        impl $ty {
            /// Force left-to-right operand matching (disable commutativity);
            /// terminal — finalises to a `Pat`.
            fn ordered(&self, py: Python<'_>) -> PyResult<PyPat> {
                self.ordered.set(true);
                let pat = self.build_pattern_py(py)?;
                Ok(PyPat::from_repr(PatRepr::Finished(Box::new(
                    std::cell::RefCell::new(Some(pat)),
                ))))
            }
        }

        // The shared `capture` / `cap` / `when` / `into_pat` methods come
        // from the common-methods macro (a second `#[pymethods]` block —
        // pyo3 allows several per class).
        builder_common_methods!($ty);
    };
}

/// Compile a binary-op core pattern with the optional `.ordered()` /
/// `.capture()` / `.when()` combinators applied, returning the root output.
fn compile_binary_chain<P: MatchPat + 'static>(
    mb: &mut MatcherBuilder,
    pat: P,
    ordered: bool,
    capture: Option<Capture>,
    when: Option<PyObject>,
) -> PatValueRef {
    // Apply `.ordered()` first (it pins commutativity on the root), then
    // capture, then when. Each combinator wraps the previous, so the order
    // mirrors `pat.ordered().capture(c).when_match(f)`.
    if ordered {
        let pat = pat.ordered();
        apply_cap_when(mb, pat, capture, when)
    } else {
        apply_cap_when(mb, pat, capture, when)
    }
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

// ── Module registration ──────────────────────────────────────────────────

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "pattern")?;
    m.add_class::<PyCapture>()?;
    m.add_class::<PyPat>()?;
    m.add_class::<PyPartialMatch>()?;
    m.add_class::<PyIntBinaryPat>()?;
    m.add_class::<PyFloatBinaryPat>()?;
    m.add_class::<PyBoolBinaryPat>()?;
    m.add_class::<PyCallPat>()?;
    m.add_class::<PyCallOtherPat>()?;
    m.add_class::<PyRetPat>()?;
    m.add_class::<PyIfPat>()?;
    m.add_class::<PyLoadPat>()?;
    m.add_class::<PyStorePat>()?;
    m.add_class::<PyPhiPat>()?;
    m.add_class::<PyMemPhiPat>()?;
    m.add_class::<PyFunctionArgPat>()?;
    m.add_class::<PyCastMask>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, &m)?)?;
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
    add_fn!(function_arg);
    add_fn!(function_arg_any);
    add_fn!(function_arg_reg);
    add_fn!(function_arg_stack);
    add_fn!(phi);
    add_fn!(phi_for);
    add_fn!(mem_phi);
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
    add_fn!(int_lt);
    add_fn!(int_le);
    add_fn!(int_slt);
    add_fn!(int_sle);
    add_fn!(int_carry);
    add_fn!(int_scarry);
    add_fn!(int_sborrow);
    add_fn!(neg);
    add_fn!(bit_not);
    add_fn!(not_);
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

    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.pattern", &m)?;
    Ok(())
}
