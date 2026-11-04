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

/// What a `Call` on the probed location's chain does to it. A
/// `preserves_memory` call never clobbers, a `CallOther` always does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Calls {
    /// Every call shadows it.
    Block,
    /// No call shadows it.
    Pass,
    /// The claims' relaxations: a listed allocator, or any call under
    /// `escape_analysis`, passes a private-frame slot outside its argument
    /// window; under `escape_analysis` with `stack_global_disjoint` an
    /// `Anchor` store passes it too. An allocator also passes an allocation
    /// it does not return.
    Forward,
}

impl Policy {
    /// No call relaxation, so a permanent rewire may rest on it.
    pub(crate) fn strict(function: &Function) -> Self {
        Self::with_calls(function, Calls::Block)
    }

    /// `LoadForward`'s forwarding verdict: [`Self::strict`] where no call
    /// relaxation can fire, so the two share their memo.
    pub(crate) fn forward(function: &Function) -> Self {
        let a = function.assumptions();
        let calls = if a.escape_analysis || !a.noalias_allocators.is_empty() {
            Calls::Forward
        } else {
            Calls::Block
        };
        Self::with_calls(function, calls)
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

/// The SP-aware query memo, one per pass. Takes the `&Function` and
/// [`Policy`] per call, so queries interleave with `&mut` edits. Valid across
/// edits that keep every memory def and the values in its address and SP
/// slots; moving a store's address off a `Load`'s value needs
/// [`Self::forget_answers`].
#[derive(Default)]
pub(crate) struct MemQuery {
    /// Built on the first query; policy-free.
    index: OnceCell<MemIndex>,
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
    /// onto another value.
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
            // Unreachable from `InitialMemory`: nothing above it is proven.
            .unwrap_or(start)
    }

    /// Whether `[offset, hi)` meets `call`'s argument window. An unplaced
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
    /// under `policy`, read as what it is to the load.
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

    /// Moves `load`'s memory edge onto its nearest clobber under
    /// [`Policy::strict`], which later walks honour, then [`Self::reaching`]
    /// under `policy`. Every relaxation clobbers a subset of what the strict
    /// policy does, so asking from the moved edge loses nothing.
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

    /// The nearest def of `[offset, offset + probe_size)` in `space` relative
    /// to SP terminal `base`, on the chain from `mem_start`. A `probe_size`
    /// of `1` discovers a store whose width [`ReachingSpStore::size`] reads.
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

    /// `clobber` as a `Store` whose address shares SP terminal `base`.
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

/// Repoints a `Load`'s memory input onto `clobber`'s memory output. Sound
/// only for a `Load`, which has no memory output. Returns whether the edge
/// moved.
pub(crate) fn narrow_load_to(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    clobber: NodeId,
) -> bool {
    let rewire = {
        let function = edit.function();
        // A `Store` has a memory input at this slot too.
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
        // `offset` is already reduced at this width.
        bits: addr_bit_width(function, base),
        space,
    }
}

/// The nearest def of a load's location, read as what it is to the load.
pub(crate) enum ReachingDef {
    /// A `Store` at the load's own address.
    Match(NodeId),
    /// `InitialMemory`: no def on the chain reaches the location.
    Clean(NodeId),
    /// Anything else that may write the location.
    Blocked(NodeId),
}

impl ReachingDef {
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
    /// Byte offset from `base`.
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

/// Byte width of a `Store`'s data operand.
#[inline]
pub(crate) fn store_value_byte_size(function: &Function, store_data: ValueId) -> i128 {
    function
        .value_type(store_data)
        .expect("Store data input is a value")
        .byte_size() as i128
}
