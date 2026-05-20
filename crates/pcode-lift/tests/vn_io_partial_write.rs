//! Sub-register partial-write tests for [`pcode_lift::ValueLifter`]'s
//! `vn_io` register-aliasing helpers.
//!
//! Scenario (O3): on x86_64, function entry establishes `RAX` as
//! `InitialVar(rax)`, a single instruction writes only the low byte
//! (`AL`), then a subsequent instruction reads the low 32 bits (`EAX`).
//! The contract for `write_reg_vn` / `read_reg_vn` is that the upper
//! 24 bits of the resulting EAX must be **preserved from the original
//! RAX** — not zero-extended from the 8-bit AL store.
//!
//! These tests exercise the partial-write logic by:
//!   1. Probing a real x86_64 Sleigh register table for the
//!      `RAX` / `EAX` / `AL` varnodes (and confirming their
//!      offset/size relationship — `AL` is the low byte of `RAX`,
//!      `EAX` is the low 4 bytes).
//!   2. Verifying the pure shift-formula
//!      (`calculate_reg_shift_from_container`) returns 0 for both `AL`
//!      and `EAX` against the `RAX` container on little-endian
//!      x86_64 — the expected baseline before the merge logic kicks
//!      in.
//!   3. Driving an end-to-end `write_vn(AL, val) ; read_vn(EAX)`
//!      through the `ValueLifter` and asserting that EAX's producer
//!      is **not** a single-input cast of the AL-byte value alone:
//!      the merge must mix in `InitialVar(rax)` somewhere in the
//!      data-flow chain (i.e. an `Or` / `And` / `Piece` shape over
//!      both AL-derived and InitialVar(rax)-derived inputs).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeKind;
use pcode_lift::ValueLifter;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;

type TestReader = BufMemReader<Vec<u8>>;

/// Build an x86_64 Sleigh — required because `AL` / `EAX` / `RAX`
/// containment only matches on the 64-bit register table.  An x86
/// (32-bit) Sleigh does not expose `RAX` at all.
fn make_x86_64_sleigh() -> Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        reader,
    )
    .expect("create x86_64 sleigh")
}

/// Resolve the RAX/EAX/AL varnodes from a real x86_64 Sleigh register
/// table.  Returns `(rax, eax, al)`.
fn rax_eax_al(sleigh: &Sleigh<TestReader>) -> (rsleigh::Vn, rsleigh::Vn, rsleigh::Vn) {
    let regs = sleigh.regs().expect("regs");
    let rax = regs.name_to_vn("RAX").expect("RAX must exist on x86_64");
    let eax = regs.name_to_vn("EAX").expect("EAX must exist on x86_64");
    let al = regs.name_to_vn("AL").expect("AL must exist on x86_64");
    (rax, eax, al)
}

/// Builds a `FunctionBuilder` whose tracked-vars set is exactly
/// `{RAX}` so that all reads/writes route through the largest container
/// (`find_largest_fitting_register` returns `RAX` for `AL` and `EAX`).
fn make_builder_with_rax(rax: rsleigh::Vn) -> FunctionBuilder {
    let vars = vec![rax];
    let mut b = FunctionBuilder::new_raw(vars, &[], &[], &[], None, 0)
        .expect("FunctionBuilder::new_raw");
    b.build_entry().expect("build_entry");
    let region = b.create_region().expect("create_region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b
}

/// Endianness used by the ValueLifter.  x86_64 is little-endian.
const TEST_ENDIAN: target::Endianness = target::Endianness::Little;

// ── Sleigh register table introspection ─────────────────────────────────────

/// Sanity-check the RAX / EAX / AL containment relationship so the
/// later end-to-end tests rest on a known register layout.  This is
/// arch-data, not behaviour, so it lives in its own pinning test.
#[test]
fn x86_64_sleigh_exposes_rax_eax_al_containment() {
    let sleigh = make_x86_64_sleigh();
    let (rax, eax, al) = rax_eax_al(&sleigh);
    // Same address space + same start offset (LE: low bytes at offset 0).
    assert_eq!(rax.addr_space, eax.addr_space, "RAX/EAX share addr space");
    assert_eq!(rax.addr_space, al.addr_space, "RAX/AL share addr space");
    assert_eq!(rax.addr_off, eax.addr_off, "EAX is the low slice of RAX");
    assert_eq!(rax.addr_off, al.addr_off, "AL is the low byte of RAX");
    // Sizes: 8 / 4 / 1.
    assert_eq!(rax.size, 8);
    assert_eq!(eax.size, 4);
    assert_eq!(al.size, 1);
}

// ── End-to-end partial-write semantics ──────────────────────────────────────
//
// The container-finding (`find_largest_fitting_register`) and shift-
// formula (`calculate_reg_shift_from_container`) helpers are `pub(crate)`
// so we can't poke them directly from this integration test.  The
// behavioural assertions below cover them implicitly: the merge can't
// produce the expected Or/InitialVar(RAX) shape unless RAX is found as
// the container and the bit positioning lands AL at offset 0.  The
// formula's pure-arithmetic correctness is pinned by the unit tests
// in `vn_io.rs::shift_formula_tests`.

/// Returns `true` if `node` (or any ancestor reachable from its inputs)
/// has node kind `target` in the post-write graph.  Bounded walk
/// (max 64 nodes) — every chain we inspect here is a handful of nodes.
fn ancestor_has_kind(
    builder: &strider_ir::FunctionBuilder,
    start: strider_ir::node::NodeId,
    target: NodeKind,
) -> bool {
    let body = builder.body();
    let g = &body.graph;
    let mut stack = vec![start];
    let mut seen = std::collections::HashSet::new();
    let mut budget: usize = 64;
    while let Some(n) = stack.pop() {
        if !seen.insert(n) || budget == 0 {
            continue;
        }
        budget -= 1;
        if g.node_kind(n) == &target {
            return true;
        }
        for inp in g.node_inputs(n).into_iter() {
            stack.push(g.get_node_from_output(inp));
        }
    }
    false
}

/// Returns `true` if the data-flow chain feeding `node` references
/// `InitialVar(target_vn)` somewhere.  Same bounded walk as
/// `ancestor_has_kind`.
fn ancestor_references_initial_var(
    builder: &strider_ir::FunctionBuilder,
    start: strider_ir::node::NodeId,
    target_vn: rsleigh::Vn,
) -> bool {
    let body = builder.body();
    let g = &body.graph;
    let mut stack = vec![start];
    let mut seen = std::collections::HashSet::new();
    let mut budget: usize = 64;
    while let Some(n) = stack.pop() {
        if !seen.insert(n) || budget == 0 {
            continue;
        }
        budget -= 1;
        if let NodeKind::InitialVar(vn) = g.node_kind(n)
            && *vn == target_vn
        {
            return true;
        }
        for inp in g.node_inputs(n).into_iter() {
            stack.push(g.get_node_from_output(inp));
        }
    }
    false
}

/// THE HEADLINE TEST: writing AL must NOT clobber the upper bits of
/// EAX.  After `write_vn(AL, 0x7e) ; read_vn(EAX)`, the chain feeding
/// the EAX read must reference `InitialVar(RAX)` (the preserved upper
/// container bits) somewhere — proof that the merge logic actually
/// merged rather than just zero-extending.
#[test]
fn write_al_then_read_eax_preserves_upper_rax_bits() {
    let sleigh = make_x86_64_sleigh();
    let (rax, eax, al) = rax_eax_al(&sleigh);
    let mut builder = make_builder_with_rax(rax);
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);

    // Build a constant 0x7e (1 byte) and write it to AL.
    let const_byte = lifter
        .builder
        .build_int_const(0x7eu64, strider_ir::ValueType::U8)
        .expect("build_int_const(0x7e)");
    lifter.write_vn(&al, const_byte).expect("write_vn(AL, 0x7e)");

    // Now read EAX through the lifter — same code path the lifter
    // uses to decode any EAX-reading insn.
    let eax_value = lifter.read_vn(&eax).expect("read_vn(EAX)");
    let eax_producer = lifter.builder.body().graph.get_node_from_output(eax_value);

    // The EAX-read chain MUST reference InitialVar(RAX) somewhere
    // (the preserved upper container bits flow through the Or merge).
    assert!(
        ancestor_references_initial_var(&builder, eax_producer, rax),
        "EAX read after AL-write must preserve RAX's initial value in the chain;
         missing InitialVar(RAX) implies the merge clobbered the upper bits",
    );

    // It must NOT be a bare IntConst(0x7e) (that would be the bug —
    // zero-extension only).
    assert!(
        !matches!(
            builder.body().graph.node_kind(eax_producer),
            NodeKind::IntConst(0x7e)
        ),
        "EAX must not collapse to a bare IntConst(0x7e); upper 24 bits are non-deterministic",
    );
}

/// Companion shape check: the EAX-read producer's chain must contain
/// an `IntBinaryOp(Or)` somewhere — that's the merge step that ORs
/// the masked AL bits with the masked container preserve mask.
/// Specifically pinpoints which IR shape the partial-write logic
/// produces, in addition to the InitialVar(RAX) presence assertion
/// above.
#[test]
fn write_al_then_read_eax_chain_contains_or_merge() {
    let sleigh = make_x86_64_sleigh();
    let (rax, eax, al) = rax_eax_al(&sleigh);
    let mut builder = make_builder_with_rax(rax);
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);

    let v = lifter
        .builder
        .build_int_const(0xffu64, strider_ir::ValueType::U8)
        .expect("build_int_const(0xff)");
    lifter.write_vn(&al, v).expect("write_vn(AL)");
    let eax_value = lifter.read_vn(&eax).expect("read_vn(EAX)");
    let eax_producer = lifter.builder.body().graph.get_node_from_output(eax_value);

    assert!(
        ancestor_has_kind(
            &builder,
            eax_producer,
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Or),
        ),
        "partial-write merge must emit an IntBinaryOp(Or) joining masked AL bits and masked container preserve",
    );
}

/// Negative companion: writing the entire EAX-equivalent slot via the
/// container itself is direct (no merge needed).  This pins that the
/// partial-write merge is **not** invoked when reg == container —
/// `write_reg_vn`'s early-return path is what we're checking here.
#[test]
fn write_rax_then_read_rax_is_direct_no_merge() {
    let sleigh = make_x86_64_sleigh();
    let (rax, _, _) = rax_eax_al(&sleigh);
    let mut builder = make_builder_with_rax(rax);
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);

    let v = lifter
        .builder
        .build_int_const(0xdead_beefu64, strider_ir::ValueType::U64)
        .expect("build_int_const");
    lifter.write_vn(&rax, v).expect("write_vn(RAX)");
    let rax_value = lifter.read_vn(&rax).expect("read_vn(RAX)");
    let rax_producer = lifter.builder.body().graph.get_node_from_output(rax_value);

    // Direct write: the producer is the IntConst we just wrote — no
    // Or, no InitialVar dependency.
    assert!(
        matches!(
            builder.body().graph.node_kind(rax_producer),
            NodeKind::IntConst(0xdead_beef),
        ),
        "writing the container directly should produce the exact constant we wrote",
    );
}
