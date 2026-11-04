//! `strider.template`: the build-valid rewrite RHS shapes.

use pyo3::prelude::*;
use strider_pattern as sp;

use crate::pattern::{Kind, PyCapture, Repr, bin, ex, un};
use crate::value_ops::value_ops;

/// A type-checked rewrite-RHS expression, passed as `replace` to
/// `Function.rewrite` / `rewrite_all`.
#[pyclass(name = "Template", module = "strider.template", frozen)]
pub struct PyTemplate {
    pub(crate) repr: Repr,
}

impl From<Repr> for PyTemplate {
    fn from(repr: Repr) -> Self {
        Self { repr }
    }
}

impl Drop for PyTemplate {
    fn drop(&mut self) {
        sp::node_builders::defer_drop(std::mem::take(&mut self.repr.args));
    }
}

#[pymethods]
impl PyTemplate {
    /// Exposes the operand sub-templates to the collector.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        self.repr.args.iter().try_for_each(|a| visit.call(a))
    }

    fn __repr__(&self) -> String {
        "Template(...)".to_string()
    }
}

/// Substituted at rewrite time by the node bound to `c` on the matched LHS.
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyTemplate {
    PyTemplate::from(crate::pattern::var_repr(c.inner))
}

/// Build an `IntConst` whose stored value, masked to the output width, is
/// `value`. Where nothing gives it a width, type it with `capture_typed`.
#[pyfunction]
pub fn int_const(value: crate::bigint::PyBigInt) -> PyResult<PyTemplate> {
    Ok(PyTemplate::from(crate::pattern::int_const_repr(
        value.value()?,
    )))
}

/// Type `inner`'s output at the width of the value `c` bound on the matched
/// LHS (for a `Popcount` operand, say). A capture or `var()` as `inner` is
/// refused.
#[pyfunction]
pub fn capture_typed(
    py: Python<'_>,
    c: PyRef<'_, PyCapture>,
    inner: Py<PyAny>,
) -> PyResult<PyTemplate> {
    let bound = inner.bind(py);
    let is_capture = bound.downcast::<PyCapture>().is_ok()
        || bound
            .downcast::<PyTemplate>()
            .is_ok_and(|t| matches!(t.get().repr.op, crate::pattern::Op::Var(_)));
    if is_capture {
        return Err(crate::errors::into_strider_err(anyhow::anyhow!(
            "capture_typed() over a capture: var(c) keeps the type of the value \
             it bound, so there is nothing to type"
        )));
    }
    let c = c.inner;
    let make = move |x: sp::Expr| x.capture_typed(c);
    Ok(PyTemplate::from(un(
        "capture_typed()",
        Kind::Build,
        Box::new(make),
        inner,
    )))
}

/// Build an `I1` boolean constant equal to `value`.
#[pyfunction]
pub fn bool_const(value: bool) -> PyTemplate {
    PyTemplate::from(crate::pattern::bool_const_repr(value))
}

/// Build a float constant with the IEEE 754 bit pattern `bits`, at the width
/// it is evaluated at; a set bit above that width refuses the rewrite.
#[pyfunction]
pub fn float_const(bits: u64) -> PyTemplate {
    PyTemplate::from(crate::pattern::float_const_repr(bits))
}

value_ops!(PyTemplate, "Build", template);

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    bind!(m;
        classes [PyTemplate];
        fns [var, int_const, bool_const, float_const, capture_typed];
    );
    register_value_ops(m)
}
