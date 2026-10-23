//! The free functions here construct the build-valid shapes; `.when()`, the
//! commutativity toggle and `.ordered()` are match-only.

use pyo3::prelude::*;
use strider_pattern::TemplatePat as _;

use crate::pattern::{CastKind, IntUnaryKind, PyCapture, TmplRepr};
use crate::value_ops::value_ops;

/// A type-checked rewrite-RHS expression. Construct via the free functions in
/// `strider.template` (`var(c)`, `int_add(...)`, `int_const`, ...); pass as
/// `replace` to `Function.rewrite` / `rewrite_all`.
#[pyclass(name = "Template", module = "strider.template")]
pub struct PyTemplate {
    pub(crate) repr: std::sync::Arc<TmplRepr>,
}

impl PyTemplate {
    pub(crate) fn from_tmpl(repr: TmplRepr) -> Self {
        Self {
            repr: std::sync::Arc::new(repr),
        }
    }

    pub(crate) fn to_template(&self, py: Python<'_>) -> PyResult<strider_pattern::Template> {
        Ok(self.repr.compile_template(py)?.into_template())
    }
}

impl Drop for PyTemplate {
    fn drop(&mut self) {
        strider_pattern::node_builders::defer_drop(std::mem::replace(
            &mut self.repr,
            std::sync::Arc::new(TmplRepr::BoolConst(false)),
        ));
    }
}

#[pymethods]
impl PyTemplate {
    /// Exposes the operand sub-templates, `Py` handles the collector cannot
    /// otherwise see.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        self.repr.traverse(&visit)
    }

    fn __repr__(&self) -> String {
        "Template(...)".to_string()
    }
}

/// Substituted at rewrite time by the node bound to `c` on the matched LHS.
/// The one build-valid wildcard.
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyTemplate {
    PyTemplate::from_tmpl(TmplRepr::Var(c.inner))
}

/// Build an `IntConst` whose stored value, masked to the output width, is
/// `value`. Bit-pattern equality; negatives use the sign-extended form. Where
/// nothing gives it a width, type it with `capture_typed`.
#[pyfunction]
pub fn int_const(value: WideInt) -> PyTemplate {
    PyTemplate::from_tmpl(TmplRepr::IntConst(value.bits()))
}

/// A 128-bit constant from either side of the signed/unsigned split, so
/// `[2^127, 2^128)` is spellable alongside a negative.
#[derive(FromPyObject)]
pub enum WideInt {
    #[pyo3(annotation = "int")]
    Signed(i128),
    #[pyo3(annotation = "int")]
    Unsigned(u128),
}

impl WideInt {
    fn bits(self) -> u128 {
        match self {
            Self::Signed(v) => v as u128,
            Self::Unsigned(v) => v,
        }
    }
}

/// Type `inner`'s output at the width of the value `c` bound on the matched
/// LHS, for an operand nothing else gives a width (a `Popcount` operand). A
/// capture or `var()` as `inner` is refused: it keeps the type of the value it
/// bound.
#[pyfunction]
pub fn capture_typed(
    py: Python<'_>,
    c: PyRef<'_, PyCapture>,
    inner: Py<PyAny>,
) -> PyResult<PyTemplate> {
    // A nested `capture_typed` was checked when it was built.
    let bound = inner.bind(py);
    let is_capture = bound.downcast::<PyCapture>().is_ok()
        || bound
            .downcast::<PyTemplate>()
            .is_ok_and(|t| matches!(*t.borrow().repr, TmplRepr::Var(_)));
    if is_capture {
        return Err(crate::errors::into_strider_err(anyhow::anyhow!(
            "capture_typed() over a capture: var(c) keeps the type of the value \
             it bound, so there is nothing to type"
        )));
    }
    Ok(PyTemplate::from_tmpl(TmplRepr::CaptureTyped(
        c.inner, inner,
    )))
}

/// Build an `I1` boolean constant equal to `value`.
#[pyfunction]
pub fn bool_const(value: bool) -> PyTemplate {
    PyTemplate::from_tmpl(TmplRepr::BoolConst(value))
}

/// Build a float constant with the IEEE 754 bit pattern `bits`, at the width
/// it is evaluated at; a set bit above that width refuses the rewrite.
#[pyfunction]
pub fn float_const(bits: u64) -> PyTemplate {
    PyTemplate::from_tmpl(TmplRepr::FloatConst(bits))
}

value_ops!(PyTemplate, "Build", "");

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTemplate>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, m)?)?;
        };
    }
    add_fn!(var);
    add_fn!(int_const);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(capture_typed);
    register_value_ops(m)?;
    Ok(())
}
