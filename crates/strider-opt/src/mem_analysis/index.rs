//! The defs of a [`MemLayout`] grouped by what they can clobber, so a climb asks
//! for the highest possible clobber of a location in a position range without
//! visiting the range.
//!
//! Every answer is a superset: [`MemWalker::def_clobbers`] still decides each
//! candidate.  A store is classified once, when the index is built; a store
//! whose address a later rewrite can still change is one whose address
//! depended on a load, which classifies as an `Anchor` and is a candidate for
//! every location.

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::{
    AddrClass, MemOptions, SizedAddr, addr_bit_width, classify_addr, store_value_byte_size,
};
use crate::mem_ssa::{MemLayout, Shape};

const NONE: u32 = u32::MAX;

/// The highest of the ascending `positions` in `lo..=hi`.
fn highest_in(positions: &[u32], lo: u32, hi: u32) -> Option<u32> {
    let i = positions.partition_point(|&p| p <= hi);
    (i > 0 && positions[i - 1] >= lo).then(|| positions[i - 1])
}

/// Ascending positions, each with a key.
struct Keyed<K> {
    pos: Vec<u32>,
    key: Vec<K>,
    /// Index of the nearest earlier entry with a different key.
    prev_other: Vec<u32>,
}

impl<K> Default for Keyed<K> {
    fn default() -> Self {
        Self {
            pos: Vec::new(),
            key: Vec::new(),
            prev_other: Vec::new(),
        }
    }
}

impl<K: Copy + Eq> Keyed<K> {
    fn push(&mut self, pos: u32, key: K) {
        let prev = match self.key.last() {
            None => NONE,
            Some(&last) if last != key => self.key.len() as u32 - 1,
            Some(_) => *self.prev_other.last().expect("parallel to key"),
        };
        self.pos.push(pos);
        self.key.push(key);
        self.prev_other.push(prev);
    }

    fn highest(&self, lo: u32, hi: u32) -> Option<u32> {
        highest_in(&self.pos, lo, hi)
    }

    /// The highest position in `lo..=hi` whose key is not `key`.
    fn highest_not(&self, key: K, lo: u32, hi: u32) -> Option<u32> {
        let i = self.pos.partition_point(|&p| p <= hi);
        let mut j = i.checked_sub(1)?;
        if self.key[j] == key {
            let other = self.prev_other[j];
            if other == NONE {
                return None;
            }
            j = other as usize;
        }
        (self.pos[j] >= lo).then(|| self.pos[j])
    }
}

/// Stores whose offsets share one coordinate system and one address width.
#[derive(Default)]
struct Offsets {
    /// Start offset, then each store width at it with its positions.
    starts: BTreeMap<i128, Vec<(i128, Vec<u32>)>>,
    all: Vec<u32>,
    /// Stores whose end saturates the carrier, which no range test separates.
    unbounded: Vec<u32>,
    max_size: i128,
    min_start: i128,
    max_start: i128,
}

impl Offsets {
    fn push(&mut self, pos: u32, start: i128, size: i128) {
        if self.all.is_empty() {
            self.min_start = start;
            self.max_start = start;
        } else {
            self.min_start = self.min_start.min(start);
            self.max_start = self.max_start.max(start);
        }
        self.all.push(pos);
        self.max_size = self.max_size.max(size);
        if start.saturating_add(size) == i128::MAX {
            self.unbounded.push(pos);
            return;
        }
        let widths = self.starts.entry(start).or_default();
        match widths.iter_mut().find(|(w, _)| *w == size) {
            Some((_, positions)) => positions.push(pos),
            None => widths.push((size, vec![pos])),
        }
    }

    /// The highest store in `lo..=hi` whose verdict against `[offset, offset +
    /// size)` at `bits` can be anything but `Disjoint`.
    fn highest(&self, offset: i128, size: i128, bits: u32, lo: u32, hi: u32) -> Option<u32> {
        // Past half the modulus two offsets name no distance, and at exactly
        // 128 bits none is comparable (see `offsets_comparable`).
        if bits == 128 || offset.saturating_add(size) == i128::MAX {
            return highest_in(&self.all, lo, hi);
        }
        if bits < 128 {
            let half = 1i128 << (bits - 1);
            let bound = half
                .saturating_sub(size.max(self.max_size))
                .max(0)
                .unsigned_abs();
            if offset.abs_diff(self.min_start) > bound || offset.abs_diff(self.max_start) > bound {
                return highest_in(&self.all, lo, hi);
            }
        }
        let mut best = highest_in(&self.unbounded, lo, hi);
        let from = offset.saturating_sub(self.max_size.max(1) - 1);
        let to = offset.saturating_add(size.max(1) - 1);
        for (&start, widths) in self.starts.range(from..=to) {
            for (width, positions) in widths {
                if start == offset || start.saturating_add(*width) > offset {
                    best = best.max(highest_in(positions, lo, hi));
                }
            }
        }
        best
    }
}

/// Stores rooted at one base, split by address width.
#[derive(Default)]
struct Rooted {
    widths: Keyed<Option<u32>>,
    offsets: FxHashMap<Option<u32>, Offsets>,
}

impl Rooted {
    fn push(&mut self, pos: u32, offset: i128, size: i128, bits: Option<u32>) {
        self.widths.push(pos, bits);
        self.offsets
            .entry(bits)
            .or_default()
            .push(pos, offset, size);
    }

    fn highest(&self, probe: &SizedAddr, offset: i128, lo: u32, hi: u32) -> Option<u32> {
        // Another width, or none at all, is never comparable.
        let other = self.widths.highest_not(probe.addr_bits, lo, hi);
        let same = match probe.addr_bits {
            None => self.widths.highest(lo, hi),
            Some(bits) => self
                .offsets
                .get(&Some(bits))
                .and_then(|o| o.highest(offset, probe.size, bits, lo, hi)),
        };
        other.max(same)
    }
}

#[derive(Default)]
struct SpaceStores {
    all: Vec<u32>,
    anchor: Vec<u32>,
    heap_opaque: Vec<u32>,
    heap_any: Vec<u32>,
    stack_any: Keyed<ValueId>,
    constant_any: Vec<u32>,
    stack: FxHashMap<ValueId, Rooted>,
    heap: FxHashMap<ValueId, Rooted>,
    constant: Rooted,
}

pub(crate) struct StoreIndex {
    /// Defs that are neither a store nor a call, which clobber every location.
    opaque: Vec<u32>,
    /// Calls not declared `preserves_memory`.
    calls: Vec<u32>,
    spaces: FxHashMap<rsleigh::VnSpace, SpaceStores>,
}

impl StoreIndex {
    pub(crate) fn build(
        function: &Function,
        layout: &MemLayout,
        noalias_allocators: &rustc_hash::FxHashSet<u64>,
    ) -> Self {
        let mut index = Self {
            opaque: Vec::new(),
            calls: Vec::new(),
            spaces: FxHashMap::default(),
        };
        for pos in 0..layout.len() as u32 {
            if layout.shape_at(pos) != Shape::Def {
                continue;
            }
            let def = layout.node_at(pos);
            match *function.node_kind(def) {
                NodeKind::Store(space) => {
                    let addr = function.store_addr(def);
                    let class = classify_addr(function, addr, noalias_allocators);
                    let size = store_value_byte_size(function, function.store_data(def));
                    let bits = addr_bit_width(function, addr);
                    let s = index.spaces.entry(space).or_default();
                    s.all.push(pos);
                    match class {
                        AddrClass::Anchor { .. } => s.anchor.push(pos),
                        AddrClass::HeapOpaque => {
                            s.heap_opaque.push(pos);
                            s.heap_any.push(pos);
                        }
                        AddrClass::HeapRooted { base, offset } => {
                            s.heap_any.push(pos);
                            s.heap
                                .entry(base)
                                .or_default()
                                .push(pos, offset, size, bits);
                        }
                        AddrClass::StackRooted { base, offset } => {
                            s.stack_any.push(pos, base);
                            s.stack
                                .entry(base)
                                .or_default()
                                .push(pos, offset, size, bits);
                        }
                        AddrClass::Constant { addr } => {
                            s.constant_any.push(pos);
                            s.constant.push(pos, addr, size, bits);
                        }
                    }
                }
                NodeKind::Call { .. } => {
                    if !function.get_cc(def).preserves_memory {
                        index.calls.push(pos);
                    }
                }
                _ => index.opaque.push(pos),
            }
        }
        index
    }

    /// The highest def in `lo..=hi` that may clobber `probe` in `space` under
    /// `options`.
    pub(crate) fn candidate(
        &self,
        probe: &SizedAddr,
        space: rsleigh::VnSpace,
        options: &MemOptions,
        lo: u32,
        hi: u32,
    ) -> Option<u32> {
        let mut best = highest_in(&self.opaque, lo, hi);
        // Without `calls_block` every call steps through, relaxed or not.
        if options.calls_block {
            best = best.max(highest_in(&self.calls, lo, hi));
        }
        let Some(s) = self.spaces.get(&space) else {
            return best;
        };
        let stores = match probe.class {
            AddrClass::Anchor { .. } => highest_in(&s.all, lo, hi),
            AddrClass::StackRooted { base, offset } => {
                let mut b = s
                    .stack
                    .get(&base)
                    .and_then(|r| r.highest(probe, offset, lo, hi));
                if !options.distinct_sp_bases_disjoint {
                    b = b.max(s.stack_any.highest_not(base, lo, hi));
                }
                if !options.stack_global_disjoint {
                    b = b.max(highest_in(&s.constant_any, lo, hi));
                }
                b.max(highest_in(&s.anchor, lo, hi))
            }
            AddrClass::Constant { addr } => {
                let mut b = s.constant.highest(probe, addr, lo, hi);
                if !options.stack_global_disjoint {
                    b = b.max(s.stack_any.highest(lo, hi));
                }
                b.max(highest_in(&s.anchor, lo, hi))
            }
            AddrClass::HeapRooted { base, offset } => s
                .heap
                .get(&base)
                .and_then(|r| r.highest(probe, offset, lo, hi))
                .max(highest_in(&s.heap_opaque, lo, hi))
                .max(highest_in(&s.anchor, lo, hi)),
            AddrClass::HeapOpaque => {
                highest_in(&s.heap_any, lo, hi).max(highest_in(&s.anchor, lo, hi))
            }
        };
        best.max(stores)
    }
}
