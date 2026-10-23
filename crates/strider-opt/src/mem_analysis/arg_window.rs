//! The outgoing-argument window of a call and the scan of the stores that
//! fill it.

use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer};

use super::analyzer::{MemAnalyzer, MemOptions, ReachingSpStore};
use super::ring::{wrap_to_addr_width, wrapped_ranges};

/// The callee-owned region of one call's outgoing-argument window, in the
/// call's SP base coordinates.  Read off the call alone, so one walk of the
/// argument prefix answers every probe against that call.
pub(super) struct ArgWindow {
    /// `[start, end)` of each anchored argument slot, ascending and
    /// non-overlapping.  Whole ABI slots, not store extents: a store narrower
    /// than the slot still hands the callee the rest of it.
    ranges: Vec<(i128, i128)>,
    /// The slot the prefix could not be shown to end at, from which the callee
    /// keeps everything above ([`SlotReach::Blinded`]).
    blind_at: Option<i128>,
    /// How far the scan looked.  A probe reaching above this is outside what
    /// the walk decided and needs a wider one.
    pub(super) known_to: i128,
}

impl ArgWindow {
    /// Does `[offset, hi)` meet the owned region?  Mirrors the prefix walk:
    /// the first range ending above `offset` decides, and a range at or above
    /// `hi` is one the probe never reaches.
    pub(super) fn covers(&self, offset: i128, hi: i128) -> bool {
        match self
            .ranges
            .get(self.ranges.partition_point(|&(_, end)| end <= offset))
        {
            Some(&(start, _)) => start < hi,
            None => self.blind_at.is_some_and(|slot| slot < hi),
        }
    }
}

/// Where a call's argument window sits, all of it read off the call.
pub(super) struct ArgWindowGeometry {
    /// The call's memory input, the chain the prefix is scanned along.
    pub(super) mem_start: ValueId,
    pub(super) base: ValueId,
    /// First argument slot.
    pub(super) lo: i128,
    /// SP at the call, the origin [`strider_target::StackArgs`] measures from.
    pub(super) sp_offset: i128,
    pub(super) args: strider_target::StackArgs,
}

/// Walks the contiguous prefix of argument slots up to `hi`, the reach the
/// probing load needs.  For every slot below a narrower `hi`, a wider scan
/// reaches the same verdict: a same-base store is anchored and the scan goes
/// on, any other def decides by its class alone, and a `MemPhi` opaque to the
/// wider window falls back per slot.  So a window scanned for one probe
/// answers a shallower one.
pub(super) fn scan_arg_window(
    function: &Function,
    analyzer: &MemAnalyzer,
    options: &MemOptions,
    geometry: &ArgWindowGeometry,
    hi: i128,
) -> ArgWindow {
    let mut scan = ArgStoreScan::new(
        function,
        analyzer,
        options.without_call_relaxations(),
        geometry.mem_start,
        geometry.base,
        geometry.lo,
        hi,
    );
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = geometry
            .sp_offset
            .saturating_add(geometry.args.offset_of(cursor));
        if slot_off >= hi {
            return ArgWindow {
                ranges,
                blind_at: None,
                known_to: hi,
            };
        }
        let store = match scan.reach_at(function, slot_off) {
            // A partly overwritten anchor still marks bytes the caller wrote,
            // which is all the window asks.
            SlotReach::Anchored { store, .. } => store,
            // Nothing the caller wrote as an argument reaches the slot, so the
            // argument list ends below it.
            //
            // KNOWN LIMIT: an argument slot the caller leaves unwritten (the
            // hole an argument aligned past the slot stride leaves, or an
            // argument it never stores) still belongs to the callee, and this
            // reads it as the end. Continuing past it would blind the window
            // from the hole up, which is indistinguishable from a spill sitting
            // above the arguments and would stop forwarding for every one of
            // those. Only the callee's arity separates the two, and the IR does
            // not have it. Reachable only under `escape_analysis` or a
            // non-empty `noalias_allocators`, both off by default.
            SlotReach::NotAnArgument => {
                return ArgWindow {
                    ranges,
                    blind_at: None,
                    known_to: i128::MAX,
                };
            }
            // The window cannot be shown to end here, so the callee keeps
            // ownership all the way up.
            SlotReach::Blinded => {
                return ArgWindow {
                    ranges,
                    blind_at: Some(slot_off),
                    known_to: i128::MAX,
                };
            }
        };
        // The callee owns the whole ABI slot, not only the bytes the store
        // wrote: a sub-slot argument (`mov [rsp+8], eax` for a 4-byte 7th
        // integer arg on SysV x86-64) leaves the slot's tail callee-writable,
        // and a range stopping at the store width would answer `false` for a
        // probe there.
        cursor += geometry.args.slots_spanned(store.size(function));
        let slot_end = geometry
            .sp_offset
            .saturating_add(geometry.args.offset_of(cursor));
        ranges.push((slot_off, slot_end));
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
pub(crate) struct ArgStoreScan<'a> {
    /// Answers the probes under `options`, from its index.
    analyzer: &'a MemAnalyzer,
    options: MemOptions,
    /// Chain start, for the `MemPhi` fallback probe.
    mem_start: ValueId,
    base: ValueId,
    /// `base`'s type: offsets wrap at its width, so a covered range running
    /// past the top of the signed range resumes at the bottom.
    addr_ty: Option<ValueType>,
    /// The window `[lo, lo + size)` as one probe.
    lo: i128,
    size: i128,
    /// Where the next resume starts, `None` once the scan has stopped.
    cursor: Option<ValueId>,
    /// Merged byte ranges of the stores scanned so far.
    covered: BTreeMap<i128, i128>,
    /// Anchor offset -> the store anchored there, for the anchors no nearer
    /// store already covers.
    anchored: FxHashMap<i128, ReachingSpStore>,
    /// Anchored offsets a nearer store overwrote in part: the range is still
    /// written, the anchor's data is no longer its value.
    overwritten: FxHashSet<i128>,
    /// A loop back-edge can put an already-scanned store back on the chain
    /// below itself, which would make the resume loop forever.
    seen: FxHashSet<NodeId>,
    /// The def that ended the scan: the one reaching every slot the scan left
    /// uncovered, and so the reason the prefix ends where it does.
    stopped_at: Option<NodeId>,
}

impl<'a> ArgStoreScan<'a> {
    pub(crate) fn new(
        function: &Function,
        analyzer: &'a MemAnalyzer,
        options: MemOptions,
        mem_start: ValueId,
        base: ValueId,
        lo: i128,
        hi: i128,
    ) -> Self {
        Self {
            analyzer,
            options,
            mem_start,
            base,
            addr_ty: function.value_type(base).ok(),
            lo,
            size: hi.saturating_sub(lo),
            cursor: Some(mem_start),
            covered: BTreeMap::new(),
            anchored: FxHashMap::default(),
            overwritten: FxHashSet::default(),
            seen: FxHashSet::default(),
            stopped_at: None,
        }
    }

    /// What reaches `slot_off` on the call's memory chain.  Scans further along
    /// it only while what is scanned leaves `slot_off` uncovered.
    pub(crate) fn reach_at(&mut self, function: &Function, slot_off: i128) -> SlotReach {
        let slot_off = wrap_to_addr_width(function, self.base, slot_off);
        while !range_covers(&self.covered, slot_off) {
            if self.advance(function) {
                continue;
            }
            let Some(def) = self.stopped_at else {
                return SlotReach::Blinded;
            };
            if !matches!(function.node_kind(def), NodeKind::MemPhi) {
                return classify_reaching_def(function, def);
            }
            // A `MemPhi` clobbers the probe that met it, not necessarily this
            // slot: its arms can still agree on this one, which only the slot's
            // own probe can say.
            let def = self.analyzer.nearest_sp_clobber(
                function,
                &self.options,
                self.mem_start,
                self.base,
                slot_off,
                1,
            );
            return match self.analyzer.anchored_store(function, def, self.base) {
                Some(hit) if hit.store_offset == slot_off => {
                    let size = hit.size(function);
                    let end = slot_off.saturating_add(size);
                    // The probe saw one byte: a path patching the rest of the
                    // store shows only to a probe of its full width.
                    let whole = !self.overlaps_covered(slot_off, end)
                        && self.analyzer.nearest_sp_clobber(
                            function,
                            &self.options,
                            self.mem_start,
                            self.base,
                            slot_off,
                            size,
                        ) == def;
                    SlotReach::Anchored { store: hit, whole }
                }
                // Anchored below the slot: it covers the slot without the
                // caller ever having written the slot itself.
                Some(_) => SlotReach::NotAnArgument,
                None => classify_reaching_def(function, def),
            };
        }
        match self.anchored.get(&slot_off) {
            Some(hit) => SlotReach::Anchored {
                store: *hit,
                whole: !self.overwritten.contains(&slot_off),
            },
            // A store anchored below covers the slot, so the slot was not
            // written as a slot.
            None => SlotReach::NotAnArgument,
        }
    }

    /// Resumes the scan at the next store, `false` once it stops.
    fn advance(&mut self, function: &Function) -> bool {
        let Some(cur) = self.cursor.take() else {
            return false;
        };
        let def = self.analyzer.nearest_sp_clobber(
            function,
            &self.options,
            cur,
            self.base,
            self.lo,
            self.size,
        );
        if !self.seen.insert(def) {
            self.stopped_at = Some(def);
            return false;
        }
        let Some(hit) = self.analyzer.anchored_store(function, def, self.base) else {
            self.stopped_at = Some(def);
            return false;
        };
        let end = hit.store_offset.saturating_add(hit.size(function));
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
        self.cursor = function.memory_input_of(def);
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
pub(crate) enum SlotReach {
    /// A store anchored exactly at the slot.  `whole` is false when a nearer
    /// store overwrote part of it: the caller still wrote the range, but the
    /// anchor's data is not what the slot holds.
    Anchored { store: ReachingSpStore, whole: bool },
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
