//! `forall_preset!` stamps out one `#[classmethod]` per preset name for the
//! `SleighArch` / `CallingConvention` wrappers.  Needs PyO3's
//! `multiple-pymethods` feature so a `#[pyclass]` can carry more than one
//! `#[pymethods]` block.

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
                        no_return: false,
                    }
                }
            )*
        }
    };
    // Arch wrapper: stores the inner preset directly.
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
