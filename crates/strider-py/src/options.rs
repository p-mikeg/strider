use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::bigint::{Addr, IntSeq};
use crate::call_other_abi::PyCallOtherAbi;
use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::strider_cls::function_bound;
use strider_orchestrator::opt::AssumptionOptions;

/// One `known_targets` value.
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
        let targets: IntSeq<Addr> = ob.extract()?;
        Ok(KnownTarget::Targets(
            targets.0.into_iter().map(|a| a.0).collect(),
        ))
    }
}

impl IntoPy<PyObject> for KnownTarget {
    fn into_py(self, py: Python<'_>) -> PyObject {
        match self {
            // A tuple, so the read-only view is read-only all the way down.
            KnownTarget::Targets(v) => pyo3::types::PyTuple::new_bound(py, v).into_py(py),
            KnownTarget::Return => "return".into_py(py),
        }
    }
}

/// One `call_other_abis` value; a string raises naming the `CallOtherAbi`
/// constructors.
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

pub struct AddrSetArg(pub rustc_hash::FxHashSet<u64>);

impl<'py> FromPyObject<'py> for AddrSetArg {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        crate::bigint::refuse_bytes(ob)?;
        ob.iter()?
            .map(|item| Ok(item?.extract::<Addr>()?.0))
            .collect::<PyResult<_>>()
            .map(Self)
    }
}

/// Any mapping, so a getter's read-only proxy round-trips. Items are
/// collected (`PyDict_Next`, which runs no Python and a `dict` subclass cannot
/// narrow) before any is extracted: an `extract` can run Python
/// (`__index__`), and PyO3 panics if the dict changes size mid-iteration.
pub struct MapArg<T>(pub T);

impl<'py, K, V, S> FromPyObject<'py> for MapArg<HashMap<K, V, S>>
where
    K: FromPyObject<'py> + Eq + std::hash::Hash,
    V: FromPyObject<'py>,
    S: std::hash::BuildHasher + Default,
{
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        let items: Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)> = match ob.downcast::<PyDict>() {
            Ok(d) => d.iter().collect(),
            Err(_) => ob
                .py()
                .get_type_bound::<PyDict>()
                .call1((ob,))?
                .downcast_into::<PyDict>()?
                .iter()
                .collect(),
        };
        let mut out = HashMap::with_capacity_and_hasher(items.len(), S::default());
        for (k, v) in items {
            out.insert(K::extract_bound(&k)?, V::extract_bound(&v)?);
        }
        Ok(Self(out))
    }
}

/// A `MappingProxyType` over `source`, built once. The Rust map stays the
/// authority.
fn mapping_view<'py, T: IntoPy<PyObject> + Clone>(
    view: &OnceLock<PyObject>,
    py: Python<'py>,
    source: &T,
) -> PyResult<Bound<'py, PyAny>> {
    memo_obj(view, py, || {
        let proxy = py.import_bound("types")?.getattr("MappingProxyType")?;
        proxy.call1((source.clone().into_py(py),))
    })
}

fn memo_obj<'py>(
    view: &OnceLock<PyObject>,
    py: Python<'py>,
    build: impl FnOnce() -> PyResult<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    if let Some(obj) = view.get() {
        return Ok(obj.bind(py).clone());
    }
    let obj = build()?.unbind();
    Ok(view.get_or_init(|| obj).bind(py).clone())
}

/// CFG-shaping knobs, keyword-only.
#[pyclass(name = "CfgOptions", module = "strider.cfg", frozen)]
#[derive(Default)]
pub struct PyCfgOptions {
    pub bound: strider_cfg::FunctionBound,
    /// `seated` is the same table in `strider_cfg`'s form.
    pub known_targets: HashMap<u64, KnownTarget>,
    pub call_other_abis: HashMap<String, PyCallOtherAbi>,
    pub(crate) no_return_targets: rustc_hash::FxHashSet<u64>,
    pub data_ranges: strider_cfg::DataRanges,
    pub fragment_ranges: strider_cfg::DataRanges,
    known_targets_view: OnceLock<PyObject>,
    call_other_abis_view: OnceLock<PyObject>,
    no_return_targets_view: OnceLock<PyObject>,
    pub(crate) seated: SeatedTargets,
    /// Rebuilt per arch.
    overrides: Mutex<OverridesMemo>,
}

/// `slot`'s table if built for `target`'s preset, else `build`'s, kept in
/// `slot`.
fn memo_per_arch<T>(
    slot: &mut Option<(strider_target::SleighArch, Arc<T>)>,
    target: strider_target::SleighArch,
    build: impl FnOnce() -> PyResult<T>,
) -> PyResult<Arc<T>> {
    if let Some((arch, built)) = slot
        && arch.preset() == target.preset()
    {
        return Ok(Arc::clone(built));
    }
    let built = Arc::new(build()?);
    *slot = Some((target, Arc::clone(&built)));
    Ok(built)
}

#[derive(Default)]
struct OverridesMemo {
    built: Option<(
        strider_target::SleighArch,
        Arc<strider_target::call_other_abi::CallOtherOverrides>,
    )>,
    #[cfg(feature = "test-hooks")]
    builds: usize,
}

pub(crate) type SeatedTargets =
    rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>;

impl PyCfgOptions {
    /// `call_other_abis` checked and built for `target`, memoized per arch.
    pub(crate) fn call_other_overrides(
        &self,
        target: strider_target::SleighArch,
    ) -> PyResult<Arc<strider_target::call_other_abi::CallOtherOverrides>> {
        let mut memo = self
            .overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let memo = &mut *memo;
        memo_per_arch(&mut memo.built, target, || {
            let overrides = crate::strider_cls::overrides_from(&self.call_other_abis, target)?;
            #[cfg(feature = "test-hooks")]
            {
                memo.builds += 1;
            }
            Ok(overrides)
        })
    }
}

#[pymethods]
impl PyCfgOptions {
    /// Raises `ValueError` for `function_max_size=0`, or a size with
    /// `allow_code_before_start_addr=True`.
    #[new]
    #[pyo3(signature = (*, function_max_size = None, allow_code_before_start_addr = false,
                        known_targets = MapArg(HashMap::new()),
                        call_other_abis = MapArg(HashMap::new()),
                        data_ranges = Vec::new(),
                        fragment_ranges = Vec::new(),
                        no_return_targets = AddrSetArg(rustc_hash::FxHashSet::default())))]
    fn new(
        function_max_size: Option<crate::bigint::Unsigned<u64>>,
        allow_code_before_start_addr: bool,
        known_targets: MapArg<HashMap<Addr, KnownTarget>>,
        call_other_abis: MapArg<HashMap<String, CallOtherAbiArg>>,
        data_ranges: Vec<(Addr, Addr)>,
        fragment_ranges: Vec<(Addr, Addr)>,
        no_return_targets: AddrSetArg,
    ) -> PyResult<Self> {
        let bound = function_bound(function_max_size.map(|s| s.0), allow_code_before_start_addr)?;
        let known_targets: HashMap<u64, KnownTarget> =
            known_targets.0.into_iter().map(|(a, t)| (a.0, t)).collect();
        let ranges = |r: Vec<(Addr, Addr)>| -> Vec<(u64, u64)> {
            r.into_iter().map(|(s, e)| (s.0, e.0)).collect()
        };
        let seated = crate::strider_cls::seat_known_targets(&known_targets);
        Ok(Self {
            bound,
            known_targets,
            call_other_abis: call_other_abis
                .0
                .into_iter()
                .map(|(name, abi)| (name, abi.0))
                .collect(),
            data_ranges: strider_cfg::DataRanges::try_new("data_ranges", ranges(data_ranges))
                .map_err(pyo3::exceptions::PyValueError::new_err)?,
            fragment_ranges: strider_cfg::DataRanges::try_new(
                "fragment_ranges",
                ranges(fragment_ranges),
            )
            .map_err(pyo3::exceptions::PyValueError::new_err)?,
            no_return_targets: no_return_targets.0,
            seated,
            ..Self::default()
        })
    }

    /// When set, an unconditional branch at or past `entry + function_max_size`
    /// is a tail call; a fall-through past it is `"past_function_end"`.
    #[getter]
    fn function_max_size(&self) -> Option<u64> {
        match self.bound {
            strider_cfg::FunctionBound::Size(size) => Some(size.get()),
            strider_cfg::FunctionBound::Open { .. } => None,
        }
    }

    /// `False` (default): an unconditional branch below the function start is
    /// a tail call. `True` follows it; always `False` beside a size.
    #[getter]
    fn allow_code_before_start_addr(&self) -> bool {
        matches!(
            self.bound,
            strider_cfg::FunctionBound::Open {
                code_before_start: true
            }
        )
    }

    /// Sorted, merged `(start, end)` ranges marked as data. A branch or
    /// fall-through into one is reported, never decoded.
    #[getter]
    fn data_ranges(&self) -> Vec<(u64, u64)> {
        self.data_ranges
            .ranges()
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    /// Sorted, merged `(start, end)` ranges outside the function's extent that
    /// are still its code (GCC `<name>.cold`). A branch into one is followed;
    /// a fall-through past one's end is `"past_function_end"`.
    #[getter]
    fn fragment_ranges(&self) -> Vec<(u64, u64)> {
        self.fragment_ranges
            .ranges()
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    /// Read-only `{dispatch_address: (target, ...) | "return"}`.
    #[getter]
    fn known_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        mapping_view(&self.known_targets_view, py, &self.known_targets)
    }

    /// Read-only `{user_op_name: CallOtherAbi}`.
    #[getter]
    fn call_other_abis<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        mapping_view(&self.call_other_abis_view, py, &self.call_other_abis)
    }

    /// Direct-call targets that never return, a `frozenset`.
    #[getter]
    fn no_return_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        memo_obj(&self.no_return_targets_view, py, || {
            Ok(pyo3::types::PyFrozenSet::new_bound(py, &self.no_return_targets)?.into_any())
        })
    }

    /// How many times `call_other_abis` was resolved.
    #[cfg(feature = "test-hooks")]
    fn _call_other_override_builds(&self) -> usize {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .builds
    }

    fn __repr__(&self) -> String {
        format!(
            "CfgOptions(function_max_size={}, allow_code_before_start_addr={}, \
             known_targets={{{} entries}}, call_other_abis={{{} entries}}, \
             data_ranges=[{} ranges], fragment_ranges=[{} ranges], \
             no_return_targets={{{} entries}})",
            self.function_max_size()
                .map_or_else(|| "None".to_owned(), |size| size.to_string()),
            py_bool(self.allow_code_before_start_addr()),
            self.known_targets.len(),
            self.call_other_abis.len(),
            self.data_ranges.ranges().len(),
            self.fragment_ranges.ranges().len(),
            self.no_return_targets.len(),
        )
    }
}

pub(crate) fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Unchecked claims about the analysed code; a wrong claim makes the answer
/// wrong.
#[pyclass(eq, name = "AssumptionOptions", module = "strider.lift", frozen)]
#[derive(Clone, Default, PartialEq)]
pub struct PyAssumptionOptions {
    pub(crate) inner: AssumptionOptions,
}

trait PyField<P> {
    fn to_py(&self) -> P;
    fn from_py(value: P) -> Self;
    fn py_repr(&self) -> String;
}

impl PyField<bool> for bool {
    fn to_py(&self) -> bool {
        *self
    }

    fn from_py(value: bool) -> Self {
        value
    }

    fn py_repr(&self) -> String {
        py_bool(*self).to_owned()
    }
}

/// Sorted, so a `repr` reads back.
impl PyField<IntSeq<Addr>> for rustc_hash::FxHashSet<u64> {
    fn to_py(&self) -> IntSeq<Addr> {
        let mut v: Vec<u64> = self.iter().copied().collect();
        v.sort_unstable();
        IntSeq(v.into_iter().map(crate::bigint::Unsigned).collect())
    }

    fn from_py(value: IntSeq<Addr>) -> Self {
        value.0.into_iter().map(|a| a.0).collect()
    }

    fn py_repr(&self) -> String {
        let v: Vec<u64> = PyField::<IntSeq<Addr>>::to_py(self)
            .0
            .iter()
            .map(|a| a.0)
            .collect();
        format!("{v:?}")
    }
}

/// The getters, constructor, `none` and `__repr__`, from one row per field.
macro_rules! assumption_fields {
    ($($(#[$doc:meta])* $name:ident: $py:ty),* $(,)?) => {
        impl PyAssumptionOptions {
            #[allow(clippy::too_many_arguments)]
            fn with_fields(base: AssumptionOptions, $($name: Option<$py>),*) -> Self {
                Self {
                    inner: AssumptionOptions {
                        $($name: $name.map_or(base.$name, PyField::from_py)),*
                    },
                }
            }
        }

        #[pymethods]
        impl PyAssumptionOptions {
            #[new]
            #[allow(clippy::too_many_arguments)]
            #[pyo3(signature = (*, $($name = None),*))]
            fn new($($name: Option<$py>),*) -> Self {
                Self::with_fields(AssumptionOptions::default(), $($name),*)
            }

            /// Every claim cleared, then the ones named set; with no argument,
            /// the sound floor.
            #[staticmethod]
            #[allow(clippy::too_many_arguments)]
            #[pyo3(signature = (*, $($name = None),*))]
            fn none($($name: Option<$py>),*) -> Self {
                Self::with_fields(AssumptionOptions::none(), $($name),*)
            }

            $(
                $(#[$doc])*
                #[getter]
                fn $name(&self) -> $py {
                    PyField::<$py>::to_py(&self.inner.$name)
                }
            )*

            fn __repr__(&self) -> String {
                let fields = [$(
                    format!(
                        "{}={}",
                        stringify!($name),
                        PyField::<$py>::py_repr(&self.inner.$name)
                    )
                ),*];
                format!("AssumptionOptions({})", fields.join(", "))
            }
        }
    };
}

assumption_fields! {
    /// The stack never overlaps constant-address memory (`.data`, `.rodata`,
    /// `.bss`, MMIO). Default `True`.
    stack_global_disjoint: bool,

    /// A call leaves the incoming stack-argument slots alone unless handed a
    /// pointer into them. Default `True`; read by incoming-argument detection.
    assume_incoming_args_survive_calls: bool,

    /// A store rooted at an SP base other than the entry SP (e.g. `sp & -16`)
    /// misses the probed slot. Default `False`; read by incoming-argument
    /// detection.
    distinct_sp_bases_disjoint: bool,

    /// A callee leaves the outgoing-argument slots the caller wrote alone.
    /// Inert without `escape_analysis` or `noalias_allocators`. Default `False`.
    callee_preserves_stack_args: bool,

    /// Callee addresses of pure `malloc`-like allocators, sorted. Each result
    /// overlaps no other allocation, stack slot or constant address, so an arena
    /// allocator does not qualify. Default empty.
    noalias_allocators: IntSeq<Addr>,

    /// When no stack address escapes to a callee, forward a spill load across
    /// a call (and, under `stack_global_disjoint`, past an opaque store).
    /// Misses a callee reaching the frame through a preserved register, or
    /// writing an argument slot above an alignment hole. Default `False`.
    escape_analysis: bool,

    /// Untracked stores, callees and user-ops leave the saved return address
    /// and entry SP slots alone. Default `True`; off, such a return is unverified.
    return_slots_survive_untracked_writes: bool,

    /// An ARM `msr cpsr_c, rN` writes the mode already in force; the
    /// immediate form and `cps` still switch. Default `True`; a lift-time claim.
    msr_register_keeps_mode: bool,

    /// Float conversions reading the dynamic rounding mode round to nearest
    /// even; off, each selects among the four modes. Wrong after `fesetround`.
    /// Default `True`; a lift-time claim.
    fp_rounding_mode_is_nearest: bool,
}

#[pymethods]
impl PyAssumptionOptions {
    /// Every claim either holds, every allocator either lists.
    fn union(&self, other: &Self) -> Self {
        let mut inner = self.inner.clone();
        inner.union_with(&other.inner);
        Self { inner }
    }

    /// `a | b` is `a.union(b)`.
    fn __or__(&self, other: &Self) -> Self {
        self.union(other)
    }
}

pub struct PerAddressCcs {
    pub(crate) py: HashMap<u64, PyCallingConvention>,
    built: Mutex<Option<BuiltPerAddressCcs>>,
}

type BuiltPerAddressCcs = (
    strider_target::SleighArch,
    Arc<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
);

/// Lift, optimize, and CFG knobs for one `analyze` call.
#[pyclass(name = "LifterOptions", module = "strider.lift", frozen)]
pub struct PyLifterOptions {
    /// The `CfgOptions`.
    #[pyo3(get)]
    pub cfg: Py<PyCfgOptions>,
    /// The `AssumptionOptions`.
    #[pyo3(get)]
    pub assumptions: Py<PyAssumptionOptions>,
    /// Drop unreachable IR nodes after analysis (default `True`).
    #[pyo3(get)]
    pub compact: bool,
    /// A call to an override whose `stack_pointer` is not the analysed
    /// function's own raises.
    pub per_address_ccs: Option<PerAddressCcs>,
    /// Run the indirect-branch classifier (default `True`). `False` leaves
    /// every site not in `known_targets` an `IndirectBranch`.
    #[pyo3(get)]
    pub resolve_indirect_branches: bool,
    /// When set, `analyze` runs this pipeline instead of the default.
    #[pyo3(get)]
    pub pipeline: Option<Py<PyOptimizerPipeline>>,
    per_address_ccs_view: OnceLock<PyObject>,
}

impl PyLifterOptions {
    pub(crate) fn new_default(py: Python<'_>) -> PyResult<Self> {
        Self::new(py, None, None, true, None, true, None)
    }

    /// Built once per arch.
    pub(crate) fn built_per_address_ccs(
        &self,
        regs: &rsleigh::SleighRegs,
        target: strider_target::SleighArch,
    ) -> PyResult<Arc<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>> {
        let Some(table) = &self.per_address_ccs else {
            return Ok(Arc::default());
        };
        let mut slot = table
            .built
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memo_per_arch(&mut slot, target, || {
            crate::strider_cls::build_per_address_ccs(&table.py, regs, target)
        })
    }
}

#[pymethods]
impl PyLifterOptions {
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
        per_address_ccs: Option<MapArg<HashMap<Addr, PyCallingConvention>>>,
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
            per_address_ccs: per_address_ccs.map(|t| PerAddressCcs {
                py: t.0.into_iter().map(|(a, cc)| (a.0, cc)).collect(),
                built: Mutex::default(),
            }),
            resolve_indirect_branches,
            pipeline,
            per_address_ccs_view: OnceLock::new(),
        })
    }

    /// Read-only `{callee_address: CallingConvention}`, or `None`.
    #[getter]
    fn per_address_ccs<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.per_address_ccs {
            Some(table) => mapping_view(&self.per_address_ccs_view, py, &table.py).map(Some),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> PyResult<String> {
        let cfg_repr = self.cfg.get().__repr__();
        let assumptions_repr = self.assumptions.get().__repr__();
        Ok(format!(
            "LifterOptions(cfg={}, compact={}, per_address_ccs={}, \
             resolve_indirect_branches={}, assumptions={}, pipeline={})",
            cfg_repr,
            py_bool(self.compact),
            self.per_address_ccs.as_ref().map_or_else(
                || "None".to_owned(),
                |t| format!("{{{} entries}}", t.py.len())
            ),
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
