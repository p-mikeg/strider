//! `forall_preset!` stamps out one `#[classmethod]` per preset name for the
//! `SleighArch` / `CallingConvention` wrappers.  Needs PyO3's
//! `multiple-pymethods` feature so a `#[pyclass]` can carry more than one
//! `#[pymethods]` block.

macro_rules! forall_preset {
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
                        source_arch: None,
                        no_return: false,
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
                        preset_name: stringify!($name),
                    }
                }
            )*
        }
    };
}

pub(crate) use forall_preset;

/// One `#[pyfunction]` per value-op constructor, wrapping its operands in a
/// single `PatRepr`. `$ty` is the module's wrapper (`PyPat` matching,
/// `PyTemplate` building); the `= "name"` arms give the Python name when it
/// differs from the Rust ident (`and_` exposed as `int_and`).
macro_rules! repr_fn {
    ($ty:ident; binary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    ($ty:ident; binary $name:ident = $py:literal, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py)]
        pub fn $name(l: Py<PyAny>, r: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, l, r))
        }
    };
    ($ty:ident; unary $name:ident, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, operand))
        }
    };
    ($ty:ident; unary $name:ident = $py:literal, $repr:ident, $op:expr, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py)]
        pub fn $name(operand: Py<PyAny>) -> $ty {
            <$ty>::from_repr(PatRepr::$repr($op, operand))
        }
    };
}
