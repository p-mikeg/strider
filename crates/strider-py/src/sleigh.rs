use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_strider_err;
use crate::reader::{AnyMemReader, MemInput};

/// Register-table lookup for an arch, independent of any `Lifter`.
/// Construct via `strider.sleigh.Sleigh(arch, mem)`.
#[pyclass(name = "Sleigh", module = "strider.sleigh")]
pub struct PySleigh {
    pub(crate) arch_name: &'static str,
    /// Probed once at construction.
    pub(crate) regs: rsleigh::SleighRegs,
}

impl PySleigh {
    /// Probe the register table for `arch`, keeping only the table.
    pub(crate) fn new_internal(arch: PySleighArch, reader: AnyMemReader) -> PyResult<Self> {
        let sleigh = crate::strider_cls::build_orch_sleigh(&arch, reader)?;
        let regs = sleigh
            .regs()
            .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::regs failed: {e:?}")))?;
        Ok(Self {
            arch_name: arch.preset_name,
            regs,
        })
    }
}

#[pymethods]
impl PySleigh {
    /// Construct a Sleigh for `arch` reading from `mem` (a `BufferReader`
    /// or `MemReader` subclass).  Raises `StriderError` if Sleigh
    /// initialisation fails.
    #[new]
    fn new(arch: PySleighArch, mem: MemInput) -> PyResult<Self> {
        let reader = mem.into_any();
        Self::new_internal(arch, reader)
    }

    /// The arch preset name this Sleigh was built for (e.g. `"x86_64"`).
    fn arch_name(&self) -> &'static str {
        self.arch_name
    }

    /// Look up a register's varnode by Sleigh name, or `None` if this arch's
    /// table has no such name.
    fn reg(&self, name: &str) -> Option<PyVn> {
        self.regs.name_to_vn(name).map(PyVn::from_inner)
    }

    /// Reverse of `reg(...)`.  `None` for a non-REGISTER space, or a
    /// REGISTER offset/size pair absent from this arch's table; never raises.
    fn reg_name(&self, vn: &PyVn) -> Option<&str> {
        self.regs.vn_to_name(vn.inner)
    }

    /// `Sleigh(arch=<preset>)`.
    fn __repr__(&self) -> String {
        format!("Sleigh(arch={})", self.arch_name)
    }
}

/// A Sleigh address space, exposed as the `RAM` / `REGISTER` / `CONST` /
/// `UNIQUE` class constants (instances, not callables).
// `gen_stub_pyclass` derives `PyStubType` so macro-emitted
// `.space(s: PyVnSpace)` signatures compile under `gen_stub_pymethods`.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "VnSpace", module = "strider.sleigh", frozen)]
#[derive(Clone, Copy)]
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

    /// Equality on the underlying Sleigh space identity.
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

    /// Hash consistent with `__eq__`.
    fn __hash__(&self) -> u64 {
        // Hash the shortcut byte (rsleigh's own PartialEq/Hash key), NOT the
        // address of `self.inner`: two instances wrapping `VnSpace::RAM` sit
        // at different addresses, which broke `a == b => hash(a) == hash(b)`.
        u64::from(self.inner.shortcut_raw())
    }
}

/// A Sleigh varnode: `(space, offset, size_in_bytes)`.
///
/// Construct via:
/// * `Sleigh.reg("RAX")`, or `None` if the name isn't a register.
/// * `Vn(space, off, size)`, for stack varnodes and custom spaces.
// Every PyClass used as a method-argument type from a stub-gen-instrumented
// impl needs `gen_stub_pyclass`, even though `pattern.pyi` is hand-written.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Vn", module = "strider.sleigh", frozen)]
#[derive(Clone, Copy)]
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

    /// Equality on all three varnode fields (space, offset, size).
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

    /// Hash consistent with `__eq__`.
    fn __hash__(&self) -> u64 {
        // `addr_space` must be in the mix: without it, RAM[0x10]:8 and
        // REGISTER[0x10]:8 shared a bucket.
        let mut h = self.inner.addr_off;
        h ^= u64::from(self.inner.size).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= u64::from(self.inner.addr_space.shortcut_raw()).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleigh>()?;
    m.add_class::<PyVnSpace>()?;
    m.add_class::<PyVn>()?;
    Ok(())
}
