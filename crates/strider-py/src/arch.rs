use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// A target architecture, from a preset such as `SleighArch.x86_64()`. Equal
/// when built from the same preset.
#[pyclass(name = "SleighArch", module = "strider.sleigh", frozen, eq, hash)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PySleighArch {
    pub(crate) inner: strider_target::SleighArch,
}

#[pymethods]
impl PySleighArch {
    /// The preset name (e.g. `"x86_64"`).
    fn name(&self) -> &'static str {
        self.inner.preset().name()
    }

    /// `"little"` or `"big"`.
    fn endianness(&self) -> &'static str {
        match self.inner.endianness() {
            strider_target::Endianness::Little => "little",
            strider_target::Endianness::Big => "big",
        }
    }

    /// The arch preset named `name` (one of `strider.sleigh.ARCH_PRESETS`).
    /// Raises `StriderError` for an unknown name.
    #[classmethod]
    fn preset(cls: &Bound<'_, PyType>, name: &str) -> PyResult<Self> {
        let known = strider_target::ArchPreset::ALL.iter().map(|p| p.name());
        if !known.clone().any(|n| n == name) {
            return Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "unknown arch preset {name:?}; known: {}",
                known.collect::<Vec<_>>().join(", ")
            )));
        }
        cls.call_method0(name)?.extract()
    }

    /// The calling-convention presets for this arch, in roster order.
    fn calling_conventions<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyTuple> {
        pyo3::types::PyTuple::new_bound(
            py,
            self.inner
                .calling_conventions()
                .map(|p| crate::cc::PyCallingConvention::from_preset(p).into_py(py))
                .collect::<Vec<_>>(),
        )
    }

    fn __repr__(&self) -> String {
        format!("SleighArch.{}()", self.name())
    }

    /// The varnode of register `name`, or `None`.
    fn reg(&self, name: &str) -> Option<crate::sleigh::PyVn> {
        self.inner
            .names()
            .regs()
            .name_to_vn(name)
            .map(crate::sleigh::PyVn::from_inner)
    }

    /// The register name of `vn`, or `None`.
    fn reg_name(&self, vn: &crate::sleigh::PyVn) -> Option<&'static str> {
        self.inner.names().regs().vn_to_name(vn.inner)
    }
}

#[rustfmt::skip]
forall_preset!(
    PySleighArch,
    strider_target::SleighArch,
    [
        x86_64, x86, mipsbe32, mipsle32, mipsbe64, mipsle64, arm, arm_be, arm_be_kernel,
        arm_thumb, aarch64, aarch64be, ppc32be, ppc32le, ppc64be, ppc64le,
    ]
);

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleighArch>()?;
    // A test checks the `forall_preset!` list above against this roster.
    m.add(
        "ARCH_PRESETS",
        pyo3::types::PyTuple::new_bound(
            py,
            strider_target::ArchPreset::ALL.iter().map(|p| p.name()),
        ),
    )?;
    Ok(())
}
