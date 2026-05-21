//! `PySleigh` — wraps a constructed `rsleigh::Sleigh` keyed off a
//! `PySleighArch` + a memory reader (either `PyMemoryMap` or any
//! `MemReader` subclass).  Holds the `Sleigh` in an `Option` so it can
//! be moved into a downstream consumer (`strider_lift::cfg::Builder`, which takes
//! the Sleigh by value) and then put back when the consumer hands it
//! back via `Cfg::sleigh`.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_lift_err;
use crate::reader::{AnyMemReader, ReaderInput};

/// A constructed Sleigh keyed off a (SleighArch, reader) pair.
///
/// The inner `Sleigh<AnyMemReader>` is held in an `Option` so it can be
/// moved out (into a `strider_lift::cfg::Builder`, for example) and put back later
/// via `put_inner`.  While the inner is `None` the wrapper is "in use"
/// by some downstream consumer; further moves fail with `LiftError`.
#[pyclass(name = "Sleigh", module = "strider")]
pub struct PySleigh {
    pub(crate) inner: Option<rsleigh::Sleigh<AnyMemReader>>,
    pub(crate) arch_name: &'static str,
    /// Retained so `build_cfg` can route through `strider_lift::cfg::Builder::for_arch`
    /// (carrying the actual arch preset, vs. the deleted `Builder::new`'s
    /// default `ArchPreset::X86_64` which used to silently mis-classify
    /// CallOther on non-x86 targets).
    pub(crate) arch: target::SleighArch,
    /// Cached register table.  `Sleigh::regs()` only requires `&self`,
    /// but we eagerly cache it at construction time so callers can read
    /// the registers after the inner Sleigh has been moved into a
    /// downstream consumer (e.g. `strider_lift::cfg::Builder`).
    pub(crate) regs: rsleigh::SleighRegs,
}

impl PySleigh {
    /// Move the inner Sleigh out, leaving the wrapper as "in use".
    /// Returns `None` if it is already in use.
    pub(crate) fn take_inner(&mut self) -> Option<rsleigh::Sleigh<AnyMemReader>> {
        self.inner.take()
    }

    /// Restore the inner Sleigh, typically the one harvested out of
    /// `Cfg::sleigh` when a consumer hands it back.
    #[allow(dead_code)]
    pub(crate) fn put_inner(&mut self, sleigh: rsleigh::Sleigh<AnyMemReader>) {
        self.inner = Some(sleigh);
    }

    /// Internal constructor (mirrors `#[new]`).  Lets the run-style
    /// helpers in `run.rs` build a PySleigh without going through
    /// PyO3's argument-conversion path.
    pub(crate) fn new_internal(arch: PySleighArch, reader: AnyMemReader) -> PyResult<Self> {
        let inner = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
            .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;
        let regs = inner
            .regs()
            .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::regs failed: {e:?}")))?;
        Ok(Self {
            inner: Some(inner),
            arch_name: arch.preset_name,
            arch: arch.inner,
            regs,
        })
    }
}

#[pymethods]
impl PySleigh {
    #[new]
    fn new(arch: PySleighArch, mem: ReaderInput) -> PyResult<Self> {
        let reader = mem.into_any().map_err(into_lift_err)?;
        Self::new_internal(arch, reader)
    }

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
/// Strider exposes the four standard Sleigh spaces; binaries that
/// reference an exotic custom space can construct one via the
/// `VnSpace.from_id(u32)` escape hatch.
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
    #[classmethod]
    fn ram(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::RAM }
    }
    #[classmethod]
    fn register(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::REGISTER }
    }
    #[classmethod]
    fn const_(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::CONST }
    }
    #[classmethod]
    fn unique(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: rsleigh::VnSpace::UNIQUE }
    }

    fn name(&self) -> &'static str {
        if self.inner == rsleigh::VnSpace::RAM { "RAM" }
        else if self.inner == rsleigh::VnSpace::REGISTER { "REGISTER" }
        else if self.inner == rsleigh::VnSpace::CONST { "CONST" }
        else if self.inner == rsleigh::VnSpace::UNIQUE { "UNIQUE" }
        else { "OTHER" }
    }

    fn __repr__(&self) -> String {
        format!("VnSpace.{}", self.name().to_lowercase())
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

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
// Phase 4 Task 4.2b — `#[gen_stub_pyclass]` derives `PyStubType` for
// `PyVn` so the `#[strider_pattern]`-emitted setter `for_vn(vn: PyVn)`
// (on `PyPhiPat`) type-checks under `#[gen_stub_pymethods]`.  Per
// EMISSION_SPEC Task 4.0 "type-info rules", every PyClass referenced
// as a method-argument type from a stub-gen-instrumented impl needs
// the derive even if its surface is hand-written in `pattern.pyi`.
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

    #[getter]
    fn space(&self) -> PyVnSpace {
        PyVnSpace { inner: self.inner.addr_space }
    }
    #[getter]
    fn off(&self) -> u64 {
        self.inner.addr_off
    }
    #[getter]
    fn size(&self) -> u32 {
        self.inner.size
    }

    fn __repr__(&self) -> String {
        // Delegate to rsleigh's `impl Display for Vn` (core_types.rs:139)
        // so the spelling tracks rsleigh upstream.  For a register varnode
        // this yields `<space-shortcut>[0x<off>]:<size>` (e.g. `%[0x20]:8`
        // for x86_64 RSP); for CONST-space, `0x<off>:<size>`.
        format!("{}", self.inner)
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }

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
