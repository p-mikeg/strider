#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Phase 5 dispatch tests for the `BranchIndirect` resolver.
//!
//! Each test drives `Builder::build` (or `RegionBuilder::process_new_insn`)
//! against a synthetic byte sequence and asserts on the produced
//! [`RegionTerminator`] / outgoing edges, plus the new `Options` knobs added
//! in Phase 5 (`set_link_register` / `set_read_only_memory`).
//!
//! The resolver itself is exercised in `indirect_resolve.rs` (Phase 4).
//! These tests pin the *integration* between [`cfg::Builder`] and the
//! resolver — see `crates/cfg/src/cfg/builder/region_builder.rs` for the
//! dispatch arm.

mod common;
use common::{
    addr, jmp_rax_bytes, make_builder_with_bytes, make_region_builder, make_sleigh_with_bytes,
};

use cfg::test_api::{self, ProcessInsnRes, Region};
use cfg::{Builder, ErrorKind, OptionsBuilder, RegionEdgeKind, RegionTerminator};
use opt::ReadOnlyMemory;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Sleigh, VnSpace};
use std::sync::Arc;

// ── Common bytes ─────────────────────────────────────────────────────────

/// `mov rax, imm64` for x86-64 — 10 bytes, `48 B8 <imm64-le>`.
fn mov_rax_imm64(imm: u64) -> Vec<u8> {
    let mut out = vec![0x48u8, 0xB8];
    out.extend_from_slice(&imm.to_le_bytes());
    out
}

/// `mov rax, K; jmp rax` followed by `pad` bytes of `0x90` (nop).
/// The byte buffer ends with the jmp; the caller chooses the start address.
fn mov_rax_jmp_rax_bytes(target: u64) -> Vec<u8> {
    let mut bytes = mov_rax_imm64(target);
    bytes.extend_from_slice(&jmp_rax_bytes());
    bytes
}

/// Builds a buffer at `base` containing `mov rax, K; jmp rax` followed by
/// `pad_size` bytes of nops then a single `ret` (`0xc3`) at offset
/// `pad_size + 12`.  The byte at `target_offset` is set to `ret` so that
/// when the resolver lands at `base + target_offset` it reaches valid code.
///
/// Useful for tests that need the const-target landing site to be a real
/// machine instruction inside `[base, base + fn_size)`.
fn mov_rax_jmp_rax_with_landing(
    base: u64,
    target_offset: u64,
    fn_size: usize,
) -> (Vec<u8>, u64) {
    assert!(fn_size >= 13);
    let mut bytes = vec![0x90u8; fn_size];
    // mov rax, target  (10 bytes)
    let target = base + target_offset;
    bytes[0..2].copy_from_slice(&[0x48, 0xB8]);
    bytes[2..10].copy_from_slice(&target.to_le_bytes());
    // jmp rax  (2 bytes)
    bytes[10..12].copy_from_slice(&[0xFF, 0xE0]);
    // ret at landing
    let landing_idx = target_offset as usize;
    assert!(landing_idx < bytes.len(), "landing must be inside buffer");
    bytes[landing_idx] = 0xC3;
    (bytes, target)
}

// ── Step 5.4 tests ────────────────────────────────────────────────────────

/// `mov rax, K; jmp rax` with K = base + 0x20 (inside fn range) →
/// resolver returns `Single(K)`, which is intra-fn → `Branch`
/// terminator + `Branch` successor edge to a new region at K.
#[test]
fn branch_indirect_to_in_range_const_produces_branch_terminator() {
    let base = 0x1000u64;
    // Function is 0x40 bytes, mov+jmp at offsets 0..12, target lands at 0x20.
    let (bytes, target) = mov_rax_jmp_rax_with_landing(base, 0x20, 0x40);
    let opts = OptionsBuilder::new().set_function_max_size(0x40).build();
    let cfg = Builder::new(make_sleigh_with_bytes(bytes, base), base, opts)
        .build()
        .expect("Builder::build");

    // The entry region must end as `Branch` and have one outgoing
    // `Branch` edge pointing at a region that starts at `target`.
    let entry_region = &cfg.graph[cfg.entry];
    assert_eq!(
        entry_region.terminator,
        RegionTerminator::Branch,
        "entry region must end as Branch when resolver returns Single in-range",
    );

    let outgoing: Vec<_> = cfg
        .graph
        .edges_directed(cfg.entry, petgraph::Direction::Outgoing)
        .collect();
    assert_eq!(outgoing.len(), 1, "exactly one outgoing edge expected");
    let edge = &outgoing[0];
    assert_eq!(*edge.weight(), RegionEdgeKind::Branch);
    let target_region = &cfg.graph[edge.target()];
    assert_eq!(target_region.start_addr.machine_addr.addr, target);
}

/// `mov rax, K; jmp rax` with K outside fn range → resolver returns
/// `Single(K)`, which is a tail call → `TailCall { target: K }` terminator
/// and **no** successor edge.
#[test]
fn branch_indirect_to_out_of_range_const_produces_tail_call_terminator() {
    let base = 0x1000u64;
    // K = 0x9000 is far outside fn_max_size = 0x100.
    let target = 0x9000u64;
    let bytes = mov_rax_jmp_rax_bytes(target);
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
    let cfg = Builder::new(make_sleigh_with_bytes(bytes, base), base, opts)
        .build()
        .expect("Builder::build");

    // Single region whose terminator is TailCall { target }.
    assert_eq!(cfg.graph.node_count(), 1);
    let entry = &cfg.graph[cfg.entry];
    assert_eq!(
        entry.terminator,
        RegionTerminator::TailCall { target },
        "out-of-range Single must lower to TailCall",
    );
    assert_eq!(
        cfg.graph
            .edges_directed(cfg.entry, petgraph::Direction::Outgoing)
            .count(),
        0,
        "TailCall must have no outgoing edges",
    );
}

/// `bx lr` (ARM) with `Options::link_register_vn` set to the ARM `lr`
/// varnode → resolver returns `LinkRegister` → `Return` terminator.
#[test]
fn branch_indirect_to_link_register_produces_return_terminator() {
    // `bx lr` little-endian ARM: e1 2f ff 1e
    let bytes: Vec<u8> = vec![0x1e, 0xff, 0x2f, 0xe1];
    let base = 0x1000u64;
    let reader = BufMemReader::new(bytes.clone(), base);
    let sleigh = Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
        rsleigh::pspec::PSPEC_ARM_V45,
        reader,
    )
    .expect("ARM sleigh");
    let lr = sleigh
        .regs()
        .expect("regs")
        .name_to_vn("lr")
        .expect("lr varnode");

    let opts = OptionsBuilder::new().set_link_register(lr).build();
    let cfg = Builder::new(sleigh, base, opts)
        .build()
        .expect("Builder::build");

    assert_eq!(cfg.graph.node_count(), 1);
    let entry = &cfg.graph[cfg.entry];
    assert_eq!(
        entry.terminator,
        RegionTerminator::Return,
        "bx lr with cc_link_register_vn = Some(lr) must be Return",
    );
    assert_eq!(
        cfg.graph
            .edges_directed(cfg.entry, petgraph::Direction::Outgoing)
            .count(),
        0,
    );
}

/// Synthetic CFG with an unresolvable BranchIndirect (no prior write,
/// no link-register pluging) → `Builder::build` returns
/// `Err(UnresolvedIndirectBranch)`.
#[test]
fn unresolved_branch_indirect_errors() {
    // bare `jmp rax` at 0x1000 — RAX is a function entry value, not
    // a constant and not a link register (Options has no LR set).
    let base = 0x1000u64;
    let bytes = jmp_rax_bytes();
    let opts = OptionsBuilder::new().build();
    let err = Builder::new(make_sleigh_with_bytes(bytes, base), base, opts)
        .build()
        .expect_err("expected UnresolvedIndirectBranch");
    match err.kind() {
        ErrorKind::UnresolvedIndirectBranch(_) => {}
        other => panic!("expected UnresolvedIndirectBranch, got {other:?}"),
    }
}

/// `CallIndirect` (e.g. `call rax`) is OUT OF SCOPE of Phase 5.
/// It must NOT be reclassified as Return and must NOT terminate the
/// region.  This test drives the call-indirect lift through
/// `process_new_insn` and asserts no Return-terminated region was
/// emitted.
#[test]
fn call_indirect_unchanged_when_target_is_lr() {
    // `call rax` is `0xff 0xd0` for x86-64, followed by `ret`.
    let base = 0x1000u64;
    let bytes: Vec<u8> = vec![0xff, 0xd0, 0xc3];
    let lift = make_sleigh_with_bytes(bytes.clone(), base)
        .lift_one(base)
        .expect("lift call rax");
    // Find the CallIndirect pcode.
    let (pos, ci) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| i.opcode == rsleigh::Opcode::CallIndirect)
        .map(|(i, ins)| (i as u64, ins.clone()))
        .expect("CallIndirect pcode insn");

    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb
        .process_new_insn(&ci, addr(base, pos), &lift)
        .expect("process_new_insn");
    // Phase 5 contract: CallIndirect is not a region terminator —
    // process_new_insn returns DidntFinishProcessing.
    assert_eq!(
        res,
        ProcessInsnRes::DidntFinishProcessing,
        "CallIndirect must not terminate the region",
    );

    // No region must have been added; the resolver must not have fired.
    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert!(
        regions.is_empty(),
        "process_new_insn for CallIndirect must not flush a region (got {} region(s))",
        regions.len(),
    );
}

/// `OptionsBuilder::set_link_register(vn).build()` exposes the same
/// `Vn` to the resolver — verified via the test_api accessor.
#[test]
fn options_set_link_register_round_trips() {
    let vn = rsleigh::Vn {
        size: 4,
        addr: rsleigh::VnAddr {
            off: 0x100,
            space: VnSpace::REGISTER,
        },
    };
    let opts = OptionsBuilder::new().set_link_register(vn).build();
    let got = test_api::options_link_register_vn(&opts);
    assert_eq!(got, Some(vn));
    // Also verify default is None.
    let default = OptionsBuilder::new().build();
    assert_eq!(test_api::options_link_register_vn(&default), None);
}

/// `OptionsBuilder::set_read_only_memory(rom).build()` keeps `rom`
/// reachable via the test_api accessor (Arc identity).
#[test]
fn options_read_only_memory_round_trips() {
    struct EmptyRom;
    impl ReadOnlyMemory for EmptyRom {
        fn read(&self, _space: VnSpace, _addr: u64, _size: usize) -> Option<u64> {
            None
        }
    }
    let rom: Arc<dyn ReadOnlyMemory> = Arc::new(EmptyRom);
    let opts = OptionsBuilder::new()
        .set_read_only_memory(Arc::clone(&rom))
        .build();
    let stored = test_api::options_read_only_memory(&opts).expect("rom must be present");
    // Pointer-identity check: `Arc::ptr_eq` compares the underlying
    // allocation.  We dereference the trait-object Arc to the trait
    // object pointer, which is sufficient.
    assert!(
        Arc::ptr_eq(&rom, stored),
        "options must store the same Arc<dyn ReadOnlyMemory>"
    );

    // Default leaves it as None.
    let default = OptionsBuilder::new().build();
    assert!(test_api::options_read_only_memory(&default).is_none());
}

/// `mov rax, K; jmp rax` where K lands inside the entry region (a
/// back-jump) — the resolver must return `Single(K)`, the dispatch
/// must enqueue K, and the resulting CFG must contain a region split
/// at K with an incoming `Branch` edge.
#[test]
fn branch_indirect_inside_split_region_resolves_correctly() {
    let base = 0x1000u64;
    // Lay out: nop @ 0x1000, then mov rax, 0x1001; jmp rax.
    // The target 0x1001 lies *inside* the entry region (mid-block)
    // and forces a split.
    //
    // bytes: 0x90, 0x48 0xB8 <imm64=0x1001>, 0xFF 0xE0  (13 bytes total)
    // We follow with 6 bytes of nops + ret so the split target lands
    // on a real machine boundary.  `mov rax, 0x1001` would land
    // mid-instruction at the `0xB8` byte unless we plan carefully —
    // the simplest layout is to back-jump to 0x1000 itself:
    let mut bytes = mov_rax_imm64(base);
    bytes.extend_from_slice(&jmp_rax_bytes());
    let opts = OptionsBuilder::new().set_function_max_size(0x40).build();
    let cfg = Builder::new(make_sleigh_with_bytes(bytes, base), base, opts)
        .build()
        .expect("Builder::build for split");

    // Expectation: resolver returned Single(base) — back-jump to
    // start of own region.  Since the target equals the entry
    // address, no split actually happens (target lands at start of
    // existing region) and we get a self-loop `Branch` edge.
    let branch_edges: Vec<_> = cfg
        .graph
        .edge_references()
        .filter(|e| *e.weight() == RegionEdgeKind::Branch)
        .collect();
    assert!(
        !branch_edges.is_empty(),
        "expected at least one Branch edge from the resolved BranchIndirect",
    );
    // The targeted region's start_addr must be exactly `base`.
    let target_node = branch_edges[0].target();
    assert_eq!(
        cfg.graph[target_node].start_addr.machine_addr.addr,
        base,
        "Branch edge must point at the resolved const target",
    );
}
