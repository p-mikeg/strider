//! Comprehensive walk-through behaviour tests for [`pattern::CastMask`].
//!
//! Each test builds a tiny IR fixture of shape `Add(<wrapped>, IntConst)`
//! where `<wrapped>` is a chain of cast nodes around an `InitialVar`, and
//! runs the pattern `add(initial_var(vn), int_const(_))` under different
//! `CastMask` settings.  The match count must agree with the spec table.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, FunctionBuilder};
use opt::{Optimizer, RedundantPhis};
use pattern::{CastMask, Matcher, Pat, Capture, add, any_int_const, initial_var_for};

/// Collapses single-predecessor `ControlPhi` / `MemPhi` / `ControlState`
/// nodes the FunctionBuilder inserts at the entry region for every
/// tracked variable.  Without this pass, `read_variable(vn)` returns the
/// `ControlPhi(vn)` output (with `InitialVar(vn)` as its sole input),
/// which sits between the matcher's input descent and the InitialVar
/// the patterns are looking for.
fn collapse_phis(g: &mut BuiltFunctionGraph) {
    RedundantPhis.optimize(&mut g.graph, g.entry).expect("RedundantPhis");
}

// ── Fixture builder ─────────────────────────────────────────────────────────

/// The varnode the InitialVar reads from in every fixture below.
fn x_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr {
            off: 0x40,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// Builds a small function whose return is
/// `Add(wrap(read(vn)), IntConst(7) : ty) : ty` after collapsing the
/// region's single-predecessor `ControlPhi` for `vn`.  After that
/// collapse, the matcher sees `Add(wrap(InitialVar(vn)), IntConst(7))`.
///
/// `wrap` may emit any chain of nodes around the InitialVar read; `ty`
/// is the width of both the const and the Add.
fn build_add_wrapped<F>(
    vn: rsleigh::Vn,
    ty: NodeOutputType,
    wrap: F,
) -> BuiltFunctionGraph
where
    F: FnOnce(&mut FunctionBuilder, NodeOutputId) -> NodeOutputId,
{
    let mut fb = FunctionBuilder::new_raw(vec![vn], &[], &[], &[], None, 0).unwrap();
    let region = fb.create_region().unwrap();
    fb.set_entry_region(region).unwrap();
    fb.set_region(region);

    let x = fb.read_variable(&vn).unwrap();
    let wrapped = wrap(&mut fb, x);
    let c = fb.build_int_const(7u64, ty);
    let total = fb
        .build_int_binary_operation(wrapped, c, ir::IntBinaryOp::Add, ty)
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
fn count(g: &BuiltFunctionGraph, mask: CastMask) -> usize {
    Matcher::new(g).ignore_casts_mask(mask).find_all(&pat()).len()
}

// ── Add(Truncate(InitialVar), IntConst) ─────────────────────────────────────

/// `Add(Truncate(InitialVar : U64) : U32, IntConst(7) : U32) : U32`.
fn fixture_truncate_then_add() -> BuiltFunctionGraph {
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
        addr: rsleigh::VnAddr {
            off: 0x40,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// `Add(ZeroExt(InitialVar : U32) : U64, IntConst(7) : U64) : U64`.
fn fixture_zext_then_add() -> BuiltFunctionGraph {
    build_add_wrapped(x_u32_vn(), NodeOutputType::U64, |fb, x| {
        fb.extend_if_needed(x, NodeOutputType::U64, ExtendOp::ZeroExtend)
            .unwrap()
    })
}

/// Pattern asking for the U32 InitialVar at offset 0x40.
fn pat_u32_initial_var() -> Pat {
    add(initial_var_for(x_u32_vn()), any_int_const(Capture::new())).into()
}

fn count_u32(g: &BuiltFunctionGraph, mask: CastMask) -> usize {
    Matcher::new(g)
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
fn fixture_sext_then_add() -> BuiltFunctionGraph {
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
//
// InitialVar is at byte 0x48 (size 2 bytes, U16).  Zero-extend to U64,
// then truncate to U32, then add U32 IntConst.  The pattern asks for the
// U16 InitialVar through both casts.

fn x_u16_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 2,
        addr: rsleigh::VnAddr {
            off: 0x48,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// `Add(Truncate(ZeroExt(InitialVar : U16) : U64) : U32, IntConst(7) : U32) : U32`.
fn fixture_truncate_of_zext_then_add() -> BuiltFunctionGraph {
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

fn count_u16(g: &BuiltFunctionGraph, mask: CastMask) -> usize {
    Matcher::new(g)
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
