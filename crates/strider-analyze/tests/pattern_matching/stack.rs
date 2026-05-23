//! `StackStore` / `StackStorePhi` pattern matching.
//!
//! Graphs in these tests run through `opt::StackStoreDetect` so the raw
//! `Store(sp + K)` nodes are lowered to dedicated `StackStore { offset: K }`
//! (and `StackStorePhi` at join points).

use strider_analyze::pattern::*;
use strider_ir::node::NodeOutputType;

use super::support::{Tb, assertions as a, shapes, sp_vn};

/// Graph where `*(sp - 4) = 0xAB`, then load it back.  After
/// `StackStoreDetect` the Store becomes a `StackStore { offset: -4 }`.
fn stack_store_minus_4(data: u64) -> strider_ir::Graph {
    let sp = sp_vn();
    let mut t = Tb::raw(vec![sp], &[], &[sp], &[], None, 0);
    let sp_val = t.read_var(&sp);
    let four = t.u64(4);
    let addr = t.sub(sp_val, four);
    let d = t.u64(data);
    t.store_ram(addr, d);
    let loaded = t.load_ram(addr, NodeOutputType::U64);
    let mut g = t.ret_val(loaded);
    shapes::run_stack_store_pipeline(&mut g, sp);
    g
}

/// Graph where two branches adjust SP by different offsets (-4, -8) and
/// then a single store goes through the joined SP.  After StackStoreDetect,
/// the merged store is a `StackStorePhi` with per-predecessor offsets
/// `[-4, -8]`.
fn stack_store_phi_4_and_8() -> strider_ir::Graph {
    let sp = sp_vn();
    let mut t = Tb::bare(vec![sp], &[], &[sp], &[], None, 0);
    let entry = t.region();
    let a_r = t.region();
    let b_r = t.region();
    let merge = t.region();
    t.set_entry(entry);

    t.enter(entry);
    let c = t.boolean(true);
    t.build_if(c, a_r, b_r);

    t.enter(a_r);
    let sp_a = t.read_var(&sp);
    let four = t.u64(4);
    let sp_a = t.sub(sp_a, four);
    t.write_var(&sp, sp_a);
    t.branch(merge);

    t.enter(b_r);
    let sp_b = t.read_var(&sp);
    let eight = t.u64(8);
    let sp_b = t.sub(sp_b, eight);
    t.write_var(&sp, sp_b);
    t.branch(merge);

    t.enter(merge);
    let sp_m = t.read_var(&sp);
    let data = t.u64(0xCC);
    t.store_ram(sp_m, data);
    let loaded = t.load_ram(sp_m, NodeOutputType::U64);
    let mut g = t.ret_val(loaded);
    shapes::run_stack_store_pipeline(&mut g, sp);
    g
}

// ── StackStore ───────────────────────────────────────────────────────────────

#[test]
fn stack_store_unconstrained_matches() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store(), 1);
}

#[test]
fn stack_store_offset_matches() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().offset(-4), 1);
    a::none(&g, stack_store().offset(0));
    a::none(&g, stack_store().offset(-8));
}

#[test]
fn stack_store_data_matches() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().data(int_const(0xABu64)), 1);
    a::none(&g, stack_store().data(int_const(0x42u64)));
}

#[test]
fn stack_store_offset_and_data_together() {
    let g = stack_store_minus_4(0xAB);
    a::matches(
        &g,
        stack_store().offset(-4).data(int_const(0xABu64)),
        1,
    );
    // Right offset, wrong data.
    a::none(&g, stack_store().offset(-4).data(int_const(0u64)));
}

#[test]
fn stack_store_space_matches() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().space(rsleigh::VnSpace::RAM), 1);
    a::none(&g, stack_store().space(rsleigh::VnSpace::UNIQUE));
}

// Regular `store()` must not find the lowered StackStore, and vice-versa.
#[test]
fn regular_store_pattern_does_not_match_stack_store() {
    let g = stack_store_minus_4(0xAB);
    a::none(&g, store());
}

// ── StackStorePhi ────────────────────────────────────────────────────────────

#[test]
fn stack_store_phi_unconstrained_matches() {
    let g = stack_store_phi_4_and_8();
    a::matches(&g, stack_store_phi(), 1);
}

#[test]
fn stack_store_phi_exact_offsets_match_order_independently() {
    let g = stack_store_phi_4_and_8();
    a::matches(&g, stack_store_phi().offsets([-4, -8]), 1);
    a::matches(&g, stack_store_phi().offsets([-8, -4]), 1);
}

#[test]
fn stack_store_phi_wrong_offsets_rejects() {
    let g = stack_store_phi_4_and_8();
    a::none(&g, stack_store_phi().offsets([0, -4]));
    a::none(&g, stack_store_phi().offsets([-4]));
    a::none(&g, stack_store_phi().offsets([-4, -8, -12]));
}

#[test]
fn stack_store_phi_data_matches() {
    let g = stack_store_phi_4_and_8();
    a::matches(&g, stack_store_phi().data(int_const(0xCCu64)), 1);
    a::none(&g, stack_store_phi().data(int_const(0u64)));
}

// ── Match::stack_offset / stack_phi_offsets accessors ─────────────────────────

/// `stack_store().capture(c)` binds `c` to the `StackStore` node;
/// `Match::stack_offset(c)` reads the offset out of the IR node kind
/// without re-walking the graph.
#[test]
fn match_stack_offset_returns_offset_for_captured_stack_store() {
    let g = stack_store_minus_4(0xAB);
    let c = Capture::new();
    let m = a::unique(&g, stack_store().capture(c));
    assert_eq!(m.stack_offset(c, &g), Some(-4));
}

/// `stack_offset` returns `None` for an unbound capture.
#[test]
fn match_stack_offset_unbound_capture_returns_none() {
    let g = stack_store_minus_4(0xAB);
    let bound = Capture::new();
    let unbound = Capture::new();
    let m = a::unique(&g, stack_store().capture(bound));
    assert_eq!(m.stack_offset(unbound, &g), None);
}

/// `stack_offset` returns `None` if the capture binds to a node that
/// is not a `StackStore` (e.g. an `IntConst` carrying the stored
/// value 0xAB).
#[test]
fn match_stack_offset_wrong_node_kind_returns_none() {
    let g = stack_store_minus_4(0xAB);
    let c = Capture::new();
    let m = a::unique(&g, int_const(0xABu64).capture(c));
    assert_eq!(m.stack_offset(c, &g), None);
}

/// `Match::stack_phi_offsets(c)` returns the per-predecessor offset
/// list for a captured `StackStorePhi`.  The slice is read from the
/// IR side table; ordering matches predecessor order.
#[test]
fn match_stack_phi_offsets_returns_side_table_slice() {
    let g = stack_store_phi_4_and_8();
    let c = Capture::new();
    let m = a::unique(&g, stack_store_phi().capture(c));
    let offsets = m.stack_phi_offsets(c, &g).expect("phi offsets");
    let mut sorted: Vec<i64> = offsets.to_vec();
    sorted.sort();
    assert_eq!(sorted, vec![-8, -4]);
}

/// `stack_phi_offsets` returns `None` for a non-`StackStorePhi` capture.
#[test]
fn match_stack_phi_offsets_wrong_node_kind_returns_none() {
    let g = stack_store_minus_4(0xAB);
    let c = Capture::new();
    let m = a::unique(&g, stack_store().capture(c));
    assert_eq!(m.stack_phi_offsets(c, &g), None);
}

// ── StackStorePat::offset_any (set-membership) ────────────────────────────────

/// `offset_any([k1, k2, …])` matches when the StackStore's offset is
/// in the set.
#[test]
fn stack_store_offset_any_matches_when_in_set() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().offset_any([-4i64, -8, 0]), 1);
}

/// `offset_any` rejects when the offset is not in the set.
#[test]
fn stack_store_offset_any_rejects_when_not_in_set() {
    let g = stack_store_minus_4(0xAB);
    a::none(&g, stack_store().offset_any([0i64, -8, 16]));
}

/// Empty set matches nothing — vacuously false (mirrors
/// `int_const_any_of`'s contract).
#[test]
fn stack_store_offset_any_empty_set_matches_nothing() {
    let g = stack_store_minus_4(0xAB);
    a::none(&g, stack_store().offset_any(std::iter::empty::<i64>()));
}

/// `.offset(K).offset_any([K])` is the redundant intersection — the
/// AND of two equal constraints — and matches the same node as
/// `.offset(K)` alone.
#[test]
fn stack_store_offset_and_offset_any_redundant_pair_matches() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().offset(-4).offset_any([-4i64]), 1);
}

/// `.offset(K1).offset_any([K2])` with `K1 ∉ {K2}` is a contradictory
/// AND-combination and must reject every candidate.  Pins the
/// docstring's "AND-combined with `.offset(K)`" claim.
#[test]
fn stack_store_offset_and_offset_any_contradiction_matches_nothing() {
    let g = stack_store_minus_4(0xAB);
    a::none(&g, stack_store().offset(-4).offset_any([-8i64, 0]));
}
