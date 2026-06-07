//! `PySleigh` — a lightweight Sleigh handle keyed off a `PySleighArch`
//! plus a memory reader (`PyMemoryMap` or any `MemReader` subclass).
//! It no longer owns a constructed `rsleigh::Sleigh` (the owning lift
//! engine, `strider_lift::lift::Lifter`, does); it retains the arch name
//! and the cached `SleighRegs` table, building a transient `Sleigh` only
//! where one is needed (e.g. p-code dumping).

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_strider_err;
use crate::reader::{AnyMemReader, MemInput};

/// A `Sleigh` register-table handle keyed off a (SleighArch, reader)
/// pair.
///
/// The `Lifter` now OWNS the `rsleigh::Sleigh` it builds CFGs with, so
/// this standalone wrapper no longer needs to retain the Sleigh itself:
/// it builds one transiently at construction to probe the register table
/// and keeps only the cached `SleighRegs` (for `reg(...)` lookups) plus
/// the arch preset name (for `arch_name()` / `__repr__`).  It is the
/// public `strider.Sleigh` class and the `RunResult.sleigh` handle whose
/// `regs` stay accessible after a run.
#[pyclass(name = "Sleigh", module = "strider")]
pub struct PySleigh {
    pub(crate) arch_name: &'static str,
    /// Cached register table, probed once at construction.  Backs
    /// `reg(...)` lookups so callers can resolve register names without
    /// re-running the (non-trivial) regs probe.
    pub(crate) regs: rsleigh::SleighRegs,
}

impl PySleigh {
    /// Internal constructor (mirrors `#[new]`).  Lets the run-style
    /// helpers in `run.rs` build a PySleigh without going through
    /// PyO3's argument-conversion path.  Builds a `Sleigh` transiently to
    /// probe the register table, then drops it.
    pub(crate) fn new_internal(arch: PySleighArch, reader: AnyMemReader) -> PyResult<Self> {
        let sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
            .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;
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
    /// Construct a Sleigh for `arch` reading from `mem` (a `MemoryMap`
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

    /// Look up a register by Sleigh name and return its varnode.
    /// Returns `None` when the name is not in the register table for
    /// this arch.  Use the resulting `Vn` with pattern constructors
    /// like `phi_for(vn)` / `initial_var_for(vn)` /
    /// `function_arg_reg(vn)` to query the IR for occurrences of
    /// that specific register.
    fn reg(&self, name: &str) -> Option<PyVn> {
        self.regs.name_to_vn(name).map(PyVn::from_inner)
    }

    /// `Sleigh(arch=<preset>)`.
    fn __repr__(&self) -> String {
        format!("Sleigh(arch={})", self.arch_name)
    }
}

// ── PyVnSpace + PyVn ────────────────────────────────────────────────

/// One of Sleigh's built-in address spaces.  Frozen pyclass so users
/// can pass `VnSpace.RAM()` to builder methods that take a space
/// constraint (`load().space(...)`, `function_arg_stack(...)`, etc.)
/// without having to thread a `Sleigh` through.
///
/// Strider exposes the four standard Sleigh spaces via the `ram()`,
/// `register()`, `const_()`, and `unique()` classmethods.
// `#[gen_stub_pyclass]` derives `PyStubType` for `PyVnSpace` so the
// macro-emitted `.space(s: PyVnSpace)` signatures compile under
// `#[gen_stub_pymethods]`.  The existing `#[pymethods]` block below is
// unchanged — the hand-written `pattern.pyi` already documents the
// surface.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "VnSpace", module = "strider", frozen)]
#[derive(Clone, Copy)]
pub struct PyVnSpace {
    pub(crate) inner: rsleigh::VnSpace,
}

#[pymethods]
impl PyVnSpace {
    /// The RAM (main memory) address space.
    #[classmethod]
    fn ram(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::RAM }
    }
    /// The register address space.
    #[classmethod]
    fn register(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::REGISTER }
    }
    /// The constant ("const") address space.
    #[classmethod]
    fn const_(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::CONST }
    }
    /// The unique (temporary) address space.
    #[classmethod]
    fn unique(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::UNIQUE }
    }

    /// The space's name (`"RAM"`, `"REGISTER"`, `"CONST"`, `"UNIQUE"`,
    /// or `"OTHER"`).
    fn name(&self) -> &'static str {
        if self.inner == rsleigh::VnSpace::RAM { "RAM" }
        else if self.inner == rsleigh::VnSpace::REGISTER { "REGISTER" }
        else if self.inner == rsleigh::VnSpace::CONST { "CONST" }
        else if self.inner == rsleigh::VnSpace::UNIQUE { "UNIQUE" }
        else { "OTHER" }
    }

    /// `VnSpace.<name>` (lowercased).
    fn __repr__(&self) -> String {
        format!("VnSpace.{}", self.name().to_lowercase())
    }

    /// Equality on the underlying Sleigh space identity.
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

    /// Hash consistent with `__eq__` (keyed on the space identity).
    fn __hash__(&self) -> u64 {
        // Hash the inner identity (the shortcut byte that's the
        // PartialEq/Hash key on `rsleigh::VnSpace`) — NOT the heap
        // address of `self.inner`.  Two PyVnSpace instances wrapping
        // the same `VnSpace::RAM` live at different `&self.inner`
        // addresses, so address-based hashing violated Python's
        // `a == b ⇒ hash(a) == hash(b)` contract.
        u64::from(self.inner.shortcut_raw())
    }
}

/// A Sleigh varnode — `(space, offset, size_in_bytes)`.  Used as the
/// argument to pattern builders that pin a specific varnode
/// (`phi_for(vn)`, `initial_var_for(vn)`, `function_arg_reg(vn)`,
/// `function_arg_stack(space, offset)`).
///
/// Construct via:
/// * `Sleigh.reg("RAX")` — looks up a register's varnode by Sleigh
///   register name; returns `None` when the name isn't a register.
/// * `Vn(space, off, size)` — direct construction (for stack
///   varnodes, custom spaces).
// `#[gen_stub_pyclass]` derives `PyStubType` for `PyVn` so the
// `#[strider_pattern]`-emitted setter `for_vn(vn: PyVn)` (on
// `PyPhiPat`) type-checks under `#[gen_stub_pymethods]`.  Per
// EMISSION_SPEC's "type-info rules", every PyClass referenced as a
// method-argument type from a stub-gen-instrumented impl needs the
// derive even if its surface is hand-written in `pattern.pyi`.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Vn", module = "strider", frozen)]
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
        PyVnSpace { inner: self.inner.addr_space }
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
        // Delegate to rsleigh's `impl Display for Vn` (core_types.rs:139)
        // so the spelling tracks rsleigh upstream.  For a register varnode
        // this yields `<space-shortcut>[0x<off>]:<size>` (e.g. `%[0x20]:8`
        // for x86_64 RSP); for CONST-space, `0x<off>:<size>`.
        format!("{}", self.inner)
    }

    /// Equality on all three varnode fields (space, offset, size).
    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

    /// Hash consistent with `__eq__` (mixes space, offset, and size).
    fn __hash__(&self) -> u64 {
        // Mix all three Vn fields into the hash so varnodes that differ
        // only in `addr_space` (e.g. RAM[0x10]:8 vs REGISTER[0x10]:8)
        // don't collide.  Without `addr_space` in the mix, equal-offset/
        // equal-size varnodes in different spaces shared a bucket.
        let mut h = self.inner.addr_off;
        h ^= u64::from(self.inner.size).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= u64::from(self.inner.addr_space.shortcut_raw())
            .wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleigh>()?;
    m.add_class::<PyVnSpace>()?;
    m.add_class::<PyVn>()?;
    Ok(())
}
