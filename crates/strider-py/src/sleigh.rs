use pyo3::prelude::*;

/// A Sleigh address space, exposed as the `RAM` / `REGISTER` / `CONST` /
/// `UNIQUE` class constants (instances, not callables).
#[pyclass(name = "VnSpace", module = "strider.sleigh", frozen, eq, hash)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyVnSpace {
    pub(crate) inner: rsleigh::VnSpace,
}

#[pymethods]
impl PyVnSpace {
    // Python class constants, hence the four `non_snake_case` allows.
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

    /// The space's name (`"RAM"`, `"REGISTER"`, `"CONST"`, `"UNIQUE"`,
    /// or `"OTHER"`).
    pub(crate) fn name(&self) -> &'static str {
        if self.inner == rsleigh::VnSpace::RAM {
            "RAM"
        } else if self.inner == rsleigh::VnSpace::REGISTER {
            "REGISTER"
        } else if self.inner == rsleigh::VnSpace::CONST {
            "CONST"
        } else if self.inner == rsleigh::VnSpace::UNIQUE {
            "UNIQUE"
        } else {
            "OTHER"
        }
    }

    /// `VnSpace.<name>` (lowercased).
    fn __repr__(&self) -> String {
        format!("VnSpace.{}", self.name().to_lowercase())
    }
}

/// A Sleigh varnode: `(space, offset, size_in_bytes)`.
///
/// Construct via:
/// * `SleighArch.reg("RAX")`, or `None` if the name isn't a register.
/// * `Vn(space, off, size)`, for stack varnodes and custom spaces.
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
    /// Construct a varnode from `(space, offset, size_in_bytes)`.
    #[new]
    fn new(space: PyVnSpace, off: u64, size: u32) -> Self {
        Self {
            inner: rsleigh::Vn {
                size,
                addr_off: off,
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

    /// rsleigh's `Display` form, e.g. `%[0x20]:8` for a register varnode.
    fn __repr__(&self) -> String {
        format!("{}", self.inner)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVnSpace>()?;
    m.add_class::<PyVn>()?;
    Ok(())
}
