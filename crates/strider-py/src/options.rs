use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::call_other_abi::PyCallOtherAbi;
use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::reader::{LocalEntries, PyLoadedElf};
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
            // A tuple, not a list: the view below blocks writes to its keys
            // only, so a mutable value would take an edit the analysis never
            // reads.
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

/// A mapping argument. Any mapping, not only a `dict`: the `known_targets` /
/// `call_other_abis` getters hand back a read-only proxy, and that has to
/// round-trip through the constructor.
pub struct MapArg<T>(pub T);

impl<'py, K, V, S> FromPyObject<'py> for MapArg<HashMap<K, V, S>>
where
    K: FromPyObject<'py> + Eq + std::hash::Hash,
    V: FromPyObject<'py>,
    S: std::hash::BuildHasher + Default,
{
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        // Two phases, because a per-element `extract` runs Python through
        // `__index__`, and PyO3 PANICS if a dict changes size or keys while it
        // is iterating it (`PanicException` is a `BaseException`, which no
        // `except Exception` catches).
        //
        // `iter` is `PyDict_Next` over raw storage and runs no Python, so
        // collecting cannot be interrupted and a `dict` SUBCLASS cannot narrow
        // it.  `PyDict::copy` is not equivalent: its raw-storage fast path is
        // conditional on the subclass not overriding `__iter__`, and the
        // `PyDict_Merge` fallback then honours an overridden `keys`, so a
        // subclass overriding both copies as `{}`.
        //
        // Phase two extracts from the `Vec`, so the Python it runs is
        // iterating nothing.  The non-dict branch is not Python-free (`dict`
        // runs `keys` and `__getitem__`) but completes before any extract.
        // Values are the caller's own objects, so a list value is read live
        // (`KnownTarget`'s `Vec<u64>`).
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

/// A table's Python face, built once and handed out read-only.
///
/// The authority is the Rust map beside it, which no mutation here would
/// reach, so every value in the proxy is immutable too.
///
/// Read-only to ordinary access only. `MappingProxyType` wraps a dict, and
/// `gc.get_referents(view)[0]` hands that dict back mutably -- CPython gives
/// no way to hide an object's referents. Writing to it desynchronizes the view
/// from the Rust map, which keeps answering every `analyze`; a value fed back
/// through a constructor is re-extracted and re-validated like any other, so
/// the reach is a caller confusing itself about its own options, not getting
/// an unchecked value into the analysis.
#[derive(Default)]
struct MappingView(OnceLock<PyObject>);

impl MappingView {
    fn get<'py, T: IntoPy<PyObject> + Clone>(
        &self,
        py: Python<'py>,
        source: &T,
    ) -> PyResult<Bound<'py, PyAny>> {
        let obj = match self.0.get() {
            Some(o) => o,
            None => {
                let dict = source.clone().into_py(py);
                let proxy = py
                    .import_bound("types")?
                    .getattr("MappingProxyType")?
                    .call1((dict,))?;
                self.0.get_or_init(|| proxy.unbind())
            }
        };
        Ok(obj.bind(py).clone())
    }
}

/// CFG-shaping knobs, keyword-only.
#[pyclass(name = "CfgOptions", module = "strider.cfg")]
#[derive(Clone, Default)]
pub struct PyCfgOptions {
    /// `function_max_size` and `allow_code_before_start_addr` as one extent.
    pub bound: strider_cfg::FunctionBound,
    /// Indirect-branch answers supplied by the caller, as
    /// `{dispatch_address: [target_address, ...] | "return"}`, in the Python
    /// form the getter hands back; `seated` is the same table as
    /// `strider_cfg` reads it.
    ///
    /// A listed site seats these answers directly instead of deferring to the
    /// classifier. `"return"` seats the site as a function return, which the
    /// resolver itself proves only for a bare link register. An empty list
    /// seats nothing, leaving the site an unresolved indirect branch; the
    /// classifier still runs unless `resolve_indirect_branches=False`.
    ///
    /// `Arc`: one seeded table is read by every `analyze` through the same
    /// options object, and a per-call clone of it is O(table).
    pub known_targets: Arc<HashMap<u64, KnownTarget>>,
    /// Classifications for Sleigh user-op names, as
    /// `{name: strider.sleigh.CallOtherAbi}`, winning over the built-in table.
    ///
    /// `Lifter.user_op_names()` lists the names a binary can contain, and
    /// `Lifter.call_other_abi(name)` reads back what strider already makes of
    /// one.
    pub call_other_abis: Arc<HashMap<String, PyCallOtherAbi>>,
    /// `no_return_targets` as `strider_cfg` borrows it, shared with every clone.
    pub(crate) no_return_targets: Arc<NoReturnTargets>,
    /// Address ranges `(start, end)` the image marks as data, decoded only for
    /// the entry instruction.
    pub data_ranges: strider_cfg::DataRanges,
    /// The two tables' Python faces, shared with every clone of this object,
    /// which shares the tables themselves.
    known_targets_view: Arc<MappingView>,
    call_other_abis_view: Arc<MappingView>,
    /// `known_targets` in `strider_cfg`'s own form, seated at construction:
    /// every analyse through this object BORROWS it, so neither the seating
    /// nor a copy is charged per call. Shared with every clone, as the table
    /// it is built from is.
    pub(crate) seated: Arc<SeatedTargets>,
    /// `call_other_abis` as `strider_cfg` borrows it, built on first use and
    /// rebuilt when the arch changes. Shared with every clone, as the table it is built from is.
    overrides: Arc<std::sync::Mutex<OverridesMemo>>,
}

/// `slot`'s table when it was built for `target`'s preset, else `build`'s,
/// kept in `slot`: every field of a `SleighArch` is fixed by its preset.
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
    builds: usize,
}

/// The set beside its `frozenset` face, so the two cannot be split apart.
#[derive(Default)]
pub(crate) struct NoReturnTargets {
    pub(crate) set: rustc_hash::FxHashSet<u64>,
    view: OnceLock<Py<pyo3::types::PyFrozenSet>>,
    local: LocalEntryMemo<NoReturnTargets>,
}

impl NoReturnTargets {
    fn new(set: rustc_hash::FxHashSet<u64>) -> Self {
        Self {
            set,
            ..Self::default()
        }
    }

    /// This set plus the local entry of every member `entries` names.
    fn with_local_entries(self: &Arc<Self>, entries: &Arc<LocalEntries>) -> Arc<Self> {
        if entries.is_empty() || self.set.is_empty() {
            return Arc::clone(self);
        }
        self.local
            .get(entries, || {
                let added: Vec<u64> = local_entries_of(entries, self.set.iter().copied())
                    .map(|(_, local)| local)
                    .filter(|local| !self.set.contains(local))
                    .collect();
                (!added.is_empty())
                    .then(|| Self::new(self.set.iter().copied().chain(added).collect()))
            })
            .unwrap_or_else(|| Arc::clone(self))
    }
}

/// A table widened by each ELF's [`LocalEntries`], one per entries table
/// still alive, so options shared by several handles rebuild for none of them
/// and a table rebuilt after `add_elf` is widened afresh. `None` when no key
/// had a local entry of its own.
struct LocalEntryMemo<T>(std::sync::Mutex<Vec<Widened<T>>>);

/// The entries a table was widened by, and the widened table. A dead `Weak`
/// still pins its allocation, so its address names no later table.
type Widened<T> = (std::sync::Weak<LocalEntries>, Option<Arc<T>>);

impl<T> Default for LocalEntryMemo<T> {
    fn default() -> Self {
        Self(std::sync::Mutex::default())
    }
}

impl<T> LocalEntryMemo<T> {
    fn get(
        &self,
        entries: &Arc<LocalEntries>,
        widen: impl FnOnce() -> Option<T>,
    ) -> Option<Arc<T>> {
        let mut slots = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, widened)) = slots
            .iter()
            .find(|(built_for, _)| std::ptr::eq(built_for.as_ptr(), Arc::as_ptr(entries)))
        {
            return widened.clone();
        }
        slots.retain(|(built_for, _)| built_for.strong_count() > 0);
        let widened = widen().map(Arc::new);
        slots.push((Arc::downgrade(entries), widened.clone()));
        widened
    }
}

/// The `(key, local entry)` pairs `entries` holds for `keys`.
fn local_entries_of(
    entries: &LocalEntries,
    keys: impl Iterator<Item = u64>,
) -> impl Iterator<Item = (u64, u64)> {
    keys.flat_map(move |key| {
        let from = entries.partition_point(|&(address, _)| address < key);
        entries[from..]
            .iter()
            .take_while(move |&&(address, _)| address == key)
            .copied()
    })
}

/// `known_targets` as `strider_cfg::CfgOptions` borrows it.
pub(crate) type SeatedTargets =
    rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>;

impl PyCfgOptions {
    /// This object with the mapping-symbol data ranges of `elf`, unless it
    /// already names some, and with the local entry of every
    /// `no_return_targets` address `elf` has one for. Shares the ELF's tables
    /// rather than copying them through Python on every analyse.
    fn with_elf(&self, elf: &PyLoadedElf) -> PyResult<Self> {
        let mut out = self.clone();
        if out.data_ranges.ranges().is_empty() {
            out.data_ranges = elf.data_ranges()?;
        }
        if !self.no_return_targets.set.is_empty() {
            out.no_return_targets = self
                .no_return_targets
                .with_local_entries(&elf.local_entries()?);
        }
        Ok(out)
    }

    /// `call_other_abis` checked against and built for `target`, rebuilt only
    /// when the arch changes, not per analyse.
    pub(crate) fn call_other_overrides(
        &self,
        target: strider_target::SleighArch,
    ) -> PyResult<Arc<strider_target::call_other_abi::CallOtherOverrides>> {
        let mut memo = self
            .overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let OverridesMemo { built, builds } = &mut *memo;
        memo_per_arch(built, target, || {
            *builds += 1;
            crate::strider_cls::overrides_from(&self.call_other_abis, target)
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
            known_targets: Arc::new(known_targets.0),
            call_other_abis: Arc::new(
                call_other_abis
                    .0
                    .into_iter()
                    .map(|(name, abi)| (name, abi.0))
                    .collect(),
            ),
            data_ranges: data_ranges_from(data_ranges)?,
            no_return_targets: Arc::new(NoReturnTargets::new(no_return_targets.0)),
            known_targets_view: Arc::default(),
            call_other_abis_view: Arc::default(),
            seated: Arc::new(seated),
            overrides: Arc::default(),
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
    /// `{dispatch_address: [target_address, ...] | "return"}` mapping.
    #[getter]
    fn known_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.known_targets_view.get(py, &*self.known_targets)
    }

    /// Classifications for Sleigh user-op names, as a read-only
    /// `{name: strider.sleigh.CallOtherAbi}` mapping.
    #[getter]
    fn call_other_abis<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.call_other_abis_view.get(py, &*self.call_other_abis)
    }

    /// Direct-call targets that never return, as a `frozenset`.
    #[getter]
    fn no_return_targets<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, pyo3::types::PyFrozenSet>> {
        let targets = &*self.no_return_targets;
        let set = match targets.view.get() {
            Some(set) => set,
            None => {
                let set = pyo3::types::PyFrozenSet::new_bound(py, targets.set.iter())?;
                targets.view.get_or_init(|| set.unbind())
            }
        };
        Ok(set.bind(py).clone())
    }

    /// This object as `ElfLifter` runs it on `elf`; see [`Self::with_elf`].
    fn _with_elf(&self, elf: PyRef<'_, PyLoadedElf>) -> PyResult<Self> {
        self.with_elf(&elf)
    }

    /// The address of the seated `known_targets` table every analyse borrows,
    /// shared by every object derived from this one.
    fn _seated_table_id(&self) -> usize {
        Arc::as_ptr(&self.seated) as usize
    }

    /// How many times `call_other_abis` was resolved for an analyse, by this
    /// object and every one derived from it.
    fn _call_other_override_builds(&self) -> usize {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .builds
    }

    /// This object bounded to `function_max_size`, sharing its tables rather
    /// than rebuilding them through Python. The size replaces the whole bound.
    fn _with_function_max_size(&self, function_max_size: u64) -> PyResult<Self> {
        Ok(Self {
            bound: function_bound(Some(function_max_size), false)?,
            ..self.clone()
        })
    }

    fn __repr__(&self) -> String {
        // The tables render as counts: a seeded table is what decides the
        // successor set, so a `repr` that omits it describes a different
        // analysis than the one that ran.
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
            self.no_return_targets.set.len(),
        )
    }
}

/// # Errors
/// If a range ends at or below its start. `start..end` with `end <= start` is
/// empty, so it used to be accepted and silently drop the range the caller
/// meant to name -- the literal pool it marks then decodes as instructions,
/// which is the failure `data_ranges` exists to prevent. Every other
/// range-checked ctor argument raises, so this one does too.
fn data_ranges_from(ranges: Vec<(u64, u64)>) -> PyResult<strider_cfg::DataRanges> {
    for &(start, end) in &ranges {
        if end <= start {
            return Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "data_ranges: ({start:#x}, {end:#x}) is empty; a range runs \
                 start..end with end > start"
            )));
        }
    }
    Ok(strider_cfg::DataRanges::new(
        ranges.into_iter().map(|(start, end)| start..end),
    ))
}

/// Render a bool the Python way, so a `repr` reads as an eval-able constructor
/// call (`True`/`False`, not Rust's `true`/`false`).
pub(crate) fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Claims about the code being analysed, keyword-only. None is checked, and
/// each one's risky value is the positive one, so any of them can make the
/// answer wrong on valid input. Clearing all nine leaves only what the IR
/// structurally proves, which still takes memory to be RAM.
///
/// Five default `True`: `stack_global_disjoint`,
/// `assume_incoming_args_survive_calls` and
/// `return_slots_survive_untracked_writes`, each of which every compiler this
/// analyses honours, `msr_register_keeps_mode`, which only ARM code
/// switching mode through a register `msr` breaks, and
/// `fp_rounding_mode_is_nearest`, which only code changing the SSE or PowerPC
/// rounding mode breaks.
///
/// An omitted or `None` field takes the analysis default, as listed above.
#[pyclass(eq, name = "AssumptionOptions", module = "strider.lift")]
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
            /// the sound floor of these fields under any input program whose
            /// memory behaves as RAM, forwarding solely what the IR
            /// structurally proves. A memory-mapped register polled after a
            /// write still reads as the value written.
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
    /// MMIO) never overlap at runtime, so a memory walk steps through a
    /// constant-address store when looking back from a stack load and
    /// vice-versa (default `True`). Wrong only where a constant address
    /// coincidentally equals `sp + K`.
    stack_global_disjoint: bool,

    /// A call on an incoming stack-argument slot's memory chain leaves the
    /// slot alone (default `True`). Reaches incoming-argument detection only,
    /// where it holds for any conforming callee: those slots are the caller's
    /// memory, above the entry SP. A function that hands a callee a pointer
    /// into them, or into the register arguments' homes a convention keeps
    /// just below them (ARM, MIPS, PowerPC64), is caught (`h(&a); return
    /// a;`), and there every call keeps the slots unknown. A `custom`
    /// convention whose ABI homes or splits register arguments next to the
    /// stack ones (ARM, MIPS, PPC64, riscv, Win64) must pass
    /// `reg_args_adjoin_stack_args=True`. A pointer to a local is taken to
    /// stay inside that local; a callee reading a pointer from a register its
    /// convention preserves is not caught.
    assume_incoming_args_survive_calls: bool,

    /// A store rooted at a different SP base than the entry SP (an
    /// alignment-masked frame local, say) is disjoint from the probed
    /// location.
    distinct_sp_bases_disjoint: bool,

    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// let a callee write those slots; this asserts compiler output does not.
    ///
    /// Inert on its own: it only empties an outgoing-argument window that
    /// something else already consults, which is `escape_analysis` or a call
    /// to an address listed in `noalias_allocators`. Set with one of those.
    callee_preserves_stack_args: bool,

    /// Callee addresses of pure `noalias` heap allocators (`malloc`/`calloc`-like:
    /// a size in, a fresh non-overlapping pointer out, no pointer arguments).
    /// Distinct allocations are treated as disjoint, and a load steps through
    /// such a call. An allocation also overlaps no stack slot and no global or
    /// other constant address, so an allocator carving from a static or a
    /// caller-supplied arena does not qualify. Disjointness among
    /// allocations holds for those live at once: nothing
    /// models deallocation, so a stale pointer is taken as disjoint from the
    /// storage the allocator handed out again. Reads back sorted, without
    /// duplicates.
    noalias_allocators: Vec<u64>,

    /// When the frame is provably private (no stack address escapes to a
    /// callee), forward a spill load across a call and past an opaque store.
    /// An address held in an argument, a stored value, a return, or a register
    /// a call clobbers escapes; the claim is that no callee reads one from a
    /// register its convention preserves.
    escape_analysis: bool,

    /// A RAM store whose address the analysis cannot place on the entry stack,
    /// and any callee or user-op, leave the slots the function saved its
    /// return address and entry stack pointer in alone (default `True`). Read
    /// by return-target verification alone: off, a return after a call is
    /// reported as unverified, and a store through a pointer the analysis can
    /// only guess at is checked against the slot. A store its SP decomposition
    /// places is checked either way.
    return_slots_survive_untracked_writes: bool,

    /// The mode an ARM register-form `msr` writes to the CPSR control field
    /// (`msr cpsr_c, rN`) is the mode already in force, so the write changes
    /// neither the mode nor the banked registers (default `True`); its masks
    /// and flags still update, and the immediate form and `cps` still switch
    /// mode. Off, it re-banks `sp`, `lr` and the rest unless its mode is
    /// provably the current one. Wrong for a new mode built in a register
    /// (Linux's boot-time `safe_svcmode_maskall`, the switch into FIQ in
    /// `fiqasm.S`) and for a saved mode restored after a switch made since
    /// the save (the return to SVC in `fiqasm.S`'s `__set_fiq_regs`). Read by
    /// the lift, so `analyze` honours it and `Function.optimize` refuses to add
    /// it.
    msr_register_keeps_mode: bool,

    /// The dynamic rounding mode a float-to-integer or narrowing conversion
    /// rounds by is round to nearest even (default `True`): x86 MXCSR.RC at
    /// `cvtsd2si`, `cvtss2si`, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`,
    /// `cvtps2pi`, `cvtsd2ss`, `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`,
    /// `cvtsi2ss`, the 64-bit `cvtsi2sd` and their scalar VEX forms, and
    /// PowerPC FPSCR.RN at `fctid`, `fctiw`, `fctidu`, `fctiwu`, `fcfid`,
    /// `fcfids`, `fcfidu`, `fcfidus`, `frsp` and `xscvdpspn`. The
    /// truncating forms (`cvtt*`, `fcti*z`) read no mode, and the x87 stores
    /// read the control word either way. Off, each reads the mode and selects
    /// among its four, so a mode set by `ldmxcsr` or `mtfsfi` folds and an
    /// unknown one stays a select. Wrong after `fesetround` or a caller's mode
    /// change. Float arithmetic is taken to round to nearest, the conversions
    /// lifted as opaque user ops (packed VEX, EVEX, `roundss`) are outside it,
    /// and MXCSR.FTZ/DAZ and PowerPC FPSCR.NI/OE/UE are taken as 0, so
    /// `none()` is not a sound floor for `-ffast-math` code or code that sets
    /// those. Read by the lift, so `analyze` honours it and
    /// `optimize` refuses to add it.
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
///
/// One object behind an `Arc`, because `ElfLifter.analyze` derives fresh
/// options through `_with_cfg` on every call to seat the ELF's data ranges: a
/// memo on the options themselves is written on the throwaway and rebuilt
/// next call, and cloning the table there is O(entries) in `Py` handle bumps.
pub struct PerAddressCcs {
    py: HashMap<u64, PyCallingConvention>,
    built: std::sync::Mutex<Option<BuiltPerAddressCcs>>,
    local: LocalEntryMemo<PerAddressCcs>,
}

impl PerAddressCcs {
    fn new(py: HashMap<u64, PyCallingConvention>) -> Self {
        Self {
            py,
            built: std::sync::Mutex::default(),
            local: LocalEntryMemo::default(),
        }
    }

    /// This table plus, for every key `entries` has a local entry for, that
    /// local entry under the key's convention. A key already at the local
    /// entry keeps its own.
    fn with_local_entries(self: &Arc<Self>, entries: &Arc<LocalEntries>) -> Arc<Self> {
        if entries.is_empty() || self.py.is_empty() {
            return Arc::clone(self);
        }
        self.local
            .get(entries, || {
                let added: Vec<(u64, u64)> = local_entries_of(entries, self.py.keys().copied())
                    .filter(|(_, local)| !self.py.contains_key(local))
                    .collect();
                (!added.is_empty()).then(|| {
                    let mut py = self.py.clone();
                    for (global, local) in added {
                        py.insert(local, self.py[&global].clone());
                    }
                    Self::new(py)
                })
            })
            .unwrap_or_else(|| Arc::clone(self))
    }
}

/// A resolved `per_address_ccs` table and the arch it was resolved against.
type BuiltPerAddressCcs = (
    strider_target::SleighArch,
    std::sync::Arc<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
);

/// Lift, optimize, and CFG knobs for one `analyze` call.
#[pyclass(name = "LifterOptions", module = "strider.lift")]
pub struct PyLifterOptions {
    /// Nested CFG-shape knobs.
    #[pyo3(get)]
    pub cfg: Py<PyCfgOptions>,
    /// Nested claims about the analysed code, each one unchecked; the
    /// defaults when not passed.
    #[pyo3(get)]
    pub assumptions: Py<PyAssumptionOptions>,
    /// Drop unreachable IR nodes after analysis (default `True`).
    #[pyo3(get)]
    pub compact: bool,
    /// Per-target-address calling-convention overrides (preset or custom
    /// CCs accepted); `None`/omitted means no overrides.
    /// Shared so the table and its resolution both survive `_with_cfg`; see
    /// [`PerAddressCcs`].
    pub per_address_ccs: Option<std::sync::Arc<PerAddressCcs>>,
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
    /// `per_address_ccs`' Python face.
    per_address_ccs_view: MappingView,
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
    ) -> PyResult<std::sync::Arc<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>>
    {
        let Some(table) = &self.per_address_ccs else {
            return Ok(std::sync::Arc::default());
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
            per_address_ccs: per_address_ccs.map(|t| std::sync::Arc::new(PerAddressCcs::new(t.0))),
            resolve_indirect_branches,
            pipeline,
            per_address_ccs_view: MappingView::default(),
        })
    }

    /// Per-target-address calling-convention overrides as a read-only
    /// `{address: CallingConvention}` mapping, or `None` when there are none.
    #[getter]
    fn per_address_ccs<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.per_address_ccs {
            Some(table) => self.per_address_ccs_view.get(py, &table.py).map(Some),
            None => Ok(None),
        }
    }

    /// These options as `ElfLifter.analyze` runs them on `elf`: the ELF's data
    /// ranges unless `cfg` names some, and the local entry of every
    /// `no_return_targets` address and `per_address_ccs` key that is a ppc64
    /// ELFv2 global entry, which a same-TOC call enters past.
    fn _with_elf(&self, py: Python<'_>, elf: PyRef<'_, PyLoadedElf>) -> PyResult<Self> {
        let cfg = Py::new(py, self.cfg.borrow(py).with_elf(&elf)?)?;
        let mut out = self._with_cfg(py, cfg);
        if let Some(table) = &self.per_address_ccs
            && !table.py.is_empty()
        {
            out.per_address_ccs = Some(table.with_local_entries(&elf.local_entries()?));
        }
        Ok(out)
    }

    /// The address of the `per_address_ccs` table an analyse resolves, or
    /// `None` without one.
    fn _per_address_ccs_table_id(&self) -> Option<usize> {
        self.per_address_ccs
            .as_ref()
            .map(|table| Arc::as_ptr(table) as usize)
    }

    /// These options with `cfg` replaced and every other field carried over.
    fn _with_cfg(&self, py: Python<'_>, cfg: Py<PyCfgOptions>) -> Self {
        Self {
            cfg,
            assumptions: self.assumptions.clone_ref(py),
            compact: self.compact,
            per_address_ccs: self.per_address_ccs.clone(),
            resolve_indirect_branches: self.resolve_indirect_branches,
            pipeline: self.pipeline.as_ref().map(|p| p.clone_ref(py)),
            per_address_ccs_view: MappingView::default(),
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
