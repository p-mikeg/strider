//! Reading a value as a run of bits of the value below it, which is what lets
//! a guard on one expression of those bits bound another.

use cranelift_entity::SecondaryMap;

use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp};

use crate::opt::known_bits::KnownBitsMap;

use super::interval::Interval;

/// The operands of the `And`s with a constant mask.
#[derive(Default)]
pub(super) struct MaskOperands {
    /// Per `And(x, c)` output whose mask clears only bits KnownBits proves zero
    /// in `x`, that `x`: the two are the same value.  `slwi` / `sldi` lift to
    /// `And(ShiftLeft(idx, k), ~(2^k - 1))`.
    pub(super) identity: SecondaryMap<ValueId, Option<ValueId>>,
    /// Per `And(Or(a, b), c)` output where `c` clears every bit `a` may set,
    /// `b`: the output is `And(b, c)`.  A PowerPC `rlwinm` lifts as the
    /// rotate `Or(x << r, x >> (32 - r))` under a mask that drops one arm.
    through_or: SecondaryMap<ValueId, Option<ValueId>>,
}

impl MaskOperands {
    pub(super) fn record(
        &mut self,
        function: &strider_ir::Function,
        node: NodeId,
        known: &KnownBitsMap,
    ) -> Option<()> {
        if !matches!(
            function.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ) {
            return None;
        }
        let &[value] = function.node_outputs(node) else {
            return None;
        };
        let (operand, mask) = crate::const_eval::var_and_const(function, node)?;
        let type_mask = function.value_type_opt(value)?.int_mask_u128()?;
        let may_set = |v: ValueId| !known[v].zeros & type_mask;
        if may_set(operand) & !mask == 0 {
            self.identity[value] = Some(operand);
        }
        if let NodeKind::IntBinaryOp(IntBinaryOp::Or) = *function.kind_of_value(operand)
            && let Ok([a, b]) = function.producer_inputs_exact::<2>(operand)
        {
            if may_set(a) & mask == 0 {
                self.through_or[value] = Some(b);
            } else if may_set(b) & mask == 0 {
                self.through_or[value] = Some(a);
            }
        }
        Some(())
    }
}

/// Producer steps [`bit_field`] takes below a value; past them the value
/// reached is the root.
const MAX_FIELD_STEPS: u32 = 16;

/// A value read as a run of bits of `root`: its bits `shift..shift + width`
/// are `root`'s bits `lo..lo + width`, and its bits below `shift` are zero.
/// When `exact` the bits above the run are zero too, so two values with one
/// exact field are one unsigned value; otherwise they are other bits of
/// `root`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct BitField {
    pub(super) root: ValueId,
    lo: u32,
    width: u32,
    shift: u32,
    pub(super) exact: bool,
}

impl BitField {
    /// Whether the two may share a bit of `root`.
    pub(super) fn overlaps(self, other: BitField) -> bool {
        self.root == other.root
            && (!self.exact
                || !other.exact
                || (self.lo < other.lo + other.width && other.lo < self.lo + self.width))
    }

    /// Whether `inner` is exact and its run lies inside this one's, which is
    /// what lets [`Self::project`] bound it.
    pub(super) fn encloses(self, inner: BitField) -> bool {
        self.root == inner.root
            && inner.exact
            && self.lo <= inner.lo
            && inner.lo + inner.width <= self.lo + self.width
    }

    /// What `iv`, an interval on a value with this field, proves about a value
    /// with the field `inner`, or `None` unless it [`encloses`](Self::encloses)
    /// `inner` and `iv`'s low end carries.
    ///
    /// The same exact field is the same value, so `iv` itself.  Otherwise the
    /// run's bits read as a number are at most `iv.hi >> shift`, whatever lies
    /// above them, and `inner`'s are that number's bits from `inner.lo - lo`
    /// on: at most `(iv.hi >> shift) >> (inner.lo - lo)`, and at most
    /// `inner`'s own width.  Only the top of an exact run is monotone in the
    /// value, so only there does `iv.lo` bound `inner` from below; anywhere
    /// else a low end above zero is refused: dropping it would enumerate
    /// values below it that the field never takes (`x` in `[10, 17]` puts
    /// `(x >> 1) & 7` in `{5, 6, 7, 0}`, not `[0, 7]`).
    pub(super) fn project(self, inner: BitField, iv: Interval) -> Option<Interval> {
        if self == inner {
            return Some(iv);
        }
        if !self.encloses(inner) || inner.shift + inner.width > 128 {
            return None;
        }
        let digits = |v: u128| {
            v.checked_shr(self.shift)
                .and_then(|run| run.checked_shr(inner.lo - self.lo))
                .unwrap_or(0)
        };
        let at_top = self.exact && inner.lo + inner.width == self.lo + self.width;
        let lo = match iv.lo {
            0 => 0,
            lo if at_top => digits(lo),
            _ => return None,
        };
        let width_max = u128::MAX >> (128 - inner.width);
        Some(Interval::new(
            lo << inner.shift,
            digits(iv.hi).min(width_max) << inner.shift,
            1u128 << inner.shift,
        ))
    }
}

/// One step of [`bit_field`]'s walk: how the value is built from `operand`.
#[derive(Clone, Copy)]
enum FieldOp {
    /// The same unsigned value.
    Same,
    SignExtend,
    Truncate,
    ShiftLeft(u32),
    ShiftRight(u32),
    /// `& (2^m - 1)`.
    LowMask(u32),
    /// `& ((2^width - 1) << shift)`: `((x >> shift) & (2^width - 1)) << shift`.
    RunMask {
        shift: u32,
        width: u32,
    },
}

impl FieldOp {
    /// The field of a value of width `w` built by this op from one whose field
    /// is `f` and whose width is `w_in`; `None` when no bit of `root` is left.
    fn apply(self, f: BitField, w: u32, w_in: u32) -> Option<BitField> {
        Some(match self {
            FieldOp::Same => f,
            FieldOp::SignExtend => BitField {
                exact: f.exact && f.shift + f.width < w_in,
                ..f
            },
            FieldOp::Truncate => {
                let room = w.checked_sub(f.shift).filter(|&r| r > 0)?;
                BitField {
                    width: f.width.min(room),
                    exact: f.exact || f.width >= room,
                    ..f
                }
            }
            FieldOp::ShiftLeft(k) => {
                let shift = f.shift.checked_add(k).filter(|&s| s < w)?;
                BitField {
                    width: f.width.min(w - shift),
                    shift,
                    exact: f.exact || shift + f.width >= w,
                    ..f
                }
            }
            FieldOp::ShiftRight(k) if k <= f.shift => BitField {
                shift: f.shift - k,
                ..f
            },
            FieldOp::ShiftRight(k) => {
                let dropped = k - f.shift;
                let width = f.width.checked_sub(dropped).filter(|&w| w > 0)?;
                BitField {
                    lo: f.lo + dropped,
                    width,
                    shift: 0,
                    ..f
                }
            }
            FieldOp::LowMask(m) => {
                let room = m.checked_sub(f.shift).filter(|&r| r > 0)?;
                BitField {
                    width: f.width.min(room),
                    exact: f.exact || f.width >= room,
                    ..f
                }
            }
            FieldOp::RunMask { shift, width } => {
                let run = FieldOp::ShiftRight(shift).apply(f, w, w_in)?;
                let run = FieldOp::LowMask(width).apply(run, w, w)?;
                return FieldOp::ShiftLeft(shift).apply(run, w, w);
            }
        })
    }
}

/// How `value` is built from one operand by an op [`bit_field`] reads.
fn field_step(
    function: &strider_ir::Function,
    masks: &MaskOperands,
    value: ValueId,
) -> Option<(ValueId, FieldOp)> {
    let f = function;
    let shift_by = || -> Option<(ValueId, u32)> {
        let [x, k] = f.producer_inputs_exact::<2>(value).ok()?;
        Some((x, u32::try_from(f.int_const_u128(k)?).ok()?))
    };
    match *f.node_kind(f.producer(value)) {
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            Some((f.int_inputs(value).next()?, FieldOp::Same))
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            Some((f.int_inputs(value).next()?, FieldOp::SignExtend))
        }
        NodeKind::Truncate => Some((f.int_inputs(value).next()?, FieldOp::Truncate)),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            shift_by().map(|(x, k)| (x, FieldOp::ShiftLeft(k)))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            shift_by().map(|(x, k)| (x, FieldOp::ShiftRight(k)))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
            let (x, c) = crate::const_eval::var_and_const(f, f.producer(value))?;
            c.is_power_of_two()
                .then(|| (x, FieldOp::ShiftLeft(c.trailing_zeros())))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            if let Some(x) = masks.identity[value] {
                return Some((x, FieldOp::Same));
            }
            let (x, c) = crate::const_eval::var_and_const(f, f.producer(value))?;
            let x = masks.through_or[value].unwrap_or(x);
            if c == 0 {
                return None;
            }
            let shift = c.trailing_zeros();
            let run = c >> shift;
            if run & run.wrapping_add(1) != 0 {
                return None;
            }
            let width = 128 - run.leading_zeros();
            Some((
                x,
                if shift == 0 {
                    FieldOp::LowMask(width)
                } else {
                    FieldOp::RunMask { shift, width }
                },
            ))
        }
        _ => None,
    }
}

/// `value` as a [`BitField`] of the deepest value the shifts, masks and casts
/// below it reach; `value` itself, all its bits, when they leave nothing exact
/// to compare.  `None` for a non-integer.
pub(super) fn bit_field(
    function: &strider_ir::Function,
    masks: &MaskOperands,
    value: ValueId,
) -> Option<BitField> {
    let width = |v: ValueId| {
        function
            .value_type_opt(v)
            .filter(|t| t.is_integer())
            .and_then(|t| u32::try_from(t.bit_width()).ok())
    };
    let whole = |root: ValueId| {
        Some(BitField {
            root,
            lo: 0,
            width: width(root)?,
            shift: 0,
            exact: true,
        })
    };
    let mut steps: Vec<(ValueId, ValueId, FieldOp)> = Vec::new();
    let mut base = value;
    for _ in 0..MAX_FIELD_STEPS {
        let Some((operand, op)) = field_step(function, masks, base) else {
            break;
        };
        steps.push((base, operand, op));
        base = operand;
    }
    let mut field = whole(base)?;
    for &(out, operand, op) in steps.iter().rev() {
        match op.apply(field, width(out)?, width(operand)?) {
            Some(next) => field = next,
            None => return whole(value),
        }
    }
    Some(field)
}
