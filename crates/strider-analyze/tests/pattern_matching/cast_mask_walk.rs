//! Comprehensive walk-through behaviour tests for the matcher's
//! `CastMask` setting.
//!
//! Each test builds a tiny IR fixture of shape `Add(<wrapped>, IntConst)`
//! where `<wrapped>` is a chain of cast nodes around an `InitialVar`, and
//! runs the pattern `add(initial_var(vn), int_const(_))` under different
//! `CastMask` settings.

use strider_analyze::opt::{Optimizer, RedundantPhis};
use strider_analyze::pattern::{
    CastMask, Capture, Matcher, Pat, add, any_int_const, initial_var_for,
};
use strider_ir::node::{NodeOutputId, NodeOutputType};
use strider_ir::{Function, ExtendOp, FunctionBuilder, IntBinaryOp};

use strider_ir_test_utils::RegisterSet;

/// Collapses single-predecessor `Phi(Some(_))` / `MemPhi` / `Region`
/// nodes the FunctionBuilder inserts at the entry region for every tracked
/// variable.  Without this pass, `read_variable(vn)` returns the
/// `Phi(Some(vn))` output (with `InitialVar(vn)` as its sole input),
/// which sits between the matcher's input descent and the InitialVar.
fn collapse_phis(g: &mut Function) {
    let entry = g.entry().expect("entry");
    RedundantPhis.optimize(g.graph_mut(), entry).expect("RedundantPhis");
}

// ── Fixture builder ─────────────────────────────────────────────────────────

/// The varnode the InitialVar reads from in every fixture below.
fn x_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 8,
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// Builds a small function whose return is
/// `Add(wrap(read(vn)), IntConst(7) : ty) : ty` after collapsing the
/// region's single-predecessor `Phi(Some(vn))`.
fn build_add_wrapped<F>(
    vn: rsleigh::Vn,
    ty: NodeOutputType,
    wrap: F,
) -> Function
where
    F: FnOnce(&mut FunctionBuilder, NodeOutputId) -> NodeOutputId,
{
    let mut fb = RegisterSet::new()
        .tracked(vn)
        .build_fn_single_region()
        .expect("build_fn_single_region");

    let x = fb.read_variable(&vn).unwrap();
    let wrapped = wrap(&mut fb, x);
    let c = fb.build_int_const(7u64, ty).unwrap();
    let total = fb
        .build_int_binary_operation(wrapped, c, IntBinaryOp::Add, ty)
        .unwrap();
    fb.build_return(Some(total), &[]).unwrap();
    let mut g = fb.build().unwrap();
    collapse_phis(&mut g);
    g
}

/// Pattern: `add(initial_var(x_vn), int_const(_))`.
fn pat() -> Pat {
    add(initial_var_for(x_vn()), any_int_const(Capture::new())).into()
}

/// Run the pattern under `mask` and return the match count.
fn count(g: &Function, mask: CastMask) -> usize {
    Matcher::try_new(g).unwrap().ignore_casts_mask(mask).find_all(&pat()).len()
}

// ── Add(Truncate(InitialVar), IntConst) ─────────────────────────────────────

/// `Add(Truncate(InitialVar : U64) : U32, IntConst(7) : U32) : U32`.
fn fixture_truncate_then_add() -> Function {
    build_add_wrapped(x_vn(), NodeOutputType::U32, |fb, x| {
        fb.truncate_if_needed(x, NodeOutputType::U32).unwrap()
    })
}

#[test]
fn truncate_initial_var_empty_mask_zero_matches() {
    let g = fixture_truncate_then_add();
    assert_eq!(count(&g, CastMask::empty()), 0);
}

#[test]
fn truncate_initial_var_truncate_mask_one_match() {
    let g = fixture_truncate_then_add();
    assert_eq!(count(&g, CastMask::TRUNCATE), 1);
}

#[test]
fn truncate_initial_var_extend_mask_zero_matches() {
    let g = fixture_truncate_then_add();
    assert_eq!(count(&g, CastMask::EXTEND), 0);
}

#[test]
fn truncate_initial_var_all_mask_one_match() {
    let g = fixture_truncate_then_add();
    assert_eq!(count(&g, CastMask::all()), 1);
}

// ── Add(ZeroExtend(InitialVar), IntConst) ───────────────────────────────────

/// U32 register varnode used for the extend fixtures.
fn x_u32_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 4,
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// `Add(ZeroExt(InitialVar : U32) : U64, IntConst(7) : U64) : U64`.
fn fixture_zext_then_add() -> Function {
    build_add_wrapped(x_u32_vn(), NodeOutputType::U64, |fb, x| {
        fb.extend_if_needed(x, NodeOutputType::U64, ExtendOp::ZeroExtend)
            .unwrap()
    })
}

/// Pattern asking for the U32 InitialVar at offset 0x40.
fn pat_u32_initial_var() -> Pat {
    add(initial_var_for(x_u32_vn()), any_int_const(Capture::new())).into()
}

fn count_u32(g: &Function, mask: CastMask) -> usize {
    Matcher::try_new(g).unwrap()
        .ignore_casts_mask(mask)
        .find_all(&pat_u32_initial_var())
        .len()
}

#[test]
fn zext_initial_var_zero_extend_mask_one_match() {
    let g = fixture_zext_then_add();
    assert_eq!(count_u32(&g, CastMask::ZERO_EXTEND), 1);
}

#[test]
fn zext_initial_var_sign_extend_mask_zero_matches() {
    let g = fixture_zext_then_add();
    assert_eq!(count_u32(&g, CastMask::SIGN_EXTEND), 0);
}

#[test]
fn zext_initial_var_extend_mask_one_match() {
    let g = fixture_zext_then_add();
    assert_eq!(count_u32(&g, CastMask::EXTEND), 1);
}

// ── Add(SignExtend(InitialVar), IntConst) ───────────────────────────────────

/// `Add(SignExt(InitialVar : U32) : U64, IntConst(7) : U64) : U64`.
fn fixture_sext_then_add() -> Function {
    build_add_wrapped(x_u32_vn(), NodeOutputType::U64, |fb, x| {
        fb.extend_if_needed(x, NodeOutputType::U64, ExtendOp::SignExtend)
            .unwrap()
    })
}

#[test]
fn sext_initial_var_sign_extend_mask_one_match() {
    let g = fixture_sext_then_add();
    assert_eq!(count_u32(&g, CastMask::SIGN_EXTEND), 1);
}

#[test]
fn sext_initial_var_zero_extend_mask_zero_matches() {
    let g = fixture_sext_then_add();
    assert_eq!(count_u32(&g, CastMask::ZERO_EXTEND), 0);
}

#[test]
fn sext_initial_var_extend_mask_one_match() {
    let g = fixture_sext_then_add();
    assert_eq!(count_u32(&g, CastMask::EXTEND), 1);
}

// ── Add(Truncate(ZeroExtend(InitialVar)), IntConst) — chained casts ─────────

fn x_u16_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 2,
        addr_off: 0x48,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// `Add(Truncate(ZeroExt(InitialVar : U16) : U64) : U32, IntConst(7) : U32) : U32`.
fn fixture_truncate_of_zext_then_add() -> Function {
    build_add_wrapped(x_u16_vn(), NodeOutputType::U32, |fb, x| {
        let widened = fb
            .extend_if_needed(x, NodeOutputType::U64, ExtendOp::ZeroExtend)
            .unwrap();
        fb.truncate_if_needed(widened, NodeOutputType::U32).unwrap()
    })
}

fn pat_u16_initial_var() -> Pat {
    add(initial_var_for(x_u16_vn()), any_int_const(Capture::new())).into()
}

fn count_u16(g: &Function, mask: CastMask) -> usize {
    Matcher::try_new(g).unwrap()
        .ignore_casts_mask(mask)
        .find_all(&pat_u16_initial_var())
        .len()
}

#[test]
fn truncate_of_zext_truncate_only_mask_zero_matches() {
    let g = fixture_truncate_of_zext_then_add();
    assert_eq!(count_u16(&g, CastMask::TRUNCATE), 0);
}

#[test]
fn truncate_of_zext_zero_extend_only_mask_zero_matches() {
    let g = fixture_truncate_of_zext_then_add();
    assert_eq!(count_u16(&g, CastMask::ZERO_EXTEND), 0);
}

#[test]
fn truncate_of_zext_truncate_or_zero_extend_mask_one_match() {
    let g = fixture_truncate_of_zext_then_add();
    assert_eq!(
        count_u16(&g, CastMask::TRUNCATE | CastMask::ZERO_EXTEND),
        1
    );
}

// ── Stress test: deep cast chain ─────────────────────────────────────────

/// Builds `Add(<truncate-extend tower>(InitialVar : U64), IntConst(7) : U64)`
/// with `levels` round-trips of `Truncate(U64 → U32)` → `Extend(U32 → U64)`.
fn fixture_deep_cast_chain(levels: usize) -> Function {
    build_add_wrapped(x_vn(), NodeOutputType::U64, |fb, x| {
        let mut current = x;
        for _ in 0..levels {
            current = fb.truncate_if_needed(current, NodeOutputType::U32).unwrap();
            current = fb
                .extend_if_needed(current, NodeOutputType::U64, ExtendOp::ZeroExtend)
                .unwrap();
        }
        current
    })
}

#[test]
fn deep_cast_chain_walks_through_all_levels() {
    let g = fixture_deep_cast_chain(500);
    let count = Matcher::try_new(&g).unwrap()
        .ignore_casts_mask(CastMask::TRUNCATE | CastMask::ZERO_EXTEND)
        .find_all(&pat())
        .len();
    assert_eq!(count, 1, "iterative cast walk-through must unwrap 500-deep cast tower");
}

#[test]
fn deep_cast_chain_with_partial_mask_does_not_match() {
    let g = fixture_deep_cast_chain(500);
    let count = Matcher::try_new(&g).unwrap()
        .ignore_casts_mask(CastMask::TRUNCATE)
        .find_all(&pat())
        .len();
    assert_eq!(count, 0);
}
