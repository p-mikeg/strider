//! What the jump-table classifier is allowed to trust: whether a bound this
//! module derived may be the one sizing a table, or whether some test the
//! analysis does not read is the real limit.

use rustc_hash::{FxHashMap, FxHashSet};

use strider_ir::eval::mask_below;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp, IntCmpOp};

use super::interval::{Interval, width_mask};
use super::{MAX_WALK, RangeMap, Stripped, cast_chain, guard_sink, single_control_consumer};

impl RangeMap<'_> {
    /// Whether a guard dominating `region` bounds bits of the same root that
    /// `value` overlaps, the bits that can vary within `iv`, its range at
    /// `region` (`narrowed` when tighter than its KnownBits bound), in a way
    /// [`BitField::project`](super::bit_field::BitField::project) does not carry to `value`.
    ///
    /// That guard may be the one sizing a table `value` indexes, in a shape
    /// this module does not read, tighter than `iv`: the KnownBits bound of a
    /// bit field is the whole field, and a read guard on it may be looser
    /// still. Enumerating it reads past the table into whatever follows, such
    /// as the next switch's table.
    pub(crate) fn guard_left_unconnected(
        &mut self,
        value: ValueId,
        region: NodeId,
        iv: Interval,
        narrowed: bool,
    ) -> bool {
        let Some(field) = self.bit_field(value) else {
            return false;
        };
        let varying = if narrowed {
            match field.below(iv.hi) {
                Some(varying) => varying,
                None => return false,
            }
        } else {
            field
        };
        self.root_fields(field.root).iter().any(|(of, over)| {
            of.overlaps(varying)
                && over.iter().any(|&guarded| {
                    guarded != value
                        && self
                            .guard_at(guarded, region)
                            .is_some_and(|iv| of.project(field, iv).is_none())
                })
        })
    }

    /// Whether `iv`, `value`'s range at `site`, is no tighter than its
    /// KnownBits bound (`!narrowed`) while an `If` dominating `site` tests
    /// bits of `strip`'s cell, a load `value` derives from, that `value`
    /// reads, or of a load overlapping the cell.
    ///
    /// Such a test (a mask, a shift, a sign) bounds the cell in a shape no
    /// guard here reads, or on a reload `range_of` does not connect, and may
    /// be what sizes a table `value` indexes. `Equal(cast*(cell), k)` whose
    /// true edge does not reach `site` only excludes `k`, which
    /// [`Self::excludes`] reads, and a guard on an edge holding at `site`
    /// whose bound, carried exactly up `strip`'s steps, still admits all of
    /// `iv` is one `range_of` reads. A narrowed range is trusted as the
    /// table's bound: a read guard looser than an unread test is a known
    /// limit.
    pub(crate) fn cell_test_left_unread(
        &mut self,
        value: ValueId,
        strip: &Stripped,
        site: NodeId,
        iv: Interval,
        narrowed: bool,
    ) -> bool {
        if narrowed {
            return false;
        }
        let Some((pre, _)) = self.doms.span(site) else {
            return false;
        };
        if !self.cell_test_spans.contains_key(&(value, iv)) {
            let spans = self.unread_cell_test_spans(value, strip, iv);
            self.cell_test_spans.insert((value, iv), spans);
        }
        let spans = &self.cell_test_spans[&(value, iv)];
        let at = spans.partition_point(|&(lo, _)| lo <= pre);
        at.checked_sub(1).is_some_and(|i| spans[i].1 >= pre)
    }

    /// The disjoint dominator-tree preorder ranges, sorted, holding the
    /// sites [`Self::cell_test_left_unread`] answers yes for: a site's `pre`
    /// lies in some `If`'s span, within its true edge's when it only
    /// excludes one value, and outside every span where a guard it records
    /// reads `iv`. A span is `pre..=post`, and `post` is on the same clock.
    fn unread_cell_test_spans(
        &self,
        value: ValueId,
        strip: &Stripped,
        iv: Interval,
    ) -> Vec<(u32, u32)> {
        let f = self.function;
        let cell = strip.cell;
        let view = cell_view(f, cell);
        let index_bits = cell_bits_read(f, value, |v| (v == cell).then_some(true));
        let span = |node: Option<NodeId>| node.and_then(|n| self.doms.span(n));
        let mut held = Vec::new();
        for if_node in self.cell_tests.reading(f, cell) {
            #[cfg(test)]
            CELL_TEST_SCANS.with(|c| c.set(c.get() + 1));
            let cond = f.if_cond(if_node);
            let Some((mut lo, mut hi)) = span(Some(if_node)) else {
                continue;
            };
            if excludes_one_value(f, cond, |v| view(v).is_some()) {
                let Some((t_lo, t_hi)) = span(self.edge_sink(if_node, NodeKind::IF_TRUE_OUT_SLOT))
                else {
                    continue;
                };
                (lo, hi) = (lo.max(t_lo), hi.min(t_hi));
            }
            if lo > hi || cell_bits_read(f, cond, &view) & index_bits == 0 {
                continue;
            }
            let outputs = f.node_outputs(if_node);
            let mut read: Vec<(u32, u32)> =
                [NodeKind::IF_TRUE_OUT_SLOT, NodeKind::IF_FALSE_OUT_SLOT]
                    .iter()
                    .filter_map(|&slot| outputs.get(slot))
                    .filter(|&edge| {
                        self.edge_guards.get(edge).is_some_and(|&(guarded, giv)| {
                            lift_up(f, strip, guarded, giv).is_some_and(|l| l.covers(iv))
                        })
                    })
                    .filter_map(|&edge| span(self.guard_holds_below(edge)))
                    .collect();
            read.sort_unstable();
            for (r_lo, r_hi) in read {
                if lo > hi {
                    break;
                }
                if r_lo > lo {
                    held.push((lo, hi.min(r_lo - 1)));
                }
                lo = lo.max(r_hi + 1);
            }
            if lo <= hi {
                held.push((lo, hi));
            }
        }
        held.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(held.len());
        for (lo, hi) in held {
            match merged.last_mut() {
                Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        merged
    }

    /// The node below which a guard recorded on `edge` holds: its sink, when
    /// reached by that edge alone or joining it into a recovered guard.
    fn guard_holds_below(&self, edge: ValueId) -> Option<NodeId> {
        let f = self.function;
        guard_sink(f, edge).or_else(|| {
            self.merged_edges
                .contains(&edge)
                .then(|| single_control_consumer(f, edge))?
        })
    }

    /// The one consumer of `if_node`'s edge at `slot`.
    fn edge_sink(&self, if_node: NodeId, slot: usize) -> Option<NodeId> {
        let f = self.function;
        f.node_outputs(if_node)
            .get(slot)
            .and_then(|&edge| single_control_consumer(f, edge))
    }

    /// Whether `value` is not `x` at `site`: an `If(y == x)` whose false
    /// edge holds at `site` tests `value` or a zero-extend `y` of it.
    ///
    /// The edge's sink must be no merge, as for any edge guard, or the fact
    /// would hold for the merge's other predecessors too. The constants only
    /// filter the values enumerated; no range a veto weighs is narrowed by
    /// them.
    pub(crate) fn excludes(&self, value: ValueId, x: u128, site: NodeId) -> bool {
        #[cfg(test)]
        EQUALITY_SCANS.with(|c| c.set(c.get() + 1));
        self.excluded
            .get(&(value, x))
            .zip(self.doms.span(site))
            .is_some_and(|(sinks, span)| sinks.meet_dominating(span).is_some())
    }

    /// The values `index` scales without wrapping, through zero-extends and
    /// constant multiplies and left shifts, each with the factor taking it
    /// to `index`: `index` is not `x` where some value is not `x / factor`.
    pub(crate) fn index_scales(&self, index: ValueId) -> Vec<(ValueId, u128)> {
        let f = self.function;
        let mut out = Vec::new();
        let (mut v, mut factor) = (index, 1u128);
        for _ in 0..MAX_WALK {
            out.push((v, factor));
            let Some((x, c)) = super::operand_step(f, v).and_then(|(x, step)| match step {
                super::Step::ZeroExtend => Some((x, 1)),
                super::Step::Mul(c) => Some((x, c)),
                super::Step::ShiftLeft(k) => Some((x, 1u128.checked_shl(k)?)),
                _ => None,
            }) else {
                break;
            };
            let exact = c != 0
                && self
                    .kb_bound(x)
                    .hi
                    .checked_mul(c)
                    .is_some_and(|top| top <= width_mask(f, v));
            let Some(scaled) = factor.checked_mul(c).filter(|_| exact) else {
                break;
            };
            (v, factor) = (x, scaled);
        }
        out
    }
}

#[cfg(test)]
thread_local! {
    /// [`RangeMap::excludes`] lookups.
    pub(crate) static EQUALITY_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// `If`s [`RangeMap::cell_test_left_unread`] weighs.
    pub(crate) static CELL_TEST_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Every `If`, by the base and offset of each load its condition's cone
/// reaches through the ops [`operand_demand`] walks.
pub(super) struct CellTests {
    by_slot: FxHashMap<(ValueId, u128), Vec<NodeId>>,
    /// Those whose cone runs past [`MAX_WALK`] values, which
    /// [`cell_bits_read`] reads as every bit of any cell.
    any: Vec<NodeId>,
    /// The widest load recorded, in bytes.
    widest: i128,
    /// Every `If`, for a cell no load extent places.
    all: Vec<NodeId>,
}

impl CellTests {
    pub(super) fn new(f: &strider_ir::Function, ifs: &[NodeId]) -> Self {
        let mut tests = Self {
            by_slot: FxHashMap::default(),
            any: Vec::new(),
            widest: 0,
            all: ifs.to_vec(),
        };
        for &if_node in ifs {
            // A superset of what `cell_bits_read` visits from the condition, which
            // ignores the demand, so a cone within the cap here is one there.
            let mut seen: FxHashSet<ValueId> = FxHashSet::default();
            let mut stack = vec![f.if_cond(if_node)];
            let mut slots: FxHashSet<(ValueId, u128)> = FxHashSet::default();
            while let Some(v) = stack.pop() {
                if !seen.insert(v) {
                    continue;
                }
                if seen.len() > MAX_WALK {
                    tests.any.push(if_node);
                    slots.clear();
                    break;
                }
                if let Some((base, off, len)) = load_extent(f, v) {
                    slots.insert((base, off));
                    tests.widest = tests.widest.max(len);
                } else if walks_operands(f.kind_of_value(v)) {
                    stack.extend(f.int_inputs(v).filter(|&x| f.int_const_u128(x).is_none()));
                }
            }
            for slot in slots {
                tests.by_slot.entry(slot).or_default().push(if_node);
            }
        }
        tests
    }

    /// The `If`s whose condition may read bits of `cell`: those reading a
    /// load overlapping it, and those past the cap. Possibly repeated.
    fn reading<'a>(
        &'a self,
        f: &strider_ir::Function,
        cell: ValueId,
    ) -> Box<dyn Iterator<Item = NodeId> + 'a> {
        let Some((base, off, len, mask)) = load_extent(f, cell).and_then(|(base, off, len)| {
            Some((base, off, len, f.value_type_opt(base)?.int_mask_u128()?))
        }) else {
            return Box::new(self.all.iter().copied());
        };
        // `d` is the load's offset less the cell's, at the base's width.
        let overlapping = (1 - self.widest..len)
            .filter_map(move |d| {
                self.by_slot
                    .get(&(base, off.wrapping_add(d as u128) & mask))
            })
            .flatten();
        Box::new(overlapping.chain(&self.any).copied())
    }
}

/// Whether [`operand_demand`] walks into a value of this kind's operands.
fn walks_operands(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::IntBinaryOp(_)
            | NodeKind::IntUnaryOp(_)
            | NodeKind::IntCmpOp(_)
            | NodeKind::Extend(_)
            | NodeKind::Truncate
            | NodeKind::Popcount
            | NodeKind::Lzcount
    )
}

/// `Some(aligned)` for `cell` or a load overlapping its bytes, `aligned`
/// when its bits are `cell`'s bit for bit.
fn cell_view(f: &strider_ir::Function, cell: ValueId) -> impl Fn(ValueId) -> Option<bool> + '_ {
    let cell_at = load_extent(f, cell);
    move |v: ValueId| {
        if v == cell {
            return Some(true);
        }
        let (cell_base, cell_off, cell_len) = cell_at?;
        let (base, off, len) = load_extent(f, v)?;
        if base != cell_base {
            return None;
        }
        let d = f
            .value_type_opt(base)?
            .get_signed_int(off.wrapping_sub(cell_off))?;
        if d == 0 && len == cell_len {
            Some(true)
        } else {
            (d > -len && d < cell_len).then_some(false)
        }
    }
}

/// Whether `cond` is `Equal(x, k)` with `x` a cast chain over a value
/// `is_cell` accepts.
fn excludes_one_value(
    f: &strider_ir::Function,
    cond: ValueId,
    is_cell: impl Fn(ValueId) -> bool,
) -> bool {
    let Some((x, _)) = crate::const_eval::var_and_const_of(
        f,
        f.producer(cond),
        NodeKind::IntCmpOp(IntCmpOp::Equal),
    ) else {
        return false;
    };
    cast_chain(f, x, |k| {
        matches!(k, NodeKind::Extend(_) | NodeKind::Truncate)
    })
    .any(is_cell)
}

/// `iv`, a bound on `guarded`, carried up `strip`'s steps to the value
/// stripped; `None` when `guarded` is not on it or a step has no exact image.
fn lift_up(
    f: &strider_ir::Function,
    strip: &Stripped,
    guarded: ValueId,
    iv: Interval,
) -> Option<Interval> {
    // A zero-extend is the identity on the value, so a guard on one bounds
    // the value it extends.
    let at = cast_chain(f, guarded, |k| {
        matches!(k, NodeKind::Extend(ExtendOp::ZeroExtend))
    })
    .find_map(|g| {
        if g == strip.cell {
            Some(strip.steps.len())
        } else {
            strip.steps.iter().position(|&(v, _)| v == g)
        }
    })?;
    strip.steps[..at]
        .iter()
        .rev()
        .try_fold(iv, |iv, &(v, step)| step.image(iv, width_mask(f, v)))
}

/// The bits of the values `view` accepts that `root`'s value depends on,
/// walking its operands through integer ops and casts, not through a phi or a
/// load. `view` answers whether a value's bits align with the cell's, and
/// every bit of one that does not counts. Every bit past [`MAX_WALK`] values.
fn cell_bits_read(
    f: &strider_ir::Function,
    root: ValueId,
    view: impl Fn(ValueId) -> Option<bool>,
) -> u128 {
    let mut demanded: FxHashMap<ValueId, u128> = FxHashMap::default();
    let mut stack = vec![(root, width_mask(f, root))];
    let mut read = 0u128;
    while let Some((v, demand)) = stack.pop() {
        let seen = demanded.entry(v).or_insert(0);
        let new = demand & !*seen;
        if new == 0 {
            continue;
        }
        *seen |= new;
        if demanded.len() > MAX_WALK {
            return u128::MAX;
        }
        match view(v) {
            Some(true) => read |= new,
            Some(false) => read = u128::MAX,
            None => stack.extend(operand_demand(f, v, new)),
        }
    }
    read
}

/// `(base, offset, bytes)` of the load producing `v`: its address as a base
/// and a constant offset from it.
fn load_extent(f: &strider_ir::Function, v: ValueId) -> Option<(ValueId, u128, i128)> {
    if !matches!(f.kind_of_value(v), NodeKind::Load(_)) {
        return None;
    }
    let addr = f.int_inputs(v).next()?;
    let len = i128::try_from(f.value_type_opt(v)?.byte_size()).ok()?;
    let (base, off) = crate::const_eval::var_and_const_of(
        f,
        f.producer(addr),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
    )
    .unwrap_or((addr, 0));
    Some((base, off, len))
}

/// The bits of each variable operand of `v`'s producer that `demand`, bits of
/// `v`, depends on. Nothing for a producer that is not an integer op or cast;
/// every bit of an operand of an op this does not model.
fn operand_demand(f: &strider_ir::Function, v: ValueId, demand: u128) -> Vec<(ValueId, u128)> {
    let kind = *f.kind_of_value(v);
    if !walks_operands(&kind) {
        return Vec::new();
    }
    let inputs: Vec<ValueId> = f.int_inputs(v).collect();
    let sign_of = |x: ValueId| {
        let m = width_mask(f, x);
        m ^ (m >> 1)
    };
    // Bit `i` of a sum or product depends on operand bits `0..=i` only.
    let at_or_below = mask_below(128 - demand.leading_zeros());
    let shift = inputs
        .get(1)
        .and_then(|&k| f.int_const_u128(k))
        .and_then(|k| u32::try_from(k).ok());
    let mask_const = crate::const_eval::var_and_const_of(
        f,
        f.producer(v),
        NodeKind::IntBinaryOp(IntBinaryOp::And),
    )
    .map(|(_, c)| c);
    // `x < 0` and `-1 < x`, signed, read the sign bit alone.
    let sign_test = matches!(kind, NodeKind::IntCmpOp(IntCmpOp::Sless))
        && match *inputs.as_slice() {
            [a, b] => {
                f.int_const_u128(b) == Some(0) || f.int_const_u128(a) == Some(width_mask(f, a))
            }
            _ => false,
        };
    inputs
        .iter()
        .enumerate()
        .filter(|&(_, &x)| f.int_const_u128(x).is_none())
        .map(|(i, &x)| {
            let d = match (kind, i, shift) {
                (NodeKind::Extend(op), 0, _) => {
                    let past = demand & !width_mask(f, x) != 0;
                    let sign = past && op == ExtendOp::SignExtend;
                    demand | if sign { sign_of(x) } else { 0 }
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft), 0, Some(k)) => {
                    demand.checked_shr(k).unwrap_or(0)
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight), 0, Some(k)) => {
                    demand.checked_shl(k).unwrap_or(0)
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight), 0, Some(k)) => {
                    demand.checked_shl(k).unwrap_or(0) | sign_of(x)
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::And), _, _) => {
                    demand & mask_const.unwrap_or(u128::MAX)
                }
                (NodeKind::IntCmpOp(_), _, _) if sign_test => sign_of(x),
                (NodeKind::Truncate, 0, _)
                | (NodeKind::IntBinaryOp(IntBinaryOp::Or | IntBinaryOp::Xor), _, _) => demand,
                (
                    NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::Mul)
                    | NodeKind::IntUnaryOp(_),
                    _,
                    _,
                ) => at_or_below,
                _ => u128::MAX,
            };
            (x, d & width_mask(f, x))
        })
        .collect()
}
