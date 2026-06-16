//! Pass-scoped SP-aliasing façade.  [`SpAliasCfg`] bundles the shared
//! SP-expression memo and the alias knobs, built once per pass and reused for
//! every query; per query it builds the [`SpAliasOracle`] that drives the
//! [`super::mem_ssa`] memory-SSA walk.  The address-class verdict logic it
//! consults lives in the sibling [`super::alias`] module.

use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use super::alias::{AddrClass, AliasVerdict, classify_addr, store_alias_verdict};
use super::decompose::{SpDecomposer, SpExpr, SpExprMemo};
use super::mem_ssa::MemorySSAWalker;
use super::ranges::store_value_byte_size;
use crate::AliasMode;

/// The single SP-aware [`MemorySSAWalker`] oracle, shared by `load_forward`
/// (store-to-load forwarding) and `function_args` (stack-arg shadow walk).
///
/// `def_clobbers` answers "does this memory def overlap the load's byte
/// range?" for a precomputed load address class:
///
/// * `Store` — via [`store_alias_verdict`]: anything but `Disjoint`
///   clobbers (a `load_forward` caller re-checks exact-`Match` afterward).
/// * `Call` / `CallOther` — clobbers iff [`call_clobbers`](Self::call_clobbers)
///   is set.  `load_forward` sets it (a load can never forward across a
///   call); `function_args` passes its `calls_clobber_stack_arguments` knob (off by
///   default — the callee is opaque, so there is nothing to inspect).
/// * any other (opaque) memory producer — conservatively clobbers.
///
/// `MemPhi` is handled structurally by [`may_clobber`], so the oracle never
/// sees one.
struct SpAliasOracle<'a> {
    /// The load's address class (`SpRooted` for a stack-arg load; any class
    /// for a general forwarded load).
    load_class: AddrClass,
    load_size: i64,
    /// The load's address space.  A store in a DIFFERENT `VnSpace` cannot
    /// clobber (or be forwarded into) this load — distinct spaces never alias,
    /// even at the same numeric address.
    load_space: rsleigh::VnSpace,
    sp_memo: &'a mut SpExprMemo,
    alias_mode: AliasMode,
    /// Whether a `Call` / `CallOther` clobbers the load.
    call_clobbers: bool,
    /// Whether two SP-rooted addresses with *different* base nodes are
    /// assumed disjoint (vs. conservatively may-alias).  `function_args`
    /// sets this from its `args_assume_distinct_sp_bases_disjoint` knob (off
    /// by default); `load_forward` leaves it `false`.
    distinct_sp_bases_disjoint: bool,
}

impl super::mem_ssa::MemorySSAWalker for SpAliasOracle<'_> {
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
                        self.sp_memo,
                        self.alias_mode,
                        self.distinct_sp_bases_disjoint,
                    ) != AliasVerdict::Disjoint
            }
            NodeKind::Call | NodeKind::CallOther { .. } => self.call_clobbers,
            // Any other (opaque) memory producer cannot be proven disjoint.
            _ => true,
        }
    }
}

/// Pass-scoped SP-aliasing context: the shared `SpExprMemo` plus the alias
/// knobs, built once per pass and reused for every query.  Bundles the data
/// that used to be threaded through `reaching_sp_store`'s 9-arg signature and
/// the inline `SpAliasOracle` builds at each `may_clobber` call site.
pub(crate) struct SpAliasCfg<'m> {
    sp_memo: &'m mut SpExprMemo,
    alias_mode: AliasMode,
    call_clobbers: bool,
    distinct_sp_bases_disjoint: bool,
}

impl<'m> SpAliasCfg<'m> {
    pub(crate) fn new(
        sp_memo: &'m mut SpExprMemo,
        alias_mode: AliasMode,
        call_clobbers: bool,
        distinct_sp_bases_disjoint: bool,
    ) -> Self {
        Self {
            sp_memo,
            alias_mode,
            call_clobbers,
            distinct_sp_bases_disjoint,
        }
    }

    /// Config for the call-blocking consumers (load-forward, call-stack-arg
    /// collection, stack-array jump tables): a `Call` on the memory chain
    /// clobbers the probed location (`call_clobbers: true`) and distinct SP
    /// bases stay conservatively non-disjoint (`distinct_sp_bases_disjoint:
    /// false`).
    pub(crate) fn call_blocking(sp_memo: &'m mut SpExprMemo, alias_mode: AliasMode) -> Self {
        Self::new(sp_memo, alias_mode, true, false)
    }

    /// Build the per-query oracle from this config + the load's address class
    /// and space.
    fn oracle(
        &mut self,
        load_class: AddrClass,
        load_size: i64,
        load_space: rsleigh::VnSpace,
    ) -> SpAliasOracle<'_> {
        SpAliasOracle {
            load_class,
            load_size,
            load_space,
            sp_memo: &mut *self.sp_memo,
            alias_mode: self.alias_mode,
            call_clobbers: self.call_clobbers,
            distinct_sp_bases_disjoint: self.distinct_sp_bases_disjoint,
        }
    }

    /// Classify a load/store address under this config's memo.
    pub(crate) fn classify_addr(&mut self, function: &Function, addr: ValueId) -> AddrClass {
        classify_addr(function, addr, self.sp_memo)
    }

    /// Mutating walk: nearest clobber of the load at `(load_class, load_size)`
    /// reachable backward from the def producing the `mem` memory token,
    /// narrowing the load's memory edge.
    pub(crate) fn nearest_clobber(
        &mut self,
        ctx: &mut crate::EditFunction<'_>,
        load: NodeId,
        load_class: AddrClass,
        load_size: i64,
        mem: ValueId,
    ) -> NodeId {
        // The load's own space scopes which stores can clobber it.
        // `load_forward` only ever passes a `Load`; RAM is a safe default.
        let load_space = match ctx.node_kind(load) {
            NodeKind::Load(s) => *s,
            _ => rsleigh::VnSpace::RAM,
        };
        let mem_node = ctx.function().producer(mem);
        let mut oracle = self.oracle(load_class, load_size, load_space);
        oracle.may_clobber(ctx, load, mem_node)
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
        // disjoint and is skipped, fail-closed for the caller).
        let mut oracle = self.oracle(
            AddrClass::SpRooted { base, offset },
            probe_size,
            rsleigh::VnSpace::RAM,
        );
        // `find_nearest_clobber` is the read-only walk (no narrowing); it resolves
        // the nearest clobber backward from the def producing `mem_start`.
        let clobber = oracle.find_nearest_clobber(function, function.producer(mem_start));
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        let data = function.store_data(clobber);
        // Resolve the store's own SP offset (side-table SSoT, else decompose); it
        // must share `base` to be comparable to the probed location.
        let store_offset = match function.stack_offset(clobber) {
            Some((b, off)) if b == base => off,
            Some(_) => return None,
            None => match SpDecomposer::new(function, oracle.sp_memo).decompose(function.store_addr(clobber)) {
                Some(SpExpr {
                    base: b,
                    offset: off,
                }) if b == base => off,
                _ => return None,
            },
        };
        // Route through the shared helper so this path enforces the same
        // "Store DATA is value-typed" invariant as every other alias check
        // (it `expect`s on malformed IR rather than fabricating a width-0
        // store that would map a real arg onto zero slots downstream).
        let size = store_value_byte_size(function.graph(), data);
        Some(ReachingSpStore {
            data,
            store_offset,
            size,
        })
    }
}

/// The nearest non-clobbered `Store` to an SP-relative location, found via the
/// shared memory-SSA walker.  Returned by [`SpAliasCfg::reaching_store`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReachingSpStore {
    /// The stored data value (the candidate argument / table entry).
    pub data: ValueId,
    /// The store's SP-relative byte offset (from `base`).  Equals the probed
    /// `offset` exactly when the store is anchored at the probed location;
    /// callers that require anchoring compare the two.
    pub store_offset: i64,
    /// The store's data byte width.  Callers derive an argument's slot span
    /// from this (`ceil(size / increment)`) without the query forcing one.
    pub size: i64,
}
