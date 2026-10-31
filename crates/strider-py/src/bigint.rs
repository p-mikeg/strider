//! Python `int` <-> `UBig` / `IBig`, in both directions without a carrier.
//!
//! A Python `int` is unbounded, so nothing here caps at 128 bits. The INPUT
//! side is bounded instead by [`MAX_CONST_BITS`]: a `UBig` of `2**10**9`
//! would allocate ~125 MB for a value no `ValueType` can hold, so the size is
//! read off `int.bit_length()` and refused before any conversion runs.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyInt};

use strider_ir::{IBig, UBig};

/// The widest declared integer width, `ValueType::int(512)`, and so the
/// widest value any constant holds.
pub(crate) const MAX_CONST_BITS: usize = 512;

/// A `UBig` as a Python `int`.
pub(crate) fn ubig_to_py_int(py: Python<'_>, v: &UBig) -> PyResult<Py<PyAny>> {
    if let Ok(narrow) = u128::try_from(v) {
        return Ok(narrow.into_py(py));
    }
    from_le_bytes(py, &v.to_le_bytes(), false)
}

/// An `IBig` as a Python `int`.
pub(crate) fn ibig_to_py_int(py: Python<'_>, v: &IBig) -> PyResult<Py<PyAny>> {
    if let Ok(narrow) = i128::try_from(v) {
        return Ok(narrow.into_py(py));
    }
    // `IBig::to_le_bytes` is two's complement, which is what `signed=True`
    // reads.
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

/// A Python `int` of any size, refused past [`MAX_CONST_BITS`].
///
/// Extraction never raises on an over-wide `int`: it records the size, so a
/// comparison can answer "not equal" while a constructor raises.
pub(crate) struct PyBigInt(Result<IBig, usize>);

impl PyBigInt {
    /// The value, or the refusal for one past the widest declared width.
    pub(crate) fn value(self) -> PyResult<IBig> {
        self.0.map_err(|bits| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "integer of {bits} bits: no strider value type is wider than \
                 {MAX_CONST_BITS} bits"
            ))
        })
    }

    /// The value, or `None` past the widest declared width, which no
    /// constant reaches.
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

/// `ob` as an integer, or `None` when it is not one at all.
pub(crate) fn py_big_int(ob: &Bound<'_, PyAny>) -> PyResult<Option<PyBigInt>> {
    let py = ob.py();
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
    // Past both carriers, so either a wide `int` or no `int` at all.
    if !ob.is_instance_of::<PyInt>() {
        return Ok(None);
    }
    let bits: usize = ob.call_method0("bit_length")?.extract()?;
    if bits > MAX_CONST_BITS {
        // Before any conversion: a `UBig` of this would allocate bits/8 bytes.
        return Ok(Some(PyBigInt(Err(bits))));
    }
    let kwargs = pyo3::types::PyDict::new_bound(py);
    kwargs.set_item("signed", true)?;
    let bytes = ob.call_method("to_bytes", (bits / 8 + 1, "little"), Some(&kwargs))?;
    let bytes = bytes.downcast::<PyBytes>()?;
    Ok(Some(PyBigInt(Ok(IBig::from_le_bytes(bytes.as_bytes())))))
}
