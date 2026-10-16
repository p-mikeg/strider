//! `MemAnalyzer`, the per-function memo the alias queries run through, and
//! its options.

use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::alias::{SizedAddr, alias_verdict, classify_addr};
use super::arg_window::{ArgWindow, ArgWindowGeometry, scan_arg_window};
use super::decompose::decompose;
use super::index::StoreIndex;
use super::ring::addr_bit_width;
use super::walker::MemWalker;
use super::{AddrClass, AliasVerdict, MemExpr};
use crate::mem_ssa::{Answers, ClobberProbe, MemLayout, WalkAnswers};

#[derive(Clone)]
pub(crate) struct MemOptions {
    pub(super) stack_global_disjoint: bool,
    /// Whether a `Call` / `CallOther` on the probed location's memory chain
    /// shadows it.
    pub(super) calls_block: bool,
    pub(super) distinct_sp_bases_disjoint: bool,
    /// Read only where the outgoing-argument window is consulted, so it is
    /// inert unless [`Self::escape_analysis`] or a non-empty allocator set
    /// opens that path.  Pinned off by [`MemOptions::call_blocking`]; only
    /// [`MemOptions::with_callee_preserves_stack_args`] opts back in.
    pub(super) callee_preserves_stack_args: bool,
    /// Let an SP-rooted `Load` step through a `Call` when the frame is provably
    /// private ([`super::frame_escape`]).
    pub(super) escape_analysis: bool,
    /// Whether any `Call` relaxation applies at all.  Cleared for the inner
    /// walk that computes a call's outgoing-argument window
    /// ([`MemWalker::in_outgoing_arg_area`]), which the relaxations would
    /// otherwise re-enter.
    pub(super) call_relaxations: bool,
    /// Whether a heap class may be claimed at all; off, a heap address is an
    /// [`AddrClass::Anchor`].
    pub(super) heap_claims: bool,
    /// Callee addresses whose return is a fresh heap base: always the run's
    /// [`crate::AssumptionOptions::noalias_allocators`], whatever the policy,
    /// since [`decompose`] memoises under it.
    pub(super) noalias_allocators: FxHashSet<u64>,
}

impl MemOptions {
    /// A `Call` on the memory chain clobbers the probed location, and
    /// distinct SP bases stay conservatively non-disjoint.
    pub(crate) fn call_blocking(
        stack_global_disjoint: bool,
        noalias_allocators: &FxHashSet<u64>,
    ) -> Self {
        Self {
            stack_global_disjoint,
            calls_block: true,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            escape_analysis: false,
            call_relaxations: true,
            heap_claims: true,
            noalias_allocators: noalias_allocators.clone(),
        }
    }

    /// The incoming-stack-argument probe: the knobs scoped to argument
    /// detection, and no private-frame relaxation.
    pub(crate) fn incoming_args(assumptions: &crate::AssumptionOptions) -> Self {
        Self {
            calls_block: !assumptions.assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint: assumptions.distinct_sp_bases_disjoint,
            ..Self::call_blocking(
                assumptions.stack_global_disjoint,
                &assumptions.noalias_allocators,
            )
        }
    }

    /// Every claim off, so a verdict holds under
    /// [`crate::AssumptionOptions::none`].  What a permanent rewire may name,
    /// modulo the per-`Call` `preserves_memory` CC flag, which is the caller's
    /// assertion and outside [`crate::AssumptionOptions`].  The set is still
    /// the run's: a heap address decomposes as heap and reads as an `Anchor`.
    pub(crate) fn structural(noalias_allocators: &FxHashSet<u64>) -> Self {
        Self {
            stack_global_disjoint: false,
            calls_block: true,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            escape_analysis: false,
            call_relaxations: false,
            heap_claims: false,
            noalias_allocators: noalias_allocators.clone(),
        }
    }

    /// Opt into forwarding an SP-rooted load across a private-frame `Call`.
    pub(crate) fn with_escape_analysis(mut self, enabled: bool) -> Self {
        self.escape_analysis = enabled;
        self
    }

    /// Opt into an empty outgoing-argument window
    /// ([`crate::AssumptionOptions::callee_preserves_stack_args`]).
    pub(crate) fn with_callee_preserves_stack_args(mut self, enabled: bool) -> Self {
        self.callee_preserves_stack_args = enabled;
        self
    }

    pub(crate) fn without_call_relaxations(&self) -> Self {
        Self {
            call_relaxations: false,
            ..self.clone()
        }
    }
}

/// The SP-aware query surface.  Takes the `&Function` per call rather than
/// binding it, so a query may be interleaved with `&mut` edits.
///
/// Its index, arg windows and per-location answers are read off the graph on
/// first use and kept for the analyzer's life, so its owner decides how long
/// they may outlive the graph state they were read from.  Edits that leave
/// every memory def and its address in place keep them valid; a rewrite that
/// reaches a store address or a call's SP must [`Self::forget_answers`].
pub(crate) struct MemAnalyzer {
    options: MemOptions,
    /// One [`ArgWindow`] per `Call`, scanned on first use.
    arg_windows: std::cell::RefCell<FxHashMap<NodeId, ArgWindow>>,
    /// `None` walks every query instead; built, `None` inside is a graph
    /// [`MemLayout`] does not answer, which is walked too.
    index: Option<std::cell::OnceCell<Option<MemIndex>>>,
    answers: std::cell::RefCell<FxHashMap<(SizedAddr, rsleigh::VnSpace), Answers>>,
    /// The same, for the queries the index does not answer.
    walk_answers: std::cell::RefCell<FxHashMap<(SizedAddr, rsleigh::VnSpace), WalkAnswers>>,
}

struct MemIndex {
    layout: MemLayout,
    stores: StoreIndex,
}

/// A [`MemWalker`] asked through a [`StoreIndex`].
struct IndexProbe<'w, 'a> {
    walker: &'w mut MemWalker<'a>,
    stores: &'w StoreIndex,
}

impl ClobberProbe for IndexProbe<'_, '_> {
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        self.walker.def_clobbers(function, def)
    }

    fn candidate(&mut self, _layout: &MemLayout, lo: u32, hi: u32) -> Option<u32> {
        self.stores.candidate(
            &self.walker.load,
            self.walker.load_space,
            self.walker.options,
            lo,
            hi,
        )
    }
}

impl MemAnalyzer {
    pub(crate) fn new(options: MemOptions) -> Self {
        Self {
            options,
            arg_windows: std::cell::RefCell::default(),
            index: Some(std::cell::OnceCell::new()),
            answers: std::cell::RefCell::default(),
            walk_answers: std::cell::RefCell::default(),
        }
    }

    /// Answers by walking each query's paths, with no index to build: for a
    /// caller asking a handful of queries of a graph it does not own.
    pub(crate) fn walking(options: MemOptions) -> Self {
        Self {
            index: None,
            ..Self::new(options)
        }
    }

    /// Drops the memoised answers, for a rewrite that can change a def's
    /// verdict.
    pub(crate) fn forget_answers(&self) {
        self.answers.borrow_mut().clear();
        self.walk_answers.borrow_mut().clear();
    }

    fn index(&self, function: &Function) -> Option<&MemIndex> {
        self.index
            .as_ref()?
            .get_or_init(|| {
                MemLayout::build(function).map(|layout| MemIndex {
                    stores: StoreIndex::build(function, &layout, &self.options),
                    layout,
                })
            })
            .as_ref()
    }

    /// The nearest clobber of `load` backward from `start`'s memory output,
    /// under `options`.  `memoize` holds for the analyzer's own options only.
    fn nearest(
        &self,
        function: &Function,
        start: NodeId,
        load: SizedAddr,
        load_space: rsleigh::VnSpace,
        options: &MemOptions,
        memoize: bool,
    ) -> NodeId {
        let mut walker = MemWalker {
            analyzer: self,
            options,
            load,
            load_space,
            private_frame: std::cell::Cell::new(None),
        };
        if let Some(index) = self.index(function) {
            let mut probe = IndexProbe {
                walker: &mut walker,
                stores: &index.stores,
            };
            let found = if memoize {
                let mut answers = self.answers.borrow_mut();
                let answers = answers.entry((load, load_space)).or_default();
                index
                    .layout
                    .nearest_clobber(function, start, &mut probe, answers)
            } else {
                index
                    .layout
                    .nearest_clobber(function, start, &mut probe, &mut Answers::default())
            };
            if let Some(clobber) = found {
                return clobber;
            }
        }
        if memoize {
            let mut answers = self.walk_answers.borrow_mut();
            let answers = answers.entry((load, load_space)).or_default();
            crate::mem_ssa::find_nearest_clobber(
                function,
                start,
                &mut |f, d| walker.def_clobbers(f, d),
                answers,
            )
        } else {
            crate::mem_ssa::find_nearest_clobber(
                function,
                start,
                &mut |f, d| walker.def_clobbers(f, d),
                &mut WalkAnswers::default(),
            )
        }
    }

    /// [`ArgWindow::covers`] against the memo, scanning `call`'s window on a
    /// miss.  A memoised window that did not reach `hi` is rescanned.
    pub(super) fn arg_window_covers(
        &self,
        function: &Function,
        options: &MemOptions,
        call: NodeId,
        geometry: &ArgWindowGeometry,
        offset: i128,
        hi: i128,
    ) -> bool {
        let cached = self
            .arg_windows
            .borrow()
            .get(&call)
            .filter(|window| hi <= window.known_to)
            .map(|window| window.covers(offset, hi));
        if let Some(covered) = cached {
            return covered;
        }
        // Scan PAST the request when a narrower window is already cached, so an
        // ascending run of probes stops missing on every step: the width
        // scanned above `lo` doubles, so one call rescans O(log range) times
        // instead of once per probe. The growth is measured from `lo` because
        // `known_to` is an absolute stack offset, normally negative. A window
        // scanned further is a superset, the same equivalence the
        // `hi <= known_to` reuse above already relies on.
        let scan_hi = match self.arg_windows.borrow().get(&call).map(|w| w.known_to) {
            Some(prev) if prev > geometry.lo => hi.max(
                geometry
                    .lo
                    .saturating_add(prev.saturating_sub(geometry.lo).saturating_mul(2)),
            ),
            _ => hi,
        };
        let window = scan_arg_window(function, self, options, geometry, scan_hi);
        let covered = window.covers(offset, hi);
        // The fresh window always reaches at least as far as any it replaces:
        // the reuse test above consumed every cached entry with
        // `known_to >= hi`, and this scan's `known_to` is `scan_hi` (`>= hi`)
        // or `i128::MAX`.
        //
        // Descending probes reuse the first window outright; ascending ones
        // rescan, but against the doubled `scan_hi` above rather than their own
        // `hi`, so the misses are logarithmic in the range rather than one per
        // probe.
        self.arg_windows.borrow_mut().insert(call, window);
        covered
    }

    pub(crate) fn options(&self) -> &MemOptions {
        &self.options
    }

    pub(crate) fn noalias_allocators(&self) -> &FxHashSet<u64> {
        &self.options.noalias_allocators
    }

    fn load_sized(&self, function: &Function, load: NodeId) -> SizedAddr {
        let class = classify_addr(function, function.load_addr(load), &self.options);
        let (_, ty) = function
            .single_value_output(load)
            .expect("Load has 1 typed output per node signature");
        SizedAddr {
            class,
            size: ty.byte_size() as i128,
            addr_bits: addr_bit_width(function, function.load_addr(load)),
        }
    }

    /// The store's address class paired with its stored width.
    pub(super) fn store_sized(&self, function: &Function, store: NodeId) -> SizedAddr {
        SizedAddr {
            class: classify_addr(function, function.store_addr(store), &self.options),
            size: store_value_byte_size(function, function.store_data(store)),
            addr_bits: addr_bit_width(function, function.store_addr(store)),
        }
    }

    /// See the free [`decompose`].
    pub(crate) fn decompose(&self, function: &Function, value: ValueId) -> Option<MemExpr> {
        decompose(function, value, &self.options.noalias_allocators)
    }

    /// The node-based counterpart of the class-based [`alias_verdict`].
    ///
    /// Distinct address spaces never alias, even at the same numeric offset,
    /// so a cross-space pair is `Disjoint` before any offset is compared.
    /// [`def_clobbers`](MemWalker::def_clobbers) applies the same rule while
    /// walking; a caller re-checking one pair has to apply it too.
    pub(crate) fn verdict(
        &self,
        function: &Function,
        load_node: NodeId,
        store_node: NodeId,
    ) -> AliasVerdict {
        if let (NodeKind::Load(load_space), NodeKind::Store(store_space)) = (
            function.node_kind(load_node),
            function.node_kind(store_node),
        ) && load_space != store_space
        {
            return AliasVerdict::Disjoint;
        }
        alias_verdict(
            self.load_sized(function, load_node),
            self.store_sized(function, store_node),
            &self.options,
        )
    }

    /// Read-only; [`crate::mem_ssa::narrow_load_to`] does the rewire.
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
        self.nearest(
            function,
            function.producer(mem),
            load,
            load_space,
            &self.options,
            true,
        )
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
        let clobber =
            self.nearest_sp_clobber(function, &self.options, mem_start, base, offset, probe_size);
        self.anchored_store(function, clobber, base)
    }

    /// The nearest def of `[offset, offset + probe_size)` relative to SP
    /// terminal `base` on the chain from `mem_start`, under `options`.
    pub(super) fn nearest_sp_clobber(
        &self,
        function: &Function,
        options: &MemOptions,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
    ) -> NodeId {
        let probe = SizedAddr {
            class: AddrClass::StackRooted { base, offset },
            size: probe_size,
            // `offset` came from `decompose`, so it is already reduced at
            // the base's width. Passing `None` would make
            // `offsets_comparable` reject every pair.
            addr_bits: addr_bit_width(function, base),
        };
        // Stack memory lives in RAM, so a same-address store in another space
        // counts as disjoint and is skipped.
        self.nearest(
            function,
            function.producer(mem_start),
            probe,
            rsleigh::VnSpace::RAM,
            options,
            false,
        )
    }

    /// `clobber` read as a `Store` whose own SP offset shares `base`, the only
    /// shape comparable to a probed location.
    pub(super) fn anchored_store(
        &self,
        function: &Function,
        clobber: NodeId,
        base: ValueId,
    ) -> Option<ReachingSpStore> {
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        let MemExpr {
            base: store_base,
            offset: store_offset,
            ..
        } = decompose(
            function,
            function.store_addr(clobber),
            &self.options.noalias_allocators,
        )?;
        (store_base == base).then_some(ReachingSpStore {
            node: clobber,
            store_offset,
        })
    }
}

/// Result of [`MemAnalyzer::reaching_store`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReachingSpStore {
    node: NodeId,
    /// Byte offset from `base`.  Equals the probed `offset` only when the
    /// store is anchored at the probed location.
    pub store_offset: i128,
}

impl ReachingSpStore {
    pub(crate) fn data(&self, function: &Function) -> ValueId {
        function.store_data(self.node)
    }

    /// Byte width of the stored value.
    pub(crate) fn size(&self, function: &Function) -> i128 {
        store_value_byte_size(function, self.data(function))
    }
}

/// Byte width of a `Store`'s DATA operand.  Panics on a non-value operand,
/// which the IR signature already rules out.
#[inline]
pub(crate) fn store_value_byte_size(function: &Function, store_data: ValueId) -> i128 {
    function
        .value_type(store_data)
        .expect("Store data input is a value")
        .byte_size() as i128
}
