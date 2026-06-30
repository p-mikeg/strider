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
//! Two arms cover the differing inner storage of the two wrappers:
//!
//! - default arm — stores the inner preset directly (used by
//!   `SleighArch`).
//! - `cc` arm — wraps the inner preset in `CcImpl::Preset(...)` (used by
//!   `CallingConvention`).
//!
//! Both inner factories are infallible (every named preset has a
//! registered row), so each classmethod returns `Self` directly.

macro_rules! forall_preset {
    // CC wrapper: stores the inner preset in `CcImpl::Preset(...)`.
    (cc $self_ty:ty, $inner_ty:ty, [$($name:ident),* $(,)?]) => {
        #[pymethods]
        impl $self_ty {
            $(
                #[doc = concat!(
                    "Preset `", stringify!($name),
                    "` calling convention (resolved lazily against a Sleigh \
                     register table at consumption time)."
                )]
                #[classmethod]
                fn $name(_cls: &Bound<'_, PyType>) -> Self {
                    Self {
                        inner: crate::cc::CcImpl::Preset(<$inner_ty>::$name()),
                        preset_name: stringify!($name),
                    }
                }
            )*
        }
    };
    // Infallible: inner ctor returns Self.
    ($self_ty:ty, $inner_ty:ty, [$($name:ident),* $(,)?]) => {
        #[pymethods]
        impl $self_ty {
            $(
                #[doc = concat!(
                    "`", stringify!($name),
                    "` architecture preset (Sleigh `.sla` + `.pspec` + endianness)."
                )]
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
