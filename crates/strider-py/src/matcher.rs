use pyo3::basic::CompareOp;
use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::function::FunctionAt;
use crate::pattern::{PyCapture, intern_str};

/// Result of a successful pattern match.
///
/// Every capture accessor raises `StriderError` once the function has been
/// compacted or otherwise reshuffled, rather than dereferencing the stored
/// `ValueId`s against the new arena.
#[pyclass(name = "Match", module = "strider.pattern", frozen)]
pub struct PyMatch {
    /// Per-input-pattern sub-matches, non-empty. Shared captures are already
    /// unified, so the `Match` presents the union of every pattern's captures.
    pub(crate) inner: Vec<strider_pattern::Match>,
    pub(crate) at: FunctionAt,
}

/// A `Capture` instance or a string name, looked up in the global intern
/// table.
#[derive(FromPyObject)]
pub enum CaptureKey<'py> {
    Capture(Bound<'py, PyCapture>),
    Str(String),
}

impl CaptureKey<'_> {
    pub(crate) fn resolve(self) -> PyResult<strider_pattern::Capture> {
        match self {
            CaptureKey::Capture(c) => Ok(c.borrow().inner),
            CaptureKey::Str(s) => intern_str(s.as_str()),
        }
    }
}

impl PyMatch {
    /// [`FunctionAt::read`] for this match.
    fn read<R>(&self, py: Python<'_>, f: impl FnOnce(&strider_ir::Function) -> R) -> PyResult<R> {
        self.at.read(py, "Match", f)
    }

    /// The first sub-match binding `cap`; the join already unified shared
    /// captures, so every such sub-match agrees.
    fn binding_for(&self, cap: strider_pattern::Capture) -> Option<&strider_pattern::Match> {
        self.inner.iter().find(|m| m.is_bound(cap))
    }

    fn is_bound(&self, cap: strider_pattern::Capture) -> bool {
        self.inner.iter().any(|m| m.is_bound(cap))
    }

    /// What `cap` bound, as `(is a value, raw id)`, or `None` when unbound.
    fn bound_identity(
        &self,
        py: Python<'_>,
        cap: strider_pattern::Capture,
    ) -> PyResult<Option<(bool, u32)>> {
        self.read(py, |function| {
            let m = self.binding_for(cap)?;
            match m.value(cap) {
                Some(v) => Some((true, v.as_u32())),
                None => m.node(cap, function.graph()).map(|n| (false, n.as_u32())),
            }
        })
    }

    /// Resolve `cap` to a node and read it under a single borrow.
    pub(crate) fn read_bound<R>(
        &self,
        py: Python<'_>,
        cap: strider_pattern::Capture,
        read: impl FnOnce(&strider_ir::Function, strider_ir::node::NodeId) -> Option<R>,
    ) -> PyResult<Option<R>> {
        self.read(py, |function| {
            self.binding_for(cap)
                .and_then(|m| m.node(cap, function.graph()))
                .and_then(|nid| read(function, nid))
        })
    }

    /// `Capture('x'): BoundCapture(...)` per bound capture, by capture id.
    fn bindings_repr(&self, py: Python<'_>) -> PyResult<String> {
        self.read(py, |function| {
            // First node seen per capture id wins.
            let mut seen = std::collections::BTreeMap::new();
            for m in &self.inner {
                for (cap_id, node_id) in m.capture_signature(function.graph()) {
                    seen.entry(cap_id).or_insert(node_id);
                }
            }
            let entries: Vec<String> = seen
                .into_iter()
                .map(|(cap_id, node_id)| {
                    let key = crate::pattern::capture_display(cap_id);
                    let value = function
                        .graph()
                        .node_id_from_u32(node_id)
                        .and_then(|nid| crate::node::uint_of(function, nid))
                        .map_or_else(|| "<node>".to_string(), |v| format!("{v:#x}"));
                    format!("{key}: BoundCapture({value})")
                })
                .collect();
            entries.join(", ")
        })
    }
}

/// Dedup key of `inner`: per sub-match, its root unless `ignore_root`, then
/// its length-prefixed `(capture-id, node-id)` signature.
pub(crate) fn dedup_key(
    inner: &[strider_pattern::Match],
    function: &strider_ir::Function,
    ignore_root: bool,
) -> Vec<u32> {
    let mut key = Vec::new();
    for m in inner {
        if !ignore_root {
            key.push(m.root().as_u32());
        }
        let sig = m.capture_signature(function.graph());
        key.push(sig.len() as u32);
        key.extend(sig.into_iter().flat_map(|(cap, node)| [cap, node]));
    }
    key
}

/// Raise where the `_opt` getter would return `None`.
fn or_missing<T>(v: Option<T>, what: &str) -> PyResult<T> {
    v.ok_or_else(|| {
        into_strider_err(anyhow::anyhow!(
            "capture has no {what} (unbound, or bound to a node without one); \
             call {what}_opt for None instead"
        ))
    })
}

#[pymethods]
impl PyMatch {
    /// Exposes the `Py<PyFunction>` edge to the cyclic collector.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.at.function)
    }

    /// Node id where the top-level pattern matched.
    #[getter]
    fn root(&self, py: Python<'_>) -> PyResult<u32> {
        self.read(py, |_| self.inner[0].root().as_u32())
    }

    /// One root node id per pattern passed to the query.
    #[getter]
    fn roots(&self, py: Python<'_>) -> PyResult<Vec<u32>> {
        self.read(py, |_| {
            self.inner.iter().map(|m| m.root().as_u32()).collect()
        })
    }

    /// `m[c]` / `m["name"]`: a `BoundCapture` whose `has` says whether `c`
    /// bound, and which converts and compares a numeric capture
    /// (`int(m[c])`, `m[c] == 0x10`).
    fn __getitem__(slf: Bound<'_, Self>, key: CaptureKey<'_>) -> PyResult<PyBoundCapture> {
        let py = slf.py();
        let cap = key.resolve()?;
        slf.get().read(py, |_| ())?;
        Ok(PyBoundCapture {
            match_: slf.unbind(),
            cap: Py::new(py, PyCapture { inner: cap })?,
        })
    }

    /// True when `key` (a `Capture` or string name) is bound in this match.
    fn __contains__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<bool> {
        self.has(py, key)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let roots: Vec<u32> = self.inner.iter().map(|m| m.root().as_u32()).collect();
        // A repr must not raise; a stale / unreadable function drops the dict.
        let bindings = self.bindings_repr(py).unwrap_or_default();
        format!("Match(roots={roots:?}, {{{bindings}}})")
    }

    /// True when `key` is bound in this match. Raises once the function has
    /// been compacted.
    fn has(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        self.read(py, |_| self.is_bound(cap))
    }

    /// Sorted, deduped machine-instruction addresses whose lift or subsequent
    /// rewrite contributed to the value of the node bound to `key`. Empty when
    /// `key` is unbound or binds an exempt structural kind.
    fn asm_fingerprint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Vec<u64>> {
        match self.node_opt(py, key)? {
            Some(node) => node.asm_fingerprint(py),
            None => Ok(Vec::new()),
        }
    }

    /// A `Node` handle on the node bound to `key`. Raises when `key` is
    /// unbound; `node_opt` returns `None`.
    fn node(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<crate::node::PyNode> {
        or_missing(self.node_opt(py, key)?, "node")
    }

    /// A `Node` handle, or `None` when `key` is unbound.
    fn node_opt(
        &self,
        py: Python<'_>,
        key: CaptureKey<'_>,
    ) -> PyResult<Option<crate::node::PyNode>> {
        let cap = key.resolve()?;
        self.read(py, |function| {
            let nid = self.binding_for(cap)?.node(cap, function.graph())?;
            Some(self.at.node(py, nid))
        })
    }
}

/// Each row generates a raising getter and its `_opt` twin over the `PyNode`
/// accessor `$acc`.
macro_rules! match_getters {
    ($(
        #[doc = $rdoc:literal] $rpy:literal $rname:ident,
        #[doc = $odoc:literal] $opy:literal $oname:ident
            : $ret:ty = $acc:ident
    );+ $(;)?) => {
        #[pymethods]
        impl PyMatch {
            $(
                #[doc = $rdoc]
                #[pyo3(name = $rpy)]
                fn $rname(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<$ret> {
                    or_missing(self.$oname(py, key)?, $rpy)
                }

                #[doc = $odoc]
                #[pyo3(name = $opy)]
                fn $oname(
                    &self,
                    py: Python<'_>,
                    key: CaptureKey<'_>,
                ) -> PyResult<Option<$ret>> {
                    match self.node_opt(py, key)? {
                        Some(node) => node.$acc(py),
                        None => Ok(None),
                    }
                }
            )+
        }
    };
}

match_getters! {
    #[doc = "The operation variant (`\"Add\"`, `\"Less\"`); raises if unbound or the node carries none."]
    "op" op,
    #[doc = "The operation variant, or `None`."]
    "op_opt" op_opt : String = op;

    #[doc = "The value-output type (`\"I1\"`, `\"I64\"`, `\"F64\"`); raises if unbound or the node has none."]
    "value_type" value_type,
    #[doc = "The value-output type, or `None`."]
    "value_type_opt" value_type_opt : String = value_type;

    #[doc = "The capture's value as an unsigned `int`; raises if unbound or not an integer node."]
    "uint" uint,
    #[doc = "The unsigned value, or `None`."]
    "uint_opt" uint_opt : Py<PyAny> = uint;

    #[doc = "The signed value, sign-interpreted at the node's width; raises if unbound or not an integer node."]
    "sint" int_,
    #[doc = "The signed value, or `None`."]
    "sint_opt" int_opt : Py<PyAny> = sint;

    #[doc = "The capture's value as a `bool`; raises if unbound or not a boolean node."]
    "boolean" bool_,
    #[doc = "The boolean value, or `None`."]
    "boolean_opt" bool_opt : bool = boolean;

    #[doc = "The capture's value as raw float bits; raises if unbound or not a float node."]
    "float_bits" float_bits,
    #[doc = "The raw float bits, or `None`."]
    "float_bits_opt" float_bits_opt : u64 = float_bits;

    #[doc = "The varnode a captured node names: an `InitialVar`'s entry-read varnode, the register a `Call` returns in, or whatever varnode the sla gives a `CallOther`'s result (a `unique` temporary, a tracked register, or none). Raises for any other kind. For a multi-output call, the first value output."]
    "vn" vn,
    #[doc = "The varnode, or `None`."]
    "vn_opt" vn_opt : crate::sleigh::PyVn = vn;
}

/// Capture `cap` bound to a specific `Match`: `m[c]`. Says whether it bound
/// and converts a numeric one.
#[pyclass(name = "BoundCapture", module = "strider.pattern", frozen)]
pub struct PyBoundCapture {
    match_: Py<PyMatch>,
    cap: Py<PyCapture>,
}

impl PyBoundCapture {
    fn key<'py>(&self, py: Python<'py>) -> CaptureKey<'py> {
        CaptureKey::Capture(self.cap.bind(py).clone())
    }

    /// The match's `uint_opt` for this capture, at any width. `None` for a
    /// non-constant binding.
    fn unsigned(&self, py: Python<'_>) -> PyResult<Option<strider_ir::UBig>> {
        let cap = self.cap.get().inner;
        self.match_.get().read_bound(py, cap, crate::node::uint_of)
    }

    /// The match's `sint_opt` for this capture, at any width.
    fn signed(&self, py: Python<'_>) -> PyResult<Option<strider_ir::IBig>> {
        let cap = self.cap.get().inner;
        self.match_.get().read_bound(py, cap, crate::node::sint_of)
    }

    /// Whether both bound the same value of the same function.
    fn same_binding(&self, py: Python<'_>, other: &Self) -> PyResult<bool> {
        let (a, b) = (self.match_.get(), other.match_.get());
        if !a.at.function.is(&b.at.function) {
            return Ok(false);
        }
        let mine = a.bound_identity(py, self.cap.borrow(py).inner)?;
        Ok(mine.is_some() && mine == b.bound_identity(py, other.cap.borrow(py).inner)?)
    }
}

#[pymethods]
impl PyBoundCapture {
    /// Exposes both `Py` handles to the cyclic collector.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.match_)?;
        visit.call(&self.cap)
    }

    /// Whether the capture bound in this match.
    #[getter]
    fn has(&self, py: Python<'_>) -> PyResult<bool> {
        self.match_.get().has(py, self.key(py))
    }

    fn __int__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.match_.get().uint(py, self.key(py))
    }
    fn __index__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.__int__(py)
    }
    /// Raises: a `BoundCapture` has no truth value.
    fn __bool__(&self) -> PyResult<bool> {
        Err(pyo3::exceptions::PyTypeError::new_err(
            "a BoundCapture has no truth value: use m[c].has (or c in m) for \
             whether it bound, int(m[c]) for its value",
        ))
    }
    /// Equal to an int matching the captured constant in either signed or
    /// unsigned spelling (`-8` and `0xFFFFFFF8` both match a 32-bit -8), or to
    /// a `BoundCapture` whose constant agrees. Two non-integer captures are
    /// equal when they bound the same value of the same function; an unbound
    /// one equals nothing. Unhashable.
    fn __richcmp__(
        &self,
        py: Python<'_>,
        other: Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<PyObject> {
        let (u, s) = (self.unsigned(py)?, self.signed(py)?);
        // Ahead of the int path: under abi3 a `u128` extract shifts its
        // operand, and a `BoundCapture` has no `__rshift__`.
        let equal = if let Ok(o) = other.downcast::<PyBoundCapture>() {
            let o = o.get();
            let (ou, os) = (o.unsigned(py)?, o.signed(py)?);
            if u.is_none() && ou.is_none() {
                self.same_binding(py, o)?
            } else {
                (u.is_some() && u == ou) || (s.is_some() && s == os)
            }
        } else {
            // An integer wider than any `ValueType` is unequal, not an error.
            match crate::bigint::py_big_int(&other)?.and_then(crate::bigint::PyBigInt::in_range) {
                Some(o) => {
                    u.is_some_and(|u| strider_ir::IBig::from(u) == o) || s.is_some_and(|s| s == o)
                }
                None => false,
            }
        };
        Ok(match op {
            CompareOp::Eq => equal.into_py(py),
            CompareOp::Ne => (!equal).into_py(py),
            _ => py.NotImplemented(),
        })
    }
    /// `BoundCapture(<value>)`, `(<node>)`, `(unbound)`, or `(stale)`.
    fn __repr__(&self, py: Python<'_>) -> String {
        match (self.unsigned(py), self.has(py)) {
            (Ok(Some(v)), _) => format!("BoundCapture({v:#x})"),
            (Ok(None), Ok(true)) => "BoundCapture(<node>)".to_string(),
            (Ok(None), Ok(false)) => "BoundCapture(unbound)".to_string(),
            _ => "BoundCapture(stale)".to_string(),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()?;
    m.add_class::<PyBoundCapture>()
}
