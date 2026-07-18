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
#[pyclass(name = "CallingConvention", module = "strider.sleigh", frozen)]
#[derive(Clone)]
pub struct PyCallingConvention {
    pub(crate) inner: CcImpl,
    pub(crate) preset_name: &'static str,
    /// `true` when this convention has been marked no-return via
    /// [`PyCallingConvention::no_return`].  Applied to the built
    /// [`strider_target::BuiltCallingConvention::no_return`] when this CC is
    /// resolved as a per-address override (see `build_per_address_ccs`); a call
    /// TARGET carrying a no-return CC terminates its calling region.
    pub(crate) no_return: bool,
}

// `x86_64_all_preserving` stays hand-written below because it carries
// a Python docstring that the macro form cannot reproduce.
// `x86_linux_kernel` is the sole Linux preset that diverges from a
// userland ABI (regparm-3); every other arch's kernel CC equals its
// userland preset, and syscalls are `CallOther` (not calling
// conventions), so neither has a preset here.
forall_preset!(
    cc PyCallingConvention,
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
        // Linux kernel preset (x86 32-bit regparm-3 is the only divergent one)
        x86_linux_kernel,
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
            inner: CcImpl::Preset(strider_target::CallingConvention::x86_64_all_preserving()),
            preset_name: "x86_64_all_preserving",
            no_return: false,
        }
    }

    /// Returns a copy of this calling convention marked **no-return** — a
    /// callee that never returns (`exit` / `abort` / `panic` /
    /// `__stack_chk_fail` / …).
    ///
    /// Use it as a per-address CC override — `CallingConvention.x86_64_systemv()
    /// .no_return()`, or `CallingConvention.custom(...).no_return()` to also
    /// capture the call's arguments precisely.  During analysis the CFG builder
    /// terminates the calling region at a call to such a target (lowered to
    /// `Call + Unreachable`), so the unreachable fall-through — including a
    /// *mid-function* one, e.g. `if (err) panic();` followed by live code — is
    /// not lifted as reachable.  This is the precise counterpart to the
    /// function-end structural fallback (a call whose return address leaves the
    /// function bound is already treated as unreachable-after without any
    /// override).
    fn no_return(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            preset_name: self.preset_name,
            no_return: true,
        }
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
    ///     stack_arg_base: Byte offset from call-time SP of the first
    ///         positional stack arg (after register args are exhausted), or
    ///         `None` if the convention passes no args on the stack.
    ///     stack_arg_increment: Byte stride between successive positional
    ///         stack args (used only when `stack_arg_base` is set).
    ///     ret_stack_pop: Net byte change `ret` inflicts on caller SP
    ///         (typically 4/8 on stack-push ISAs, 0 on link-register ISAs).
    ///     link_register: Optional register name of the link register
    ///         (ARM/AArch64/MIPS/PowerPC); pass `None` on x86/x86_64.
    ///     preserves_memory: `True` for transparent hooks
    ///         (`__fentry__`/`mcount`-style) that preserve memory.
    ///
    /// Chain `.no_return()` on the result to mark the callee as never-returning.
    #[classmethod]
    #[pyo3(signature = (
        sleigh,
        arg_passing_regs,
        callee_saved_regs,
        ret_val_regs,
        ret_val_regs_float,
        stack_pointer,
        stack_arg_base,
        stack_arg_increment,
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
        stack_arg_base: Option<i128>,
        stack_arg_increment: i128,
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
        let stack_args = stack_arg_base.map(|base_offset| strider_target::StackArgs {
            base_offset,
            increment: stack_arg_increment,
        });
        // Route through the shared validating constructor via the named-field
        // parts struct so this path can't transpose the two same-typed return
        // lists (the whole point of the parts struct).  The Python-facing
        // keyword signature above stays as individual PyO3 arguments — PyO3
        // needs them named individually to preserve the public API — so this
        // method keeps `#[allow(clippy::too_many_arguments)]`.
        let built = strider_target::BuiltCallingConvention::try_new(
            strider_target::BuiltCallingConventionParts {
                arg_passing_regs: arg_vns,
                callee_saved_regs: callee_vns,
                ret_val_regs: ret_vns,
                ret_val_regs_float: ret_float_vns,
                stack_vn: sp_vn,
                stack_args,
                ret_stack_pop,
                link_register_vn: lr_vn,
                preserves_memory,
            },
        )
        .map_err(|e| crate::errors::into_strider_err(e.into()))?;
        Ok(Self {
            inner: CcImpl::Custom(Box::new(built)),
            preset_name: "custom",
            no_return: false,
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
