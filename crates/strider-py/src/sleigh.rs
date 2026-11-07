use pyo3::prelude::*;

/// A Sleigh address space: `VnSpace.RAM`, `REGISTER`, `CONST` or `UNIQUE`.
#[pyclass(name = "VnSpace", module = "strider.sleigh", frozen, eq, hash)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyVnSpace {
    pub(crate) inner: rsleigh::VnSpace,
}

#[pymethods]
impl PyVnSpace {
    /// The RAM (main memory) address space.
    #[classattr]
    #[allow(non_snake_case)]
    fn RAM() -> Self {
        Self {
            inner: rsleigh::VnSpace::RAM,
        }
    }
    /// The register address space.
    #[classattr]
    #[allow(non_snake_case)]
    fn REGISTER() -> Self {
        Self {
            inner: rsleigh::VnSpace::REGISTER,
        }
    }
    /// The constant ("const") address space.
    #[classattr]
    #[allow(non_snake_case)]
    fn CONST() -> Self {
        Self {
            inner: rsleigh::VnSpace::CONST,
        }
    }
    /// The unique (temporary) address space.
    #[classattr]
    #[allow(non_snake_case)]
    fn UNIQUE() -> Self {
        Self {
            inner: rsleigh::VnSpace::UNIQUE,
        }
    }

    /// `"RAM"`, `"REGISTER"`, `"CONST"`, `"UNIQUE"` or `"OTHER"`.
    pub(crate) fn name(&self) -> &'static str {
        use rsleigh::VnSpace as S;
        [
            (S::RAM, "RAM"),
            (S::REGISTER, "REGISTER"),
            (S::CONST, "CONST"),
            (S::UNIQUE, "UNIQUE"),
        ]
        .into_iter()
        .find_map(|(space, name)| (space == self.inner).then_some(name))
        .unwrap_or("OTHER")
    }

    fn __repr__(&self) -> String {
        match self.name() {
            "OTHER" => format!("<VnSpace OTHER {:?}>", self.inner.shortcut()),
            name => format!("VnSpace.{name}"),
        }
    }
}

/// A Sleigh varnode `(space, offset, size_in_bytes)`, from
/// `SleighArch.reg("RAX")` or `Vn(space, off, size)`.
#[pyclass(name = "Vn", module = "strider.sleigh", frozen, eq, hash)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyVn {
    pub(crate) inner: rsleigh::Vn,
}

impl PyVn {
    pub(crate) fn from_inner(inner: rsleigh::Vn) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PyVn {
    /// A varnode from `(space, offset, size_in_bytes)`.
    #[new]
    fn new(space: PyVnSpace, off: crate::bigint::Addr, size: crate::bigint::Unsigned<u32>) -> Self {
        Self {
            inner: rsleigh::Vn {
                size: size.0,
                addr_off: off.0,
                addr_space: space.inner,
            },
        }
    }

    /// The varnode's address space.
    #[getter]
    fn space(&self) -> PyVnSpace {
        PyVnSpace {
            inner: self.inner.addr_space,
        }
    }
    /// The varnode's offset within its space.
    #[getter]
    fn off(&self) -> u64 {
        self.inner.addr_off
    }
    /// The varnode's size in bytes.
    #[getter]
    fn size(&self) -> u32 {
        self.inner.size
    }

    fn __repr__(&self) -> String {
        format!("{}", self.inner)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVnSpace>()?;
    m.add_class::<PyVn>()?;
    Ok(())
}
