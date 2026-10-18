use pyo3::prelude::*;
use pyo3::types::PyType;

use crate::macros::forall_preset;

/// A target architecture description (Sleigh `.sla` + `.pspec` +
/// endianness).  Construct via one of the preset classmethods, e.g.
/// `SleighArch.x86_64()`.
#[pyclass(name = "SleighArch", module = "strider.sleigh", frozen)]
#[derive(Clone)]
pub struct PySleighArch {
    pub(crate) inner: strider_target::SleighArch,
}

#[pymethods]
impl PySleighArch {
    /// The preset name this arch was constructed from (e.g. `"x86_64"`).
    fn name(&self) -> &'static str {
        self.inner.preset().name()
    }

    /// Byte order of this architecture: `"little"` or `"big"`.
    fn endianness(&self) -> &'static str {
        match self.inner.endianness() {
            strider_target::Endianness::Little => "little",
            strider_target::Endianness::Big => "big",
        }
    }

    /// `SleighArch.<preset>()`, the call that produces this arch.
    fn __repr__(&self) -> String {
        format!("SleighArch.{}()", self.name())
    }
}

#[pymethods]
impl PySleighArch {
    /// Look up a register's varnode by Sleigh name, or `None` if this arch's
    /// table has no such name.
    fn reg(&self, name: &str) -> Option<crate::sleigh::PyVn> {
        self.inner
            .names()
            .regs()
            .name_to_vn(name)
            .map(crate::sleigh::PyVn::from_inner)
    }

    /// Reverse of `reg(...)`. `None` for a non-REGISTER space, or a REGISTER
    /// offset/size pair absent from this arch's table; never raises.
    fn reg_name(&self, vn: &crate::sleigh::PyVn) -> Option<&'static str> {
        self.inner.names().regs().vn_to_name(vn.inner)
    }
}

/// What `custom(...)` resolves register names against: a `SleighArch`, or a
/// `Sleigh` built for one.
#[derive(FromPyObject)]
pub(crate) enum ArchArg {
    #[pyo3(annotation = "SleighArch")]
    Arch(PySleighArch),
    #[pyo3(annotation = "Sleigh")]
    Sleigh(Py<crate::sleigh::PySleigh>),
}

impl ArchArg {
    pub(crate) fn arch(&self, py: Python<'_>) -> strider_target::SleighArch {
        match self {
            ArchArg::Arch(arch) => arch.inner,
            ArchArg::Sleigh(sleigh) => sleigh.borrow(py).arch,
        }
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

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleighArch>()
}
