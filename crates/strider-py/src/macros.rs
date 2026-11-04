//! `forall_preset!` stamps out one `#[classmethod]` per preset name for the
//! `SleighArch` / `CallingConvention` wrappers (needs PyO3's
//! `multiple-pymethods`).

macro_rules! forall_preset {
    (cc $self_ty:ty, $inner_ty:ty, [$($name:ident),* $(,)?]) => {
        #[pymethods]
        impl $self_ty {
            $(
                #[doc = concat!(
                    "Preset `", stringify!($name),
                    "` calling convention."
                )]
                #[classmethod]
                fn $name(_cls: &Bound<'_, PyType>) -> Self {
                    Self {
                        inner: crate::cc::CcImpl::Preset(<$inner_ty>::$name()),
                        preset_name: stringify!($name),
                        source_arch: None,
                    }
                }
            )*
        }
    };
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
                    }
                }
            )*
        }
    };
}

pub(crate) use forall_preset;

/// Registers a module's `#[pyclass]`es and free functions. `$m` is whatever
/// `wrap_pyfunction!` wants: `m` for a `&Bound`, `&m` for an owned one.
macro_rules! bind {
    ($m:expr; $(classes [$($c:ty),* $(,)?];)? $(fns [$($f:ident),* $(,)?];)?) => {{
        $($( $m.add_class::<$c>()?; )*)?
        $($( $m.add_function(pyo3::wrap_pyfunction!($f, $m)?)?; )*)?
    }};
}
