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

    /// The value as `T`, or `OverflowError` outside it.
    pub(crate) fn to<T: TryFrom<IBig>>(self) -> PyResult<T> {
        let ty = std::any::type_name::<T>();
        let out_of_range = |v: String| {
            pyo3::exceptions::PyOverflowError::new_err(format!("{v} is out of range for {ty}"))
        };
        match self.0 {
            Ok(v) => T::try_from(v.clone()).map_err(|_| out_of_range(v.to_string())),
            Err(bits) => Err(out_of_range(format!("an integer of {bits} bits"))),
        }
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

/// `ob`, or the `int` its `__index__` gives, as an integer; `None` when it is
/// not one.
pub(crate) fn py_big_int(ob: &Bound<'_, PyAny>) -> PyResult<Option<PyBigInt>> {
    let py = ob.py();
    // Only `TypeError` / `OverflowError` mean "not an integer"; any other
    // (a raising `__index__`) propagates, or a re-entrant one retries
    // exponentially.
    let wrong_kind = |e: &PyErr| {
        e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
            || e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py)
    };
    if !ob.is_instance_of::<PyInt>() {
        // Under abi3 a 128-bit extract shifts its operand, so an `__index__`
        // object is read through the `int` it gives.
        if !ob.hasattr(pyo3::intern!(py, "__index__"))? {
            return Ok(None);
        }
        return match py.import_bound("operator")?.getattr("index")?.call1((ob,)) {
            Ok(int) => py_big_int(&int),
            Err(e) if wrong_kind(&e) => Ok(None),
            Err(e) => Err(e),
        };
    }
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

/// An `int`, or an `__index__` object, in `T`'s range: another type raises
/// `TypeError`, an int outside `T` `OverflowError`.
pub(crate) struct Ranged<T>(pub T);

impl<'py, T: TryFrom<IBig>> FromPyObject<'py> for Ranged<T> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        PyBigInt::extract_bound(ob)?.to().map(Self)
    }
}

/// An address, node id or operand index. A `bool` raises `TypeError`, an int outside `T`
/// (or an `__index__` giving one) `StriderError`.
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
            let py = ob.py();
            let value = if ob.is_instance_of::<PyInt>() {
                Some(ob.clone())
            } else if e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py) {
                py.import_bound("operator")
                    .and_then(|m| m.getattr("index")?.call1((ob,)))
                    .ok()
            } else {
                None
            };
            match value {
                Some(v) => crate::errors::into_strider_err(anyhow::anyhow!(
                    "{v} is out of range: it must fit an unsigned {}-bit integer",
                    8 * std::mem::size_of::<T>()
                )),
                None => e,
            }
        })
    }
}

impl<T: IntoPy<PyObject>> IntoPy<PyObject> for Unsigned<T> {
    fn into_py(self, py: Python<'_>) -> PyObject {
        self.0.into_py(py)
    }
}

/// A sequence of ints, refusing the `bytes` / `bytearray` PyO3 would read
/// as the list of its byte values.
pub struct IntSeq<T>(pub Vec<T>);

/// Refuses the `bytes` / `bytearray` PyO3 would read as a list of ints.
pub fn refuse_bytes(ob: &Bound<'_, PyAny>) -> PyResult<()> {
    if ob.is_instance_of::<PyBytes>() || ob.is_instance_of::<pyo3::types::PyByteArray>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(format!(
            "expected a list of ints, not {}",
            ob.get_type().name()?
        )));
    }
    Ok(())
}

impl<'py, T: FromPyObject<'py>> FromPyObject<'py> for IntSeq<T> {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        refuse_bytes(ob)?;
        ob.extract().map(Self)
    }
}

impl<T: IntoPy<PyObject>> IntoPy<PyObject> for IntSeq<T> {
    fn into_py(self, py: Python<'_>) -> PyObject {
        self.0.into_py(py)
    }
}
