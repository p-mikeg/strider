//! Property-based invariants over value-only DAGs, generated as a sequence of
//! [`FunctionBuilder`] actions against type-tagged operand buckets.
//!
//! Value-only on purpose: control-flow invariants stay in hand-authored
//! fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::enum_variant_names
)]

use proptest::prelude::*;
use strider_ir::{IRBuilderExt, IRWalker};

use strider_ir::node::ValueType;
use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp, IntCmpOp, IntUnaryOp};

/// Base address every step stamps its own `lift_off` on top of.
const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// I8..I64 only, so no step crosses the wide-constant boundary.
fn int_ty() -> impl Strategy<Value = ValueType> {
    prop_oneof![
        Just(ValueType::I8),
        Just(ValueType::I16),
        Just(ValueType::I32),
        Just(ValueType::I64),
    ]
}

fn binary_op() -> impl Strategy<Value = IntBinaryOp> {
    prop_oneof![
        Just(IntBinaryOp::Add),
        Just(IntBinaryOp::Mul),
        Just(IntBinaryOp::And),
        Just(IntBinaryOp::Or),
        Just(IntBinaryOp::Xor),
        Just(IntBinaryOp::ShiftLeft),
        Just(IntBinaryOp::ShiftRight),
    ]
}

fn unary_op() -> impl Strategy<Value = IntUnaryOp> {
    // `Neg` is the only variant: bitwise complement is `Xor(x, all_ones)`.
    Just(IntUnaryOp::Neg)
}

fn cmp_op() -> impl Strategy<Value = IntCmpOp> {
    prop_oneof![
        Just(IntCmpOp::Equal),
        Just(IntCmpOp::Less),
        Just(IntCmpOp::Sless),
    ]
}

fn extend_op() -> impl Strategy<Value = ExtendOp> {
    prop_oneof![Just(ExtendOp::ZeroExtend), Just(ExtendOp::SignExtend)]
}

/// Operand indices are taken modulo the bucket size at replay time, so no
/// step is ever out of bounds; a step needing an empty bucket is a no-op.
#[derive(Debug, Clone)]
enum Step {
    EmitIntConst {
        width: ValueType,
        value: u64,
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
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        // Constants are weighted higher so the operand pool stays non-empty.
        4 => (int_ty(), any::<u64>(), any::<u16>())
            .prop_map(|(width, value, lift_off)| Step::EmitIntConst { width, value, lift_off }),
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
    ]
}

fn step_seq() -> impl Strategy<Value = Vec<Step>> {
    proptest::collection::vec(step_strategy(), 1..50)
}

/// One operand bucket per width `int_ty()` emits, plus I1 for comparisons.
#[derive(Default)]
struct Pools {
    u8s: Vec<strider_ir::Value>,
    u16s: Vec<strider_ir::Value>,
    u32s: Vec<strider_ir::Value>,
    u64s: Vec<strider_ir::Value>,
    bools: Vec<strider_ir::Value>,
}

impl Pools {
    fn bucket(&self, ty: ValueType) -> &Vec<strider_ir::Value> {
        match ty {
            ValueType::I8 => &self.u8s,
            ValueType::I16 => &self.u16s,
            ValueType::I32 => &self.u32s,
            ValueType::I64 => &self.u64s,
            ValueType::I1 => &self.bools,
            _ => panic!("unsupported width in strategy: {ty:?}"),
        }
    }

    fn bucket_mut(&mut self, ty: ValueType) -> &mut Vec<strider_ir::Value> {
        match ty {
            ValueType::I8 => &mut self.u8s,
            ValueType::I16 => &mut self.u16s,
            ValueType::I32 => &mut self.u32s,
            ValueType::I64 => &mut self.u64s,
            ValueType::I1 => &mut self.bools,
            _ => panic!("unsupported width in strategy: {ty:?}"),
        }
    }

    fn pick(&self, ty: ValueType, idx: u8) -> Option<strider_ir::Value> {
        let b = self.bucket(ty);
        if b.is_empty() {
            None
        } else {
            Some(b[(idx as usize) % b.len()])
        }
    }

    /// Any tracked value, so the replay always has something to return.
    fn any_value(&self) -> Option<strider_ir::Value> {
        for b in [&self.u64s, &self.u32s, &self.u16s, &self.u8s, &self.bools] {
            if let Some(&v) = b.last() {
                return Some(v);
            }
        }
        None
    }
}

/// `None` when the graph would be empty (nothing to return).
fn replay(steps: &[Step]) -> Option<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder().ok()?;
    let region = b.create_region_all().ok()?;
    b.set_entry_region_all(region).ok()?;
    b.set_region(region);

    let mut pools = Pools::default();

    for s in steps {
        let lift_addr = SENTINEL_LIFT_ADDR.wrapping_add(step_lift_off(s) as u64);
        b.set_lift_addr(Some(lift_addr));
        apply_step(&mut b, &mut pools, s);
    }
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let ret_val = pools.any_value()?;
    b.build_return(Some(ret_val), &[]).ok()?;
    b.set_lift_addr(None);

    b.build().ok()
}

fn step_lift_off(s: &Step) -> u16 {
    match s {
        Step::EmitIntConst { lift_off, .. }
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
                pools.bucket_mut(*width).push(v);
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
                pools.bucket_mut(*width).push(v);
            }
        }
        Step::EmitUnaryOp {
            width, op, src_idx, ..
        } => {
            let Some(src) = pools.pick(*width, *src_idx) else {
                return;
            };
            if let Ok(v) = b.build_int_unary_operation(src, *op, *width) {
                pools.bucket_mut(*width).push(v);
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
                pools.bucket_mut(ValueType::I1).push(v);
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
            if let Ok(v) = b.truncate_if_needed(src, *dst_width) {
                pools.bucket_mut(*dst_width).push(v);
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
            if let Ok(v) = b.extend_if_needed(src, *dst_width, *op) {
                pools.bucket_mut(*dst_width).push(v);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_validate_always_passes(steps in step_seq()) {
        let Some(fg) = replay(&steps) else {
            return Ok(());
        };
        let res = strider_ir::validate::validate(&fg);
        prop_assert!(
            res.is_ok(),
            "validate failed on a strategy-generated graph: {:?}",
            res.err()
        );
    }

    #[test]
    fn prop_preorder_visits_each_node_at_most_once(steps in step_seq()) {
        let Some(fg) = replay(&steps) else {
            return Ok(());
        };
        use std::collections::HashSet;
        let visited: Vec<_> = fg.walk().collect();
        let unique: HashSet<_> = visited.iter().copied().collect();
        prop_assert_eq!(
            visited.len(),
            unique.len(),
            "preorder produced a duplicate visit: {:?}",
            visited
        );
    }

    /// Dedup must be independent of construction history.
    #[test]
    fn prop_dedup_determinism(steps in step_seq()) {
        let Some(mut fg) = replay(&steps) else {
            return Ok(());
        };
        use strider_ir::node::{NodeKind, ValueKind, ValueType};
        let id = fg.intern_int_const(42, ValueType::I32);
        let a = fg.graph_mut().create_node(
            NodeKind::IntConst(id),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let b = fg.graph_mut().create_node(
            NodeKind::IntConst(id),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        prop_assert_eq!(
            a, b,
            "dedup cache returned distinct NodeIds for identical IntConst(42:I32)"
        );
    }
}
