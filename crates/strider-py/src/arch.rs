//! `PySleighArch` — opaque wrapper over `strider_target::SleighArch` with one
//! Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

#[pyclass(name = "SleighArch", module = "strider", frozen)]
#[derive(Clone)]
pub struct PySleighArch {
    pub(crate) inner: strider_target::SleighArch,
    pub(crate) preset_name: &'static str,
}

#[pymethods]
impl PySleighArch {
    fn name(&self) -> &'static str {
        self.preset_name
    }

    fn __repr__(&self) -> String {
        format!("SleighArch.{}()", self.preset_name)
    }
}

forall_preset!(
    PySleighArch,
    strider_target::SleighArch,
    [
        x86_64, x86, mipsbe32, mipsle32, mipsbe64, mipsle64, arm, arm_be,
        arm_thumb, aarch64, aarch64be, ppc32be, ppc32le, ppc64be, ppc64le,
    ]
);

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleighArch>()
}
