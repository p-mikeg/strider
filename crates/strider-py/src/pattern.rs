//! A capture name interns to one `Capture` per process, so `.capture("x")`, a
//! `Match` reader keyed by `"x"` and a rewrite RHS `"x"` all name the same
//! capture. A bare string is not a sub-pattern operand. `"_"` and `"any_"` are
//! reserved.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use pyo3::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};
use strider_ir::node::ValueType as T;

use strider_pattern as sp;
use strider_pattern::matcher::{MatcherBuilder, PatValueRef};
use strider_pattern::template::{TemplateBuilder, TmplValueRef};
use strider_pattern::{
    Capture, CaptureExt, JoinConstraint, MatchPat, MemPat, Pattern, Template, TemplatePat,
    template as tpl,
};

use crate::errors::into_strider_err;
use crate::value_ops::value_ops;

/// Binds a matched node so its value, op variant or fingerprint can be read
/// back from the `Match`. Each `Capture()` is globally unique.
#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: Capture,
}

#[pymethods]
impl PyCapture {
    /// A capture variable. `Capture()` is fresh and unique; `Capture("name")`
    /// interns the name, so it is the SAME capture as a rewrite RHS `"name"`
    /// and reads back with `match.uint("name")`. A bare string is not a
    /// match-pattern operand. Reserved names (`"_"`, `"any_"`) raise.
    #[new]
    #[pyo3(signature = (name=None))]
    fn new(name: Option<&str>) -> PyResult<Self> {
        Ok(Self {
            inner: match name {
                Some(name) => intern_str(name)?,
                None => Capture::new(),
            },
        })
    }

    fn __repr__(&self) -> String {
        capture_display(self.inner.id())
    }

    fn __hash__(&self) -> isize {
        self.inner.id() as i64 as isize
    }

    /// Equal when both wrap the same interned id, so `Capture("x") ==
    /// Capture("x")` and a set/dict dedups them, consistent with `__hash__`.
    fn __richcmp__(&self, py: Python<'_>, other: &PyCapture, op: CompareOp) -> PyObject {
        let same = self.inner.id() == other.inner.id();
        match op {
            CompareOp::Eq => same.into_py(py),
            CompareOp::Ne => (!same).into_py(py),
            // No meaningful ordering on captures.
            _ => py.NotImplemented(),
        }
    }
}

// Both tables live for the process: a `Capture` is a bare u32 handle into
// them, so an id handed out stays resolvable. Names come from source text, so
// the set is bounded by the calling program.
fn intern_table() -> &'static Mutex<HashMap<String, Capture>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, Capture>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Reverse of `intern_table`, so a bound capture's id can be rendered as its
// source name in a `repr`.
fn reverse_name_table() -> &'static Mutex<HashMap<u32, String>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<u32, String>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn intern_str(name: &str) -> PyResult<Capture> {
    if name == "_" || name == "any_" {
        return Err(into_strider_err(anyhow::anyhow!(
            "{name:?} is reserved (use anything() / var() / _ explicitly)"
        )));
    }
    // Hits are every `m["off"]` / `"off" in m`, so they take one lock and allocate
    // nothing; only a first sighting builds the `String` and the reverse entry.
    let mut table = intern_table()
        .lock()
        .map_err(|_| into_strider_err(anyhow::anyhow!("intern table lock poisoned")))?;
    if let Some(&cap) = table.get(name) {
        return Ok(cap);
    }
    let cap = Capture::new();
    table.insert(name.to_string(), cap);
    drop(table);
    if let Ok(mut rev) = reverse_name_table().lock() {
        rev.entry(cap.id()).or_insert_with(|| name.to_string());
    }
    Ok(cap)
}

/// `Capture('name')` for a string-interned capture, else `Capture(<id>)` for a
/// fresh anonymous one.
pub(crate) fn capture_display(id: u32) -> String {
    match reverse_name_table()
        .lock()
        .ok()
        .and_then(|t| t.get(&id).cloned())
    {
        Some(name) => format!("Capture('{name}')"),
        None => format!("Capture({id})"),
    }
}

pub(crate) struct DynMatch(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef + Send>);

impl MatchPat for DynMatch {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

pub(crate) struct DynTemplate(
    pub(crate) Box<dyn FnOnce(&mut TemplateBuilder) -> TmplValueRef + Send>,
);

impl TemplatePat for DynTemplate {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        (self.0)(b)
    }
}

/// Type-erased sub-pattern yielding a memory token.
pub(crate) struct DynMem(pub(crate) Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef + Send>);

impl MatchPat for DynMem {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        (self.0)(b)
    }
}

impl MemPat for DynMem {}

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
    IntConstAnyWidth(i64),
    IntConstAnyWidthAnyOf(Vec<i64>),
    IntConstAnyOf(Vec<u128>),
    BoolConst(bool),
    FloatConst(u64),
    AnyInt(Option<Capture>),
    AnyBool(Option<Capture>),
    AnyFloat(Option<Capture>),
    AnyIntConst(Option<Capture>),
    AnyBoolConst(Option<Capture>),
    AnyFloatConst(Option<Capture>),
    InitialVar,
    InitialVarFor(rsleigh::Vn),
    /// Alternation (`one_of` union / `first_of` cut when the flag is set).
    /// Match-only: a rewrite RHS must build one concrete shape.
    OneOf(Vec<Py<PyAny>>, bool),
    IntBinary(strider_ir::IntBinaryOp, Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_add(l, int_neg(r))`.
    Sub(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(x, all_ones)`.
    BitNot(Py<PyAny>),
    IntUnary(IntUnaryKind, Py<PyAny>),
    Cast(CastKind, Py<PyAny>),
    Extend(strider_ir::ExtendOp, Py<PyAny>),
    IntCmp(strider_ir::IntCmpOp, Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_eq(l, r), 1)`.
    IntNe(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_lt(r, l), 1)`, operands swapped.
    IntLe(Py<PyAny>, Py<PyAny>),
    /// Lowers to `int_xor(int_slt(r, l), 1)`, operands swapped.
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
    /// Lowers to `int_xor(x, 1)`.
    BoolNot(Py<PyAny>),
    AnyIntBinary(Capture, Py<PyAny>, Py<PyAny>),
    AnyIntUnary(Capture, Py<PyAny>),
    AnyIntCmp(Capture, Py<PyAny>, Py<PyAny>),
    AnyBoolBinary(Capture, Py<PyAny>, Py<PyAny>),
    AnyFloatBinary(Capture, Py<PyAny>, Py<PyAny>),
    AnyFloatUnary(Capture, Py<PyAny>),
    AnyFloatCmp(Capture, Py<PyAny>, Py<PyAny>),
    // The wrapped `Pat` OBJECT, not its repr: a repr shared by several `Pat`s
    // has no owner to attribute its `Py` handles to, and reporting them once
    // per wrapper tells the collector about references that do not exist.
    Captured(Py<PyAny>, Capture),
    Guarded(Py<PyAny>, Py<PyAny>),
    /// Pins the wrapped shape's root operand order. Match-only.
    Ordered(Py<PyAny>),
    OfWidth(Py<PyAny>, u32),
    ValueTy(Py<PyAny>, strider_ir::node::ValueType),
    /// A finished control / variadic [`Pattern`] from a control builder's
    /// `.into_pat()`. One-shot: consumed when queried, and not nestable as a
    /// value operand.
    Finished {
        /// Taken on the first query.
        pattern: Box<std::sync::Mutex<Option<Pattern>>>,
        /// The `.when()` predicates compiled into `pattern`, shared with its
        /// closures: the only Python objects a `Pattern` holds, and otherwise
        /// invisible to the cyclic collector.
        when_handles: Vec<WhenFn>,
    },
}

/// A finished pattern. Reusable across `find_all` / rewrite calls, except one
/// built from a control or variadic builder's `into_pat()`, which is consumed
/// by its first query.
#[pyclass(name = "Pat", module = "strider.pattern")]
pub struct PyPat {
    pub(crate) repr: Arc<PatRepr>,
    /// Links below this one in the `.capture()` / `.when()` / `.of_width()`
    /// chain. Dropping the chain is native recursion, so it is bounded.
    depth: u32,
}

impl PyPat {
    pub(crate) fn from_repr(repr: PatRepr) -> Self {
        Self {
            repr: Arc::new(repr),
            depth: 0,
        }
    }
}

/// A `Pat` wrapping `slf`, which it owns as an operand handle.
fn derive(slf: &Bound<'_, PyPat>, make: impl FnOnce(Py<PyAny>) -> PatRepr) -> PyResult<PyPat> {
    let depth = slf.borrow().depth + 1;
    if depth > MAX_PATTERN_NESTING {
        return Err(into_strider_err(anyhow::anyhow!(
            "pattern nesting too deep (max {MAX_PATTERN_NESTING} compile levels; a nested builder call costs two)"
        )));
    }
    Ok(PyPat {
        repr: Arc::new(make(slf.clone().into_any().unbind())),
        depth,
    })
}

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
    // A raw int is `int_const(value)`, carried as the u128 `IntConst` interns;
    // narrow with `.of_width`.  Signed first so a negative keeps its
    // two's-complement bits, then unsigned for `[2^127, 2^128)`, which `i128`
    // cannot hold.  Anything wider has no carrier and falls through to the
    // operand-kind error below.
    if let Ok(v) = ob.extract::<i128>() {
        return PatRepr::IntConst(v as u128).compile_match(py);
    }
    if let Ok(v) = ob.extract::<u128>() {
        return PatRepr::IntConst(v).compile_match(py);
    }
    if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
        let cap = c.inner;
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
        "expected a value pattern (a Pat, a Capture, an int, or a value \
         builder). A bare string is not a capture: use Capture(\"name\") for a \
         capture or anything() for a wildcard. A control / variadic builder \
         (store / ret / if / mem_phi) cannot be nested as a value operand."
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
    // A raw int builds an `int_const(value)` on the RHS too. Signed first, then
    // unsigned for `[2^127, 2^128)`, matching `compile_operand_match`.
    if let Ok(v) = ob.extract::<i128>() {
        return PatRepr::IntConst(v as u128).compile_template(py);
    }
    if let Ok(v) = ob.extract::<u128>() {
        return PatRepr::IntConst(v).compile_template(py);
    }
    if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
        let cap = c.inner;
        return Ok(DynTemplate(Box::new(move |b| template_var(b, cap))));
    }
    Err(into_strider_err(anyhow::anyhow!(
        "cannot use {} as a nested rewrite RHS operand: expected a Template, a \
         build-valid Pat, a Capture or an int. A bare string names a capture \
         only as the whole RHS; nested, use Capture(\"name\")",
        operand_kind_name(ob)
    )))
}

pub(crate) fn compile_operand_mem(ob: &Bound<'_, PyAny>) -> PyResult<DynMem> {
    // Same recursion bound as `compile_operand_match`.
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    // A `one_of` / `first_of` nests in a memory slot: its output kind is `Any`,
    // and `DynMatch` / `DynMem` wrap the same closure, so bridge `.0` across.
    // Through the chainable wrappers too, so `.cap` does not change which
    // slots accept the alternation.
    if let Ok(p) = ob.downcast::<PyPat>()
        && wraps_alternation(ob)
    {
        return Ok(DynMem(compile_repr_match(&p.borrow().repr, py)?.0));
    }
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
        "a memory-input slot (`mem`) requires a memory producer: \
         store() / mem_phi() / call() / call_other(); got a value operand \
         ({})",
        operand_kind_name(ob)
    )))
}

/// A `list` operand becomes a `one_of` union, so `target([a, b])` matches
/// either and `target([])` matches nothing. Anything else passes through.
fn coerce_operand_list(py: Python<'_>, p: Py<PyAny>) -> PyResult<Py<PyAny>> {
    match p.bind(py).downcast::<pyo3::types::PyList>() {
        Ok(list) => Ok(Py::new(py, one_of(list.extract()?))?.into_any()),
        Err(_) => Ok(p),
    }
}

/// `any_input` operands and `one_of` / `first_of` arms: a memory producer
/// (`store` / `mem_phi`) wires as a memory sub, everything else as a value sub
/// (a wildcard already reaches control / phi-token slots). `DynMatch` and
/// `DynMem` wrap the same closure, so the memory sub reuses the value path.
pub(crate) fn compile_any_input(ob: &Bound<'_, PyAny>) -> PyResult<DynMatch> {
    if ob.downcast::<PyStorePat>().is_ok() || ob.downcast::<PyMemPhiPat>().is_ok() {
        return Ok(DynMatch(compile_operand_mem(ob)?.0));
    }
    compile_operand_match(ob)
}

/// Compile a `one_of` / `first_of` arm, which is any pattern: the slot the
/// alternation sits in decides which edge the arm binds, not the caller. The
/// node-rooted control builders (ret / if / switch / indirect_branch /
/// unreachable) have no output vertex, so they route through their
/// `compile_alt_arm` (a synthesized `Any` output); every other kind goes
/// through the value / memory arm bridge.
fn compile_alt_arm_operand(ob: &Bound<'_, PyAny>) -> PyResult<DynMatch> {
    let py = ob.py();
    // `call` / `call_other` produce a memory token as well as values, and an
    // arm binds whichever edge the alternation is matched against, so the
    // anchor stays kind-agnostic instead of pinning the value output.
    if ob.downcast::<PyCallPat>().is_ok() || ob.downcast::<PyCallOtherPat>().is_ok() {
        let inner = compile_operand_match(ob)?;
        return Ok(DynMatch(Box::new(move |b| {
            let out = (inner.0)(b);
            b.set_output_any(out);
            out
        })));
    }
    if let Ok(b) = ob.downcast::<PyRetPat>() {
        return b.borrow().compile_alt_arm(py);
    }
    if let Ok(b) = ob.downcast::<PyIfPat>() {
        return b.borrow().compile_alt_arm(py);
    }
    if let Ok(b) = ob.downcast::<PySwitchPat>() {
        return b.borrow().compile_alt_arm(py);
    }
    if let Ok(b) = ob.downcast::<PyIndirectBranchPat>() {
        return b.borrow().compile_alt_arm(py);
    }
    if let Ok(b) = ob.downcast::<PyUnreachablePat>() {
        return b.borrow().compile_alt_arm(py);
    }
    compile_any_input(ob)
}

/// Whether `ob` is a `one_of` / `first_of`, looking through the chainable
/// wrappers (`.capture` / `.when` / `.of_width` / `.value_ty`), which
/// leave the alternation's `Any` output kind alone.
fn wraps_alternation(ob: &Bound<'_, PyAny>) -> bool {
    let py = ob.py();
    let mut cur: Py<PyAny> = ob.clone().unbind();
    for _ in 0..MAX_PATTERN_NESTING {
        let Ok(p) = cur.bind(py).downcast::<PyPat>() else {
            return false;
        };
        let next = match &*p.borrow().repr {
            PatRepr::OneOf(..) => return true,
            PatRepr::Captured(inner, _)
            | PatRepr::Guarded(inner, _)
            | PatRepr::OfWidth(inner, _)
            | PatRepr::ValueTy(inner, _) => inner.clone_ref(py),
            _ => return false,
        };
        cur = next;
    }
    false
}

/// Ok for the binary ops and comparisons `.ordered()` can pin; Err names the
/// shape otherwise. Commutativity itself is not decided here: the matcher pins
/// nothing unless the matched IR kind reports `is_commutative`.
fn orderable(repr: &PatRepr) -> Result<(), &'static str> {
    match repr {
        PatRepr::IntBinary(..)
        | PatRepr::Sub(..)
        | PatRepr::IntCmp(..)
        | PatRepr::IntNe(..)
        | PatRepr::IntLe(..)
        | PatRepr::IntSle(..)
        | PatRepr::FloatBinary(..)
        | PatRepr::FloatSub(..)
        | PatRepr::FloatCmp(..)
        | PatRepr::FloatNe(..)
        | PatRepr::FloatLe(..)
        | PatRepr::FloatIsNan(_)
        | PatRepr::BoolBinary(..)
        | PatRepr::AnyIntBinary(..)
        | PatRepr::AnyIntCmp(..)
        | PatRepr::AnyBoolBinary(..)
        | PatRepr::AnyFloatBinary(..)
        | PatRepr::AnyFloatCmp(..) => Ok(()),
        PatRepr::Any => Err("anything()"),
        PatRepr::Var(_) => Err("var()"),
        PatRepr::ValueOfWidth(_) => Err("value_of_width()"),
        PatRepr::InputsOfWidth(..) => Err("inputs_of_width()"),
        PatRepr::IntConst(_) | PatRepr::AnyIntConst(_) => Err("int_const()"),
        PatRepr::IntConstAnyWidth(_) => Err("int_const_any_width()"),
        PatRepr::IntConstAnyWidthAnyOf(_) => Err("int_const_any_width([..])"),
        PatRepr::IntConstAnyOf(_) => Err("int_const([..])"),
        PatRepr::BoolConst(_) | PatRepr::AnyBoolConst(_) => Err("bool_const()"),
        PatRepr::FloatConst(_) | PatRepr::AnyFloatConst(_) => Err("float_const()"),
        PatRepr::AnyInt(_) => Err("any_int()"),
        PatRepr::AnyBool(_) => Err("any_bool()"),
        PatRepr::AnyFloat(_) => Err("any_float()"),
        PatRepr::InitialVar => Err("initial_var()"),
        PatRepr::InitialVarFor(_) => Err("initial_var_for()"),
        PatRepr::OneOf(_, true) => Err("first_of()"),
        PatRepr::OneOf(_, false) => Err("one_of()"),
        PatRepr::BitNot(_) => Err("int_not()"),
        PatRepr::IntUnary(IntUnaryKind::Neg, _) => Err("int_neg()"),
        PatRepr::IntUnary(IntUnaryKind::Popcount, _) => Err("int_popcount()"),
        PatRepr::IntUnary(IntUnaryKind::Lzcount, _) => Err("int_lzcount()"),
        PatRepr::Cast(..) | PatRepr::Extend(..) => Err("a cast"),
        PatRepr::FloatUnary(..) => Err("a float unary op"),
        PatRepr::BoolNot(_) => Err("bool_not()"),
        PatRepr::AnyIntUnary(..) => Err("any_int_unary()"),
        PatRepr::AnyFloatUnary(..) => Err("any_float_unary()"),
        PatRepr::Finished { .. } => Err("a control / variadic builder"),
        PatRepr::Captured(..)
        | PatRepr::Guarded(..)
        | PatRepr::Ordered(_)
        | PatRepr::OfWidth(..)
        | PatRepr::ValueTy(..) => Err("a chained wrapper"),
    }
}

/// The shape below the chainable wrappers, which decorate the same root node
/// they wrap and so leave what `.ordered()` pins unchanged.
fn unwrapped_repr(pat: &Bound<'_, PyPat>) -> Arc<PatRepr> {
    let py = pat.py();
    let mut cur = Arc::clone(&pat.borrow().repr);
    for _ in 0..MAX_PATTERN_NESTING {
        let next = match &*cur {
            PatRepr::Captured(inner, _)
            | PatRepr::Guarded(inner, _)
            | PatRepr::Ordered(inner)
            | PatRepr::OfWidth(inner, _)
            | PatRepr::ValueTy(inner, _) => match inner.bind(py).downcast::<PyPat>() {
                Ok(p) => Arc::clone(&p.borrow().repr),
                Err(_) => return cur,
            },
            _ => return cur,
        };
        cur = next;
    }
    cur
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
        "cannot use {kind} as a rewrite RHS; the RHS must be a buildable \
         value expression"
    ))
}

/// Case-insensitive: `"i1"` / `"I64"` / `"f32"`.
fn parse_value_ty(name: &str) -> PyResult<T> {
    let lower = name.to_ascii_lowercase();
    T::ALL
        .into_iter()
        .find(|t| t.as_str() == lower)
        .ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "unknown output type {name:?}: expected one of {}",
                T::ALL
                    .into_iter()
                    .map(T::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

impl PatRepr {
    /// Every Python object this node holds, for the cyclic GC.
    pub(crate) fn traverse(&self, visit: &pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        match self {
            PatRepr::InputsOfWidth(_, x)
            | PatRepr::BitNot(x)
            | PatRepr::IntUnary(_, x)
            | PatRepr::Cast(_, x)
            | PatRepr::Extend(_, x)
            | PatRepr::FloatUnary(_, x)
            | PatRepr::FloatIsNan(x)
            | PatRepr::BoolNot(x)
            | PatRepr::AnyIntUnary(_, x)
            | PatRepr::AnyFloatUnary(_, x) => visit.call(x)?,
            PatRepr::IntBinary(_, l, r)
            | PatRepr::Sub(l, r)
            | PatRepr::IntCmp(_, l, r)
            | PatRepr::IntNe(l, r)
            | PatRepr::IntLe(l, r)
            | PatRepr::IntSle(l, r)
            | PatRepr::FloatBinary(_, l, r)
            | PatRepr::FloatSub(l, r)
            | PatRepr::FloatCmp(_, l, r)
            | PatRepr::FloatNe(l, r)
            | PatRepr::FloatLe(l, r)
            | PatRepr::BoolBinary(_, l, r)
            | PatRepr::AnyIntBinary(_, l, r)
            | PatRepr::AnyIntCmp(_, l, r)
            | PatRepr::AnyBoolBinary(_, l, r)
            | PatRepr::AnyFloatBinary(_, l, r)
            | PatRepr::AnyFloatCmp(_, l, r) => {
                visit.call(l)?;
                visit.call(r)?;
            }
            PatRepr::OneOf(alts, _) => {
                for a in alts {
                    visit.call(a)?;
                }
            }
            PatRepr::Guarded(inner, f) => {
                visit.call(inner)?;
                visit.call(f)?;
            }
            PatRepr::Captured(inner, _)
            | PatRepr::Ordered(inner)
            | PatRepr::OfWidth(inner, _)
            | PatRepr::ValueTy(inner, _) => visit.call(inner)?,
            PatRepr::Finished { when_handles, .. } => {
                for f in when_handles {
                    visit.call(&**f)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

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
// process. This counter turns that abort into an exception.
//
// It bounds COMPILING, and the `.capture()` / `.when()` / `.of_width()` chain
// at construction. It does NOT bound the other two ways to nest: a free
// constructor (`int_add(deep, ...)`) goes through `PyPat::from_repr`, which
// starts a fresh count, and a builder operand slot holds a bare `Py<PyAny>`
// with no count at all. Either can be driven from a Python `for` loop, which
// involves no Python recursion and so hits no interpreter limit; DROPPING the
// result is unbounded native recursion, and MEASURED it overflows an 8 MiB
// stack at roughly 7_000 links (raising `ulimit -s` moves it). Bounding those
// needs the operand depth computed at construction, or an iterative `Drop`.

/// Compile-recursion levels, NOT builder calls: a nested call costs two, so
/// the ceiling a caller sees is about half this. Well above any hand-written
/// pattern; a machine-generated one can exceed it.
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
                    "pattern nesting too deep (max {MAX_PATTERN_NESTING} compile levels; a nested builder call costs two)"
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
        PatRepr::Any => DynMatch(Box::new(|b| mc(sp::anything(), b))),
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
        PatRepr::IntConstAnyWidth(v) => {
            let v = *v;
            DynMatch(Box::new(move |b| mc(sp::int_const_any_width(v), b)))
        }
        PatRepr::IntConstAnyWidthAnyOf(vals) => {
            let vals = vals.clone();
            DynMatch(Box::new(move |b| mc(sp::int_const_any_width(vals), b)))
        }
        PatRepr::IntConstAnyOf(vals) => {
            let vals = vals.clone();
            DynMatch(Box::new(move |b| mc(sp::int_const(vals), b)))
        }
        PatRepr::BoolConst(v) => {
            let v = *v;
            DynMatch(Box::new(move |b| mc(sp::bool_const(v), b)))
        }
        PatRepr::FloatConst(bits) => {
            let bits = *bits;
            DynMatch(Box::new(move |b| mc(sp::float_const(bits), b)))
        }
        PatRepr::AnyInt(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_int().capture(c), b),
                None => mc(sp::any_int(), b),
            }))
        }
        PatRepr::AnyBool(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_bool().capture(c), b),
                None => mc(sp::any_bool(), b),
            }))
        }
        PatRepr::AnyFloat(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::any_float().capture(c), b),
                None => mc(sp::any_float(), b),
            }))
        }
        PatRepr::AnyIntConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::int_const(c), b),
                None => mc(sp::any_int_const(), b),
            }))
        }
        PatRepr::AnyBoolConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::bool_const(c), b),
                None => mc(sp::any_bool_const(), b),
            }))
        }
        PatRepr::AnyFloatConst(c) => {
            let c = *c;
            DynMatch(Box::new(move |b| match c {
                Some(c) => mc(sp::float_const(c), b),
                None => mc(sp::any_float_const(), b),
            }))
        }
        PatRepr::InitialVar => DynMatch(Box::new(|b| mc(sp::initial_var(), b))),
        PatRepr::InitialVarFor(vn) => {
            let vn = *vn;
            DynMatch(Box::new(move |b| mc(sp::initial_var_for(vn), b)))
        }
        PatRepr::OneOf(alts, first) => {
            let first = *first;
            let boxed: Vec<sp::BoxedAlt> = alts
                .iter()
                .map(|a| compile_alt_arm_operand(a.bind(py)).map(sp::boxed_alt))
                .collect::<PyResult<_>>()?;
            let alt = if first {
                sp::OneOf::first(boxed)
            } else {
                sp::OneOf::new(boxed)
            };
            DynMatch(Box::new(move |b| mc(alt, b)))
        }
        PatRepr::IntBinary(op, l, r) => m_binop!(sp::int_binary, op, l, r),
        PatRepr::Sub(l, r) => m_bin!(sp::int_sub, l, r),
        PatRepr::BitNot(x) => m_un!(sp::int_not, x),
        PatRepr::IntUnary(kind, x) => {
            let kind = *kind;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| match kind {
                IntUnaryKind::Neg => mc(sp::int_neg(x), b),
                IntUnaryKind::Popcount => mc(sp::int_popcount(x), b),
                IntUnaryKind::Lzcount => mc(sp::int_lzcount(x), b),
            }))
        }
        PatRepr::Cast(kind, x) => {
            let kind = *kind;
            let x = op_match(py, x)?;
            DynMatch(Box::new(move |b| cast_match(kind, x, b)))
        }
        PatRepr::Extend(op, x) => m_unop!(sp::int_extend, op, x),
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
        PatRepr::AnyIntBinary(c, l, r) => m_bin_any!(sp::any_int_binary, c, l, r),
        PatRepr::AnyIntUnary(c, x) => m_un_any!(sp::any_int_unary, c, x),
        PatRepr::AnyIntCmp(c, l, r) => m_bin_any!(sp::any_int_cmp, c, l, r),
        PatRepr::AnyBoolBinary(c, l, r) => m_bin_any!(sp::any_bool_binary, c, l, r),
        PatRepr::AnyFloatBinary(c, l, r) => m_bin_any!(sp::any_float_binary, c, l, r),
        PatRepr::AnyFloatUnary(c, x) => m_un_any!(sp::any_float_unary, c, x),
        PatRepr::AnyFloatCmp(c, l, r) => m_bin_any!(sp::any_float_cmp, c, l, r),
        PatRepr::Captured(inner, c) => {
            let c = *c;
            let inner = compile_operand_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.capture(c), b)))
        }
        PatRepr::Guarded(inner, f) => {
            let inner = compile_operand_match(inner.bind(py))?;
            let f = f.clone_ref(py);
            DynMatch(Box::new(move |b| mc(wrap_when(inner, f), b)))
        }
        PatRepr::Ordered(inner) => compile_ordered_match(inner.bind(py))?,
        PatRepr::OfWidth(inner, n) => {
            let n = *n;
            let inner = compile_operand_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.of_width(n), b)))
        }
        PatRepr::ValueTy(inner, ty) => {
            let ty = *ty;
            let inner = compile_operand_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.value_ty(ty), b)))
        }
        PatRepr::Finished { .. } => {
            return Err(into_strider_err(anyhow::anyhow!(
                "a finished control / variadic pattern cannot be nested as a \
                 value operand"
            )));
        }
    })
}

fn ordered_dyn<P: MatchPat + 'static>(p: P) -> DynMatch {
    DynMatch(Box::new(move |b| mc(p.ordered(), b)))
}

/// Compiles `ob` with `.ordered()` on the node carrying the operand pair the
/// caller wrote, which for a lowered comparison is the comparison itself
/// rather than the root the lowering adds.
fn compile_ordered_match(ob: &Bound<'_, PyAny>) -> PyResult<DynMatch> {
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    let Ok(pat) = ob.downcast::<PyPat>() else {
        return Ok(ordered_dyn(compile_operand_match(ob)?));
    };
    let repr = Arc::clone(&pat.borrow().repr);
    // `xor(cmp, 1)`, the lowered form's root, with the comparison pinned.
    macro_rules! m_ordered_cmp {
        ($f:path, $l:expr, $r:expr) => {{
            let cmp = ordered_dyn($f(op_match(py, $l)?, op_match(py, $r)?));
            DynMatch(Box::new(move |b| {
                mc(sp::int_xor(cmp, sp::bool_const(true)), b)
            }))
        }};
    }
    Ok(match &*repr {
        // The chainable wrappers decorate the node below them, so the pin has
        // to be placed under them and the wrapper re-applied on top.
        PatRepr::Captured(inner, c) => {
            let c = *c;
            let inner = compile_ordered_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.capture(c), b)))
        }
        PatRepr::Guarded(inner, f) => {
            let f = f.clone_ref(py);
            let inner = compile_ordered_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(wrap_when(inner, f), b)))
        }
        PatRepr::OfWidth(inner, n) => {
            let n = *n;
            let inner = compile_ordered_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.of_width(n), b)))
        }
        PatRepr::ValueTy(inner, ty) => {
            let ty = *ty;
            let inner = compile_ordered_match(inner.bind(py))?;
            DynMatch(Box::new(move |b| mc(inner.value_ty(ty), b)))
        }
        PatRepr::Ordered(inner) => compile_ordered_match(inner.bind(py))?,
        PatRepr::IntNe(l, r) => m_ordered_cmp!(sp::int_eq, l, r),
        PatRepr::IntLe(l, r) => m_ordered_cmp!(sp::int_lt, r, l),
        PatRepr::IntSle(l, r) => m_ordered_cmp!(sp::int_slt, r, l),
        PatRepr::FloatNe(l, r) => m_ordered_cmp!(sp::float_eq, l, r),
        // `float_le` fans its pair into a `Less` that pins the order already,
        // and `float_is_nan` writes one operand into both equality slots.
        PatRepr::FloatLe(..) | PatRepr::FloatIsNan(_) => compile_operand_match(ob)?,
        _ => ordered_dyn(compile_operand_match(ob)?),
    })
}

fn cast_match(kind: CastKind, x: DynMatch, b: &mut MatcherBuilder) -> PatValueRef {
    match kind {
        CastKind::Truncate => mc(sp::int_truncate(x), b),
        CastKind::ZeroExtend => mc(sp::int_zero_extend(x), b),
        CastKind::SignExtend => mc(sp::int_sign_extend(x), b),
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
        PatRepr::IntConstAnyWidth(v) => {
            let v = *v;
            DynTemplate(Box::new(move |b| tc(sp::int_const_any_width(v), b)))
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
        PatRepr::Sub(l, r) => t_bin!(tpl::int_sub, l, r),
        PatRepr::BitNot(x) => t_un!(tpl::int_not, x),
        PatRepr::IntUnary(kind, x) => {
            let kind = *kind;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| match kind {
                IntUnaryKind::Neg => tc(tpl::int_neg(x), b),
                IntUnaryKind::Popcount => tc(tpl::int_popcount(x), b),
                IntUnaryKind::Lzcount => tc(tpl::int_lzcount(x), b),
            }))
        }
        PatRepr::Cast(kind, x) => {
            let kind = *kind;
            let x = op_tpl(py, x)?;
            DynTemplate(Box::new(move |b| cast_tpl(kind, x, b)))
        }
        PatRepr::Extend(op, x) => t_unop!(tpl::int_extend, op, x),
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
            // whatever it wrapped, so the RHS drops `inner`.
            let c = *c;
            DynTemplate(Box::new(move |b| b.capture(c)))
        }
        PatRepr::Any => return Err(rhs_error("any")),
        PatRepr::ValueOfWidth(_) => return Err(rhs_error("value_of_width")),
        PatRepr::InputsOfWidth(..) => return Err(rhs_error("inputs_of_width")),
        PatRepr::IntConstAnyOf(_) => return Err(rhs_error("int_const([..])")),
        PatRepr::IntConstAnyWidthAnyOf(_) => return Err(rhs_error("int_const_any_width([..])")),
        PatRepr::AnyInt(_) => return Err(rhs_error("any_int")),
        PatRepr::AnyBool(_) => return Err(rhs_error("any_bool")),
        PatRepr::AnyFloat(_) => return Err(rhs_error("any_float")),
        PatRepr::AnyIntConst(_) => return Err(rhs_error("int_const()")),
        PatRepr::AnyBoolConst(_) => return Err(rhs_error("bool_const()")),
        PatRepr::AnyFloatConst(_) => return Err(rhs_error("float_const()")),
        PatRepr::InitialVar => return Err(rhs_error("initial_var")),
        PatRepr::InitialVarFor(_) => return Err(rhs_error("initial_var_for")),
        PatRepr::OneOf(_, first) => {
            return Err(rhs_error(if *first { "first_of" } else { "one_of" }));
        }
        PatRepr::IntNe(..) => return Err(rhs_error("int_ne")),
        PatRepr::IntLe(..) => return Err(rhs_error("int_le")),
        PatRepr::IntSle(..) => return Err(rhs_error("int_sle")),
        PatRepr::FloatNe(..) => return Err(rhs_error("float_ne")),
        PatRepr::FloatLe(..) => return Err(rhs_error("float_le")),
        PatRepr::FloatIsNan(_) => return Err(rhs_error("float_is_nan")),
        PatRepr::AnyIntBinary(..) => return Err(rhs_error("any_int_binary")),
        PatRepr::AnyIntUnary(..) => return Err(rhs_error("any_int_unary")),
        PatRepr::AnyIntCmp(..) => return Err(rhs_error("any_int_cmp")),
        PatRepr::AnyBoolBinary(..) => return Err(rhs_error("any_bool_binary")),
        PatRepr::AnyFloatBinary(..) => return Err(rhs_error("any_float_binary")),
        PatRepr::AnyFloatUnary(..) => return Err(rhs_error("any_float_unary")),
        PatRepr::AnyFloatCmp(..) => return Err(rhs_error("any_float_cmp")),
        PatRepr::Ordered(..) => return Err(rhs_error(".ordered()")),
        PatRepr::Guarded(..) => return Err(rhs_error(".when()")),
        PatRepr::OfWidth(..) => return Err(rhs_error(".of_width()")),
        PatRepr::ValueTy(..) => return Err(rhs_error(".value_ty()")),
        PatRepr::Finished { .. } => return Err(rhs_error("control / variadic builder")),
    })
}

fn cast_tpl(kind: CastKind, x: DynTemplate, b: &mut TemplateBuilder) -> TmplValueRef {
    match kind {
        CastKind::Truncate => tc(tpl::int_truncate(x), b),
        CastKind::ZeroExtend => tc(tpl::int_zero_extend(x), b),
        CastKind::SignExtend => tc(tpl::int_sign_extend(x), b),
        CastKind::IntToFloat => tc(tpl::int_to_float(x), b),
        CastKind::FloatToInt => tc(tpl::float_to_int(x), b),
        CastKind::IntBitsToFloat => tc(tpl::int_bits_to_float(x), b),
        CastKind::FloatBitsToInt => tc(tpl::float_bits_to_int(x), b),
        CastKind::FloatToFloat => tc(tpl::float_to_float(x), b),
    }
}

impl PatRepr {
    pub(crate) fn to_pattern(&self, py: Python<'_>) -> PyResult<Pattern> {
        if let PatRepr::Finished {
            pattern,
            when_handles,
        } = self
        {
            // Compiled in before this scope opened, so the attachments are
            // reported here instead.
            if !when_handles.is_empty() {
                note_when_attached();
            }
            return pattern
                .lock()
                .expect("pattern cache poisoned")
                .take()
                .ok_or_else(|| {
                    into_strider_err(anyhow::anyhow!(
                        "this control / variadic pattern was already consumed by a \
                     prior query; rebuild it for each find/rewrite call"
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
/// a `Pat`, a `Capture`, a raw int (an `int_const`), or any typed builder that
/// finalises to a pattern. A bare string is NOT a capture: use `Capture(name)`.
#[derive(FromPyObject)]
pub enum PatLike<'py> {
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Int(i128),
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
// `Single` is tried before `Many` so a lone pattern is not read as a length-1
// join.
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

impl PatLike<'_> {
    pub(crate) fn to_pattern(&self, py: Python<'_>) -> PyResult<Pattern> {
        match self {
            PatLike::Pat(p) => p.borrow().repr.to_pattern(py),
            PatLike::Capture(c) => Ok(strider_pattern::var(c.borrow().inner).into_pattern()),
            PatLike::Int(v) => PatRepr::IntConst(*v as u128).to_pattern(py),
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

// A `.when()` predicate that raises stashes the error here instead of
// propagating: the matcher must finish its walk, and returning to CPython
// with an exception still set trips its "returned a result with an exception
// set" guard. The first error wins, every later call returning no-match
// without running the predicate; the outer find boundary drains and re-raises.
thread_local! {
    static PENDING_QUERY_ERROR: std::cell::Cell<Option<PyErr>> =
        const { std::cell::Cell::new(None) };
}

pub(crate) fn take_pending_query_error() -> Option<PyErr> {
    PENDING_QUERY_ERROR.with(std::cell::Cell::take)
}

pub(crate) fn peek_pending_query_error() -> bool {
    PENDING_QUERY_ERROR.with(|cell| {
        let t = cell.take();
        let pending = t.is_some();
        cell.set(t);
        pending
    })
}

pub(crate) fn stash_pending_query_error(e: PyErr) {
    PENDING_QUERY_ERROR.with(|cell| cell.set(Some(e)));
}

// A `.when(f)` predicate is attached at pattern-build time, before any
// `Function` exists, and the same pattern can run against several of them, so
// the closure cannot capture a `Py<PyFunction>`. `Function::run_query` pushes
// the function plus its generation here instead, and `run_when_predicate`
// peeks the top to build the `Match` it hands the callback.
//
// A stack rather than one slot: a predicate may itself issue a nested query,
// which must not clobber the outer entry. Both entries share the live query's
// `Arc<RefCell<Function>>`, so a `Match` accessor re-borrowing it under
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
/// `node` becomes the `Match.root` the predicate sees. A predicate that
/// raises stashes its exception for the outer boundary and counts as
/// no-match, so the walk still finishes.
fn run_when_predicate(
    node: strider_ir::node::NodeId,
    bindings: &strider_pattern::Bindings,
    py_func: &PyObject,
) -> bool {
    Python::with_gil(|py| {
        if peek_pending_query_error() {
            return false;
        }
        let Some((function, generation)) = current_query_function(py) else {
            // Unreachable: every query path pushes an entry before running the
            // matcher. Fail closed rather than panic into the matcher's stack.
            eprintln!(
                "strider .when(): no active query function on this thread (internal \
                 error); treating as no-match"
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
                stash_pending_query_error(e);
                return false;
            }
        };
        let args = PyTuple::new_bound(py, [py_match]);
        let result = py_func.call_bound(py, args, None);
        match result {
            Ok(obj) => match obj.extract::<bool>(py) {
                Ok(b) => b,
                Err(e) => {
                    stash_pending_query_error(e);
                    false
                }
            },
            Err(e) => {
                stash_pending_query_error(e);
                false
            }
        }
    })
}

// A rewrite holds the function borrowed for mutation while its rules run, so a
// `.when()` predicate could not read the `Match` it is handed. The LHS compile
// runs inside this scope and every predicate attachment reports itself, so the
// rewrite can refuse up front instead of silently matching nothing.
thread_local! {
    static REWRITE_LHS_WHEN: std::cell::Cell<Option<bool>> = const {
        std::cell::Cell::new(None)
    };
}

fn note_when_attached() {
    REWRITE_LHS_WHEN.with(|c| {
        if c.get().is_some() {
            c.set(Some(true));
        }
    });
}

/// A `.when()` predicate baked into a compiled [`Pattern`], shared with the
/// closure that runs it. One attachment is one strong Python reference, which
/// the owning `Pat` reports to the cyclic collector exactly once.
pub(crate) type WhenFn = Arc<PyObject>;

// Collects the predicates baked into the pattern being finalised, while a
// `retain_when_handles` scope is open.
thread_local! {
    static RETAINED_WHEN: std::cell::RefCell<Option<Vec<WhenFn>>> =
        const { std::cell::RefCell::new(None) };
}

/// Reinstates the enclosing scope on every exit, including an unwind: left
/// open, the next `attach_when` lands in a scope whose pattern no longer
/// exists and the predicate is retained for the process lifetime.
struct RetainedWhenScope(Option<Vec<WhenFn>>);

impl RetainedWhenScope {
    fn open() -> Self {
        Self(RETAINED_WHEN.with(|c| c.replace(Some(Vec::new()))))
    }

    /// What the open scope has collected, leaving it open for `Drop`.
    fn handles() -> Vec<WhenFn> {
        RETAINED_WHEN.with(|c| c.borrow().clone().unwrap_or_default())
    }
}

impl Drop for RetainedWhenScope {
    fn drop(&mut self) {
        RETAINED_WHEN.with(|c| c.replace(self.0.take()));
    }
}

/// Runs `build` and returns the predicates its compiled closures kept.
fn retain_when_handles<T>(build: impl FnOnce() -> PyResult<T>) -> PyResult<(T, Vec<WhenFn>)> {
    let _scope = RetainedWhenScope::open();
    let built = build();
    Ok((built?, RetainedWhenScope::handles()))
}

/// Wraps `f` for a compiled closure, registering it with the open scope.
fn attach_when(f: PyObject) -> WhenFn {
    note_when_attached();
    let f = Arc::new(f);
    RETAINED_WHEN.with(|c| {
        if let Some(handles) = c.borrow_mut().as_mut() {
            handles.push(Arc::clone(&f));
        }
    });
    f
}

/// Compile `pat` as a rewrite LHS, rejecting a `.when()` guard anywhere in it.
pub(crate) fn compile_rewrite_lhs(py: Python<'_>, pat: &PatLike<'_>) -> PyResult<Pattern> {
    let outer = REWRITE_LHS_WHEN.with(|c| c.replace(Some(false)));
    let compiled = pat.to_pattern(py);
    let saw_when = REWRITE_LHS_WHEN.with(|c| c.replace(outer)) == Some(true);
    let compiled = compiled?;
    if saw_when {
        return Err(into_strider_err(anyhow::anyhow!(
            "a .when() predicate cannot run on a rewrite `find` pattern: the \
             function is held for mutation while the rule fires, so the \
             predicate would have no readable Match. Select the sites with \
             find_all(pat.when(f)) first, or put the condition in the pattern \
             itself (.of_width() / .value_ty() / int_const([..]))"
        )));
    }
    Ok(compiled)
}

/// Compiles a memory-token producer with its `.when()` predicate attached.
pub(crate) fn mem_with_when<M: MemPat + 'static>(
    inner: M,
    when: Option<PyObject>,
    b: &mut MatcherBuilder,
) -> PatValueRef {
    let out = inner.compile_mem(b);
    if let Some(f) = when {
        let py_func = attach_when(f);
        b.set_post_match(
            out,
            Box::new(move |_matcher, node, _ty, bindings| {
                run_when_predicate(node, bindings, &py_func)
            }),
        );
    }
    out
}

/// Attaches the predicate as a full `PostMatchFn`, which keeps the matched
/// `NodeId` the predicate needs for a real `Match.root`.
pub(crate) fn wrap_when<P: MatchPat + 'static>(inner: P, py_func: PyObject) -> impl MatchPat {
    let py_func = attach_when(py_func);
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
    let py_func = attach_when(py_func);
    Box::new(move |_matcher, node, _ty, bindings| run_when_predicate(node, bindings, &py_func))
}

/// Attaches an optional `.when()` predicate to a node-rooted `one_of` arm's
/// synthesized output.
fn alt_arm_with_when(
    out: PatValueRef,
    mb: &mut MatcherBuilder,
    when: Option<PyObject>,
) -> PatValueRef {
    if let Some(f) = when {
        mb.set_post_match(out, make_root_post_match(f));
    }
    out
}

fn apply_when_to_pattern(py: Python<'_>, common: &CommonState, pat: Pattern) -> Pattern {
    match common.when.as_ref() {
        Some(f) => pat.with_root_post_match(make_root_post_match(f.clone_ref(py))),
        None => pat,
    }
}

/// Selects which value-passthrough cast node kinds the matcher walks through
/// transparently. Build from the classmethods, combine with `|`, and pass as
/// `find_all(pat, ignore_casts=...)`.
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
                                                "The `", stringify!($value),
                                                "` cast mask, for the matcher to walk through."
                                            )]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self {
                    inner: strider_pattern::CastMask::$value,
                }
            }
        }
    };
    ($name:ident => fn $value:ident, $doc:literal) => {
        #[pymethods]
        impl PyCastMask {
            #[doc = $doc]
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
forall_castmask!(all => fn all,
    "Every value-passthrough cast, so the matcher walks through all of them.");
forall_castmask!(none => fn empty,
    "No cast at all, so the matcher walks through none of them.");

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
    /// Exposes the operand sub-patterns and any `.when()` predicate, which are
    /// `Py` handles the collector cannot otherwise see.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        self.repr.traverse(&visit)
    }

    /// Capture this pattern's matched node under `c`, a `Capture` or a name.
    /// Returns a new `Pat`.
    fn capture(slf: &Bound<'_, Self>, c: crate::matcher::CaptureKey<'_>) -> PyResult<PyPat> {
        let c = c.resolve()?;
        derive(slf, |inner| PatRepr::Captured(inner, c))
    }

    /// Attach a Python predicate that runs after this pattern matches.
    /// Returning `False` fails the match; raising aborts the whole query and
    /// re-raises out of `find_all` / `find_unique`, every later candidate
    /// failing without running the predicate.
    fn when(slf: &Bound<'_, Self>, f: PyObject) -> PyResult<PyPat> {
        derive(slf, |inner| PatRepr::Guarded(inner, f))
    }

    /// Constrain the matched node's value output to exactly `n` bits.
    /// Match-only.
    fn of_width(slf: &Bound<'_, Self>, n: u32) -> PyResult<PyPat> {
        derive(slf, |inner| PatRepr::OfWidth(inner, n))
    }

    /// Constrain the matched node's value output to the type named by `ty`,
    /// e.g. `"i1"`, `"i64"`, `"f32"`. Match-only.
    fn value_ty(slf: &Bound<'_, Self>, ty: &str) -> PyResult<PyPat> {
        let t = parse_value_ty(ty)?;
        derive(slf, |inner| PatRepr::ValueTy(inner, t))
    }

    /// Sugar for `.of_width(1)`, a boolean output. Match-only.
    fn bool_valued(slf: &Bound<'_, Self>) -> PyResult<PyPat> {
        derive(slf, |inner| PatRepr::OfWidth(inner, 1))
    }

    /// Stop the matcher retrying this op with its operands swapped; a no-op
    /// where the op's operands are ordered already. Pins this node alone, so
    /// a nested op still commutes unless itself `.ordered()`. A shape with no
    /// operands to order raises. Match-only.
    fn ordered(slf: &Bound<'_, Self>) -> PyResult<PyPat> {
        if let Err(kind) = orderable(&unwrapped_repr(slf)) {
            return Err(into_strider_err(anyhow::anyhow!(
                "ordered() pins the operand order of a binary op; got {kind}"
            )));
        }
        derive(slf, PatRepr::Ordered)
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

/// One constant, a set of them, or a capture, for [`int_const`].
#[derive(FromPyObject)]
pub enum IntConstArg {
    #[pyo3(annotation = "int")]
    One(i128),
    #[pyo3(annotation = "list[int]")]
    Set(Vec<i128>),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match an `IntConst` whose stored value equals `value`, BOTH masked to the
/// output width, so an over-wide argument still matches a narrow constant. Equals
/// `value`, or is one of `value` when given a list. Bit-pattern equality: a
/// negative uses its sign-extended form. For the cross-width form use
/// `int_const_any_width`.
///
/// Given a `Capture`, or no argument at all, match any integer constant,
/// binding it to the capture when there is one.
///
/// A list is one set-membership test, not an alternation, and an empty one
/// vacuously fails; each element is treated exactly as the scalar form treats
/// it.
#[pyfunction]
#[pyo3(signature = (value=None))]
pub fn int_const(value: Option<IntConstArg>) -> PyPat {
    PyPat::from_repr(match value {
        None => PatRepr::AnyIntConst(None),
        Some(IntConstArg::Cap(c)) => PatRepr::AnyIntConst(Some(c.get().inner)),
        Some(IntConstArg::One(v)) => PatRepr::IntConst(v as u128),
        // `as u128` is the scalar form's conversion: a negative carries its
        // 128-bit two's complement, which the core masks to the candidate's
        // width.  Narrowing to `u64` first would sign-extend only to 64 bits
        // and match a different constant above `I64`.
        Some(IntConstArg::Set(vs)) => {
            PatRepr::IntConstAnyOf(vs.into_iter().map(|v| v as u128).collect())
        }
    })
}

/// One value, or a set of them, for [`int_const_any_width`].
#[derive(FromPyObject)]
pub enum IntConstAnyWidthArg {
    #[pyo3(annotation = "int")]
    One(i128),
    #[pyo3(annotation = "list[int]")]
    Set(Vec<i128>),
}

/// Match an `IntConst` holding `value` however it was width-extended into the
/// constant's own type: exact, widened by zero extension, or widened by sign
/// extension. Given a list, any member of it. More permissive than
/// `int_const`, which is bit-exact at the output width.
///
/// A value outside the signed 64-bit range raises.
#[pyfunction]
pub fn int_const_any_width(value: IntConstAnyWidthArg) -> PyResult<PyPat> {
    Ok(PyPat::from_repr(match value {
        IntConstAnyWidthArg::One(v) => PatRepr::IntConstAnyWidth(checked_signed_i64(v)?),
        IntConstAnyWidthArg::Set(vs) => PatRepr::IntConstAnyWidthAnyOf(
            vs.into_iter()
                .map(checked_signed_i64)
                .collect::<PyResult<_>>()?,
        ),
    }))
}

/// One value, or a capture, for [`bool_const`].
#[derive(FromPyObject)]
pub enum BoolConstArg {
    #[pyo3(annotation = "bool")]
    One(bool),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match an `I1` boolean constant equal to `value`. Given a `Capture`, or no
/// argument at all, match any `I1` constant, binding it to the capture when
/// there is one.
#[pyfunction]
#[pyo3(signature = (value=None))]
pub fn bool_const(value: Option<BoolConstArg>) -> PyPat {
    PyPat::from_repr(match value {
        None => PatRepr::AnyBoolConst(None),
        Some(BoolConstArg::Cap(c)) => PatRepr::AnyBoolConst(Some(c.get().inner)),
        Some(BoolConstArg::One(v)) => PatRepr::BoolConst(v),
    })
}

/// The IEEE 754 bit pattern, or a capture, for [`float_const`].
#[derive(FromPyObject)]
pub enum FloatConstArg {
    #[pyo3(annotation = "int")]
    Bits(u64),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match a `FloatConst` whose raw bits equal `bits`. Given a `Capture`, or no
/// argument at all, match any float constant, binding it to the capture when
/// there is one.
#[pyfunction]
#[pyo3(signature = (bits=None))]
pub fn float_const(bits: Option<FloatConstArg>) -> PyPat {
    PyPat::from_repr(match bits {
        None => PatRepr::AnyFloatConst(None),
        Some(FloatConstArg::Cap(c)) => PatRepr::AnyFloatConst(Some(c.get().inner)),
        Some(FloatConstArg::Bits(b)) => PatRepr::FloatConst(b),
    })
}

/// Match any node with an integer value output (`I1` through `I512`),
/// optionally binding it to `c`. `int_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_int(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyInt(c.map(|c| c.inner)))
}

/// Match any node with a 1-bit (`I1`) value output, optionally binding it to
/// `c`. `bool_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_bool(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyBool(c.map(|c| c.inner)))
}

/// Match any node with a float value output (`F16` through `F128`), optionally
/// binding it to `c`. `float_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_float(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloat(c.map(|c| c.inner)))
}

/// Match any `InitialVar` node.
#[pyfunction]
pub fn initial_var() -> PyPat {
    PyPat::from_repr(PatRepr::InitialVar)
}

/// Match `InitialVar(vn)` for `vn` or any register containing it, so `eax`
/// matches a node tagged `rax`.
#[pyfunction]
pub fn initial_var_for(vn: crate::sleigh::PyVn) -> PyPat {
    PyPat::from_repr(PatRepr::InitialVarFor(vn.inner))
}

/// Alternation (logical OR): match if any listed sub-pattern matches. The dual
/// of a `find_all([...])` list, which is a logical AND. An arm is anything a
/// top-level pattern is, and the result nests in any slot: value, memory, or
/// control. Match-only. No alternatives matches nothing.
///
/// A union, not an ordered choice: every arm that matches is enumerated, each
/// with its own bindings, so the order of the alternatives carries no meaning.
/// Two overlapping arms both fire on a node they share, so a wildcard arm
/// (`anything()` / `var(c)`) contributes its own match rather than shadowing a
/// narrower arm; a join or `.when` downstream then keeps the binding it needs.
/// Use `first_of` for an ordered choice that cuts to the first matching arm.
///
/// Captures under an alternative that did not fire are left UNBOUND, not
/// defaulted, so `Match.has(c)` (or a `None` from `Match.uint_opt(c)`)
/// tells you which arm matched and lets you pick your own default:
///
/// ```python
/// offset = h.uint(off) if h.has(off) else 0
/// ```
#[pyfunction]
pub fn one_of(patterns: Vec<Py<PyAny>>) -> PyPat {
    PyPat::from_repr(PatRepr::OneOf(patterns, false))
}

/// Alternation with a first-match cut (ordered OR): match the first listed
/// sub-pattern that matches, ignoring the rest. Unlike `one_of` (a union),
/// overlapping arms do not each fire; the first arm that matches wins, so a
/// leading `anything()` arm shadows everything after it. Same generality as
/// `one_of`: any arm, any slot. Match-only. No alternatives matches nothing.
#[pyfunction]
pub fn first_of(patterns: Vec<Py<PyAny>>) -> PyPat {
    PyPat::from_repr(PatRepr::OneOf(patterns, true))
}

/// Match any node, subject to a Python predicate. Shorthand for
/// `anything().when(f)`.
#[pyfunction]
pub fn predicate(py: Python<'_>, f: PyObject) -> PyResult<PyPat> {
    let any = Py::new(py, PyPat::from_repr(PatRepr::Any))?.into_any();
    Ok(PyPat::from_repr(PatRepr::Guarded(any, f)))
}

// The canonical spelling each parser accepts is the op variant's `Debug`
// output, the same string `Match.op` reads back. Canonical names come from an
// exhaustive `match`, so adding a variant in strider-ir is a compile error
// here instead of a silent desync.

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
    // Hand-rolled rather than `op_parser!` because the two spellings do not
    // follow that macro's naming. The exhaustive match below is what couples it
    // to the enum: a third `ExtendOp` variant is a build error here, not a
    // variant silently unnameable from Python.
    const _: fn(strider_ir::ExtendOp) = |op| match op {
        strider_ir::ExtendOp::ZeroExtend | strider_ir::ExtendOp::SignExtend => {}
    };
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
            "int_const_any_width value {value} does not fit in i64 (the core candidate width)"
        ))
    })
}

value_ops!(PyPat, "Pattern", " Commutative.");

/// Pattern: integer `a != b`.
#[pyfunction]
pub fn int_ne(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntNe(l, r))
}

/// Pattern: unsigned `a <= b`.
#[pyfunction]
pub fn int_le(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntLe(l, r))
}

/// Pattern: signed `a <= b`.
#[pyfunction]
pub fn int_sle(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::IntSle(l, r))
}

/// Pattern: `x` is NaN, the IEEE 754 self-inequality `x != x`.
#[pyfunction]
pub fn float_is_nan(operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatIsNan(operand))
}

/// Pattern: float `a != b`.
#[pyfunction]
pub fn float_ne(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatNe(l, r))
}

/// Pattern: float `a <= b`, NaN-aware.
#[pyfunction]
pub fn float_le(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::FloatLe(l, r))
}

/// Match any `IntBinaryOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn any_int_binary(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyIntBinary(c.inner, l, r))
}

/// Match any `IntUnaryOp` over `operand` and bind the op variant to `c`.
#[pyfunction]
pub fn any_int_unary(c: PyRef<'_, PyCapture>, operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyIntUnary(c.inner, operand))
}

/// Match any `IntCmpOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn any_int_cmp(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyIntCmp(c.inner, l, r))
}

/// Match any boolean binary op over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn any_bool_binary(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyBoolBinary(c.inner, l, r))
}

/// Match any `FloatBinaryOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn any_float_binary(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloatBinary(c.inner, l, r))
}

/// Match any `FloatUnaryOp` over `operand` and bind the op variant to `c`.
#[pyfunction]
pub fn any_float_unary(c: PyRef<'_, PyCapture>, operand: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloatUnary(c.inner, operand))
}

/// Match any `FloatCmpOp` over `(l, r)` and bind the op variant to `c`.
#[pyfunction]
pub fn any_float_cmp(c: PyRef<'_, PyCapture>, l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    PyPat::from_repr(PatRepr::AnyFloatCmp(c.inner, l, r))
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
    /// `(raw slot, operand)` from `.input(i, p)`.
    inputs: Vec<(usize, Py<PyAny>)>,
    /// Operands from `.any_input(p)`, one existential slot each.
    any_inputs: Vec<Py<PyAny>>,
    outputs: Vec<OutputSpecPy>,
}

/// The one aspect a single `.output(slot)` / `.any_output()` call commits.
#[derive(Clone, Copy)]
enum OutputAspect {
    Capture(Capture),
    Width(u32),
    Ty(strider_ir::node::ValueType),
}

/// A sibling-output constraint staged on a builder; `slot` is `None` for the
/// existential `.any_output()`.
#[derive(Clone, Copy)]
struct OutputSpecPy {
    slot: Option<usize>,
    aspect: OutputAspect,
}

macro_rules! builder_common_methods {
    ($ty:ty) => {
        #[pymethods]
        impl $ty {
            /// Capture the matched node under `c`, a `Capture` or a name.
            fn capture<'py>(
                slf: PyRef<'py, Self>,
                c: crate::matcher::CaptureKey<'_>,
            ) -> PyResult<PyRef<'py, Self>> {
                slf.common.borrow_mut().capture = Some(c.resolve()?);
                Ok(slf)
            }
            /// Attach a Python predicate that runs after the match.
            fn when(slf: PyRef<'_, Self>, f: PyObject) -> PyRef<'_, Self> {
                slf.common.borrow_mut().when = Some(f);
                slf
            }
            /// Finalise into a `Pat`.
            fn into_pat(&self, py: Python<'_>) -> PyResult<PyPat> {
                let (pat, when_handles) = retain_when_handles(|| self.build_pattern_py(py))?;
                Ok(PyPat::from_repr(PatRepr::Finished {
                    pattern: Box::new(std::sync::Mutex::new(Some(pat))),
                    when_handles,
                }))
            }
            fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
                let name: String = slf.get_type().getattr("__name__")?.extract()?;
                Ok(format!("{name}(...)"))
            }

            /// Exposes the operand slots and the `.when()` predicate to the
            /// cyclic GC. `try_borrow`: a collection can land mid-setter, with
            /// the cell already held for mutation.
            fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
                if let Ok(common) = self.common.try_borrow() {
                    if let Some(f) = common.when.as_ref() {
                        visit.call(f)?;
                    }
                    for (_, p) in &common.inputs {
                        visit.call(p)?;
                    }
                    for p in &common.any_inputs {
                        visit.call(p)?;
                    }
                }
                self.traverse_operands(&visit)
            }

            /// Drops everything `__traverse__` reports. Without it these types
            /// carry `tp_traverse` and a NULL `tp_clear`, and CPython can only
            /// break a cycle at an object that HAS `tp_clear`: a cycle of
            /// builders alone would be found, skipped, and never freed.
            fn __clear__(&self) {
                if let Ok(mut common) = self.common.try_borrow_mut() {
                    common.when = None;
                    common.inputs.clear();
                    common.any_inputs.clear();
                }
                self.clear_operands();
            }
        }
    };
}

// The generic slot vocabulary beneath the named operand accessors, declared
// once per builder by naming the slot kinds it has rather than restating the
// methods. `any_input` / `input` / `output` are emitted here and applied from
// `CommonState`, so a builder opts in with one token.
//
// A builder declares only the kinds its node shape has: `EntryPat` has outputs
// and no inputs, the sinks (`RetPat` / `IndirectBranchPat` / `UnreachablePat`)
// the reverse.
macro_rules! builder_slot_methods {
    ($ty:ty, any_input) => {
        #[pymethods]
        impl $ty {
            /// Require SOME input of the node to match `p`, without pinning a
            /// slot. A typed sub binds an input of its own kind; `var` /
            /// `anything` also reaches the control, memory and phi-token
            /// edges. Repeatable, each call taking a distinct slot.
            fn any_input<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.common.borrow_mut().any_inputs.push(p);
                slf
            }
        }
    };
    ($ty:ty, input) => {
        #[pymethods]
        impl $ty {
            /// Match `p` against raw input slot `idx`. Slot 0 is not uniform
            /// across kinds: `Call` is `[ctrl, mem, target, sp, arg0, ...]`,
            /// `Load` is `[mem, addr]`, `If` is `[ctrl, cond]`. The IR's
            /// `expected_signature` (`strider-ir/src/node_signature.rs`) is
            /// the source of truth. This is the escape hatch beneath the named
            /// accessors, not a replacement for them.
            fn input<'py>(slf: PyRef<'py, Self>, idx: usize, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.common.borrow_mut().inputs.push((idx, p));
                slf
            }
        }
    };
    ($ty:ty, output) => {
        #[pymethods]
        impl $ty {
            /// Bind or constrain the value at raw output `slot`. Slot
            /// numbering is per kind and asymmetric with the inputs: a `Call`
            /// is `[ctrl, mem, result, ...clobbers]` while a `Load` is
            /// `[value]`. The IR's `expected_signature`
            /// (`strider-ir/src/node_signature.rs`) is the source of truth.
            /// Returns a terminal taking one of `.capture(c)`,
            /// `.of_width(w)`, `.of_type("i64")`.
            ///
            /// This names the output value itself; it does not recurse into
            /// whatever consumes that output.
            fn output(slf: Bound<'_, Self>, slot: usize) -> PyOutputSlot {
                PyOutputSlot {
                    parent: slf.into_any().unbind(),
                    slot: Some(slot),
                }
            }

            /// Some output rather than a fixed slot; otherwise `output`.
            fn any_output(slf: Bound<'_, Self>) -> PyOutputSlot {
                PyOutputSlot {
                    parent: slf.into_any().unbind(),
                    slot: None,
                }
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
//   * `mem` produces a memory token and exposes `compile_mem` for a `mem`
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
    (@members $inner:ident [ $($acc:tt)* ] { pat_list $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<Py<PyAny>>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { pattern $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
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
    (@members $inner:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar_clone $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar_inner $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { flag $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: bool, ] $($rest)*);
    };

    // Bound to a `let` first: an `if let` scrutinee keeps the `Ref` alive for
    // the body, where compiling the operand runs Python (`__index__`) that can
    // re-enter a setter and take `borrow_mut`.
    (@apply $self:ident, $py:ident, $b:ident, { pat $name:ident: $m:ident = $doc:literal }) => {
        let __slot = clone_opt($py, &$self.inner.borrow().$name);
        if let Some(__p) = __slot {
            $b = $b.$m(compile_operand_match(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { pat_list $name:ident: $m:ident = $doc:literal }) => {
        let __slot = clone_opt($py, &$self.inner.borrow().$name);
        if let Some(__p) = __slot {
            $b = $b.$m(compile_operand_match(__p.bind($py))?);
        }
    };
    // A whole nested `Pattern`, not a sub-pattern operand: the core matches it
    // at the node the slot forward-walks to.
    (@apply $self:ident, $py:ident, $b:ident, { pattern $name:ident: $m:ident = $doc:literal }) => {
        let __slot = clone_opt($py, &$self.inner.borrow().$name);
        if let Some(__p) = __slot {
            $b = $b.$m(pattern_for_operand(__p.bind($py))?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { mem $name:ident: $m:ident = $doc:literal }) => {
        let __slot = clone_opt($py, &$self.inner.borrow().$name);
        if let Some(__p) = __slot {
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
            $b = $b.$m(compile_any_input(__p.bind($py))?);
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
    (@apply $self:ident, $py:ident, $b:ident, { scalar $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar_clone $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name.clone() {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar_inner $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {
        if let Some(__v) = $self.inner.borrow().$name {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { flag $name:ident: $m:ident = $doc:literal }) => {
        if $self.inner.borrow().$name {
            $b = $b.$m();
        }
    };

    // The generic slot vocabulary, replayed from `CommonState` onto the core
    // builder. Guarded by the invocation's `slots:` list, since a core builder
    // without the slot kind has no method to call.
    (@apply_slot $self:ident, $py:ident, $b:ident, any_input) => {
        let __items: Vec<Py<PyAny>> = $self
            .common
            .borrow()
            .any_inputs
            .iter()
            .map(|p| p.clone_ref($py))
            .collect();
        for __p in __items {
            $b = $b.any_input(compile_any_input(__p.bind($py))?);
        }
    };
    (@apply_slot $self:ident, $py:ident, $b:ident, input) => {
        let __items: Vec<(usize, Py<PyAny>)> = $self
            .common
            .borrow()
            .inputs
            .iter()
            .map(|(i, p)| (*i, p.clone_ref($py)))
            .collect();
        for (__idx, __p) in __items {
            $b = $b.input(__idx, compile_operand_match(__p.bind($py))?);
        }
    };
    (@apply_slot $self:ident, $py:ident, $b:ident, output) => {
        let __specs: Vec<OutputSpecPy> = $self.common.borrow().outputs.clone();
        for __spec in __specs {
            let __slot = match __spec.slot {
                Some(__s) => $b.output(__s),
                None => $b.any_output(),
            };
            $b = match __spec.aspect {
                OutputAspect::Capture(__c) => __slot.capture(__c),
                OutputAspect::Width(__w) => __slot.of_width(__w),
                OutputAspect::Ty(__t) => __slot.of_type(__t),
            };
        }
    };

    (@traverse $inner:ident, $visit:ident, { pat $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = $inner.$name.as_ref() { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { pat_list $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = $inner.$name.as_ref() { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { pattern $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = $inner.$name.as_ref() { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { mem $name:ident: $m:ident = $doc:literal }) => {
        if let Some(__p) = $inner.$name.as_ref() { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { multi_pat $name:ident: $m:ident = $doc:literal }) => {
        for __p in &$inner.$name { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { multi_match $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        for (_, __p) in &$inner.$name { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { multi_mem $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        for (_, __p) in &$inner.$name { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { scalar $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@traverse $inner:ident, $visit:ident, { scalar_clone $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@traverse $inner:ident, $visit:ident, { scalar_inner $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@traverse $inner:ident, $visit:ident, { flag $name:ident: $m:ident = $doc:literal }) => {};

    // Mirrors `@traverse`: every slot reported there has to be droppable here,
    // or a cycle through it is uncollectable.
    (@clear $inner:ident, { pat $name:ident: $m:ident = $doc:literal }) => {
        $inner.$name = None;
    };
    (@clear $inner:ident, { pat_list $name:ident: $m:ident = $doc:literal }) => {
        $inner.$name = None;
    };
    (@clear $inner:ident, { pattern $name:ident: $m:ident = $doc:literal }) => {
        $inner.$name = None;
    };
    (@clear $inner:ident, { mem $name:ident: $m:ident = $doc:literal }) => {
        $inner.$name = None;
    };
    (@clear $inner:ident, { multi_pat $name:ident: $m:ident = $doc:literal }) => {
        $inner.$name.clear();
    };
    (@clear $inner:ident, { multi_match $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        $inner.$name.clear();
    };
    (@clear $inner:ident, { multi_mem $name:ident($idx:ty): $m:ident = $doc:literal }) => {
        $inner.$name.clear();
    };
    (@clear $inner:ident, { scalar $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@clear $inner:ident, { scalar_clone $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@clear $inner:ident, { scalar_inner $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal }) => {};
    (@clear $inner:ident, { flag $name:ident: $m:ident = $doc:literal }) => {};

    // pyo3's proc-macro cannot see through a nested macro item, so the setters
    // cannot be `node_builder!(@setter ...)` calls inside `#[pymethods]`.
    // Instead the field list is munched one head at a time into an
    // accumulator, and the whole impl is emitted once the list empties.
    (@setters $ty:ident [ $($acc:tt)* ] ) => {
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
    (@setters $ty:ident [ $($acc:tt)* ] { pat_list $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(
                slf: PyRef<'py, Self>,
                py: Python<'_>,
                p: Py<PyAny>,
            ) -> PyResult<PyRef<'py, Self>> {
                slf.inner.borrow_mut().$name = Some(coerce_operand_list(py, p)?);
                Ok(slf)
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { pattern $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
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
    (@setters $ty:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, $arg: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some($arg);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar_clone $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, $arg: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some($arg);
                slf
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar_inner $name:ident($set:ty => $store:ty): $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, $arg: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some($arg.inner);
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

    (@compile_mem) => {
        /// Compile as a memory-token producer for a `mem` slot,
        /// honouring `.when()`.
        fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
            let b = self.core_builder(py)?;
            let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
            Ok(DynMem(Box::new(move |mb| mem_with_when(b, when, mb))))
        }
    };
    (@compile_value $doc:literal) => {
        #[doc = $doc]
        fn compile_value(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let b = self.core_builder(py)?;
            let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
            Ok(match when {
                Some(f) => DynMatch(Box::new(move |mb| wrap_when(b, f).compile(mb))),
                None => DynMatch(Box::new(move |mb| b.compile(mb))),
            })
        }
    };
    (@build_from_core) => {
        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            let pat = self.core_builder(py)?.build();
            Ok(apply_when_to_pattern(py, &self.common.borrow(), pat))
        }
    };
    (@flavor value $py_name:literal $core:path) => {
        node_builder!(@compile_value
            "Compile as a value operand (`MatchPat`), honouring `.when()`.");

        fn build_pattern_py(&self, py: Python<'_>) -> PyResult<Pattern> {
            Ok(self.compile_value(py)?.into_pattern())
        }
    };
    (@flavor mem $py_name:literal $core:path) => {
        node_builder!(@compile_mem);
        node_builder!(@build_from_core);
    };
    (@flavor mem_value $py_name:literal $core:path) => {
        node_builder!(@compile_mem);
        node_builder!(@compile_value
            "Nest as a value operand, e.g. `int_add(x, call_other().name(\"f\"))`. \
             Loose by default: any value output, narrowed by `.res()`.");
        node_builder!(@build_from_core);
    };
    (@flavor node $py_name:literal $core:path) => {
        node_builder!(@build_from_core);

        /// Compile as a `one_of` / `first_of` arm: node-rooted, so the core
        /// synthesizes an `Any` output for the alternation to wire.
        fn compile_alt_arm(&self, py: Python<'_>) -> PyResult<DynMatch> {
            let b = self.core_builder(py)?;
            let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
            Ok(DynMatch(Box::new(move |mb| alt_arm_with_when(mc(b, mb), mb, when))))
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
        slots: [ $( $slot:ident ),* $(,)? ],
        fields: [ $( $field:tt ),* $(,)? ] $(,)?
    ) => {
        node_builder!(@members $inner [] $($field)*);

        #[doc = $doc]
        #[pyclass(name = $py_name, module = "strider.pattern")]
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
                $( node_builder!(@apply_slot self, py, b, $slot); )*
                if let Some(c) = self.common.borrow().capture {
                    b = b.capture(c);
                }
                Ok(b)
            }

            /// The operand slots, for `__traverse__`.
            fn traverse_operands(
                &self,
                visit: &pyo3::PyVisit<'_>,
            ) -> Result<(), pyo3::PyTraverseError> {
                // Both are unused for a field-less builder such as `EntryPat`.
                let _ = visit;
                if let Ok(__inner) = self.inner.try_borrow() {
                    let _ = &__inner;
                    $( node_builder!(@traverse __inner, visit, $field); )*
                }
                Ok(())
            }

            /// Drops the operand slots, for `__clear__`.
            fn clear_operands(&self) {
                if let Ok(mut __inner) = self.inner.try_borrow_mut() {
                    let _ = &mut __inner;
                    $( node_builder!(@clear __inner, $field); )*
                }
            }

            node_builder!(@flavor $root $py_name $core);
        }

        node_builder!(@setters $ty [] $($field)*);
        builder_common_methods!($ty);
        $( builder_slot_methods!($ty, $slot); )*
    };
}

node_builder! {
    ty: PyLoadPat,
    inner: LoadInner,
    py_name: "LoadPat",
    doc: "Typed builder for `Load` node patterns. Chain `.addr(p)`, \
          `.space(s)`, `.mem(m)`, `.bit_width(n)`, `.stack_only()`.",
    core: strider_pattern::load,
    core_ty: strider_pattern::LoadPat,
    root: value,
    slots: [any_input, input, output],
    fields: [
        { scalar_inner space(crate::sleigh::PyVnSpace => rsleigh::VnSpace): space(s)
            = "Restrict the match to a specific memory space." },
        { pat addr: addr = "Constrain the load's address operand." },
        { mem mem: mem
            = "Constrain the load's memory predecessor (a memory-producing sub-pattern)." },
        { scalar bit_width(u32 => u32): bit_width(n) = "Filter loads by value width in bits." },
        { scalar stack_offset(i128 => i128): stack_offset(k)
            = "Match only loads whose address decomposes to exactly `sp + k`." },
        { flag stack_only: stack_only
            = "Keep only accesses whose address decomposes to a stack base, an \
               offset pinned by `stack_offset` included." },
        { flag non_stack: non_stack
            = "Keep only accesses proven heap-rooted or proven not memory-rooted; \
               an address with no decomposition verdict is rejected." },
        { flag heap_only: heap_only
            = "Keep only accesses whose address decomposes to a heap base \
               (a pure allocator's return pointer)." },
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
    slots: [any_input, input, output],
    fields: [
        { pat addr: addr = "Constrain the store's address operand." },
        { pat data: data = "Constrain the store's stored-value operand." },
        { scalar_inner space(crate::sleigh::PyVnSpace => rsleigh::VnSpace): space(s)
            = "Restrict the match to a specific memory space." },
        { mem mem: mem = "Constrain the store's memory predecessor." },
        { scalar bit_width(u32 => u32): bit_width(n) = "Filter stores by data width in bits." },
        { scalar stack_offset(i128 => i128): stack_offset(k)
            = "Match only stores whose address decomposes to exactly `sp + k`." },
        { flag stack_only: stack_only
            = "Keep only accesses whose address decomposes to a stack base, an \
               offset pinned by `stack_offset` included." },
        { flag non_stack: non_stack
            = "Keep only accesses proven heap-rooted or proven not memory-rooted; \
               an address with no decomposition verdict is rejected." },
        { flag heap_only: heap_only
            = "Keep only accesses whose address decomposes to a heap base \
               (a pure allocator's return pointer)." },
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
    ctrl: Option<Py<PyAny>>,
    args: Vec<(usize, Py<PyAny>)>,
    mem: Option<Py<PyAny>>,
    /// Pins a nested value operand to the declared result output, excluding
    /// caller-saved clobber outputs.
    res: bool,
}

/// Typed builder for `Call` node patterns. Chain `.target(p)`,
/// `.arg(idx, p)`, `.mem(m)`, `.ctrl(p)`.
#[pyclass(name = "CallPat", module = "strider.pattern")]
pub struct PyCallPat {
    inner: std::cell::RefCell<CallInner>,
    common: std::cell::RefCell<CommonState>,
}

impl PyCallPat {
    fn new() -> Self {
        Self {
            inner: std::cell::RefCell::new(CallInner::default()),
            common: std::cell::RefCell::new(CommonState::default()),
        }
    }

    fn core_builder(&self, py: Python<'_>) -> PyResult<strider_pattern::CallPat> {
        let mut b = strider_pattern::call();
        // `let` first: compiling an operand runs Python that can re-enter a
        // setter, which takes `borrow_mut`.
        let target = clone_opt(py, &self.inner.borrow().target);
        if let Some(t) = target {
            b = b.target(compile_operand_match(t.bind(py))?);
        }
        let ctrl = clone_opt(py, &self.inner.borrow().ctrl);
        if let Some(c) = ctrl {
            b = b.ctrl(compile_operand_match(c.bind(py))?);
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
        let mem = clone_opt(py, &self.inner.borrow().mem);
        if let Some(m) = mem {
            b = b.mem(compile_operand_mem(m.bind(py))?);
        }
        if self.inner.borrow().res {
            b = b.res();
        }
        node_builder!(@apply_slot self, py, b, any_input);
        node_builder!(@apply_slot self, py, b, input);
        node_builder!(@apply_slot self, py, b, output);
        if let Some(c) = self.common.borrow().capture {
            b = b.capture(c);
        }
        Ok(b)
    }

    /// The operand slots, for `__traverse__`.
    fn traverse_operands(&self, visit: &pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        let Ok(inner) = self.inner.try_borrow() else {
            return Ok(());
        };
        for p in [&inner.target, &inner.ctrl, &inner.mem]
            .into_iter()
            .flatten()
        {
            visit.call(p)?;
        }
        for (_, p) in &inner.args {
            visit.call(p)?;
        }
        Ok(())
    }

    /// Drops everything `traverse_operands` reports, for `__clear__`.
    fn clear_operands(&self) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        inner.target = None;
        inner.ctrl = None;
        inner.mem = None;
        inner.args.clear();
    }

    /// Honours `.when()`, like the macro-generated memory flavours.
    fn compile_mem(&self, py: Python<'_>) -> PyResult<DynMem> {
        let b = self.core_builder(py)?;
        let when = self.common.borrow().when.as_ref().map(|f| f.clone_ref(py));
        Ok(DynMem(Box::new(move |mb| mem_with_when(b, when, mb))))
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

#[pymethods]
impl PyCallPat {
    /// Constrain the call target. `p` is any pattern operand, including a raw
    /// int, which matches a call to that literal address (`target(0x1000)`).
    /// A list of them matches a call to any one entry; an empty list matches
    /// nothing.
    fn target<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'_>,
        p: Py<PyAny>,
    ) -> PyResult<PyRef<'py, Self>> {
        slf.inner.borrow_mut().target = Some(coerce_operand_list(py, p)?);
        Ok(slf)
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
    /// Match `p` against the Call's direct ctrl predecessor (`inputs[0]`).
    fn ctrl<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.borrow_mut().ctrl = Some(p);
        slf
    }
    /// When nested as a value operand, pin it to the declared result output,
    /// excluding caller-saved clobbers.
    fn res(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf.inner.borrow_mut().res = true;
        slf
    }
}
builder_common_methods!(PyCallPat);
builder_slot_methods!(PyCallPat, any_input);
builder_slot_methods!(PyCallPat, input);
builder_slot_methods!(PyCallPat, output);

/// Returned by `.output(slot)` / `.any_output()`. Each terminal commits one
/// constraint onto the parent builder and hands it back so the chain
/// continues.
#[pyclass(name = "OutputSlotPat", module = "strider.pattern")]
pub struct PyOutputSlot {
    parent: Py<PyAny>,
    slot: Option<usize>,
}

// The parent is held untyped, so committing dispatches over the builders that
// expose `output`. Listed once here rather than one terminal type per builder.
macro_rules! output_slot_parents {
    ($($ty:ty),* $(,)?) => {
        impl PyOutputSlot {
            fn commit(&self, py: Python<'_>, aspect: OutputAspect) -> PyResult<Py<PyAny>> {
                let spec = OutputSpecPy {
                    slot: self.slot,
                    aspect,
                };
                let parent = self.parent.bind(py);
                $(
                    if let Ok(b) = parent.downcast::<$ty>() {
                        b.borrow().common.borrow_mut().outputs.push(spec);
                        return Ok(self.parent.clone_ref(py));
                    }
                )*
                Err(pyo3::exceptions::PyTypeError::new_err(
                    "output slot terminal holds a parent that is not a pattern builder",
                ))
            }
        }
    };
}

output_slot_parents!(
    PyCallPat,
    PyCallOtherPat,
    PyLoadPat,
    PyStorePat,
    PyIfPat,
    PySwitchPat,
    PyPhiPat,
    PyMemPhiPat,
    PyEntryPat,
    PyRegionPat,
);

#[pymethods]
impl PyOutputSlot {
    fn __repr__(&self) -> String {
        match self.slot {
            Some(slot) => format!("OutputSlotPat(slot={slot})"),
            None => "OutputSlotPat(any)".to_string(),
        }
    }

    /// Bind the sibling output's value to `c`, a `Capture` or a name.
    fn capture(&self, py: Python<'_>, c: crate::matcher::CaptureKey<'_>) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Capture(c.resolve()?))
    }

    /// Constrain the sibling output to bit width `bits`.
    fn of_width(&self, py: Python<'_>, bits: u32) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Width(bits))
    }

    /// Constrain the sibling output to the type named by `ty`, e.g. `"i64"`.
    fn of_type(&self, py: Python<'_>, ty: &str) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Ty(parse_value_ty(ty)?))
    }
}

#[pymethods]
impl PyOutputSlot {
    /// The parent handle is a `Py`, invisible to the collector without this.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.parent)
    }
}

/// Start a `Call` pattern builder.
#[pyfunction]
pub fn call() -> PyCallPat {
    PyCallPat::new()
}

node_builder! {
    ty: PyCallOtherPat,
    inner: CallOtherInner,
    py_name: "CallOtherPat",
    doc: "Typed builder for `CallOther` node patterns.",
    core: strider_pattern::call_other,
    core_ty: strider_pattern::CallOtherPat,
    root: mem_value,
    slots: [any_input, input, output],
    fields: [
        { scalar user_op_id(u64 => u64): user_op_id(v)
            = "Constrain the matched node's user-op id." },
        { scalar_clone name(String => String): name(n)
            = "Constrain the matched node's user-op name." },
        { pat ctrl: ctrl
            = "Match the CallOther's control predecessor (`inputs[0]`)." },
        { mem mem: mem
            = "Match the CallOther's memory predecessor (`inputs[1]`); \
               takes a memory producer (store / mem_phi / call / call_other)." },
        { multi_match args(usize): arg
            = "Constrain raw `inputs[idx]` of the matched CallOther." },
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
    slots: [any_input, input],
    fields: [
        { pat ctrl: ctrl
            = "Match `p` against the Return's direct ctrl predecessor (`inputs[0]`)." },
        { multi_match ret_vals(usize): ret_val
            = "Constrain the return value at position `idx`, raw input slot `idx + 2`." },
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
    slots: [any_input, input],
    fields: [
        { pat_list target: target
            = "Constrain the dispatch target (`inputs[2]`). `p` is any pattern \
               operand, including a raw int, which matches a branch to that \
               literal address. A list of them matches any one entry; an empty \
               list matches nothing." },
        { pat ctrl: ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
        { mem mem: mem
            = "Constrain the node's memory predecessor (`inputs[1]`)." },
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
    slots: [any_input, input],
    fields: [
        { pat ctrl: ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
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
    slots: [any_input, input, output],
    fields: [
        { pat_list selector: selector
            = "The value the switch dispatches on (`inputs[1]`). The arms' \
               addresses are the control outputs, not this slot. `p` is any \
               pattern operand, including a raw int, which matches that literal \
               value. A list of them matches any one entry; an empty list \
               matches nothing." },
        { pat ctrl: ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
    ],
}

/// Start a `Switch` pattern builder.
#[pyfunction]
pub fn switch() -> PySwitchPat {
    PySwitchPat::new()
}

node_builder! {
    ty: PyIfPat,
    inner: IfInner,
    py_name: "IfPat",
    doc: "Typed builder for `If` node patterns.",
    core: strider_pattern::if_else,
    core_ty: strider_pattern::IfPat,
    root: node,
    slots: [any_input, input, output],
    fields: [
        { pat cond: cond = "Constrain the If's condition operand." },
        { pat ctrl: ctrl
            = "Match `p` against the If's direct ctrl predecessor (`inputs[0]`)." },
        { pattern true_branch: with_true
            = "Match the unique consumer of the If's true output." },
        { pattern false_branch: with_false
            = "Match the unique consumer of the If's false output." },
        { scalar_inner capture_true(PyCapture => Capture): capture_true(c)
            = "Bind the If's true control-output value to `c`, the edge \
               operand `dominates` / `phi_input_from_edge` take." },
        { scalar_inner capture_false(PyCapture => Capture): capture_false(c)
            = "Bind the If's false control-output value to `c`. See \
               `capture_true`." },
    ],
}

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
#[pyclass(name = "JoinConstraint", module = "strider.pattern.constraints")]
pub struct PyJoinConstraint {
    pub(crate) inner: Arc<ConstraintTree>,
    /// The constraints this one was built from. `inner` reaches a
    /// `JoinPredicate` only through a Rust closure, so these keep the edge
    /// visible to the cyclic GC.
    operands: Vec<PyObject>,
    /// The `JoinPredicate` handles the closures in `inner` run, each shared
    /// with its closure so the pair is one reference reported once.
    predicates: Vec<PredicateFn>,
}

/// A `JoinPredicate` instance baked into a `Where` closure.
type PredicateFn = Arc<PyObject>;

/// Well above any realistic constraint expression.
const MAX_CONSTRAINT_NESTING: u32 = 512;

/// Cap on the materialised node count, which repeated sharing
/// (`all_of([c, c])`) grows exponentially in the depth.
const MAX_CONSTRAINT_NODES: u64 = 65_536;

/// A composed constraint, shared by `Rc` rather than copied at every wrap and
/// materialised into a `JoinConstraint` once, at the query boundary.
pub(crate) struct ConstraintTree {
    node: ConstraintNode,
    /// `1` for a leaf, one more than the deepest child otherwise.
    depth: u32,
    /// Materialised node count, saturating.
    size: u64,
}

enum ConstraintNode {
    Leaf(JoinConstraint),
    Not(Arc<ConstraintTree>),
    Any(Vec<Arc<ConstraintTree>>),
    All(Vec<Arc<ConstraintTree>>),
}

impl ConstraintTree {
    fn new(node: ConstraintNode) -> PyResult<Arc<Self>> {
        let children: &[Arc<Self>] = match &node {
            ConstraintNode::Leaf(_) => &[],
            ConstraintNode::Not(c) => std::slice::from_ref(c),
            ConstraintNode::Any(cs) | ConstraintNode::All(cs) => cs,
        };
        let depth = 1 + children.iter().map(|c| c.depth).max().unwrap_or(0);
        let size = children
            .iter()
            .fold(1u64, |acc, c| acc.saturating_add(c.size));
        if depth > MAX_CONSTRAINT_NESTING {
            return Err(into_strider_err(anyhow::anyhow!(
                "constraint nesting too deep (max {MAX_CONSTRAINT_NESTING})"
            )));
        }
        if size > MAX_CONSTRAINT_NODES {
            return Err(into_strider_err(anyhow::anyhow!(
                "constraint expands to more than {MAX_CONSTRAINT_NODES} nodes"
            )));
        }
        Ok(Arc::new(Self { node, depth, size }))
    }

    fn materialize(&self) -> JoinConstraint {
        let all = |cs: &[Arc<Self>]| cs.iter().map(|c| c.materialize()).collect();
        match &self.node {
            ConstraintNode::Leaf(c) => c.clone(),
            ConstraintNode::Not(c) => JoinConstraint::Not(Box::new(c.materialize())),
            ConstraintNode::Any(cs) => JoinConstraint::Or(all(cs)),
            ConstraintNode::All(cs) => JoinConstraint::And(all(cs)),
        }
    }
}

impl PyJoinConstraint {
    fn leaf(inner: JoinConstraint) -> Self {
        Self {
            // A leaf is depth 1 and size 1, under both bounds.
            inner: ConstraintTree::new(ConstraintNode::Leaf(inner)).expect("leaf is within bounds"),
            operands: Vec::new(),
            predicates: Vec::new(),
        }
    }
}

#[pymethods]
impl PyJoinConstraint {
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        for o in &self.operands {
            visit.call(o)?;
        }
        for f in &self.predicates {
            visit.call(&**f)?;
        }
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!("JoinConstraint({:?})", self.inner.materialize())
    }
}

/// `a` dominates `b`: every control-flow path from entry to `b` passes through
/// `a`. Both are captured NODES (any capture) or an `If` branch-EDGE capture
/// (`capture_true` / `capture_false`), so the meaningful pairs are node->node
/// (`b` is downstream of `a`), edge->node (`b` sits in the block that edge
/// leads into), and edge->edge (nested branches).
///
/// This is dominance, not "reachable from". A merge or loop-header `Phi` is
/// reached from several predecessors, so NO single incoming edge dominates it:
/// `dominates(false_edge, phi)` is false there and drops the tuple. To say
/// "the value `phi` merges from that edge is X", use `phi_input_from_edge`.
///
/// Only a capture with a control edge can be placed. A `load`, `store` or
/// arithmetic capture has no position in the dominator tree, so the constraint
/// drops the tuple and so does `negate(dominates(...))`.
#[pyfunction]
pub fn dominates(a: PyRef<'_, PyCapture>, b: PyRef<'_, PyCapture>) -> PyJoinConstraint {
    PyJoinConstraint::leaf(JoinConstraint::Dominates {
        dominator: a.inner,
        dominated: b.inner,
    })
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
    PyJoinConstraint::leaf(JoinConstraint::PhiInputFromEdge {
        phi: phi.inner,
        edge: edge.inner,
        value: value.inner,
    })
}

/// The negation of `c`: a match survives only if `c` does not hold. `c` is a
/// built-in `JoinConstraint` or a `JoinPredicate` instance.
#[pyfunction]
pub fn negate(c: &Bound<'_, PyAny>) -> PyResult<PyJoinConstraint> {
    let mut predicates = Vec::new();
    let inner = ConstraintTree::new(ConstraintNode::Not(coerce_tree(c, &mut predicates)?))?;
    Ok(PyJoinConstraint {
        inner,
        operands: vec![c.clone().unbind()],
        predicates,
    })
}

/// The coerced operands, the constraint objects themselves, and any predicate
/// handle a `Where` closure captured.
type CoercedConstraints = (Vec<Arc<ConstraintTree>>, Vec<PyObject>, Vec<PredicateFn>);

/// Coerces every listed constraint, collecting the handles the composite ends
/// up owning.
fn coerce_all(constraints: &[Bound<'_, PyAny>]) -> PyResult<CoercedConstraints> {
    let mut predicates = Vec::new();
    let coerced = constraints
        .iter()
        .map(|c| coerce_tree(c, &mut predicates))
        .collect::<PyResult<_>>()?;
    let operands = constraints.iter().map(|c| c.clone().unbind()).collect();
    Ok((coerced, operands, predicates))
}

/// A constraint that passes when ANY of the listed constraints passes. An
/// empty list passes nothing.
///
/// The top-level `constraints=[...]` list is already an AND, so `any_of` is
/// how you express OR.
#[pyfunction]
pub fn any_of(constraints: Vec<Bound<'_, PyAny>>) -> PyResult<PyJoinConstraint> {
    let (coerced, operands, predicates) = coerce_all(&constraints)?;
    Ok(PyJoinConstraint {
        inner: ConstraintTree::new(ConstraintNode::Any(coerced))?,
        operands,
        predicates,
    })
}

/// A constraint that passes only when EVERY listed constraint passes. An
/// empty list passes everything. Use it to AND constraints inside an `any_of`
/// (the top-level list does not nest).
#[pyfunction]
pub fn all_of(constraints: Vec<Bound<'_, PyAny>>) -> PyResult<PyJoinConstraint> {
    let (coerced, operands, predicates) = coerce_all(&constraints)?;
    Ok(PyJoinConstraint {
        inner: ConstraintTree::new(ConstraintNode::All(coerced))?,
        operands,
        predicates,
    })
}

/// Base class for a user-defined join constraint. Subclass and override
/// `constraint` to decide whether a joined match survives; optionally override
/// `captures` to declare the captures it correlates, which lets it connect
/// otherwise-independent patterns and range-check like a built-in constraint.
#[pyclass(
    name = "JoinPredicate",
    module = "strider.pattern.constraints",
    subclass
)]
pub struct PyJoinPredicate;

#[pymethods]
impl PyJoinPredicate {
    /// Base initialiser; ignores any args so subclasses can call
    /// `super().__init__(...)` freely.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        Self
    }

    /// The captures this predicate correlates. The base returns none (a pure
    /// filter); override to make the predicate a connector too.
    fn captures(&self) -> Vec<Py<PyCapture>> {
        Vec::new()
    }

    /// Override to return whether the joined match survives. The base raises
    /// `NotImplementedError`.
    #[allow(unused_variables)]
    fn constraint(&self, m: &Bound<'_, PyAny>) -> PyResult<bool> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "JoinPredicate.constraint must be overridden by subclass",
        ))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        let name: String = slf.get_type().getattr("__name__")?.extract()?;
        Ok(format!("{name}()"))
    }
}

/// A built-in `JoinConstraint` used as-is, or a `JoinPredicate` subclass
/// wrapped into a `Where` (its declared `captures()` read once, its
/// `constraint` method wired to run per tuple). Anything else is a `TypeError`.
///
/// The `Where` closure's handle is shared onto `held`, so whoever keeps the
/// constraint reports that one reference to the cyclic GC.
pub(crate) fn coerce_join_constraint(
    obj: &Bound<'_, PyAny>,
    held: &mut Vec<PredicateFn>,
) -> PyResult<JoinConstraint> {
    Ok(coerce_tree(obj, held)?.materialize())
}

/// [`coerce_join_constraint`] without the materialisation, so a composite
/// shares its operand instead of copying it.
fn coerce_tree(
    obj: &Bound<'_, PyAny>,
    held: &mut Vec<PredicateFn>,
) -> PyResult<Arc<ConstraintTree>> {
    if let Ok(c) = obj.downcast::<PyJoinConstraint>() {
        return Ok(Arc::clone(&c.borrow().inner));
    }
    if obj.is_instance_of::<PyJoinPredicate>() {
        let caps: Vec<PyRef<'_, PyCapture>> = obj.call_method0("captures")?.extract()?;
        let captures = caps.iter().map(|c| c.inner).collect();
        let handle: PredicateFn = Arc::new(obj.clone().unbind());
        held.push(Arc::clone(&handle));
        let pred: sp::JoinPredicateFn =
            Arc::new(move |_f, tuple| run_join_predicate(tuple, &handle));
        return ConstraintTree::new(ConstraintNode::Leaf(JoinConstraint::Where {
            captures,
            pred,
        }));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "constraints must be JoinConstraint or JoinPredicate instances",
    ))
}

/// Runs a `JoinPredicate.constraint` over the whole joined tuple. It sees one
/// `Match` spanning the join, so `m.uint(c)` (and every other reader) finds
/// `c` in whichever pattern bound it. Any exception is stashed for the query
/// boundary to re-raise; the row drops meanwhile.
fn run_join_predicate(tuple: &sp::JoinedMatch, obj: &PyObject) -> Option<bool> {
    Python::with_gil(|py| {
        if peek_pending_query_error() {
            return None;
        }
        let Some((function, generation)) = current_query_function(py) else {
            eprintln!(
                "strider JoinPredicate: no active query function on this thread \
                 (internal error); dropping the row"
            );
            return None;
        };
        let verdict = Py::new(
            py,
            crate::matcher::PyMatch {
                inner: tuple.clone(),
                function,
                generation,
            },
        )
        .and_then(|py_match| obj.call_method1(py, "constraint", (py_match,)))
        .and_then(|r| r.extract::<bool>(py));
        match verdict {
            Ok(b) => Some(b),
            Err(e) => {
                stash_pending_query_error(e);
                None
            }
        }
    })
}

node_builder! {
    ty: PyPhiPat,
    inner: PhiInner,
    py_name: "PhiPat",
    doc: "Typed builder for tagged-`Phi` patterns.",
    core: strider_pattern::phi,
    core_ty: strider_pattern::PhiPat,
    root: value,
    slots: [any_input, input, output],
    fields: [
        { scalar_inner for_vn(crate::sleigh::PyVn => rsleigh::Vn): for_vn(vn)
            = "Restrict the match to phi nodes tagged `vn` or a register containing \
               it, so `eax` matches a phi tagged `rax`." },
        { multi_match inputs(usize): phi_input
            = "Constrain the value arriving from predecessor `idx`, raw input \
               slot `idx + 1`. `.input(i, p)` addresses the raw slot instead." },
        { pat phi_token: phi_token
            = "Constrain the phi's ownership edge, the PhiToken input at raw \
               slot 0 (the owning Region's PhiToken output), which \
               `.input(0, p)` also names. A typed sub can never bind it \
               (PhiToken falls outside the value domain a typed sub matches); \
               use var()/anything() to bind the edge." },
    ],
}

/// Start a tagged-`Phi` pattern builder.
#[pyfunction]
pub fn phi() -> PyPhiPat {
    PyPhiPat::new()
}

/// Match a `Phi` tagged `vn` or a register containing it.
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
    slots: [any_input, input, output],
    fields: [
        { multi_mem inputs(usize): phi_input
            = "Constrain the memory token arriving from predecessor `idx`, raw \
               input slot `idx + 1`. `.input(i, p)` addresses the raw slot \
               instead, and takes a value operand." },
        { pat phi_token: phi_token
            = "Constrain the MemPhi's ownership edge, the PhiToken input at \
               raw slot 0 (the owning Region's PhiToken output), which \
               `.input(0, p)` also names. See PhiPat.phi_token for the \
               value-phi analogue." },
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
          `Entry` takes no inputs and produces one control output, the \
          function's initial control edge. Nests as a control operand, e.g. \
          `region().any_input(entry())`.",
    core: strider_pattern::entry,
    core_ty: strider_pattern::EntryPat,
    root: value,
    slots: [output],
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
    doc: "Typed builder for `Region` (CFG-merge) node patterns. Every input \
          is one predecessor's control edge, at raw slots `0..N`, so \
          `.input(idx, p)` / `.any_input(p)` constrain a predecessor \
          directly. A typed value sub can never bind a Control edge; use \
          entry() / region() or an untyped wildcard. Nests as a control \
          operand, e.g. `region().any_input(region())`.",
    core: strider_pattern::region,
    core_ty: strider_pattern::RegionPat,
    root: value,
    slots: [any_input, input, output],
    fields: [],
}

/// Matches any CFG-merge `Region` node.
#[pyfunction]
pub fn region() -> PyRegionPat {
    PyRegionPat::new()
}

/// Typed builder for `FunctionArg` carrier patterns. Chain `.index(i)`,
/// `.source_register(vn)`, `.source_stack(space, offset)`.
#[pyclass(name = "FunctionArgPat", module = "strider.pattern")]
pub struct PyFunctionArgPat {
    source: std::cell::RefCell<Option<strider_ir::node::FunctionArgSource>>,
    /// Which index space `index` names; `Any` unless a constructor pinned it.
    class: std::cell::RefCell<strider_pattern::FunctionArgClass>,
    index: std::cell::RefCell<Option<u32>>,
    common: std::cell::RefCell<CommonState>,
}

impl PyFunctionArgPat {
    fn new() -> Self {
        Self {
            source: std::cell::RefCell::new(None),
            class: std::cell::RefCell::new(strider_pattern::FunctionArgClass::Any),
            index: std::cell::RefCell::new(None),
            common: std::cell::RefCell::new(CommonState::default()),
        }
    }

    /// No operand slots; `__traverse__` still needs the hook.
    fn traverse_operands(&self, visit: &pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        let _ = visit;
        Ok(())
    }

    /// No operand slots; `__clear__` still drops `.when()`.
    fn clear_operands(&self) {}

    fn core_builder(&self) -> strider_pattern::FunctionArgPat {
        let mut b = strider_pattern::any_function_arg().class(*self.class.borrow());
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

/// Start a `FunctionArg` pattern builder at integer-class index `i`.
#[pyfunction]
pub fn function_arg(i: u32) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.class.replace(strider_pattern::FunctionArgClass::Integer);
    b.index.replace(Some(i));
    b
}

/// Start a `FunctionArg` pattern builder at float-class index `i`, counting
/// only float parameters.
#[pyfunction]
pub fn function_arg_float(i: u32) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.class.replace(strider_pattern::FunctionArgClass::Float);
    b.index.replace(Some(i));
    b
}

/// Start a `FunctionArg` pattern builder matching any index of either class.
#[pyfunction]
pub fn any_function_arg() -> PyFunctionArgPat {
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
        #[pyclass(name = $py_name, module = "strider.pattern")]
        pub struct $ty {
            op: $op_ty,
            lhs: Py<PyAny>,
            rhs: Py<PyAny>,
            ordered: std::cell::Cell<bool>,
            common: std::cell::RefCell<CommonState>,
        }

        impl $ty {
            /// The operand slots, for `__traverse__`.
            fn traverse_operands(
                &self,
                visit: &pyo3::PyVisit<'_>,
            ) -> Result<(), pyo3::PyTraverseError> {
                visit.call(&self.lhs)?;
                visit.call(&self.rhs)
            }

            /// `lhs`/`rhs` are fixed at construction, so a cycle cannot close
            /// through them; only `.when()` can capture this builder, and
            /// `__clear__` drops that.
            fn clear_operands(&self) {}

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
    m.add_class::<PyOutputSlot>()?;
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
    add_fn!(inputs_of_width);
    add_fn!(bool_inputs);
    add_fn!(int_const);
    add_fn!(int_const_any_width);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(any_int);
    add_fn!(any_bool);
    add_fn!(any_float);
    add_fn!(initial_var);
    add_fn!(initial_var_for);
    add_fn!(one_of);
    add_fn!(first_of);
    add_fn!(function_arg);
    add_fn!(any_function_arg);
    add_fn!(function_arg_float);
    add_fn!(function_arg_reg);
    add_fn!(function_arg_stack);
    add_fn!(phi);
    add_fn!(phi_for);
    add_fn!(mem_phi);
    add_fn!(entry);
    add_fn!(region);
    add_fn!(predicate);
    register_value_ops(m)?;
    add_fn!(int_ne);
    add_fn!(int_le);
    add_fn!(int_sle);
    add_fn!(float_is_nan);
    add_fn!(float_ne);
    add_fn!(float_le);
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
    add_fn!(any_int_binary);
    add_fn!(any_int_unary);
    add_fn!(any_int_cmp);
    add_fn!(any_bool_binary);
    add_fn!(any_float_binary);
    add_fn!(any_float_unary);
    add_fn!(any_float_cmp);

    register_constraints(py, m)?;
    Ok(())
}

/// `parent` must be the `pattern` module so the `sys.modules` key is the full
/// dotted path. Without that, `from strider.pattern import constraints` fails
/// even though attribute access works.
fn register_constraints(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "constraints")?;
    m.add_class::<PyJoinConstraint>()?;
    m.add_class::<PyJoinPredicate>()?;

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
