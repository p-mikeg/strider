//! Pass-scoped SP-aliasing façade.  [`SpAliasCfg`] bundles the shared
//! SP-expression memo and the alias knobs, built once per pass and reused for
//! every query; per query it builds the [`SpAliasOracle`] that drives the
//! [`super::mem_ssa`] memory-SSA walk.  The address-class verdict logic it
//! consults lives in the sibling [`super::alias`] module.

use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use super::alias::{AddrClass, AliasVerdict, alias_verdict, classify_addr, store_alias_verdict};
use super::decompose::{SpDecomposer, SpExpr, SpExprMemo};
use super::mem_ssa::MemorySSAWalker;
use super::ranges::store_value_byte_size;
use crate::{AliasMode, MemAliasOptions};

/// The single SP-aware [`MemorySSAWalker`] oracle, shared by `load_forward`
/// (store-to-load forwarding) and `function_args` (stack-arg shadow walk).
///
/// `def_clobbers` answers "does this memory def overlap the load's byte
/// range?" for a precomputed load address class:
///
/// * `Store` — via [`store_alias_verdict`]: anything but `Disjoint`
///   clobbers (a `load_forward` caller re-checks exact-`Match` afterward).
/// * `Call` / `CallOther` — clobbers iff `mem.calls_clobber` is set.
///   `load_forward` sets it (a load can never forward across a call);
///   `function_args` passes its `calls_clobber` knob (off by default — the
///   callee is opaque, so there is nothing to inspect).
/// * any other (opaque) memory producer — conservatively clobbers.
///
/// `MemPhi` is handled structurally by the walk, so the oracle never
/// sees one.
struct SpAliasOracle<'a, 'm> {
    /// The owning config — the source of the shared `sp_memo` + alias knobs,
    /// so the oracle holds only the per-query load facts below and reads the
    /// rest through `cfg` instead of copying them.
    cfg: &'a mut SpAliasCfg<'m>,
    /// The load's address class (`SpRooted` for a stack-arg load; any class
    /// for a general forwarded load).
    load_class: AddrClass,
    load_size: i64,
    /// The load's address space.  A store in a DIFFERENT `VnSpace` cannot
    /// clobber (or be forwarded into) this load — distinct spaces never alias,
    /// even at the same numeric address.
    load_space: rsleigh::VnSpace,
}

impl super::mem_ssa::MemorySSAWalker for SpAliasOracle<'_, '_> {
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        match *function.node_kind(def) {
            // A store in a different address space than the load cannot clobber
            // it — distinct `VnSpace`s (RAM / register / unique / const) never
            // alias.  Treating it as non-clobbering lets the walk continue to a
            // same-space def instead of falsely stopping here (and, in
            // `load_forward`, forwarding a different-space store's value into
            // the load — a miscompile).
            NodeKind::Store(store_space) => {
                store_space == self.load_space
                    && store_alias_verdict(
                        function,
                        def,
                        self.load_class,
                        self.load_size,
                        &mut *self.cfg.sp_memo,
                        self.cfg.alias_mode,
                        self.cfg.mem.assume_distinct_sp_bases_disjoint,
                    ) != AliasVerdict::Disjoint
            }
            NodeKind::Call | NodeKind::CallOther { .. } => self.cfg.mem.calls_clobber,
            // Any other (opaque) memory producer cannot be proven disjoint.
            _ => true,
        }
    }
}

/// Pass-scoped SP-aliasing context: the shared `SpExprMemo` plus the alias
/// knobs, built once per pass and reused for every query.  Bundles the data
/// that used to be threaded through `reaching_sp_store`'s 9-arg signature and
/// the inline `SpAliasOracle` builds at each `nearest_clobber` call site.
pub(crate) struct SpAliasCfg<'m> {
    sp_memo: &'m mut SpExprMemo,
    alias_mode: AliasMode,
    mem: MemAliasOptions,
}

impl<'m> SpAliasCfg<'m> {
    pub(crate) fn new(
        sp_memo: &'m mut SpExprMemo,
        alias_mode: AliasMode,
        mem: MemAliasOptions,
    ) -> Self {
        Self {
            sp_memo,
            alias_mode,
            mem,
        }
    }

    /// Config for the call-blocking consumers (load-forward, call-stack-arg
    /// collection, stack-array jump tables): a `Call` on the memory chain
    /// clobbers the probed location (`calls_clobber: true`) and distinct SP
    /// bases stay conservatively non-disjoint
    /// (`assume_distinct_sp_bases_disjoint: false`).
    pub(crate) fn call_blocking(sp_memo: &'m mut SpExprMemo, alias_mode: AliasMode) -> Self {
        Self::new(
            sp_memo,
            alias_mode,
            MemAliasOptions {
                calls_clobber: true,
                assume_distinct_sp_bases_disjoint: false,
            },
        )
    }

    /// Build the per-query oracle borrowing this config (the source of the
    /// shared memo + knobs) plus the load's address class, size, and space.
    fn oracle(
        &mut self,
        load_class: AddrClass,
        load_size: i64,
        load_space: rsleigh::VnSpace,
    ) -> SpAliasOracle<'_, 'm> {
        SpAliasOracle {
            cfg: self,
            load_class,
            load_size,
            load_space,
        }
    }

    /// Classify a load/store address under this config's memo.
    pub(crate) fn classify_addr(&mut self, function: &Function, addr: ValueId) -> AddrClass {
        classify_addr(function, addr, self.sp_memo)
    }

    /// Decompose an address into an SP terminal through this config's shared
    /// memo.  The single decompose entry for consumers, so they no longer
    /// materialise a transient [`SpDecomposer`] at each call site.
    pub(crate) fn decompose(&mut self, function: &Function, value: ValueId) -> Option<SpExpr> {
        SpDecomposer::new(function, self.sp_memo).decompose(value)
    }

    /// Exact pairwise alias verdict between a `Load` and a `Store`, deriving
    /// each side's address class + byte size from the node itself (O(1) cached
    /// reads / decompose-memo hits) under this config's alias mode and
    /// distinct-base knob.  The node-based counterpart of the class-based
    /// [`alias_verdict`] primitive.
    pub(crate) fn verdict(
        &mut self,
        function: &Function,
        load_node: NodeId,
        store_node: NodeId,
    ) -> AliasVerdict {
        let load_class = self.classify_addr(function, function.load_addr(load_node));
        let [load_out] = function
            .node_outputs_exact::<1>(load_node)
            .expect("Load has 1 output per node signature");
        let load_size = function
            .value_type_opt(load_out)
            .expect("Load output is a value")
            .byte_size() as i64;
        let store_class = self.classify_addr(function, function.store_addr(store_node));
        let store_size = store_value_byte_size(function.graph(), function.store_data(store_node));
        alias_verdict(
            load_class,
            load_size,
            store_class,
            store_size,
            self.alias_mode,
            self.mem.assume_distinct_sp_bases_disjoint,
        )
    }

    /// Read-only walk: the nearest clobber of `load` reachable backward from
    /// the def producing the `mem` memory token.  The load's address class,
    /// byte size, and space are derived from the `load` node itself — each an
    /// O(1) cached read (the SP decompose is a memo hit when the caller has
    /// already classified the address), so a caller never threads them in.
    /// Performs no narrowing; a caller that wants to shorten the load's memory
    /// edge onto the returned clobber calls [`super::narrow_load_to`].
    pub(crate) fn nearest_clobber(
        &mut self,
        function: &Function,
        load: NodeId,
        mem: ValueId,
    ) -> NodeId {
        // The load's own space scopes which stores can clobber it.  Production
        // callers only ever pass a `Load`; RAM is a safe default.
        let load_space = match function.node_kind(load) {
            NodeKind::Load(s) => *s,
            _ => rsleigh::VnSpace::RAM,
        };
        let load_class = classify_addr(function, function.load_addr(load), self.sp_memo);
        let [out] = function
            .node_outputs_exact::<1>(load)
            .expect("Load has 1 output per node signature");
        let load_size = function
            .value_type_opt(out)
            .expect("Load output is a value")
            .byte_size() as i64;
        let mem_node = function.producer(mem);
        let mut oracle = self.oracle(load_class, load_size, load_space);
        oracle.find_nearest_clobber(function, mem_node)
    }

    /// Finds the nearest `Store` reachable backward (memory-SSA, `MemPhi`-sound)
    /// from the def producing the `mem_start` memory token that covers byte
    /// `[offset, offset + probe_size)` relative
    /// to SP terminal `base`, returning its data / offset / width — or `None` when
    /// the nearest covering def is not a same-base `Store` (a `Call`, a
    /// disagreeing `MemPhi`, `InitialMemory`, an opaque producer, or a store
    /// rooted at a different SP base).
    ///
    /// This is the one SP-store lookup shared by the stack-array jump-table
    /// classifier (which probes one typed table entry and checks exactness) and
    /// `CallStackArgCollect` (which probes a single byte — `probe_size == 1` — to
    /// discover an argument store and reads its natural width back from `size`).
    /// `probe_size` is the caller's choice: a width-sensitive consumer passes the
    /// access width so a partial tail-overlap is caught as a clobber; a discovery
    /// consumer passes `1`.
    pub(crate) fn reaching_store(
        &mut self,
        function: &Function,
        mem_start: ValueId,
        base: ValueId,
        offset: i64,
        probe_size: i64,
    ) -> Option<ReachingSpStore> {
        // Stack memory lives in RAM, so a probed SP-rooted location only
        // matches RAM stores (a same-address store in another space is
        // disjoint and is skipped, fail-closed for the caller).  Scope the
        // oracle to just the read-only walk so its `sp_memo` borrow ends before
        // the `self.decompose` below reuses the same memo.
        let clobber = {
            let mut oracle = self.oracle(
                AddrClass::SpRooted { base, offset },
                probe_size,
                rsleigh::VnSpace::RAM,
            );
            // `find_nearest_clobber` is the read-only walk (no narrowing); it
            // resolves the nearest clobber backward from the def producing
            // `mem_start`.
            oracle.find_nearest_clobber(function, function.producer(mem_start))
        };
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        // Resolve the store's own SP offset (side-table SSoT, else decompose); it
        // must share `base` to be comparable to the probed location.
        let store_offset = match function.stack_offset(clobber) {
            Some((b, off)) if b == base => off,
            Some(_) => return None,
            None => match self.decompose(function, function.store_addr(clobber)) {
                Some(SpExpr {
                    base: b,
                    offset: off,
                }) if b == base => off,
                _ => return None,
            },
        };
        Some(ReachingSpStore { node: clobber, store_offset })
    }
}

/// The nearest non-clobbered `Store` to an SP-relative location, found via the
/// shared memory-SSA walker.  Returned by [`SpAliasCfg::reaching_store`].
///
/// Carries the store NODE plus the one fact the query computed that the node
/// alone doesn't give (`store_offset`, the SP-decomposition result).  The
/// stored data and its width are derived from the node on demand
/// ([`Self::data`] / [`Self::size`]) rather than stored, so the result holds
/// no information twice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReachingSpStore {
    /// The reaching `Store` node.
    pub node: NodeId,
    /// The store's SP-relative byte offset (from `base`).  Equals the probed
    /// `offset` exactly when the store is anchored at the probed location;
    /// callers that require anchoring compare the two.
    pub store_offset: i64,
}

impl ReachingSpStore {
    /// The stored data value (the candidate argument / table entry).
    pub fn data(&self, function: &Function) -> ValueId {
        function.store_data(self.node)
    }

    /// The store's data byte width.  Callers derive an argument's slot span
    /// from this (`ceil(size / increment)`).
    pub fn size(&self, function: &Function) -> i64 {
        store_value_byte_size(function.graph(), self.data(function))
    }
}
