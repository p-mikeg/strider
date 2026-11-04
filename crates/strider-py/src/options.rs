use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::call_other_abi::PyCallOtherAbi;
use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::strider_cls::function_bound;
use strider_orchestrator::opt::AssumptionOptions;

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
            // A tuple, so the read-only view is read-only all the way down.
            KnownTarget::Targets(v) => pyo3::types::PyTuple::new_bound(py, v).into_py(py),
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

/// Any iterable of addresses.
pub struct AddrSetArg(pub rustc_hash::FxHashSet<u64>);

impl<'py> FromPyObject<'py> for AddrSetArg {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        ob.iter()?
            .map(|item| item?.extract::<u64>())
            .collect::<PyResult<_>>()
            .map(Self)
    }
}

/// Any mapping argument, so a getter's read-only proxy round-trips through the
/// constructor.
///
/// Extracts in two phases: PyO3 panics if a dict changes size while it
/// iterates it, and a per-element `extract` can run Python (`__index__`). So
/// the items are collected first with `PyDict_Next`, which runs no Python and
/// which a `dict` subclass cannot narrow (unlike `PyDict::copy`, whose
/// `PyDict_Merge` fallback honours an overridden `keys`), then extracted.
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

/// A table's Python face, a `MappingProxyType` built once. The Rust map beside
/// it stays the authority.
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

/// `view`'s object, built by `build` on first use.
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
    /// `function_max_size` and `allow_code_before_start_addr` as one extent.
    pub bound: strider_cfg::FunctionBound,
    /// `{dispatch_address: (target_address, ...) | "return"}`, as the getter
    /// hands it back; `seated` is the same table in `strider_cfg`'s form.
    pub known_targets: HashMap<u64, KnownTarget>,
    /// `{name: strider.sleigh.CallOtherAbi}`, winning over the built-in table.
    pub call_other_abis: HashMap<String, PyCallOtherAbi>,
    pub(crate) no_return_targets: rustc_hash::FxHashSet<u64>,
    /// Ranges the image marks as data, decoded only for the entry instruction.
    pub data_ranges: strider_cfg::DataRanges,
    /// The three tables' Python faces.
    known_targets_view: OnceLock<PyObject>,
    call_other_abis_view: OnceLock<PyObject>,
    no_return_targets_view: OnceLock<PyObject>,
    /// `known_targets` seated at construction, borrowed by every analyse.
    pub(crate) seated: SeatedTargets,
    /// `call_other_abis` as `strider_cfg` borrows it, rebuilt per arch.
    overrides: Mutex<OverridesMemo>,
}

/// `slot`'s table when it was built for `target`'s preset, else `build`'s,
/// kept in `slot`.
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

/// The last `call_other_abis` resolution and how many were made.
#[derive(Default)]
struct OverridesMemo {
    built: Option<(
        strider_target::SleighArch,
        Arc<strider_target::call_other_abi::CallOtherOverrides>,
    )>,
    #[cfg(feature = "test-hooks")]
    builds: usize,
}

/// `known_targets` as `strider_cfg::CfgOptions` borrows it.
pub(crate) type SeatedTargets =
    rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>;

impl PyCfgOptions {
    /// `call_other_abis` checked against and built for `target`, memoized per
    /// arch.
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
    /// Raises `ValueError` for `function_max_size=0` (omit the argument for an
    /// unbounded lift), and for a size with `allow_code_before_start_addr=True`.
    #[new]
    #[pyo3(signature = (*, function_max_size = None, allow_code_before_start_addr = false,
                        known_targets = MapArg(HashMap::new()),
                        call_other_abis = MapArg(HashMap::new()),
                        data_ranges = Vec::new(),
                        no_return_targets = AddrSetArg(rustc_hash::FxHashSet::default())))]
    fn new(
        function_max_size: Option<u64>,
        allow_code_before_start_addr: bool,
        known_targets: MapArg<HashMap<u64, KnownTarget>>,
        call_other_abis: MapArg<HashMap<String, CallOtherAbiArg>>,
        data_ranges: Vec<(u64, u64)>,
        no_return_targets: AddrSetArg,
    ) -> PyResult<Self> {
        let bound = function_bound(function_max_size, allow_code_before_start_addr)?;
        let seated = crate::strider_cls::seat_known_targets(&known_targets.0);
        Ok(Self {
            bound,
            known_targets: known_targets.0,
            call_other_abis: call_other_abis
                .0
                .into_iter()
                .map(|(name, abi)| (name, abi.0))
                .collect(),
            data_ranges: strider_cfg::DataRanges::try_new(data_ranges)
                .map_err(pyo3::exceptions::PyValueError::new_err)?,
            no_return_targets: no_return_targets.0,
            seated,
            ..Self::default()
        })
    }

    /// When set, an unconditional branch targeting at or past
    /// `entry + function_max_size` is treated as a tail call, bounding the
    /// lift; so is a fall-through past it, reported as `"past_function_end"`.
    #[getter]
    fn function_max_size(&self) -> Option<u64> {
        match self.bound {
            strider_cfg::FunctionBound::Size(size) => Some(size.get()),
            strider_cfg::FunctionBound::Open { .. } => None,
        }
    }

    /// When `False` (the default), an unconditional branch targeting an
    /// address *below* the function start is treated as a tail call. `True`
    /// follows such branches; always `False` beside a size.
    #[getter]
    fn allow_code_before_start_addr(&self) -> bool {
        matches!(
            self.bound,
            strider_cfg::FunctionBound::Open {
                code_before_start: true
            }
        )
    }

    /// Address ranges `(start, end)`, end exclusive, the image marks as data,
    /// sorted and merged. A branch or fall-through into one is reported, never
    /// decoded.
    #[getter]
    fn data_ranges(&self) -> Vec<(u64, u64)> {
        self.data_ranges
            .ranges()
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    /// Indirect-branch answers supplied by the caller, as a read-only
    /// `{dispatch_address: (target_address, ...) | "return"}` mapping.
    #[getter]
    fn known_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        mapping_view(&self.known_targets_view, py, &self.known_targets)
    }

    /// Classifications for Sleigh user-op names, as a read-only
    /// `{name: strider.sleigh.CallOtherAbi}` mapping.
    #[getter]
    fn call_other_abis<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        mapping_view(&self.call_other_abis_view, py, &self.call_other_abis)
    }

    /// Direct-call targets that never return, as a `frozenset`.
    #[getter]
    fn no_return_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        memo_obj(&self.no_return_targets_view, py, || {
            Ok(pyo3::types::PyFrozenSet::new_bound(py, &self.no_return_targets)?.into_any())
        })
    }

    /// How many times `call_other_abis` was resolved for an analyse.
    #[cfg(feature = "test-hooks")]
    fn _call_other_override_builds(&self) -> usize {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .builds
    }

    fn __repr__(&self) -> String {
        // Tables render as counts; omitting them would misdescribe the run.
        format!(
            "CfgOptions(function_max_size={}, allow_code_before_start_addr={}, \
             known_targets={{{} entries}}, call_other_abis={{{} entries}}, \
             data_ranges=[{} ranges], no_return_targets={{{} entries}})",
            self.function_max_size()
                .map_or_else(|| "None".to_owned(), |size| size.to_string()),
            py_bool(self.allow_code_before_start_addr()),
            self.known_targets.len(),
            self.call_other_abis.len(),
            self.data_ranges.ranges().len(),
            self.no_return_targets.len(),
        )
    }
}

/// `True` / `False`, for an eval-able `repr`.
pub(crate) fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Claims about the code being analysed, keyword-only. None is checked, and
/// each one's risky value is the positive one, so a wrong claim makes the
/// answer wrong. Clearing all nine still takes memory to be RAM and the
/// floating-point controls to be the IEEE default.
///
/// Five default `True`: `stack_global_disjoint`,
/// `assume_incoming_args_survive_calls`,
/// `return_slots_survive_untracked_writes`, `msr_register_keeps_mode` and
/// `fp_rounding_mode_is_nearest`. An omitted or `None` field takes its
/// default.
#[pyclass(eq, name = "AssumptionOptions", module = "strider.lift", frozen)]
#[derive(Clone, Default, PartialEq)]
pub struct PyAssumptionOptions {
    pub(crate) inner: AssumptionOptions,
}

/// One field as Python reads and writes it.
trait PyField<P> {
    fn to_py(&self) -> P;
    fn from_py(value: P) -> Self;
    /// The value as a constructor argument spells it.
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

/// Sorted: the set has no order of its own, and a `repr` must read back.
impl PyField<Vec<u64>> for rustc_hash::FxHashSet<u64> {
    fn to_py(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.iter().copied().collect();
        v.sort_unstable();
        v
    }

    fn from_py(value: Vec<u64>) -> Self {
        value.into_iter().collect()
    }

    fn py_repr(&self) -> String {
        format!("{:?}", PyField::<Vec<u64>>::to_py(self))
    }
}

/// The getters, the keyword-only constructor (an omitted or `None` field
/// takes `AssumptionOptions::default`'s), `none` (the same keywords over
/// `AssumptionOptions::none`) and `__repr__`, from one row per field.
macro_rules! assumption_fields {
    ($($(#[$doc:meta])* $name:ident: $py:ty),* $(,)?) => {
        impl PyAssumptionOptions {
            /// `base` with each given field replaced.
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

            /// Every claim cleared, then the ones named set. With none named,
            /// the sound floor for code whose memory behaves as RAM (a polled
            /// MMIO register reads as the value written) and whose float
            /// arithmetic rounds to nearest with flush-to-zero and
            /// denormals-are-zero off.
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

            /// Reads back as the constructor call that produces it.
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
    /// The stack and global/constant memory (`.data`, `.rodata`, `.bss`,
    /// MMIO) never overlap, so a memory walk steps through a constant-address
    /// store from a stack load and vice-versa (default `True`). Wrong only
    /// where a constant address equals `sp + K`.
    stack_global_disjoint: bool,

    /// A call leaves the incoming stack-argument slots alone (default
    /// `True`); read by incoming-argument detection only. A pointer the
    /// function hands a callee into them, or into the register-argument homes
    /// just below them, is caught, and there the slots stay unknown. A
    /// `custom` convention homing register arguments next to the stack ones
    /// (ARM, MIPS, PPC64, riscv, Win64) must pass
    /// `reg_args_adjoin_stack_args=True`. Not caught: a pointer to a local
    /// escaping that local, or a callee reading a pointer from a register its
    /// convention preserves.
    assume_incoming_args_survive_calls: bool,

    /// A store rooted at a different SP base than the entry SP (an
    /// alignment-masked frame local, say) is disjoint from the probed
    /// location.
    distinct_sp_bases_disjoint: bool,

    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// allow such writes; this asserts compiler output makes none. Inert
    /// without `escape_analysis` or a call to a `noalias_allocators` entry.
    callee_preserves_stack_args: bool,

    /// Callee addresses of pure `noalias` heap allocators (`malloc`-like: a
    /// size in, a fresh pointer out, no pointer arguments). Distinct
    /// allocations are disjoint from each other, the stack and every constant
    /// address, and a load steps through such a call, so an allocator carving
    /// from a static or caller-supplied arena does not qualify. Deallocation
    /// is not modelled: a stale pointer is taken as disjoint from storage the
    /// allocator handed out again. Reads back sorted, without duplicates.
    noalias_allocators: Vec<u64>,

    /// When no stack address escapes to a callee, forward a spill load across
    /// a call and, under `stack_global_disjoint`, past an opaque store. An
    /// address in an argument, a stored value, a return or a clobbered
    /// register escapes; the claim is that no callee reads one from a register
    /// its convention preserves.
    escape_analysis: bool,

    /// Untracked RAM stores, callees and user-ops leave the slots holding the
    /// saved return address and entry stack pointer alone (default `True`).
    /// Read by return-target verification only: off, a return after a user-op
    /// or a memory-clobbering call is reported unverified, and a store through
    /// an unplaced pointer is checked against the slot.
    return_slots_survive_untracked_writes: bool,

    /// An ARM register-form `msr cpsr_c, rN` writes the mode already in force,
    /// so it changes neither the mode nor the banked registers (default
    /// `True`); the immediate form and `cps` still switch mode. Off, it
    /// re-banks `sp`, `lr` and the rest unless its mode is provably current.
    /// Wrong for a mode built in a register or restored after a later switch
    /// (Linux's `safe_svcmode_maskall`, `fiqasm.S`). Read by the lift, so
    /// `Function.optimize` refuses to add it.
    msr_register_keeps_mode: bool,

    /// Float conversions that read the dynamic rounding mode (x86 MXCSR.RC,
    /// x87 RC at `fst` / `fstp`, PowerPC FPSCR.RN, ARM FPSCR.RMode, AArch64
    /// FPCR.RMode, MIPS FCSR.RM) round to nearest even (default `True`), so
    /// they fold. Fixed-mode forms (`cvtt*`, `fcti*z`, ...) read no mode, and
    /// x87 `fist`, `fistp` and `frndint` always read RC. Off, each selects
    /// among the four modes, folding a mode the function sets.
    /// Wrong after `fesetround` or a caller's mode change. Read by the lift,
    /// so `Function.optimize` refuses to add it.
    fp_rounding_mode_is_nearest: bool,
}

#[pymethods]
impl PyAssumptionOptions {
    /// Every claim either holds, and every allocator either lists.
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

/// A caller's `per_address_ccs` table and the resolution taken from it.
pub struct PerAddressCcs {
    pub(crate) py: HashMap<u64, PyCallingConvention>,
    built: Mutex<Option<BuiltPerAddressCcs>>,
}

/// A resolved `per_address_ccs` table and the arch it was resolved against.
type BuiltPerAddressCcs = (
    strider_target::SleighArch,
    Arc<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
);

/// Lift, optimize, and CFG knobs for one `analyze` call.
#[pyclass(name = "LifterOptions", module = "strider.lift", frozen)]
pub struct PyLifterOptions {
    /// Nested CFG-shape knobs.
    #[pyo3(get)]
    pub cfg: Py<PyCfgOptions>,
    /// Nested claims about the analysed code; the defaults when not passed.
    #[pyo3(get)]
    pub assumptions: Py<PyAssumptionOptions>,
    /// Drop unreachable IR nodes after analysis (default `True`).
    #[pyo3(get)]
    pub compact: bool,
    /// Per-target-address calling-convention overrides (preset or custom
    /// CCs accepted); `None`/omitted means no overrides. An override's
    /// `stack_pointer` must be the analysed function's own, or `analyze`
    /// raises: the override describes a callee, not another frame.
    pub per_address_ccs: Option<PerAddressCcs>,
    /// Run the indirect-branch classifier (default `True`). `False` leaves
    /// every site not in `CfgOptions(known_targets=...)` an `IndirectBranch`
    /// placeholder.
    #[pyo3(get)]
    pub resolve_indirect_branches: bool,
    /// When set, `analyze` runs this pipeline instead of the built-in
    /// default, for this call only.
    #[pyo3(get)]
    pub pipeline: Option<Py<PyOptimizerPipeline>>,
    /// `per_address_ccs`' Python face.
    per_address_ccs_view: OnceLock<PyObject>,
}

impl PyLifterOptions {
    /// All-defaults fallback for `opts=None`.
    pub(crate) fn new_default(py: Python<'_>) -> PyResult<Self> {
        Self::new(py, None, None, true, None, true, None)
    }

    /// The overrides resolved against `regs`, built once per arch.
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
    /// Exposes the `Py` handles to the cyclic GC.
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
        per_address_ccs: Option<MapArg<HashMap<u64, PyCallingConvention>>>,
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
                py: t.0,
                built: Mutex::default(),
            }),
            resolve_indirect_branches,
            pipeline,
            per_address_ccs_view: OnceLock::new(),
        })
    }

    /// Per-target-address calling-convention overrides as a read-only
    /// `{address: CallingConvention}` mapping, or `None` when there are none.
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
