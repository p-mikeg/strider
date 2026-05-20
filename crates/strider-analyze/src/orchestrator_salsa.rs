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
// `BuiltFunctionGraph` does not implement `PartialEq` but as of Phase 7
// Task 7.1 it IS `Clone` (structural-copy clone of the sea-of-nodes
// arena — meaningfully cheaper than re-lifting).  Salsa needs the cache
// entry to be either `Eq` or marked `no_eq`.  We hold an `Arc<BfgEntry>`
// so the salsa cache and the driver share ownership without copying the
// graph until the driver actually needs an owned BFG (at which point it
// `clone()`s out of the entry).

/// Cache entry produced by [`optimized_function`].  Holds the lifted
/// BFG on success or a stringified error on failure.
///
/// Wrapped in `Arc` so salsa-side cloning is O(1); the driver clones the
/// inner BFG out by structural copy when it needs an owned value (see
/// [`run_v2`]).
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

    /// Builds the CFG (using the same configuration the build closure
    /// captures) and returns one entry per region:
    /// `(region_start_addr, region_content_fingerprint)`.
    ///
    /// The fingerprint hashes the region's pcode bytes plus its
    /// terminator kind so two regions with identical contents at the
    /// same start address produce the same fingerprint regardless of
    /// CFG-rebuild iteration.  Phase 7 Task 7.2 uses this as the
    /// granular cache key for per-region salsa queries:
    /// `region_lift_signature(db, region_addr, fingerprint)` only
    /// re-executes when `fingerprint` differs from the cached value.
    ///
    /// # Errors
    ///
    /// Surfaces any error from the underlying `cfg::Builder` or
    /// `Sleigh` construction in the build closure.
    fn cfg_region_signatures(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<Vec<(u64, u64)>>;

    /// Increments the query-invocation counter.  Called from inside
    /// the tracked function body so salsa cache hits are
    /// distinguishable from misses.
    fn record_optimized_call(&self);

    /// Returns the current invocation count (for testing).
    fn optimized_function_calls(&self) -> usize;

    /// Increments the per-region tracked-body counter.  Called from
    /// inside `region_lift_signature` so salsa cache hits on
    /// `(region_addr, fingerprint)` pairs are distinguishable from
    /// body executions.
    fn record_region_lift_call(&self);

    /// Returns the current per-region invocation count (for testing).
    fn region_lift_invocation_count(&self) -> usize;
}

/// Trait-object signature for the build closure.  Concrete impls
/// must support both the full `build` (BFG-producing) path and the
/// lighter-weight `region_signatures` (per-region fingerprint) path.
///
/// The two methods take the same `targets` map so the per-region
/// fingerprints reflect the CFG that `build` would produce for the
/// same input.  Implementors typically build the CFG twice (once for
/// signatures, once inside `build`); salsa's cache amortises this
/// because both calls memoise independently on their inputs.
pub trait RunBuilder: Send + Sync + 'static {
    fn build(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph>;

    /// Build the CFG and return per-region fingerprints.  See
    /// [`StriderDb::cfg_region_signatures`].
    fn region_signatures(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<Vec<(u64, u64)>>;
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
    /// Phase 7 Task 7.2 — per-region tracked-body counter.
    region_lift_calls: AtomicUsize,
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
            region_lift_calls: AtomicUsize::new(0),
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

    fn cfg_region_signatures(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<Vec<(u64, u64)>> {
        self.build.region_signatures(targets)
    }

    fn record_optimized_call(&self) {
        self.optimized_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn optimized_function_calls(&self) -> usize {
        self.optimized_calls.load(Ordering::SeqCst)
    }

    fn record_region_lift_call(&self) {
        self.region_lift_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn region_lift_invocation_count(&self) -> usize {
        self.region_lift_calls.load(Ordering::SeqCst)
    }
}

// ── Salsa interned region key ─────────────────────────────────────────────
//
// Phase 7 Task 7.2 — per-region tracked queries are keyed by an
// interned `RegionKey { addr, fingerprint }`.  Salsa interning
// guarantees that two `RegionKey::new(db, a, f)` calls with the same
// `(a, f)` return the same interned id, so the per-region tracked
// query memoises on the underlying content fingerprint.
//
// Why interned rather than tracked: the key is a pure value (two
// primitive `u64`s), not derived from another query.  Interned
// structs are the salsa idiom for "stable identity for a value
// tuple"; their identity is invariant across revisions, which is
// what we want for the per-region cache (a region's fingerprint
// doesn't depend on `IndirectTargets`'s current revision — it
// depends only on the region's bytes).

/// Salsa-interned per-region key.  Two calls to `RegionKey::new(db,
/// a, f)` with identical `(a, f)` return the same interned id, so
/// `region_lift_signature` memoises on content rather than on
/// invocation-order.
#[salsa::interned]
pub struct RegionKey<'db> {
    /// Region start address (machine, not pcode-index).
    pub addr: u64,
    /// Per-region content fingerprint produced by
    /// [`StriderDb::cfg_region_signatures`].  Two regions with the
    /// same `(addr, fingerprint)` are assumed to lift to identical
    /// per-region IR — the contract `cfg_region_signatures` must
    /// honour.
    pub fingerprint: u64,
}

// ── Tracked queries ────────────────────────────────────────────────────────

/// Per-region tracked query.  Phase 7 Task 7.2.
///
/// The body is a near-no-op that records a per-region cache-miss
/// counter.  Its **value** is the region fingerprint (returned
/// unchanged); the **dependency edge** it establishes is what
/// matters: every call site (currently `optimized_function`) becomes
/// dependent on this specific `(addr, fingerprint)` key, so when a
/// later salsa revision produces a CFG with the same per-region
/// fingerprints, the per-region cache hits and the counter does NOT
/// increment.
///
/// ## What it does NOT yet cache
///
/// The per-region IR itself.  The full IR lift remains monolithic in
/// v1's `run`.  Splitting v1's `Strider::analyze_cfg_with` into
/// per-region IR-producing queries is deferred to Phase 8 — the
/// cross-region phi joins make `combine_and_optimize(Vec<RegionIr>)`
/// non-trivial.  This delivery puts the salsa dep-graph in place so a
/// Phase 8 cache-promote can swap the returned `u64` for an
/// `Arc<RegionIrShard>` without changing the invalidation topology.
#[salsa::tracked]
pub fn region_lift_signature<'db>(
    db: &'db dyn StriderDb,
    region: RegionKey<'db>,
) -> u64 {
    db.record_region_lift_call();
    region.fingerprint(db)
}

/// Tracked wrapper around [`StriderDb::cfg_region_signatures`] so
/// the (expensive) CFG enumeration is cached across salsa revisions.
///
/// Without this wrapper, every `optimized_function` body invocation
/// would re-build the CFG just to enumerate fingerprints — pure
/// overhead on top of the lift that happens inside
/// `run_v1_with_targets`.  Caching the result behind a salsa-tracked
/// query means: identical `IndirectTargets` ⇒ identical
/// `cfg_region_signatures` result ⇒ no CFG rebuild on repeat queries.
///
/// Returns `Arc<Vec<(u64, u64)>>` because `Vec<...>` doesn't implement
/// `Update` cleanly for salsa's no-eq path.  Errors are stringified
/// (same convention as [`BfgEntry::result`]).
#[salsa::tracked(no_eq, returns(ref))]
fn region_signatures_query<'db>(
    db: &'db dyn StriderDb,
    targets: IndirectTargets,
) -> Arc<std::result::Result<Vec<(u64, u64)>, String>> {
    let map = targets.map(db);
    Arc::new(match db.cfg_region_signatures(map.as_ref()) {
        Ok(v) => Ok(v),
        Err(e) => Err(format!("{e:?}")),
    })
}

/// The top-level tracked query: lift + optimise the function for the
/// current `(Binary, IndirectTargets)` pair.
///
/// Phase 7 Task 7.2 — body now drives the per-region tracked
/// `region_lift_signature` queries before delegating to v1's `run`.
/// This wires the per-region dependency edges into salsa's
/// red-green algorithm: when `IndirectTargets` mutates, the
/// per-region queries cache-hit on every region whose content
/// fingerprint is unchanged (typically: every region in the function
/// when the new target is a no-op).
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

    // Phase 7 Task 7.2 — drive the per-region tracked queries.
    // `region_signatures_query` is a salsa-tracked wrapper around the
    // CFG-enumeration call, so the (expensive) CFG rebuild only
    // happens once per `IndirectTargets` revision.  Repeat queries
    // with identical inputs hit the cache and the body skips the
    // entire CFG path.
    //
    // Errors from the signatures query are NOT fatal here: a failure
    // to enumerate signatures means we've lost incremental
    // granularity for this revision, but the BFG can still be lifted
    // via v1's `run`.  We swallow the error after logging so the
    // orchestrator's parity contract holds even when (e.g.) the
    // binary fails to load on the signature path but succeeds on the
    // lift path — an unlikely but possible skew.
    let sigs_arc = region_signatures_query(db, targets).clone();
    match sigs_arc.as_ref() {
        Ok(sigs) => {
            for (addr, fp) in sigs {
                let key = RegionKey::new(db, *addr, *fp);
                let _ = region_lift_signature(db, key);
            }
        }
        Err(msg) => {
            eprintln!(
                "salsa optimized_function: cfg_region_signatures failed; \
                 proceeding without per-region cache for this revision: {msg}"
            );
        }
    }

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
                    // Clone the BFG out of the cache entry — this is
                    // a structural copy of the sea-of-nodes arena
                    // (Phase 7 Task 7.1: `BuiltFunctionGraph: Clone`),
                    // which is meaningfully cheaper than re-lifting
                    // from pcode.
                    return Ok(bfg.clone());
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
                    return Ok(bfg.clone());
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

    let builder = ElfRunBuilder {
        arch,
        strider,
        rom,
        fn_max_size,
        allow_code_before_start_addr,
        compact,
        per_address_ccs: Arc::new(per_address_ccs),
        reader_factory: Arc::new(reader_factory),
        start_addr,
    };

    Ok(StriderDbImpl::new(Box::new(builder)))
}

/// `RunBuilder` implementation that re-builds a Sleigh + CFG + IR
/// pipeline on every call from a captured arch / CC / reader factory.
///
/// Lives outside `make_db_for_elf` so we can implement both the
/// full-`build` and the lighter-weight `region_signatures` methods.
///
/// Generic over `R: MemReader + 'static` because the rsleigh trait has
/// an associated `Err` type that makes `dyn MemReader` impractical.
/// The struct is parameterised, then boxed as `Box<dyn RunBuilder>`
/// inside `StriderDbImpl` — the `RunBuilder` trait itself is
/// dyn-safe (no associated types, no generic methods).
struct ElfRunBuilder<R: rsleigh::MemReader, F: Fn() -> R + Send + Sync + 'static> {
    arch: target::SleighArch,
    strider: Arc<Strider>,
    rom: Option<Arc<dyn ReadOnlyMemory>>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    compact: bool,
    per_address_ccs: Arc<HashMap<u64, target::CallingConvention>>,
    reader_factory: Arc<F>,
    start_addr: u64,
}

impl<R, F> RunBuilder for ElfRunBuilder<R, F>
where
    R: rsleigh::MemReader + 'static,
    F: Fn() -> R + Send + Sync + 'static,
{
    fn build(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<BuiltFunctionGraph> {
        // For Phase 3.9 wrapper-mode: ignore `targets` and let v1's
        // internal fixed-point loop converge.  When Phase 8 splits the
        // lift, this body becomes thinner — just per-region calls
        // through salsa's `region_lift_signature` (Phase 7.2) plus a
        // monolithic optimizer post-pass.
        let _ = targets;
        let reader = (self.reader_factory)();
        let sleigh = rsleigh::Sleigh::new(self.arch.sla_spec(), self.arch.pspec(), reader)?;
        let config = RunConfig {
            strider: self.strider.as_ref(),
            start_addr: self.start_addr.into(),
            sleigh,
            rom: self.rom.clone(),
            fn_max_size: self.fn_max_size,
            allow_code_before_start_addr: self.allow_code_before_start_addr,
            compact: self.compact,
            per_address_ccs: (*self.per_address_ccs).clone(),
        };
        crate::orchestrator::run(config)
    }

    fn region_signatures(
        &self,
        targets: &BTreeMap<u64, BTreeSet<u64>>,
    ) -> Result<Vec<(u64, u64)>> {
        use cfg::{Builder, Cfg, OptionsBuilder};
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Build a fresh CFG with the supplied `known_targets`.  We do
        // this independently of `build` so we can call it from
        // `optimized_function` BEFORE invoking the full lift — the
        // per-region salsa cache hits on signature equality, which
        // gates whether downstream per-region IR (a Phase 8 deliverable)
        // gets invalidated.
        let reader = (self.reader_factory)();
        let sleigh = rsleigh::Sleigh::new(self.arch.sla_spec(), self.arch.pspec(), reader)?;

        let mut opts_builder = OptionsBuilder::new();
        if let Some(rom) = self.rom.clone() {
            opts_builder = opts_builder.set_read_only_memory(rom);
        }
        if let Some(lr) = self.strider.calling_convention().link_register_vn() {
            opts_builder = opts_builder.set_link_register(lr);
        }
        if let Some(max) = self.fn_max_size {
            opts_builder = opts_builder.set_function_max_size(max);
        }
        if self.allow_code_before_start_addr {
            opts_builder = opts_builder.allow_code_before_start_addr();
        }
        let cfg_opts = opts_builder.build();

        let resolver: Arc<dyn cfg::IndirectTargetResolver<R>> =
            Arc::new(crate::opt::indirect_resolver::MiniIrIndirectResolver);
        // Convert BTreeMap → HashMap with PcodeInsnAddr keys for
        // `with_known_targets`.  The map shape is BTreeMap<u64,
        // BTreeSet<u64>> (anchor pcode-addr → resolved target
        // addresses); we wrap as ResolvedTargets::Single when the set
        // is a singleton, otherwise Multiple.
        let known_targets: HashMap<cfg::PcodeInsnAddr, cfg::ResolvedTargets> = targets
            .iter()
            .map(|(addr, set)| {
                let resolved = if set.len() == 1 {
                    cfg::ResolvedTargets::Single(
                        *set.iter().next().expect("len==1 set has one"),
                    )
                } else {
                    cfg::ResolvedTargets::Multiple(set.iter().copied().collect())
                };
                (cfg::PcodeInsnAddr::at_machine_start(*addr), resolved)
            })
            .collect();

        let cfg: Cfg<R> = Builder::for_arch(
            &self.strider.arch,
            sleigh,
            self.start_addr,
            cfg_opts,
        )
        .with_known_targets(known_targets)
        .with_indirect_resolver(resolver)
        .build()?;

        // Per-region fingerprint: hash the region's start address,
        // terminator kind, and per-instruction pcode bytes.  Two
        // regions with identical content at the same address produce
        // the same fingerprint — the contract `region_lift_signature`
        // assumes.
        let mut out: Vec<(u64, u64)> = Vec::new();
        for region in cfg.regions() {
            let mut hasher = DefaultHasher::new();
            region.start_addr.machine_addr_u64().hash(&mut hasher);
            // Terminator kind discriminates regions that share bytes
            // but differ in how they end (e.g. Fallthrough vs
            // TailCall after a CondBranch-OOB collapse).
            std::mem::discriminant(&region.terminator).hash(&mut hasher);
            for wrapped in &region.insns {
                wrapped.addr.machine_addr_u64().hash(&mut hasher);
                // `rsleigh::Insn` does not implement Hash; hash its
                // Debug form as a stable surrogate.  The format is
                // deterministic for a given opcode + varnodes tuple.
                format!("{:?}", wrapped.insn).hash(&mut hasher);
            }
            let fp = hasher.finish();
            out.push((region.start_addr.machine_addr_u64(), fp));
        }
        Ok(out)
    }
}
