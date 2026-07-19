//! SP decomposition, address classification, and the alias verdict table,
//! shared by every SP-aware pass.  Drives the payload-agnostic walk in
//! [`crate::mem_ssa`].
//!
//! `decompose` reduces `InitialVar(sp)` plus `Add`-of-constant steps (or an
//! alignment-masked `sp & mask` anchor) to one `SpExpr { base, offset }`,
//! memoized into the function's `stack_offsets` side-table.
//!
//! It does NOT look through `Phi`: a stack-tagged `Phi(sp)` decomposes to
//! `None`.  That is safe because `PhiCollapse` / `RedundantPhis` run first and
//! collapse the single-predecessor phis the lifter wraps around
//! `read_variable(sp)`, and every caller reads `None` conservatively as "not a
//! provable SP terminal".

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, IntBinaryOp, SpDecomp};
use strider_target::Endianness;

use crate::mem_ssa::MemorySSAWalker;
use crate::{AliasMode, MemAliasOptions};
use AddrClass::*;

/// `base + offset`, where `base` is an SP-rooted node: `InitialVar(sp)` or an
/// alignment-masked SP `And` output.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpExpr {
    pub base: ValueId,
    pub offset: i128,
}

/// Coarse Load / Store address class; [`alias_verdict`] is keyed on the
/// `(load_class, store_class)` pair.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddrClass {
    /// Offsets are only comparable within one `base`.  Distinct bases (say
    /// `InitialVar(sp)` vs `sp & -16`) differ by the caller-dependent
    /// `sp mod align`, so their offsets live in different coordinate systems
    /// and must be treated as may-alias.
    SpRooted { base: ValueId, offset: i128 },
    /// Literal `.data` / `.rodata` / `.bss` / MMIO pointer.
    Constant { addr: i128 },
    /// Anything else.  Only `ValueId` equality proves two of these equal;
    /// distinct ids can still compute the same runtime address.
    Anchor { value: ValueId },
}

/// Implements the table described in the [`AliasMode`] docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasVerdict {
    /// Same byte range, so `load_forward` can use this Store as its source.
    Match,
    /// Provably non-overlapping; the caller steps through.
    Disjoint,
    /// Neither provable; the caller bails.
    MayAlias,
}

/// Memoized: a hit is O(1), a miss walks the SP spine and caches the verdict
/// (positive or `NotStack`).
///
/// The cache sits behind a `RefCell` so it fills through a shared `&Function`;
/// the memory-SSA and range-scoped callers cannot hand over `&mut`.  Sound
/// during the fixed point because the optimizer clears the cache on every
/// graph mutation, so no memoized verdict outlives its graph.
pub(crate) fn decompose(function: &Function, value: ValueId) -> Option<SpExpr> {
    match function.side_tables().stack_slot(value) {
        SpDecomp::Stack(_) => return resolve_slot(function, value),
        SpDecomp::NotStack => return None,
        SpDecomp::Unknown => {}
    }
    let result = spine_walk(function, value);
    match result {
        Some(e) => function
            .side_tables()
            .set_stack_slot(value, e.base, e.offset),
        None => function.side_tables().set_stack_slot_not(value),
    }
    result
}

/// Walks the `Add`-of-const / alignment-`And` chain down to `InitialVar(sp)`,
/// accumulating the constant offset.  O(spine depth), not O(cone): only the
/// single SP-bearing operand is followed at each step.
fn spine_walk(function: &Function, value: ValueId) -> Option<SpExpr> {
    let mut cur = value;
    let mut acc: i128 = 0;
    // Once an alignment-`And` is met the base is fixed to that And output (an
    // opaque, entry-alignment-dependent base) carrying the offset accrued above
    // it.  Below the anchor the walk only confirms SP-rooting, so those offsets
    // are ignored.  Iterative so a deep `And`-of-`And` chain cannot blow the
    // host stack.
    //
    // Termination: each step follows one input edge toward `InitialVar(sp)`, and
    // revisiting a node needs a data cycle, which in valid SSA passes through a
    // `Phi` and hits the `_ => return None` arm.  So the spine is acyclic.
    let mut anchor: Option<SpExpr> = None;
    loop {
        // A committed verdict short-circuits the walk.
        match function.side_tables().stack_slot(cur) {
            SpDecomp::Stack(_) => {
                let hit = resolve_slot(function, cur)?;
                return Some(match anchor {
                    // Below an anchor the hit only confirms SP-rooting.
                    Some(a) => a,
                    None => SpExpr {
                        base: hit.base,
                        offset: hit.offset.checked_add(acc)?,
                    },
                });
            }
            SpDecomp::NotStack => return None,
            SpDecomp::Unknown => {}
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(id)
                if function.initial_vn(id) == function.default_cc().stack_vn =>
            {
                return Some(anchor.unwrap_or(SpExpr {
                    base: cur,
                    offset: acc,
                }));
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let [lhs, rhs] = function.node_inputs_exact::<2>(node).ok()?;
                // SP + const in either order.  The const shifts the accumulated
                // offset only while above the anchor.
                let (sp_operand, c) =
                    match (function.int_const_i128(rhs), function.int_const_i128(lhs)) {
                        (Some(c), _) => (lhs, c),
                        (None, Some(c)) => (rhs, c),
                        _ => return None,
                    };
                if anchor.is_none() {
                    acc = acc.checked_add(c)?; // fail-closed on overflow
                }
                cur = sp_operand;
            }
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let [l, r] = function.node_inputs_exact::<2>(node).ok()?;
                // Alignment-masked SP is a fresh opaque base, since its value
                // depends on entry-SP alignment.  The non-mask operand must be
                // SP-rooted; the base fixes to this And output the first time.
                let sp_operand = if function.int_const_u128(r).is_some_and(is_alignment_mask) {
                    l
                } else if function.int_const_u128(l).is_some_and(is_alignment_mask) {
                    r
                } else {
                    return None;
                };
                anchor.get_or_insert(SpExpr {
                    base: cur,
                    offset: acc,
                });
                cur = sp_operand;
            }
            _ => return None,
        }
    }
}

fn resolve_slot(function: &Function, value: ValueId) -> Option<SpExpr> {
    function
        .side_tables()
        .stack_slot_resolved(value)
        .map(|(base, offset)| SpExpr { base, offset })
}

fn classify_addr(function: &Function, addr: ValueId) -> AddrClass {
    match decompose(function, addr) {
        Some(SpExpr { base, offset }) => AddrClass::SpRooted { base, offset },
        None => {
            if let Some(c) = function.int_const_u128(addr) {
                AddrClass::Constant { addr: c as i128 }
            } else {
                AddrClass::Anchor { value: addr }
            }
        }
    }
}

/// Named separately so `def_clobbers` and the exact re-check in
/// [`SpAnalyzer::verdict`] classify a store identically.
fn classify_store_addr(function: &Function, store_node: NodeId) -> AddrClass {
    classify_addr(function, function.store_addr(store_node))
}

/// A contiguous run of high 1-bits over at least one low 0-bit, e.g.
/// `0xFFFF_FFF0`.  A low-bit mask like `0xF` is a bit-extraction, not a base,
/// and zero / all-ones have no alignment effect, so all are rejected.
pub(crate) fn is_alignment_mask(m: u128) -> bool {
    let tz = m.trailing_zeros();
    if tz == 0 || tz == 128 {
        return false;
    }
    // Past the low zero run the rest must be a contiguous block of 1s, i.e.
    // `shifted + 1` is a power of two.
    let shifted = m >> tz;
    shifted != 0 && shifted & shifted.wrapping_add(1) == 0
}

impl AddrClass {
    fn offset(self) -> Option<i128> {
        match self {
            SpRooted { offset, .. } => Some(offset),
            Constant { addr } => Some(addr),
            Anchor { .. } => None,
        }
    }
}

/// Endpoints saturate so a caller passing `size = i128::MAX` as a
/// soundness-pessimistic fallback (unknown `value_byte_size`) cannot panic or
/// wrap.  A saturated endpoint then reports "not disjoint", treating an
/// unknown-extent range as infinite.
#[inline]
fn ranges_disjoint(a_off: i128, a_size: i128, b_off: i128, b_size: i128) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    if a_end == i128::MAX || b_end == i128::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Only the same-base `SpRooted` and `Constant`/`Constant` arms of
/// [`alias_verdict`] reach here, and both carry an offset, so the unwraps hold.
fn offset_range_verdict(load: SizedAddr, store: SizedAddr) -> AliasVerdict {
    let load_off = load.class.offset().expect("offset-bearing load class");
    let store_off = store.class.offset().expect("offset-bearing store class");
    if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load.size, store_off, store.size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// One operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
}

pub(crate) fn alias_verdict(load: SizedAddr, store: SizedAddr, options: SpOptions) -> AliasVerdict {
    let mode = options.alias_mode;
    let distinct_sp_bases_disjoint = options.mem.assume_distinct_sp_bases_disjoint;
    let SizedAddr {
        class: load_class, ..
    } = load;
    let SizedAddr {
        class: store_class, ..
    } = store;
    match (load_class, store_class) {
        // Different SP bases differ by an unknown amount, so their offsets are
        // unrelated and normally may-alias.  `distinct_sp_bases_disjoint` opts
        // into assuming they address disjoint regions, which stack-arg
        // detection relies on: incoming-arg slots above the entry SP do not
        // overlap frame locals rooted at an alignment-masked SP.
        (SpRooted { base: lb, .. }, SpRooted { base: sb, .. }) => {
            if lb == sb {
                offset_range_verdict(load, store)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        (Constant { .. }, Constant { .. }) => offset_range_verdict(load, store),
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Distinct ids can still compute the same runtime address.
                AliasVerdict::MayAlias
            }
        }
        (SpRooted { .. }, Constant { .. }) | (Constant { .. }, SpRooted { .. }) => match mode {
            AliasMode::Strict => AliasVerdict::MayAlias,
            AliasMode::StackGlobalDisjoint => AliasVerdict::Disjoint,
        },
        // Anchor against anything bails under both modes.  Closing this needs
        // escape analysis.
        _ => AliasVerdict::MayAlias,
    }
}

/// The SP-aware [`MemorySSAWalker`], shared by `load_forward` and
/// `function_args`.  `MemPhi` is handled structurally by the walk, so
/// `def_clobbers` never sees one.
struct SpMemWalker<'a> {
    analyzer: &'a SpAnalyzer,
    /// A precomputed [`SizedAddr`] rather than a `Load` `NodeId` because
    /// `reaching_store` probes a synthetic SP-rooted location with no backing
    /// `Load` node.  The class is the one representation a real load and that
    /// probe share.
    load: SizedAddr,
    /// Distinct spaces never alias, even at the same numeric address.
    load_space: rsleigh::VnSpace,
}

impl MemorySSAWalker for SpMemWalker<'_> {
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        match *function.node_kind(def) {
            // A cross-space store cannot clobber this load, so the walk
            // continues to a same-space def.  Stopping here would let
            // `load_forward` forward a different-space store's value into the
            // load, a miscompile.
            NodeKind::Store(store_space) => {
                if store_space != self.load_space {
                    return false;
                }
                // Anything but `Disjoint` clobbers; `load_forward` re-checks
                // for an exact `Match` afterward.  Same store derivation as
                // `verdict`, so the walk stops at exactly the stores that
                // re-check sees.
                self.analyzer
                    .alias(self.load, self.analyzer.store_sized(function, def))
                    != AliasVerdict::Disjoint
            }
            NodeKind::Call | NodeKind::CallOther { .. } => self.analyzer.options.mem.calls_clobber,
            // No opaque memory producer can be proven disjoint.
            _ => true,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SpOptions {
    alias_mode: AliasMode,
    mem: MemAliasOptions,
}

impl SpOptions {
    pub(crate) fn new(alias_mode: AliasMode, mem: MemAliasOptions) -> Self {
        Self { alias_mode, mem }
    }

    /// For load-forward, call-stack-arg collection, and stack-array jump
    /// tables: a `Call` on the memory chain clobbers the probed location, and
    /// distinct SP bases stay conservatively non-disjoint.
    pub(crate) fn call_blocking(alias_mode: AliasMode) -> Self {
        Self::new(
            alias_mode,
            MemAliasOptions {
                calls_clobber: true,
                assume_distinct_sp_bases_disjoint: false,
            },
        )
    }
}

/// Built once per pass.  Takes the `&Function` per call rather than binding
/// it: consumers interleave these read queries with `&mut EditFunction`
/// mutations, so a captured shared borrow would collide with the edits.
pub(crate) struct SpAnalyzer {
    options: SpOptions,
}

impl SpAnalyzer {
    pub(crate) fn new(options: SpOptions) -> Self {
        Self { options }
    }

    fn walker(&self, load: SizedAddr, load_space: rsleigh::VnSpace) -> SpMemWalker<'_> {
        SpMemWalker {
            analyzer: self,
            load,
            load_space,
        }
    }

    /// The one place the knobs meet [`alias_verdict`], so `verdict` and
    /// `def_clobbers` cannot drift apart.
    fn alias(&self, load: SizedAddr, store: SizedAddr) -> AliasVerdict {
        alias_verdict(load, store, self.options)
    }

    fn load_sized(&self, function: &Function, load: NodeId) -> SizedAddr {
        let class = classify_addr(function, function.load_addr(load));
        let (_, ty) = function
            .single_value_output(load)
            .expect("Load has 1 typed output per node signature");
        SizedAddr {
            class,
            size: ty.byte_size() as i128,
        }
    }

    /// Goes through [`classify_store_addr`] so [`Self::verdict`] classifies a
    /// store the same way [`SpMemWalker::def_clobbers`] does, even after a
    /// rewrite leaves the store's raw address non-decomposable.
    fn store_sized(&self, function: &Function, store: NodeId) -> SizedAddr {
        SizedAddr {
            class: classify_store_addr(function, store),
            size: store_value_byte_size(function, function.store_data(store)),
        }
    }

    /// The single decompose entry for consumers outside this module.
    pub(crate) fn decompose(&self, function: &Function, value: ValueId) -> Option<SpExpr> {
        decompose(function, value)
    }

    /// The node-based counterpart of the class-based [`alias_verdict`].
    pub(crate) fn verdict(
        &self,
        function: &Function,
        load_node: NodeId,
        store_node: NodeId,
    ) -> AliasVerdict {
        self.alias(
            self.load_sized(function, load_node),
            self.store_sized(function, store_node),
        )
    }

    /// No narrowing; call [`crate::mem_ssa::narrow_load_to`] for that.
    pub(crate) fn nearest_clobber(
        &self,
        function: &Function,
        load: NodeId,
        mem: ValueId,
    ) -> NodeId {
        let NodeKind::Load(load_space) = *function.node_kind(load) else {
            unreachable!("nearest_clobber is only called on Load nodes");
        };
        let load = self.load_sized(function, load);
        let mem_node = function.producer(mem);
        let mut walker = self.walker(load, load_space);
        walker.find_nearest_clobber(function, mem_node)
    }

    /// The nearest `Store` covering `[offset, offset + probe_size)` relative to
    /// SP terminal `base`, or `None` when the nearest covering def is anything
    /// else (a `Call`, a disagreeing `MemPhi`, `InitialMemory`, an opaque
    /// producer, or a store rooted at a different SP base).
    ///
    /// `probe_size` is the caller's call: pass the access width to catch a
    /// partial tail-overlap as a clobber, or `1` to merely discover a store and
    /// read its natural width back off `size`.
    pub(crate) fn reaching_store(
        &self,
        function: &Function,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
    ) -> Option<ReachingSpStore> {
        // Stack memory lives in RAM, so a same-address store in another space
        // counts as disjoint and is skipped.
        let clobber = {
            let mut walker = self.walker(
                SizedAddr {
                    class: AddrClass::SpRooted { base, offset },
                    size: probe_size,
                },
                rsleigh::VnSpace::RAM,
            );
            walker.find_nearest_clobber(function, function.producer(mem_start))
        };
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        // The store's own SP offset must share `base` to be comparable to the
        // probed location.
        let SpExpr {
            base: store_base,
            offset: store_offset,
        } = decompose(function, function.store_addr(clobber))?;
        if store_base != base {
            return None;
        }
        Some(ReachingSpStore {
            node: clobber,
            store_offset,
        })
    }
}

/// Result of [`SpAnalyzer::reaching_store`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReachingSpStore {
    node: NodeId,
    /// Byte offset from `base`.  Equals the probed `offset` only when the store
    /// is anchored at the probed location; callers needing anchoring compare
    /// the two themselves.
    pub store_offset: i128,
}

impl ReachingSpStore {
    pub(crate) fn data(&self, function: &Function) -> ValueId {
        function.store_data(self.node)
    }

    /// Callers derive an argument's slot span from this: `ceil(size / increment)`.
    pub(crate) fn size(&self, function: &Function) -> i128 {
        store_value_byte_size(function, self.data(function))
    }
}

/// Right-shift in bits that brings the `load_ty`-width slice of a wider
/// `store_ty` integer into the low end.  Shared by `LoadForward`'s node-building
/// narrowing and the jump-table evaluator's symbolic `reshape`.
///
/// Little-endian keeps the load bytes low already, so the shift is 0.  Callers
/// only invoke this when the load is narrower, so the subtraction cannot
/// underflow.
#[inline]
pub(crate) fn high_low_shift_bits(
    store_ty: ValueType,
    load_ty: ValueType,
    endianness: Endianness,
) -> u32 {
    match endianness {
        Endianness::Little => 0,
        Endianness::Big => (store_ty.byte_size() - load_ty.byte_size()) as u32 * 8,
    }
}

/// The IR signature guarantees a valid `Store`'s DATA slot is value-typed, so
/// a non-value here means malformed IR and panics rather than silently
/// degrading the alias verdict.
#[inline]
pub(crate) fn store_value_byte_size(function: &Function, store_data: ValueId) -> i128 {
    function
        .value_type(store_data)
        .expect("Store data input is a value")
        .byte_size() as i128
}

#[cfg(test)]
mod tests;
