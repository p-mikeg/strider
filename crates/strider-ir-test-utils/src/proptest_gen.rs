//! Value-only DAG generator: a sequence of [`FunctionBuilder`] actions
//! replayed against type-tagged operand buckets. Leaves are constants,
//! entry reads of tracked registers and RAM loads. No control flow.

use proptest::prelude::*;
use strider_ir::node::ValueType;
use strider_ir::{ExtendOp, FunctionBuilder, IRBuilderExt, IntBinaryOp, IntCmpOp, IntUnaryOp};

/// I8..I64 only, so no step crosses the wide-constant boundary.
fn int_ty() -> impl Strategy<Value = ValueType> {
    prop_oneof![
        Just(ValueType::int(8)),
        Just(ValueType::int(16)),
        Just(ValueType::int(32)),
        Just(ValueType::int(64)),
    ]
}

fn binary_op() -> impl Strategy<Value = IntBinaryOp> {
    prop::sample::select(IntBinaryOp::ALL.to_vec())
}

fn unary_op() -> impl Strategy<Value = IntUnaryOp> {
    prop::sample::select(IntUnaryOp::ALL.to_vec())
}

fn cmp_op() -> impl Strategy<Value = IntCmpOp> {
    prop::sample::select(IntCmpOp::ALL.to_vec())
}

fn extend_op() -> impl Strategy<Value = ExtendOp> {
    prop_oneof![Just(ExtendOp::ZeroExtend), Just(ExtendOp::SignExtend)]
}

/// Registers tracked per [`int_ty`] width, each its own container.
const REGS_PER_WIDTH: u64 = 2;

/// Register `idx` (mod [`REGS_PER_WIDTH`]) of `width`'s byte size.
fn tracked_reg(width: ValueType, idx: u8) -> rsleigh::Vn {
    let bytes = (width.bit_width() / 8) as u64;
    let slot = bytes.trailing_zeros() as u64 * REGS_PER_WIDTH + u64::from(idx) % REGS_PER_WIDTH;
    rsleigh::Vn {
        addr_off: 0x100 + 0x10 * slot,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: bytes as u32,
    }
}

/// Operand indices are taken modulo the bucket size at replay time, so no
/// step is ever out of bounds; a step needing an empty bucket is a no-op.
#[derive(Debug, Clone)]
pub enum Step {
    EmitIntConst {
        width: ValueType,
        value: u64,
        lift_off: u16,
    },
    /// The entry value of a tracked register.
    EmitEntryVar {
        width: ValueType,
        idx: u8,
        lift_off: u16,
    },
    /// A RAM load through an I64 address.
    EmitLoad {
        width: ValueType,
        addr_idx: u8,
        lift_off: u16,
    },
    EmitBinaryOp {
        width: ValueType,
        op: IntBinaryOp,
        lhs_idx: u8,
        rhs_idx: u8,
        lift_off: u16,
    },
    EmitUnaryOp {
        width: ValueType,
        op: IntUnaryOp,
        src_idx: u8,
        lift_off: u16,
    },
    EmitCmp {
        width: ValueType,
        op: IntCmpOp,
        lhs_idx: u8,
        rhs_idx: u8,
        lift_off: u16,
    },
    EmitTruncate {
        src_width: ValueType,
        dst_width: ValueType,
        src_idx: u8,
        lift_off: u16,
    },
    EmitExtend {
        src_width: ValueType,
        dst_width: ValueType,
        op: ExtendOp,
        src_idx: u8,
        lift_off: u16,
    },
    EmitBitCount {
        src_width: ValueType,
        dst_width: ValueType,
        lzcount: bool,
        src_idx: u8,
        lift_off: u16,
    },
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        // Leaves are weighted higher so the operand pool stays non-empty.
        2 => (int_ty(), any::<u64>(), any::<u16>())
            .prop_map(|(width, value, lift_off)| Step::EmitIntConst { width, value, lift_off }),
        3 => (int_ty(), any::<u8>(), any::<u16>())
            .prop_map(|(width, idx, lift_off)| Step::EmitEntryVar { width, idx, lift_off }),
        1 => (int_ty(), any::<u8>(), any::<u16>())
            .prop_map(|(width, addr_idx, lift_off)| Step::EmitLoad { width, addr_idx, lift_off }),
        2 => (int_ty(), binary_op(), any::<u8>(), any::<u8>(), any::<u16>())
            .prop_map(|(width, op, lhs_idx, rhs_idx, lift_off)|
                Step::EmitBinaryOp { width, op, lhs_idx, rhs_idx, lift_off }),
        2 => (int_ty(), unary_op(), any::<u8>(), any::<u16>())
            .prop_map(|(width, op, src_idx, lift_off)|
                Step::EmitUnaryOp { width, op, src_idx, lift_off }),
        1 => (int_ty(), cmp_op(), any::<u8>(), any::<u8>(), any::<u16>())
            .prop_map(|(width, op, lhs_idx, rhs_idx, lift_off)|
                Step::EmitCmp { width, op, lhs_idx, rhs_idx, lift_off }),
        1 => (int_ty(), int_ty(), any::<u8>(), any::<u16>())
            .prop_map(|(src_width, dst_width, src_idx, lift_off)|
                Step::EmitTruncate { src_width, dst_width, src_idx, lift_off }),
        1 => (int_ty(), int_ty(), extend_op(), any::<u8>(), any::<u16>())
            .prop_map(|(src_width, dst_width, op, src_idx, lift_off)|
                Step::EmitExtend { src_width, dst_width, op, src_idx, lift_off }),
        1 => (int_ty(), int_ty(), any::<bool>(), any::<u8>(), any::<u16>())
            .prop_map(|(src_width, dst_width, lzcount, src_idx, lift_off)|
                Step::EmitBitCount { src_width, dst_width, lzcount, src_idx, lift_off }),
    ]
}

pub fn step_seq() -> impl Strategy<Value = Vec<Step>> {
    proptest::collection::vec(step_strategy(), 1..50)
}

/// One operand bucket per width `int_ty()` emits, plus I1 for comparisons.
#[derive(Default)]
struct Pools {
    u8s: Vec<strider_ir::node::ValueId>,
    u16s: Vec<strider_ir::node::ValueId>,
    u32s: Vec<strider_ir::node::ValueId>,
    u64s: Vec<strider_ir::node::ValueId>,
    bools: Vec<strider_ir::node::ValueId>,
    /// The latest value any step emitted: what the replay returns.
    last: Option<strider_ir::node::ValueId>,
}

impl Pools {
    fn bucket(&self, ty: ValueType) -> &Vec<strider_ir::node::ValueId> {
        match ty {
            ValueType::Int(8) => &self.u8s,
            ValueType::Int(16) => &self.u16s,
            ValueType::Int(32) => &self.u32s,
            ValueType::Int(64) => &self.u64s,
            ValueType::I1 => &self.bools,
            ValueType::Int(_) | ValueType::Float(_) => {
                panic!("unsupported width in strategy: {ty:?}")
            }
        }
    }

    fn bucket_mut(&mut self, ty: ValueType) -> &mut Vec<strider_ir::node::ValueId> {
        match ty {
            ValueType::Int(8) => &mut self.u8s,
            ValueType::Int(16) => &mut self.u16s,
            ValueType::Int(32) => &mut self.u32s,
            ValueType::Int(64) => &mut self.u64s,
            ValueType::I1 => &mut self.bools,
            ValueType::Int(_) | ValueType::Float(_) => {
                panic!("unsupported width in strategy: {ty:?}")
            }
        }
    }

    fn pick(&self, ty: ValueType, idx: u8) -> Option<strider_ir::node::ValueId> {
        let b = self.bucket(ty);
        if b.is_empty() {
            None
        } else {
            Some(b[(idx as usize) % b.len()])
        }
    }

    fn push(&mut self, ty: ValueType, v: strider_ir::node::ValueId) {
        self.bucket_mut(ty).push(v);
        self.last = Some(v);
    }
}

/// `None` when the graph would be empty (nothing to return).
pub fn replay(steps: &[Step]) -> Option<strider_ir::Function> {
    let mut regs = crate::RegisterSet::new();
    for width in [8, 16, 32, 64] {
        for idx in 0..REGS_PER_WIDTH as u8 {
            regs = regs.tracked(tracked_reg(ValueType::int(width), idx));
        }
    }
    let mut b = regs.build_fn_single_region().ok()?;

    let mut pools = Pools::default();

    for s in steps {
        let lift_addr = crate::SENTINEL_LIFT_ADDR.wrapping_add(step_lift_off(s) as u64);
        b.set_lift_addr(Some(lift_addr));
        apply_step(&mut b, &mut pools, s);
    }
    b.set_lift_addr(Some(crate::SENTINEL_LIFT_ADDR));

    let ret_val = pools.last?;
    b.build_return(Some(ret_val), &[]).ok()?;
    b.set_lift_addr(None);

    b.build().ok()
}

fn step_lift_off(s: &Step) -> u16 {
    match s {
        Step::EmitIntConst { lift_off, .. }
        | Step::EmitEntryVar { lift_off, .. }
        | Step::EmitLoad { lift_off, .. }
        | Step::EmitBitCount { lift_off, .. }
        | Step::EmitBinaryOp { lift_off, .. }
        | Step::EmitUnaryOp { lift_off, .. }
        | Step::EmitCmp { lift_off, .. }
        | Step::EmitTruncate { lift_off, .. }
        | Step::EmitExtend { lift_off, .. } => *lift_off,
    }
}

fn apply_step(b: &mut FunctionBuilder, pools: &mut Pools, s: &Step) {
    match s {
        Step::EmitIntConst { width, value, .. } => {
            if let Ok(v) = b.build_int_const(*value as u128, *width) {
                pools.push(*width, v);
            }
        }
        Step::EmitEntryVar { width, idx, .. } => {
            if let Ok(v) = b.read_variable(&tracked_reg(*width, *idx)) {
                pools.push(*width, v);
            }
        }
        Step::EmitLoad {
            width, addr_idx, ..
        } => {
            let Some(addr) = pools.pick(ValueType::int(64), *addr_idx) else {
                return;
            };
            if let Ok(v) = b.build_load(addr, rsleigh::VnSpace::RAM, *width) {
                pools.push(*width, v);
            }
        }
        Step::EmitBitCount {
            src_width,
            dst_width,
            lzcount,
            src_idx,
            ..
        } => {
            let Some(src) = pools.pick(*src_width, *src_idx) else {
                return;
            };
            let v = if *lzcount {
                b.build_lzcount(src, *dst_width)
            } else {
                b.build_popcount(src, *dst_width)
            };
            if let Ok(v) = v {
                pools.push(*dst_width, v);
            }
        }
        Step::EmitBinaryOp {
            width,
            op,
            lhs_idx,
            rhs_idx,
            ..
        } => {
            let Some(lhs) = pools.pick(*width, *lhs_idx) else {
                return;
            };
            let Some(rhs) = pools.pick(*width, *rhs_idx) else {
                return;
            };
            if let Ok(v) = b.build_int_binary_operation(lhs, rhs, *op, *width) {
                pools.push(*width, v);
            }
        }
        Step::EmitUnaryOp {
            width, op, src_idx, ..
        } => {
            let Some(src) = pools.pick(*width, *src_idx) else {
                return;
            };
            if let Ok(v) = b.build_int_unary_operation(src, *op, *width) {
                pools.push(*width, v);
            }
        }
        Step::EmitCmp {
            width,
            op,
            lhs_idx,
            rhs_idx,
            ..
        } => {
            let Some(lhs) = pools.pick(*width, *lhs_idx) else {
                return;
            };
            let Some(rhs) = pools.pick(*width, *rhs_idx) else {
                return;
            };
            if let Ok(v) = b.build_int_cmp_operation(lhs, rhs, *op, *width) {
                pools.push(ValueType::I1, v);
            }
        }
        Step::EmitTruncate {
            src_width,
            dst_width,
            src_idx,
            ..
        } => {
            let Some(src) = pools.pick(*src_width, *src_idx) else {
                return;
            };
            // Both builders return the input unchanged when it already fits;
            // pushing that would put a wrong-width value in the bucket.
            if let Ok(v) = b.truncate_if_needed(src, *dst_width)
                && v != src
            {
                pools.push(*dst_width, v);
            }
        }
        Step::EmitExtend {
            src_width,
            dst_width,
            op,
            src_idx,
            ..
        } => {
            let Some(src) = pools.pick(*src_width, *src_idx) else {
                return;
            };
            if let Ok(v) = b.extend_if_needed(src, *dst_width, *op)
                && v != src
            {
                pools.push(*dst_width, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::NodeKind;
    use strider_ir::{IRViewer, IRWalker};

    /// A widening `EmitTruncate` returns the I32 input, which must not enter
    /// the I64 bucket.
    #[test]
    fn a_no_op_width_change_does_not_poison_the_destination_bucket() {
        let f = replay(&[
            Step::EmitIntConst {
                width: ValueType::int(64),
                value: 1,
                lift_off: 0,
            },
            Step::EmitIntConst {
                width: ValueType::int(32),
                value: 2,
                lift_off: 1,
            },
            Step::EmitTruncate {
                src_width: ValueType::int(32),
                dst_width: ValueType::int(64),
                src_idx: 0,
                lift_off: 2,
            },
            // Index 1 of the I64 bucket: the poisoned entry, if it got in.
            Step::EmitBinaryOp {
                width: ValueType::int(64),
                op: IntBinaryOp::Add,
                lhs_idx: 1,
                rhs_idx: 1,
                lift_off: 3,
            },
        ])
        .expect("the sequence returns a value");
        assert!(
            f.walk()
                .any(|n| matches!(f.node_kind(n), NodeKind::IntBinaryOp(_))),
            "the Add was skipped: an I32 value reached the I64 bucket",
        );
    }
}
