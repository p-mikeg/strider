use std::collections::HashMap;

use pyo3::prelude::*;

use crate::call_other_abi::PyCallOtherAbi;
use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::strider_cls::reject_zero_max_size;

/// One caller-supplied indirect-branch answer: concrete targets, or a return.
#[derive(Clone, Debug)]
pub enum KnownTarget {
    Targets(Vec<u64>),
    Return,
}

impl<'py> FromPyObject<'py> for KnownTarget {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(s) = ob.extract::<String>() {
            return match s.as_str() {
                "return" => Ok(KnownTarget::Return),
                other => Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "known_targets value {other:?} is not understood; use a list \
                     of target addresses or the string \"return\""
                ))),
            };
        }
        Ok(KnownTarget::Targets(ob.extract::<Vec<u64>>()?))
    }
}

impl IntoPy<PyObject> for KnownTarget {
    fn into_py(self, py: Python<'_>) -> PyObject {
        match self {
            KnownTarget::Targets(v) => v.into_py(py),
            KnownTarget::Return => "return".into_py(py),
        }
    }
}

/// One `call_other_abis` value. A string raises a message naming the
/// `CallOtherAbi` constructors rather than a bare type error.
pub struct CallOtherAbiArg(pub PyCallOtherAbi);

impl<'py> FromPyObject<'py> for CallOtherAbiArg {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(name) = ob.extract::<String>() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "call_other_abis value {name:?} is a string; values are \
                 strider.sleigh.CallOtherAbi objects: CallOtherAbi.noop(), \
                 .pure(), .mem_clobber(), .no_return(), or \
                 CallOtherAbi.custom(sleigh, ...) for an implicit register \
                 footprint"
            )));
        }
        Ok(Self(ob.extract::<PyCallOtherAbi>()?))
    }
}

/// CFG-shaping knobs, keyword-only.
#[pyclass(name = "CfgOptions", module = "strider.cfg")]
#[derive(Clone, Default)]
pub struct PyCfgOptions {
    /// When set, an unconditional branch targeting at or past
    /// `entry + function_max_size` is treated as a tail call, bounding the
    /// lift. Must be `> 0`.
    #[pyo3(get)]
    pub function_max_size: Option<u64>,
    /// When `False` (the default), an unconditional branch targeting an
    /// address *below* the function start is treated as a tail call.
    /// When `True`, such branches are followed normally.
    #[pyo3(get)]
    pub allow_code_before_start_addr: bool,
    /// Indirect-branch answers supplied by the caller, as
    /// `{dispatch_address: [target_address, ...] | "return"}`.
    ///
    /// A listed site seats these answers directly instead of deferring to the
    /// classifier. `"return"` seats the site as a function return, which the
    /// resolver itself proves only for a bare link register. An empty list
    /// seats nothing, leaving the site an unresolved indirect branch; the
    /// classifier still runs unless `resolve_indirect_branches=False`.
    #[pyo3(get)]
    pub known_targets: std::collections::HashMap<u64, KnownTarget>,
    /// Classifications for Sleigh user-op names, as
    /// `{name: strider.sleigh.CallOtherAbi}`, winning over the built-in table.
    ///
    /// `Lifter.user_op_names()` lists the names a binary can contain, and
    /// `Lifter.call_other_abi(name)` reads back what strider already makes of
    /// one.
    #[pyo3(get)]
    pub call_other_abis: std::collections::HashMap<String, PyCallOtherAbi>,
}

#[pymethods]
impl PyCfgOptions {
    /// Raises `ValueError` for `function_max_size=0`; omit the argument
    /// for an unbounded lift.
    #[new]
    #[pyo3(signature = (*, function_max_size = None, allow_code_before_start_addr = false,
                        known_targets = std::collections::HashMap::new(),
                        call_other_abis = std::collections::HashMap::new()))]
    fn new(
        function_max_size: Option<u64>,
        allow_code_before_start_addr: bool,
        known_targets: std::collections::HashMap<u64, KnownTarget>,
        call_other_abis: std::collections::HashMap<String, CallOtherAbiArg>,
    ) -> PyResult<Self> {
        reject_zero_max_size(function_max_size)?;
        Ok(Self {
            function_max_size,
            allow_code_before_start_addr,
            known_targets,
            call_other_abis: call_other_abis
                .into_iter()
                .map(|(name, abi)| (name, abi.0))
                .collect(),
        })
    }

    fn __repr__(&self) -> String {
        // The two maps render as counts: a seeded table is what decides the
        // successor set, so a `repr` that omits it describes a different
        // analysis than the one that ran.
        format!(
            "CfgOptions(function_max_size={:?}, allow_code_before_start_addr={}, \
             known_targets={{{} entries}}, call_other_abis={{{} entries}})",
            self.function_max_size,
            py_bool(self.allow_code_before_start_addr),
            self.known_targets.len(),
            self.call_other_abis.len(),
        )
    }
}

/// Render a bool the Python way, so a `repr` reads as an eval-able constructor
/// call (`True`/`False`, not Rust's `true`/`false`).
pub(crate) fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// How one field renders inside a `repr`: `bool` as Python's `True`/`False`,
/// `debug` through `Debug`.
macro_rules! py_option_repr {
    (bool, $v:expr) => {
        py_bool($v)
    };
    (debug, $v:expr) => {
        format!("{:?}", $v)
    };
}

/// A frozen options class from one field table: the `#[pyclass]` struct, its
/// `Default`, the keyword-only `#[new]` whose signature defaults ARE those
/// `Default` values, and a `repr` that reads back as that constructor call.
/// A row is `name: Type = default, render`.
macro_rules! py_options {
    (
        $(#[$cls:meta])*
        $pyname:literal, $module:literal => $ty:ident;
        $( $(#[$fattr:meta])* $name:ident : $fty:ty = $default:expr, $render:ident; )*
    ) => {
        $(#[$cls])*
        #[pyclass(name = $pyname, module = $module)]
        #[derive(Clone)]
        pub struct $ty {
            $(
                $(#[$fattr])*
                #[pyo3(get)]
                pub $name: $fty,
            )*
        }

        impl Default for $ty {
            fn default() -> Self {
                Self { $($name: $default,)* }
            }
        }

        #[pymethods]
        impl $ty {
            #[new]
            #[pyo3(signature = (*, $($name = $default),*))]
            #[allow(clippy::fn_params_excessive_bools)]
            fn new($($name: $fty),*) -> Self {
                Self { $($name,)* }
            }

            fn __repr__(&self) -> String {
                let fields = [$(
                    format!(
                        concat!(stringify!($name), "={}"),
                        py_option_repr!($render, self.$name),
                    ),
                )*];
                format!(concat!($pyname, "({})"), fields.join(", "))
            }
        }
    };
}

py_options! {
    /// Claims about the code being analysed, keyword-only. None is checked, and
    /// each one's risky value is the positive one, so any of them can make the
    /// answer wrong on valid input. Clearing all six leaves only what the IR
    /// structurally proves.
    ///
    /// Two default `True`: `stack_global_disjoint` and
    /// `assume_incoming_args_survive_calls`, both of which every compiler this
    /// analyses honours and without which the alias oracle answers may-alias
    /// almost everywhere.
    "AssumptionOptions", "strider.lift" => PyAssumptionOptions;

    /// The stack and global/constant memory (`.data`, `.rodata`, `.bss`,
    /// MMIO) never overlap at runtime, so a memory walk steps through a
    /// constant-address store when looking back from a stack load and
    /// vice-versa (default `True`). Wrong only where a constant address
    /// coincidentally equals `sp + K`.
    stack_global_disjoint: bool = true, bool;

    /// A call on an incoming stack-argument slot's memory chain leaves the
    /// slot alone (default `True`). Reaches incoming-argument detection only,
    /// where it holds for any conforming callee: those slots are the caller's
    /// memory, above the entry SP.
    assume_incoming_args_survive_calls: bool = true, bool;

    /// A store rooted at a different SP base than the entry SP (an
    /// alignment-masked frame local, say) is disjoint from the probed
    /// location.
    distinct_sp_bases_disjoint: bool = false, bool;

    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// let a callee write those slots; this asserts compiler output does not.
    callee_preserves_stack_args: bool = false, bool;

    /// Callee addresses of pure `noalias` heap allocators (`malloc`/`calloc`-like:
    /// a size in, a fresh non-overlapping pointer out, no pointer arguments).
    /// Distinct allocations are treated as disjoint, and a load steps through
    /// such a call.
    noalias_allocators: Vec<u64> = Vec::new(), debug;

    /// When the frame is provably private (no stack address escapes to a
    /// callee), forward a spill load across a call and past an opaque store.
    /// The proof is sound; the claim is that no callee returns a struct by
    /// value, an sret hidden pointer being a frame-address escape the analysis
    /// may not see.
    escape_analysis: bool = false, bool;
}

/// Lift, optimize, and CFG knobs for one `analyze` call.
///
/// Raises `ValueError` for a nested `function_max_size=0`.
#[pyclass(name = "LifterOptions", module = "strider.lift")]
pub struct PyLifterOptions {
    /// Nested CFG-shape knobs.
    #[pyo3(get)]
    pub cfg: Py<PyCfgOptions>,
    /// Nested claims about the analysed code, each one unchecked.
    #[pyo3(get)]
    pub assumptions: Py<PyAssumptionOptions>,
    /// Drop unreachable IR nodes after analysis (default `True`).
    #[pyo3(get)]
    pub compact: bool,
    /// Per-target-address calling-convention overrides (preset or custom
    /// CCs accepted); `None`/omitted means no overrides.
    #[pyo3(get)]
    pub per_address_ccs: Option<HashMap<u64, PyCallingConvention>>,
    /// Run the indirect-branch classifier (default `True`). `False` leaves
    /// every site an `IndirectBranch` placeholder, so you can supply your own
    /// answers via `CfgOptions(known_targets=...)`, or inspect the raw
    /// dispatch shape when a resolution looks wrong.
    #[pyo3(get)]
    pub resolve_indirect_branches: bool,
    /// When set, `analyze` runs this pipeline instead of the built-in
    /// default, for this call only.
    #[pyo3(get)]
    pub pipeline: Option<Py<PyOptimizerPipeline>>,
}

impl PyLifterOptions {
    /// All-defaults fallback for `opts=None`.
    pub(crate) fn new_default(py: Python<'_>) -> PyResult<Self> {
        Self::new(py, None, None, true, None, true, None)
    }
}

#[pymethods]
impl PyLifterOptions {
    /// `cfg`, `assumptions` and `pipeline` are `Py` handles, invisible to the
    /// collector without this.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.cfg)?;
        visit.call(&self.assumptions)?;
        if let Some(p) = self.pipeline.as_ref() {
            visit.call(p)?;
        }
        Ok(())
    }

    #[new]
    #[pyo3(signature = (
        *,
        cfg = None,
        assumptions = None,
        compact = true,
        per_address_ccs = None,
        resolve_indirect_branches = true,
        pipeline = None,
    ))]
    fn new(
        py: Python<'_>,
        cfg: Option<Py<PyCfgOptions>>,
        assumptions: Option<Py<PyAssumptionOptions>>,
        compact: bool,
        per_address_ccs: Option<HashMap<u64, PyCallingConvention>>,
        resolve_indirect_branches: bool,
        pipeline: Option<Py<PyOptimizerPipeline>>,
    ) -> PyResult<Self> {
        // Fresh nested defaults, never a shared instance.
        let cfg = match cfg {
            Some(c) => c,
            None => Py::new(py, PyCfgOptions::default())?,
        };
        let assumptions = match assumptions {
            Some(a) => a,
            None => Py::new(py, PyAssumptionOptions::default())?,
        };
        Ok(Self {
            cfg,
            assumptions,
            compact,
            per_address_ccs,
            resolve_indirect_branches,
            pipeline,
        })
    }

    /// These options with `cfg` replaced and every other field carried over.
    /// The supported way to override the nested `CfgOptions`, since the fields
    /// are read-only.
    fn with_cfg(&self, py: Python<'_>, cfg: Py<PyCfgOptions>) -> Self {
        Self {
            cfg,
            assumptions: self.assumptions.clone_ref(py),
            compact: self.compact,
            per_address_ccs: self.per_address_ccs.clone(),
            resolve_indirect_branches: self.resolve_indirect_branches,
            pipeline: self.pipeline.as_ref().map(|p| p.clone_ref(py)),
        }
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let cfg_repr = self.cfg.borrow(py).__repr__();
        let assumptions_repr = self.assumptions.borrow(py).__repr__();
        Ok(format!(
            "LifterOptions(cfg={}, compact={}, per_address_ccs={}, \
             resolve_indirect_branches={}, assumptions={}, pipeline={})",
            cfg_repr,
            py_bool(self.compact),
            if self.per_address_ccs.is_some() {
                "<...>"
            } else {
                "None"
            },
            py_bool(self.resolve_indirect_branches),
            assumptions_repr,
            if self.pipeline.is_some() {
                "<...>"
            } else {
                "None"
            },
        ))
    }
}

pub fn register_cfg(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfgOptions>()?;
    Ok(())
}

pub fn register_lift(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifterOptions>()?;
    m.add_class::<PyAssumptionOptions>()?;
    Ok(())
}
