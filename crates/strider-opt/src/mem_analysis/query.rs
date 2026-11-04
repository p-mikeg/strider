//! `MemQuery`, the per-pass memo the alias queries run through, and the
//! `Policy` each query runs under.

use std::cell::{OnceCell, RefCell};

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::alias::{Loc, alias_verdict};
use super::arg_window::ArgPrefix;
use super::decompose::decompose;
use super::index::StoreIndex;
use super::ring::addr_bit_width;
use super::walker::MemWalker;
use super::{AddrClass, AliasVerdict, MemExpr};
use crate::mem_ssa::{Answers, MemLayout};

/// Which of the function's claims a query honours.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Policy {
    pub(super) stack_global_disjoint: bool,
    pub(super) distinct_sp_bases_disjoint: bool,
    pub(super) calls: Calls,
}

/// What a `Call` on the probed location's memory chain does to it.  A
/// `preserves_memory` call steps through under every policy, a `CallOther`
/// under none.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Calls {
    /// Every call shadows it.
    Block,
    /// No call shadows it.
    Pass,
    /// `LoadForward`'s relaxations: a listed allocator steps through a
    /// private-frame slot outside its argument window and an allocation it
    /// does not return; `escape_analysis` lets any call step through that
    /// slot, and lets an `Anchor` store too; `callee_preserves_stack_args`
    /// empties the window.
    Forward,
}

impl Policy {
    /// No call relaxation: the jump-table walk's own steps, the row a
    /// permanent rewire may rest on, since the walks that read a narrowed
    /// edge honour none, the row a call's argument window is scanned under,
    /// which a relaxation would re-enter, and `CallStackArgCollect`'s, whose
    /// arguments a later reader takes as settled.
    pub(crate) fn strict(function: &Function) -> Self {
        Self::with_calls(function, Calls::Block)
    }

    /// `LoadForward`'s forwarding verdict.
    pub(crate) fn forward(function: &Function) -> Self {
        Self::with_calls(function, Calls::Forward)
    }

    /// `FunctionArgDetect`'s incoming stack-argument probe.
    pub(crate) fn incoming_args(function: &Function) -> Self {
        let a = function.assumptions();
        let calls = if a.assume_incoming_args_survive_calls
            && !super::frame_escape::incoming_area_escapes(function)
        {
            Calls::Pass
        } else {
            Calls::Block
        };
        Self {
            distinct_sp_bases_disjoint: a.distinct_sp_bases_disjoint,
            ..Self::with_calls(function, calls)
        }
    }

    fn with_calls(function: &Function, calls: Calls) -> Self {
        Self {
            stack_global_disjoint: function.assumptions().stack_global_disjoint,
            distinct_sp_bases_disjoint: false,
            calls,
        }
    }
}

/// The SP-aware query surface, one per pass.  Takes the `&Function` and the
/// [`Policy`] per call rather than binding them, so a query may be
/// interleaved with `&mut` edits and one memo serves every policy.
///
/// Its index, arg windows and per-location answers are read off the graph on
/// first use and kept for the query's life, so its owner decides how long
/// they may outlive the graph state they were read from.  Edits that leave
/// every memory def and the values in its address and SP slots in place keep
/// all three valid: every verdict reads the memory classes, keyed by value and
/// dropped by the pipeline only between passes, so a recompute reads the
/// classes the memo did.  An edit that moves a store's address onto another
/// value must [`Self::forget_answers`], since that value's class is read
/// fresh.  The other two survive it: the moved value was a `Load`'s, which the
/// index classes an `Anchor` and offers every location, and which places no
/// window (`tests::stale_window_is_conservative` covers a window under a
/// dropped class).  A call's SP moved off a `Load` needs nothing: a stack
/// address reloaded from memory was stored, which escapes the frame, so no
/// call steps through a stack probe whatever its window.
#[derive(Default)]
pub(crate) struct MemQuery {
    /// Built on the first query, so a caller that asks none builds none.  A
    /// function of the graph alone; the policy enters at
    /// [`StoreIndex::candidate`].
    index: OnceCell<MemIndex>,
    /// One [`ArgPrefix`] per `Call`, scanned on first use.
    arg_prefixes: RefCell<FxHashMap<NodeId, Option<ArgPrefix>>>,
    answers: RefCell<FxHashMap<(Loc, Policy), Answers>>,
}

struct MemIndex {
    layout: MemLayout,
    stores: StoreIndex,
}

impl MemQuery {
    fn index(&self, function: &Function) -> &MemIndex {
        self.index.get_or_init(|| {
            let layout = MemLayout::build(function);
            MemIndex {
                stores: StoreIndex::build(function, &layout),
                layout,
            }
        })
    }

    /// Drops the memoised answers, for an edit that moves a store's address
    /// onto another value; the type doc says why the index and the windows
    /// stand.
    pub(crate) fn forget_answers(&self) {
        self.answers.borrow_mut().clear();
    }

    /// The nearest clobber of `loc` backward from `start`'s memory output,
    /// under `policy`.
    fn nearest(
        &self,
        function: &Function,
        start: NodeId,
        loc: Loc,
        policy: Policy,
        memoize: bool,
    ) -> NodeId {
        debug_assert!(
            !policy.distinct_sp_bases_disjoint
                || !matches!(loc.class, AddrClass::StackRooted { base, .. } if !function.is_entry_sp(base)),
            "distinct SP bases are disjoint only for a probe at the entry SP"
        );
        let index = self.index(function);
        let mut walker = MemWalker {
            query: self,
            policy,
            loc,
            stores: Some(&index.stores),
            private_frame: OnceCell::new(),
        };
        let mut memo = memoize.then(|| self.answers.borrow_mut());
        let mut fresh = Answers::default();
        let answers = match &mut memo {
            Some(memo) => memo.entry((loc, policy)).or_default(),
            None => &mut fresh,
        };
        index
            .layout
            .nearest_clobber(function, start, &mut walker, answers)
            // A start the layout does not cover reaches `InitialMemory` by no
            // path, so nothing above it is proven and it bounds itself: at or
            // nearer than any clobber in its own cone.
            .unwrap_or(start)
    }

    /// Whether `[offset, hi)` meets `call`'s argument window
    /// ([`ArgPrefix::covers`]), scanning the prefix on a miss.  An unplaced
    /// prefix covers everything.
    pub(super) fn arg_window_covers(
        &self,
        function: &Function,
        call: NodeId,
        offset: i128,
        hi: i128,
    ) -> bool {
        if !self.arg_prefixes.borrow().contains_key(&call) {
            let prefix = ArgPrefix::scan(function, self, call);
            self.arg_prefixes.borrow_mut().insert(call, prefix);
        }
        self.arg_prefixes.borrow()[&call]
            .as_ref()
            .is_none_or(|prefix| prefix.covers(offset, hi))
    }

    /// The nearest def of `load`'s location backward from its memory input
    /// under `policy`, read as what it is to the load.  Read-only;
    /// [`Self::reach_and_narrow`] also rewires.
    ///
    /// The verdict is the walk's own: the climb stops at the stores
    /// [`alias_verdict`] calls anything but `Disjoint`, so re-asking it for
    /// the def it stopped at is the same question answered twice, except at
    /// a start its layout does not cover, which bounds itself and can be a
    /// store of any space.
    pub(crate) fn reaching(
        &self,
        function: &Function,
        load: NodeId,
        policy: Policy,
    ) -> ReachingDef {
        let probe = Loc::read_by(function, load);
        let start = function.producer(function.load_mem(load));
        let node = self.nearest(function, start, probe, policy, true);
        match *function.node_kind(node) {
            NodeKind::Store(_)
                if alias_verdict(probe, Loc::written_by(function, node), policy)
                    == AliasVerdict::Match =>
            {
                ReachingDef::Match(node)
            }
            NodeKind::InitialMemory => ReachingDef::Clean(node),
            _ => ReachingDef::Blocked(node),
        }
    }

    /// `load`'s memory edge moved onto its nearest clobber under
    /// [`Policy::strict`], never a relaxed policy's: the walks that read the
    /// edge afterwards (the jump-table walk, return-target verification with
    /// its claim off) honour no call relaxation.  Then [`Self::reaching`] under
    /// `policy` from the moved edge: every relaxation clobbers a subset of what
    /// the strict policy does, so the defs it skipped are skipped here too.
    pub(crate) fn reach_and_narrow(
        &self,
        edit: &mut crate::EditFunction<'_>,
        load: NodeId,
        policy: Policy,
    ) -> ReachingDef {
        let function = edit.function();
        let clobber = self
            .reaching(function, load, Policy::strict(function))
            .node();
        narrow_load_to(edit, load, clobber);
        self.reaching(edit.function(), load, policy)
    }

    /// The nearest def of the slot `[offset, offset + probe_size)` in `space`
    /// relative to SP terminal `base` on the chain from `mem_start`, under
    /// `policy`; a same-address store in another space is disjoint and
    /// skipped.  [`Self::anchored_store`] reads it as a store at `base`.
    ///
    /// `probe_size` is the caller's call: pass the access width to catch a
    /// partial tail-overlap as a clobber, or `1` to merely discover a store and
    /// read its natural width back off [`ReachingSpStore::size`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn nearest_sp_clobber(
        &self,
        function: &Function,
        policy: Policy,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
        space: rsleigh::VnSpace,
    ) -> NodeId {
        let probe = sp_slot(function, base, offset, probe_size, space);
        self.nearest(function, function.producer(mem_start), probe, policy, false)
    }

    /// `clobber` read as a `Store` whose own SP offset shares `base`, the only
    /// shape comparable to a probed location.
    pub(crate) fn anchored_store(
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

/// Repoints a `Load`'s memory input onto `clobber`'s memory output, skipping
/// the proven-disjoint defs in between.
///
/// Sound only for a `Load`: it is a pure consumer with no memory output, so
/// moving its single incoming memory edge is invisible to every other node.
/// Idempotent, and monotone across fixed-point iterations (verdicts only ever
/// move MayAlias to Disjoint).  Returns whether the edge moved.
pub(crate) fn narrow_load_to(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    clobber: NodeId,
) -> bool {
    let rewire = {
        let function = edit.function();
        // Checked in release too: a `Store` also has a memory input at this
        // slot, so falling through would rewire one.
        assert!(
            matches!(function.node_kind(load), NodeKind::Load(_)),
            "narrow_load_to on a non-Load"
        );
        let target_mem = function
            .memory_output_of(clobber)
            .expect("a clobber node has a memory output");
        let mem_use = function
            .node_input_id_at(load, NodeKind::ACCESS_MEM_SLOT)
            .expect("a Load has a memory input");
        let cur_mem = function.node_inputs(load)[NodeKind::ACCESS_MEM_SLOT];
        (cur_mem != target_mem).then_some((mem_use, target_mem))
    };
    if let Some((mem_use, target_mem)) = rewire {
        edit.update_input(mem_use, target_mem);
    }
    rewire.is_some()
}

/// `[offset, offset + size)` relative to SP terminal `base`, in `space`.
fn sp_slot(
    function: &Function,
    base: ValueId,
    offset: i128,
    size: i128,
    space: rsleigh::VnSpace,
) -> Loc {
    Loc {
        class: AddrClass::StackRooted { base, offset },
        size,
        // `offset` came from `decompose`, so it is already reduced at the
        // base's width.
        bits: addr_bit_width(function, base),
        space,
    }
}

/// The nearest def of a load's location, read as what it is to the load.
pub(crate) enum ReachingDef {
    /// A `Store` at the load's own address, whose value the load may be
    /// forwarded.
    Match(NodeId),
    /// `InitialMemory`: no def on the chain reaches the location.
    Clean(NodeId),
    /// Any other store, a `Call`, a `MemPhi` or an opaque def: something
    /// writes the location and the load cannot read what.
    Blocked(NodeId),
}

impl ReachingDef {
    /// The def itself, which is the memory edge a narrowing rewires onto.
    pub(crate) fn node(self) -> NodeId {
        match self {
            Self::Match(node) | Self::Clean(node) | Self::Blocked(node) => node,
        }
    }
}

/// Result of [`MemQuery::anchored_store`].
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

    /// One past the last byte written.
    pub(crate) fn end(&self, function: &Function) -> i128 {
        self.store_offset.saturating_add(self.size(function))
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
