//! `PySleighArch` — opaque wrapper over `strider_target::SleighArch` with one
//! Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

#[pyclass(name = "SleighArch", module = "strider", frozen)]
#[derive(Clone)]
pub struct PySleighArch {
    pub(crate) inner: strider_target::SleighArch,
    pub(crate) preset_name: &'static str,
}

// Stamp out one `#[classmethod] fn $name(_cls) -> Self` per preset
// name, inside its own `#[pymethods] impl $ty { … }` block.  Each
// classmethod has the same 4-line shape — name appears three times
// (Python method name, Rust factory call, stored `preset_name`
// static-string).  Driving the list once eliminates the repetition
// while preserving the Python API (`SleighArch.x86_64()` etc.)
// byte-for-byte.  Relies on PyO3's `multiple-pymethods` feature so
// `#[pyclass]` can carry more than one `#[pymethods]` block.
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
