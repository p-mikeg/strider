//! Pass-scoped SP-aliasing façade.  [`SpAliasCfg`] bundles the shared
//! SP-expression memo and the alias knobs, built once per pass and reused for
//! every query; per query it builds the [`SpAliasOracle`] that drives the
//! [`super::mem_ssa`] memory-SSA walk.  The address-class verdict logic it
//! consults lives in the sibling [`super::analyzer`] module.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::analyzer::{
    AddrClass, AliasVerdict, SizedAddr, SpAnalyzer, SpExpr, SpExprMemo, alias_verdict,
};
use super::mem_ssa::MemorySSAWalker;
use super::ranges::store_value_byte_size;
use crate::{AliasMode, MemAliasOptions};

/// The single SP-aware [`MemorySSAWalker`] oracle, shared by `load_forward`
/// (store-to-load forwarding) and `function_args` (stack-arg shadow walk).
///
/// `def_clobbers` answers "does this memory def overlap the load's byte
/// range?" for a precomputed load address class:
///
/// * `Store` — classify the store address via
///   [`SpAnalyzer::classify_store_addr`] (stack-offset SSoT before
///   `decompose`) and run the pure [`alias_verdict`]: anything but `Disjoint`
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
    load_size: i128,
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
                if store_space != self.load_space {
                    return false;
                }
                // Classify the store address (stack-offset SSoT before
                // `decompose`) and its size, then run the pure class-on-class
                // verdict directly — anything but `Disjoint` clobbers (a
                // `load_forward` caller re-checks exact-`Match` afterward).
                let store_size = store_value_byte_size(function.graph(), function.store_data(def));
                let store_class =
                    SpAnalyzer::new(function, &mut *self.cfg.sp_memo).classify_store_addr(def);
                alias_verdict(
                    SizedAddr {
                        class: self.load_class,
                        size: self.load_size,
                    },
                    SizedAddr {
                        class: store_class,
                        size: store_size,
                    },
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
        load_size: i128,
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
        SpAnalyzer::new(function, self.sp_memo).classify_addr(addr)
    }

    /// Classify a `Store`'s address, preferring the `stack_offsets` SSoT (via
    /// [`SpAnalyzer::classify_store_addr`]).  The store-side counterpart of
    /// [`Self::classify_addr`]: it must be used wherever the walk's
    /// [`SpAliasOracle::def_clobbers`] uses `classify_store_addr`, so the exact
    /// re-check in [`Self::verdict`] agrees with which stores the walk stops
    /// at even after a rewrite leaves a store's raw address non-decomposable.
    pub(crate) fn classify_store_addr(
        &mut self,
        function: &Function,
        store_node: NodeId,
    ) -> AddrClass {
        SpAnalyzer::new(function, self.sp_memo).classify_store_addr(store_node)
    }

    /// Decompose an address into an SP terminal through this config's shared
    /// memo.  The single decompose entry for consumers, so they no longer
    /// materialise a transient [`SpAnalyzer`] at each call site.
    pub(crate) fn decompose(&mut self, function: &Function, value: ValueId) -> Option<SpExpr> {
        SpAnalyzer::new(function, self.sp_memo).decompose(value)
    }

    /// Class + byte size of a `Load`'s address, both derived from the node
    /// itself (O(1) cached reads; the SP decompose is a memo hit when the
    /// address was already classified).  The shared load-side derivation for
    /// [`verdict`](Self::verdict) and [`nearest_clobber`](Self::nearest_clobber).
    fn load_class_and_size(&mut self, function: &Function, load: NodeId) -> (AddrClass, i128) {
        let class = self.classify_addr(function, function.load_addr(load));
        let (_, ty) = function
            .single_value_output(load)
            .expect("Load has 1 typed output per node signature");
        (class, ty.byte_size() as i128)
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
        let (load_class, load_size) = self.load_class_and_size(function, load_node);
        let store_class = self.classify_store_addr(function, store_node);
        let store_size = store_value_byte_size(function.graph(), function.store_data(store_node));
        alias_verdict(
            SizedAddr {
                class: load_class,
                size: load_size,
            },
            SizedAddr {
                class: store_class,
                size: store_size,
            },
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
        // The load's own space scopes which stores can clobber it.  Every caller
        // passes a `Load` (the class/size derivation below already assumes it).
        let NodeKind::Load(load_space) = *function.node_kind(load) else {
            unreachable!("nearest_clobber is only called on Load nodes");
        };
        let (load_class, load_size) = self.load_class_and_size(function, load);
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
        offset: i128,
        probe_size: i128,
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
        let store_offset = match function.side_tables().stack_offset(clobber) {
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
        Some(ReachingSpStore {
            node: clobber,
            store_offset,
        })
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
    node: NodeId,
    /// The store's SP-relative byte offset (from `base`).  Equals the probed
    /// `offset` exactly when the store is anchored at the probed location;
    /// callers that require anchoring compare the two.
    pub store_offset: i128,
}

impl ReachingSpStore {
    /// The stored data value (the candidate argument / table entry).
    pub(crate) fn data(&self, function: &Function) -> ValueId {
        function.store_data(self.node)
    }

    /// The store's data byte width.  Callers derive an argument's slot span
    /// from this (`ceil(size / increment)`).
    pub(crate) fn size(&self, function: &Function) -> i128 {
        store_value_byte_size(function.graph(), self.data(function))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse};
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IRViewer, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// Regression for the `verdict` / `def_clobbers` SSoT divergence: after a
    /// rewrite leaves a store's raw address non-decomposable, `stack_offsets`
    /// still records it as `[sp+K]`.  The memory-SSA walk stops at the store
    /// (its `def_clobbers` uses `classify_store_addr`, the SSoT), so the exact
    /// re-check in `verdict` must classify the store the SAME way — otherwise
    /// it falls back to `Anchor`, reports `MayAlias`, and `LoadForward`
    /// silently misses a legal forward.
    #[test]
    fn verdict_uses_stack_offset_ssot_for_nondecomposable_store() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let k = b.build_int_const(8u64, ValueType::I32)?;
            // Opaque store address: `xor(sp, 8)` is not a recognised SP base,
            // so `classify_addr` on it yields `Anchor` — standing in for an
            // address a later rewrite folded into a non-decomposable shape.
            let opaque =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Xor, ValueType::I32)?;
            let data = b.build_int_const(0x11u64, ValueType::I32)?;
            b.build_store(opaque, data, rsleigh::VnSpace::RAM)?;
            // Load at the real `sp + 8` (decomposable -> SpRooted(8)).
            let load_addr =
                b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::I32)?;
            let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .expect("build sp fn");

        // Collapse the entry `read_variable(sp)` phi so SP addresses are bare
        // `InitialVar(sp) + k` terminals the decomposer recognises.
        let mut p = OptimizerPipeline::new();
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut f, &mut OptCtx::new(None)).expect("collapse");

        let store = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("store node");
        let load = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Load(_)))
            .expect("load node");
        let entry_sp = f.initial_var_value(&sp).expect("entry sp value");

        // Simulate `StackOffsetDetect`: the SSoT records the store as `[sp+8]`
        // even though its raw address folded to an opaque shape.
        f.side_tables_mut().set_stack_offset(store, entry_sp, 8);

        let mut memo = SpExprMemo::default();
        let mut cfg = SpAliasCfg::call_blocking(&mut memo, AliasMode::StackGlobalDisjoint);
        assert_eq!(
            cfg.verdict(&f, load, store),
            AliasVerdict::Match,
            "verdict must classify the store via the stack_offsets SSoT (like def_clobbers), \
             so a non-decomposable store still verifies as an exact match"
        );
    }
}
