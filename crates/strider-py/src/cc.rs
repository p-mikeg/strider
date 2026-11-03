use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// `Preset` resolves its static register names against a Sleigh lazily, at
/// consumption time.  `Custom` is resolved eagerly instead, so typos and
/// ABI-invariant violations in user-supplied names surface at construction.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum CcImpl {
    Preset(strider_target::CallingConvention),
    Custom(Box<strider_target::BuiltCallingConvention>),
}

/// A calling convention.  Construct via a preset classmethod (e.g.
/// `CallingConvention.x86_64_systemv()`) or `CallingConvention.custom(...)`
/// for a binary whose ABI matches no built-in preset.
///
/// Equal when the same constructor calls built both: a `custom` never equals a
/// preset, nor one resolved against another arch.
#[pyclass(
    name = "CallingConvention",
    module = "strider.sleigh",
    frozen,
    eq,
    hash
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyCallingConvention {
    pub(crate) inner: CcImpl,
    pub(crate) preset_name: &'static str,
    /// The arch a `custom(...)` convention resolved its register names
    /// against; `None` for a preset, which resolves at consumption time.
    pub(crate) source_arch: Option<strider_target::SleighArch>,
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
        powerpc_sysv32_linux_kernel,
        powerpc64_elf_v1_linux_kernel,
        powerpc64_elf_v2_linux_kernel,
    ]
);

impl PyCallingConvention {
    pub(crate) fn from_preset(p: &'static strider_target::CcPreset) -> Self {
        Self {
            inner: CcImpl::Preset(p.cc),
            preset_name: p.name,
            source_arch: None,
        }
    }

    fn check_expressible(&self, preserves_all_registers: bool) -> PyResult<()> {
        if preserves_all_registers {
            return Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "CallingConvention.to_dict: a preserves_all() / preserves_regs() \
                 variant is not expressible through custom(...)"
            )));
        }
        Ok(())
    }

    fn vn_name(&self, arch: strider_target::SleighArch, vn: rsleigh::Vn) -> PyResult<&'static str> {
        arch.names().regs().vn_to_name(vn).ok_or_else(|| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "CallingConvention.to_dict: no register name for {vn:?}"
            ))
        })
    }
}

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
        }
    }

    /// Like `preserves_all` but leaves memory clobberable: registers are all
    /// preserved, memory is not.
    ///
    /// The argument registers and stack arguments survive, unlike under
    /// `preserves_all`, so a call still carries its arguments: a callee that
    /// writes memory has to be seen taking a frame address.
    fn preserves_regs(&self) -> Self {
        Self {
            inner: match &self.inner {
                CcImpl::Preset(cc) => CcImpl::Preset(cc.preserves_regs()),
                CcImpl::Custom(cc) => CcImpl::Custom(Box::new((**cc).clone().preserves_regs())),
            },
            preset_name: self.preset_name,
            source_arch: self.source_arch,
        }
    }

    /// Build a calling convention from explicit register-name lists, for a
    /// binary whose ABI matches no built-in preset.  Names are resolved and
    /// most ABI invariants checked here, so a bad one raises `StriderError` at
    /// construction rather than at first use.
    ///
    /// Args:
    ///     sleigh: The `SleighArch` to resolve register names against.
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
    ///     reg_args_adjoin_stack_args: `True` where each argument register
    ///         has a stack home just below the stack arguments, one slot each
    ///         (ARM AAPCS, MIPS O32 / N64, PowerPC64 ELF): a by-value
    ///         argument split between registers and the stack is then one
    ///         object, and its address reaching a callee keeps every
    ///         incoming stack argument read after a call unknown.
    ///     entry_address: Register name the ABI guarantees holds the
    ///         function's own entry address on entry (PPC64 ELFv2 `r12`,
    ///         MIPS abicalls `t9`), or `None` for no such promise.
    ///
    /// Raises `StriderError` here when `stack_pointer` is not as wide as the
    /// arch's addresses, when `stack_arg_base` is set and `stack_arg_increment`
    /// is not that width, or when `link_register=None` and `ret_stack_pop` is
    /// below it.
    ///
    /// The frozen varnodes belong to `sleigh`'s registers, so the result is
    /// usable only with a `Lifter` with the same register table (`arm`,
    /// `arm_thumb` and `arm_be_kernel` share one); any other raises
    /// `StriderError` at `analyze` time.
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
        reg_args_adjoin_stack_args=false,
        entry_address=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn custom(
        _cls: &Bound<'_, PyType>,
        sleigh: crate::arch::PySleighArch,
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
        reg_args_adjoin_stack_args: bool,
        entry_address: Option<String>,
    ) -> PyResult<Self> {
        let source_arch = sleigh.inner;
        let regs = source_arch.names().regs();
        let in_field = |field: &str, e: anyhow::Error| {
            crate::errors::into_strider_err(e.context(format!("CallingConvention.custom: {field}")))
        };
        let list = |field: &str, names: &[String]| {
            strider_target::calling_convention::regs_to_vns(regs, names)
                .map_err(|e| in_field(field, e))
        };
        let one = |field: &str, name: &str| {
            strider_target::calling_convention::vn_for_name(regs, name)
                .map_err(|e| in_field(field, e))
        };
        let arg_vns = list("arg_passing_regs", &arg_passing_regs)?;
        let arg_float_vns = list("arg_passing_regs_float", &arg_passing_regs_float)?;
        let callee_vns = list("callee_saved_regs", &callee_saved_regs)?;
        let ret_vns = list("ret_val_regs", &ret_val_regs)?;
        let ret_float_vns = list("ret_val_regs_float", &ret_val_regs_float)?;
        let sp_vn = one("stack_pointer", &stack_pointer)?;
        let lr_vn = link_register
            .as_deref()
            .map(|name| one("link_register", name))
            .transpose()?;
        let entry_vn = entry_address
            .as_deref()
            .map(|name| one("entry_address", name))
            .transpose()?;
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
            stack_ptr: sp_vn,
            stack_args,
            ret_stack_pop,
            link_register: lr_vn,
            preserves_memory,
            preserves_all_registers: false,
            entry_address: entry_vn,
            reg_args_adjoin_stack_args,
        };
        built.validate().map_err(crate::errors::into_strider_err)?;
        built
            .check_addr_size(source_arch.names().ram_addr_size())
            .map_err(crate::errors::into_strider_err)?;
        Ok(Self {
            inner: CcImpl::Custom(Box::new(built)),
            preset_name: "custom",
            source_arch: Some(source_arch),
        })
    }

    /// The preset named `name` (one of `strider.sleigh.CC_PRESETS`), as
    /// `CallingConvention.<name>()` builds it.  Raises `StriderError` for an
    /// unknown name, listing the known ones.
    #[classmethod]
    fn preset(_cls: &Bound<'_, PyType>, name: &str) -> PyResult<Self> {
        strider_target::CallingConvention::presets()
            .find(|p| p.name == name)
            .map(Self::from_preset)
            .ok_or_else(|| {
                let known: Vec<_> = strider_target::CallingConvention::presets()
                    .map(|p| p.name)
                    .collect();
                crate::errors::into_strider_err(anyhow::anyhow!(
                    "unknown calling convention preset {name:?}; known: {}",
                    known.join(", ")
                ))
            })
    }

    /// The keyword arguments of `custom` other than `sleigh`, so
    /// `CallingConvention.custom(arch, **cc.to_dict())` rebuilds an equivalent
    /// convention.  A preset yields its own register names; a `custom` one the
    /// names of its resolved registers.  `stack_arg_increment` is `0` when
    /// the convention passes nothing on the stack.
    ///
    /// Raises `StriderError` for a `preserves_all()` / `preserves_regs()`
    /// variant: `custom` cannot express `preserves_all_registers`.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        use pyo3::types::PyDict;
        let d = PyDict::new_bound(py);
        match &self.inner {
            CcImpl::Preset(cc) => {
                self.check_expressible(cc.preserves_all_registers)?;
                d.set_item("arg_passing_regs", cc.arg_passing_regs.to_vec())?;
                d.set_item("callee_saved_regs", cc.callee_saved_regs.to_vec())?;
                d.set_item("ret_val_regs", cc.ret_val_regs.to_vec())?;
                d.set_item("ret_val_regs_float", cc.ret_val_regs_float.to_vec())?;
                d.set_item("stack_pointer", cc.stack_ptr)?;
                d.set_item("stack_arg_base", cc.stack_args.map(|s| s.base_offset))?;
                d.set_item(
                    "stack_arg_increment",
                    cc.stack_args.map_or(0, |s| s.increment),
                )?;
                d.set_item("ret_stack_pop", cc.ret_stack_pop)?;
                d.set_item("link_register", cc.link_register)?;
                d.set_item("preserves_memory", cc.preserves_memory)?;
                d.set_item("arg_passing_regs_float", cc.arg_passing_regs_float.to_vec())?;
                d.set_item("reg_args_adjoin_stack_args", cc.reg_args_adjoin_stack_args)?;
                d.set_item("entry_address", cc.entry_address)?;
            }
            CcImpl::Custom(cc) => {
                self.check_expressible(cc.preserves_all_registers)?;
                let arch = self.source_arch.expect("custom has a source arch");
                let one = |vn| self.vn_name(arch, vn);
                let names = |vns: &[rsleigh::Vn]| {
                    vns.iter().map(|&vn| one(vn)).collect::<PyResult<Vec<_>>>()
                };
                d.set_item("arg_passing_regs", names(&cc.arg_passing_regs)?)?;
                d.set_item("callee_saved_regs", names(&cc.callee_saved_regs)?)?;
                d.set_item("ret_val_regs", names(&cc.ret_val_regs)?)?;
                d.set_item("ret_val_regs_float", names(&cc.ret_val_regs_float)?)?;
                d.set_item("stack_pointer", one(cc.stack_ptr)?)?;
                d.set_item("stack_arg_base", cc.stack_args.map(|s| s.base_offset))?;
                d.set_item(
                    "stack_arg_increment",
                    cc.stack_args.map_or(0, |s| s.increment),
                )?;
                d.set_item("ret_stack_pop", cc.ret_stack_pop)?;
                d.set_item("link_register", cc.link_register.map(one).transpose()?)?;
                d.set_item("preserves_memory", cc.preserves_memory)?;
                d.set_item("arg_passing_regs_float", names(&cc.arg_passing_regs_float)?)?;
                d.set_item("reg_args_adjoin_stack_args", cc.reg_args_adjoin_stack_args)?;
                d.set_item("entry_address", cc.entry_address.map(one).transpose()?)?;
            }
        }
        Ok(d)
    }

    /// The preset name this convention was constructed from (e.g.
    /// `"x86_64_systemv"`), or `"custom"` for a `custom(...)`-built one.
    fn name(&self) -> &'static str {
        self.preset_name
    }

    /// `CallingConvention.<preset>()` for a preset, then the `preserves_*`
    /// variant taken of it.  A `custom(...)` one has no argument-free
    /// reproduction, so it renders as a plain description naming the arch its
    /// registers were resolved against.
    fn __repr__(&self) -> String {
        let (all_regs, memory) = match &self.inner {
            CcImpl::Preset(cc) => (cc.preserves_all_registers, cc.preserves_memory),
            CcImpl::Custom(cc) => (cc.preserves_all_registers, cc.preserves_memory),
        };
        let variant = match (all_regs, memory) {
            (false, _) => "",
            (true, true) => ".preserves_all()",
            (true, false) => ".preserves_regs()",
        };
        match self.source_arch {
            Some(arch) => format!(
                "<CallingConvention custom{variant} for {}>",
                arch.preset().name()
            ),
            None => format!("CallingConvention.{}(){variant}", self.preset_name),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()
}
