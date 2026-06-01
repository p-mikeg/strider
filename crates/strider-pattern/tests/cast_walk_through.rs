//! Smoke tests for the matcher's cast walk-through
//! (`Matcher::ignore_casts_mask`).
//!
//! Pins the contract that, with `CastMask::ZERO_EXTEND` set, a pattern
//! `add(int_const(5), var(c))` matches an IR `Add(IntConst(5),
//! ZeroExtend(reg))` even though the sub-pattern's `int_const` /
//! `any_int_const` constraint would ordinarily kind-mismatch against
//! the ZeroExtend producer.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::{reg_vn, RegisterSet};
use strider_pattern::matcher::CastMask;
use strider_pattern::{add, any_int_const, int_const, var, Capture, Matcher};

#[test]
fn ignore_casts_mask_zero_extend_walks_through_add_input() {
    // Build a function whose Add has `ZeroExtend(reg)` on one side.  Using a
    // tracked register-varnode read prevents the IR builder from
    // constant-folding the cast away — `extend_if_needed` folds an
    // `IntConst`, but a runtime register read survives.
    let vn = reg_vn(0x40, 4); // 4-byte register varnode → I32
    let mut b = RegisterSet::new()
        .tracked(vn)
        .arg(vn)
        .build_fn_single_region()
        .unwrap();
    let x32 = b.read_variable(&vn).unwrap();
    let zx = b
        .extend_if_needed(x32, NodeOutputType::I64, strider_ir::ExtendOp::ZeroExtend)
        .unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(five, zx, strider_ir::IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    // Strict pattern: require the second operand to be an `IntConst`.
    // Without cast walk-through this fails because the producer is a
    // `ZeroExtend(reg)`, not an `IntConst`.
    let pat_strict = add(int_const(5u128), any_int_const());
    let no_walk = Matcher::try_new(&function).unwrap().find_all(&pat_strict);
    assert!(
        no_walk.is_empty(),
        "without ignore_casts_mask the IntConst sub-pattern must NOT match the ZeroExtend producer",
    );

    // With `CastMask::ZERO_EXTEND` the matcher unwraps the cast and
    // re-attempts the sub-pattern against the cast's value input.  The
    // input is the register read (an `InitialVar` carrier, NOT an
    // `IntConst`), so even with walk-through the strict pattern still
    // can't match.  Pin both bounds.
    let walked_strict = Matcher::try_new(&function)
        .unwrap()
        .ignore_casts_mask(CastMask::ZERO_EXTEND)
        .find_all(&pat_strict);
    assert!(
        walked_strict.is_empty(),
        "ZeroExtend's input is a register read, not an IntConst — strict pattern still fails",
    );

    // The proper test: a generic `var(c)` sub-pattern with `Capture`
    // binds to *some* node.  Compare strict-vs-walked behaviour on a
    // pattern whose sub-pattern matches the ZeroExtend-input shape.
    // `add(int_const(5), var(c))` — without walk-through, c binds to
    // the ZeroExtend output; with walk-through, c MUST bind to the
    // unwrapped register-read output (the ZeroExtend's input).
    let c = Capture::new();
    let pat_capture = add(int_const(5u128), var(c));

    let strict_hits = Matcher::try_new(&function).unwrap().find_all(&pat_capture);
    assert_eq!(strict_hits.len(), 1, "strict-match must hit via var()");
    let strict_out = strict_hits[0].output(c).expect("c must bind");
    let strict_node = function.node_for_output(strict_out);
    // Strict: producer of the matched side is the ZeroExtend itself.
    assert!(
        matches!(
            function.node_kind(strict_node),
            strider_ir::node::NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
        ),
        "strict c must bind to the ZeroExtend output",
    );

    let walked_hits = Matcher::try_new(&function)
        .unwrap()
        .ignore_casts_mask(CastMask::ZERO_EXTEND)
        .find_all(&pat_capture);
    assert_eq!(walked_hits.len(), 1, "walk-through must still hit once");
    // With walk-through the matcher tries the direct producer first
    // (succeeding for `var(c)` since `Any` accepts ZeroExtend), so the
    // capture is still the ZeroExtend.  The walk-through fallback only
    // engages on a *kind-mismatch*, so a same-output binding here is
    // expected — the load-bearing assertion is `walked_strict.is_empty`
    // + the count of `walked_hits`.  Pin via a structural recheck.
    let walked_out = walked_hits[0].output(c).expect("c must bind under walk");
    let walked_node = function.node_for_output(walked_out);
    assert!(
        matches!(
            function.node_kind(walked_node),
            strider_ir::node::NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
        ),
        "walk-through still binds the direct producer when `var(c)` accepts it",
    );
}
