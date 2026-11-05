use cranelift_entity::SecondaryMap;

use strider_ir::eval::{bit, mask_below};
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp};

use crate::opt::known_bits::KnownBitsMap;

use super::interval::Interval;
use super::{Step, operand_step};

/// The operands of the `And`s with a constant mask, and of the `Truncate`s
/// of an `Or`.
#[derive(Default)]
pub(super) struct MaskOperands {
    /// Per `And(x, c)` output whose mask clears only bits KnownBits proves zero
    /// in `x`, that `x` (e.g. `slwi`'s `And(ShiftLeft(idx, k), ~(2^k - 1))`).
    pub(super) identity: SecondaryMap<ValueId, Option<ValueId>>,
    /// Per `And(Or(a, b), c)` output where `c` clears every bit `a` may set,
    /// `b` (e.g. a PowerPC `rlwinm` rotate whose mask drops one arm), and per
    /// `Truncate(Or(a, b))` where the truncation drops every such bit.
    through_or: SecondaryMap<ValueId, Option<ValueId>>,
}

/// The arm of the `Or` `or` that alone sets its bits under `keep`: the other
/// arm may set none of them (read at `ty_mask`).
fn kept_or_arm(
    function: &strider_ir::Function,
    or: ValueId,
    keep: u128,
    ty_mask: u128,
    known: &KnownBitsMap,
) -> Option<ValueId> {
    let NodeKind::IntBinaryOp(IntBinaryOp::Or) = *function.kind_of_value(or) else {
        return None;
    };
    let [a, b] = function.producer_inputs_exact::<2>(or).ok()?;
    let may_set = |v: ValueId| known[v].max_value(ty_mask);
    if may_set(a) & keep == 0 {
        Some(b)
    } else if may_set(b) & keep == 0 {
        Some(a)
    } else {
        None
    }
}

impl MaskOperands {
    pub(super) fn record(
        &mut self,
        function: &strider_ir::Function,
        node: NodeId,
        known: &KnownBitsMap,
    ) -> Option<()> {
        if let &[value] = function.node_outputs(node)
            && let NodeKind::Truncate = *function.kind_of_value(value)
        {
            let keep = function.value_type_opt(value)?.int_mask_u128()?;
            let [operand] = function.producer_inputs_exact::<1>(value).ok()?;
            let ty_mask = function.value_type_opt(operand)?.int_mask_u128()?;
            self.through_or[value] = kept_or_arm(function, operand, keep, ty_mask, known);
            return Some(());
        }
        let (operand, mask) = crate::const_eval::var_and_const_of(
            function,
            node,
            NodeKind::IntBinaryOp(IntBinaryOp::And),
        )?;
        let &[value] = function.node_outputs(node) else {
            return None;
        };
        let type_mask = function.value_type_opt(value)?.int_mask_u128()?;
        let may_set = |v: ValueId| known[v].max_value(type_mask);
        if may_set(operand) & !mask == 0 {
            self.identity[value] = Some(operand);
        }
        self.through_or[value] = kept_or_arm(function, operand, mask, type_mask, known);
        Some(())
    }
}

/// Producer steps [`bit_field`] takes below a value; past them the value
/// reached is the root.
const MAX_FIELD_STEPS: u32 = 16;

/// A value whose bits `shift..shift + width` are `root`'s bits
/// `lo..lo + width`, and below `shift` zero. When `exact` the bits above the
/// run are zero too, so two values with one exact field are one value.
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

    /// This field of a value at most `hi`: the run's bits that can still be
    /// set, `None` when none can.
    pub(super) fn below(self, hi: u128) -> Option<BitField> {
        let width = self
            .width
            .min((128 - hi.leading_zeros()).saturating_sub(self.shift));
        (width > 0).then_some(BitField { width, ..self })
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
    /// with the field `inner`; `None` unless it [`encloses`](Self::encloses)
    /// `inner` and `iv`'s low end carries. Only the top of an exact run is
    /// monotone in the value, so elsewhere a nonzero `iv.lo` is refused: `x`
    /// in `[10, 17]` puts `(x >> 1) & 7` in `{5, 6, 7, 0}`.
    pub(super) fn project(self, inner: BitField, iv: Interval) -> Option<Interval> {
        if self == inner {
            return Some(iv);
        }
        if !self.encloses(inner) || inner.shift + inner.width > 128 {
            return None;
        }
        let digits = |v: u128| v.checked_shr(self.shift)?.checked_shr(inner.lo - self.lo);
        let at_top = self.exact && inner.lo + inner.width == self.lo + self.width;
        let lo = match iv.lo {
            0 => 0,
            lo if at_top => digits(lo)?,
            _ => return None,
        };
        let width_max = mask_below(inner.width);
        Some(Interval::new(
            lo << inner.shift,
            digits(iv.hi)?.min(width_max) << inner.shift,
            bit(inner.shift),
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
            FieldOp::Truncate => return FieldOp::LowMask(w).apply(f, w, w_in),
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
    let (x, step) = operand_step(function, value)?;
    Some(match step {
        Step::ZeroExtend => (x, FieldOp::Same),
        Step::SignExtend => (x, FieldOp::SignExtend),
        Step::Truncate => (masks.through_or[value].unwrap_or(x), FieldOp::Truncate),
        Step::ShiftLeft(k) => (x, FieldOp::ShiftLeft(k)),
        Step::ShiftRight(k) => (x, FieldOp::ShiftRight(k)),
        Step::Mul(c) if c.is_power_of_two() => (x, FieldOp::ShiftLeft(c.trailing_zeros())),
        Step::And(c) => {
            if let Some(same) = masks.identity[value] {
                return Some((same, FieldOp::Same));
            }
            if c == 0 {
                return None;
            }
            let x = masks.through_or[value].unwrap_or(x);
            let shift = c.trailing_zeros();
            let run = c >> shift;
            if run & run.wrapping_add(1) != 0 {
                return None;
            }
            let width = 128 - run.leading_zeros();
            (x, FieldOp::RunMask { shift, width })
        }
        Step::Mul(_) | Step::Div(_) => return None,
    })
}

/// `value` as a [`BitField`] of the deepest value the shifts, masks and casts
/// below it reach, else of itself; `None` for a non-integer.
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

#[cfg(test)]
mod tests {
    use super::{BitField, Interval};

    /// A run whose digits shift out of the `u128` carrier bounds nothing.
    /// Mutation: read an out-of-carrier shift as zero digits.
    #[test]
    fn a_shift_past_the_carrier_projects_no_bound() {
        let root = cranelift_entity::EntityRef::new(0);
        let of = BitField {
            root,
            lo: 0,
            width: 8,
            shift: 128,
            exact: true,
        };
        let inner = BitField {
            root,
            lo: 0,
            width: 4,
            shift: 0,
            exact: true,
        };
        assert_eq!(of.project(inner, Interval::dense(0, 0xff)), None);
    }
}
