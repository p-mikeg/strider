//! Address-alias classification shared by the SP-aware memory-chain
//! analyses (`function_args`, `load_forward`).
//!
//! A load and an intervening store are each reduced to a coarse
//! [`AddrClass`] (SP-rooted terminal, literal constant, or opaque anchor);
//! [`alias_verdict`] then answers whether they `Match` the same bytes,
//! are provably `Disjoint`, or `MayAlias`.  [`store_alias_verdict`] is the
//! one-call entry both consumers use: classify a `Store`'s address against
//! a precomputed load class and return the verdict.

use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::AliasMode;
use crate::memory_ssa::MemorySSAWalker;

use super::decompose::{SpDecomposer, SpExpr, SpExprMemo};
use super::ranges::{ranges_disjoint, store_value_byte_size};

/// Coarse classification of a Load / Store address.  The verdict table in
/// [`alias_verdict`] is keyed on the `(load_class, store_class)` pair:
/// matching addresses use the diagonal of the table, disjointness uses the
/// off-diagonal.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddrClass {
    /// `decompose_sp` returned a terminal `{ base, offset }`.  Two
    /// `SpRooted` addresses refer to the same byte range only when they
    /// share the same `base` (the SP-derived terminal node) AND offset;
    /// disjoint offsets on the SAME base are proven non-overlapping via
    /// [`ranges_disjoint`].  Different bases — e.g. `InitialVar(sp)` vs an
    /// alignment-masked `sp & -16` — differ by an unknown amount (the
    /// caller-dependent `sp mod align`), so their offsets are in different
    /// coordinate systems and are treated as may-alias.
    SpRooted { base: ValueId, offset: i64 },
    /// `NodeKind::IntConst(_)` address — a literal `.data`/`.rodata`/
    /// `.bss`/MMIO pointer.  Two `Constant` addresses with equal values
    /// refer to the same byte range; disjoint values are proven
    /// non-overlapping via [`ranges_disjoint`].
    Constant { addr: i64 },
    /// Anything else (`Load`-of-pointer, `Add` of opaque values, a
    /// non-collapsing `Phi`-of-offsets, …).  Two `Anchor` addresses are
    /// proven equal only by `ValueId` equality; different ids can compute
    /// to the same address at runtime, so we treat them as
    /// possibly-aliasing.
    Anchor { value: ValueId },
}

/// Pairwise verdict between a Load's address class + size and an
/// intervening Store's address class + size.  Implements the table
/// described in the [`AliasMode`] module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasVerdict {
    /// Same byte range — a `load_forward` caller treats this Store as the
    /// forwarding source.
    Match,
    /// Provably non-overlapping byte range — caller steps through.
    Disjoint,
    /// Cannot prove either; caller bails (shadow / no-forward).
    MayAlias,
}

/// Classifies a load / store address.  Cheap: `decompose_sp` is memoised
/// across the function, the `IntConst` peek is a single match.
fn classify_addr(function: &Function, addr: ValueId, memo: &mut SpExprMemo) -> AddrClass {
    match SpDecomposer::new(function, memo).decompose(addr) {
        Some(SpExpr { base, offset }) => AddrClass::SpRooted { base, offset },
        None => {
            if let Some(c) = function.int_const_u128(addr) {
                AddrClass::Constant { addr: c as i64 }
            } else {
                AddrClass::Anchor { value: addr }
            }
        }
    }
}

/// Diagonal verdict for two in-class offsets: equal → `Match`,
/// range-disjoint → `Disjoint`, otherwise `MayAlias`.  Shared by the
/// `SpRooted`/`SpRooted` and `Constant`/`Constant` arms of
/// [`alias_verdict`] (the `Anchor`/`Anchor` arm uses `ValueId` equality
/// and has no offset/range shape).
fn cmp_same_class_offsets(
    load_off: i64,
    load_size: i64,
    store_off: i64,
    store_size: i64,
) -> AliasVerdict {
    if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load_size, store_off, store_size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// Pairwise alias verdict between a load's class + size and a store's
/// class + size under the given [`AliasMode`].
pub(crate) fn alias_verdict(
    load_class: AddrClass,
    load_size: i64,
    store_class: AddrClass,
    store_size: i64,
    mode: AliasMode,
    distinct_sp_bases_disjoint: bool,
) -> AliasVerdict {
    use AddrClass::*;
    match (load_class, store_class) {
        // Diagonal: in-class equality + range-disjoint.  Two SP-rooted
        // addresses are only comparable when they share the same base node;
        // different SP bases (initial SP vs an alignment-masked SP) differ
        // by an unknown amount, so their offsets can't be related → normally
        // may-alias.  `distinct_sp_bases_disjoint` opts into the optimistic
        // assumption that distinct SP bases address disjoint regions (used by
        // stack-arg detection, where incoming-arg slots above the entry SP do
        // not overlap frame locals rooted at an alignment-masked SP).
        (
            SpRooted {
                base: lb,
                offset: lo,
            },
            SpRooted {
                base: sb,
                offset: so,
            },
        ) => {
            if lb == sb {
                cmp_same_class_offsets(lo, load_size, so, store_size)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        (Constant { addr: lo }, Constant { addr: so }) => {
            cmp_same_class_offsets(lo, load_size, so, store_size)
        }
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Different ids can compute to the same address at runtime;
                // no disjointness proof available.
                AliasVerdict::MayAlias
            }
        }
        // Off-diagonal: cross-class.  Strict cannot prove disjoint;
        // StackGlobalDisjoint admits SP↔Constant pairs.
        (SpRooted { .. }, Constant { .. }) | (Constant { .. }, SpRooted { .. }) => match mode {
            AliasMode::Strict => AliasVerdict::MayAlias,
            AliasMode::StackGlobalDisjoint => AliasVerdict::Disjoint,
        },
        // Every other cross-class pair (Anchor vs anything) still bails
        // under both modes; closing this requires escape analysis.
        _ => AliasVerdict::MayAlias,
    }
}

/// Classifies a raw `NodeKind::Store` against a precomputed load address
/// class + size, returning the [`AliasVerdict`].  This is the single
/// store-aliasing entry shared by the `function_args` shadow walk (which
/// treats anything but `Disjoint` as a clobber) and the `load_forward`
/// oracle (which additionally forwards on `Match`).
pub(crate) fn store_alias_verdict(
    function: &Function,
    store_node: NodeId,
    load_class: AddrClass,
    load_size: i64,
    sp_memo: &mut SpExprMemo,
    mode: AliasMode,
    distinct_sp_bases_disjoint: bool,
) -> AliasVerdict {
    // Store inputs: [MEM, ADDR, DATA] — exactly 3 once the kind is
    // established by the caller (validated structural invariant).
    let inputs = function
        .graph()
        .node_inputs_exact::<3>(store_node)
        .expect("Store node has 3 inputs (validated)");
    let store_size = store_value_byte_size(function.graph(), inputs[2]);
    // `Function::stack_offsets` (populated by `StackOffsetDetect`) is the SSoT
    // for a store's SP-relative offset: it survives address rewrites that leave
    // `decompose_sp` unable to re-derive the offset (an earlier pass folding the
    // address into an opaque shape).  Consult it before falling back to
    // `decompose_sp`.
    let store_class = match function.stack_offset(store_node) {
        Some((base, offset)) => AddrClass::SpRooted { base, offset },
        None => classify_addr(function, inputs[1], sp_memo),
    };
    alias_verdict(
        load_class,
        load_size,
        store_class,
        store_size,
        mode,
        distinct_sp_bases_disjoint,
    )
}

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

impl crate::memory_ssa::MemorySSAWalker for SpAliasOracle<'_> {
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
        // Store inputs: [memory, addr, data].
        let inputs = function
            .graph()
            .node_inputs_exact::<3>(clobber)
            .expect("Store node has 3 inputs (validated)");
        let data = inputs[2];
        // Resolve the store's own SP offset (side-table SSoT, else decompose); it
        // must share `base` to be comparable to the probed location.
        let store_offset = match function.stack_offset(clobber) {
            Some((b, off)) if b == base => off,
            Some(_) => return None,
            None => match SpDecomposer::new(function, oracle.sp_memo).decompose(inputs[1]) {
                Some(SpExpr { base: b, offset: off }) if b == base => off,
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

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::IRBuilderExt;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// The `InitialVar(sp)` output — the canonical entry-SP terminal base
    /// that `decompose_sp` returns for any clean `sp + k` address.
    fn entry_sp_value(f: &Function, sp: rsleigh::Vn) -> ValueId {
        let node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(*f.node_kind(n), NodeKind::InitialVar(vn) if vn == sp))
            .expect("InitialVar(sp) exists");
        f.node_outputs_exact::<1>(node)
            .expect("InitialVar has 1 output")[0]
    }

    fn only_store(f: &Function) -> NodeId {
        f.graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("one store")
    }

    /// Collapse the single-predecessor `read_variable(sp)` phi so SP
    /// addresses are bare `InitialVar(sp) + k` terminals — the shape these
    /// alias helpers see in production (the decomposer no longer looks
    /// through phis).
    fn collapse(f: &mut Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(f, &mut crate::OptCtx::new(None)).expect("phi collapse");
    }

    /// Regression for the two-terminal base bug: a `Store` whose address is
    /// an *alignment-masked* SP base (`(sp & mask) + 8`) must NOT be proven
    /// disjoint from a query slot rooted at the *entry* SP just because
    /// their offsets don't overlap.  The two bases differ by the runtime
    /// alignment delta `sp mod align`, so the offset comparison is
    /// meaningless and the verdict must be may-alias (not `Disjoint`).
    #[test]
    fn different_base_terminal_store_may_alias() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            // aligned = sp & 0xFFFF_FFF8  (a distinct SP base)
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            // store at aligned + 8
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            &mut memo,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(
            verdict,
            AliasVerdict::MayAlias,
            "store at an alignment-masked base must may-alias an entry-SP query \
             (different bases are not offset-comparable)"
        );
    }

    /// With the `distinct_sp_bases_disjoint` opt-in (used by stack-arg
    /// detection), the SAME different-base store is instead treated as
    /// `Disjoint`: incoming-arg slots above the entry SP are assumed not to
    /// overlap frame locals rooted at an alignment-masked SP.
    #[test]
    fn different_base_terminal_store_disjoint_when_opted_in() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            &mut memo,
            AliasMode::StackGlobalDisjoint,
            true,
        );
        assert_eq!(
            verdict,
            AliasVerdict::Disjoint,
            "with distinct_sp_bases_disjoint, a different-base store is assumed disjoint"
        );
    }

    /// Sanity: same base, non-overlapping offsets are provably disjoint.
    #[test]
    fn same_base_disjoint_offsets_is_disjoint() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+0 (size 4): disjoint.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            &mut memo,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Disjoint);
    }

    /// Sanity: same base, same offset is an exact `Match`.
    #[test]
    fn same_base_same_offset_is_match() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+8 (size 4): exact match.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 8,
            },
            4,
            &mut memo,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Match);
    }
}
