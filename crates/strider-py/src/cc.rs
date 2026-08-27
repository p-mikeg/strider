use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// `Preset` resolves its static register names against a Sleigh lazily, at
/// consumption time.  `Custom` is resolved eagerly instead, so typos and
/// ABI-invariant violations in user-supplied names surface at construction.
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
    /// The arch a `custom(...)` convention resolved its register names
    /// against; `None` for a preset, which resolves at consumption time.
    pub(crate) source_arch: Option<&'static str>,
    /// Only meaningful when this CC is resolved as a per-address override.
    pub(crate) no_return: bool,
}

forall_preset!(
    cc PyCallingConvention,
    strider_target::CallingConvention,
    [
        x86_64_systemv,
        aarch64_aapcs64,
        arm_aapcs,
        arm_aapcs_soft,
        mips_o32,
        mips_n64,
        powerpc_sysv32,
        powerpc64_elf_v1,
        powerpc64_elf_v2,
        x86_cdecl,
        x86_linux_kernel,
    ]
);

#[pymethods]
impl PyCallingConvention {
    /// A variant of this convention that clobbers nothing: every register
    /// callee-saved and memory unchanged, with no arguments or return value.
    /// The stack/link-register geometry of the original is kept.
    ///
    /// Use as a per-address CC override for a transparent hook that observes
    /// no caller state changes (Linux-kernel `__fentry__` / `mcount`), e.g.
    /// `CallingConvention.x86_64_systemv().preserves_all()`.
    fn preserves_all(&self) -> Self {
        Self {
            inner: match &self.inner {
                CcImpl::Preset(cc) => CcImpl::Preset(cc.preserves_all()),
                CcImpl::Custom(cc) => CcImpl::Custom(Box::new((**cc).clone().preserves_all())),
            },
            preset_name: self.preset_name,
            source_arch: self.source_arch,
            no_return: self.no_return,
        }
    }

    /// Like `preserves_all` but leaves memory clobberable: registers are all
    /// preserved, memory is not.
    fn preserves_regs(&self) -> Self {
        Self {
            inner: match &self.inner {
                CcImpl::Preset(cc) => CcImpl::Preset(cc.preserves_regs()),
                CcImpl::Custom(cc) => CcImpl::Custom(Box::new((**cc).clone().preserves_regs())),
            },
            preset_name: self.preset_name,
            source_arch: self.source_arch,
            no_return: self.no_return,
        }
    }

    /// A copy of this convention marked no-return, for a callee that never
    /// returns (`exit` / `abort` / `panic` / `__stack_chk_fail`).
    ///
    /// Use it as a per-address CC override, e.g.
    /// `CallingConvention.x86_64_systemv().no_return()`.  A call to such a
    /// target terminates its region, so the unreachable fall-through is not
    /// lifted.
    fn no_return(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            preset_name: self.preset_name,
            source_arch: self.source_arch,
            no_return: true,
        }
    }

    /// Build a calling convention from explicit register-name lists, for a
    /// binary whose ABI matches no built-in preset.  Names are resolved and
    /// ABI invariants checked here, so a bad one raises `StriderError` at
    /// construction rather than at first use.
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
    ///     arg_passing_regs_float: Float/vector argument registers, in ABI
    ///         order.  Appended after `arg_passing_regs` in a `Call`'s
    ///         argument list, so the first one is `arg(len(arg_passing_regs))`.
    ///
    /// The frozen varnodes belong to `sleigh`'s architecture, so the result is
    /// usable only with a `Lifter` for that same arch; any other raises
    /// `StriderError` at `analyze` time.
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
        arg_passing_regs_float=Vec::new(),
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
        arg_passing_regs_float: Vec<String>,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        let source_arch = sleigh_borrow.arch_name;
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
        let arg_float_vns = resolve_list(&arg_passing_regs_float)?;
        let callee_vns = resolve_list(&callee_saved_regs)?;
        let ret_vns = resolve_list(&ret_val_regs)?;
        let ret_float_vns = resolve_list(&ret_val_regs_float)?;
        let sp_vn = resolve(&stack_pointer)?;
        let lr_vn = link_register.as_deref().map(resolve).transpose()?;
        // A stack-pushing convention (no link register) has `call` push a
        // pointer-size return address; a smaller `ret_stack_pop` drifts SP.
        let ptr_size = i64::from(sp_vn.size);
        if lr_vn.is_none() && ret_stack_pop < ptr_size {
            return Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "custom(): a stack-pushing convention (link_register=None) needs \
                 ret_stack_pop >= {ptr_size} to account for the return address \
                 `call` pushes (x86: 4, x86-64: 8); got {ret_stack_pop}. A \
                 link-register arch (ARM/MIPS/PPC) uses 0 but must set link_register."
            )));
        }
        let stack_args = stack_arg_base.map(|base_offset| strider_target::StackArgs {
            base_offset,
            increment: stack_arg_increment,
        });
        let built = strider_target::BuiltCallingConvention {
            arg_passing_regs: arg_vns,
            arg_passing_regs_float: arg_float_vns,
            callee_saved_regs: callee_vns,
            ret_val_regs: ret_vns,
            ret_val_regs_float: ret_float_vns,
            stack_vn: sp_vn,
            stack_args,
            ret_stack_pop,
            link_register_vn: lr_vn,
            preserves_memory,
            preserves_all_registers: false,
            no_return: false,
        };
        built
            .validate()
            .map_err(|e| crate::errors::into_strider_err(e.into()))?;
        Ok(Self {
            inner: CcImpl::Custom(Box::new(built)),
            preset_name: "custom",
            source_arch: Some(source_arch),
            no_return: false,
        })
    }

    /// The preset name this convention was constructed from (e.g.
    /// `"x86_64_systemv"`), or `"custom"` for a `custom(...)`-built one.
    fn name(&self) -> &'static str {
        self.preset_name
    }

    /// `CallingConvention.<preset>()` for a preset.  A `custom(...)` one has
    /// no argument-free reproduction, so it renders as a plain description
    /// naming the arch its registers were resolved against.
    fn __repr__(&self) -> String {
        match self.source_arch {
            Some(arch) => format!("<CallingConvention custom for {arch}>"),
            None => format!("CallingConvention.{}()", self.preset_name),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()
}
