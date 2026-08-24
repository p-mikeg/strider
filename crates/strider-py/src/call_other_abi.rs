use pyo3::prelude::*;
use pyo3::types::PyType;

use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass, CallOtherOverride};

/// A preset carries the static register names of a table row, resolved
/// against a Sleigh at lift time.  `Custom` is resolved eagerly instead, so a
/// name typo surfaces at construction.
#[derive(Clone)]
pub(crate) enum AbiImpl {
    Class(CallOtherClass),
    Built(Box<strider_target::BuiltCallOtherAbi>),
}

/// How one Sleigh user-op is lifted: the implicit register footprint, the
/// memory effect and whether control returns, none of which Sleigh's pcode
/// encodes.
///
/// Construct via `CallOtherAbi.pure()` / `.noop()` / `.mem_clobber()` /
/// `.no_return()` for the footprint-free classes, or `CallOtherAbi.custom(...)`
/// to name registers.  Pass one to
/// `CfgOptions(call_other_abis={name: abi})`.
#[pyclass(name = "CallOtherAbi", module = "strider.sleigh", frozen)]
#[derive(Clone)]
pub struct PyCallOtherAbi {
    pub(crate) inner: AbiImpl,
    /// The register names as the caller (or the built-in table) spells them.
    reads: Vec<String>,
    writes: Vec<String>,
}

impl PyCallOtherAbi {
    fn from_class(class: CallOtherClass) -> Self {
        let names = |ns: &'static [&'static str]| ns.iter().map(|n| (*n).to_owned()).collect();
        let (reads, writes) = match class {
            CallOtherClass::NoOp => (Vec::new(), Vec::new()),
            CallOtherClass::Call(abi) => (names(abi.implicit_reads), names(abi.implicit_writes)),
        };
        Self {
            inner: AbiImpl::Class(class),
            reads,
            writes,
        }
    }

    /// The classification `preset` gives `name`, or `None` for a name no
    /// built-in table answers for.
    pub(crate) fn builtin(preset: strider_target::ArchPreset, name: &str) -> Option<Self> {
        strider_target::call_other_abi::classify(preset, name).map(Self::from_class)
    }

    pub(crate) fn to_override(&self) -> CallOtherOverride {
        match &self.inner {
            AbiImpl::Class(class) => CallOtherOverride::Class(*class),
            AbiImpl::Built(abi) => CallOtherOverride::Built((**abi).clone()),
        }
    }

    fn abi(&self) -> Option<&CallOtherAbi> {
        match &self.inner {
            AbiImpl::Class(CallOtherClass::Call(abi)) => Some(abi),
            _ => None,
        }
    }
}

#[pymethods]
impl PyCallOtherAbi {
    /// The op is dropped: no IR node, control and memory unchanged, and any
    /// pcode result ignored.
    #[classmethod]
    fn noop(_cls: &Bound<'_, PyType>) -> Self {
        Self::from_class(CallOtherClass::NoOp)
    }

    /// Pure compute: a pcode result, no implicit registers, no memory effect.
    #[classmethod]
    fn pure(_cls: &Bound<'_, PyType>) -> Self {
        Self::from_class(CallOtherClass::PURE)
    }

    /// No implicit registers, memory conservatively clobbered.
    #[classmethod]
    fn mem_clobber(_cls: &Bound<'_, PyType>) -> Self {
        Self::from_class(CallOtherClass::MEM_CLOBBER)
    }

    /// Control does not pass the op, which ends its region.  Empty footprint;
    /// `custom(..., no_return=True)` carries one.
    #[classmethod]
    fn no_return(_cls: &Bound<'_, PyType>) -> Self {
        Self::from_class(CallOtherClass::NO_RETURN)
    }

    /// Build an ABI naming implicit registers, for an op strider reads
    /// wrongly or an OS convention it cannot know.  The names are resolved
    /// here, so a bad one raises `StriderError` at construction rather than at
    /// first use.
    ///
    /// Args:
    ///     sleigh: The `Sleigh` instance to resolve register names against.
    ///     implicit_reads: Registers read beyond the pcode-explicit operands.
    ///         They lead the lifted call's argument list.
    ///     implicit_writes: Registers written or clobbered beyond the
    ///         pcode-explicit result.
    ///     clobbers_memory: `True` for anything touching memory (atomics,
    ///         barriers, port I/O, syscalls); `False` for pure compute.
    ///     no_return: `True` when control does not pass the op.
    #[classmethod]
    #[pyo3(signature = (
        sleigh,
        implicit_reads = Vec::new(),
        implicit_writes = Vec::new(),
        clobbers_memory = false,
        no_return = false,
    ))]
    fn custom(
        _cls: &Bound<'_, PyType>,
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        implicit_reads: Vec<String>,
        implicit_writes: Vec<String>,
        clobbers_memory: bool,
        no_return: bool,
    ) -> PyResult<Self> {
        let regs = sleigh.borrow(py).regs.clone();
        let resolve = |names: &[String]| -> PyResult<Vec<rsleigh::Vn>> {
            names
                .iter()
                .map(|name| {
                    regs.name_to_vn(name).ok_or_else(|| {
                        crate::errors::into_strider_err(anyhow::anyhow!(
                            "CallOtherAbi.custom: unknown register name {name:?}"
                        ))
                    })
                })
                .collect()
        };
        let built = strider_target::BuiltCallOtherAbi {
            implicit_reads: resolve(&implicit_reads)?,
            implicit_writes: resolve(&implicit_writes)?,
            clobbers_memory,
            no_return,
        };
        Ok(Self {
            inner: AbiImpl::Built(Box::new(built)),
            reads: implicit_reads,
            writes: implicit_writes,
        })
    }

    /// Register names read beyond the pcode-explicit operands.
    #[getter]
    fn implicit_reads(&self) -> Vec<String> {
        self.reads.clone()
    }

    /// Register names written beyond the pcode-explicit result.
    #[getter]
    fn implicit_writes(&self) -> Vec<String> {
        self.writes.clone()
    }

    /// Whether the op advances the IR's memory edge.
    #[getter]
    fn clobbers_memory(&self) -> bool {
        match &self.inner {
            AbiImpl::Class(_) => self.abi().is_some_and(|a| a.clobbers_memory),
            AbiImpl::Built(abi) => abi.clobbers_memory,
        }
    }

    /// Whether control does NOT pass the op.
    #[getter]
    fn is_no_return(&self) -> bool {
        match &self.inner {
            AbiImpl::Class(class) => class.is_no_return(),
            AbiImpl::Built(abi) => abi.no_return,
        }
    }

    /// Whether the op lifts to nothing at all (the `noop()` class).
    #[getter]
    fn is_noop(&self) -> bool {
        matches!(self.inner, AbiImpl::Class(CallOtherClass::NoOp))
    }

    /// The constructor call that produces this ABI, less the `sleigh` a
    /// footprint was resolved against.
    fn __repr__(&self) -> String {
        if self.is_noop() {
            return "CallOtherAbi.noop()".to_owned();
        }
        if self.reads.is_empty() && self.writes.is_empty() {
            if self.is_no_return() {
                return "CallOtherAbi.no_return()".to_owned();
            }
            if self.clobbers_memory() {
                return "CallOtherAbi.mem_clobber()".to_owned();
            }
            return "CallOtherAbi.pure()".to_owned();
        }
        format!(
            "CallOtherAbi.custom(implicit_reads={:?}, implicit_writes={:?}, \
             clobbers_memory={}, no_return={})",
            self.reads,
            self.writes,
            crate::options::py_bool(self.clobbers_memory()),
            crate::options::py_bool(self.is_no_return()),
        )
    }

    /// Equality on the footprint, memory effect and no-return flag: a preset
    /// and a `custom` spelling the same thing compare equal.
    fn __eq__(&self, other: &Self) -> bool {
        self.is_noop() == other.is_noop()
            && self.reads == other.reads
            && self.writes == other.writes
            && self.clobbers_memory() == other.clobbers_memory()
            && self.is_no_return() == other.is_no_return()
    }

    /// Hash consistent with `__eq__`.
    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        self.is_noop().hash(&mut h);
        self.reads.hash(&mut h);
        self.writes.hash(&mut h);
        self.clobbers_memory().hash(&mut h);
        self.is_no_return().hash(&mut h);
        h.finish()
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallOtherAbi>()
}
