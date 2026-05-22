//! Crate-local declarative macros shared across the strider-py modules.
//!
//! `forall_preset!` stamps out one `#[classmethod] fn $name(_cls) -> …`
//! per preset name for the `PySleighArch` / `PyCallingConvention`
//! opaque-preset wrappers.  Each classmethod has the same shape — the
//! preset name appears three times (Python method name, Rust factory
//! call on `$inner_ty`, stored `preset_name` static-string).  Driving
//! the preset list once eliminates the repetition while preserving the
//! Python API byte-for-byte.  Relies on PyO3's `multiple-pymethods`
//! feature so `#[pyclass]` can carry more than one `#[pymethods]`
//! block.
//!
//! Two arms cover the inner-factory return-type split:
//!
//! - default arm — inner ctor returns `Self` (used by `SleighArch`).
//! - `try` arm — inner ctor returns `Result<_, _>`; the wrapper lifts
//!   the error through `errors::into_lift_err` (used by
//!   `CallingConvention`).

macro_rules! forall_preset {
    // Fallible: inner ctor returns Result; lift the error.
    (try $self_ty:ty, $inner_ty:ty, [$($name:ident),* $(,)?]) => {
        #[pymethods]
        impl $self_ty {
            $(
                #[classmethod]
                fn $name(_cls: &Bound<'_, PyType>) -> PyResult<Self> {
                    let inner = <$inner_ty>::$name()
                        .map_err(|e| crate::errors::into_lift_err(e.into()))?;
                    Ok(Self {
                        inner,
                        preset_name: stringify!($name),
                    })
                }
            )*
        }
    };
    // Infallible: inner ctor returns Self.
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

pub(crate) use forall_preset;
