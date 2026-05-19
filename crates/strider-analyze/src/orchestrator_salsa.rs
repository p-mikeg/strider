//! Salsa-based orchestrator.  Phase 3 Task 3.9.
//!
//! Replaces v1's `RegionLiftHandles` + `RegionIndex` snapshot via an
//! incremental query graph (audit finding G8): the indirect-branch
//! fixed-point loop becomes a sequence of input mutations + query
//! re-evaluations against a `salsa::Database`, with salsa tracking
//! per-input dependency edges so only the affected derivations are
//! recomputed.
//!
//! ## Query graph
//!
//! Inputs (mutable):
//! - [`Binary`] (`Durability::HIGH`) — the binary identity (key) used
//!   to memoise the v1 build.  Set once per analysis; stable across
//!   the entire fixed-point loop.
//! - [`IndirectTargets`] (`Durability::LOW`) — the
//!   `BTreeMap<u64, BTreeSet<u64>>` of resolved indirect-branch
//!   targets discovered so far.  Grows monotonically as the driver
//!   classifies more anchors.
//!
//! Tracked (derived):
//! - [`optimized_function`] — runs the full lift + stable + destructive
//!   optimizer pipeline for the current `(Binary, IndirectTargets)`
//!   pair and returns an `Arc<BfgEntry>`.  Cached across queries with
//!   identical inputs.  Marked `no_eq` because `BuiltFunctionGraph`
//!   has no equality (interned arenas, side-tables).
//!
//! ## Driver
//!
//! [`run_v2`] sits outside salsa and orchestrates the fixed-point
//! externally (per V2 verification — no `cycle_fn`):
//!
//! 1. Query `optimized_function`.
//! 2. Walk the returned BFG for unresolved `IndirectBranch` placeholders.
//! 3. Classify each via the existing resolver helpers.
//! 4. For any new resolutions, mutate the `IndirectTargets` input and
//!    loop.  Salsa invalidates `optimized_function` on the next call.
//! 5. Stop when classification produces no new targets.
//!
//! ## Scope (Phase 3 Task 3.9)
//!
//! This delivery wires the **wrapper-level** memoization: a repeat call
//! with the same `IndirectTargets` map returns the cached BFG with NO
//! lift / opt-pipeline work.  True region-level incrementality (a
//! single indirect-target addition only re-lifts the affected regions)
//! requires splitting the lift into per-region tracked queries and is
//! left to Phase 6.

#![allow(clippy::missing_errors_doc)] // internal driver, surfaces upstream anyhow

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, anyhow};

use crate::orchestrator::RunConfig;
use crate::strider::Strider;
use crate::opt::ReadOnlyMemory;
use ir::BuiltFunctionGraph;

// ── Salsa cache entry ──────────────────────────────────────────────────────
//
// `BuiltFunctionGraph` does not implement `PartialEq` or `Clone` and
// owns sea-of-nodes arena state.  Salsa needs the cache entry to be
// either `Eq` or marked `no_eq`.  We hold an `Arc<BfgEntry>` where the
// entry stores either the BFG or an error message (we can't return
// `Result` directly because the tracked function body's return type
// must be `Update`).

/// Cache entry produced by [`optimized_function`].  Holds the lifted
/// BFG on success or a stringified error on failure.
///
/// Wrapped in `Arc` so cloning is O(1); the salsa cache and the driver
/// share ownership.
pub struct BfgEntry {
    /// `Ok(bfg)` on a successful lift; `Err(message)` when v1's build
    /// closure failed.  We can't carry an `anyhow::Error` across the
    /// salsa boundary (not `'static` in general after `chain`), so we
    /// stringify at the salsa edge.
    pub result: std::result::Result<BuiltFunctionGraph, String>,
}

// SAFETY: We always replace the old value.  Salsa's `maybe_update`
// contract permits this: returning `true` means "the value may have
// changed" and salsa will invalidate dependents accordingly.  Since the
// new entry is moved into place and the old is dropped via standard
// drop glue, there is no aliasing or use-after-free risk.
//
// We combine this with `#[salsa::tracked(no_eq)]` so salsa skips the
// would-be-Eq comparison entirely.  `Update` is still required.
unsafe impl salsa::Update for BfgEntry {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        // SAFETY: `old_pointer` is a valid `&mut Self` from salsa.
        unsafe { *old_pointer = new_value };
        true
    }
}

// ── Salsa inputs ───────────────────────────────────────────────────────────

/// Salsa input wrapping the binary identity under analysis.  Marked
/// `Durability::HIGH` at construction: it never changes during the
/// fixed-point loop.
#[salsa::input]
pub struct Binary {
    /// Stable opaque identifier (e.g. canonicalised path) used by
    /// the database to look up the actual loaded ELF + Sleigh + Strider
    /// configuration.  Salsa requires `Eq + Hash` on input fields; a
    /// `String` is simpler than carrying the bytes themselves.
    #[returns(ref)]
    pub key: String,
}

/// Encoded form of `BTreeMap<u64, BTreeSet<u64>>` that is `Eq + Hash`.
/// `Arc`-wrapped for cheap cloning.
pub type IndirectTargetsMap = Arc<BTreeMap<u64, BTreeSet<u64>>>;

/// Salsa input carrying the current set of resolved indirect-branch
/// targets.  `Durability::LOW` — grows during the fixed-point loop.
#[salsa::input]
pub struct IndirectTargets {
    #[returns(ref)]
    pub map: IndirectTargetsMap,
}

// ── Database trait + impl ──────────────────────────────────────────────────

/// Strider-specific database trait.  Tracked queries are dispatched
/// against `dyn StriderDb`; concrete databases (here:
/// [`StriderDbImpl`]) implement this to provide access to the
/// non-salsa-input configuration (Sleigh, Strider, ROM, options).
///
/// The configuration is held *outside* salsa's revisioned state because
/// it is build-time-only (a fresh `StriderDb` is constructed per
/// analysis) and not all of it implements the traits salsa requires of
/// inputs (`Eq + Hash + 'static`).
#[salsa::db]
pub trait StriderDb: salsa::Database {
    /// Runs v1's [`crate::orchestrator::run`] using the supplied
    /// indirect-target map as the initial `known_targets`.  This is the
    /// closure salsa invokes inside the tracked `optimized_function`
    /// body.
    ///
    /// Counted via [`Self::record_optimized_call`] so Step C's
    /// incremental test can observe re-run frequency.
    fn run_v1_with_targets(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph>;

    /// Increments the query-invocation counter.  Called from inside
    /// the tracked function body so salsa cache hits are
    /// distinguishable from misses.
    fn record_optimized_call(&self);

    /// Returns the current invocation count (for testing).
    fn optimized_function_calls(&self) -> usize;
}

/// Trait-object signature for the build closure.  Implemented for any
/// suitable `Fn(...)` by the blanket impl below.
pub trait RunBuilder: Send + Sync + 'static {
    fn build(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph>;
}

impl<F> RunBuilder for F
where
    F: Fn(&BTreeMap<u64, BTreeSet<u64>>) -> Result<BuiltFunctionGraph>
        + Send
        + Sync
        + 'static,
{
    fn build(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph> {
        (self)(targets)
    }
}

/// Concrete strider database.  Owns the salsa storage plus the
/// per-analysis configuration (the build closure that drives v1's
/// `run`).
#[salsa::db]
pub struct StriderDbImpl {
    storage: salsa::Storage<Self>,
    /// Build closure: takes the current indirect-targets map and
    /// produces a freshly-lifted, fully-optimised BFG.  Encapsulates
    /// the per-analysis Strider / Sleigh / ROM / options that don't
    /// fit into salsa's input model.
    ///
    /// The closure must be re-runnable: salsa may call it many times
    /// across the fixed-point loop.  The implementation handles this by
    /// constructing a fresh `Sleigh<R>` each call from a captured
    /// SLA-spec + ELF byte slice + reader factory.
    build: Box<dyn RunBuilder>,
    /// Diagnostic counter — incremented by `record_optimized_call`.
    optimized_calls: AtomicUsize,
}

impl StriderDbImpl {
    /// Construct a new database with the supplied per-analysis build
    /// closure.  The closure is invoked once per `(Binary,
    /// IndirectTargets)` pair where the cache misses.
    pub fn new(build: Box<dyn RunBuilder>) -> Self {
        Self {
            storage: salsa::Storage::new(None),
            build,
            optimized_calls: AtomicUsize::new(0),
        }
    }
}

#[salsa::db]
impl salsa::Database for StriderDbImpl {}

#[salsa::db]
impl StriderDb for StriderDbImpl {
    fn run_v1_with_targets(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph> {
        self.build.build(targets)
    }

    fn record_optimized_call(&self) {
        self.optimized_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn optimized_function_calls(&self) -> usize {
        self.optimized_calls.load(Ordering::SeqCst)
    }
}

// ── Tracked query ──────────────────────────────────────────────────────────

/// The single top-level tracked query: lift + optimise the function
/// for the current `(Binary, IndirectTargets)` pair.
///
/// Salsa caches the returned `Arc<BfgEntry>` against the input pair.
/// A repeat call with identical inputs returns the cached value
/// without re-running the closure (and without incrementing the
/// `record_optimized_call` counter).
///
/// Marked `no_eq` because `BuiltFunctionGraph` doesn't implement
/// `PartialEq`.  This means salsa never compares old vs new values —
/// any rerun marks the result as "may have changed" and invalidates
/// any downstream queries.  Acceptable since this is the top-level
/// query.
#[salsa::tracked(no_eq, returns(ref))]
pub fn optimized_function<'db>(
    db: &'db dyn StriderDb,
    _binary: Binary,
    targets: IndirectTargets,
) -> Arc<BfgEntry> {
    db.record_optimized_call();
    let map = targets.map(db);
    let result = match db.run_v1_with_targets(map.as_ref()) {
        Ok(bfg) => Ok(bfg),
        Err(e) => Err(format!("{e:?}")),
    };
    Arc::new(BfgEntry { result })
}

// ── External fixed-point driver ────────────────────────────────────────────

/// Cap on outer fixed-point iterations.  Mirrors v1's
/// `2 * pending_at_iter_0 + 4` but with a static ceiling; salsa's
/// dependency tracking handles the actual incremental work.
const SALSA_OUTER_CAP: usize = 64;

/// Run the salsa-driven fixed-point loop and return an owned BFG.
///
/// `binary_key` is a stable identifier used only for cache keying —
/// the actual binary state is captured inside the `db`'s build closure.
///
/// # Errors
///
/// Surfaces any error from v1's `run` via the build closure, or
/// `bail!`s if the loop exceeds [`SALSA_OUTER_CAP`] iterations.
///
/// ## Phase 3.9 wrapper-mode note
///
/// v1's internal `run` already drives its own indirect-branch
/// fixed-point.  In this wrapper-mode delivery we delegate the entire
/// inner loop to v1; the outer salsa driver typically terminates after
/// one query call (no new external resolutions to add).  The salsa
/// scaffolding is in place so Phase 6 can split `run_v1_with_targets`
/// into per-region tracked queries — the external loop body stays
/// unchanged.
pub fn run_v2(
    db: &mut StriderDbImpl,
    binary_key: &str,
) -> Result<BuiltFunctionGraph> {
    use salsa::Setter;

    let binary = Binary::new(db, binary_key.to_string());
    let initial: IndirectTargetsMap = Arc::new(BTreeMap::new());
    let targets = IndirectTargets::new(db, initial);

    let mut current_map: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();

    for _iter in 0..SALSA_OUTER_CAP {
        // Query into salsa; this either hits the cache or runs the
        // tracked body which calls the v1 build closure.  Returns
        // `&Arc<BfgEntry>` — borrow lives only as long as `db` is not
        // mutated.
        let entry: Arc<BfgEntry> = optimized_function(db, binary, targets).clone();
        match &entry.result {
            Ok(bfg) => {
                let unresolved = collect_unresolved(bfg);
                let new_resolutions = classify_unresolved_external(bfg, &unresolved);
                if new_resolutions.is_empty() {
                    // No external progress; v1 has done all it can.
                    // Drop the cache entry's Arc reference and re-run
                    // v1 directly to materialise an owned BFG (BFG is
                    // not Clone, and salsa holds the cache copy).
                    drop(entry);
                    return db.run_v1_with_targets(&current_map);
                }
                let mut next_map = current_map.clone();
                let mut grew = false;
                for (addr, targets_set) in new_resolutions {
                    let bucket = next_map.entry(addr).or_default();
                    for t in targets_set {
                        if bucket.insert(t) {
                            grew = true;
                        }
                    }
                }
                if !grew {
                    drop(entry);
                    return db.run_v1_with_targets(&current_map);
                }
                current_map = next_map;
                targets.set_map(db).to(Arc::new(current_map.clone()));
            }
            Err(msg) => {
                let msg = msg.clone();
                return Err(anyhow!("salsa orchestrator: v1 run failed: {msg}"));
            }
        }
    }
    anyhow::bail!("salsa orchestrator: exceeded {SALSA_OUTER_CAP} outer iterations")
}

/// Collect the unresolved `IndirectBranch` placeholders in the BFG.
/// Walks the graph and returns every reachable `IndirectBranch` node's
/// `(machine_addr, node_id)`.  Phase 3.9 wrapper-mode: v1's internal
/// `run` already resolves these, so an `IndirectBranch` remaining at
/// this layer is a genuinely-unresolved anchor.
fn collect_unresolved(bfg: &BuiltFunctionGraph) -> Vec<(u64, ir::node::NodeId)> {
    use ir::node::NodeKind;
    let mut out = Vec::new();
    for nid in bfg.graph.all_node_ids() {
        if matches!(bfg.graph.node_kind(nid), NodeKind::IndirectBranch) {
            // Use the asm-fingerprint as a stable anchor address.  An
            // IndirectBranch is lifted from one machine instruction
            // (the trailing branch) so its fingerprint has at least
            // one entry; pick the smallest as the canonical addr.
            let addr = bfg
                .graph
                .asm_fingerprint(nid)
                .iter()
                .copied()
                .min()
                .unwrap_or(0);
            out.push((addr, nid));
        }
    }
    out
}

/// External classification: walks each unresolved anchor and tries to
/// derive new target addresses.  Used by the driver to feed salsa's
/// `IndirectTargets` input.
///
/// Phase 3.9 wrapper notes:
/// - v1's `run` already runs its own internal fixed-point — by the
///   time the salsa wrapper sees the BFG, every anchor v1 can resolve
///   IS resolved.  So in the wrapper-mode 3.9 scope, this function
///   returns an empty Vec and the driver terminates after one outer
///   iteration.
/// - When Phase 6 splits the lift into per-region salsa queries, this
///   function is where the external classifier replaces v1's internal
///   loop — same call site, different implementation.
fn classify_unresolved_external(
    _bfg: &BuiltFunctionGraph,
    _unresolved: &[(u64, ir::node::NodeId)],
) -> Vec<(u64, BTreeSet<u64>)> {
    Vec::new()
}

// ── Helper: convenience constructor for the build closure ──────────────────

/// Build a [`StriderDbImpl`] whose `run_v1_with_targets` closure drives
/// v1's [`crate::orchestrator::run`] against a captured arch / CC /
/// reader configuration.
///
/// Captures `'static` data only so the resulting closure is
/// `Send + Sync + 'static` and fits the [`RunBuilder`] bound.
///
/// `reader_factory` produces a fresh `R: MemReader + 'static` per
/// invocation — v1's `RunConfig` consumes the Sleigh, so we re-build
/// it on every closure call.
///
/// # Errors
///
/// Returns an error if [`Strider::new`] fails (e.g. an unresolvable
/// register name in the chosen calling convention).
pub fn make_db_for_elf<R, F>(
    arch: target::SleighArch,
    cc: target::CallingConvention,
    reader_factory: F,
    start_addr: u64,
    rom: Option<Arc<dyn ReadOnlyMemory>>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    compact: bool,
    per_address_ccs: HashMap<u64, target::CallingConvention>,
) -> Result<StriderDbImpl>
where
    R: rsleigh::MemReader + 'static,
    F: Fn() -> R + Send + Sync + 'static,
{
    let regs = arch.probe_regs()?;
    let strider = Arc::new(Strider::new(arch, regs, cc)?);
    let rom_arc = rom;
    let per_address_ccs_arc = Arc::new(per_address_ccs);
    let reader_factory_arc = Arc::new(reader_factory);

    let build = Box::new(
        move |targets: &BTreeMap<u64, BTreeSet<u64>>| -> Result<BuiltFunctionGraph> {
            // For Phase 3.9 wrapper-mode: ignore `targets` and let
            // v1's internal fixed-point loop converge.  When Phase 6
            // splits the lift, this closure becomes thinner — just
            // region lift for the regions whose dep on `targets`
            // changed.
            let _ = targets;
            let reader = (reader_factory_arc)();
            let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)?;
            let config = RunConfig {
                strider: strider.as_ref(),
                start_addr: start_addr.into(),
                sleigh,
                rom: rom_arc.clone(),
                fn_max_size,
                allow_code_before_start_addr,
                compact,
                per_address_ccs: (*per_address_ccs_arc).clone(),
            };
            crate::orchestrator::run(config)
        },
    );

    Ok(StriderDbImpl::new(build))
}
