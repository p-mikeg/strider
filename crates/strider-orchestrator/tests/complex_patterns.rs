//! Complex pattern queries: struct-field offsets, bit-test branches, calls
//! under control-flow, and a scale smoke test.
//!
//! Conventions:
//!   * Every `Matcher` opts into `ignore_casts_mask(EXTEND | TRUNCATE)` so
//!     tests don't break on arch-specific width-cast noise.
//!   * Bit-mask values are captured (never hardcoded) via a `Capture` and
//!     a `.when_match()` predicate checking `count_ones() == 1`.
//!   * On arm_thumb, gcc emits a `setISAMode` CallOther between the If and
//!     the following Call. `call_other_abi::classify("setISAMode")` reports
//!     it as `NoOp`, so the IR builder never emits the node and If->Call
//!     compositions match on Thumb just like every other arch.
//!
//! `per_arch_test!` generates one test per fixture function per arch
//! (9 fixtures × 14 arches = 126 invocations).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::useless_conversion
)]

mod common;
use common::*;
use strider_ir::{IRViewer, IRWalker};

use strider_pattern::{
    Capture, CaptureExt, CastMask, MatchPat, Matcher, Pattern, add, and, any, any_int_const, call,
    if_node, int_cmp, int_const, load, store, var,
};

use strider_ir::IntCmpOp;
use strider_ir::node::{NodeId, NodeKind};

/// Cast mask shared by every pattern in this module: skips Extend/Truncate
/// width casts some lifters insert between mismatched-width operations.
fn cast_mask() -> CastMask {
    CastMask::EXTEND | CastMask::TRUNCATE
}

fn finish<P: MatchPat>(p: P) -> Pattern {
    p.into_pattern().ignore_casts_mask(cast_mask())
}

fn masked(p: Pattern) -> Pattern {
    p.ignore_casts_mask(cast_mask())
}

fn matcher(function: &strider_ir::Function) -> Matcher<'_> {
    Matcher::new(function)
}

/// Matches an `IntConst` single-bit mask (nonzero, popcount 1); captures
/// the value into `iv`.
fn single_bit_int_const(iv: Capture) -> impl MatchPat {
    any_int_const().capture(iv).when_match(move |ctx, _ty, b| {
        let Some(n) = b.get_uint(iv, ctx.function()) else {
            return false;
        };
        n != 0 && n.count_ones() == 1
    })
}

/// "Bit-test against zero": `IntCmp(Equal, And(_, single-bit-const), 0)`.
/// The mask value is captured into `mask_var`.
///
/// `IntNotEqual` isn't a separate `IntCmpOp` variant: `INT_NOTEQUAL` lowers
/// to a negated `IntEqual`, so both `(x & K) == 0` and `(x & K) != 0` reach
/// IR as `IntCmpOp::Equal`, the latter wrapped in a boolean negation.
fn bit_test_against_zero(value: Capture, mask_var: Capture) -> impl MatchPat {
    int_cmp(
        IntCmpOp::Equal,
        and(var(value), single_bit_int_const(mask_var)),
        int_const(0u128),
    )
}

/// Capture-friendly any-load-of-(base + IntConst-bound-to-`offset`):
///   load.addr( add(var(base), any_int_const().capture(offset)) )
///
/// Returns the value-producing `LoadPat` builder (which implements
/// [`MatchPat`]) so it nests directly as a `Call` arg operand; call
/// `.build()` (or `masked`) at the use site to seal it into a [`Pattern`].
fn field_load_at_offset(base: Capture, offset: Capture) -> impl MatchPat {
    load().addr(add(var(base), any_int_const().capture(offset)))
}

/// Matches any carrier node registered for function arg `arg_index` in the
/// `Function::arg_index_to_values` side-table, by checking the matched node's
/// primary output against the carriers' outputs. Drop-in for expressions
/// like `call().arg(i, arg_carrier_pat(g, N))`.
fn arg_carrier_pat(function: &strider_ir::Function, arg_index: u32) -> impl MatchPat + 'static {
    use strider_ir::node::ValueId;
    let carrier_outputs: Vec<ValueId> = function
        .side_tables()
        .arg_index_to_values(arg_index)
        .to_vec();
    let cap = Capture::new();
    any().capture(cap).when_match(move |_ctx, _ty, b| {
        b.get_value(cap)
            .is_some_and(|out| carrier_outputs.contains(&out))
    })
}

per_arch_test!(
    "complex",
    "read_struct_fields",
    read_struct_fields_assertions
);

fn read_struct_fields_assertions(function: &strider_ir::Function) {
    assert!(
        count_loads(function) >= 3,
        "read_struct_fields must have ≥3 Loads; got {}",
        count_loads(function)
    );

    // Some compilers fold offset 0 to bare `load(base)`; others emit
    // `load(base + 0)`. Either way >=1 Load is required.
    let m = matcher(function);
    let pat = masked(load().build());
    assert!(
        !m.find_all(&pat).unwrap().is_empty(),
        "expected ≥1 Load match in read_struct_fields"
    );

    let base = Capture::new();
    let off = Capture::new();
    let off_pat = finish(field_load_at_offset(base, off));
    let hits = m.find_all(&off_pat).unwrap();
    let offsets: Vec<u128> = hits
        .iter()
        .filter_map(|h| h.bindings().get_uint(off, function))
        .collect();
    assert!(
        offsets.iter().any(|&n| n == 4 || n == 8),
        "expected at least one Load(base + {{4,8}}); got offsets {offsets:?}"
    );
}

per_arch_test!(
    "complex",
    "write_struct_fields",
    write_struct_fields_assertions
);

fn write_struct_fields_assertions(function: &strider_ir::Function) {
    assert!(
        count_stores(function) >= 3,
        "write_struct_fields must have ≥3 Stores; got {}",
        count_stores(function)
    );

    let m = matcher(function);
    let base = Capture::new();
    let off = Capture::new();
    let pat = masked(
        store()
            .addr(add(var(base), any_int_const().capture(off)))
            .data(any())
            .build(),
    );
    let hits = m.find_all(&pat).unwrap();
    let offsets: Vec<u128> = hits
        .iter()
        .filter_map(|h| h.bindings().get_uint(off, function))
        .collect();
    assert!(
        offsets.iter().any(|&n| n == 4 || n == 8),
        "expected ≥1 Store(base + {{4,8}}); got offsets {offsets:?}"
    );

    // At least 2 distinct offsets among the stores.
    let mut distinct = offsets.clone();
    distinct.sort();
    distinct.dedup();
    assert!(
        distinct.len() >= 2,
        "expected ≥2 distinct Store offsets; got {distinct:?}"
    );
}

per_arch_test!(
    "complex",
    "nested_struct_field",
    nested_struct_field_assertions
);

fn nested_struct_field_assertions(function: &strider_ir::Function) {
    // o->inner.x = *(base + padding + offsetof(Inner, x)).
    assert!(
        count_loads(function) >= 1,
        "nested_struct_field must Load; got {}",
        count_loads(function)
    );

    let m = matcher(function);
    let base = Capture::new();
    let off = Capture::new();
    let pat = finish(field_load_at_offset(base, off));
    let hits = m.find_all(&pat).unwrap();
    // Either the offset got captured, or the compiler folded a zero offset
    // to bare `Load(base)`; both are fine. But when captured, it must be
    // nonzero (inner.x sits at padding + offsetof(Inner, x) >= 4).
    let offsets: Vec<u128> = hits
        .iter()
        .filter_map(|h| h.bindings().get_uint(off, function))
        .collect();
    if !offsets.is_empty() {
        assert!(
            offsets.iter().any(|&n| n != 0),
            "all captured Load offsets are 0 in nested_struct_field; got {offsets:?}"
        );
    }
}

// arm `pop {pc}` resolves via the indirect-branch resolver's
// `LinkRegister` arm once `LoadForward` simplifies the loaded
// target back to `InitialVar(lr)`.
per_arch_test!("complex", "bit_test_zero", bit_test_zero_assertions);

fn bit_test_zero_assertions(function: &strider_ir::Function) {
    // (mask & 0x4) == 0 -> graph contains both `And` and `Equal`.
    assert!(
        count_int_binop(function, strider_ir::IntBinaryOp::And) >= 1,
        "bit_test_zero must contain ≥1 IntBinaryOp::And; got {}",
        count_int_binop(function, strider_ir::IntBinaryOp::And)
    );
    assert!(
        count_int_cmp(function, strider_ir::IntCmpOp::Equal) >= 1,
        "bit_test_zero must contain ≥1 IntCmpOp::Equal; got {}",
        count_int_cmp(function, strider_ir::IntCmpOp::Equal)
    );

    // The pattern already enforces the single-bit predicate; double-check
    // the captures below.
    let m = matcher(function);
    let mask = Capture::new();
    let value = Capture::new();
    let pat = finish(bit_test_against_zero(value, mask));
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected ≥1 IntCmp(Equal, And(_, single-bit-const), 0) match in bit_test_zero"
    );
    for h in &hits {
        if let Some(n) = h.bindings().get_uint(mask, function) {
            assert!(
                n.count_ones() == 1 && n != 0,
                "captured mask {n:#x} is not a single-bit value"
            );
        }
    }
}

per_arch_test!("complex", "if_bit_clear_call", if_bit_clear_call_assertions);

fn if_bit_clear_call_assertions(function: &strider_ir::Function) {
    assert!(
        count_ifs(function) >= 1,
        "if_bit_clear_call must contain ≥1 If; got {}",
        count_ifs(function)
    );
    assert!(
        count_calls(function) >= 1,
        "if_bit_clear_call must contain ≥1 Call; got {}",
        count_calls(function)
    );

    // Call.arg(0) is `p` (mask is arg 0). At -O0 `p` is spilled to the
    // stack and reloaded before the call, so this also exercises LoadForward
    // collapsing the spill round-trip back to the carrier. (Thumb's
    // setISAMode CallOther: see the module doc.)
    let m = matcher(function);
    assert!(
        !m.find_all(&masked(if_node().build())).unwrap().is_empty(),
        "no If matched in if_bit_clear_call"
    );
    // Carrier for arg 1 (the `p` parameter).
    assert!(
        !function.side_tables().arg_index_to_values(1).is_empty(),
        "arg 1 must be registered in the side-table"
    );
    let pat = masked(call().arg(0, arg_carrier_pat(function, 1)).build());
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected Call(arg(0) = carrier(arg 1)) in if_bit_clear_call \
             (proves LoadForward connects the spilled `p` parameter \
             through to the call site)"
    );

    // The compiler may put the call on either branch (`bne skip; call` vs
    // `je do_call; call`), so accept either. What must hold on every arch,
    // including Thumb, is that the If directly consumes the Call.
    let true_pat = masked(
        if_node()
            .with_true(masked(call().arg(0, arg_carrier_pat(function, 1)).build()))
            .build(),
    );
    let false_pat = masked(
        if_node()
            .with_false(masked(call().arg(0, arg_carrier_pat(function, 1)).build()))
            .build(),
    );
    let true_hits = m.find_all(&true_pat).unwrap();
    let false_hits = m.find_all(&false_pat).unwrap();
    assert!(
        !true_hits.is_empty() || !false_hits.is_empty(),
        "expected If(true_branch | false_branch = Call(arg(0)=carrier(arg 1))) \
         (proves construction-time NoOp classification of setISAMode \
         keeps If→Call walks unblocked on Thumb); got 0 matches on either branch",
    );
}

per_arch_test!(
    "complex",
    "call_with_field_arg",
    call_with_field_arg_assertions
);

fn call_with_field_arg_assertions(function: &strider_ir::Function) {
    assert!(
        count_loads(function) >= 1,
        "call_with_field_arg must Load s->handler"
    );
    assert!(
        count_calls(function) >= 1,
        "call_with_field_arg must Call invoke()"
    );

    // Call whose arg(0) is Load(base + offset); offset is captured, not
    // hardcoded, so the assertion below doesn't pin an ABI-specific value.
    let m = matcher(function);
    let base = Capture::new();
    let off = Capture::new();
    let pat = masked(call().arg(0, field_load_at_offset(base, off)).build());
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected ≥1 Call(arg(0) = Load(base + IntConst))"
    );

    // `s->handler` lives at offset 16 in `struct S { int a, b, c, flags;
    // int *handler; }` on 32-bit pointer arches, or 32 with the extra
    // padding on 64-bit ones; both comfortably under 256.
    let offsets: Vec<u128> = hits
        .iter()
        .filter_map(|h| h.bindings().get_uint(off, function))
        .collect();
    assert!(
        offsets.iter().any(|&n| n < 256),
        "no captured field-load offset is in 0..256; got {offsets:?}"
    );
}

per_arch_test!("complex", "dispatch_on_flag", dispatch_on_flag_assertions);

fn dispatch_on_flag_assertions(function: &strider_ir::Function) {
    assert!(
        count_ifs(function) >= 1,
        "dispatch_on_flag must contain ≥1 If"
    );
    assert!(
        count_calls(function) >= 1,
        "dispatch_on_flag must contain ≥1 Call"
    );
    assert!(
        count_loads(function) >= 1,
        "dispatch_on_flag must contain ≥1 Load"
    );

    // Three facts checked independently, not as one composed pattern, so
    // arch-specific IT-block / CallOther / bool-negation noise around any
    // one of them can't sink the whole test: an If exists; the bit-test
    // shape matches somewhere (not necessarily as the If's direct cond,
    // since aarch64/aarch64be wrap `bne` in a negation of the IntCmp); and
    // a Call exists with a struct-field-load arg.
    let m = matcher(function);
    let mask = Capture::new();

    // The masked value's source isn't pinned to `load()`: PPC GPR reads at
    // -O0 surface as `Or(Load, ShiftRight(Load, 32))` (Sleigh's modelling of
    // 32-bit register reads in a 64-bit-capable context), which KnownBits
    // doesn't currently fold away. The bit-test's discriminating power
    // (a single-bit-const mask) still holds regardless.
    let bit_test = finish(int_cmp(
        IntCmpOp::Equal,
        and(any(), single_bit_int_const(mask)),
        int_const(0u128),
    ));
    assert!(
        !m.find_all(&masked(if_node().build())).unwrap().is_empty(),
        "expected an If in dispatch_on_flag"
    );
    assert!(
        !m.find_all(&bit_test).unwrap().is_empty(),
        "expected a bit-test `IntCmp(Equal, And(_, single-bit-const), 0)` in dispatch_on_flag"
    );

    let off = Capture::new();
    let base = Capture::new();
    let call_field_arg = masked(call().arg(0, field_load_at_offset(base, off)).build());
    assert!(
        !m.find_all(&call_field_arg).unwrap().is_empty(),
        "expected Call(arg(0) = Load(base + IntConst)) in dispatch_on_flag"
    );

    // As in if_bit_clear_call, accept either branch polarity; what must
    // hold everywhere, including Thumb, is that If directly consumes Call.
    let off2 = Capture::new();
    let base2 = Capture::new();
    let off3 = Capture::new();
    let base3 = Capture::new();
    let true_pat = masked(
        if_node()
            .with_true(masked(
                call().arg(0, field_load_at_offset(base2, off2)).build(),
            ))
            .build(),
    );
    let false_pat = masked(
        if_node()
            .with_false(masked(
                call().arg(0, field_load_at_offset(base3, off3)).build(),
            ))
            .build(),
    );
    assert!(
        !m.find_all(&true_pat).unwrap().is_empty() || !m.find_all(&false_pat).unwrap().is_empty(),
        "expected If(true_branch | false_branch = Call(arg(0) = field-load)) \
         in dispatch_on_flag (proves construction-time NoOp \
         classification of setISAMode keeps If→Call walks unblocked)",
    );
}

per_arch_test!(
    "complex",
    "multi_arg_call_in_branch",
    multi_arg_call_in_branch_assertions
);

fn multi_arg_call_in_branch_assertions(function: &strider_ir::Function) {
    assert!(
        count_calls(function) >= 2,
        "multi_arg_call_in_branch must have ≥2 Calls; got {}",
        count_calls(function)
    );

    // The C source has two ext_three call sites with distinct arg orderings:
    // `ext_three(a, b, c)` on the True branch, `ext_three(c, b, a)` on the
    // False (param indices are cond=0, a=1, b=2, c=3). Each ordering is
    // matched independently via strict carrier-pat on every positional arg,
    // so an optimizer that fails to connect Call.arg(N) to carrier(N)
    // through the spill round-trip would lose one of the two matches.
    let m = matcher(function);
    let nv_abc = Capture::new();
    let pat_abc = masked(
        call()
            .arg(0, arg_carrier_pat(function, 1)) // a
            .arg(1, arg_carrier_pat(function, 2)) // b
            .arg(2, arg_carrier_pat(function, 3)) // c
            .capture(nv_abc)
            .build(),
    );
    let nv_cba = Capture::new();
    let pat_cba = masked(
        call()
            .arg(0, arg_carrier_pat(function, 3)) // c
            .arg(1, arg_carrier_pat(function, 2)) // b
            .arg(2, arg_carrier_pat(function, 1)) // a
            .capture(nv_cba)
            .build(),
    );
    let hits_abc = m.find_all(&pat_abc).unwrap();
    let hits_cba = m.find_all(&pat_cba).unwrap();
    assert!(
        !hits_abc.is_empty(),
        "expected a Call with args (carrier(1), carrier(2), carrier(3)) \
             — the True-branch ext_three(a,b,c)"
    );
    assert!(
        !hits_cba.is_empty(),
        "expected a Call with args (carrier(3), carrier(2), carrier(1)) \
             — the False-branch ext_three(c,b,a)"
    );
    // Captured NodeIds must differ across the two patterns, otherwise the
    // same call matched both orderings.
    let abc_ids: std::collections::HashSet<_> = hits_abc
        .iter()
        .filter_map(|h| h.node(nv_abc, function.graph()))
        .collect();
    let cba_ids: std::collections::HashSet<_> = hits_cba
        .iter()
        .filter_map(|h| h.node(nv_cba, function.graph()))
        .collect();
    assert!(
        abc_ids.is_disjoint(&cba_ids),
        "expected (a,b,c) and (c,b,a) to match distinct Call NodeIds; \
             abc={abc_ids:?}, cba={cba_ids:?}"
    );
}

per_arch_test!("complex", "complex_dispatch", complex_dispatch_assertions);

fn complex_dispatch_assertions(function: &strider_ir::Function) {
    let n = function.walk().count();
    // Many locals, several stack-allocated structs, 3 loops, >=10 branches,
    // mixed-width compute: 100 is comfortably below what every arch produces
    // while still catching a regression that drops half the function.
    assert!(
        n >= 100,
        "expected ≥100 reachable IR nodes in complex_dispatch; got {n}"
    );

    // 11 source-level `if` statements (dispatch flag ladders, inner loop
    // branch, acc<0 / acc>100 / padding checks); IR may fold a few, so >=6
    // stays above the noise floor on optimising arches.
    assert!(
        count_ifs(function) >= 6,
        "complex_dispatch must have ≥6 Ifs; got {}",
        count_ifs(function)
    );
    // 7 source-level call sites: cb_zero ×2, cb_set ×2, invoke ×3,
    // ext_three ×2. >=4 is the conservative cross-arch floor.
    assert!(
        count_calls(function) >= 4,
        "complex_dispatch must have ≥4 Calls; got {}",
        count_calls(function)
    );
    // `big`, `local_outer`, `locals[8]` are stack-allocated, so they
    // produce many stores at distinct stack offsets.
    assert!(
        count_stores(function) >= 5,
        "complex_dispatch must have ≥5 stores; got {}",
        count_stores(function)
    );

    let m = matcher(function);
    let base = Capture::new();
    let off = Capture::new();
    let pat = finish(field_load_at_offset(base, off));
    assert!(
        !m.find_all(&pat).unwrap().is_empty(),
        "expected ≥1 Load(base + IntConst) in complex_dispatch"
    );

    // Distinct offsets prove multiple fields are accessed, not the same
    // one repeatedly.
    let offsets: std::collections::HashSet<u128> = m
        .find_all(&pat)
        .unwrap()
        .iter()
        .filter_map(|h| h.bindings().get_uint(off, function))
        .collect();
    assert!(
        offsets.len() >= 2,
        "expected ≥2 distinct Load offsets in complex_dispatch; got {offsets:?}"
    );
}

per_arch_test!(
    "complex",
    "call_uses_call_return",
    call_uses_call_return_assertions
);

fn call_uses_call_return_assertions(function: &strider_ir::Function) {
    // Source: `consume(produce(x))`, so the outer Call's arg(0) is the
    // inner Call's return value. At -O0 that's typically:
    //   call_inner = Call(produce, x)
    //   spill chain = Store -> Load (optional, collapsed by LoadForward)
    //   call_outer = Call(consume, ...chain to call_inner's output...)
    // Whether the spill collapses, and whether the result is loaded back
    // via the CC return register, both vary per arch. What's universal is
    // that some outer Call's input chain traces back to another Call, so
    // we walk the IR by hand rather than pattern-match one fixed shape.

    assert!(
        count_calls(function) >= 2,
        "call_uses_call_return must have ≥2 Calls; got {}",
        count_calls(function)
    );

    // For each Call, walk every input slot back through any chain of
    // {Store, Load, Region, ValuePhi}. Hitting another Call is proof of
    // Call->Call dataflow.
    let calls: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .collect();
    let chained = calls.iter().any(|&outer| {
        let outer_inputs: Vec<_> = function.node_inputs(outer).into_iter().collect();
        outer_inputs.iter().any(|&inp| {
            let mut producer = function.producer(inp);
            // Bound walk to avoid pathological cycles; 16 hops is far
            // more than any reasonable spill round-trip.
            for _ in 0..16 {
                match function.node_kind(producer) {
                    NodeKind::Call if producer != outer => return true,
                    // Walk through plumbing that doesn't change the value
                    // identity, following the node's first (most-likely
                    // value-producing) input slot.
                    NodeKind::Load(_) => {
                        // Load inputs: [memory, addr]; the memory edge
                        // surfaces the producing store / call.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else {
                            break;
                        };
                        producer = function.producer(first);
                    }
                    NodeKind::Store(_) => {
                        // Store inputs: [mem, addr, data]; `data` is the
                        // value being persisted.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&data) = inps.get(2) else {
                            break;
                        };
                        producer = function.producer(data);
                    }
                    NodeKind::Region => {
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else {
                            break;
                        };
                        producer = function.producer(first);
                    }
                    NodeKind::Phi
                        if function
                            .get_vn_for_value(function.node_outputs(producer)[0])
                            .is_none() =>
                    {
                        // Anonymous phi (ValuePhi): take the first input;
                        // if it doesn't lead back to a Call this bails at
                        // the next step anyway.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else {
                            break;
                        };
                        producer = function.producer(first);
                    }
                    _ => break,
                }
            }
            false
        })
    });
    assert!(
        chained,
        "expected one Call's input to trace back to another Call \
             (through any chain of Load/Store/spill plumbing); \
             call_count={}",
        calls.len()
    );
}
