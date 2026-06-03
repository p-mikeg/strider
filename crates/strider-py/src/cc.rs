//! `PyCallingConvention` — opaque wrapper over `strider_target::CallingConvention`
//! with one Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// Backing storage for [`PyCallingConvention`].
///
/// `Preset` carries a built-in `CallingConvention` whose static-string
/// register names are resolved lazily against a Sleigh at consumption
/// time.  `Custom` carries an already-resolved
/// `BuiltCallingConvention` produced by [`PyCallingConvention::custom`]
/// from user-supplied register-name lists — pre-resolution allows
/// validation to surface bad inputs (typos, ABI-invariant violations)
/// at construction time rather than at first use.
#[derive(Clone)]
pub(crate) enum CcImpl {
    Preset(strider_target::CallingConvention),
    Custom(Box<strider_target::BuiltCallingConvention>),
}

/// A calling convention.  Construct via a preset classmethod (e.g.
/// `CallingConvention.x86_64_systemv()`) or `CallingConvention.custom(...)`
/// for a binary whose ABI matches no built-in preset.
#[pyclass(name = "CallingConvention", module = "strider", frozen)]
#[derive(Clone)]
pub struct PyCallingConvention {
    pub(crate) inner: CcImpl,
    pub(crate) preset_name: &'static str,
}

// `x86_64_all_preserving` stays hand-written below because it carries
// a Python docstring that the macro form cannot reproduce.  Linux
// kernel + syscall presets fit the same zero-arg shape and run
// through the macro — see
// `docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md`
// for the full list and rationale.
forall_preset!(
    try PyCallingConvention,
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
    fn x86_64_all_preserving(_cls: &Bound<'_, PyType>) -> PyResult<Self> {
        let inner = strider_target::CallingConvention::x86_64_all_preserving()
            .map_err(|e| crate::errors::into_strider_err(e.into()))?;
        Ok(Self {
            inner: CcImpl::Preset(inner),
            preset_name: "x86_64_all_preserving",
        })
    }

    /// Build a custom calling convention from explicit register-name
    /// lists.  Resolves every name against `sleigh`'s register table
    /// and validates the canonical ABI invariants via
    /// [`strider_target::BuiltCallingConvention::try_new`] — typos
    /// (unknown register name) and invariant violations
    /// (SP listed in arg_passing_regs, LR not in callee_saved, etc.)
    /// surface as `StriderError` at construction time.
    ///
    /// Use this when none of the built-in presets matches the binary's
    /// ABI (custom hardware ABIs, in-house RPC dispatchers, hot-patch
    /// trampolines that pin a non-standard register set).
    ///
    /// Args:
    ///     sleigh: The `Sleigh` instance to resolve register names against.
    ///     arg_passing_regs: Register names passing positional args, in ABI order.
    ///     callee_saved_regs: Registers the callee must preserve.  When
    ///         `link_register` is set, it MUST appear here.
    ///     ret_val_regs: Integer return-value registers, in ABI order.
    ///     ret_val_regs_float: Float return-value registers, in ABI order.
    ///     stack_pointer: Register name of the hardware stack pointer.
    ///     stack_arg_offsets: Byte offsets from call-time SP for each
    ///         positional stack arg (after register args are exhausted).
    ///     ret_stack_pop: Net byte change `ret` inflicts on caller SP
    ///         (typically 4/8 on stack-push ISAs, 0 on link-register ISAs).
    ///     link_register: Optional register name of the link register
    ///         (ARM/AArch64/MIPS/PowerPC); pass `None` on x86/x86_64.
    ///     preserves_memory: `True` for transparent hooks
    ///         (`__fentry__`/`mcount`-style) that preserve memory.
    #[classmethod]
    #[pyo3(signature = (
        sleigh,
        arg_passing_regs,
        callee_saved_regs,
        ret_val_regs,
        ret_val_regs_float,
        stack_pointer,
        stack_arg_offsets,
        ret_stack_pop,
        link_register=None,
        preserves_memory=false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn custom(
        _cls: &Bound<'_, PyType>,
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        arg_passing_regs: Vec<String>,
        callee_saved_regs: Vec<String>,
        ret_val_regs: Vec<String>,
        ret_val_regs_float: Vec<String>,
        stack_pointer: String,
        stack_arg_offsets: Vec<i64>,
        ret_stack_pop: i64,
        link_register: Option<String>,
        preserves_memory: bool,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let resolve = |name: &str| {
            regs.name_to_vn(name).ok_or_else(|| {
                crate::errors::into_strider_err(anyhow::anyhow!(
                    "CallingConvention.custom: unknown register name {name:?}"
                ))
            })
        };
        let resolve_list = |names: &[String]| -> PyResult<Vec<rsleigh::Vn>> {
            names.iter().map(|n| resolve(n)).collect()
        };
        let arg_vns = resolve_list(&arg_passing_regs)?;
        let callee_vns = resolve_list(&callee_saved_regs)?;
        let ret_vns = resolve_list(&ret_val_regs)?;
        let ret_float_vns = resolve_list(&ret_val_regs_float)?;
        let sp_vn = resolve(&stack_pointer)?;
        let lr_vn = link_register.as_deref().map(resolve).transpose()?;
        let built = strider_target::BuiltCallingConvention::try_new(
            arg_vns,
            callee_vns,
            ret_vns,
            ret_float_vns,
            sp_vn,
            stack_arg_offsets,
            ret_stack_pop,
            lr_vn,
            preserves_memory,
        )
        .map_err(|e| crate::errors::into_strider_err(e.into()))?;
        Ok(Self {
            inner: CcImpl::Custom(Box::new(built)),
            preset_name: "custom",
        })
    }

    /// The preset name this convention was constructed from (e.g.
    /// `"x86_64_systemv"`), or `"custom"` for a `custom(...)`-built one.
    fn name(&self) -> &'static str {
        self.preset_name
    }

    /// `CallingConvention.<preset>()` — the constructor call that produces
    /// this convention.
    fn __repr__(&self) -> String {
        format!("CallingConvention.{}()", self.preset_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()
}

