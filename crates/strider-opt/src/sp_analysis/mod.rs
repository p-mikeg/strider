//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`call_stack_args`, `load_forward`, `function_args::stack_args`): SP
//! decomposition + address classification + alias verdict + the pass-scoped
//! [`SpAnalyzer`] handle (bundling [`SpOptions`]), all in this module.  The payload-agnostic memory-SSA
//! walk it drives lives in the sibling [`crate::mem_ssa`] module.
//!
//! `decompose` is the workhorse: given an output that is
//! `InitialVar(sp)` transformed by `Add` of constants (subtraction appears as
//! `Add(_, Neg(K))`) or anchored at an alignment-masked `sp & mask`, it returns
//! a single `SpExpr { base, offset }` terminal (or `None`).  It MEMOIZES into
//! the function's `stack_offsets` side-table: a cache hit returns O(1), else it
//! walks the SP spine backward and caches the verdict so the next query on that
//! value is a hit.
//!
//! The decomposer does **not** look through `Phi` nodes — a stack-tagged
//! `Phi(sp)` (loop-header join, or the single-predecessor phi the lifter wraps
//! around `read_variable(sp)`) decomposes to `None`.  By the time any SP-aware
//! pass runs `decompose`, `PhiCollapse` / `RedundantPhis` have already
//! collapsed those single-predecessor phis to their `InitialVar(sp)` input, so
//! the decomposer only ever meets real terminals.  A `None` reads as "not a
//! provable SP terminal", which every caller already treats conservatively
//! (may-alias / opaque base).
//!
//! On top of the decomposition, the free `classify_addr` / `classify_store_addr`
//! sort a load / store address into a coarse [`AddrClass`] (both decomposing via
//! the cache-backed `decompose`); the pure, class-on-class verdict table is
//! [`alias_verdict`]; and [`SpAnalyzer`] (holding [`SpOptions`]) layers the
//! pass-scoped alias knobs on top, driving the `mem_ssa` walk via a transient
//! [`SpMemWalker`].

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, IntBinaryOp, SpDecomp};
use strider_target::Endianness;

use crate::mem_ssa::MemorySSAWalker;
use crate::{AliasMode, MemAliasOptions};
use AddrClass::*;

/// Decomposed stack-pointer expression: `base + offset`, where `base` is an
/// SP-rooted node (`InitialVar(sp)` or an alignment-masked SP `And` output).
///
/// `decompose` returns `Option<SpExpr>`; `None` carries the
/// "not a provable SP terminal" case, so there is no separate variant for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpExpr {
    pub base: ValueId,
    pub offset: i128,
}

/// Coarse classification of a Load / Store address.  The verdict table in
/// [`alias_verdict`] is keyed on the `(load_class, store_class)` pair:
/// matching addresses use the diagonal of the table, disjointness uses the
/// off-diagonal.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddrClass {
    /// `decompose` returned a terminal `{ base, offset }`.  Two
    /// `SpRooted` addresses refer to the same byte range only when they
    /// share the same `base` (the SP-derived terminal node) AND offset;
    /// disjoint offsets on the SAME base are proven non-overlapping via
    /// [`ranges_disjoint`].  Different bases — e.g. `InitialVar(sp)` vs an
    /// alignment-masked `sp & -16` — differ by an unknown amount (the
    /// caller-dependent `sp mod align`), so their offsets are in different
    /// coordinate systems and are treated as may-alias.
    SpRooted { base: ValueId, offset: i128 },
    /// `NodeKind::IntConst(_)` address — a literal `.data`/`.rodata`/
    /// `.bss`/MMIO pointer.  Two `Constant` addresses with equal values
    /// refer to the same byte range; disjoint values are proven
    /// non-overlapping via [`ranges_disjoint`].
    Constant { addr: i128 },
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

/// The stack decomposition of `value`, MEMOIZED: a cache hit returns O(1), else
/// it walks the SP spine and caches the verdict (positive or `NotStack`) so the
/// next query on `value` is a hit.
///
/// The cache lives behind a `RefCell` on the function's side-tables, so this
/// fills it through a shared `&Function` — its memory-SSA / range-scoped callers
/// can't hand it `&mut`.  The optimizer clears the cache on every graph mutation
/// (so a memoized verdict never outlives its graph), which is why filling here
/// is sound during the fixed point.
pub(crate) fn decompose(function: &Function, value: ValueId) -> Option<SpExpr> {
    // Cache hit for the queried value → done, no re-walk.
    match function.side_tables().stack_slot(value) {
        SpDecomp::Stack(_) => return resolve_slot(function, value),
        SpDecomp::NotStack => return None,
        SpDecomp::Unknown => {}
    }
    // Miss: walk the spine (short-circuiting on any already-cached ancestor),
    // then memoize `value`'s verdict.
    let result = spine_walk(function, value);
    match result {
        Some(e) => function
            .side_tables()
            .set_stack_slot(value, e.base, e.offset),
        None => function.side_tables().set_stack_slot_not(value),
    }
    result
}

/// The SP-spine walk backing [`decompose`]: the
/// `Add`-of-const / alignment-`And` chain down to `InitialVar(sp)`, accumulating
/// the constant offset.  O(spine depth), NOT O(cone) — it follows only the
/// single SP-bearing operand at each step.  Mutates nothing; short-circuits on
/// any cached ancestor slot.
fn spine_walk(function: &Function, value: ValueId) -> Option<SpExpr> {
    let mut cur = value;
    let mut acc: i128 = 0;
    // Once an alignment-`And` is met, the base is fixed to *that* And output (an
    // opaque, entry-alignment-dependent base) carrying the offset accrued above
    // it; the rest of the walk only *confirms* the operand is SP-rooted, so
    // offsets below the And are ignored.  Iterative (not recursive) so a deep
    // `And`-of-`And` chain cannot overflow the stack.
    //
    // The loop terminates by graph construction: each step follows a single
    // input edge up toward `InitialVar(sp)`, and the only way to revisit a node
    // is a data cycle — which in valid SSA passes through a `Phi`, hitting the
    // `_ => return None` arm.  So the `Add`/`And` spine is acyclic and the walk
    // always reaches a terminal (`InitialVar` / cache hit / a non-spine kind).
    let mut anchor: Option<SpExpr> = None;
    loop {
        // A committed verdict short-circuits the walk.
        match function.side_tables().stack_slot(cur) {
            SpDecomp::Stack(_) => {
                let hit = resolve_slot(function, cur)?;
                return Some(match anchor {
                    // Below an anchor, the committed hit only confirms SP-rooting.
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
                // SP + const in either order; the const shifts the accumulated
                // offset (only while above the anchor) and the walk continues
                // down the other operand.
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
                // Alignment-masked SP is a fresh opaque base (its exact value
                // depends on entry-SP alignment).  Require the non-mask operand
                // to be SP-rooted; fix the base to this And output the first time.
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

/// Resolves a value's cached `(base, offset)` slot into an [`SpExpr`], or `None`
/// when the slot is unknown / not-stack.
fn resolve_slot(function: &Function, value: ValueId) -> Option<SpExpr> {
    function
        .side_tables()
        .stack_slot_resolved(value)
        .map(|(base, offset)| SpExpr { base, offset })
}

/// Classifies a load / store address.  Cheap: `decompose` is a cache hit /
/// local recompute, the `IntConst` peek is a single match.
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

/// Classifies a raw `NodeKind::Store`'s address into an [`AddrClass`] — the
/// store-side counterpart of [`classify_addr`], kept named so `def_clobbers`
/// and the exact re-check in [`SpAnalyzer::verdict`] classify a store
/// identically.  `classify_addr` decomposes via [`decompose`], which consults
/// the `stack_offsets` cache itself, so there is no separate side-table
/// preference here.
fn classify_store_addr(function: &Function, store_node: NodeId) -> AddrClass {
    classify_addr(function, function.store_addr(store_node))
}

/// Is `m` a stack-*alignment* mask — a contiguous run of high 1-bits with at
/// least one low 0-bit (e.g. `0xFFFF_FFF8`, `0xFFFF_FFF0`)?  An alignment mask
/// clears only the low-order bits; a low-bit mask (`0xF`) is a bit-extraction,
/// not a base.  `0` and all-ones masks are rejected (no alignment effect / not
/// a low-clearing mask).
pub(crate) fn is_alignment_mask(m: u128) -> bool {
    let tz = m.trailing_zeros();
    // `tz == 0`: no low zero bits → not clearing any alignment (low mask or all-ones).
    // `tz == 128`: m is zero → all bits cleared, not a valid alignment mask.
    if tz == 0 || tz == 128 {
        return false;
    }
    // After dropping the low zero run, the remaining bits must be a contiguous
    // block of 1s (all-ones once shifted), i.e. `shifted + 1` is a power of two.
    let shifted = m >> tz;
    shifted != 0 && shifted & shifted.wrapping_add(1) == 0
}

impl AddrClass {
    /// The linear byte offset of an offset-bearing class (`SpRooted` /
    /// `Constant`); `Anchor` has none.  Lets [`offset_range_verdict`] read the
    /// offset off either diagonal-comparable class uniformly.
    fn offset(self) -> Option<i128> {
        match self {
            SpRooted { offset, .. } => Some(offset),
            Constant { addr } => Some(addr),
            Anchor { .. } => None,
        }
    }
}

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` are
/// disjoint.
///
/// Endpoint computations use `saturating_add` so that callers passing
/// `size = i128::MAX` as a soundness-pessimistic fallback (e.g. when a Store's
/// `value_byte_size` is unknown) cannot panic in debug or wrap in release.
/// A saturated upper endpoint additionally short-circuits to "not disjoint"
/// — i.e. an unknown-extent range is treated as effectively infinite in both
/// directions, matching the conservative verdict callers expect.
#[inline]
fn ranges_disjoint(a_off: i128, a_size: i128, b_off: i128, b_size: i128) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    // If either endpoint saturated, treat the corresponding range as
    // unbounded and report "not disjoint" — the conservative answer.
    if a_end == i128::MAX || b_end == i128::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Diagonal verdict for two in-class offset-bearing addresses: equal offset →
/// `Match`, range-disjoint → `Disjoint`, otherwise `MayAlias`.  Shared by the
/// `SpRooted`/`SpRooted` (same base) and `Constant`/`Constant` arms of
/// [`alias_verdict`] — the only arms whose classes carry an offset (the
/// `Anchor`/`Anchor` arm uses `ValueId` equality and has no offset/range
/// shape), so [`AddrClass::offset`] is always `Some` here.
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

/// An address class paired with the byte size of the access at it — one
/// operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
}

/// Pairwise alias verdict between a load's class + size and a store's
/// class + size under the given [`SpOptions`] (its alias mode + distinct-base
/// knob).
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
        // Diagonal: in-class equality + range-disjoint.  Two SP-rooted
        // addresses are only comparable when they share the same base node;
        // different SP bases (initial SP vs an alignment-masked SP) differ
        // by an unknown amount, so their offsets can't be related → normally
        // may-alias.  `distinct_sp_bases_disjoint` opts into the optimistic
        // assumption that distinct SP bases address disjoint regions (used by
        // stack-arg detection, where incoming-arg slots above the entry SP do
        // not overlap frame locals rooted at an alignment-masked SP).
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

/// The single SP-aware [`MemorySSAWalker`], shared by `load_forward`
/// (store-to-load forwarding) and `function_args` (stack-arg shadow walk).
///
/// `def_clobbers` answers "does this memory def overlap the load's byte
/// range?" for a precomputed load address class:
///
/// * `Store` — classify the store address via [`classify_store_addr`] and run
///   the pure [`alias_verdict`]: anything but `Disjoint` clobbers (a
///   `load_forward` caller re-checks exact-`Match` afterward).
/// * `Call` / `CallOther` — clobbers iff `mem.calls_clobber` is set.
///   `load_forward` sets it (a load can never forward across a call);
///   `function_args` passes its `calls_clobber` knob (off by default — the
///   callee is opaque, so there is nothing to inspect).
/// * any other (opaque) memory producer — conservatively clobbers.
///
/// `MemPhi` is handled structurally by the walk, so it never sees one.
struct SpMemWalker<'a> {
    /// The owning analyzer — the source of the alias knobs + the store
    /// classification, so this holds only the per-query load facts below and
    /// reads the rest through `analyzer` instead of copying them.
    analyzer: &'a SpAnalyzer,
    /// The load's address class + access size.  A precomputed [`SizedAddr`]
    /// rather than a `Load` `NodeId` because one consumer — `reaching_store` —
    /// probes a SYNTHETIC SP-rooted location that has no backing `Load` node;
    /// carrying the class directly is the one representation both a real load
    /// and that synthetic probe share.
    load: SizedAddr,
    /// The load's address space.  A store in a DIFFERENT `VnSpace` cannot
    /// clobber (or be forwarded into) this load — distinct spaces never alias,
    /// even at the same numeric address.
    load_space: rsleigh::VnSpace,
}

impl MemorySSAWalker for SpMemWalker<'_> {
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
                // Anything but `Disjoint` clobbers (a `load_forward` caller
                // re-checks exact-`Match` afterward).  Same store derivation as
                // `verdict`, so the walk stops at exactly the stores `verdict`
                // later re-checks.
                self.analyzer
                    .alias(self.load, self.analyzer.store_sized(function, def))
                    != AliasVerdict::Disjoint
            }
            NodeKind::Call | NodeKind::CallOther { .. } => self.analyzer.options.mem.calls_clobber,
            // Any other (opaque) memory producer cannot be proven disjoint.
            _ => true,
        }
    }
}

/// Pass-scoped SP-aliasing options: just the alias knobs.  A plain data bundle
/// handed to [`SpAnalyzer::new`]; the query API lives on the analyzer.
#[derive(Clone, Copy)]
pub(crate) struct SpOptions {
    alias_mode: AliasMode,
    mem: MemAliasOptions,
}

impl SpOptions {
    pub(crate) fn new(alias_mode: AliasMode, mem: MemAliasOptions) -> Self {
        Self { alias_mode, mem }
    }

    /// Options for the call-blocking consumers (load-forward, call-stack-arg
    /// collection, stack-array jump tables): a `Call` on the memory chain
    /// clobbers the probed location (`calls_clobber: true`) and distinct SP
    /// bases stay conservatively non-disjoint
    /// (`assume_distinct_sp_bases_disjoint: false`).
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

/// The SP-aliasing analysis API: bundles the pass-scoped [`SpOptions`] and
/// exposes the memory-SSA queries (`decompose` / `verdict` / `nearest_clobber`
/// / `reaching_store`).  Built once per pass and reused for every query, taking
/// the `&Function` per call rather than binding it — consumers (`load_forward`,
/// `function_args`) interleave these read queries with `&mut EditFunction`
/// mutations, so a captured shared borrow would collide with those edits.  Per
/// query it builds a transient [`SpMemWalker`] to drive the [`MemorySSAWalker`]
/// walk; the decomposition / classification logic is the free functions above.
pub(crate) struct SpAnalyzer {
    options: SpOptions,
}

impl SpAnalyzer {
    pub(crate) fn new(options: SpOptions) -> Self {
        Self { options }
    }

    /// Build the per-query walker borrowing this analyzer (the source of the
    /// alias knobs + store classification) plus the load's address class + size
    /// and space.
    fn walker(&self, load: SizedAddr, load_space: rsleigh::VnSpace) -> SpMemWalker<'_> {
        SpMemWalker {
            analyzer: self,
            load,
            load_space,
        }
    }

    /// Apply this analyzer's alias mode + distinct-base knob to the pure
    /// class-on-class [`alias_verdict`] table — the single place the two knobs
    /// meet the primitive, so every consumer (`verdict`, `def_clobbers`) agrees.
    fn alias(&self, load: SizedAddr, store: SizedAddr) -> AliasVerdict {
        alias_verdict(load, store, self.options)
    }

    /// A `Load`'s address class + access byte size, derived from the node
    /// itself (O(1) cached reads; the SP decompose is a cache hit when the
    /// address was already classified).
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

    /// A `Store`'s address class + data byte size — the store-side counterpart
    /// of [`Self::load_sized`].  The class comes from [`classify_store_addr`]
    /// (which prefers the `stack_offset` SSoT), so the exact re-check in
    /// [`Self::verdict`] classifies a store the SAME way the walk's
    /// [`SpMemWalker::def_clobbers`] does — even after a rewrite leaves the
    /// store's raw address non-decomposable.
    fn store_sized(&self, function: &Function, store: NodeId) -> SizedAddr {
        SizedAddr {
            class: classify_store_addr(function, store),
            size: store_value_byte_size(function, function.store_data(store)),
        }
    }

    /// Decompose an address into an SP terminal via the function's cache.  The
    /// single decompose entry for consumers outside this module.
    pub(crate) fn decompose(&self, function: &Function, value: ValueId) -> Option<SpExpr> {
        decompose(function, value)
    }

    /// Exact pairwise alias verdict between a `Load` and a `Store`, deriving
    /// each side's [`SizedAddr`] from the node itself (O(1) cached reads /
    /// decompose-memo hits) under this config's alias mode and distinct-base
    /// knob.  The node-based counterpart of the class-based [`alias_verdict`]
    /// primitive.
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

    /// Read-only walk: the nearest clobber of `load` reachable backward from
    /// the def producing the `mem` memory token.  The load's address class,
    /// byte size, and space are derived from the `load` node itself — each an
    /// O(1) cached read (the SP decompose is a memo hit when the caller has
    /// already classified the address), so a caller never threads them in.
    /// Performs no narrowing; a caller that wants to shorten the load's memory
    /// edge onto the returned clobber calls [`crate::mem_ssa::narrow_load_to`].
    pub(crate) fn nearest_clobber(
        &self,
        function: &Function,
        load: NodeId,
        mem: ValueId,
    ) -> NodeId {
        // The load's own space scopes which stores can clobber it.  Every caller
        // passes a `Load` (the class/size derivation below already assumes it).
        let NodeKind::Load(load_space) = *function.node_kind(load) else {
            unreachable!("nearest_clobber is only called on Load nodes");
        };
        let load = self.load_sized(function, load);
        let mem_node = function.producer(mem);
        let mut walker = self.walker(load, load_space);
        walker.find_nearest_clobber(function, mem_node)
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
        &self,
        function: &Function,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
    ) -> Option<ReachingSpStore> {
        // Stack memory lives in RAM, so a probed SP-rooted location only
        // matches RAM stores (a same-address store in another space is
        // disjoint and is skipped, fail-closed for the caller).
        let clobber = {
            let mut walker = self.walker(
                SizedAddr {
                    class: AddrClass::SpRooted { base, offset },
                    size: probe_size,
                },
                rsleigh::VnSpace::RAM,
            );
            // `find_nearest_clobber` is the read-only walk (no narrowing); it
            // resolves the nearest clobber backward from the def producing
            // `mem_start`.
            walker.find_nearest_clobber(function, function.producer(mem_start))
        };
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        // Resolve the store's own SP offset — `decompose` is cache-first (it
        // reads the `stack_offsets` SSoT before walking), so this one call
        // covers both the per-node cache hit and the fresh-walk case.  It must
        // share `base` to be comparable to the probed location.
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

/// The nearest non-clobbered `Store` to an SP-relative location, found via the
/// shared memory-SSA walker.  Returned by [`SpAnalyzer::reaching_store`].
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
        store_value_byte_size(function, self.data(function))
    }
}

/// Bit shift that aligns the load-width sub-word of a wider stored value into
/// the low end before truncation, given the store/load types and byte order.
///
/// The single SSoT for the "extract the `load_ty`-width slice from a wider
/// `store_ty` integer" rule, shared by the `LoadForward` node-building
/// narrowing and the jump-table evaluator's symbolic `reshape`:
///
/// * Little-endian: the load bytes are the LOW bytes — no shift (`0`).
/// * Big-endian: the load bytes are the HIGH bytes — shift right by
///   `(store_bytes - load_bytes) * 8` so they land in the low end.
///
/// Returns the shift in bits.  Callers only invoke it when the load is
/// narrower than the store, so the byte-size subtraction does not underflow.
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

/// Byte size of a `Store`'s DATA slot, used as a range bound for the
/// disjointness check in [`alias_verdict`].  The IR signature guarantees the
/// slot is value-typed for any valid `Store` (`DATA` is an `AnyInt` slot), so a
/// non-value here means malformed IR and panics rather than silently degrading
/// the alias verdict.
#[inline]
pub(crate) fn store_value_byte_size(function: &Function, store_data: ValueId) -> i128 {
    function
        .value_type(store_data)
        .expect("Store data input is a value")
        .byte_size() as i128
}

#[cfg(test)]
mod tests;
