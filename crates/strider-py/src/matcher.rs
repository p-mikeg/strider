use pyo3::basic::CompareOp;
use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::pattern::{PyCapture, intern_str};

/// Result of a successful pattern match.
///
/// Every capture accessor raises `StriderError` once the function has been
/// compacted or otherwise reshuffled, rather than dereferencing the stored
/// `ValueId`s against the new arena.
#[pyclass(name = "Match", module = "strider.pattern", unsendable)]
pub struct PyMatch {
    /// Per-input-pattern sub-matches, non-empty. Shared captures are already
    /// unified, so the `Match` presents the union of every pattern's captures.
    pub(crate) inner: Vec<strider_pattern::Match>,
    pub(crate) function: Py<PyFunction>,
    /// Sampled at construction, compared on every accessor.
    pub(crate) generation: u64,
}

/// Per-pattern roots (empty under `ignore_root`) paired with each sub-match's
/// `(capture-id, node-id)` signature.
type DedupKey = (Vec<u32>, Vec<Vec<(u32, u32)>>);

/// A `Capture` instance or a string name, looked up in the global intern
/// table.
#[derive(FromPyObject)]
pub enum CaptureKey<'py> {
    Capture(Bound<'py, PyCapture>),
    Str(String),
}

// Hand-written so pyo3-stub-gen emits the union rather than one arm.
impl pyo3_stub_gen::PyStubType for CaptureKey<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        pyo3_stub_gen::TypeInfo::with_module("strider.pattern.Capture", "strider.pattern".into())
            | <String as pyo3_stub_gen::PyStubType>::type_output()
    }
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
    /// Error if the arena was reshuffled since this match was built.
    fn assert_generation(&self, function: &strider_ir::Function) -> PyResult<()> {
        if function.graph().generation() != self.generation {
            return Err(into_strider_err(anyhow::anyhow!(
                "Match is stale: function was compacted / reshuffled after this Match was \
                 created (match generation = {}, function generation = {}).  Re-run the \
                 pattern against the post-compaction function.",
                self.generation,
                function.graph().generation(),
            )));
        }
        Ok(())
    }

    /// Run `f` only if the arena still has the generation this match was
    /// built against, so a raw node id never escapes a compaction.
    fn checked<R>(&self, py: Python<'_>, f: impl FnOnce(&Self) -> R) -> PyResult<R> {
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        Ok(f(self))
    }

    /// The first sub-match binding `cap`; the join already unified shared
    /// captures, so every such sub-match agrees.
    fn binding_for(&self, cap: strider_pattern::Capture) -> Option<&strider_pattern::Match> {
        self.inner.iter().find(|m| m.is_bound(cap))
    }

    fn is_bound(&self, cap: strider_pattern::Capture) -> bool {
        self.inner.iter().any(|m| m.is_bound(cap))
    }

    /// Resolve `cap` to a node id and read it under a single borrow, skipping
    /// the `PyNode` round trip's three resolutions and discarded incref.
    fn read_bound<R>(
        &self,
        py: Python<'_>,
        cap: strider_pattern::Capture,
        read: impl FnOnce(&strider_ir::Function, u32) -> Option<R>,
    ) -> PyResult<Option<R>> {
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        Ok(self
            .binding_for(cap)
            .and_then(|m| m.node(cap, function.graph()))
            .and_then(|nid| read(&function, nid.as_u32())))
    }

    /// The unsigned constant bound to `cap`, or `None` when `cap` is unbound or
    /// its node is not an integer constant. For `find_unique_value`.
    pub(crate) fn uint_for(
        &self,
        py: Python<'_>,
        cap: strider_pattern::Capture,
    ) -> PyResult<Option<u128>> {
        self.read_bound(py, cap, crate::node::uint_of)
    }

    /// The signed (sign-extended from its width) constant bound to `cap`. Like
    /// [`Self::uint_for`] but reads the value as two's-complement.
    pub(crate) fn sint_for(
        &self,
        py: Python<'_>,
        cap: strider_pattern::Capture,
    ) -> PyResult<Option<i128>> {
        self.read_bound(py, cap, crate::node::sint_of)
    }

    /// `Capture('x'): BoundCapture(...)` entries for every bound capture, in
    /// ascending capture-id order. The value mirrors `BoundCapture.__repr__`: a
    /// hex constant when the node is an integer const, else `<node>`.
    fn bindings_repr(&self, py: Python<'_>) -> PyResult<String> {
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        // Distinct captures across sub-matches; `capture_signature` is sorted by
        // id, so the first node seen per id wins and order is deterministic.
        let mut seen: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        for m in &self.inner {
            for (cap_id, node_id) in m.capture_signature(function.graph()) {
                seen.entry(cap_id).or_insert(node_id);
            }
        }
        let entries: Vec<String> = seen
            .into_iter()
            .map(|(cap_id, node_id)| {
                let key = crate::pattern::capture_display(cap_id);
                let value = crate::node::uint_of(&function, node_id)
                    .map_or_else(|| "<node>".to_string(), |v| format!("{v:#x}"));
                format!("{key}: BoundCapture({value})")
            })
            .collect();
        Ok(entries.join(", "))
    }

    /// Without `ignore_root` the per-pattern roots join the key; with it only
    /// bindings matter.
    pub(crate) fn dedup_key(&self, py: Python<'_>, ignore_root: bool) -> PyResult<DedupKey> {
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        let roots = if ignore_root {
            Vec::new()
        } else {
            self.inner.iter().map(|m| m.root().as_u32()).collect()
        };
        let sigs = self
            .inner
            .iter()
            .map(|m| m.capture_signature(function.graph()))
            .collect();
        Ok((roots, sigs))
    }
}

/// Unwrap a capture read for the raising getters, raising a uniform error when
/// the `_opt` counterpart would return `None` (unbound, or a node without the
/// requested aspect).
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
        visit.call(&self.function)
    }

    /// Node id where the top-level pattern matched. The root carries no
    /// user-visible capture binding.
    #[getter]
    fn root(&self, py: Python<'_>) -> PyResult<u32> {
        self.checked(py, |m| m.inner[0].root().as_u32())
    }

    /// One root node id per pattern passed to the query.
    #[getter]
    fn roots(&self, py: Python<'_>) -> PyResult<Vec<u32>> {
        self.checked(py, |m| m.inner.iter().map(|m| m.root().as_u32()).collect())
    }

    /// `m[c]` / `m["name"]`: capture `c` bound to THIS match, a `BoundCapture`
    /// carrying every reader (`.uint`, `.node`, `.op`, ... and their
    /// `_opt` forms) without repeating the capture. A numeric capture also
    /// converts and compares directly (`int(m[c])`, `m[c] == 0x10`).
    fn __getitem__(slf: Bound<'_, Self>, key: CaptureKey<'_>) -> PyResult<PyBoundCapture> {
        let py = slf.py();
        let cap = key.resolve()?;
        slf.borrow().checked(py, |_| ())?;
        Ok(PyBoundCapture {
            match_: slf.unbind(),
            cap: Py::new(py, PyCapture { inner: cap })?,
        })
    }

    /// True when `key` (a `Capture` or string name) is bound in this match.
    fn __contains__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        self.checked(py, |m| m.is_bound(cap))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let roots: Vec<u32> = self.inner.iter().map(|m| m.root().as_u32()).collect();
        // A repr must not raise; a stale / unreadable function drops the dict.
        let bindings = self.bindings_repr(py).unwrap_or_default();
        format!("Match(roots={roots:?}, {{{bindings}}})")
    }

    /// True when `key` is bound in this match. Raises once the function has
    /// been compacted, like every other capture accessor, so `if c in m` and
    /// `m.node(c)` agree.
    fn has(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        self.checked(py, |m| m.is_bound(cap))
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
        // Re-borrow to check the generation before handing out a node id that
        // could point into a stale arena, and to resolve an `Output` binding
        // back to its owning node.
        let nid = {
            let function = self.function.borrow(py);
            let function = function.read_inner().map_err(into_strider_err)?;
            self.assert_generation(&function)?;
            self.binding_for(cap)
                .and_then(|m| m.node(cap, function.graph()))
        };
        // Resolved under the borrow just dropped, so `PyNode::new`'s own
        // re-validation would repeat it.
        Ok(nid.map(|nid| {
            crate::node::PyNode::validated(
                self.function.clone_ref(py),
                nid.as_u32(),
                self.generation,
            )
        }))
    }
}

/// Each row generates the raising getter (`or_missing` over its `_opt` twin,
/// the error label is the Python name) and the `_opt` getter (delegates to the
/// `PyNode` aspect accessor, `None` when the capture binds no node carrying it).
/// `node_opt`, its raising `node`, and `asm_fingerprint` (empty vec, not `None`)
/// stay hand-written above. Own `#[pymethods]` block (multiple-pymethods).
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
    "uint_opt" uint_opt : u128 = uint;

    #[doc = "The signed value, sign-interpreted at the node's width; raises if unbound or not an integer node."]
    "sint" int_,
    #[doc = "The signed value, or `None`."]
    "sint_opt" int_opt : i128 = sint;

    #[doc = "The capture's value as a `bool`; raises if unbound or not a boolean node."]
    "boolean" bool_,
    #[doc = "The boolean value, or `None`."]
    "boolean_opt" bool_opt : bool = boolean;

    #[doc = "The capture's value as raw float bits; raises if unbound or not a float node."]
    "float_bits" float_bits,
    #[doc = "The raw float bits, or `None`."]
    "float_bits_opt" float_bits_opt : u64 = float_bits;

    #[doc = "The varnode a captured node names: an `InitialVar`'s entry-read varnode, the register a `Call` returns in, or whatever varnode the sla gives a `CallOther`'s result (a `unique` temporary, a tracked register, or none). Raises for any other kind. A capture binds a NODE, so this answers for a multi-output call's FIRST value output, never for one clobber in particular."]
    "vn" vn,
    #[doc = "The varnode, or `None`."]
    "vn_opt" vn_opt : crate::sleigh::PyVn = vn;
}

/// Capture `cap` bound to a specific `PyMatch`: `m[c]`. Every reader delegates
/// to the owning match, so the capture is named once.
#[pyclass(name = "BoundCapture", module = "strider.pattern", unsendable)]
pub struct PyBoundCapture {
    match_: Py<PyMatch>,
    cap: Py<PyCapture>,
}

/// Each `BoundCapture` reader forwards to the same-named (or `$delegate`)
/// `PyMatch` method with this capture's key: `self.match_.borrow(py).$delegate(
/// py, self.key(py))`.  Stamped as its own `#[pymethods]` block
/// (multiple-pymethods), so `has` and the dunders stay hand-written below.
macro_rules! bound_getters {
    ($( #[doc = $doc:literal] $name:ident -> $ret:ty = $delegate:ident );+ $(;)?) => {
        #[pymethods]
        impl PyBoundCapture {
            $(
                #[doc = $doc]
                #[getter]
                fn $name(&self, py: Python<'_>) -> PyResult<$ret> {
                    self.match_.borrow(py).$delegate(py, self.key(py))
                }
            )+
        }
    };
}

bound_getters! {
    #[doc = "The unsigned integer constant (raises if unbound or not one)."]
    uint -> u128 = uint;
    #[doc = "The unsigned integer constant, or `None` instead of raising."]
    uint_opt -> Option<u128> = uint_opt;
    #[doc = "The signed integer constant (raises if unbound or not one)."]
    sint -> i128 = int_;
    #[doc = "The signed integer constant, or `None` instead of raising."]
    sint_opt -> Option<i128> = int_opt;
    #[doc = "The boolean constant (raises if unbound or not one)."]
    boolean -> bool = bool_;
    #[doc = "The boolean constant, or `None` instead of raising."]
    boolean_opt -> Option<bool> = bool_opt;
    #[doc = "The raw float bits (raises if unbound or not a float node)."]
    float_bits -> u64 = float_bits;
    #[doc = "The raw float bits, or `None` instead of raising."]
    float_bits_opt -> Option<u64> = float_bits_opt;
    #[doc = "The operation variant (raises if unbound or the node carries none)."]
    op -> String = op;
    #[doc = "The operation variant, or `None` instead of raising."]
    op_opt -> Option<String> = op_opt;
    #[doc = "The value-output type (raises if unbound or the node has none)."]
    value_type -> String = value_type;
    #[doc = "The value-output type, or `None` instead of raising."]
    value_type_opt -> Option<String> = value_type_opt;
    #[doc = "The varnode (raises if the capture binds none)."]
    vn -> crate::sleigh::PyVn = vn;
    #[doc = "The varnode, or `None` instead of raising."]
    vn_opt -> Option<crate::sleigh::PyVn> = vn_opt;
    #[doc = "The matched `Node` (raises if the capture is unbound)."]
    node -> crate::node::PyNode = node;
    #[doc = "The matched `Node`, or `None` instead of raising."]
    node_opt -> Option<crate::node::PyNode> = node_opt;
    #[doc = "The machine addresses that produced the bound node (`[]` if unbound)."]
    asm_fingerprint -> Vec<u64> = asm_fingerprint;
}

impl PyBoundCapture {
    fn key<'py>(&self, py: Python<'py>) -> CaptureKey<'py> {
        CaptureKey::Capture(self.cap.bind(py).clone())
    }
}

#[pymethods]
impl PyBoundCapture {
    /// Both handles are `Py<...>`, invisible to the GC without this, so a cycle
    /// through a `BoundCapture` would leak the whole match graph.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.match_)?;
        visit.call(&self.cap)
    }

    /// Whether the capture bound in this match.
    #[getter]
    fn has(&self, py: Python<'_>) -> PyResult<bool> {
        self.match_.borrow(py).has(py, self.key(py))
    }

    fn __int__(&self, py: Python<'_>) -> PyResult<u128> {
        self.uint(py)
    }
    fn __index__(&self, py: Python<'_>) -> PyResult<u128> {
        self.uint(py)
    }
    /// Equal to an int matching the captured constant at its width, in either
    /// signed or unsigned spelling (so `-8` and `0xFFFFFFF8` both match a
    /// 32-bit -8). Value-comparing, hence `BoundCapture` is unhashable.
    fn __richcmp__(
        &self,
        py: Python<'_>,
        other: Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<PyObject> {
        let equal = self
            .uint_opt(py)?
            .is_some_and(|u| other.extract::<u128>().is_ok_and(|o| o == u))
            || self
                .sint_opt(py)?
                .is_some_and(|s| other.extract::<i128>().is_ok_and(|o| o == s));
        Ok(match op {
            CompareOp::Eq => equal.into_py(py),
            CompareOp::Ne => (!equal).into_py(py),
            _ => py.NotImplemented(),
        })
    }
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(match self.uint_opt(py)? {
            Some(v) => format!("BoundCapture({v:#x})"),
            None if self.has(py)? => "BoundCapture(<node>)".to_string(),
            None => "BoundCapture(unbound)".to_string(),
        })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()?;
    m.add_class::<PyBoundCapture>()
}
