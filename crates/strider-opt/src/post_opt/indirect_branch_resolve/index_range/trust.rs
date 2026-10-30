//! What the jump-table classifier is allowed to trust: whether a bound this
//! module derived may be the one sizing a table, or whether some test the
//! analysis does not read is the real limit.

use rustc_hash::{FxHashMap, FxHashSet};

use strider_ir::eval;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp, IntCmpOp, UBig};

use super::interval::{Interval, value_bound};
use super::{RangeMap, cast_chain, single_control_consumer};

impl RangeMap<'_> {
    /// Whether `iv`, `value`'s range at `region`, is no tighter than its
    /// KnownBits bound while a guard dominating `region` bounds bits of the
    /// same root that `value` overlaps, but in a way
    /// [`BitField::project`](super::bit_field::BitField::project) does not carry to `value`.
    ///
    /// That guard may be the one sizing a table `value` indexes, in a shape
    /// this module does not read, and the KnownBits bound of a bit field is
    /// the whole field: enumerating it reads past the table into whatever
    /// follows, such as the next switch's table.
    pub(crate) fn guard_left_unconnected(
        &self,
        value: ValueId,
        region: NodeId,
        iv: &Interval,
    ) -> bool {
        if *iv != self.resolve_leaf_guarded(value, None) {
            return false;
        }
        let Some(field) = self.bit_field(value) else {
            return false;
        };
        self.root_fields(field.root).iter().any(|(of, over)| {
            of.overlaps(field)
                && over.iter().any(|&guarded| {
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
        iv: &Interval,
    ) -> bool {
        if *iv != self.resolve_leaf_guarded(value, None) {
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
                    && cell_bits_read(f, cond, view) & &index_bits != UBig::ZERO
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
    pub(crate) fn ruled_out(&self, value: ValueId, region: NodeId) -> FxHashSet<UBig> {
        let f = self.function;
        f.value_uses(value)
            .filter_map(|(cmp, _)| {
                if !matches!(f.node_kind(cmp), NodeKind::IntCmpOp(IntCmpOp::Equal)) {
                    return None;
                }
                let eq = f.first_value_output_of(cmp)?;
                let [a, b] = f.producer_inputs_exact::<2>(eq).ok()?;
                let k = f.int_const(if a == value { b } else { a })?.clone();
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
    let x = match (f.int_const(a), f.int_const(b)) {
        (Some(_), None) => b,
        (None, Some(_)) => a,
        _ => return false,
    };
    cast_chain(f, x, |k| {
        matches!(k, NodeKind::Extend(_) | NodeKind::Truncate)
    })
    .any(is_cell)
}

/// Whether `root`'s value reads bits of `target`. Past [`MAX_TEST_CONE`]
/// values it answers no.
fn cone_reaches(f: &strider_ir::Function, root: ValueId, target: ValueId) -> bool {
    bits_read(f, root, |v| (v == target).then_some(true)).is_some_and(|b| b != UBig::ZERO)
}

/// [`bits_read`], every bit past [`MAX_TEST_CONE`] values.
fn cell_bits_read(
    f: &strider_ir::Function,
    root: ValueId,
    view: impl Fn(ValueId) -> Option<bool>,
) -> UBig {
    bits_read(f, root, view).unwrap_or_else(super::interval::unbounded)
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
) -> Option<UBig> {
    let mut demanded: FxHashMap<ValueId, UBig> = FxHashMap::default();
    let mut stack = vec![(root, value_bound(f, root))];
    let mut read = UBig::ZERO;
    while let Some((v, demand)) = stack.pop() {
        let seen = demanded.entry(v).or_insert(UBig::ZERO);
        // `demand & !seen`, which `UBig` has no `Not` for.
        let new = &demand ^ (&demand & &*seen);
        if new == UBig::ZERO {
            continue;
        }
        *seen |= &new;
        if demanded.len() > MAX_TEST_CONE {
            return None;
        }
        match view(v) {
            Some(true) => read |= new,
            Some(false) => read = super::interval::unbounded(),
            None => stack.extend(operand_demand(f, v, &new)),
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
fn operand_demand(f: &strider_ir::Function, v: ValueId, demand: &UBig) -> Vec<(ValueId, UBig)> {
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
        let m = value_bound(f, x);
        &m ^ (&m >> 1usize)
    };
    // Bit `i` of a sum or product depends on operand bits `0..=i` only.
    let at_or_below = at_or_below(demand);
    // A count past the widest type shifts every demanded bit out, and the
    // clamp keeps a left shift from sizing an allocation off a binary's own
    // constant.
    let shift = inputs
        .get(1)
        .and_then(|&k| f.int_const(k))
        .and_then(|k| usize::try_from(k).ok())
        .map(|k| k.min(super::interval::widest_int().bit_width()));
    let mask_const = match (kind, inputs.as_slice()) {
        (NodeKind::IntBinaryOp(IntBinaryOp::And), &[a, b]) => {
            f.int_const(b).or_else(|| f.int_const(a))
        }
        _ => None,
    };
    inputs
        .iter()
        .enumerate()
        .filter(|&(_, &x)| f.int_const(x).is_none())
        .map(|(i, &x)| {
            let d = match (kind, i, shift) {
                (NodeKind::Extend(op), 0, _) => {
                    let bound = value_bound(f, x);
                    let past = demand ^ (demand & &bound) != UBig::ZERO;
                    if past && op == ExtendOp::SignExtend {
                        demand | sign_of(x)
                    } else {
                        demand.clone()
                    }
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft), 0, Some(k)) => demand >> k,
                (NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight), 0, Some(k)) => demand << k,
                (NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight), 0, Some(k)) => {
                    (demand << k) | sign_of(x)
                }
                (NodeKind::IntBinaryOp(IntBinaryOp::And), _, _) => match mask_const {
                    Some(m) => demand & m,
                    None => demand.clone(),
                },
                (NodeKind::Truncate, 0, _)
                | (NodeKind::IntBinaryOp(IntBinaryOp::Or | IntBinaryOp::Xor), _, _) => {
                    demand.clone()
                }
                (
                    NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::Mul)
                    | NodeKind::IntUnaryOp(_),
                    _,
                    _,
                ) => at_or_below.clone(),
                _ => super::interval::unbounded(),
            };
            let bound = value_bound(f, x);
            (x, d & bound)
        })
        .collect()
}

/// Every bit at or below `d`'s highest set bit.
fn at_or_below(d: &UBig) -> UBig {
    let bytes = d.to_le_bytes();
    match bytes.iter().rposition(|&b| b != 0) {
        None => UBig::ZERO,
        Some(i) => eval::ones_at(i * 8 + (8 - bytes[i].leading_zeros() as usize), 0),
    }
}
