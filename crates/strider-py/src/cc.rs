//! `PyCallingConvention` — opaque wrapper over `strider_target::CallingConvention`
//! with one Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

#[pyclass(name = "CallingConvention", module = "strider", frozen)]
#[derive(Clone)]
pub struct PyCallingConvention {
    pub(crate) inner: strider_target::CallingConvention,
    pub(crate) preset_name: &'static str,
}

// Stamp out one `#[classmethod] fn $name(_cls) -> Self` per preset
// name in its own `#[pymethods]` block.  Each classmethod has the
// same shape — name appears three times (Python method name, Rust
// factory call, stored `preset_name` static-string).  Driving the
// list once eliminates the repetition while preserving the Python
// API (`CallingConvention.x86_64_systemv()` etc.) byte-for-byte.
// Relies on PyO3's `multiple-pymethods` feature.
//
// `x86_64_all_preserving` stays hand-written below because it carries
// a Python docstring that the macro form cannot reproduce.  Linux
// kernel + syscall presets fit the same zero-arg shape and run
// through the macro — see
// `docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md`
// for the full list and rationale.
macro_rules! forall_preset {
    ($self_ty:ty, $inner_ty:ty, [$($name:ident),* $(,)?]) => {
        #[pymethods]
        impl $self_ty {
            $(
                #[classmethod]
                fn $name(_cls: &Bound<'_, PyType>) -> Self {
                    Self {
                        inner: <$inner_ty>::$name(),
                        preset_name: stringify!($name),
                    }
                }
            )*
        }
    };
}

forall_preset!(
    PyCallingConvention,
    strider_target::CallingConvention,
    [
        // Userland presets
        x86_64_systemv,
        aarch64_aapcs64,
        arm_aapcs,
        mips_o32,
        mips_n64,
        powerpc_sysv32,
        powerpc64_elf_v1,
        powerpc64_elf_v2,
        x86_cdecl,
        // Linux kernel presets
        x86_linux_kernel,
        x86_64_linux_kernel,
        aarch64_linux_kernel,
        arm_linux_kernel,
        mips_linux_kernel_o32,
        mips_linux_kernel_n64,
        // Linux syscall presets
        x86_linux_syscall,
        x86_64_linux_syscall,
        aarch64_linux_syscall,
        arm_linux_syscall,
        mips_linux_syscall_o32,
        mips_linux_syscall_n64,
    ]
);

#[pymethods]
impl PyCallingConvention {
    /// "All-preserving" x86_64 calling convention: every userland
    /// caller-clobbered register is listed as callee-saved.  Use as a
    /// per-address CC override for sites that observe no caller state
    /// changes (e.g. Linux-kernel `__fentry__` / `mcount`).
    #[classmethod]
    fn x86_64_all_preserving(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: strider_target::CallingConvention::x86_64_all_preserving(),
            preset_name: "x86_64_all_preserving",
        }
    }

    fn name(&self) -> &'static str {
        self.preset_name
    }

    fn __repr__(&self) -> String {
        format!("CallingConvention.{}()", self.preset_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()
}

/// Resolve the calling-convention preset's static-string register names
/// against `sleigh`'s register table to produce a [`strider_target::BuiltCallingConvention`].
///
/// Centralises the pattern that the strider-py FFI layer used to repeat
/// at every constructor that needs a built CC (StackStoreDetect,
/// StackLoadForward, FunctionArgDetect, CallStackArgCollect, Strider).
/// The pattern was: borrow Sleigh, clone regs, drop borrow, call
/// `cc.inner.build(&regs)`, map LiftError.
pub(crate) fn build_cc_for_sleigh(
    py: Python<'_>,
    sleigh: &Py<crate::sleigh::PySleigh>,
    cc: &PyCallingConvention,
) -> PyResult<strider_target::BuiltCallingConvention> {
    let sleigh_borrow = sleigh.borrow(py);
    let regs = sleigh_borrow.regs.clone();
    drop(sleigh_borrow);
    cc.inner.build(&regs).map_err(crate::errors::into_lift_err)
}
