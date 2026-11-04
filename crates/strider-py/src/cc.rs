use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// `Preset` resolves its register names at use; `Custom` at construction,
/// so a bad name raises there.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum CcImpl {
    Preset(strider_target::CallingConvention),
    Custom(Box<strider_target::BuiltCallingConvention>),
}

/// A calling convention: a preset (e.g. `CallingConvention.x86_64_systemv()`)
/// or `CallingConvention.custom(...)`. Equal when built by the same
/// constructor calls; a `custom` never equals a preset.
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
    /// `None` for a preset.
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
    /// A variant that clobbers nothing: every register callee-saved, memory
    /// unchanged, no arguments or return value; stack geometry kept. For a
    /// transparent hook (`__fentry__` / `mcount`).
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

    /// Like `preserves_all`, but memory may be clobbered, so the call keeps its
    /// arguments.
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

    /// A convention from register names resolved against `sleigh`; an unknown
    /// name or a broken ABI invariant raises `StriderError`.
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

    /// The preset named `name` (one of `strider.sleigh.CC_PRESETS`). Raises
    /// `StriderError` for an unknown name.
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

    /// The keyword arguments of `custom` other than `sleigh`:
    /// `CallingConvention.custom(arch, **cc.to_dict())` rebuilds it. Raises
    /// `StriderError` for a `preserves_all()` / `preserves_regs()` variant.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        match &self.inner {
            CcImpl::Preset(cc) => dict(py, cc, Ok),
            CcImpl::Custom(cc) => {
                let arch = self.source_arch.expect("custom has a source arch");
                dict(py, cc, |vn| self.vn_name(arch, vn))
            }
        }
    }

    /// The preset name (e.g. `"x86_64_systemv"`), or `"custom"`.
    fn name(&self) -> &'static str {
        self.preset_name
    }

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

/// `name` spells each register.
fn dict<'py, R: Copy, L: AsRef<[R]>>(
    py: Python<'py>,
    cc: &strider_target::calling_convention::Cc<R, L>,
    name: impl Fn(R) -> PyResult<&'static str>,
) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
    if cc.preserves_all_registers {
        return Err(crate::errors::into_strider_err(anyhow::anyhow!(
            "CallingConvention.to_dict: a preserves_all() / preserves_regs() \
             variant is not expressible through custom(...)"
        )));
    }
    let names = |regs: &L| {
        regs.as_ref()
            .iter()
            .map(|&r| name(r))
            .collect::<PyResult<Vec<_>>>()
    };
    let d = pyo3::types::PyDict::new_bound(py);
    d.set_item("arg_passing_regs", names(&cc.arg_passing_regs)?)?;
    d.set_item("callee_saved_regs", names(&cc.callee_saved_regs)?)?;
    d.set_item("ret_val_regs", names(&cc.ret_val_regs)?)?;
    d.set_item("ret_val_regs_float", names(&cc.ret_val_regs_float)?)?;
    d.set_item("stack_pointer", name(cc.stack_ptr)?)?;
    d.set_item("stack_arg_base", cc.stack_args.map(|s| s.base_offset))?;
    d.set_item(
        "stack_arg_increment",
        cc.stack_args.map_or(0, |s| s.increment),
    )?;
    d.set_item("ret_stack_pop", cc.ret_stack_pop)?;
    d.set_item("link_register", cc.link_register.map(&name).transpose()?)?;
    d.set_item("preserves_memory", cc.preserves_memory)?;
    d.set_item("arg_passing_regs_float", names(&cc.arg_passing_regs_float)?)?;
    d.set_item("reg_args_adjoin_stack_args", cc.reg_args_adjoin_stack_args)?;
    d.set_item("entry_address", cc.entry_address.map(&name).transpose()?)?;
    Ok(d)
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()?;
    m.add(
        "CC_PRESETS",
        pyo3::types::PyTuple::new_bound(
            py,
            strider_target::CallingConvention::presets()
                .map(|p| p.name)
                .collect::<Vec<_>>(),
        ),
    )
}
