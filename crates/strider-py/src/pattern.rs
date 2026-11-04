//! A capture name interns to one `Capture` per process: `.capture("x")`,
//! `m["x"]` and a rewrite RHS `"x"` name the same capture. `"_"` and `"any_"`
//! are reserved.

use std::sync::Arc;
use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyString, PyTuple};
use rustc_hash::{FxBuildHasher, FxHashMap};
use strider_ir::node::{FunctionArgSource, ValueType as T};

use strider_pattern as sp;
use strider_pattern::node_builders::defer_drop;
use strider_pattern::{
    Capture, CaptureExt, Expr, IntoExpr, JoinConstraint, OutputAspect, Pattern, Template,
    TemplatePat,
};

use crate::errors::into_strider_err;
use crate::value_ops::value_ops;
use strider_pattern::WithOutput;

/// Binds a matched node so it can be read back from the `Match`. Each
/// `Capture()` is unique; `Capture("x") == Capture("x")`.
#[pyclass(eq, name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone, PartialEq)]
pub struct PyCapture {
    pub(crate) inner: Capture,
}

#[pymethods]
impl PyCapture {
    /// A capture variable. `Capture()` is fresh and unique; `Capture("name")`
    /// interns the name, the same capture as a rewrite RHS `"name"`. A bare
    /// string is not a match-pattern operand. Reserved names (`"_"`,
    /// `"any_"`) raise.
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
}

/// Every interned name both ways, for the life of the process (never pruned).
struct Names {
    by_name: FxHashMap<String, Capture>,
    by_id: FxHashMap<u32, String>,
}

static NAMES: Mutex<Names> = Mutex::new(Names {
    by_name: FxHashMap::with_hasher(FxBuildHasher),
    by_id: FxHashMap::with_hasher(FxBuildHasher),
});

pub(crate) fn intern_str(name: &str) -> PyResult<Capture> {
    if name == "_" || name == "any_" {
        return Err(into_strider_err(anyhow::anyhow!(
            "{name:?} is reserved (use anything() / var() / _ explicitly)"
        )));
    }
    let mut names = NAMES
        .lock()
        .map_err(|_| into_strider_err(anyhow::anyhow!("intern table lock poisoned")))?;
    if let Some(&cap) = names.by_name.get(name) {
        return Ok(cap);
    }
    let cap = Capture::new();
    names.by_name.insert(name.to_string(), cap);
    names.by_id.insert(cap.id(), name.to_string());
    Ok(cap)
}

/// `Capture('name')` for a string-interned capture, else `Capture(<id>)` for a
/// fresh anonymous one.
pub(crate) fn capture_display(id: u32) -> String {
    match NAMES.lock().ok().and_then(|t| t.by_id.get(&id).cloned()) {
        Some(name) => format!("Capture('{name}')"),
        None => format!("Capture({id})"),
    }
}

/// Where a Python operand sits, which decides what it may be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Value,
    Memory,
    /// A rewrite RHS operand.
    Template,
}

impl Slot {
    fn admits(self, b: &dyn Builder) -> bool {
        match self {
            Slot::Value => true,
            Slot::Memory => b.memory(),
            Slot::Template => false,
        }
    }
}

/// What a `Repr` is, beyond how it lowers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Buildable: `strider.template` has a twin.
    Build,
    Match,
    /// Constrains the node it wraps, which `.ordered()` looks through.
    Through,
    /// One operand in both slots of a pair `.ordered()` can pin.
    Pair,
}

pub(crate) type Make0 = Box<dyn Fn() -> Expr + Send + Sync>;
pub(crate) type Make1 = Box<dyn Fn(Expr) -> Expr + Send + Sync>;
pub(crate) type Make2 = Box<dyn Fn(Expr, Expr) -> Expr + Send + Sync>;

/// How a `Repr` lowers, over the `Expr`s of its operands.
pub(crate) enum Op {
    Any,
    Var(Capture),
    Leaf(Make0),
    Un(Make1),
    Bin(Make2),
    /// Every operand an arm; `true` cuts to the first that matches.
    OneOf(bool),
    /// A `.when()` predicate is the second operand.
    Wrap(Wrap),
    /// A builder's `.into_pat()` (its one operand), bound to the captures
    /// chained on since.
    Finished(Vec<Capture>),
}

/// The recipe behind a `Pat` or a `Template`, lowered fresh on every query.
pub(crate) struct Repr {
    /// The constructor, as an error names it.
    pub(crate) name: &'static str,
    pub(crate) kind: Kind,
    pub(crate) op: Op,
    pub(crate) args: Vec<Py<PyAny>>,
}

impl Repr {
    pub(crate) fn new(name: &'static str, kind: Kind, op: Op, args: Vec<Py<PyAny>>) -> Self {
        Self {
            name,
            kind,
            op,
            args,
        }
    }
}

pub(crate) fn leaf(name: &'static str, kind: Kind, make: Make0) -> Repr {
    Repr::new(name, kind, Op::Leaf(make), Vec::new())
}

pub(crate) fn un(name: &'static str, kind: Kind, make: Make1, x: Py<PyAny>) -> Repr {
    Repr::new(name, kind, Op::Un(make), vec![x])
}

pub(crate) fn bin(name: &'static str, kind: Kind, make: Make2, l: Py<PyAny>, r: Py<PyAny>) -> Repr {
    Repr::new(name, kind, Op::Bin(make), vec![l, r])
}

/// One of the wildcards bound to `c` when given one.
fn cap_opt(
    name: &'static str,
    kind: Kind,
    make: fn() -> sp::MatchOnly,
    c: Option<Capture>,
) -> PyPat {
    PyPat::from(leaf(
        name,
        kind,
        Box::new(move || match c {
            Some(c) => ex(make().capture(c)),
            None => ex(make()),
        }),
    ))
}

/// A carrier as the `Expr` either side lowers.
pub(crate) fn ex<P: IntoExpr>(p: P) -> Expr {
    p.into_expr()
}

/// A chained verb, decorating the root of the `Pat` it wraps.
pub(crate) enum Wrap {
    Capture(Capture),
    When,
    /// Pins the wrapped shape's operand order.
    Ordered,
    OfWidth(u32),
    ValueTy(strider_ir::node::ValueType),
}

impl Wrap {
    /// The method that applies this verb, as an error names it.
    fn name(&self) -> &'static str {
        match self {
            Wrap::Capture(_) => ".capture()",
            Wrap::When => ".when()",
            Wrap::Ordered => ".ordered()",
            Wrap::OfWidth(_) => ".of_width()",
            Wrap::ValueTy(_) => ".value_ty()",
        }
    }

    /// `e` decorated; `f` is a `.when()` predicate.
    fn apply(&self, py: Python<'_>, e: Expr, f: Option<&Py<PyAny>>) -> Expr {
        match self {
            Wrap::Capture(c) => ex(e.capture(*c)),
            Wrap::When => e.post_match(when_guard(
                f.expect("a .when() holds its predicate").clone_ref(py),
            )),
            Wrap::Ordered => ex(e.ordered()),
            Wrap::OfWidth(n) => ex(e.of_width(*n)),
            Wrap::ValueTy(ty) => ex(e.value_ty(*ty)),
        }
    }
}

impl Repr {
    /// The `Expr` this shape lowers to, its operands walked for `slot`'s side.
    fn expr(&self, py: Python<'_>, slot: Slot) -> PyResult<Expr> {
        let side = if slot == Slot::Template {
            Slot::Template
        } else {
            Slot::Value
        };
        let arg = |i: usize| expr(self.args[i].bind(py), side);
        Ok(match &self.op {
            Op::Any => ex(sp::anything()),
            Op::Var(c) => ex(sp::var(*c)),
            Op::Leaf(make) => make(),
            Op::Un(make) => make(arg(0)?),
            Op::Bin(make) => make(arg(0)?, arg(1)?),
            Op::OneOf(first) => {
                let mut arms = Vec::with_capacity(self.args.len());
                for a in &self.args {
                    arms.push(sp::boxed_alt(expr(a.bind(py), Slot::Value)?));
                }
                ex(if *first {
                    sp::OneOf::first(arms)
                } else {
                    sp::OneOf::new(arms)
                })
            }
            Op::Wrap(w) => w.apply(py, arg(0)?, self.args.get(1)),
            Op::Finished(caps) => return builder_expr(self.args[0].bind(py), slot, caps),
        })
    }

    /// The shape under the wrappers `through` passes.
    fn under<'a>(&'a self, py: Python<'a>, through: fn(&Repr) -> bool) -> &'a Repr {
        let mut r = self;
        while through(r) {
            match r.args[0].bind(py).downcast::<PyPat>() {
                Ok(p) => r = &p.get().repr,
                Err(_) => break,
            }
        }
        r
    }

    /// A memory slot takes a wildcard or an alternation, through a
    /// `.capture()` or `.when()`.
    fn is_memory(&self, py: Python<'_>) -> bool {
        let through = |r: &Repr| matches!(&r.op, Op::Wrap(Wrap::Capture(_) | Wrap::When));
        let r = self.under(py, through);
        matches!(r.op, Op::Any | Op::Var(_) | Op::OneOf(_))
    }
}

/// A finished pattern, reusable across `find_all` / rewrite calls.
#[pyclass(name = "Pat", module = "strider.pattern", frozen)]
pub struct PyPat {
    pub(crate) repr: Repr,
    /// Links below this one in the `.capture()` / `.when()` / `.of_width()`
    /// chain, bounded by `MAX_PATTERN_NESTING`.
    depth: u32,
}

impl From<Repr> for PyPat {
    fn from(repr: Repr) -> Self {
        Self { repr, depth: 0 }
    }
}

// Deferred: a plain drop recurses once per link and can overflow the stack.
impl Drop for PyPat {
    fn drop(&mut self) {
        defer_drop(std::mem::take(&mut self.repr.args));
    }
}

/// A `Pat` applying `wrap` to `slf`, which it owns as an operand handle.
fn derive(slf: &Bound<'_, PyPat>, wrap: Wrap, more: Option<PyObject>) -> PyResult<PyPat> {
    let depth = slf.get().depth + 1;
    if depth > MAX_PATTERN_NESTING {
        return Err(too_deep());
    }
    let mut args = vec![slf.clone().into_any().unbind()];
    args.extend(more);
    Ok(PyPat {
        repr: Repr::new(wrap.name(), Kind::Match, Op::Wrap(wrap), args),
        depth,
    })
}

/// `ob` as an integer of any width, or `None` when it is not an integer at all.
fn extract_int_operand(ob: &Bound<'_, PyAny>) -> PyResult<Option<strider_ir::IBig>> {
    crate::bigint::py_big_int(ob)?
        .map(crate::bigint::PyBigInt::value)
        .transpose()
}

/// `ob` as the `Expr` the `slot` it sits in takes.
pub(crate) fn expr(ob: &Bound<'_, PyAny>, slot: Slot) -> PyResult<Expr> {
    let _depth = DepthGuard::enter()?;
    let py = ob.py();
    if slot == Slot::Template {
        if let Ok(t) = ob.downcast::<crate::template::PyTemplate>() {
            return t.get().repr.expr(py, slot);
        }
        if let Ok(p) = ob.downcast::<PyPat>() {
            return Err(into_strider_err(anyhow::anyhow!(pat_rhs_message(
                &p.get().repr
            ))));
        }
    } else if let Ok(p) = ob.downcast::<PyPat>() {
        let repr = &p.get().repr;
        if slot == Slot::Memory && !matches!(repr.op, Op::Finished(_)) && !repr.is_memory(py) {
            return Err(refusal(ob, slot));
        }
        return repr.expr(py, slot);
    }
    if let Ok(c) = ob.downcast::<PyCapture>() {
        return Ok(ex(sp::var(c.get().inner)));
    }
    // A raw int is `int_const(value)` at any width.
    if slot != Slot::Memory
        && let Some(v) = extract_int_operand(ob)?
    {
        return Ok(ex(sp::int_const(v)));
    }
    builder_expr(ob, slot, &[])
}

/// `ob` as a builder the `slot` takes, bound to `caps` too.
fn builder_expr(ob: &Bound<'_, PyAny>, slot: Slot, caps: &[Capture]) -> PyResult<Expr> {
    match with_builder(ob, |b| slot.admits(b).then(|| b.expr(ob.py(), caps))) {
        Some(Some(e)) => e,
        _ => Err(refusal(ob, slot)),
    }
}

fn refusal(ob: &Bound<'_, PyAny>, slot: Slot) -> PyErr {
    let kind = operand_kind_name(ob);
    into_strider_err(match slot {
        Slot::Memory => anyhow::anyhow!(
            "a memory-input slot (`mem`) requires a memory producer: store() / \
             mem_phi() / call() / call_other(); got a value operand ({kind})"
        ),
        Slot::Template => anyhow::anyhow!(
            "cannot use {kind} as a nested rewrite RHS operand: expected a \
             Template, a Capture or an int. A bare string names a capture only \
             as the whole RHS; nested, use Capture(\"name\")"
        ),
        Slot::Value => anyhow::anyhow!(
            "expected a pattern (a Pat, a Capture, an int, or a typed builder), \
             got {kind}. A bare string is not a capture: use Capture(\"name\") \
             for a capture or anything() for a wildcard."
        ),
    })
}

/// `ob` sealed as a query's root.
pub(crate) fn root_pattern(ob: &Bound<'_, PyAny>) -> PyResult<Pattern> {
    let py = ob.py();
    if let Ok(p) = ob.downcast::<PyPat>() {
        return match &p.get().repr.op {
            Op::Finished(caps) => {
                let builder = p.get().repr.args[0].bind(py);
                with_builder(builder, |b| b.pattern(py, caps)).expect("into_pat holds a builder")
            }
            _ => Ok(expr(ob, Slot::Value)?.build()),
        };
    }
    match with_builder(ob, |b| b.pattern(py, &[])) {
        Some(pat) => pat,
        None => Ok(expr(ob, Slot::Value)?.build()),
    }
}

/// A `list` operand becomes a `one_of` union, so `target([a, b])` matches
/// either and `target([])` matches nothing. Anything else passes through.
fn coerce_operand_list(py: Python<'_>, p: Py<PyAny>) -> PyResult<Py<PyAny>> {
    match p.bind(py).downcast::<pyo3::types::PyList>() {
        Ok(list) => Ok(Py::new(py, one_of(list.extract()?))?.into_any()),
        Err(_) => Ok(p),
    }
}

pub(crate) fn operand_kind_name(ob: &Bound<'_, PyAny>) -> String {
    ob.get_type()
        .name()
        .map_or_else(|_| "value".to_string(), |n| n.to_string())
}

/// The refusal of a `Pat` on a rewrite RHS, at the top or nested.
fn pat_rhs_message(repr: &Repr) -> String {
    let name = repr.name;
    if repr.kind == Kind::Build {
        format!(
            "cannot use {name} as a rewrite RHS: a strider.pattern Pat only \
             matches; build it with strider.template (e.g. template.var(c), \
             template.int_const(v))"
        )
    } else {
        format!(
            "cannot use {name} as a rewrite RHS: a strider.pattern Pat only \
             matches, and {name} has no strider.template twin"
        )
    }
}

/// Case-insensitive: `"i1"` / `"I64"` / `"f32"`.
/// `n`, when some value type is `n` bits wide.
pub(crate) fn width_arg(n: u32) -> PyResult<u32> {
    let mut widths: Vec<usize> = T::SUPPORTED.iter().map(|ty| ty.bit_width()).collect();
    if widths.contains(&(n as usize)) {
        return Ok(n);
    }
    widths.sort_unstable();
    widths.dedup();
    let names: Vec<String> = widths.iter().map(ToString::to_string).collect();
    Err(into_strider_err(anyhow::anyhow!(
        "unsupported width {n}: expected one of {}",
        names.join(", ")
    )))
}

fn parse_value_ty(name: &str) -> PyResult<T> {
    name.to_ascii_lowercase()
        .parse()
        .or_else(|_| name.parse())
        .map_err(into_strider_err)
}

// The per-query walk recurses natively, so these bounds turn a stack overflow
// into an exception. They bound the walk and the `.capture()` chain, not
// construction; dropping a deep constructor is safe (`defer_drop`).

/// Walk levels, the root and one per nested operand. The stack budget is what
/// holds on a thread smaller than the main one.
const MAX_PATTERN_NESTING: u32 = 512;

/// Operands one walk may visit: a reused `Pat` (`q = int_add(q, q)`) lowers
/// once per use, so depth alone does not bound the work. Above
/// [`strider_pattern::MAX_PATTERN_NODES`], so no runnable pattern is refused.
const MAX_COMPILE_STEPS: u64 = 4096;

/// One compile may consume `1 / STACK_BUDGET_SHARE` of the stack left below
/// the outermost guard, less [`STEP_RESERVE`]; the match walk after it costs
/// about as much again.
const STACK_BUDGET_SHARE: usize = 4;

/// Stack held back for what runs past the last passing check: one lowering
/// step and the refusal. Under this much left, the outermost guard refuses.
const STEP_RESERVE: usize = 32 * 1024;

/// Stack one compile may consume where the thread's bounds are not reported.
const FALLBACK_PATTERN_STACK: usize = 48 * 1024;

thread_local! {
    static COMPILE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Lowering steps the live compile has spent, reset when the outermost
    /// guard opens.
    static COMPILE_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// A frame address from the outermost live guard, the zero consumption is
    /// measured against.
    static STACK_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// What one compile may consume on this thread, taken from its real stack
    /// bounds when the outermost guard enters.
    static STACK_BUDGET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Address of a local in the calling frame. The stack grows down, so a deeper
/// frame reads lower.
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u8;
    std::hint::black_box(std::ptr::from_ref(&probe)) as usize
}

fn too_deep() -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "pattern nesting too deep: over {MAX_PATTERN_NESTING} levels. That is \
         the loosest bound; this thread's stack budget and the matcher's own \
         node cap both reject shallower patterns still"
    ))
}

struct DepthGuard;

impl DepthGuard {
    fn enter() -> PyResult<Self> {
        let here = frame_mark();
        COMPILE_DEPTH.with(|d| {
            let next = d.get() + 1;
            if next > MAX_PATTERN_NESTING {
                return Err(too_deep());
            }
            if next == 1 {
                STACK_BASE.set(here);
                let budget = match stacker::remaining_stack() {
                    None => FALLBACK_PATTERN_STACK,
                    Some(left) if left < STEP_RESERVE => {
                        return Err(into_strider_err(anyhow::anyhow!(
                            "too little of this thread's stack left to compile a \
                             pattern ({} KiB, under the {} KiB one lowering step and \
                             its refusal take); give the thread a larger stack_size",
                            left / 1024,
                            STEP_RESERVE / 1024,
                        )));
                    }
                    Some(left) => (left - STEP_RESERVE) / STACK_BUDGET_SHARE,
                };
                STACK_BUDGET.set(budget);
                COMPILE_STEPS.set(0);
            } else {
                let used = STACK_BASE.get().saturating_sub(here);
                let budget = STACK_BUDGET.get();
                if used > budget {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "pattern nesting too deep for this thread's stack ({} KiB \
                         used of a {} KiB compile budget at {next} levels; give the \
                         thread a larger stack_size, or nest less)",
                        used / 1024,
                        budget / 1024,
                    )));
                }
            }
            let steps = COMPILE_STEPS.get() + 1;
            if steps > MAX_COMPILE_STEPS {
                return Err(into_strider_err(anyhow::anyhow!(
                    "pattern too large to compile: over {MAX_COMPILE_STEPS} lowered \
                     nodes. A Pat used in two operands (q = int_add(q, q)) lowers \
                     once per use, so each level of reuse doubles the pattern; a \
                     query stays well under the matcher's node cap"
                )));
            }
            COMPILE_STEPS.set(steps);
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

/// A query pattern: a `Pat`, a `Capture`, a raw int (an `int_const`), or a
/// typed builder. A bare string is not a capture.
pub struct PatLike<'py>(Bound<'py, PyAny>);

impl<'py> FromPyObject<'py> for PatLike<'py> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        let is_pattern = ob.is_instance_of::<PyPat>()
            || ob.is_instance_of::<PyCapture>()
            || with_builder(ob, |_| ()).is_some()
            || crate::bigint::py_big_int(ob)?.is_some();
        if !is_pattern {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "expected a pattern (a Pat, a typed builder, a Capture or an \
                 int), got {}",
                operand_kind_name(ob)
            )));
        }
        Ok(Self(ob.clone()))
    }
}

/// One pattern, or a list matched as a join: captures unify on shared
/// `Capture` objects, and each result is one merged `Match`.
#[derive(FromPyObject)]
pub enum PatQuery<'py> {
    Single(PatLike<'py>),
    Many(Vec<PatLike<'py>>),
}

impl PatQuery<'_> {
    pub(crate) fn to_patterns(&self) -> PyResult<Vec<Pattern>> {
        match self {
            PatQuery::Single(p) => Ok(vec![root_pattern(&p.0)?]),
            PatQuery::Many(ps) => ps.iter().map(|p| root_pattern(&p.0)).collect(),
        }
    }
}

/// Rewrite-RHS input: a `Template`, a `Capture` or a capture name.
pub enum TemplateLike<'py> {
    Template(Bound<'py, crate::template::PyTemplate>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
}

impl<'py> FromPyObject<'py> for TemplateLike<'py> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(t) = ob.downcast::<crate::template::PyTemplate>() {
            return Ok(Self::Template(t.clone()));
        }
        if let Ok(c) = ob.downcast::<PyCapture>() {
            return Ok(Self::Capture(c.clone()));
        }
        if let Ok(s) = ob.downcast::<PyString>() {
            return Ok(Self::Str(s.clone()));
        }
        let msg = match ob.downcast::<PyPat>() {
            Ok(p) => pat_rhs_message(&p.borrow().repr),
            Err(_) => format!(
                "cannot use {} as a rewrite RHS: expected a strider.template \
                 Template, a Capture or a capture name ({})",
                operand_kind_name(ob),
                if ob.is_instance_of::<PyBool>() {
                    "a bool constant is template.bool_const(b)"
                } else {
                    "an int constant is template.int_const(v)"
                }
            ),
        };
        Err(pyo3::exceptions::PyTypeError::new_err(msg))
    }
}

impl TemplateLike<'_> {
    pub(crate) fn to_template(&self) -> PyResult<Template> {
        let cap = match self {
            TemplateLike::Template(t) => {
                return Ok(expr(t.as_any(), Slot::Template)?.into_template());
            }
            TemplateLike::Capture(c) => c.get().inner,
            TemplateLike::Str(s) => {
                let name = s.to_string();
                if name == "_" || name == "any_" {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "cannot use {name:?} as a rewrite RHS: it is the \
                         wildcard, not a capture name"
                    )));
                }
                intern_str(&name)?
            }
        };
        Ok(TemplatePat::into_template(sp::var(cap)))
    }
}

// A raising predicate stashes its error here and the matcher finishes its
// walk: the first error wins, later predicates answer no-match without
// running, and the query boundary re-raises.
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

// The function a running query reads, for the `Match` a predicate is handed.
// A stack: a predicate may run a nested query. The predicate's `Match`
// re-borrows under the query's read borrow; only `PyFunction::edit` borrows
// mutably.
thread_local! {
    static CURRENT_QUERY_FUNCTION: std::cell::RefCell<Vec<crate::function::FunctionAt>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Must be paired with [`pop_current_query_function`] once the query, and
/// every `.when()` predicate it invokes, has finished.
pub(crate) fn push_current_query_function(at: crate::function::FunctionAt) {
    CURRENT_QUERY_FUNCTION.with(|c| c.borrow_mut().push(at));
}

pub(crate) fn pop_current_query_function() {
    CURRENT_QUERY_FUNCTION.with(|c| {
        c.borrow_mut().pop();
    });
}

fn current_query_function(py: Python<'_>) -> Option<crate::function::FunctionAt> {
    CURRENT_QUERY_FUNCTION.with(|c| c.borrow().last().map(|at| at.clone_ref(py)))
}

/// Runs a `.when()` predicate on a `Match` rooted at `node`; a raise counts
/// as no-match.
fn run_when_predicate(
    node: strider_ir::node::NodeId,
    bindings: &strider_pattern::Bindings,
    py_func: &PyObject,
) -> bool {
    run_predicate(
        ".when()",
        |at| {
            vec![strider_pattern::Match::new(
                node,
                bindings.clone(),
                at.generation,
            )]
        },
        |py, m| py_func.call1(py, (m,)),
    )
    .unwrap_or(false)
}

/// A Python predicate's verdict on the `Match` `inner` builds. `None` without
/// a live query, or once any predicate has raised.
fn run_predicate(
    what: &str,
    inner: impl FnOnce(&crate::function::FunctionAt) -> Vec<strider_pattern::Match>,
    call: impl FnOnce(Python<'_>, Py<crate::matcher::PyMatch>) -> PyResult<PyObject>,
) -> Option<bool> {
    Python::with_gil(|py| {
        if peek_pending_query_error() {
            return None;
        }
        let Some(at) = current_query_function(py) else {
            // Unreachable; fail closed rather than panic into the matcher.
            eprintln!("strider {what}: no active query function on this thread (internal error)");
            return None;
        };
        let inner = inner(&at);
        let verdict = Py::new(py, crate::matcher::PyMatch { inner, at })
            .and_then(|m| call(py, m))
            .and_then(|r| r.extract::<bool>(py));
        verdict.map_err(stash_pending_query_error).ok()
    })
}

// Set while a rewrite LHS compiles; a `.when()` attachment marks it, so the
// rewrite refuses a predicate that could not read the mutably-held function.
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

/// `f` as a post-match guard, handed the matched node as `Match.root`.
fn when_guard(f: PyObject) -> sp::PostMatchFn {
    note_when_attached();
    Box::new(move |_matcher, node, _ty, bindings| run_when_predicate(node, bindings, &f))
}

/// Compile `pat` as a rewrite LHS, rejecting a `.when()` guard anywhere in it.
pub(crate) fn compile_rewrite_lhs(pat: &PatLike<'_>) -> PyResult<Pattern> {
    let outer = REWRITE_LHS_WHEN.with(|c| c.replace(Some(false)));
    let compiled = root_pattern(&pat.0);
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

/// A typed builder: field state replayed onto the core builder per query.
pub(crate) trait Builder {
    /// Produces a memory token, so a memory slot takes it.
    fn memory(&self) -> bool {
        false
    }
    fn common(&self) -> &std::cell::RefCell<CommonState>;
    /// The node with its operands walked, bound to its capture and `caps`,
    /// without `.when()`.
    fn node(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<Expr>;
}

impl dyn Builder + '_ {
    fn when(&self, py: Python<'_>) -> Option<sp::PostMatchFn> {
        let f = self
            .common()
            .borrow()
            .when
            .as_ref()
            .map(|f| f.clone_ref(py));
        f.map(when_guard)
    }

    /// Nested as an operand.
    fn expr(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<Expr> {
        let e = self.node(py, caps)?;
        Ok(match self.when(py) {
            Some(f) => e.post_match(f),
            None => e,
        })
    }

    /// Sealed as a root. Opens a walk level so the stack budget covers the
    /// builder's own frames.
    fn pattern(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<Pattern> {
        let _depth = DepthGuard::enter()?;
        let pat = self.node(py, caps)?.build();
        Ok(match self.when(py) {
            Some(f) => pat.with_root_post_match(f),
            None => pat,
        })
    }
}

/// `f` over `ob` as a builder, if it is one.
macro_rules! builders {
    ($($ty:ty),* $(,)?) => {
        pub(crate) fn with_builder<R>(
            ob: &Bound<'_, PyAny>,
            f: impl FnOnce(&dyn Builder) -> R,
        ) -> Option<R> {
            $(
                if let Ok(b) = ob.downcast::<$ty>() {
                    return Some(f(&*b.borrow()));
                }
            )*
            None
        }
    };
}

builders!(
    PyLoadPat,
    PyStorePat,
    PyCallPat,
    PyCallOtherPat,
    PyPhiPat,
    PyMemPhiPat,
    PyIntBinaryPat,
    PyFloatBinaryPat,
    PyBoolBinaryPat,
    PyFunctionArgPat,
    PyEntryPat,
    PyRegionPat,
    PyRetPat,
    PyIfPat,
    PySwitchPat,
    PyIndirectBranchPat,
    PyUnreachablePat,
);

/// The value-passthrough casts the matcher walks through. Combine with `|`;
/// pass as `find_all(pat, ignore_casts=...)`.
#[pyclass(eq, hash, name = "CastMask", module = "strider.pattern", frozen)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyCastMask {
    pub(crate) inner: strider_pattern::CastMask,
}

/// One classmethod per row.
macro_rules! forall_castmask {
    ($($name:ident => $value:expr, $doc:literal;)*) => {$(
        #[pymethods]
        impl PyCastMask {
            #[doc = $doc]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self { inner: $value }
            }
        }
    )*};
}

forall_castmask! {
    zero_extend => sp::CastMask::ZERO_EXTEND,
        "Zero-extending widenings only.";
    sign_extend => sp::CastMask::SIGN_EXTEND,
        "Sign-extending widenings only.";
    extend => sp::CastMask::EXTEND,
        "Both widening kinds.";
    truncate => sp::CastMask::TRUNCATE,
        "Narrowings only.";
    int_bits_to_float => sp::CastMask::INT_BITS_TO_FLOAT,
        "Integer-bits-to-float reinterpretations only.";
    float_bits_to_int => sp::CastMask::FLOAT_BITS_TO_INT,
        "Float-bits-to-integer reinterpretations only.";
    all => sp::CastMask::all(),
        "Every passthrough cast.";
    none => sp::CastMask::empty(),
        "No passthrough casts.";
}

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
    /// The mask as a raw bitset.
    fn bits(&self) -> u32 {
        self.inner.bits()
    }
    /// Whether any cast is selected.
    fn __bool__(&self) -> bool {
        !self.inner.is_empty()
    }
    fn __repr__(&self) -> String {
        if self.inner.is_empty() {
            return "CastMask.none()".to_owned();
        }
        let names: Vec<String> = self
            .inner
            .iter_names()
            .map(|(name, _)| format!("CastMask.{}()", name.to_lowercase()))
            .collect();
        names.join(" | ")
    }
}

#[pymethods]
impl PyPat {
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        self.repr.args.iter().try_for_each(|a| visit.call(a))
    }

    /// Bind the matched node to `c`, a `Capture` or a name.
    fn capture(slf: &Bound<'_, Self>, c: crate::matcher::CaptureKey<'_>) -> PyResult<PyPat> {
        let c = c.resolve()?;
        let pat = slf.get();
        let Op::Finished(caps) = &pat.repr.op else {
            return derive(slf, Wrap::Capture(c), None);
        };
        let op = Op::Finished(caps.iter().copied().chain([c]).collect());
        let args = vec![pat.repr.args[0].clone_ref(slf.py())];
        Ok(PyPat {
            repr: Repr::new(pat.repr.name, Kind::Match, op, args),
            depth: pat.depth,
        })
    }

    /// Keep a match only when `f(match)` returns `True`. A non-`bool` return
    /// is a `TypeError`; raising aborts the query and re-raises. `f` may run
    /// on a partial match, more than once, or not at all.
    fn when(slf: &Bound<'_, Self>, f: PyObject) -> PyResult<PyPat> {
        derive(slf, Wrap::When, Some(f))
    }

    /// Constrain the matched node's value output to `n` bits. Match-only.
    fn of_width(slf: &Bound<'_, Self>, n: u32) -> PyResult<PyPat> {
        derive(slf, Wrap::OfWidth(width_arg(n)?), None)
    }

    /// Constrain the matched node's value output to type `ty` (`"i64"`,
    /// `"f32"`). Match-only.
    fn value_ty(slf: &Bound<'_, Self>, ty: &str) -> PyResult<PyPat> {
        let t = parse_value_ty(ty)?;
        derive(slf, Wrap::ValueTy(t), None)
    }

    /// `.of_width(1)`: a boolean output. Match-only.
    fn bool_valued(slf: &Bound<'_, Self>) -> PyResult<PyPat> {
        derive(slf, Wrap::OfWidth(1), None)
    }

    /// Stop the matcher retrying this op with its operands swapped. Pins this
    /// node alone (for `int_ne` / `float_ne`, the `Equal` under the `Xor`). A
    /// shape with no operands to order raises. Match-only.
    fn ordered(slf: &Bound<'_, Self>) -> PyResult<PyPat> {
        let r = slf.get().repr.under(slf.py(), |r| {
            matches!(r.op, Op::Wrap(_)) || r.kind == Kind::Through
        });
        if !matches!(r.op, Op::Bin(_)) && r.kind != Kind::Pair {
            return Err(into_strider_err(anyhow::anyhow!(
                "ordered() pins the operand order of a binary op; got {}",
                r.name
            )));
        }
        derive(slf, Wrap::Ordered, None)
    }

    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

/// Wildcard: matches any node without binding it.
#[pyfunction(name = "anything")]
pub fn any_() -> PyPat {
    PyPat::from(Repr::new("anything()", Kind::Match, Op::Any, Vec::new()))
}

/// Wildcard that binds the matched node to capture `c`.
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from(var_repr(c.inner))
}

pub(crate) fn var_repr(c: Capture) -> Repr {
    Repr::new("var()", Kind::Build, Op::Var(c), Vec::new())
}

/// Match any value output exactly `n` bits wide.
#[pyfunction]
pub fn value_of_width(n: u32) -> PyResult<PyPat> {
    let n = width_arg(n)?;
    let make = move || ex(sp::value_of_width(n));
    Ok(PyPat::from(leaf(
        "value_of_width()",
        Kind::Match,
        Box::new(make),
    )))
}

/// `int_const(v)`, either side.
pub(crate) fn int_const_repr(v: strider_ir::IBig) -> Repr {
    let make = move || ex(sp::int_const(v.clone()));
    leaf("int_const()", Kind::Build, Box::new(make))
}

/// `bool_const(v)`, either side.
pub(crate) fn bool_const_repr(v: bool) -> Repr {
    leaf(
        "bool_const()",
        Kind::Build,
        Box::new(move || ex(sp::bool_const(v))),
    )
}

/// `float_const(bits)`, either side.
pub(crate) fn float_const_repr(bits: u64) -> Repr {
    let make = move || ex(sp::float_const(bits));
    leaf("float_const()", Kind::Build, Box::new(make))
}

/// Match `inner` and require all value inputs to be `n` bits wide.
#[pyfunction]
pub fn inputs_of_width(n: u32, inner: Py<PyAny>) -> PyResult<PyPat> {
    let n = width_arg(n)?;
    let make = move |e| ex(sp::inputs_of_width(n, e));
    Ok(PyPat::from(un(
        "inputs_of_width()",
        Kind::Through,
        Box::new(make),
        inner,
    )))
}

/// Match `inner` whose value inputs are all 1-bit (`I1`).
#[pyfunction]
pub fn bool_inputs(inner: Py<PyAny>) -> PyResult<PyPat> {
    inputs_of_width(1, inner)
}

/// A `Capture`, tried first as a cheap downcast, else `T`.
pub enum CapOr<T> {
    Cap(Py<PyCapture>),
    Else(T),
}

impl<'py, T: FromPyObject<'py>> FromPyObject<'py> for CapOr<T> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        match ob.downcast::<PyCapture>() {
            Ok(c) => Ok(Self::Cap(c.clone().unbind())),
            Err(_) => T::extract_bound(ob).map(Self::Else),
        }
    }
}

/// One constant, a set of them, or a capture, for [`int_const`]. `Cap` is
/// never extracted past [`CapOr`]; it names the capture in the error.
#[derive(FromPyObject)]
pub enum IntConstArg {
    #[pyo3(annotation = "int")]
    One(crate::bigint::PyBigInt),
    #[pyo3(annotation = "list[int]")]
    Set(Vec<crate::bigint::PyBigInt>),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match an `IntConst` equal to `value` (or any member of a list; an empty
/// list matches nothing), both masked to the output width. Bit-pattern
/// equality: a negative uses its sign-extended form. Given a `Capture`, or no
/// argument, match any integer constant, binding it to the capture if given.
#[pyfunction]
#[pyo3(signature = (value=None))]
pub fn int_const(value: Option<CapOr<IntConstArg>>) -> PyResult<PyPat> {
    let any = |c| cap_opt("int_const()", Kind::Build, sp::any_int_const, c);
    Ok(match value {
        None => any(None),
        Some(CapOr::Cap(c) | CapOr::Else(IntConstArg::Cap(c))) => any(Some(c.get().inner)),
        Some(CapOr::Else(IntConstArg::One(v))) => PyPat::from(int_const_repr(v.value()?)),
        Some(CapOr::Else(IntConstArg::Set(vs))) => {
            let vs = vs
                .into_iter()
                .map(crate::bigint::PyBigInt::value)
                .collect::<PyResult<Vec<_>>>()?;
            let make = move || ex(sp::int_const(vs.clone()));
            PyPat::from(leaf("int_const([..])", Kind::Build, Box::new(make)))
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

/// Match an `IntConst` holding `value` exactly, zero-extended or
/// sign-extended (or any member of a list). `value` must fit the matched
/// width. A value outside the signed 64-bit range raises.
#[pyfunction]
pub fn int_const_any_width(value: IntConstAnyWidthArg) -> PyResult<PyPat> {
    Ok(PyPat::from(match value {
        IntConstAnyWidthArg::One(v) => {
            let v = checked_signed_i64(v)?;
            let make = move || ex(sp::int_const_any_width(v));
            leaf("int_const_any_width()", Kind::Build, Box::new(make))
        }
        IntConstAnyWidthArg::Set(vs) => {
            let vs: Vec<i64> = vs
                .into_iter()
                .map(checked_signed_i64)
                .collect::<PyResult<_>>()?;
            let make = move || ex(sp::int_const_any_width(vs.clone()));
            leaf("int_const_any_width([..])", Kind::Build, Box::new(make))
        }
    }))
}

/// One value, or a capture, for [`bool_const`], through [`CapOr`].
#[derive(FromPyObject)]
pub enum BoolConstArg {
    #[pyo3(annotation = "bool")]
    One(bool),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match an `I1` constant equal to `value`. Given a `Capture`, or no
/// argument, match any `I1` constant, binding it to the capture if given.
#[pyfunction]
#[pyo3(signature = (value=None))]
pub fn bool_const(value: Option<CapOr<BoolConstArg>>) -> PyPat {
    let any = |c| cap_opt("bool_const()", Kind::Build, sp::any_bool_const, c);
    match value {
        None => any(None),
        Some(CapOr::Cap(c) | CapOr::Else(BoolConstArg::Cap(c))) => any(Some(c.get().inner)),
        Some(CapOr::Else(BoolConstArg::One(v))) => PyPat::from(bool_const_repr(v)),
    }
}

/// The IEEE 754 bit pattern, or a capture, for [`float_const`], through
/// [`CapOr`].
#[derive(FromPyObject)]
pub enum FloatConstArg {
    #[pyo3(annotation = "int")]
    Bits(u64),
    #[pyo3(annotation = "Capture")]
    Cap(Py<PyCapture>),
}

/// Match a `FloatConst` whose raw bits equal `bits`. Given a `Capture`, or no
/// argument, match any float constant, binding it to the capture if given.
#[pyfunction]
#[pyo3(signature = (bits=None))]
pub fn float_const(bits: Option<CapOr<FloatConstArg>>) -> PyPat {
    let any = |c| cap_opt("float_const()", Kind::Build, sp::any_float_const, c);
    match bits {
        None => any(None),
        Some(CapOr::Cap(c) | CapOr::Else(FloatConstArg::Cap(c))) => any(Some(c.get().inner)),
        Some(CapOr::Else(FloatConstArg::Bits(b))) => PyPat::from(float_const_repr(b)),
    }
}

/// Match any node with an integer value output (`I1` through `I512`),
/// optionally binding it to `c`. `int_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_int(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    cap_opt("any_int()", Kind::Match, sp::any_int, c.map(|c| c.inner))
}

/// Match any node with a 1-bit (`I1`) value output, optionally binding it to
/// `c`. `bool_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_bool(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    cap_opt("any_bool()", Kind::Match, sp::any_bool, c.map(|c| c.inner))
}

/// Match any node with a float value output (`F16` through `F128`), optionally
/// binding it to `c`. `float_const()` is the constant-only form.
#[pyfunction]
#[pyo3(signature = (c=None))]
pub fn any_float(c: Option<PyRef<'_, PyCapture>>) -> PyPat {
    cap_opt(
        "any_float()",
        Kind::Match,
        sp::any_float,
        c.map(|c| c.inner),
    )
}

/// Match any `InitialVar` node.
#[pyfunction]
pub fn initial_var() -> PyPat {
    let make = || ex(sp::initial_var());
    PyPat::from(leaf("initial_var()", Kind::Match, Box::new(make)))
}

/// Match `InitialVar(vn)` for `vn` or any register containing it, so `eax`
/// matches a node tagged `rax`.
#[pyfunction]
pub fn initial_var_for(vn: crate::sleigh::PyVn) -> PyPat {
    let vn = vn.inner;
    let make = move || ex(sp::initial_var_for(vn));
    PyPat::from(leaf("initial_var_for()", Kind::Match, Box::new(make)))
}

/// Match if any listed sub-pattern matches (a union: every matching arm
/// fires with its own bindings; `first_of` cuts). Arms matching one node with
/// the same bindings are one match; a capture under an arm that did not fire
/// stays unbound. Nests in any slot. Match-only; no arms matches nothing.
#[pyfunction]
pub fn one_of(patterns: Vec<Py<PyAny>>) -> PyPat {
    PyPat::from(Repr::new(
        "one_of()",
        Kind::Match,
        Op::OneOf(false),
        patterns,
    ))
}

/// Match the first listed sub-pattern that matches (an ordered OR): at each
/// value it is offered, the first arm yielding a match wins, so a leading
/// `anything()` shadows the rest. Several settle in pattern order (operands
/// left to right, inner before outer). Nests in any slot. Match-only; no arms
/// matches nothing.
#[pyfunction]
pub fn first_of(patterns: Vec<Py<PyAny>>) -> PyPat {
    PyPat::from(Repr::new(
        "first_of()",
        Kind::Match,
        Op::OneOf(true),
        patterns,
    ))
}

/// `anything().when(f)`.
#[pyfunction]
pub fn predicate(py: Python<'_>, f: PyObject) -> PyResult<PyPat> {
    derive(&Bound::new(py, any_())?, Wrap::When, Some(f))
}

// Each parser accepts the variant's `Debug` spelling, as `Match.op` reads it.

fn lookup_op<Op: Copy>(
    variants: &[Op],
    canonical: impl Fn(Op) -> &'static str,
    name: &str,
    op_kind: &str,
) -> PyResult<Op> {
    variants
        .iter()
        .copied()
        .find(|&op| canonical(op) == name)
        .ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "unknown {op_kind} variant {name:?}: expected one of {}",
                variants
                    .iter()
                    .map(|&op| canonical(op))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

// `rest_unreachable` lets a subset (the boolean ops) still match exhaustively.
macro_rules! op_parser {
    (
        $vis:vis $fn:ident, $names:ident, $ty:ty, $op_kind:literal,
        variants = [$($variant:ident),+ $(,)?]
        $(, rest_unreachable = $msg:literal)? $(,)?
    ) => {
        /// Every name the parser accepts.
        pub(crate) const $names: &[&str] = &[$(stringify!($variant)),+];

        $vis fn $fn(name: &str) -> PyResult<$ty> {
            use $ty as Op;
            static VARIANTS: &[$ty] = &[$(Op::$variant),+];
            fn canonical(op: $ty) -> &'static str {
                match op {
                    $(Op::$variant => stringify!($variant),)+
                    $(_ => unreachable!($msg, ),)?
                }
            }
            lookup_op(VARIANTS, canonical, name, $op_kind)
        }
    };
}

op_parser!(
    pub(crate) parse_int_cmp_op,
    INT_CMP_OPS,
    strider_ir::IntCmpOp,
    "IntCmpOp",
    variants = [Equal, Less, Sless, Carry, Scarry, Sborrow]
);

op_parser!(
    parse_int_binary_op,
    INT_BINARY_OPS,
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
    ]
);

op_parser!(
    parse_bool_binary_op,
    BOOL_BINARY_OPS,
    strider_ir::IntBinaryOp,
    "boolean binary op",
    variants = [And, Or, Xor],
    rest_unreachable = "non-boolean IntBinaryOp in bool table"
);

op_parser!(
    parse_float_binary_op,
    FLOAT_BINARY_OPS,
    strider_ir::FloatBinaryOp,
    "FloatBinaryOp",
    variants = [Add, Mul, Div]
);

op_parser!(
    pub(crate) parse_extend_op,
    EXTEND_OPS,
    strider_ir::ExtendOp,
    "ExtendOp",
    variants = [ZeroExtend, SignExtend]
);

pub(crate) fn checked_signed_i64(value: i128) -> PyResult<i64> {
    i64::try_from(value).map_err(|_| {
        into_strider_err(anyhow::anyhow!(
            "int_const_any_width value {value} does not fit in i64 (the core candidate width)"
        ))
    })
}

value_ops!(PyPat, "Match", pattern);

/// Match a NaN test, the IEEE 754 self-inequality `x != x`.
#[pyfunction]
pub fn float_is_nan(operand: Py<PyAny>) -> PyPat {
    let make = |x| ex(sp::float_is_nan(x));
    PyPat::from(un("float_is_nan()", Kind::Pair, Box::new(make), operand))
}

/// Match a float less-or-equal test, NaN-aware.
#[pyfunction]
pub fn float_le(l: Py<PyAny>, r: Py<PyAny>) -> PyPat {
    let make = |l, r| ex(sp::float_le(l, r));
    PyPat::from(bin("float_le()", Kind::Match, Box::new(make), l, r))
}

/// The `any_*` ops: any variant of one op family, the variant bound to `c`.
macro_rules! any_variant {
    ($($name:ident($($x:ident),+) = $doc:literal;)*) => {$(
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(c: PyRef<'_, PyCapture>, $($x: Py<PyAny>),+) -> PyPat {
            let c = c.inner;
            let name = concat!(stringify!($name), "()");
            PyPat::from(any_variant!(@repr $name, name, c, $($x),+))
        }
    )*};
    (@repr $name:ident, $n:ident, $c:ident, $x:ident) => {
        un($n, Kind::Match, Box::new(move |x| ex(sp::$name(x).capture($c))), $x)
    };
    (@repr $name:ident, $n:ident, $c:ident, $l:ident, $r:ident) => {
        bin($n, Kind::Match, Box::new(move |l, r| ex(sp::$name(l, r).capture($c))), $l, $r)
    };
}

any_variant! {
    any_int_binary(l, r)
        = "Match any `IntBinaryOp` over `(l, r)` and bind the op variant to `c`.";
    any_int_unary(operand)
        = "Match any `IntUnaryOp` over `operand` and bind the op variant to `c`.";
    any_int_cmp(l, r)
        = "Match any `IntCmpOp` over `(l, r)` and bind the op variant to `c`.";
    any_bool_binary(l, r)
        = "Match any boolean binary op over `(l, r)` and bind the op variant to `c`.";
    any_float_binary(l, r)
        = "Match any `FloatBinaryOp` over `(l, r)` and bind the op variant to `c`.";
    any_float_unary(operand)
        = "Match any `FloatUnaryOp` over `operand` and bind the op variant to `c`.";
    any_float_cmp(l, r)
        = "Match any `FloatCmpOp` over `(l, r)` and bind the op variant to `c`.";
}

fn clone_opt(py: Python<'_>, slot: &Option<Py<PyAny>>) -> Option<Py<PyAny>> {
    slot.as_ref().map(|p| p.clone_ref(py))
}

#[derive(Default)]
pub(crate) struct CommonState {
    capture: Option<Capture>,
    when: Option<PyObject>,
    /// `(raw slot, operand)` from `.input(i, p)`.
    inputs: Vec<(usize, Py<PyAny>)>,
    /// Operands from `.any_input(p)`, one existential slot each.
    any_inputs: Vec<Py<PyAny>>,
    /// `(slot, aspect)`; `slot` is `None` for the existential `.any_output()`.
    outputs: Vec<(Option<usize>, OutputAspect)>,
    /// `(edge, value)` from a phi's `.input_from`.
    edge_links: Vec<(Capture, Capture)>,
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
            /// Attach a Python predicate that runs after the match, returning
            /// a `bool`.
            fn when(slf: PyRef<'_, Self>, f: PyObject) -> PyRef<'_, Self> {
                slf.common.borrow_mut().when = Some(f);
                slf
            }
            /// Finalise into a `Pat`. A bad operand raises here.
            fn into_pat(slf: Bound<'_, Self>) -> PyResult<PyPat> {
                (&*slf.borrow() as &dyn Builder).pattern(slf.py(), &[])?;
                let name = "a control / variadic builder";
                let args = vec![slf.into_any().unbind()];
                Ok(PyPat::from(Repr::new(
                    name,
                    Kind::Match,
                    Op::Finished(Vec::new()),
                    args,
                )))
            }
            fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
                Ok(format!("{}(...)", slf.get_type().name()?))
            }

            /// `try_borrow`: a collection can land mid-setter.
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

            /// Drops everything `__traverse__` reports.
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

/// Ceiling on a caller-supplied operand index, low enough that
/// `input_head_len() + idx` cannot overflow.
const MAX_OPERAND_INDEX: usize = 1 << 20;

fn check_operand_index(idx: usize) -> PyResult<()> {
    if idx > MAX_OPERAND_INDEX {
        return Err(into_strider_err(anyhow::anyhow!(
            "operand index {idx} is out of range (max {MAX_OPERAND_INDEX})"
        )));
    }
    Ok(())
}

// The generic slot methods beneath the named accessors; a builder names the
// slot kinds its node shape has.
macro_rules! builder_slot_methods {
    ($ty:ty, any_input) => {
        #[pymethods]
        impl $ty {
            /// Require some input of the node to match `p`, without pinning a
            /// slot. `var` / `anything` also reach control, memory and
            /// phi-token edges. Repeatable, each call a distinct slot.
            fn any_input<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyRef<'py, Self> {
                slf.common.borrow_mut().any_inputs.push(p);
                slf
            }
        }
    };
    ($ty:ty, input) => {
        #[pymethods]
        impl $ty {
            /// Match `p` against raw input slot `idx`: `Call` is `[ctrl, mem,
            /// target, sp, arg0, ...]`, `Load` `[mem, addr]`, `If` `[ctrl,
            /// cond]`.
            fn input<'py>(
                slf: PyRef<'py, Self>,
                idx: crate::bigint::Unsigned<usize>,
                p: Py<PyAny>,
            ) -> PyResult<PyRef<'py, Self>> {
                crate::pattern::check_operand_index(idx.0)?;
                slf.common.borrow_mut().inputs.push((idx.0, p));
                Ok(slf)
            }
        }
    };
    ($ty:ty, output) => {
        #[pymethods]
        impl $ty {
            /// Bind or constrain raw output `slot` (a `Call` is `[ctrl, mem,
            /// result, ...clobbers]`, a `Load` `[value]`) through the returned
            /// `.capture(c)` / `.of_width(w)` / `.of_type(ty)`.
            fn output(slf: Bound<'_, Self>, slot: crate::bigint::Unsigned<usize>) -> PyOutputSlot {
                PyOutputSlot {
                    parent: slf.into_any().unbind(),
                    slot: Some(slot.0),
                }
            }

            /// Like `output`, over some output rather than a fixed slot.
            fn any_output(slf: Bound<'_, Self>) -> PyOutputSlot {
                PyOutputSlot {
                    parent: slf.into_any().unbind(),
                    slot: None,
                }
            }
        }
    };
}

/// `Option::clone` behind a generic, so a `Copy` store does not trip
/// `clone_on_copy`.
fn cloned_opt<T: Clone>(v: &Option<T>) -> Option<T> {
    v.clone()
}

// Generates a node builder from a field spec. Field kinds: `pat` (one
// operand), `multi` (indexed operands), `scalar` (a value), `flag` (no-arg
// bool setter), `region` (the four address-region setters, in call order).

macro_rules! node_builder {
    // Accumulates field decls; a macro call cannot sit in field position.
    (@members $inner:ident [ $($acc:tt)* ] ) => {
        #[derive(Default)]
        struct $inner {
            $($acc)*
        }
    };
    (@members $inner:ident [ $($acc:tt)* ] { pat $name:ident($c:ident) $($coerce:ident)?: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<Py<PyAny>>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { multi $name:ident($idx:ty): $m:ident($compile:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Vec<($idx, Py<PyAny>)>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty) $(.$field:ident)?: $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Option<$store>, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { flag $name:ident: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: bool, ] $($rest)*);
    };
    (@members $inner:ident [ $($acc:tt)* ] { region $name:ident } $($rest:tt)*) => {
        node_builder!(@members $inner [ $($acc)* $name: Vec<RegionCall>, ] $($rest)*);
    };

    // Bound to a `let` first so the `Ref` is released before the operand
    // compile runs Python that can re-enter a setter.
    (@apply $self:ident, $py:ident, $b:ident, { pat $name:ident($c:ident) $($coerce:ident)?: $m:ident = $doc:literal }) => {
        let __slot = clone_opt($py, &$self.inner.borrow().$name);
        if let Some(__p) = __slot {
            $b = $b.$m(expr(__p.bind($py), Slot::$c)?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { multi $name:ident($idx:ty): $m:ident($compile:ident) = $doc:literal }) => {
        let __items: Vec<($idx, Py<PyAny>)> = $self
            .inner
            .borrow()
            .$name
            .iter()
            .map(|(i, p)| (*i, p.clone_ref($py)))
            .collect();
        for (__idx, __p) in __items {
            $b = $b.$m(__idx, expr(__p.bind($py), Slot::$compile)?);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { scalar $name:ident($set:ty => $store:ty) $(.$field:ident)?: $m:ident($arg:ident) = $doc:literal }) => {
        if let Some(__v) = cloned_opt(&$self.inner.borrow().$name) {
            $b = $b.$m(__v);
        }
    };
    (@apply $self:ident, $py:ident, $b:ident, { flag $name:ident: $m:ident = $doc:literal }) => {
        if $self.inner.borrow().$name {
            $b = $b.$m();
        }
    };
    // In call order: the core setters decide which region filter survives.
    (@apply $self:ident, $py:ident, $b:ident, { region $name:ident }) => {
        for &__call in $self.inner.borrow().$name.iter() {
            $b = match __call {
                RegionCall::StackOffset(k) => $b.stack_offset(k),
                RegionCall::StackOnly => $b.stack_only(),
                RegionCall::NonStack => $b.non_stack(),
                RegionCall::HeapOnly => $b.heap_only(),
            };
        }
    };

    (@apply_slot $self:ident, $py:ident, $b:ident, any_input) => {
        let __items: Vec<Py<PyAny>> = $self
            .common
            .borrow()
            .any_inputs
            .iter()
            .map(|p| p.clone_ref($py))
            .collect();
        for __p in __items {
            $b = $b.any_input(expr(__p.bind($py), Slot::Value)?);
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
            $b = $b.input(__idx, expr(__p.bind($py), Slot::Value)?);
        }
    };
    (@apply_slot $self:ident, $py:ident, $b:ident, output) => {
        let __specs = $self.common.borrow().outputs.clone();
        for (__slot, __aspect) in __specs {
            $b = $b.with_output(__slot, __aspect);
        }
    };

    (@traverse $inner:ident, $visit:ident, { pat $name:ident($c:ident) $($coerce:ident)?: $m:ident = $doc:literal }) => {
        if let Some(__p) = $inner.$name.as_ref() { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { multi $name:ident($idx:ty): $m:ident($compile:ident) = $doc:literal }) => {
        for (_, __p) in &$inner.$name { $visit.call(__p)?; }
    };
    (@traverse $inner:ident, $visit:ident, { scalar $name:ident($set:ty => $store:ty) $(.$field:ident)?: $m:ident($arg:ident) = $doc:literal }) => {};
    (@traverse $inner:ident, $visit:ident, { flag $name:ident: $m:ident = $doc:literal }) => {};
    (@traverse $inner:ident, $visit:ident, { region $name:ident }) => {};

    // Mirrors `@traverse`, or a cycle through a slot is uncollectable.
    (@clear $inner:ident, { pat $name:ident($c:ident) $($coerce:ident)?: $m:ident = $doc:literal }) => {
        $inner.$name = None;
    };
    (@clear $inner:ident, { multi $name:ident($idx:ty): $m:ident($compile:ident) = $doc:literal }) => {
        $inner.$name.clear();
    };
    (@clear $inner:ident, { scalar $name:ident($set:ty => $store:ty) $(.$field:ident)?: $m:ident($arg:ident) = $doc:literal }) => {};
    (@clear $inner:ident, { flag $name:ident: $m:ident = $doc:literal }) => {};
    (@clear $inner:ident, { region $name:ident }) => {};

    // Accumulates the setters into one impl: pyo3 cannot see a nested macro
    // inside `#[pymethods]`.
    (@setters $ty:ident [ $($acc:tt)* ] ) => {
        #[pymethods]
        impl $ty {
            $($acc)*
        }
    };
    (@setters $ty:ident [ $($acc:tt)* ] { pat $name:ident($c:ident) $($coerce:ident)?: $m:ident = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, p: Py<PyAny>) -> PyResult<PyRef<'py, Self>> {
                $( let p = $coerce(slf.py(), p)?; )?
                slf.inner.borrow_mut().$name = Some(p);
                Ok(slf)
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { multi $name:ident($idx:ty): $m:ident($compile:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $m<'py>(
                slf: PyRef<'py, Self>,
                idx: crate::bigint::Unsigned<$idx>,
                p: Py<PyAny>,
            ) -> PyResult<PyRef<'py, Self>> {
                check_operand_index(idx.0)?;
                slf.inner.borrow_mut().$name.push((idx.0, p));
                Ok(slf)
            }
        ] $($rest)*);
    };
    (@setters $ty:ident [ $($acc:tt)* ] { scalar $name:ident($set:ty => $store:ty) $(.$field:ident)?: $m:ident($arg:ident) = $doc:literal } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            #[doc = $doc]
            fn $name<'py>(slf: PyRef<'py, Self>, $arg: $set) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name = Some($arg $(.$field)?);
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
    (@setters $ty:ident [ $($acc:tt)* ] { region $name:ident } $($rest:tt)*) => {
        node_builder!(@setters $ty [ $($acc)*
            /// Match only accesses whose address decomposes to exactly
            /// `sp + k`. Replaces any region filter called before it.
            fn stack_offset<'py>(slf: PyRef<'py, Self>, k: i128) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push(RegionCall::StackOffset(k));
                slf
            }
            /// Keep only accesses whose address decomposes to a stack base,
            /// keeping an offset an earlier `stack_offset` pinned.
            fn stack_only<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push(RegionCall::StackOnly);
                slf
            }
            /// Keep only accesses proven heap-rooted or proven not
            /// memory-rooted; an undecided address is rejected. Replaces any
            /// region filter called before it.
            fn non_stack<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push(RegionCall::NonStack);
                slf
            }
            /// Keep only accesses whose address decomposes to a heap base (a
            /// pure allocator's return pointer). Replaces any region filter
            /// called before it.
            fn heap_only<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
                slf.inner.borrow_mut().$name.push(RegionCall::HeapOnly);
                slf
            }
        ] $($rest)*);
    };

    (
        ty: $ty:ident,
        inner: $inner:ident,
        py_name: $py_name:literal,
        doc: $doc:literal,
        core: $core:path,
        core_ty: $core_ty:ty,
        memory: $mem:literal,
        slots: [ $( $slot:ident ),* $(,)? ],
        fields: [ $( $field:tt ),* $(,)? ],
        // A zero-argument module-level constructor.
        $( ctor: $ctor:ident = $ctor_doc:literal, )?
    ) => {
        node_builder!(@members $inner [] $($field)*);

        $(
            #[doc = $ctor_doc]
            #[pyfunction]
            pub fn $ctor() -> $ty {
                $ty::new()
            }
        )?

        #[doc = $doc]
        #[pyclass(name = $py_name, module = "strider.pattern")]
        pub struct $ty {
            inner: std::cell::RefCell<$inner>,
            common: std::cell::RefCell<CommonState>,
        }

        // Deferred, so a long operand chain cannot overflow the stack.
        impl Drop for $ty {
            fn drop(&mut self) {
                if let Ok(mut inner) = self.inner.try_borrow_mut() {
                    defer_drop(std::mem::take(&mut *inner));
                }
                if let Ok(mut common) = self.common.try_borrow_mut() {
                    defer_drop(std::mem::take(&mut *common));
                }
            }
        }

        impl $ty {
            fn new() -> Self {
                Self {
                    inner: std::cell::RefCell::new($inner::default()),
                    common: std::cell::RefCell::new(CommonState::default()),
                }
            }

            /// Applies every set field plus `common.capture` and `caps`.
            fn core_builder(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<$core_ty> {
                // Unused by a field-less builder such as `EntryPat`.
                let _ = &self.inner;
                let _ = py;
                let mut b = $core();
                $( node_builder!(@apply self, py, b, $field); )*
                $( node_builder!(@apply_slot self, py, b, $slot); )*
                for &c in self.common.borrow().capture.iter().chain(caps) {
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

        }

        impl Builder for $ty {
            fn memory(&self) -> bool {
                $mem
            }
            fn common(&self) -> &std::cell::RefCell<CommonState> {
                &self.common
            }
            fn node(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<Expr> {
                Ok(ex(self.core_builder(py, caps)?))
            }
        }

        node_builder!(@setters $ty [] $($field)*);
        builder_common_methods!($ty);
        $( builder_slot_methods!($ty, $slot); )*
    };
}

/// One `load()` / `store()` region-filter call.
#[derive(Clone, Copy)]
enum RegionCall {
    StackOffset(i128),
    StackOnly,
    NonStack,
    HeapOnly,
}

node_builder! {
    ty: PyLoadPat,
    inner: LoadInner,
    py_name: "LoadPat",
    doc: "Typed builder for `Load` node patterns. Chain `.addr(p)`, \
          `.space(s)`, `.mem(m)`, `.bit_width(n)`, `.stack_only()`.",
    core: strider_pattern::load,
    core_ty: strider_pattern::LoadPat,
    memory: false,
    slots: [any_input, input, output],
    fields: [
        { scalar space(crate::sleigh::PyVnSpace => rsleigh::VnSpace).inner: space(s)
            = "Restrict the match to a specific memory space." },
        { pat addr(Value): addr = "Constrain the load's address operand." },
        { pat mem(Memory): mem
            = "Constrain the load's memory predecessor (a memory-producing sub-pattern)." },
        { scalar bit_width(u32 => u32): bit_width(n) = "Filter loads by value width in bits." },
        { region region },
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
    memory: true,
    slots: [any_input, input, output],
    fields: [
        { pat addr(Value): addr = "Constrain the store's address operand." },
        { pat data(Value): data = "Constrain the store's stored-value operand." },
        { scalar space(crate::sleigh::PyVnSpace => rsleigh::VnSpace).inner: space(s)
            = "Restrict the match to a specific memory space." },
        { pat mem(Memory): mem = "Constrain the store's memory predecessor." },
        { scalar bit_width(u32 => u32): bit_width(n) = "Filter stores by data width in bits." },
        { region region },
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

node_builder! {
    ty: PyCallPat,
    inner: CallInner,
    py_name: "CallPat",
    doc: "Typed builder for `Call` node patterns. Chain `.target(p)`, \
          `.arg(idx, p)`, `.mem(m)`, `.ctrl(p)`.",
    core: strider_pattern::call,
    core_ty: strider_pattern::CallPat,
    memory: true,
    slots: [any_input, input, output],
    fields: [
        { pat target(Value) coerce_operand_list: target
            = "Constrain the call target. A raw int matches that literal address \
               (`target(0x1000)`); a list matches any one entry, an empty list \
               nothing." },
        { pat ctrl(Value): ctrl
            = "Match `p` against the Call's direct ctrl predecessor (`inputs[0]`)." },
        { multi args(usize): arg(Value) = "Constrain positional argument `idx` (0-based)." },
        { pat mem(Memory): mem = "Constrain the call's memory predecessor." },
        { flag res: res
            = "When nested as a value operand, pin it to the declared result \
               output, excluding caller-saved clobbers." },
    ],
    ctor: call = "Start a `Call` pattern builder.",
}

/// One output slot of a builder, from `.output(slot)` / `.any_output()`.
/// Each terminal commits one constraint and returns the builder.
#[pyclass(name = "OutputSlotPat", module = "strider.pattern")]
pub struct PyOutputSlot {
    parent: Py<PyAny>,
    slot: Option<usize>,
}

impl PyOutputSlot {
    fn commit(&self, py: Python<'_>, aspect: OutputAspect) -> PyResult<Py<PyAny>> {
        with_builder(self.parent.bind(py), |b| {
            b.common().borrow_mut().outputs.push((self.slot, aspect))
        })
        .ok_or_else(|| {
            pyo3::exceptions::PyTypeError::new_err(
                "output slot terminal holds a parent that is not a pattern builder",
            )
        })?;
        Ok(self.parent.clone_ref(py))
    }
}

#[pymethods]
impl PyOutputSlot {
    fn __repr__(&self) -> String {
        match self.slot {
            Some(slot) => format!("OutputSlotPat(slot={slot})"),
            None => "OutputSlotPat(any)".to_string(),
        }
    }

    /// Bind this output to `c`, a `Capture` or a name.
    fn capture(&self, py: Python<'_>, c: crate::matcher::CaptureKey<'_>) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Capture(c.resolve()?))
    }

    /// Require this output to be `bits` wide.
    fn of_width(&self, py: Python<'_>, bits: u32) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Width(width_arg(bits)?))
    }

    /// Require this output's type to be `ty`, e.g. `"i64"`.
    fn of_type(&self, py: Python<'_>, ty: &str) -> PyResult<Py<PyAny>> {
        self.commit(py, OutputAspect::Ty(parse_value_ty(ty)?))
    }
}

#[pymethods]
impl PyOutputSlot {
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.parent)
    }
}

node_builder! {
    ty: PyCallOtherPat,
    inner: CallOtherInner,
    py_name: "CallOtherPat",
    doc: "Typed builder for `CallOther` node patterns.",
    core: strider_pattern::call_other,
    core_ty: strider_pattern::CallOtherPat,
    memory: true,
    slots: [any_input, input, output],
    fields: [
        { scalar user_op_id(u64 => u64): user_op_id(v)
            = "Constrain the matched node's user-op id." },
        { scalar name(String => String): name(n)
            = "Constrain the matched node's user-op name." },
        { pat ctrl(Value): ctrl
            = "Match the CallOther's control predecessor (`inputs[0]`)." },
        { pat mem(Memory): mem
            = "Match the CallOther's memory predecessor (`inputs[1]`); \
               takes a memory producer (store / mem_phi / call / call_other)." },
        { multi args(usize): arg(Value)
            = "Constrain raw `inputs[idx]` of the matched CallOther." },
        { flag res: res
            = "When nested as a value operand, pin it to the declared result \
               output (excludes implicit-write clobber outputs)." },
    ],
    ctor: call_other = "Start a `CallOther` pattern builder.",
}

node_builder! {
    ty: PyRetPat,
    inner: RetInner,
    py_name: "RetPat",
    doc: "Typed builder for `Return` node patterns.",
    core: strider_pattern::ret,
    core_ty: strider_pattern::RetPat,
    memory: false,
    slots: [any_input, input],
    fields: [
        { pat ctrl(Value): ctrl
            = "Match `p` against the Return's direct ctrl predecessor (`inputs[0]`)." },
        { multi ret_vals(usize): ret_val(Value)
            = "Constrain the return value at position `idx`, raw input slot `idx + 2`." },
    ],
    ctor: ret = "Start a `Return` pattern builder.",
}

node_builder! {
    ty: PyIndirectBranchPat,
    inner: IndirectBranchInner,
    py_name: "IndirectBranchPat",
    doc: "Typed builder for `IndirectBranch` node patterns.",
    core: strider_pattern::indirect_branch,
    core_ty: strider_pattern::IndirectBranchPat,
    memory: false,
    slots: [any_input, input],
    fields: [
        { pat target(Value) coerce_operand_list: target
            = "Constrain the dispatch target (`inputs[2]`). A raw int matches \
               that literal address; a list matches any one entry, an empty list \
               nothing." },
        { pat ctrl(Value): ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
        { pat mem(Memory): mem
            = "Constrain the node's memory predecessor (`inputs[1]`)." },
    ],
    ctor: indirect_branch = "Start an `IndirectBranch` pattern builder.",
}

node_builder! {
    ty: PyUnreachablePat,
    inner: UnreachableInner,
    py_name: "UnreachablePat",
    doc: "Typed builder for `Unreachable` node patterns.",
    core: strider_pattern::unreachable,
    core_ty: strider_pattern::UnreachablePat,
    memory: false,
    slots: [any_input, input],
    fields: [
        { pat ctrl(Value): ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
    ],
    ctor: unreachable = "Start an `Unreachable` pattern builder.",
}

node_builder! {
    ty: PySwitchPat,
    inner: SwitchInner,
    py_name: "SwitchPat",
    doc: "Typed builder for `Switch` node patterns.",
    core: strider_pattern::switch,
    core_ty: strider_pattern::SwitchPat,
    memory: false,
    slots: [any_input, input, output],
    fields: [
        { pat selector(Value) coerce_operand_list: selector
            = "The value the switch dispatches on (`inputs[1]`). A raw int \
               matches that literal value; a list matches any one entry, an \
               empty list nothing." },
        { pat ctrl(Value): ctrl
            = "Match `p` against the node's direct ctrl predecessor (`inputs[0]`)." },
    ],
    ctor: switch = "Start a `Switch` pattern builder.",
}

node_builder! {
    ty: PyIfPat,
    inner: IfInner,
    py_name: "IfPat",
    doc: "Typed builder for `If` node patterns.",
    core: strider_pattern::if_else,
    core_ty: strider_pattern::IfPat,
    memory: false,
    slots: [any_input, input, output],
    fields: [
        { pat cond(Value): cond = "Constrain the If's condition operand." },
        { pat ctrl(Value): ctrl
            = "Match `p` against the If's direct ctrl predecessor (`inputs[0]`)." },
        { scalar capture_true(PyCapture => Capture).inner: capture_true(c)
            = "Bind the If's true control-output value to `c`, the edge \
               operand `dominates` / `phi_input_from_edge` take." },
        { scalar capture_false(PyCapture => Capture).inner: capture_false(c)
            = "Bind the If's false control-output value to `c`. See \
               `capture_true`." },
    ],
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

/// A CFG relation between captures, passed as
/// `Function.find_all([...], constraints=[...])` to filter joined matches.
/// Built by `dominates`, `phi_input_from_edge`, `negate`, `any_of`, `all_of`.
#[pyclass(name = "JoinConstraint", module = "strider.pattern.constraints")]
pub struct PyJoinConstraint {
    pub(crate) inner: Arc<ConstraintTree>,
    /// The constraints this one was built from, kept visible to the GC.
    operands: Vec<PyObject>,
    /// The `JoinPredicate` handles the closures in `inner` run.
    predicates: Vec<PredicateFn>,
}

/// A `JoinPredicate` instance baked into a `Where` closure.
type PredicateFn = Arc<PyObject>;

const MAX_CONSTRAINT_NESTING: u32 = 512;

/// Cap on the materialised node count, which repeated sharing
/// (`all_of([c, c])`) grows exponentially in the depth.
const MAX_CONSTRAINT_NODES: u64 = 65_536;

/// A composed constraint, materialised into a `JoinConstraint` at the query
/// boundary.
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

    pub(crate) fn materialize(&self) -> JoinConstraint {
        let all = |cs: &[Arc<Self>]| cs.iter().map(|c| c.materialize()).collect();
        match &self.node {
            ConstraintNode::Leaf(c) => c.clone(),
            ConstraintNode::Not(c) => JoinConstraint::Not(Box::new(c.materialize())),
            ConstraintNode::Any(cs) => JoinConstraint::Or(all(cs)),
            ConstraintNode::All(cs) => JoinConstraint::And(all(cs)),
        }
    }
}

// Deferred, so a long chain cannot overflow the stack.
impl Drop for PyJoinConstraint {
    fn drop(&mut self) {
        defer_drop(std::mem::take(&mut self.operands));
    }
}

impl PyJoinConstraint {
    fn leaf(inner: JoinConstraint) -> Self {
        Self {
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
/// `a`. Each is a captured node or an `If` branch-edge capture
/// (`capture_true` / `capture_false`). No single incoming edge dominates a
/// merge `Phi`; use `phi_input_from_edge`. A capture with no control position
/// (a `load`, `store` or arithmetic node) drops the match, negated or not.
#[pyfunction]
pub fn dominates(a: PyRef<'_, PyCapture>, b: PyRef<'_, PyCapture>) -> PyJoinConstraint {
    PyJoinConstraint::leaf(JoinConstraint::Dominates {
        dominator: a.inner,
        dominated: b.inner,
    })
}

/// `phi` merges `value` from the branch `edge` binds (an `If`'s
/// `capture_true` / `capture_false`).
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

/// Passes when `c`, a `JoinConstraint` or `JoinPredicate`, fails.
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

/// `node` over every listed constraint, coerced.
fn composite(
    constraints: &[Bound<'_, PyAny>],
    node: fn(Vec<Arc<ConstraintTree>>) -> ConstraintNode,
) -> PyResult<PyJoinConstraint> {
    let mut predicates = Vec::new();
    let coerced = constraints
        .iter()
        .map(|c| coerce_tree(c, &mut predicates))
        .collect::<PyResult<_>>()?;
    Ok(PyJoinConstraint {
        inner: ConstraintTree::new(node(coerced))?,
        operands: constraints.iter().map(|c| c.clone().unbind()).collect(),
        predicates,
    })
}

/// Passes when any listed constraint passes; an empty list passes nothing.
#[pyfunction]
pub fn any_of(constraints: Vec<Bound<'_, PyAny>>) -> PyResult<PyJoinConstraint> {
    composite(&constraints, ConstraintNode::Any)
}

/// Passes when every listed constraint passes; an empty list passes
/// everything.
#[pyfunction]
pub fn all_of(constraints: Vec<Bound<'_, PyAny>>) -> PyResult<PyJoinConstraint> {
    composite(&constraints, ConstraintNode::All)
}

/// Base class for a user-defined join constraint: override `constraint`, and
/// optionally `captures` to correlate captures across patterns.
#[pyclass(
    name = "JoinPredicate",
    module = "strider.pattern.constraints",
    subclass
)]
pub struct PyJoinPredicate;

#[pymethods]
impl PyJoinPredicate {
    /// Base initialiser; ignores any args.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        Self
    }

    /// The captures this predicate correlates; the base returns none.
    fn captures(&self) -> Vec<Py<PyCapture>> {
        Vec::new()
    }

    /// Whether the joined match survives, as a `bool`; anything else is a
    /// `TypeError`. The base raises `NotImplementedError`.
    #[allow(unused_variables)]
    fn constraint(&self, m: &Bound<'_, PyAny>) -> PyResult<bool> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "JoinPredicate.constraint must be overridden by subclass",
        ))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        Ok(format!("{}()", slf.get_type().name()?))
    }
}

/// A `JoinConstraint` as-is, or a `JoinPredicate` wrapped into a `Where`
/// whose handle is also pushed onto `held` for the GC. Anything else is a
/// `TypeError`.
pub(crate) fn coerce_tree(
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

/// Runs a `JoinPredicate.constraint` on one `Match` spanning the joined
/// tuple. A raise is stashed for the query boundary and drops the row.
fn run_join_predicate(tuple: &sp::JoinedMatch, obj: &PyObject) -> Option<bool> {
    run_predicate(
        "JoinPredicate",
        |_| tuple.clone(),
        |py, m| obj.call_method1(py, "constraint", (m,)),
    )
}

node_builder! {
    ty: PyPhiPat,
    inner: PhiInner,
    py_name: "PhiPat",
    doc: "Typed builder for tagged-`Phi` patterns.",
    core: strider_pattern::phi,
    core_ty: strider_pattern::PhiPat,
    memory: false,
    slots: [any_input, input, output],
    fields: [
        { scalar for_vn(crate::sleigh::PyVn => rsleigh::Vn).inner: for_vn(vn)
            = "Restrict the match to phi nodes tagged `vn` or a register containing \
               it, so `eax` matches a phi tagged `rax`." },
        { multi inputs(usize): phi_input(Value)
            = "Constrain the value arriving from predecessor `idx`, raw input \
               slot `idx + 1`. `.input(i, p)` addresses the raw slot instead." },
        { pat phi_token(Value): phi_token
            = "Constrain the phi's ownership edge, the PhiToken input at raw \
               slot 0. Only var()/anything() can bind it." },
    ],
    ctor: phi = "Start a tagged-`Phi` pattern builder.",
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
    memory: true,
    slots: [any_input, input, output],
    fields: [
        { multi inputs(usize): phi_input(Memory)
            = "Constrain the memory token arriving from predecessor `idx`, raw \
               input slot `idx + 1`. `.input(i, p)` addresses the raw slot \
               instead, and takes a value operand." },
        { pat phi_token(Value): phi_token
            = "Constrain the MemPhi's ownership edge, the PhiToken input at \
               raw slot 0. Only var()/anything() can bind it." },
    ],
    ctor: mem_phi = "Start a `MemPhi` pattern builder.",
}

macro_rules! phi_edge_links {
    ($ty:ty) => {
        #[pymethods]
        impl $ty {
            /// Merge `value` from the branch edge `edge` binds (an `if_else`
            /// `capture_true` / `capture_false`). A pattern `value` becomes an
            /// input; a `Capture` names one another pattern binds. Pass
            /// `constraints()` to the query.
            fn input_from<'py>(
                slf: PyRef<'py, Self>,
                edge: crate::matcher::CaptureKey<'_>,
                value: &Bound<'py, PyAny>,
            ) -> PyResult<PyRef<'py, Self>> {
                let py = slf.py();
                let edge = edge.resolve()?;
                let value_cap = match value.extract::<crate::matcher::CaptureKey<'_>>() {
                    Ok(key) => key.resolve()?,
                    Err(_) => {
                        let cap = Capture::new();
                        let arm = Bound::new(py, one_of(vec![value.clone().unbind()]))?;
                        let captured = derive(&arm, Wrap::Capture(cap), None)?;
                        slf.common
                            .borrow_mut()
                            .any_inputs
                            .push(Py::new(py, captured)?.into_any());
                        cap
                    }
                };
                let mut common = slf.common.borrow_mut();
                common.capture.get_or_insert_with(Capture::new);
                common.edge_links.push((edge, value_cap));
                drop(common);
                Ok(slf)
            }

            /// One `phi_input_from_edge` per `input_from`, on this phi's
            /// current capture.
            fn constraints(&self) -> Vec<PyJoinConstraint> {
                let common = self.common.borrow();
                let Some(phi) = common.capture else {
                    return Vec::new();
                };
                common
                    .edge_links
                    .iter()
                    .map(|&(edge, value)| {
                        PyJoinConstraint::leaf(JoinConstraint::PhiInputFromEdge {
                            phi,
                            edge,
                            value,
                        })
                    })
                    .collect()
            }
        }
    };
}

phi_edge_links!(PyPhiPat);
phi_edge_links!(PyMemPhiPat);

node_builder! {
    ty: PyEntryPat,
    inner: EntryInner,
    py_name: "EntryPat",
    doc: "Typed builder for the function's unique `Entry` node. Nests as a \
          control operand, e.g. `region().any_input(entry())`.",
    core: strider_pattern::entry,
    core_ty: strider_pattern::EntryPat,
    memory: false,
    slots: [output],
    fields: [],
    ctor: entry = "Matches the function's unique `Entry` node.",
}

node_builder! {
    ty: PyRegionPat,
    inner: RegionInner,
    py_name: "RegionPat",
    doc: "Typed builder for `Region` (CFG-merge) node patterns. Input `i` is \
          one predecessor's control edge, bound by entry() / region() or a \
          wildcard. Nests as a control operand.",
    core: strider_pattern::region,
    core_ty: strider_pattern::RegionPat,
    memory: false,
    slots: [any_input, input, output],
    fields: [],
    ctor: region = "Matches any CFG-merge `Region` node.",
}

/// Typed builder for `FunctionArg` carrier patterns. Chain `.index(i)`,
/// `.source_register(vn)`, `.source_stack(space, offset)`.
#[pyclass(name = "FunctionArgPat", module = "strider.pattern")]
pub struct PyFunctionArgPat {
    source: std::cell::Cell<Option<FunctionArgSource>>,
    /// Which index space `index` names; `Any` unless a constructor pinned it.
    class: strider_pattern::FunctionArgClass,
    index: std::cell::Cell<Option<u32>>,
    common: std::cell::RefCell<CommonState>,
}

// Deferred, so a long operand chain cannot overflow the stack.
impl Drop for PyFunctionArgPat {
    fn drop(&mut self) {
        if let Ok(mut common) = self.common.try_borrow_mut() {
            defer_drop(std::mem::take(&mut *common));
        }
    }
}

impl PyFunctionArgPat {
    fn new(
        class: strider_pattern::FunctionArgClass,
        index: Option<u32>,
        source: Option<FunctionArgSource>,
    ) -> Self {
        Self {
            source: std::cell::Cell::new(source),
            class,
            index: std::cell::Cell::new(index),
            common: std::cell::RefCell::new(CommonState::default()),
        }
    }

    fn traverse_operands(&self, visit: &pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        let _ = visit;
        Ok(())
    }

    fn clear_operands(&self) {}
}

impl Builder for PyFunctionArgPat {
    fn common(&self) -> &std::cell::RefCell<CommonState> {
        &self.common
    }
    fn node(&self, _py: Python<'_>, caps: &[Capture]) -> PyResult<Expr> {
        let mut b = strider_pattern::any_function_arg().class(self.class);
        if let Some(s) = self.source.get() {
            b = b.source(s);
        }
        if let Some(i) = self.index.get() {
            b = b.index(i);
        }
        for &c in self.common.borrow().capture.iter().chain(caps) {
            b = b.capture(c);
        }
        Ok(ex(b))
    }
}

#[pymethods]
impl PyFunctionArgPat {
    /// Constrain the match to argument at ABI position `i`.
    fn index(slf: PyRef<'_, Self>, i: u32) -> PyRef<'_, Self> {
        slf.index.set(Some(i));
        slf
    }
    /// Constrain to an argument arriving in register `vn` or one containing it.
    fn source_register(slf: PyRef<'_, Self>, vn: crate::sleigh::PyVn) -> PyRef<'_, Self> {
        slf.source.set(Some(FunctionArgSource::Register(vn.inner)));
        slf
    }
    /// Constrain to an argument arriving on the stack at `(space, offset)`.
    fn source_stack(
        slf: PyRef<'_, Self>,
        space: crate::sleigh::PyVnSpace,
        offset: i128,
    ) -> PyRef<'_, Self> {
        slf.source.set(Some(stack_source(space, offset)));
        slf
    }
}

fn stack_source(space: crate::sleigh::PyVnSpace, offset: i128) -> FunctionArgSource {
    FunctionArgSource::Stack {
        space: space.inner,
        offset,
    }
}

builder_common_methods!(PyFunctionArgPat);

/// Start a `FunctionArg` pattern builder at integer-class index `i`.
#[pyfunction]
pub fn function_arg(i: crate::bigint::Unsigned<u32>) -> PyFunctionArgPat {
    PyFunctionArgPat::new(sp::FunctionArgClass::Integer, Some(i.0), None)
}

/// Start a `FunctionArg` pattern builder at float-class index `i`, counting
/// only float parameters.
#[pyfunction]
pub fn function_arg_float(i: crate::bigint::Unsigned<u32>) -> PyFunctionArgPat {
    PyFunctionArgPat::new(sp::FunctionArgClass::Float, Some(i.0), None)
}

/// Start a `FunctionArg` pattern builder matching any index of either class.
#[pyfunction]
pub fn any_function_arg() -> PyFunctionArgPat {
    PyFunctionArgPat::new(sp::FunctionArgClass::Any, None, None)
}

/// Match a `FunctionArg` whose source is register `vn` or a register containing it.
#[pyfunction]
pub fn function_arg_reg(vn: crate::sleigh::PyVn) -> PyFunctionArgPat {
    let source = FunctionArgSource::Register(vn.inner);
    PyFunctionArgPat::new(sp::FunctionArgClass::Any, None, Some(source))
}

/// Match a `FunctionArg` whose source is a specific stack slot.
#[pyfunction]
pub fn function_arg_stack(space: crate::sleigh::PyVnSpace, offset: i128) -> PyFunctionArgPat {
    let source = stack_source(space, offset);
    PyFunctionArgPat::new(sp::FunctionArgClass::Any, None, Some(source))
}

macro_rules! binary_op_builder {
    ($ty:ident, $py_name:literal, $doc:literal, $ctor:ident($parse:path), $ctor_doc:literal) => {
        #[doc = $ctor_doc]
        #[pyfunction]
        pub fn $ctor(op: &str, l: Py<PyAny>, r: Py<PyAny>) -> PyResult<$ty> {
            let op = $parse(op)?;
            let make = move |l, r| ex(sp::$ctor(op, l, r));
            let name = concat!(stringify!($ctor), "()");
            Ok($ty::new(bin(name, Kind::Build, Box::new(make), l, r)))
        }

        #[doc = $doc]
        #[pyclass(name = $py_name, module = "strider.pattern")]
        pub struct $ty {
            /// The op over its two operands, fixed at construction.
            repr: Repr,
            ordered: std::cell::Cell<bool>,
            common: std::cell::RefCell<CommonState>,
        }

        // Deferred, so a long operand chain cannot overflow the stack.
        impl Drop for $ty {
            fn drop(&mut self) {
                defer_drop(std::mem::take(&mut self.repr.args));
                if let Ok(mut common) = self.common.try_borrow_mut() {
                    defer_drop(std::mem::take(&mut *common));
                }
            }
        }

        impl $ty {
            /// The operand slots, for `__traverse__`.
            fn traverse_operands(
                &self,
                visit: &pyo3::PyVisit<'_>,
            ) -> Result<(), pyo3::PyTraverseError> {
                self.repr.args.iter().try_for_each(|a| visit.call(a))
            }

            /// Operands are fixed at construction, so no cycle closes through
            /// them.
            fn clear_operands(&self) {}

            fn new(repr: Repr) -> Self {
                Self {
                    repr,
                    ordered: std::cell::Cell::new(false),
                    common: std::cell::RefCell::new(CommonState::default()),
                }
            }
        }

        impl Builder for $ty {
            fn common(&self) -> &std::cell::RefCell<CommonState> {
                &self.common
            }
            /// `.ordered()` pins the root ahead of the capture.
            fn node(&self, py: Python<'_>, caps: &[Capture]) -> PyResult<Expr> {
                let mut e = self.repr.expr(py, Slot::Value)?;
                if self.ordered.get() {
                    e = Wrap::Ordered.apply(py, e, None);
                }
                for &c in self.common.borrow().capture.iter().chain(caps) {
                    e = Wrap::Capture(c).apply(py, e, None);
                }
                Ok(e)
            }
        }

        #[pymethods]
        impl $ty {
            /// Match operands left to right only, not commuted.
            fn ordered(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
                slf.ordered.set(true);
                slf
            }
        }

        builder_common_methods!($ty);
    };
}

binary_op_builder!(
    PyIntBinaryPat,
    "IntBinaryPat",
    "Typed builder for an integer binary-op pattern.",
    int_binary(parse_int_binary_op),
    "Build an `IntBinaryOp` pattern builder for the named `op`."
);
binary_op_builder!(
    PyFloatBinaryPat,
    "FloatBinaryPat",
    "Typed builder for a float binary-op pattern.",
    float_binary(parse_float_binary_op),
    "Build a `FloatBinaryOp` pattern builder for the named `op`."
);
binary_op_builder!(
    PyBoolBinaryPat,
    "BoolBinaryPat",
    "Typed builder for a boolean binary-op pattern (`IntBinaryOp` at `I1`).",
    bool_binary(parse_bool_binary_op),
    "Build a boolean binary pattern builder for the named `op`."
);

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // The rosters behind the `Literal` type aliases in `strider/__init__.py`.
    for (name, roster) in [
        ("INT_CMP_OPS", INT_CMP_OPS),
        ("INT_BINARY_OPS", INT_BINARY_OPS),
        ("BOOL_BINARY_OPS", BOOL_BINARY_OPS),
        ("FLOAT_BINARY_OPS", FLOAT_BINARY_OPS),
        ("EXTEND_OPS", EXTEND_OPS),
    ] {
        m.add(name, pyo3::types::PyTuple::new_bound(py, roster))?;
    }
    // Lowercase (`FromStr`) then uppercase (what the readers emit), the stub's
    // order.
    let widths = strider_ir::ValueType::SUPPORTED;
    let value_types: Vec<String> = widths
        .iter()
        .map(|t| t.to_string())
        .chain(widths.iter().map(|t| format!("{t:?}")))
        .collect();
    m.add(
        "VALUE_TYPES",
        pyo3::types::PyTuple::new_bound(py, value_types),
    )?;
    bind!(m;
        classes [
            PyCapture, PyPat, PyIntBinaryPat, PyFloatBinaryPat, PyBoolBinaryPat,
            PyCallPat, PyOutputSlot, PyCallOtherPat, PyRetPat, PyIndirectBranchPat,
            PyUnreachablePat, PySwitchPat, PyIfPat, PyLoadPat, PyStorePat, PyPhiPat,
            PyMemPhiPat, PyFunctionArgPat, PyEntryPat, PyRegionPat, PyCastMask,
        ];
        fns [
            any_, var, value_of_width, inputs_of_width, bool_inputs, int_const,
            int_const_any_width, bool_const, float_const, any_int, any_bool, any_float,
            initial_var, initial_var_for, one_of, first_of, function_arg,
            any_function_arg, function_arg_float, function_arg_reg, function_arg_stack,
            phi, phi_for, mem_phi, entry, region, predicate, float_is_nan, float_le,
            load, store, call, call_other, ret, indirect_branch, unreachable, switch,
            if_, int_binary, bool_binary, float_binary, any_int_binary, any_int_unary,
            any_int_cmp, any_bool_binary, any_float_binary, any_float_unary,
            any_float_cmp,
        ];
    );
    register_value_ops(m)?;

    register_constraints(py, m)?;
    Ok(())
}

/// `parent` must be the `pattern` module, so `strider.pattern.constraints`
/// resolves by attribute.
fn register_constraints(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "constraints")?;
    bind!(&m;
        classes [PyJoinConstraint, PyJoinPredicate];
        fns [dominates, phi_input_from_edge, negate, any_of, all_of];
    );

    parent.add_submodule(&m)?;
    // `__init__.py` adds the `sys.modules` entry.
    Ok(())
}
