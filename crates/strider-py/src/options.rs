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
#[pyclass(name = "CfgOptions", module = "strider.cfg", frozen)]
#[derive(Default)]
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
    pub known_targets: HashMap<u64, KnownTarget>,
    /// Classifications for Sleigh user-op names, as
    /// `{name: strider.sleigh.CallOtherAbi}`, winning over the built-in table.
    ///
    /// `Lifter.user_op_names()` lists the names a binary can contain, and
    /// `Lifter.call_other_abi(name)` reads back what strider already makes of
    /// one.
    pub call_other_abis: HashMap<String, PyCallOtherAbi>,
    pub(crate) no_return_targets: rustc_hash::FxHashSet<u64>,
    /// Address ranges `(start, end)` the image marks as data, decoded only for
    /// the entry instruction.
    pub data_ranges: strider_cfg::DataRanges,
    /// The three tables' Python faces.
    known_targets_view: MappingView,
    call_other_abis_view: MappingView,
    no_return_targets_view: OnceLock<Py<pyo3::types::PyFrozenSet>>,
    /// `known_targets` in `strider_cfg`'s own form, seated at construction:
    /// every analyse through this object BORROWS it, so neither the seating
    /// nor a copy is charged per call.
    pub(crate) seated: SeatedTargets,
    /// `call_other_abis` as `strider_cfg` borrows it, built on first use and
    /// rebuilt when the arch changes.
    overrides: Mutex<OverridesMemo>,
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

/// `known_targets` as `strider_cfg::CfgOptions` borrows it.
pub(crate) type SeatedTargets =
    rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets>;

impl PyCfgOptions {
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
            let overrides = crate::strider_cls::overrides_from(&self.call_other_abis, target)?;
            *builds += 1;
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
    /// `{dispatch_address: [target_address, ...] | "return"}` mapping.
    #[getter]
    fn known_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.known_targets_view.get(py, &self.known_targets)
    }

    /// Classifications for Sleigh user-op names, as a read-only
    /// `{name: strider.sleigh.CallOtherAbi}` mapping.
    #[getter]
    fn call_other_abis<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.call_other_abis_view.get(py, &self.call_other_abis)
    }

    /// Direct-call targets that never return, as a `frozenset`.
    #[getter]
    fn no_return_targets<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, pyo3::types::PyFrozenSet>> {
        let set = match self.no_return_targets_view.get() {
            Some(set) => set,
            None => {
                let set = pyo3::types::PyFrozenSet::new_bound(py, self.no_return_targets.iter())?;
                self.no_return_targets_view.get_or_init(|| set.unbind())
            }
        };
        Ok(set.bind(py).clone())
    }

    /// How many times `call_other_abis` was resolved for an analyse.
    fn _call_other_override_builds(&self) -> usize {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .builds
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
            self.no_return_targets.len(),
        )
    }
}

/// Render a bool the Python way, so a `repr` reads as an eval-able constructor
/// call (`True`/`False`, not Rust's `true`/`false`).
pub(crate) fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Claims about the code being analysed, keyword-only. None is checked, and
/// each one's risky value is the positive one, so any of them can make the
/// answer wrong on valid input. Clearing all nine leaves only what the IR
/// structurally proves, which still takes memory to be RAM and the
/// floating-point controls to be the IEEE default.
///
/// Five default `True`: `stack_global_disjoint`,
/// `assume_incoming_args_survive_calls` and
/// `return_slots_survive_untracked_writes`, each of which every compiler this
/// analyses honours, `msr_register_keeps_mode`, which only ARM code
/// switching mode through a register `msr` breaks, and
/// `fp_rounding_mode_is_nearest`, which only code changing the SSE, x87,
/// PowerPC, ARM VFP, AArch64 or MIPS FPU rounding mode breaks.
///
/// An omitted or `None` field takes the analysis default, as listed above.
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
            /// the sound floor of these fields under any input program whose
            /// memory behaves as RAM and whose floating-point controls are the
            /// IEEE default, forwarding solely what the IR structurally
            /// proves. A memory-mapped register polled after a write still
            /// reads as the value written. Outside the conversion rounding
            /// mode `fp_rounding_mode_is_nearest` claims, float arithmetic
            /// rounds to nearest with MXCSR.FTZ/DAZ, FPSCR.NI/OE/UE, FPSCR.FZ,
            /// FPCR.FZ and FCSR.FS clear.
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
    /// by return-target verification alone: off, a return after a user-op or
    /// a call whose convention does not preserve memory is reported as
    /// unverified, and a store through a pointer the analysis can only guess
    /// at is checked against the slot. A store its SP decomposition places is
    /// checked either way.
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

    /// The dynamic rounding mode a float-to-integer, narrowing or
    /// integer-to-float conversion rounds by is round to nearest even
    /// (default `True`): x86 MXCSR.RC at `cvtsd2si`, `cvtss2si`,
    /// `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`, `cvtps2pi`, `cvtsd2ss`,
    /// `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`, `cvtsi2ss`, the 64-bit `cvtsi2sd`
    /// and their scalar VEX forms, the x87 control word's RC at `fst` and
    /// `fstp` to memory, PowerPC FPSCR.RN at `fctid`, `fctiw`, `fctidu`,
    /// `fctiwu`, `fcfid`, `fcfids`, `fcfidu`, `fcfidus`, `frsp` and
    /// `xscvdpspn`, ARM FPSCR.RMode at `vcvtr`, `vcvt.f32.f64`,
    /// `vcvt.f32.s32`, `vcvt.f32.u32`, `vcvtb` and `vcvtt`, AArch64
    /// FPCR.RMode at `fcvt` to a narrower float, `fcvtn`, `scvtf`, `ucvtf`
    /// and `bfcvt`, and MIPS FCSR.RM at `cvt.w`, `cvt.l`, `cvt.s` and
    /// `cvt.d.l`. The fixed-mode forms (`cvtt*`, `fcti*z`, ARM `vcvt` to an
    /// integer, MIPS `round`, `trunc`, `floor`, `ceil`, AArch64
    /// `fcvt{n,a,m,p,z}{s,u}` and `fcvtxn`) read no mode, and x87 `fist`,
    /// `fistp` and `frndint` read the control word either way. Off, each
    /// reads the mode and selects among its four, so a mode set by
    /// `ldmxcsr`, `fldcw`, `mtfsfi`, `vmsr` or `ctc1` folds and an unknown
    /// one stays a select (an AArch64 FPCR read is volatile, so `msr fpcr`
    /// does not fold). Wrong after `fesetround` or a caller's mode change.
    /// Float arithmetic is taken to round to nearest either way, the
    /// conversions lifted as opaque user ops (packed VEX, EVEX, `roundss`,
    /// AArch64 vector and FP-register `#fbits` forms, ARM NEON and
    /// fixed-point forms, `vrint*`, `frint*`) are outside it, and
    /// MXCSR.FTZ/DAZ, PowerPC FPSCR.NI/OE/UE, ARM FPSCR.FZ, AArch64 FPCR.FZ
    /// and MIPS FCSR.FS are taken as 0, so `none()` is not a sound floor for
    /// `-ffast-math` code or code that sets those. Read by the lift, so `analyze` honours it and
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
    /// Nested claims about the analysed code, each one unchecked; the
    /// defaults when not passed.
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
            per_address_ccs: per_address_ccs.map(|t| PerAddressCcs {
                py: t.0,
                built: Mutex::default(),
            }),
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
