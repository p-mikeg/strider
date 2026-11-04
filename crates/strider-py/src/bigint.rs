//! Input past [`MAX_CONST_BITS`] is refused via `int.bit_length()` before
//! converting, so a huge `int` never allocates.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyInt};

use strider_ir::{IBig, UBig};

/// The widest integer `ValueType`.
pub(crate) const MAX_CONST_BITS: usize = 512;

pub(crate) fn ubig_to_py_int(py: Python<'_>, v: &UBig) -> PyResult<Py<PyAny>> {
    if let Ok(narrow) = u128::try_from(v) {
        return Ok(narrow.into_py(py));
    }
    from_le_bytes(py, &v.to_le_bytes(), false)
}

pub(crate) fn ibig_to_py_int(py: Python<'_>, v: &IBig) -> PyResult<Py<PyAny>> {
    if let Ok(narrow) = i128::try_from(v) {
        return Ok(narrow.into_py(py));
    }
    // `IBig::to_le_bytes` is two's complement.
    from_le_bytes(py, &v.to_le_bytes(), true)
}

fn from_le_bytes(py: Python<'_>, bytes: &[u8], signed: bool) -> PyResult<Py<PyAny>> {
    let kwargs = pyo3::types::PyDict::new_bound(py);
    kwargs.set_item("signed", signed)?;
    Ok(py
        .get_type_bound::<PyInt>()
        .call_method(
            "from_bytes",
            (PyBytes::new_bound(py, bytes), "little"),
            Some(&kwargs),
        )?
        .unbind())
}

/// A Python `int`. An over-wide one records its bit count instead of
/// raising, so a comparison can answer "not equal" while a constructor raises.
pub(crate) struct PyBigInt(Result<IBig, usize>);

impl PyBigInt {
    /// The value, or `StriderError` past [`MAX_CONST_BITS`].
    pub(crate) fn value(self) -> PyResult<IBig> {
        self.0.map_err(|bits| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "integer of {bits} bits: no strider value type is wider than \
                 {MAX_CONST_BITS} bits"
            ))
        })
    }

    /// The value, or `None` past [`MAX_CONST_BITS`].
    pub(crate) fn in_range(self) -> Option<IBig> {
        self.0.ok()
    }
}

impl<'py> FromPyObject<'py> for PyBigInt {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        py_big_int(ob)?.ok_or_else(|| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "expected an int, got {}",
                crate::pattern::operand_kind_name(ob)
            ))
        })
    }
}

/// `ob` as an integer, or `None` when it is not one.
pub(crate) fn py_big_int(ob: &Bound<'_, PyAny>) -> PyResult<Option<PyBigInt>> {
    let py = ob.py();
    // Only `TypeError` / `OverflowError` mean "not an integer"; any other
    // (a raising `__index__`) propagates, or a re-entrant one retries
    // exponentially.
    let wrong_kind = |e: &PyErr| {
        e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
            || e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py)
    };
    match ob.extract::<i128>() {
        Ok(v) => return Ok(Some(PyBigInt(Ok(IBig::from(v))))),
        Err(e) if !wrong_kind(&e) => return Err(e),
        Err(_) => {}
    }
    match ob.extract::<u128>() {
        Ok(v) => return Ok(Some(PyBigInt(Ok(IBig::from(v))))),
        Err(e) if !wrong_kind(&e) => return Err(e),
        Err(_) => {}
    }
    if !ob.is_instance_of::<PyInt>() {
        return Ok(None);
    }
    let bits: usize = ob.call_method0("bit_length")?.extract()?;
    if bits > MAX_CONST_BITS {
        return Ok(Some(PyBigInt(Err(bits))));
    }
    let kwargs = pyo3::types::PyDict::new_bound(py);
    kwargs.set_item("signed", true)?;
    let bytes = ob.call_method("to_bytes", (bits / 8 + 1, "little"), Some(&kwargs))?;
    let bytes = bytes.downcast::<PyBytes>()?;
    Ok(Some(PyBigInt(Ok(IBig::from_le_bytes(bytes.as_bytes())))))
}

/// An address, node id or operand index. A `bool` raises `TypeError`, an int outside `T`
/// `StriderError`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Unsigned<T>(pub T);

pub(crate) type Addr = Unsigned<u64>;

impl<'py, T: FromPyObject<'py>> FromPyObject<'py> for Unsigned<T> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if ob.is_instance_of::<pyo3::types::PyBool>() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "expected an int, got bool",
            ));
        }
        ob.extract().map(Self).map_err(|e| {
            if ob.is_instance_of::<PyInt>() {
                crate::errors::into_strider_err(anyhow::anyhow!(
                    "{ob} is out of range: it must fit an unsigned {}-bit integer",
                    8 * std::mem::size_of::<T>()
                ))
            } else {
                e
            }
        })
    }
}

impl<T: IntoPy<PyObject>> IntoPy<PyObject> for Unsigned<T> {
    fn into_py(self, py: Python<'_>) -> PyObject {
        self.0.into_py(py)
    }
}
