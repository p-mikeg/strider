//! Incremental-friendly facade over the CFG builder + DecodeCache.
//!
//! Thin wrapper: the underlying CFG builder already does on-demand
//! reachable-cached lifting.  The Lifter exposes a cleaner per-region
//! API surface for callers that want to drive lifting region-by-region
//! without owning the full Builder.
//!
//! # Design
//!
//! The Lifter owns a `Sleigh` handle, a [`DecodeCache`], and a lazily
//! built [`Cfg`].  The first call to [`Lifter::region`] triggers a
//! full BFS-from-entry CFG build; subsequent calls return a memoized
//! `Arc<Region>` for the region containing the requested address.  The
//! `Arc::ptr_eq` invariant lets downstream incremental queries cheaply
//! detect "this region is the same one I saw last query".
//!
//! Per-instruction decode work is paid at most once thanks to the
//! `DecodeCache` plumbed into [`crate::cfg::Builder::with_decode_cache`].
//! The [`DecodeStats`] returned by [`Lifter::decode_stats`] surfaces
//! `(unique_addresses, total_lift_calls)` so tests can pin "no
//! address decoded more than once" after a CFG build.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::cfg::{
    Builder, Cfg, DecodeCache, FunctionBoundary, OptionsBuilder, PcodeInsnAddr, Region,
};
use strider_target::{BuiltCallingConvention, SleighArch};

/// Diagnostic snapshot of the underlying [`DecodeCache`].
///
/// `unique_addresses` is the number of distinct machine addresses
/// the cache currently holds (one entry per decoded native
/// instruction).  `total_lift_calls` is the running count of `get`
/// lookups served — both cache hits and cache misses.  After one
/// `Lifter::region` BFS build, `unique_addresses == total_lift_calls`
/// holds (every reachable address is decoded exactly once).
#[derive(Debug, Default, Clone, Copy)]
pub struct DecodeStats {
    /// Number of distinct machine addresses currently in the cache.
    pub unique_addresses: usize,
    /// Total number of cache lookups (hits + misses) since the cache
    /// was created.
    pub total_lift_calls: usize,
}

/// Incremental-friendly per-region facade over the CFG builder.
///
/// Construction wires the Sleigh handle, calling convention, and
/// entry address but does not yet build the CFG.  The first call to
/// [`Self::region`] triggers a single BFS-from-entry build through
/// `cfg::Builder`, populating an internal [`Cfg`] and the
/// [`DecodeCache`].  Subsequent calls reuse the built CFG.
///
/// The Lifter is intentionally `!Sync` (and not `Clone`): it owns a
/// `Sleigh<R>` handle, which is a `&mut self`-only state machine.
pub struct Lifter<R: rsleigh::MemReader> {
    /// Lazy CFG; `None` until the first `region()` call.
    cfg: Option<Cfg<R>>,
    /// Sleigh handle moved into the `cfg::Builder` on first build.
    /// Wrapped in `Option` so we can move it out in the `&mut self`
    /// `region()` call.
    sleigh: Option<rsleigh::Sleigh<R>>,
    /// Architecture description (sla spec + pspec + endianness +
    /// preset).  Kept so we can rebuild the cfg::Builder with
    /// `for_arch(...)` on demand.
    arch: SleighArch,
    /// Resolved calling convention.  Owns the link-register varnode
    /// that the cfg builder threads into the indirect-branch
    /// resolver.
    cc: BuiltCallingConvention,
    /// Function entry-point virtual address.
    entry_addr: u64,
    /// Shared decode cache.  Threaded into the `cfg::Builder` via
    /// `with_decode_cache`; read by `decode_stats()` for per-region
    /// reuse diagnostics.
    decode_cache: DecodeCache,
    /// Per-address `Arc<Region>` memoization.  Populated lazily on
    /// `region()` calls so `Arc::ptr_eq` holds across repeated
    /// queries for the same address.
    region_cache: BTreeMap<PcodeInsnAddr, Arc<Region>>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    /// Construct a new Lifter for `entry_addr` on `arch` / `cc`.
    ///
    /// The CFG is not yet built — the first [`Self::region`] call
    /// triggers a BFS-from-entry build via `cfg::Builder::for_arch`.
    ///
    /// # Errors
    /// Returns an error if the underlying `rsleigh::Sleigh::new`
    /// call fails (e.g. invalid SLA spec or memory reader).
    pub fn new(
        reader: R,
        arch: SleighArch,
        cc: BuiltCallingConvention,
        entry_addr: u64,
    ) -> anyhow::Result<Self> {
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .map_err(|e| anyhow::anyhow!("Sleigh::new failed: {e:?}"))?;
        Ok(Self {
            cfg: None,
            sleigh: Some(sleigh),
            arch,
            cc,
            entry_addr,
            decode_cache: DecodeCache::new(),
            region_cache: BTreeMap::new(),
        })
    }

    /// Returns the [`Region`] containing `addr`, lifting on demand.
    ///
    /// The first call builds the full CFG (BFS-from-entry).
    /// Subsequent calls return the memoized `Arc<Region>` for the
    /// region containing `addr`; `Arc::ptr_eq` holds across repeated
    /// queries for the same address.
    ///
    /// # Errors
    /// Returns an error if the CFG build fails, or if `addr` does
    /// not land within any region in the built CFG.
    pub fn region(&mut self, addr: u64) -> anyhow::Result<Arc<Region>> {
        self.ensure_built()?;
        let pcode_addr = PcodeInsnAddr::at_machine_start(addr);
        let cfg = self
            .cfg
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Lifter::region: CFG not built after ensure_built"))?;
        // Normalize the query: find the region the address lands in
        // and key the memoization on its actual start address.  This
        // makes the Arc-identity invariant `lifter.region(start) ==
        // lifter.region(addr_inside_same_region)` hold for free.
        let (start_addr, region_id) = lookup_region_id(cfg, pcode_addr)?;
        if let Some(existing) = self.region_cache.get(&start_addr) {
            return Ok(Arc::clone(existing));
        }
        let region = cfg
            .graph()
            .node_weight(region_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Lifter::region: region_id {region_id:?} resolved but \
                     petgraph node_weight returned None"
                )
            })?
            .clone();
        let arc = Arc::new(region);
        self.region_cache.insert(start_addr, Arc::clone(&arc));
        Ok(arc)
    }

    /// Returns the set of region start addresses in the built CFG.
    ///
    /// Triggers a CFG build if one has not yet occurred.  Useful for
    /// Salsa-side enumeration and for tests that need to discover a
    /// non-entry region without hard-coding addresses.
    ///
    /// # Errors
    /// Returns an error if the CFG build fails.
    pub fn region_starts(
        &mut self,
    ) -> std::vec::IntoIter<PcodeInsnAddr> {
        // We deliberately swallow the build error here: callers that
        // care about build failure use `region()` first.  The
        // documented return shape is a plain iterator.  If the build
        // failed, the iterator is empty.
        if self.ensure_built().is_err() {
            return Vec::new().into_iter();
        }
        match &self.cfg {
            Some(cfg) => {
                let starts: Vec<PcodeInsnAddr> =
                    cfg.start_addr_to_region_id.keys().copied().collect();
                starts.into_iter()
            }
            None => Vec::new().into_iter(),
        }
    }

    /// Returns the current [`DecodeStats`] snapshot.
    #[must_use]
    pub fn decode_stats(&self) -> DecodeStats {
        DecodeStats {
            unique_addresses: self.decode_cache.unique_addresses(),
            total_lift_calls: self.decode_cache.total_lift_calls(),
        }
    }

    /// Build the CFG if it has not been built yet.
    ///
    /// Moves the Sleigh out of `self.sleigh` into the `cfg::Builder`
    /// and stashes the resulting `Cfg` in `self.cfg`.  Subsequent
    /// calls are no-ops.
    fn ensure_built(&mut self) -> anyhow::Result<()> {
        if self.cfg.is_some() {
            return Ok(());
        }
        let sleigh = self.sleigh.take().ok_or_else(|| {
            anyhow::anyhow!("Lifter::ensure_built: Sleigh already consumed")
        })?;
        let mut opts_b = OptionsBuilder::new()
            // Unbounded with `allow_code_before_start_addr = true` matches the
            // permissive lifter used by the strider analyze() harness for
            // pre-built fixtures.  Salsa callers that need a stricter
            // boundary can swap to an enriched constructor later.
            .set_function_boundary(FunctionBoundary::Unbounded {
                allow_code_before_start: true,
            });
        if let Some(lr) = self.cc.link_register_vn() {
            opts_b = opts_b.set_link_register(lr);
        }
        let opts = opts_b.build();
        let cfg = Builder::for_arch(&self.arch, sleigh, self.entry_addr, opts)
            .with_decode_cache(self.decode_cache.clone())
            .build()
            .map_err(|e| anyhow::anyhow!("Lifter::ensure_built: cfg::Builder::build: {e:?}"))?;
        self.cfg = Some(cfg);
        Ok(())
    }
}

/// Locate the region containing `pcode_addr` in `cfg`.  Returns the
/// region's start address (for cache keying) and its `NodeIndex`.
fn lookup_region_id<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    pcode_addr: PcodeInsnAddr,
) -> anyhow::Result<(PcodeInsnAddr, petgraph::graph::NodeIndex)> {
    // `Cfg::start_addr_to_region_id` is sorted by `PcodeInsnAddr`;
    // the region containing `pcode_addr` is the last entry whose
    // start <= pcode_addr (mirrors
    // `cfg::Builder::find_region_containing_addr`).
    let (&start, &region_id) = cfg
        .start_addr_to_region_id
        .range(..=pcode_addr)
        .next_back()
        .ok_or_else(|| {
            anyhow::anyhow!("Lifter::region: no region with start <= {pcode_addr:?}")
        })?;
    let region = cfg.graph().node_weight(region_id).ok_or_else(|| {
        anyhow::anyhow!(
            "Lifter::region: start_addr_to_region_id pointed at \
             region_id {region_id:?} but petgraph has no such node"
        )
    })?;
    if !region.contains_addr(pcode_addr) {
        anyhow::bail!(
            "Lifter::region: address {pcode_addr:?} falls between \
             regions (last region starting <= it does not contain it)"
        );
    }
    Ok((start, region_id))
}
