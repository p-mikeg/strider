//! The defs of a [`MemLayout`] grouped by what they can clobber, so a climb
//! finds the highest possible clobber in a position range without visiting
//! it. Answers are supersets; [`super::walker::MemWalker`] decides each. Each
//! store is classified once, at build. An address a forward can still change
//! derives from a load, so it is an `Anchor`, a candidate for every location.

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::AddrClass;
use super::alias::Loc;
use super::query::{Calls, Policy};
use super::ring::{addr_bit_width, ring_overlap_at, ring_window};
use crate::mem_ssa::{MemLayout, Shape};

const NONE: u32 = u32::MAX;

/// The highest of the ascending `positions` in `lo..=hi`.
fn highest_in(positions: &[u32], lo: u32, hi: u32) -> Option<u32> {
    let i = positions.partition_point(|&p| p <= hi);
    (i > 0 && positions[i - 1] >= lo).then(|| positions[i - 1])
}

/// Ascending positions, each with a key.
#[derive(Default)]
struct Keyed<K> {
    pos: Vec<u32>,
    key: Vec<K>,
    /// Index of the nearest earlier entry with a different key.
    prev_other: Vec<u32>,
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
    /// The positions of the stores of each start offset and byte size.
    starts: BTreeMap<(i128, i128), Vec<u32>>,
    all: Vec<u32>,
    max_size: i128,
}

impl Offsets {
    fn push(&mut self, pos: u32, start: i128, size: i128) {
        self.all.push(pos);
        self.max_size = self.max_size.max(size);
        self.starts.entry((start, size)).or_default().push(pos);
    }

    /// The highest store in `lo..=hi` that may overlap `[offset, offset +
    /// size)` at `bits`.
    fn highest(&self, offset: i128, size: i128, bits: u32, lo: u32, hi: u32) -> Option<u32> {
        let all = || highest_in(&self.all, lo, hi);
        // At 128 bits a ring window does not fit `i128`.
        if bits == 128 {
            return all();
        }
        let from = offset.saturating_sub(self.max_size.max(1) - 1);
        let to = offset.saturating_add(size.max(1) - 1);
        let Some(windows) = ring_window(bits, from, to) else {
            return all();
        };
        let mut best = None;
        for (window_lo, window_hi) in windows.into_iter().flatten() {
            for (&(start, width), positions) in self
                .starts
                .range((window_lo, i128::MIN)..=(window_hi, i128::MAX))
            {
                if ring_overlap_at(bits, start, width, offset, size) {
                    best = best.max(highest_in(positions, lo, hi));
                }
            }
        }
        best
    }
}

#[derive(Default)]
struct SpaceStores {
    all: Vec<u32>,
    anchor: Vec<u32>,
    heap_opaque: Vec<u32>,
    heap_any: Vec<u32>,
    stack_any: Keyed<ValueId>,
    /// Per base, at the base's width.
    stack: FxHashMap<ValueId, Offsets>,
    heap: FxHashMap<ValueId, Offsets>,
    /// Per address width.
    constant: FxHashMap<u32, Offsets>,
}

impl SpaceStores {
    /// The highest constant-address store in `lo..=hi`, whatever its address.
    fn any_constant(&self, lo: u32, hi: u32) -> Option<u32> {
        self.constant
            .values()
            .filter_map(|o| highest_in(&o.all, lo, hi))
            .max()
    }
}

pub(crate) struct StoreIndex {
    /// `CallOther`s, which clobber every location.
    opaque: Vec<u32>,
    /// Calls not declared `preserves_memory`.
    calls: Vec<u32>,
    spaces: FxHashMap<rsleigh::VnSpace, SpaceStores>,
}

impl StoreIndex {
    pub(crate) fn build(function: &Function, layout: &MemLayout) -> Self {
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
                NodeKind::Store(_) => {
                    let Loc {
                        class,
                        size,
                        bits,
                        space,
                    } = Loc::written_by(function, def);
                    let s = index.spaces.entry(space).or_default();
                    s.all.push(pos);
                    match class {
                        AddrClass::Anchor { .. } => s.anchor.push(pos),
                        AddrClass::HeapOpaque => {
                            s.heap_opaque.push(pos);
                            s.heap_any.push(pos);
                        }
                        AddrClass::HeapRooted { base, offset } => {
                            assert_eq!(bits, addr_bit_width(function, base));
                            s.heap_any.push(pos);
                            s.heap.entry(base).or_default().push(pos, offset, size);
                        }
                        AddrClass::StackRooted { base, offset } => {
                            assert_eq!(bits, addr_bit_width(function, base));
                            s.stack_any.push(pos, base);
                            s.stack.entry(base).or_default().push(pos, offset, size);
                        }
                        AddrClass::Constant { addr } => {
                            s.constant.entry(bits).or_default().push(pos, addr, size);
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

    /// The highest def in `lo..=hi` that may clobber `probe` under
    /// `policy`.
    pub(crate) fn candidate(&self, probe: &Loc, policy: Policy, lo: u32, hi: u32) -> Option<u32> {
        let mut best = highest_in(&self.opaque, lo, hi);
        if policy.calls != Calls::Pass {
            best = best.max(highest_in(&self.calls, lo, hi));
        }
        let Some(s) = self.spaces.get(&probe.space) else {
            return best;
        };
        let stores = match probe.class {
            AddrClass::Anchor { .. } => highest_in(&s.all, lo, hi),
            AddrClass::StackRooted { base, offset } => {
                let mut b = s
                    .stack
                    .get(&base)
                    .and_then(|o| o.highest(offset, probe.size, probe.bits, lo, hi));
                if !policy.distinct_sp_bases_disjoint {
                    b = b.max(s.stack_any.highest_not(base, lo, hi));
                }
                if !policy.stack_global_disjoint {
                    b = b.max(s.any_constant(lo, hi));
                }
                b.max(highest_in(&s.anchor, lo, hi))
            }
            AddrClass::Constant { addr } => {
                let mut b = s
                    .constant
                    .iter()
                    .filter_map(|(&bits, o)| {
                        if bits == probe.bits {
                            o.highest(addr, probe.size, bits, lo, hi)
                        } else {
                            highest_in(&o.all, lo, hi)
                        }
                    })
                    .max();
                if !policy.stack_global_disjoint {
                    b = b.max(s.stack_any.highest(lo, hi));
                }
                b.max(highest_in(&s.anchor, lo, hi))
            }
            AddrClass::HeapRooted { base, offset } => s
                .heap
                .get(&base)
                .and_then(|o| o.highest(offset, probe.size, probe.bits, lo, hi))
                .max(highest_in(&s.heap_opaque, lo, hi))
                .max(highest_in(&s.anchor, lo, hi)),
            AddrClass::HeapOpaque => {
                highest_in(&s.heap_any, lo, hi).max(highest_in(&s.anchor, lo, hi))
            }
        };
        best.max(stores)
    }
}
