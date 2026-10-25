//! `MemAnalyzer`, the per-function memo the alias queries run through, and
//! its options.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::alias::{SizedAddr, alias_verdict, classify_addr};
use super::arg_window::{ArgWindow, ArgWindowGeometry, scan_arg_window};
use super::decompose::decompose;
use super::index::StoreIndex;
use super::ring::addr_bit_width;
use super::walker::MemWalker;
use super::{AddrClass, AliasVerdict, MemExpr};
use crate::mem_ssa::{Answers, ClobberProbe, MemLayout};

/// What a `MemOptions` is for, which decides the claims it honours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MemPurpose {
    /// `LoadForward`'s forwarding verdict.
    Forward,
    /// `FunctionArgDetect`'s incoming stack-argument probe.
    IncomingArgs,
    /// `CallStackArgCollect`'s outgoing-argument window.
    CallStackArgs,
    /// The jump-table walk's own steps and the narrowing of `LoadForward` and
    /// `FunctionArgDetect`: no call relaxation.  The dispatch value the walk
    /// reads was forwarded under `Forward`'s.
    TableWalk,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct MemOptions {
    pub(super) stack_global_disjoint: bool,
    /// Whether a `Call` / `CallOther` on the probed location's memory chain
    /// shadows it.
    pub(super) calls_block: bool,
    pub(super) distinct_sp_bases_disjoint: bool,
    /// Read only where the outgoing-argument window is consulted, so it is
    /// inert unless [`Self::escape_analysis`] or a non-empty allocator set
    /// opens that path.  [`MemOptions::for_purpose`] says which purpose
    /// honours it.
    pub(super) callee_preserves_stack_args: bool,
    /// Let an SP-rooted `Load` step through a `Call` when the frame is provably
    /// private ([`super::frame_escape`]).
    pub(super) escape_analysis: bool,
    /// Whether any `Call` relaxation applies at all.  Cleared for the inner
    /// walk that computes a call's outgoing-argument window
    /// ([`MemWalker::in_outgoing_arg_area`]), which the relaxations would
    /// otherwise re-enter.
    pub(super) call_relaxations: bool,
}

impl MemOptions {
    /// The options `purpose` runs under: which of `function`'s claims it
    /// honours, one line per field.
    pub(crate) fn for_purpose(purpose: MemPurpose, function: &Function) -> Self {
        use MemPurpose::*;
        let a = function.assumptions();
        Self {
            stack_global_disjoint: a.stack_global_disjoint,
            calls_block: !(purpose == IncomingArgs
                && a.assume_incoming_args_survive_calls
                && !incoming_area_escapes(function)),
            distinct_sp_bases_disjoint: purpose == IncomingArgs && a.distinct_sp_bases_disjoint,
            escape_analysis: purpose == Forward && a.escape_analysis,
            callee_preserves_stack_args: purpose == Forward && a.callee_preserves_stack_args,
            call_relaxations: purpose != TableWalk,
        }
    }

    pub(crate) fn without_call_relaxations(&self) -> Self {
        Self {
            call_relaxations: false,
            ..self.clone()
        }
    }
}

/// Whether a callee may hold a pointer into the incoming arguments' memory,
/// register homes included, the case `assume_incoming_args_survive_calls`
/// leaves out.
fn incoming_area_escapes(function: &Function) -> bool {
    function
        .default_cc()
        .incoming_args_start()
        .is_some_and(|start| super::frame_escape::incoming_area_escapes(function, start))
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
    /// One [`ArgWindow`] per `Call`, scanned on first use.  Keyed by the call
    /// alone: the window is only ever consulted from inside a walk that honours
    /// the `Call` relaxations, so the only options that reach it are
    /// [`Self::options`].
    arg_windows: std::cell::RefCell<FxHashMap<NodeId, ArgWindow>>,
    /// Built on the first query, so a caller that asks none builds none.
    index: std::cell::OnceCell<MemIndex>,
    /// Keyed by the options too: a walk under the relaxation-free row reaches
    /// different defs, so its answers are not the configured row's.
    answers: std::cell::RefCell<FxHashMap<(SizedAddr, rsleigh::VnSpace, MemOptions), Answers>>,
}

struct MemIndex {
    layout: MemLayout,
    /// Absent without a position index, which is what a range of defs means.
    stores: Option<StoreIndex>,
}

/// A [`MemWalker`] asked through a [`StoreIndex`].
struct IndexProbe<'w, 'a> {
    walker: &'w mut MemWalker<'a>,
    stores: Option<&'w StoreIndex>,
}

impl ClobberProbe for IndexProbe<'_, '_> {
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        self.walker.def_clobbers(function, def)
    }

    fn candidate(&mut self, lo: u32, hi: u32) -> Option<u32> {
        self.stores
            .expect("only a positioned layout asks for a candidate")
            .candidate(
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
            index: std::cell::OnceCell::new(),
            answers: std::cell::RefCell::default(),
        }
    }

    /// Drops the memoised answers, for a rewrite that can change a def's
    /// verdict.
    pub(crate) fn forget_answers(&self) {
        self.answers.borrow_mut().clear();
    }

    fn index(&self, function: &Function) -> &MemIndex {
        self.index.get_or_init(|| {
            let layout = MemLayout::build(function);
            MemIndex {
                stores: layout
                    .positions()
                    .map(|_| StoreIndex::build(function, &layout)),
                layout,
            }
        })
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
        debug_assert!(
            *options == self.options || *options == self.options.without_call_relaxations(),
            "a query runs under the analyzer's row or that row less the call relaxations"
        );
        let mut walker = MemWalker {
            analyzer: self,
            options,
            load,
            load_space,
            private_frame: std::cell::Cell::new(None),
        };
        let index = self.index(function);
        let mut probe = IndexProbe {
            walker: &mut walker,
            stores: index.stores.as_ref(),
        };
        let mut memo = memoize.then(|| self.answers.borrow_mut());
        let mut fresh = Answers::default();
        let answers = match &mut memo {
            Some(memo) => memo.entry((load, load_space, options.clone())).or_default(),
            None => &mut fresh,
        };
        index
            .layout
            .nearest_clobber(function, start, &mut probe, answers)
            // A start the layout does not cover reaches `InitialMemory` by no
            // path, so nothing above it is proven and it bounds itself: at or
            // nearer than any clobber in its own cone.
            .unwrap_or(start)
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

    fn load_sized(&self, function: &Function, load: NodeId) -> SizedAddr {
        let class = classify_addr(function, function.load_addr(load));
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
            class: classify_addr(function, function.store_addr(store)),
            size: store_value_byte_size(function, function.store_data(store)),
            addr_bits: addr_bit_width(function, function.store_addr(store)),
        }
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
            // the base's width.
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
        } = decompose(function, function.store_addr(clobber))?;
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
