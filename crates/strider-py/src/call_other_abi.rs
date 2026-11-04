use pyo3::prelude::*;
use pyo3::types::PyType;

use strider_target::call_other_abi::{CallOtherClass, CallOtherOverride};

/// `Built` resolves at construction, so a name typo raises there.
#[derive(Clone)]
pub(crate) enum AbiImpl {
    Class(CallOtherClass),
    Built(Box<strider_target::BuiltCallOtherAbi>),
}

/// How one Sleigh user-op is lifted: implicit registers, memory effect and
/// whether control returns. Pass to `CfgOptions(call_other_abis={name: abi})`.
#[pyclass(name = "CallOtherAbi", module = "strider.sleigh", frozen)]
#[derive(Clone)]
pub struct PyCallOtherAbi {
    pub(crate) inner: AbiImpl,
    reads: Vec<String>,
    writes: Vec<String>,
    /// `None` for a class.
    pub(crate) source_arch: Option<strider_target::SleighArch>,
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
            source_arch: None,
        }
    }

    pub(crate) fn builtin(preset: strider_target::ArchPreset, name: &str) -> Option<Self> {
        strider_target::call_other_abi::classify(preset, name).map(Self::from_class)
    }

    pub(crate) fn to_override(&self) -> CallOtherOverride {
        match &self.inner {
            AbiImpl::Class(class) => CallOtherOverride::Class(*class),
            AbiImpl::Built(abi) => CallOtherOverride::Built((**abi).clone()),
        }
    }

    fn source_preset(&self) -> Option<strider_target::ArchPreset> {
        self.source_arch.map(|a| a.preset())
    }
}

#[pymethods]
impl PyCallOtherAbi {
    /// The op is dropped: no IR node, any pcode result ignored.
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

    /// Control does not pass the op. Empty footprint; use
    /// `custom(..., no_return=True)` for one.
    #[classmethod]
    fn no_return(_cls: &Bound<'_, PyType>) -> Self {
        Self::from_class(CallOtherClass::NO_RETURN)
    }

    /// An ABI naming implicit registers; a bad name raises `StriderError`.
    ///
    /// Args:
    ///     sleigh: The `SleighArch` to resolve register names against.
    ///     implicit_reads: Registers read beyond the explicit operands; they
    ///         lead the lifted call's arguments.
    ///     implicit_writes: Registers written beyond the explicit result.
    ///     clobbers_memory: `True` for anything touching memory (atomics,
    ///         barriers, port I/O, syscalls).
    ///     no_return: `True` when control does not pass the op.
    ///
    /// Usable only with a `Lifter` sharing `sleigh`'s register table (`arm`,
    /// `arm_thumb` and `arm_be_kernel` share one).
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
        sleigh: crate::arch::PySleighArch,
        implicit_reads: Vec<String>,
        implicit_writes: Vec<String>,
        clobbers_memory: bool,
        no_return: bool,
    ) -> PyResult<Self> {
        let source_arch = sleigh.inner;
        let regs = source_arch.names().regs();
        let resolve = |field: &str, names: &[String]| {
            strider_target::calling_convention::regs_to_vns(regs, names).map_err(|e| {
                crate::errors::into_strider_err(e.context(format!("CallOtherAbi.custom: {field}")))
            })
        };
        let built = strider_target::BuiltCallOtherAbi {
            implicit_reads: resolve("implicit_reads", &implicit_reads)?,
            implicit_writes: resolve("implicit_writes", &implicit_writes)?,
            clobbers_memory,
            no_return,
        };
        Ok(Self {
            inner: AbiImpl::Built(Box::new(built)),
            reads: implicit_reads,
            writes: implicit_writes,
            source_arch: Some(source_arch),
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
            AbiImpl::Class(CallOtherClass::Call(abi)) => abi.clobbers_memory,
            AbiImpl::Class(CallOtherClass::NoOp) => false,
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

    /// Whether this is `noop()`.
    #[getter]
    fn is_noop(&self) -> bool {
        matches!(self.inner, AbiImpl::Class(CallOtherClass::NoOp))
    }

    fn __repr__(&self) -> String {
        if self.is_noop() {
            return "CallOtherAbi.noop()".to_owned();
        }
        // A `custom` keeps its form however empty, as `__eq__` separates it.
        if self.source_arch.is_none() && self.reads.is_empty() && self.writes.is_empty() {
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

    /// Equal footprint, memory effect, no-return flag and `custom` arch
    /// preset; a `custom` never equals a class.
    fn __eq__(&self, other: &Self) -> bool {
        self.source_preset() == other.source_preset()
            && self.is_noop() == other.is_noop()
            && self.reads == other.reads
            && self.writes == other.writes
            && self.clobbers_memory() == other.clobbers_memory()
            && self.is_no_return() == other.is_no_return()
    }

    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        self.source_preset().hash(&mut h);
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
