//! `PySleighArch` — opaque wrapper over `target::SleighArch` with one
//! Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

#[pyclass(name = "SleighArch", module = "strider", frozen)]
#[derive(Clone)]
pub struct PySleighArch {
    pub(crate) inner: target::SleighArch,
    pub(crate) preset_name: &'static str,
}

#[pymethods]
impl PySleighArch {
    #[classmethod]
    fn x86_64(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::x86_64(), preset_name: "x86_64" }
    }
    #[classmethod]
    fn x86(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::x86(), preset_name: "x86" }
    }
    #[classmethod]
    fn mipsbe32(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::mipsbe32(), preset_name: "mipsbe32" }
    }
    #[classmethod]
    fn mipsle32(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::mipsle32(), preset_name: "mipsle32" }
    }
    #[classmethod]
    fn mipsbe64(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::mipsbe64(), preset_name: "mipsbe64" }
    }
    #[classmethod]
    fn mipsle64(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::mipsle64(), preset_name: "mipsle64" }
    }
    #[classmethod]
    fn arm(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::arm(), preset_name: "arm" }
    }
    #[classmethod]
    fn arm_be(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::arm_be(), preset_name: "arm_be" }
    }
    #[classmethod]
    fn arm_thumb(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::arm_thumb(), preset_name: "arm_thumb" }
    }
    #[classmethod]
    fn aarch64(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::aarch64(), preset_name: "aarch64" }
    }
    #[classmethod]
    fn aarch64be(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::aarch64be(), preset_name: "aarch64be" }
    }
    #[classmethod]
    fn ppc32be(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::ppc32be(), preset_name: "ppc32be" }
    }
    #[classmethod]
    fn ppc32le(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::ppc32le(), preset_name: "ppc32le" }
    }
    #[classmethod]
    fn ppc64be(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::ppc64be(), preset_name: "ppc64be" }
    }
    #[classmethod]
    fn ppc64le(_cls: &Bound<'_, PyType>) -> Self {
        Self { inner: target::SleighArch::ppc64le(), preset_name: "ppc64le" }
    }

    fn name(&self) -> &'static str {
        self.preset_name
    }

    fn __repr__(&self) -> String {
        format!("SleighArch.{}()", self.preset_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleighArch>()
}
