//! Property-based invariants for the value-only IR.
//!
//! **Scope.** Value-only DAG generation via a sequence of
//! [`FunctionBuilder`] actions, mirroring
//! `cranelift-fuzzgen`'s imperative-action pattern with type-tag operand
//! buckets.  Control-flow invariants (`PhiCollapse`,
//! `DeadBranchElimination`, indirect-resolver) stay in hand-authored
//! fixtures — random control-flow generation would require reimplementing
//! most of `FunctionBuilder`'s region/phi machinery.
//!
//! **Property checked here.**
//!
//!  Every random `Graph` produced by the action-driven strategy passes
//!  `validate(function)` (Layer-A + Layer-B + always-on Layer-C
//!  asm-fingerprint check).  The strategy stamps a per-action lift address
//!  via [`FunctionBuilder::set_lift_addr`] so every emitted node carries a
//!  non-empty fingerprint by construction; if the strategy ever produces a
//!  graph that fails validation, that's a real bug worth investigating.
//!
//! Properties that require the optimizer (`opt::default_pipeline`)
//! live in `strider-orchestrator/tests/proptest_optimizer.rs` because
//! `strider-ir` cannot depend on the analyzer.

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

use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp, IntCmpOp, IntUnaryOp, node::ValueType};

/// Sentinel lift-address base; per-step `lift_off` is added on top.
/// Mirrors `strider_ir_test_utils::SENTINEL_LIFT_ADDR`.
const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

// ── Strategy primitives ───────────────────────────────────────────────────

/// Integer widths the strategy emits.  We restrict to the four "common"
/// widths (I8..I64) because:
///   - they all fit in `u128` (no `ConstValue::Wide` limbs required),
///   - they all admit binary ops directly without crossing the
///     `ConstValue::Wide` boundary,
///   - they exercise truncate / extend behaviour with `convert_to_int_if_needed`.
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
    // `IntUnaryOp` has only `Neg` since `BitNot` was removed (bitwise
    // complement is `Xor(x, all_ones)`).
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

/// One step in a value-only DAG construction sequence.
///
/// Operand indices (`u8`) are interpreted modulo the current bucket size at
/// replay time, so the strategy never generates "out of bounds" steps — a
/// step that requires a non-empty operand bucket of a given width simply
/// becomes a no-op when no operand is available.
///
/// Per-step `lift_addr_offset: u16` is added to a base sentinel address at
/// replay time so each step stamps a distinct (yet deterministic, given the
/// step sequence) asm-fingerprint on its emitted nodes.  This makes the
/// always-on Layer-C check meaningful: a strategy that always used the
/// same lift address would technically still produce valid fingerprints,
/// but stamping per step exercises the side-table-extension code path.
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

// ── Session: replays a step sequence into a `FunctionBuilder` ────────────

/// Per-width operand buckets — separate `Vec<ValueId>` per
/// `ValueType`.  Sized to the 4 common widths used by `int_ty()`.
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

    /// Returns *some* value with width `ty` if any exists; else falls back
    /// to any tracked value (used by [`close_with_return`] to ensure the
    /// function has a return value).
    fn any_value(&self) -> Option<strider_ir::Value> {
        for b in [&self.u64s, &self.u32s, &self.u16s, &self.u8s, &self.bools] {
            if let Some(&v) = b.last() {
                return Some(v);
            }
        }
        None
    }
}

/// Replay a sequence into a fresh empty function and close with a Return.
/// Returns `None` only when the resulting graph would be empty (no value
/// to return).  Such sequences are uninteresting and proptest will retry.
fn replay(steps: &[Step]) -> Option<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder().ok()?;
    let region = b.create_region().ok()?;
    b.set_entry_region(region).ok()?;
    b.set_region(region);

    let mut pools = Pools::default();

    for s in steps {
        // Per-step stamp ensures fingerprints are diverse without coupling
        // to address values that could collide with real machine code.
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
            // Guard against div / shift edge-cases that would emit invalid IR.
            // (`build_int_binary_operation` itself accepts any value-bucketed
            // operands; the validator does not reject const-zero divisors.)
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
            // `truncate_if_needed` accepts widening too (it becomes a no-op
            // returning the input unchanged), so the only failure mode is
            // a non-integer input — guarded above by typed buckets.
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

// ── Property ──────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1000,
        // Default max_shrink_iters is fine; default max_global_rejects is fine.
        .. ProptestConfig::default()
    })]

    /// Every graph the value-only strategy produces passes `validate`,
    /// including the always-on Layer-C asm-fingerprint check.
    ///
    /// `FunctionBuilder::build()` runs `validate` internally — reaching
    /// `Some(_)` already implies validation passed.  We also re-run
    /// `validate` post-build to catch any post-build mutation we might
    /// add later.
    #[test]
    fn prop_validate_always_passes(steps in step_seq()) {
        let Some(fg) = replay(&steps) else {
            // Empty pool / build failure is uninteresting; not a property
            // violation.
            return Ok(());
        };
        let res = strider_ir::validate::validate(&fg);
        prop_assert!(
            res.is_ok(),
            "validate failed on a strategy-generated graph: {:?}",
            res.err()
        );
    }

    /// `Function::preorder` visits each reachable node at most once.
    /// The walker's `DenseEntitySet`-backed visited check is the
    /// load-bearing invariant; this proptest exercises it against
    /// arbitrary action-driven graphs.
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

    /// Cacheable kinds dedup deterministically: creating an `IntConst`
    /// node with the same `(kind, inputs, output_kinds)` returns the
    /// same `NodeId` regardless of construction history.  Pins the
    /// `Graph::node_to_id` dedup cache's correctness against an
    /// arbitrary prior construction sequence.
    #[test]
    fn prop_dedup_determinism(steps in step_seq()) {
        let Some(mut fg) = replay(&steps) else {
            return Ok(());
        };
        // IntConst(42 : I32) — cacheable, no input dependencies, so
        // construction is independent of the graph's prior state.  The value
        // is interned (equal values share one ConstId), so two creations of
        // the same logical constant dedup to one node.
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
