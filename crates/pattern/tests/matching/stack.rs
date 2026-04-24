//! `StackStore` / `StackStorePhi` pattern matching.
//!
//! Graphs in these tests run through `opt::StackStoreDetect` so the raw
//! `Store(sp + K)` nodes are lowered to dedicated `StackStore { offset: K }`
//! (and `StackStorePhi` at join points).

use ir::node::NodeOutputType;
use pattern::*;

use super::support::{Tb, assertions as a, shapes, sp_vn};

/// Graph where `*(sp - 4) = 0xAB`, then load it back.  After
/// `StackStoreDetect` the Store becomes a `StackStore { offset: -4 }`.
fn stack_store_minus_4(data: u64) -> ir::BuiltFunctionGraph {
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
fn stack_store_phi_4_and_8() -> ir::BuiltFunctionGraph {
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
    a::matches(&g, stack_store().data(int_const(0xAB)), 1);
    a::none(&g, stack_store().data(int_const(0x42)));
}

#[test]
fn stack_store_offset_and_data_together() {
    let g = stack_store_minus_4(0xAB);
    a::matches(&g, stack_store().offset(-4).data(int_const(0xAB)), 1);
    // Right offset, wrong data.
    a::none(&g, stack_store().offset(-4).data(int_const(0)));
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
    a::matches(&g, stack_store_phi().data(int_const(0xCC)), 1);
    a::none(&g, stack_store_phi().data(int_const(0)));
}
