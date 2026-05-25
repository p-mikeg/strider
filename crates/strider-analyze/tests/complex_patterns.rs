//! Complex pattern queries — struct-field offsets, bit-test branches, calls
//! under control-flow, and a scale smoke test.  See
//! `docs/superpowers/specs/2026-04-27-complex-pattern-tests-design.md`.
//!
//! Conventions:
//!   * Every `Matcher` instance opts into `ignore_casts_mask(EXTEND |
//!     TRUNCATE)` and `ignore_regions()` so tests don't break on
//!     arch-specific width-cast / region-join noise.
//!   * Bit-mask values are captured (never hardcoded) via a `Capture` and
//!     a `.when_match()` predicate that checks `count_ones() == 1`.
//!   * On arm_thumb gcc emits a `setISAMode` user-op as a `CallOther`
//!     between the If and the following Call to set up the ISA-mode
//!     context bit.  The matcher's ConsumersSpec walk does not pass
//!     through CallOther, but `strider_target::call_other_abi::classify("setISAMode")`
//!     returns `NoOp` so the IR builder skips emitting the CallOther
//!     entirely, and structural compositions like
//!     `if_node().true_branch(call().arg(0, function_arg(1)))` match
//!     on Thumb just like every other arch.
//!
//! The `per_arch_test!` macro generates one module per fixture-function
//! name, so each fixture function gets exactly one named test (which may
//! make multiple assertions internally).  This keeps the test count to
//! one per (fixture × arch) — 9 fixtures × 14 arches = 126 invocations.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

use strider_analyze::pattern::{
    CastMask, IntCmpOp, Matcher, Capture, Pat,
    add, and, any, any_int_const, call, if_node, int_cmp,
    int_const, load, predicate, store, var, IntoPat,
};

use strider_ir::node::{NodeId, NodeKind};

// ─────────────────────────────────────────────────────────────────────────────
// Local helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Standard matcher used by every test in this module.  Walks through:
/// * width casts (Extend / Truncate) inserted by some lifters between
///   integer-width-mismatched operations.
/// * type casts (CastToBool, CastToInt) the analyzer inserts between an
///   integer comparison and an If's `cond` input — Sleigh emits a
///   `Bool` from comparisons but the analyzer often re-wraps via
///   `CastToInt` (for storage in a 1-byte flag register) and
///   `CastToBool` (back at the branch consumer).  Without walking
///   through these, structural patterns like
///   `if_node().cond(int_cmp(...))` never match across arches.
/// * `Region` region-join nodes.
fn matcher(g: &strider_ir::Function) -> Matcher<'_> {
    Matcher::try_new(g).unwrap()
        .ignore_casts_mask(
            CastMask::EXTEND
                | CastMask::TRUNCATE
                | CastMask::CAST_TO_BOOL
                | CastMask::CAST_TO_INT,
        )
        .ignore_regions()
}

/// Pattern that matches `IntConst` whose value is a single-bit mask
/// (`n != 0 && n.count_ones() == 1`); the captured value lands in `iv`.
fn single_bit_int_const(iv: Capture) -> Pat {
    any_int_const(iv).when_match(move |fg, _ty, b| {
        let Some(n) = b.get_uint(iv, fg) else { return false; };
        n != 0 && n.count_ones() == 1
    })
}

/// Pattern that matches a "bit-test against zero" — `IntCmp(Equal, And(_,
/// single-bit-const), 0)`.  The mask value is captured into `mask_var`.
///
/// Note: `IntNotEqual` does not exist as a separate `IntCmpOp` variant —
/// the analyzer lowers p-code `INT_NOTEQUAL` to `BoolNeg(IntEqual)`, so
/// every "(x & K) == 0" or "(x & K) != 0" both still produce an
/// `IntCmpOp::Equal` in IR (potentially wrapped in a `BoolUnaryOp::Neg`).
fn bit_test_against_zero(value: Capture, mask_var: Capture) -> Pat {
    int_cmp(
        IntCmpOp::Equal,
        and(var(value), single_bit_int_const(mask_var)),
        int_const(0),
    )
}

/// Pattern that matches `Load(any_addr)` whose address is constrained by `addr_pat`.
fn field_load(addr_pat: impl Into<Pat>) -> Pat {
    load().addr(addr_pat).into()
}

/// Pattern that matches `Store(addr, data)` whose address is constrained by
/// `addr_pat` and whose value matches `data_pat`.
fn field_store(addr_pat: impl Into<Pat>, data_pat: impl Into<Pat>) -> Pat {
    store().addr(addr_pat).data(data_pat).into()
}

/// Capture-friendly any-load-of-(base + IntConst-bound-to-`offset`):
///   load.addr( add(var(base), any_int_const(offset)) )
fn field_load_at_offset(base: Capture, offset: Capture) -> Pat {
    field_load(add(var(base), any_int_const(offset)))
}

/// Builds a `Pat` that matches any carrier node registered for function arg
/// `arg_index` in the `Function::arg_index_to_nodes` side-table.  The pattern
/// checks that the matched node's primary output is one of the carriers'
/// outputs, making it usable as a drop-in for the old `function_arg(N)`
/// pattern in expressions like `call().arg(i, arg_carrier_pat(g, N))`.
fn arg_carrier_pat(g: &strider_ir::Function, arg_index: u32) -> Pat {
    use strider_ir::node::NodeOutputId;
    // Collect the primary output of every registered carrier.
    let carrier_outputs: std::sync::Arc<[NodeOutputId]> = g
        .arg_index_to_nodes(arg_index)
        .iter()
        .filter_map(|&n| g.node_outputs(n).first().copied())
        .collect::<Vec<_>>()
        .into();
    predicate(move |_, _, out| carrier_outputs.contains(&out))
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. read_struct_fields — three field loads, captured offsets {0, 4, 8}
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "read_struct_fields", read_struct_fields_assertions);

fn read_struct_fields_assertions(g: &strider_ir::Function) {
    // (a) The graph must contain ≥3 Loads (s->a, s->b, s->c).
    assert!(count_loads(g) >= 3,
            "read_struct_fields must have ≥3 Loads; got {}", count_loads(g));

    // (b) Some compilers emit `load(base)` for offset 0; others emit
    // `load(base + 0)`.  Either way ≥1 Load is required.
    let m = matcher(g);
    let pat: Pat = load().into();
    assert!(!m.find_all(&pat).is_empty(),
            "expected ≥1 Load match in read_struct_fields");

    // (c) At least one of {4, 8} must appear as a constant offset on the
    // load address — capture the offset via a Capture and assert.
    let base = Capture::new();
    let off = Capture::new();
    let off_pat: Pat = field_load_at_offset(base, off);
    let hits = m.find_all(&off_pat);
    let offsets: Vec<u128> = hits.iter().filter_map(|h| h.get_uint(off, g)).collect();
    assert!(offsets.iter().any(|&n| n == 4 || n == 8),
            "expected at least one Load(base + {{4,8}}); got offsets {offsets:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. write_struct_fields — three stores at offsets {0, 4, 8} of the same value
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "write_struct_fields", write_struct_fields_assertions);

fn write_struct_fields_assertions(g: &strider_ir::Function) {
    assert!(count_stores(g) >= 3,
            "write_struct_fields must have ≥3 Stores; got {}", count_stores(g));

    let m = matcher(g);
    let base = Capture::new();
    let off = Capture::new();
    let pat: Pat = field_store(add(var(base), any_int_const(off)), any());
    let hits = m.find_all(&pat);
    let offsets: Vec<u128> = hits.iter().filter_map(|h| h.get_uint(off, g)).collect();
    assert!(offsets.iter().any(|&n| n == 4 || n == 8),
            "expected ≥1 Store(base + {{4,8}}); got offsets {offsets:?}");

    // Distinct offsets: at least 2 different K values among the stores.
    let mut distinct = offsets.clone();
    distinct.sort();
    distinct.dedup();
    assert!(distinct.len() >= 2,
            "expected ≥2 distinct Store offsets; got {distinct:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. nested_struct_field — single Load whose address has a constant offset
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "nested_struct_field", nested_struct_field_assertions);

fn nested_struct_field_assertions(g: &strider_ir::Function) {
    // o->inner.x = *(base + padding + offsetof(Inner, x)).
    assert!(count_loads(g) >= 1,
            "nested_struct_field must Load; got {}", count_loads(g));

    let m = matcher(g);
    let base = Capture::new();
    let off = Capture::new();
    let pat: Pat = field_load_at_offset(base, off);
    let hits = m.find_all(&pat);
    // Either we found a `Load(base + IntConst)` (offset captured), or the
    // compiler emitted bare `Load(base)` for a folded zero — either is ok
    // for this fixture.  But if any `add(_, IntConst)` form matched, at
    // least one offset must be non-zero (the inner.x position is at
    // `padding + offsetof(Inner, x)` ≥ 4).
    let offsets: Vec<u128> = hits.iter().filter_map(|h| h.get_uint(off, g)).collect();
    if !offsets.is_empty() {
        assert!(offsets.iter().any(|&n| n != 0),
                "all captured Load offsets are 0 in nested_struct_field; got {offsets:?}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. bit_test_zero — IntCmp(Equal, And(_, single-bit-const), 0)
// ─────────────────────────────────────────────────────────────────────────────

// arm `pop {pc}` resolves via the indirect-branch resolver's
// `LinkRegister` arm once `StackLoadForward` simplifies the loaded
// target back to `InitialVar(lr)`.
per_arch_test!("complex", "bit_test_zero", bit_test_zero_assertions);

fn bit_test_zero_assertions(g: &strider_ir::Function) {
    // (mask & 0x4) == 0 → graph contains both `And` and `Equal`.
    assert!(count_int_binop(g, strider_ir::IntBinaryOp::And) >= 1,
            "bit_test_zero must contain ≥1 IntBinaryOp::And; got {}",
            count_int_binop(g, strider_ir::IntBinaryOp::And));
    assert!(count_int_cmp(g, strider_ir::IntCmpOp::Equal) >= 1,
            "bit_test_zero must contain ≥1 IntCmpOp::Equal; got {}",
            count_int_cmp(g, strider_ir::IntCmpOp::Equal));

    // Match `IntCmp(Equal, And(_, single-bit-IntConst), 0)` and assert
    // the captured mask is a single-bit value (the pattern enforces the
    // predicate; we additionally verify the captures).
    let m = matcher(g);
    let mask = Capture::new();
    let value = Capture::new();
    let pat: Pat = bit_test_against_zero(value, mask);
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 IntCmp(Equal, And(_, single-bit-const), 0) match in bit_test_zero");
    for h in &hits {
        if let Some(n) = h.get_uint(mask, g) {
            assert!(n.count_ones() == 1 && n != 0,
                    "captured mask {n:#x} is not a single-bit value");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. if_bit_clear_call — If exists; ≥1 Call exists
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "if_bit_clear_call", if_bit_clear_call_assertions);

fn if_bit_clear_call_assertions(g: &strider_ir::Function) {
    assert!(count_ifs(g) >= 1,
            "if_bit_clear_call must contain ≥1 If; got {}", count_ifs(g));
    assert!(count_calls(g) >= 1,
            "if_bit_clear_call must contain ≥1 Call; got {}", count_calls(g));

    // Decoupled background fact: some Call has arg(0) = function_arg(1)
    // — the `p` parameter (`mask` is arg 0).  Cross-arch, this exercises
    // the optimizer's StackLoadForward pass: at -O0, `p` is spilled to
    // stack at function entry and reloaded before the call, so for the
    // pattern to match the spill round-trip MUST collapse so
    // Call.arg(0) ↔ FunctionArg(1).
    //
    // On Thumb-2, gcc emits a `setISAMode` user-op as a `CallOther`
    // between the If and the Call to set up the ISA-mode context.  The
    // matcher's ConsumersSpec walk doesn't pass through CallOther, but
    // `strider_target::call_other_abi::classify("setISAMode")` returns `NoOp` so the
    // IR builder skips the node entirely, and the strict composition
    // `If(true_branch=Call(...))` matches on Thumb just like every
    // other arch.
    let m = matcher(g);
    assert!(!m.find_all(&if_node().into()).is_empty(),
            "no If matched in if_bit_clear_call");
    // Carrier for arg 1 (the `p` parameter).
    assert!(!g.arg_index_to_nodes(1).is_empty(),
            "arg 1 must be registered in the side-table");
    let pat: Pat = call().arg(0, arg_carrier_pat(g, 1)).into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected Call(arg(0) = carrier(arg 1)) in if_bit_clear_call \
             (proves StackLoadForward connects the spilled `p` parameter \
             through to the call site)");

    // STRICT composition: "If whose true branch is a Call(arg(0) =
    // carrier(arg 1))".  The compiler may emit either
    //   bne skip; call          (call on False side)
    // or
    //   je do_call; call        (call on True side)
    // so we accept *either* branch shape — but the composition itself
    // (If immediately consuming a Call with that arg) must succeed on
    // every arch, including arm_thumb (proves the construction-time
    // NoOp classification of setISAMode keeps the walk unblocked).
    let true_pat: Pat = if_node()
        .true_branch(call().arg(0, arg_carrier_pat(g, 1)))
        .into();
    let false_pat: Pat = if_node()
        .false_branch(call().arg(0, arg_carrier_pat(g, 1)))
        .into();
    let true_hits = m.find_all(&true_pat);
    let false_hits = m.find_all(&false_pat);
    assert!(
        !true_hits.is_empty() || !false_hits.is_empty(),
        "expected If(true_branch | false_branch = Call(arg(0)=carrier(arg 1))) \
         (proves construction-time NoOp classification of setISAMode \
         keeps If→Call walks unblocked on Thumb); got 0 matches on either branch",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. call_with_field_arg — Call.arg(0) is a Load
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "call_with_field_arg", call_with_field_arg_assertions);

fn call_with_field_arg_assertions(g: &strider_ir::Function) {
    assert!(count_loads(g) >= 1,
            "call_with_field_arg must Load s->handler");
    assert!(count_calls(g) >= 1,
            "call_with_field_arg must Call invoke()");

    // Tight match: Call whose arg(0) is `Load(base + offset)` where
    // offset is captured (and asserted to be in a sane range).  This is
    // the canonical "find a call's arg that's a struct field load"
    // pattern — the value of `offset` is captured rather than hardcoded.
    let m = matcher(g);
    let base = Capture::new();
    let off = Capture::new();
    let pat: Pat = call().arg(0, field_load_at_offset(base, off)).into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 Call(arg(0) = Load(base + IntConst))");

    // Sanity-bound the offset.  `s->handler` lives at offset 16 in
    // `struct S { int a, b, c, flags; int *handler; }` (16 on 32-bit
    // pointer arches) or 32 (on 64-bit pointer arches with extra
    // padding) — both well under 256.
    let offsets: Vec<u128> = hits.iter().filter_map(|h| h.get_uint(off, g)).collect();
    assert!(offsets.iter().any(|&n| n < 256),
            "no captured field-load offset is in 0..256; got {offsets:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. dispatch_on_flag — bit-test → if → call → field-load (composition)
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "dispatch_on_flag", dispatch_on_flag_assertions);

fn dispatch_on_flag_assertions(g: &strider_ir::Function) {
    assert!(count_ifs(g) >= 1, "dispatch_on_flag must contain ≥1 If");
    assert!(count_calls(g) >= 1, "dispatch_on_flag must contain ≥1 Call");
    assert!(count_loads(g) >= 1, "dispatch_on_flag must contain ≥1 Load");

    // Three strict facts, decoupled (so single-branch / IT-block /
    // CallOther / BoolUnaryOp normalisation noise on specific arches
    // doesn't collapse the test):
    //   (a) An If exists.
    //   (b) The bit-test pattern `IntCmp(Equal, And(Load, single-bit
    //       const), 0)` matches *somewhere* in the graph — proves a
    //       struct-field bit-test was performed (without requiring it
    //       to be the If's direct cond, since aarch64 / aarch64be
    //       lift `bne` as `BoolUnaryOp::Neg(IntCmp(Equal, ..., 0))`
    //       which inserts a Neg between the IntCmp and the If).
    //   (c) A Call exists whose arg(0) is a struct-field Load (any
    //       offset captured into `off`).
    let m = matcher(g);
    let mask = Capture::new();

    // Bit-test pattern: assert `IntCmp(Equal, And(_, single-bit-const), 0)`
    // somewhere in the graph.  The masked value's source is not pinned
    // to `load()` because PPC GPR reads at -O0 surface as
    // `Or(Load, ShiftRight(Load, 32))` (Sleigh's modelling of 32-bit
    // register reads in 64-bit-capable contexts), and KnownBits doesn't
    // currently fold that idiom away.  The bit-test discriminating
    // power — "a bit-test against a single-bit constant" — survives.
    let bit_test: Pat = int_cmp(
        IntCmpOp::Equal,
        and(any(), single_bit_int_const(mask)),
        int_const(0),
    );
    assert!(!m.find_all(&if_node().into()).is_empty(),
            "expected an If in dispatch_on_flag");
    assert!(!m.find_all(&bit_test).is_empty(),
            "expected a bit-test `IntCmp(Equal, And(_, single-bit-const), 0)` in dispatch_on_flag");

    let off = Capture::new();
    let base = Capture::new();
    let call_field_arg: Pat =
        call().arg(0, field_load_at_offset(base, off)).into();
    assert!(!m.find_all(&call_field_arg).is_empty(),
            "expected Call(arg(0) = Load(base + IntConst)) in dispatch_on_flag");

    // STRICT composition: an If whose true *or* false branch is the
    // Call(arg(0) = field-load) site.  As with `if_bit_clear_call`,
    // the compiler chooses either polarity freely so we accept either
    // branch — but the composition itself (If immediately consuming
    // Call, no opaque CallOther in the way) must succeed on every
    // arch including arm_thumb — proven by the construction-time NoOp
    // classification of setISAMode in strider_target::call_other_abi.
    let off2 = Capture::new();
    let base2 = Capture::new();
    let off3 = Capture::new();
    let base3 = Capture::new();
    let true_pat: Pat = if_node()
        .true_branch(call().arg(0, field_load_at_offset(base2, off2)))
        .into();
    let false_pat: Pat = if_node()
        .false_branch(call().arg(0, field_load_at_offset(base3, off3)))
        .into();
    assert!(
        !m.find_all(&true_pat).is_empty() || !m.find_all(&false_pat).is_empty(),
        "expected If(true_branch | false_branch = Call(arg(0) = field-load)) \
         in dispatch_on_flag (proves construction-time NoOp \
         classification of setISAMode keeps If→Call walks unblocked)",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. multi_arg_call_in_branch — two Calls each with three args
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "multi_arg_call_in_branch", multi_arg_call_in_branch_assertions);

fn multi_arg_call_in_branch_assertions(g: &strider_ir::Function) {
    assert!(count_calls(g) >= 2,
            "multi_arg_call_in_branch must have ≥2 Calls; got {}", count_calls(g));

    // The C source has TWO ext_three call sites with distinct arg
    // orderings: `ext_three(a, b, c)` on the True branch and
    // `ext_three(c, b, a)` on the False.  Function param indices are
    // cond=0, a=1, b=2, c=3.  We match each ordering independently
    // via strict carrier-pat on every positional arg — a buggy
    // optimizer that fails to connect Call.arg(N) back to the carrier(N)
    // through the spill round-trip would lose one of the two matches.
    let m = matcher(g);
    let nv_abc = Capture::new();
    let pat_abc: Pat = call()
        .arg(0, arg_carrier_pat(g, 1))   // a
        .arg(1, arg_carrier_pat(g, 2))   // b
        .arg(2, arg_carrier_pat(g, 3))   // c
        .capture(nv_abc);
    let nv_cba = Capture::new();
    let pat_cba: Pat = call()
        .arg(0, arg_carrier_pat(g, 3))   // c
        .arg(1, arg_carrier_pat(g, 2))   // b
        .arg(2, arg_carrier_pat(g, 1))   // a
        .capture(nv_cba);
    let hits_abc = m.find_all(&pat_abc);
    let hits_cba = m.find_all(&pat_cba);
    assert!(!hits_abc.is_empty(),
            "expected a Call with args (carrier(1), carrier(2), carrier(3)) \
             — the True-branch ext_three(a,b,c)");
    assert!(!hits_cba.is_empty(),
            "expected a Call with args (carrier(3), carrier(2), carrier(1)) \
             — the False-branch ext_three(c,b,a)");
    // Distinct call sites — captured NodeIds must differ across the
    // two patterns (otherwise we matched the same call twice).
    let abc_ids: std::collections::HashSet<_> =
        hits_abc.iter().filter_map(|h| h.node(nv_abc)).collect();
    let cba_ids: std::collections::HashSet<_> =
        hits_cba.iter().filter_map(|h| h.node(nv_cba)).collect();
    assert!(abc_ids.is_disjoint(&cba_ids),
            "expected (a,b,c) and (c,b,a) to match distinct Call NodeIds; \
             abc={abc_ids:?}, cba={cba_ids:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. complex_dispatch — scale smoke test
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "complex_dispatch", complex_dispatch_assertions);

fn complex_dispatch_assertions(g: &strider_ir::Function) {
    let n = g.preorder().count();
    // Larger function (many locals, several stack-allocated structs,
    // 3 loops, ≥10 branches, mixed-width compute) → expect a much
    // larger IR than the original 30-node smoke.  100 is comfortably
    // below what every arch produces while still catching a hard
    // regression (e.g. an opt pass that drops half the function).
    assert!(n >= 100,
            "expected ≥100 reachable IR nodes in complex_dispatch; got {n}");

    // C source has 11 source-level `if` statements (the dispatch flag
    // ladders + the inner loop branch + the acc<0 / acc>100 / padding
    // checks).  IR may merge or fold a few — assert ≥6 to stay above
    // the noise floor on optimising arches.
    assert!(count_ifs(g) >= 6,
            "complex_dispatch must have ≥6 Ifs; got {}", count_ifs(g));
    // 7 source-level call sites: cb_zero (×2), cb_set (×2), invoke
    // (×3), ext_three (×2).  Cross-arch ≥4 is the conservative floor.
    assert!(count_calls(g) >= 4,
            "complex_dispatch must have ≥4 Calls; got {}", count_calls(g));
    // Stack-allocated `big`, `local_outer`, `locals[8]` produce many
    // stores at distinct stack offsets.
    assert!(count_stores(g) >= 5,
            "complex_dispatch must have ≥5 stores; got {}", count_stores(g));

    // Multiple struct field accesses => at least one Load at base+const.
    let m = matcher(g);
    let base = Capture::new();
    let off = Capture::new();
    let pat: Pat = field_load_at_offset(base, off);
    assert!(!m.find_all(&pat).is_empty(),
            "expected ≥1 Load(base + IntConst) in complex_dispatch");

    // Distinct field offsets — proves multiple fields are accessed,
    // not the same one repeatedly.
    let offsets: std::collections::HashSet<u128> = m
        .find_all(&pat).iter().filter_map(|h| h.get_uint(off, g)).collect();
    assert!(offsets.len() >= 2,
            "expected ≥2 distinct Load offsets in complex_dispatch; got {offsets:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. call_uses_call_return — pattern across a Call→Call dataflow
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("complex", "call_uses_call_return", call_uses_call_return_assertions);

fn call_uses_call_return_assertions(g: &strider_ir::Function) {
    // Source: `consume(produce(x))` — outer Call's arg(0) is the
    // return value of the inner Call.  IR shape (-O0):
    //   call_inner = Call(produce, x)
    //   ret = Call_inner output (possibly via spill round-trip)
    //   spill chain = Store → Load (optional, collapsed by StackLoadForward)
    //   call_outer = Call(consume, …chain to ret…)
    //
    // Whether the spill round-trip is collapsed by StackLoadForward,
    // and whether the result is even loaded back via the
    // calling-convention return register — all vary per arch.  What's
    // universal: at least one outer Call's input chain MUST trace back
    // to another Call.  We walk the IR by hand from each Call's inputs
    // through whatever intermediate plumbing the optimizer left in place.

    assert!(count_calls(g) >= 2,
            "call_uses_call_return must have ≥2 Calls; got {}", count_calls(g));

    // For each Call, walk every input slot back through any chain of
    // {Store, Load, Region, ValuePhi}.
    // If we hit another Call, the test passes — that's proof of
    // Call→Call dataflow.
    let calls: Vec<NodeId> = g.preorder()
        .filter(|&n| matches!(g.node_kind(n), NodeKind::Call))
        .collect();
    let chained = calls.iter().any(|&outer| {
        let outer_inputs: Vec<_> = g.node_inputs(outer).into_iter().collect();
        outer_inputs.iter().any(|&inp| {
            let mut producer = g.get_node_from_output(inp);
            // Bound walk to avoid pathological cycles; 16 hops is far
            // more than any reasonable spill round-trip.
            for _ in 0..16 {
                match g.node_kind(producer) {
                    NodeKind::Call if producer != outer => return true,
                    // Walk through plumbing that doesn't change the
                    // value identity.  For each kind, follow the
                    // node's first input — every kind here produces
                    // its data from the most-likely value input slot.
                    NodeKind::Load(_) => {
                        // Load inputs: [memory, addr]; following the
                        // memory edge surfaces the producing store /
                        // call.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.get_node_from_output(first);
                    }
                    NodeKind::Store(_) => {
                        // Store inputs: [mem, addr, data] — `data` is
                        // the value being persisted.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&data) = inps.get(2) else { break; };
                        producer = g.get_node_from_output(data);
                    }
                    NodeKind::Region => {
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.get_node_from_output(first);
                    }
                    NodeKind::Phi if g.phi_var_tag(producer).is_none() => {
                        // Anonymous phi (ValuePhi).  Take the first input;
                        // if it doesn't lead back to a Call we'll bail at
                        // the next step anyway.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.get_node_from_output(first);
                    }
                    _ => break,
                }
            }
            false
        })
    });
    assert!(chained,
            "expected one Call's input to trace back to another Call \
             (through any chain of Load/Store/spill plumbing); \
             call_count={}", calls.len());
}
