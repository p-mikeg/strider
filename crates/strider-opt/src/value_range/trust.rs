//! What the jump-table classifier is allowed to trust: whether a bound this
//! module derived may be the one sizing a table, or whether some test the
//! analysis does not read is the real limit.

use rustc_hash::{FxHashMap, FxHashSet};

use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp, IntCmpOp};

use super::interval::{Interval, width_mask};
use super::{MAX_GUARD_LADDER, RangeMap, single_control_consumer};

impl RangeMap<'_> {
    /// Whether `iv`, `value`'s range at `region`, is no tighter than its
    /// KnownBits bound while a guard dominating `region` bounds bits of the
    /// same root that `value` overlaps, but in a way
    /// [`BitField::project`] does not carry to `value`.
    ///
    /// That guard may be the one sizing a table `value` indexes, in a shape
    /// this module does not read, and the KnownBits bound of a bit field is
    /// the whole field: enumerating it reads past the table into whatever
    /// follows, such as the next switch's table.
    pub(crate) fn guard_left_unconnected(
        &self,
        value: ValueId,
        region: NodeId,
        iv: Interval,
    ) -> bool {
        if iv != self.resolve_leaf_guarded(value, None) {
            return false;
        }
        let Some(field) = self.bit_field(value) else {
            return false;
        };
        self.root_fields(field.root).iter().any(|&of| {
            of.overlaps(field)
                && self.guards_by_field[&of].iter().any(|&guarded| {
                    guarded != value
                        && self
                            .guard_at(guarded, region)
                            .is_some_and(|iv| of.project(field, iv).is_none())
                })
        })
    }

    /// Whether `iv`, `value`'s range at `site`, is no tighter than its
    /// KnownBits bound while an `If` dominating `site` tests bits of `cell`, a
    /// load `value` derives from, that `value` reads, or of a load
    /// overlapping `cell`.
    ///
    /// Such a test (a mask, a shift, a sign) bounds the cell in a shape no
    /// guard here reads, or on a reload `range_of` does not connect, and may
    /// be what sizes a table `value` indexes. `Equal(cast*(cell), k)` whose
    /// true edge does not reach `site` only excludes `k`, which
    /// [`Self::ruled_out`] reads, and a guard recorded between `cell` and
    /// `value` is one `range_of` reads.
    pub(crate) fn cell_test_left_unread(
        &self,
        value: ValueId,
        cell: ValueId,
        site: NodeId,
        iv: Interval,
    ) -> bool {
        if iv != self.resolve_leaf_guarded(value, None) {
            return false;
        }
        let f = self.function;
        let cell_at = load_extent(f, cell);
        // `Some(aligned)` for `cell` or a load overlapping its bytes: aligned
        // when its bits are `cell`'s bit for bit.
        let view = |v: ValueId| {
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
        };
        let index_bits = cell_bits_read(f, value, |v| (v == cell).then_some(true));
        self.ifs.iter().any(|&if_node| {
            self.doms.dominates(if_node, site) && {
                let cond = f.if_cond(if_node);
                (!excludes_one_value(f, cond, |v| view(v).is_some())
                    || self.edge_reaches(if_node, NodeKind::IF_TRUE_OUT_SLOT, site))
                    && !self.guards_index(if_node, value, cell)
                    && cell_bits_read(f, cond, view) & index_bits != 0
            }
        })
    }

    /// Whether an edge of `if_node` records a guard on a value between `cell`
    /// and `value`, which `range_of` reads on the way to `value`.
    fn guards_index(&self, if_node: NodeId, value: ValueId, cell: ValueId) -> bool {
        let f = self.function;
        let outputs = f.node_outputs(if_node);
        [NodeKind::IF_TRUE_OUT_SLOT, NodeKind::IF_FALSE_OUT_SLOT]
            .iter()
            .filter_map(|&slot| self.edge_guards.get(outputs.get(slot)?))
            .any(|&(guarded, _)| cone_reaches(f, value, guarded) && cone_reaches(f, guarded, cell))
    }

    /// Whether the successor of `if_node`'s edge at `slot` dominates `site`.
    fn edge_reaches(&self, if_node: NodeId, slot: usize, site: NodeId) -> bool {
        let f = self.function;
        f.node_outputs(if_node)
            .get(slot)
            .and_then(|&edge| single_control_consumer(f, edge))
            .is_some_and(|sink| self.doms.dominates(sink, site))
    }

    /// The constants `value` is not at `region`: each `k` of an
    /// `If(value == k)` whose false edge alone reaches `region`.
    ///
    /// The edge's sink must be no merge, as for any edge guard, or the fact
    /// would hold for the merge's other predecessors too.
    pub(crate) fn ruled_out(&self, value: ValueId, region: NodeId) -> FxHashSet<u128> {
        let f = self.function;
        f.value_uses(value)
            .filter_map(|(cmp, _)| {
                if !matches!(f.node_kind(cmp), NodeKind::IntCmpOp(IntCmpOp::Equal)) {
                    return None;
                }
                let eq = f.first_value_output_of(cmp)?;
                let [a, b] = f.producer_inputs_exact::<2>(eq).ok()?;
                let k = f.int_const_u128(if a == value { b } else { a })?;
                f.value_uses(eq)
                    .any(|(if_node, _)| {
                        if !matches!(f.node_kind(if_node), NodeKind::If) || f.if_cond(if_node) != eq
                        {
                            return false;
                        }
                        let outputs = f.node_outputs(if_node);
                        outputs
                            .get(NodeKind::IF_FALSE_OUT_SLOT)
                            .is_some_and(|&false_ctrl| {
                                single_control_consumer(f, false_ctrl).is_some_and(|sink| {
                                    !matches!(f.node_kind(sink), NodeKind::Region)
                                        && self.doms.dominates(sink, region)
                                })
                            })
                    })
                    .then_some(k)
            })
            .collect()
    }
}

/// Whether `cond` is `Equal(x, k)` with `x` a cast chain over a value
/// `is_cell` accepts.
fn excludes_one_value(
    f: &strider_ir::Function,
    cond: ValueId,
    is_cell: impl Fn(ValueId) -> bool,
) -> bool {
    if !matches!(
        f.node_kind(f.producer(cond)),
        NodeKind::IntCmpOp(IntCmpOp::Equal)
    ) {
        return false;
    }
    let Ok([a, b]) = f.producer_inputs_exact::<2>(cond) else {
        return false;
    };
    let mut x = match (f.int_const_u128(a), f.int_const_u128(b)) {
        (Some(_), None) => b,
        (None, Some(_)) => a,
        _ => return false,
    };
    for _ in 0..MAX_GUARD_LADDER {
        if is_cell(x) {
            return true;
        }
        match f.node_kind(f.producer(x)) {
            NodeKind::Extend(_) | NodeKind::Truncate => match f.int_inputs(x).next() {
                Some(inner) => x = inner,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}

/// Whether `root`'s value reads bits of `target`. Past [`MAX_TEST_CONE`]
/// values it answers no.
fn cone_reaches(f: &strider_ir::Function, root: ValueId, target: ValueId) -> bool {
    bits_read(f, root, |v| (v == target).then_some(true)).is_some_and(|b| b != 0)
}

/// [`bits_read`], every bit past [`MAX_TEST_CONE`] values.
fn cell_bits_read(
    f: &strider_ir::Function,
    root: ValueId,
    view: impl Fn(ValueId) -> Option<bool>,
) -> u128 {
    bits_read(f, root, view).unwrap_or(u128::MAX)
}

/// The bits of the values `view` accepts that `root`'s value depends on,
/// walking its operands through integer ops and casts, not through a phi or a
/// load. `view` answers whether a value's bits align with the cell's, and
/// every bit of one that does not counts. `None` past [`MAX_TEST_CONE`]
/// values.
fn bits_read(
    f: &strider_ir::Function,
    root: ValueId,
    view: impl Fn(ValueId) -> Option<bool>,
) -> Option<u128> {
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
        if demanded.len() > MAX_TEST_CONE {
            return None;
        }
        match view(v) {
            Some(true) => read |= new,
            Some(false) => read = u128::MAX,
            None => stack.extend(operand_demand(f, v, new)),
        }
    }
    Some(read)
}

/// Values [`bits_read`] visits under one root.
const MAX_TEST_CONE: usize = 64;

/// `(base, offset, bytes)` of the load producing `v`: its address as a base
/// and a constant offset from it.
fn load_extent(f: &strider_ir::Function, v: ValueId) -> Option<(ValueId, u128, i128)> {
    if !matches!(f.node_kind(f.producer(v)), NodeKind::Load(_)) {
        return None;
    }
    let addr = f.int_inputs(v).next()?;
    let len = i128::try_from(f.value_type_opt(v)?.byte_size()).ok()?;
    let (base, off) = match f.node_kind(f.producer(addr)) {
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            crate::const_eval::var_and_const(f, f.producer(addr)).unwrap_or((addr, 0))
        }
        _ => (addr, 0),
    };
    Some((base, off, len))
}

/// The bits of each variable operand of `v`'s producer that `demand`, bits of
/// `v`, depends on. Nothing for a producer that is not an integer op or cast;
/// every bit of an operand of an op this does not model.
fn operand_demand(f: &strider_ir::Function, v: ValueId, demand: u128) -> Vec<(ValueId, u128)> {
    let kind = *f.node_kind(f.producer(v));
    if !matches!(
        kind,
        NodeKind::IntBinaryOp(_)
            | NodeKind::IntUnaryOp(_)
            | NodeKind::IntCmpOp(_)
            | NodeKind::Extend(_)
            | NodeKind::Truncate
            | NodeKind::Popcount
            | NodeKind::Lzcount
    ) {
        return Vec::new();
    }
    let inputs: Vec<ValueId> = f.int_inputs(v).collect();
    let sign_of = |x: ValueId| {
        let m = width_mask(f, x);
        m ^ (m >> 1)
    };
    // Bit `i` of a sum or product depends on operand bits `0..=i` only.
    let at_or_below = match 128 - demand.leading_zeros() {
        128 => u128::MAX,
        n => (1u128 << n) - 1,
    };
    let shift = inputs
        .get(1)
        .and_then(|&k| f.int_const_u128(k))
        .and_then(|k| u32::try_from(k).ok());
    let mask_const = match (kind, inputs.as_slice()) {
        (NodeKind::IntBinaryOp(IntBinaryOp::And), &[a, b]) => {
            f.int_const_u128(b).or_else(|| f.int_const_u128(a))
        }
        _ => None,
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
