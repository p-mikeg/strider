//! The free functions here construct the build-valid subset of pattern
//! variants; `.when()`, the commutativity toggle and `.ordered()` are
//! match-only.

use pyo3::prelude::*;

use crate::pattern::{CastKind, FloatUnaryKind, IntUnaryKind, PatRepr, PyCapture};
use crate::value_ops::value_ops;

/// A type-checked rewrite-RHS expression. Construct via the free functions in
/// `strider.template` (`var(c)`, `int_add(...)`, `int_const`, ...); pass as
/// `replace` to `Function.rewrite` / `rewrite_all`.
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
    PyTemplate::from_repr(PatRepr::Var(c.inner))
}

/// Build an `IntConst` whose stored value, masked to the output width, is
/// `value`. Bit-pattern equality; negatives use the sign-extended form.
#[pyfunction]
pub fn int_const(value: i128) -> PyTemplate {
    PyTemplate::from_repr(PatRepr::IntConst(value as u128))
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
    register_value_ops(m)?;
    Ok(())
}
