use std::collections::BTreeMap;

use rustc_hash::FxHashSet;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer};

use super::query::{MemQuery, Policy, ReachingSpStore};
use super::ring::{wrap_to_addr_width, wrapped_ranges};

/// One call's outgoing-argument prefix: the contiguous argument slots from the
/// first upward, each with the store anchoring it, under [`Policy::strict`].
pub(crate) struct ArgPrefix {
    /// Ascending and non-overlapping.
    slots: Vec<ArgSlot>,
    /// The slots before the first whose first byte the scan could not see.
    window_len: usize,
    /// The slot from which the callee keeps everything above, the scan having
    /// been blinded there. `None` when the prefix ends at a visible
    /// non-argument.
    blind_from: Option<i128>,
}

/// One anchored argument slot of an [`ArgPrefix`].
pub(crate) struct ArgSlot {
    /// `[start, end)` in whole ABI slots, not the store's extent.
    start: i128,
    end: i128,
    pub(crate) store: ReachingSpStore,
    /// False when a nearer store overwrote part of the anchor.
    pub(crate) whole: bool,
    /// From the slot's start to the store's end.
    pub(crate) span: i128,
}

impl ArgPrefix {
    /// `call`'s prefix, `None` when its convention declares no stack
    /// arguments or its SP decomposes to no stack base.
    pub(crate) fn scan(function: &Function, query: &MemQuery, call: NodeId) -> Option<Self> {
        let args = function.get_cc(call).stack_args?;
        let (base, sp_offset) = super::decompose::call_sp(function, call)?;
        let mem = function
            .memory_input_of(call)
            .expect("a Call has a memory input");
        // The arity is unknown, so the window is `[first slot, inf)`.
        let mut scan = ArgStoreScan::new(
            function,
            query,
            mem,
            base,
            args.slot_offset(sp_offset, 0),
            i128::MAX,
        );
        let mut prefix = Self {
            slots: Vec::new(),
            window_len: 0,
            blind_from: None,
        };
        let mut cursor = 0usize;
        let mut previous = None;
        loop {
            // A saturated slot offset ends it.
            let start = args.slot_offset(sp_offset, cursor);
            if previous == Some(start) {
                break;
            }
            previous = Some(start);
            match scan.reach_at(function, start, args.increment) {
                SlotReach::Anchored {
                    store,
                    whole,
                    span,
                    hidden_head,
                } => {
                    // Past a hidden first byte the store may reach in from
                    // below, so the callee keeps everything from here up.
                    if hidden_head && prefix.blind_from.is_none() {
                        prefix.window_len = prefix.slots.len();
                        prefix.blind_from = Some(start);
                    }
                    cursor += args.slots_spanned(span);
                    prefix.slots.push(ArgSlot {
                        start,
                        end: args.slot_offset(sp_offset, cursor),
                        store,
                        whole,
                        span,
                    });
                }
                // Known limit: an unwritten argument slot (an alignment hole)
                // reads as the end; without the callee's arity it looks like a
                // spill above the arguments.
                SlotReach::NotAnArgument => break,
                SlotReach::Blinded => {
                    if prefix.blind_from.is_none() {
                        prefix.window_len = prefix.slots.len();
                        prefix.blind_from = Some(start);
                    }
                    break;
                }
            }
        }
        if prefix.blind_from.is_none() {
            prefix.window_len = prefix.slots.len();
        }
        Some(prefix)
    }

    /// Every anchored slot, ascending.
    pub(crate) fn slots(&self) -> &[ArgSlot] {
        &self.slots
    }

    /// Whether `[offset, hi)` meets the callee-owned region.
    pub(super) fn covers(&self, offset: i128, hi: i128) -> bool {
        let window = &self.slots[..self.window_len];
        match window.get(window.partition_point(|slot| slot.end <= offset)) {
            Some(slot) => slot.start < hi,
            None => self.blind_from.is_some_and(|slot| slot < hi),
        }
    }
}

/// One lazy reverse scan of a call's memory chain for the stores anchoring its
/// outgoing-argument slots: it probes the whole window and resumes past each
/// store, so all slots cost one traversal. A slot left uncovered at a `MemPhi`
/// falls back to its own probe.
struct ArgStoreScan<'a> {
    query: &'a MemQuery,
    policy: Policy,
    /// Chain start, for the `MemPhi` fallback probe.
    mem_start: ValueId,
    base: ValueId,
    /// `base`'s type, whose width offsets wrap at.
    addr_ty: Option<ValueType>,
    /// The window `[lo, lo + size)` as one probe.
    lo: i128,
    size: i128,
    at: ScanAt,
    /// Merged byte ranges of the stores scanned so far.
    covered: BTreeMap<i128, i128>,
    /// The store anchored at each offset no nearer store covers.
    anchored: BTreeMap<i128, ReachingSpStore>,
    /// Anchored offsets a nearer store overwrote in part.
    overwritten: FxHashSet<i128>,
    /// Scanned stores; a back edge can put one back on the chain.
    seen: FxHashSet<NodeId>,
    /// Slots read by their own probe past a `MemPhi`.
    fallbacks: u32,
}

/// Slots a scan reads by their own probe before reading the rest as
/// [`SlotReach::Blinded`].
const MAX_FALLBACKS: u32 = 64;

/// Where an [`ArgStoreScan`] stands.
#[derive(Clone, Copy)]
enum ScanAt {
    /// The memory value the next resume starts from.
    Resume(ValueId),
    /// The def that ended the scan, reaching every slot left uncovered.
    Stopped(NodeId),
}

impl<'a> ArgStoreScan<'a> {
    fn new(
        function: &Function,
        query: &'a MemQuery,
        mem_start: ValueId,
        base: ValueId,
        lo: i128,
        hi: i128,
    ) -> Self {
        Self {
            query,
            policy: Policy::strict(function),
            mem_start,
            base,
            addr_ty: function.value_type(base).ok(),
            lo,
            size: hi.saturating_sub(lo),
            at: ScanAt::Resume(mem_start),
            covered: BTreeMap::new(),
            anchored: BTreeMap::new(),
            overwritten: FxHashSet::default(),
            seen: FxHashSet::default(),
            fallbacks: 0,
        }
    }

    /// What reaches the `size`-byte slot at `slot_off`, scanning further only
    /// while `slot_off` is uncovered. A store inside the slot past its first
    /// byte (a right-justified big-endian argument) anchors it when nothing
    /// covers the first byte.
    fn reach_at(&mut self, function: &Function, slot_off: i128, size: i128) -> SlotReach {
        let slot_off = wrap_to_addr_width(function, self.base, slot_off);
        let slot_end = slot_off.saturating_add(size);
        while !range_covers(&self.covered, slot_off) {
            if self.advance(function) {
                continue;
            }
            let inside = slot_off.saturating_add(1)..slot_end;
            if !inside.is_empty()
                && let Some((&at, &hit)) = self.anchored.range(inside).next()
            {
                return SlotReach::Anchored {
                    store: hit,
                    whole: !self.overwritten.contains(&at),
                    span: hit.end(function) - slot_off,
                    hidden_head: self.head_def(function, slot_off).is_none_or(|def| {
                        !matches!(function.node_kind(def), NodeKind::InitialMemory)
                    }),
                };
            }
            let ScanAt::Stopped(def) = self.at else {
                unreachable!("a scan that cannot advance has stopped");
            };
            if !matches!(function.node_kind(def), NodeKind::MemPhi) {
                return classify_reaching_def(function, def);
            }
            // The `MemPhi` clobbered the window, not necessarily this slot.
            let Some(def) = self.fallback(function, slot_off) else {
                return SlotReach::Blinded;
            };
            if !matches!(function.node_kind(def), NodeKind::InitialMemory) {
                return self.probed(function, def, slot_off, slot_end);
            }
            let def = self.nearest(function, slot_off, size);
            return self.probed(function, def, slot_off, slot_end);
        }
        match self.anchored.get(&slot_off) {
            Some(hit) => SlotReach::Anchored {
                store: *hit,
                whole: !self.overwritten.contains(&slot_off),
                span: hit.size(function),
                hidden_head: false,
            },
            // Covered by a store anchored below.
            None => SlotReach::NotAnArgument,
        }
    }

    /// What reaches the uncovered `slot_off` once the scan has stopped;
    /// `None` past [`MAX_FALLBACKS`].
    fn head_def(&mut self, function: &Function, slot_off: i128) -> Option<NodeId> {
        let ScanAt::Stopped(def) = self.at else {
            unreachable!("a scan that cannot advance has stopped");
        };
        if matches!(function.node_kind(def), NodeKind::MemPhi) {
            self.fallback(function, slot_off)
        } else {
            Some(def)
        }
    }

    /// What reaches `slot_off` by its own probe; `None` past
    /// [`MAX_FALLBACKS`].
    fn fallback(&mut self, function: &Function, slot_off: i128) -> Option<NodeId> {
        if self.fallbacks == MAX_FALLBACKS {
            return None;
        }
        self.fallbacks += 1;
        Some(self.nearest(function, slot_off, 1))
    }

    fn nearest(&self, function: &Function, offset: i128, size: i128) -> NodeId {
        self.query.nearest_sp_clobber(
            function,
            self.policy,
            self.mem_start,
            self.base,
            offset,
            size,
            rsleigh::VnSpace::RAM,
        )
    }

    /// `def`, the nearest clobber of `[slot_off, slot_end)` from the chain
    /// start, read as what reaches the slot.
    fn probed(
        &self,
        function: &Function,
        def: NodeId,
        slot_off: i128,
        slot_end: i128,
    ) -> SlotReach {
        match self.query.anchored_store(function, def, self.base) {
            Some(hit) if (slot_off..slot_end).contains(&hit.store_offset) => {
                let size = hit.size(function);
                // A path patching the rest of the store shows only to a
                // probe of its full width.
                let whole = !self.overlaps_covered(hit.store_offset, hit.end(function))
                    && self.nearest(function, hit.store_offset, size) == def;
                SlotReach::Anchored {
                    store: hit,
                    whole,
                    span: hit.end(function) - slot_off,
                    hidden_head: false,
                }
            }
            Some(_) => SlotReach::NotAnArgument,
            None => classify_reaching_def(function, def),
        }
    }

    /// Resumes the scan at the next store, `false` once it stops.
    fn advance(&mut self, function: &Function) -> bool {
        let ScanAt::Resume(cur) = self.at else {
            return false;
        };
        let def = self.query.nearest_sp_clobber(
            function,
            self.policy,
            cur,
            self.base,
            self.lo,
            self.size,
            rsleigh::VnSpace::RAM,
        );
        let hit = self
            .seen
            .insert(def)
            .then(|| self.query.anchored_store(function, def, self.base))
            .flatten();
        let Some(hit) = hit else {
            self.at = ScanAt::Stopped(def);
            return false;
        };
        let end = hit.end(function);
        // A covered start is not this store's to anchor; one overlapped past
        // its start keeps the anchor, marked overwritten.
        if !range_covers(&self.covered, hit.store_offset) {
            self.anchored.insert(hit.store_offset, hit);
            if self.overlaps_covered(hit.store_offset, end) {
                self.overwritten.insert(hit.store_offset);
            }
        }
        self.cover(hit.store_offset, end);
        self.at = ScanAt::Resume(
            function
                .memory_input_of(def)
                .expect("a Store has a memory input"),
        );
        true
    }

    fn cover(&mut self, start: i128, end: i128) {
        for (s, e) in wrapped_ranges(self.addr_ty, start, end) {
            add_range(&mut self.covered, s, e);
        }
    }

    /// Whether any covered byte lies in `[start, end)`, mod 2^w.
    fn overlaps_covered(&self, start: i128, end: i128) -> bool {
        wrapped_ranges(self.addr_ty, start, end)
            .into_iter()
            .any(|(s, e)| range_overlaps(&self.covered, s, e))
    }
}

/// What reaches one argument slot on a call's memory chain.
enum SlotReach {
    /// A store anchored in the slot. `whole` is false when a nearer store
    /// overwrote part of it; `span` runs from the slot's start to the store's
    /// end; `hidden_head` when the store sits past the slot's first byte and
    /// what reaches that byte is unseen.
    Anchored {
        store: ReachingSpStore,
        whole: bool,
        span: i128,
        hidden_head: bool,
    },
    /// A visible def that is not a store anchored at the slot.
    NotAnArgument,
    /// A def the probe cannot see through: whether the slot was written is
    /// unknown.
    Blinded,
}

fn classify_reaching_def(function: &Function, def: NodeId) -> SlotReach {
    match *function.node_kind(def) {
        NodeKind::InitialMemory => SlotReach::NotAnArgument,
        // A call, an opaque def, a `MemPhi` whose arms disagree, or a store
        // the scan could not place: each hides what lies below.
        _ => SlotReach::Blinded,
    }
}

/// Adds `[start, end)` to a set of disjoint, non-touching intervals.
fn add_range(set: &mut BTreeMap<i128, i128>, start: i128, end: i128) {
    if end <= start {
        return;
    }
    let (mut start, mut end) = (start, end);
    if let Some((&s, &e)) = set.range(..=start).next_back()
        && e >= start
    {
        start = s;
        end = end.max(e);
    }
    while let Some((&s, &e)) = set.range(start..=end).next() {
        set.remove(&s);
        end = end.max(e);
    }
    set.insert(start, end);
}

fn range_covers(set: &BTreeMap<i128, i128>, point: i128) -> bool {
    set.range(..=point)
        .next_back()
        .is_some_and(|(_, &e)| e > point)
}

/// Whether any covered byte lies in `[start, end)`.
fn range_overlaps(set: &BTreeMap<i128, i128>, start: i128, end: i128) -> bool {
    end > start
        && (set
            .range(..start)
            .next_back()
            .is_some_and(|(_, &e)| e > start)
            || set.range(start..end).next().is_some())
}
