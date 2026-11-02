//! The outgoing-argument window of a call and the scan of the stores that
//! fill it.

use std::collections::BTreeMap;

use rustc_hash::FxHashSet;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer};

use super::query::{MemQuery, Policy, ReachingSpStore};
use super::ring::{wrap_to_addr_width, wrapped_ranges};

/// One call's outgoing-argument prefix: the contiguous argument slots from the
/// first upward, each with the store anchoring it, read off one
/// [`ArgStoreScan`] under [`Policy::strict`] to the open top.  A property of
/// the call alone, so one scan serves every reader of its arguments.
pub(crate) struct ArgPrefix {
    /// Ascending and non-overlapping.
    slots: Vec<ArgSlot>,
    /// The slots before the first whose first byte the scan could not see.
    window_len: usize,
    /// The slot from which the callee keeps everything above: one whose first
    /// byte, or whose reaching def, the scan could not see past.  `None` when
    /// the prefix ends at a visible non-argument.
    blind_from: Option<i128>,
}

/// One anchored argument slot of an [`ArgPrefix`].
pub(crate) struct ArgSlot {
    /// `[start, end)` in whole ABI slots, not the store's extent: a store
    /// narrower than the slot still hands the callee the rest of it.
    start: i128,
    end: i128,
    pub(crate) store: ReachingSpStore,
    /// False when a nearer store overwrote part of the anchor: the caller
    /// still wrote the range, but the anchor's data is not what it holds.
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
        // The arity is unknown, so the window is `[first slot, inf)`.  No store
        // on the same base is disjoint from it, so the scan anchors each one,
        // a push below the window included: a cost in work and precision only.
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
            // Open above, so a slot offset that stops advancing (a saturated
            // step past the carrier) is what ends it.
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
                    // Past a hidden first byte a store inside the slot may be
                    // one reaching in from below, not an argument, so the
                    // window keeps everything from here up.
                    if hidden_head && prefix.blind_from.is_none() {
                        prefix.window_len = prefix.slots.len();
                        prefix.blind_from = Some(start);
                    }
                    // The callee owns the whole ABI slot, not only the bytes
                    // the store wrote.
                    cursor += args.slots_spanned(span);
                    prefix.slots.push(ArgSlot {
                        start,
                        end: args.slot_offset(sp_offset, cursor),
                        store,
                        whole,
                        span,
                    });
                }
                // KNOWN LIMIT: an argument slot the caller leaves unwritten
                // (the hole an argument aligned past the slot stride leaves,
                // or an argument it never stores) still belongs to the callee,
                // and this reads it as the end.  Continuing past it would blind
                // the window from the hole up, which is indistinguishable from
                // a spill sitting above the arguments and would stop forwarding
                // for every one of those.  Only the callee's arity separates
                // the two, and the IR does not have it.  Reachable only under
                // `escape_analysis` or a non-empty `noalias_allocators`, both
                // off by default.
                SlotReach::NotAnArgument => break,
                // The window cannot be shown to end here, so the callee keeps
                // ownership all the way up.
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

    /// Does `[offset, hi)` meet the callee-owned region?  The first window
    /// slot ending above `offset` decides, and a slot at or above `hi` is one
    /// the probe never reaches.
    pub(super) fn covers(&self, offset: i128, hi: i128) -> bool {
        let window = &self.slots[..self.window_len];
        match window.get(window.partition_point(|slot| slot.end <= offset)) {
            Some(slot) => slot.start < hi,
            None => self.blind_from.is_some_and(|slot| slot < hi),
        }
    }
}

/// One lazy reverse scan of a call's memory chain for the stores anchoring its
/// outgoing-argument slots, replacing a rescan per slot.
///
/// The scan probes the whole window at once and resumes past each store it
/// finds, so the scanned segments partition the chain: every slot together
/// costs one traversal, where a probe per slot costs one each.
///
/// A per-slot probe reaches the same defs.  Every def kind but `Store` and
/// `MemPhi` clobbers regardless of the probed range, and a `Store` disjoint
/// from the whole window is disjoint from every slot in it, so a def the
/// window probe skips no slot probe would stop at.  `MemPhi` transparency does
/// depend on the probed slot, so a slot left uncovered when the scan stops at
/// one falls back to its own probe.
struct ArgStoreScan<'a> {
    /// Answers the probes under `policy`, from its index.
    query: &'a MemQuery,
    policy: Policy,
    /// Chain start, for the `MemPhi` fallback probe.
    mem_start: ValueId,
    base: ValueId,
    /// `base`'s type: offsets wrap at its width, so a covered range running
    /// past the top of the signed range resumes at the bottom.
    addr_ty: Option<ValueType>,
    /// The window `[lo, lo + size)` as one probe.
    lo: i128,
    size: i128,
    at: ScanAt,
    /// Merged byte ranges of the stores scanned so far.
    covered: BTreeMap<i128, i128>,
    /// Anchor offset -> the store anchored there, for the anchors no nearer
    /// store already covers.
    anchored: BTreeMap<i128, ReachingSpStore>,
    /// Anchored offsets a nearer store overwrote in part: the range is still
    /// written, the anchor's data is no longer its value.
    overwritten: FxHashSet<i128>,
    /// A loop back-edge can put an already-scanned store back on the chain
    /// below itself, which would make the resume loop forever.
    seen: FxHashSet<NodeId>,
    /// Slots read by their own probe past a `MemPhi`, each a climb from the
    /// chain start.
    fallbacks: u32,
}

/// Slots a scan reads by their own probe before it reads the rest as
/// [`SlotReach::Blinded`]: past a `MemPhi` each costs a climb of the chain.
const MAX_FALLBACKS: u32 = 64;

/// Where an [`ArgStoreScan`] stands.
#[derive(Clone, Copy)]
enum ScanAt {
    /// The memory value the next resume starts from.
    Resume(ValueId),
    /// The def that ended the scan: the one reaching every slot the scan left
    /// uncovered, and so the reason the prefix ends where it does.
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

    /// What reaches the `size`-byte slot at `slot_off` on the call's memory
    /// chain.  Scans further along it only while what is scanned leaves
    /// `slot_off` uncovered.
    ///
    /// A store anchored past the slot's first byte but inside the slot (a
    /// big-endian argument right-justified in it) writes the slot too, and is
    /// read as its anchor when nothing covers the first byte.
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
            // A `MemPhi` clobbers the probe that met it, not necessarily this
            // slot: its arms can still agree on this one, which only the slot's
            // own probe can say.
            let Some(def) = self.fallback(function, slot_off) else {
                return SlotReach::Blinded;
            };
            if !matches!(function.node_kind(def), NodeKind::InitialMemory) {
                return self.probed(function, def, slot_off, slot_end);
            }
            // Nothing writes the first byte; the rest of the slot may still
            // hold a store.
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
            // A store anchored below covers the slot, so the slot was not
            // written as a slot.
            None => SlotReach::NotAnArgument,
        }
    }

    /// What reaches `slot_off`'s first byte, which no scanned store covers,
    /// once the scan has stopped; `None` past [`MAX_FALLBACKS`].
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

    /// What reaches `slot_off`'s first byte by the slot's own probe, `None`
    /// once the scan has read [`MAX_FALLBACKS`] slots that way.
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

    /// `def`, the nearest clobber of a probe of the slot `[slot_off,
    /// slot_end)` from the chain start, read as what reaches the slot.
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
                // The probe may have seen only part of the store: a path
                // patching the rest shows only to a probe of its full width.
                let whole = !self.overlaps_covered(hit.store_offset, hit.end(function))
                    && self.nearest(function, hit.store_offset, size) == def;
                SlotReach::Anchored {
                    store: hit,
                    whole,
                    span: hit.end(function) - slot_off,
                    hidden_head: false,
                }
            }
            // Anchored below the slot: it covers the slot without the caller
            // ever having written the slot itself.
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
        // A store an already-scanned one covers is not the nearest reaching
        // def of its own anchor, so it must not claim that anchor.  One
        // overlapped past its start keeps the anchor, since the range is still
        // written, but a nearer store owns some of its bytes.
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
    /// A store anchored exactly at the slot.  `whole` is false when a nearer
    /// store overwrote part of it: the caller still wrote the range, but the
    /// anchor's data is not what the slot holds.
    /// `span` runs from the slot's start to the store's end. `hidden_head`:
    /// the store sits inside the slot and the scan cannot see what reaches
    /// the slot's first byte.
    Anchored {
        store: ReachingSpStore,
        whole: bool,
        span: i128,
        hidden_head: bool,
    },
    /// A visible def that is not a store anchored at the slot, so the caller
    /// did not write it as an argument.
    NotAnArgument,
    /// A def the argument probe cannot see through, leaving it unknown whether
    /// the caller wrote the slot.  Hidden is not absent.
    Blinded,
}

fn classify_reaching_def(function: &Function, def: NodeId) -> SlotReach {
    match *function.node_kind(def) {
        // The clean bottom of the chain.
        NodeKind::InitialMemory => SlotReach::NotAnArgument,
        // A `Call` / `CallOther` hides the caller's own argument stores from a
        // relaxation-free probe, an opaque producer hides everything, a
        // `MemPhi` reaching here has arms that disagree about the slot, and a
        // `Store` reaching here is one the scan could not place (its address
        // does not decompose to this base, or a back-edge put it back on the
        // chain), so it hides whatever lies below it.
        _ => SlotReach::Blinded,
    }
}

/// Adds `[start, end)` to a set of disjoint, non-touching `start -> end`
/// intervals, absorbing every interval it meets.
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
