use crate::pipeline::Optimizer;
use crate::test_support::make_fn;
use super::*;
use anyhow::anyhow;
use crate::error::Result;
use ir::node::{NodeKind, NodeOutputType};

// ── tiny ROM fixture ──────────────────────────────────────────────────────────

struct TestRom;

impl ReadOnlyMemory for TestRom {
    fn read(&self, _space: rsleigh::VnSpace, addr: u64, _size: usize) -> Option<u64> {
        match addr {
            0x1000 => Some(42),
            0x2000 => Some(0xFF),
            _ => None,
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    let val = fg.node_inputs(ret)[2];
    Ok(*fg.kind_of_output(val))
}

// ── original tests ────────────────────────────────────────────────────────────

#[test]
fn load_from_rom_const_addr() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(42));
    Ok(())
}

#[test]
fn load_non_rom_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xDEADu64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    assert!(!LoadReadOnly(TestRom).optimize(&mut fg.graph, fg.entry)?.changed());
    // Load node should still be present.
    assert!(
        fg.all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Load(_)))
    );
    Ok(())
}

#[test]
fn load_non_const_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        // addr = 0x1000 + 0 — a non-trivial expression that constant_fold
        // would simplify, but we don't run constant_fold here.
        let base = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        let off = b.build_int_const(0u64, NodeOutputType::U64)?;
        let addr =
            b.build_int_binary_operation(base, off, ir::IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    // addr is an Add node, not a const → LoadReadOnly must not fire.
    assert!(!LoadReadOnly(TestRom).optimize(&mut fg.graph, fg.entry)?.changed());
    Ok(())
}

// ── comprehensive tests ───────────────────────────────────────────────────────

/// Loading more bytes than the ROM provides (read returns None) leaves the
/// Load node intact.
#[test]
fn load_oversize_read_no_change() -> Result<()> {
    struct Limited;
    impl ReadOnlyMemory for Limited {
        fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
            // Only single-byte reads are supported.
            if size == 1 && addr == 0x1000 {
                Some(42)
            } else {
                None
            }
        }
    }
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        // Request 8 bytes — limited ROM returns None.
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    assert!(!LoadReadOnly(Limited).optimize(&mut fg.graph, fg.entry)?.changed());
    Ok(())
}

/// A ROM that distinguishes spaces must not fold a load from a
/// non-matching space.
#[test]
fn load_other_space_no_change() -> Result<()> {
    struct RamOnly;
    impl ReadOnlyMemory for RamOnly {
        fn read(&self, space: rsleigh::VnSpace, _addr: u64, _size: usize) -> Option<u64> {
            if space == rsleigh::VnSpace::RAM {
                Some(0)
            } else {
                None
            }
        }
    }
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::REGISTER, NodeOutputType::U64)
    })?;
    assert!(!LoadReadOnly(RamOnly).optimize(&mut fg.graph, fg.entry)?.changed());
    Ok(())
}

/// Loading at U8 from a ROM cell that returns 0xFF: the optimizer applies
/// `ty.get_unsigned_int(loaded)`, so 0xFF in U8 stays 0xFF.
#[test]
fn load_u8_masks_to_byte() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x2000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// Multiple loads at different addresses fold independently in one pass.
#[test]
fn multiple_loads_fold_in_one_pass() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a1 = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        let a2 = b.build_int_const(0x2000u64, NodeOutputType::U64)?;
        let l1 = b.build_load(a1, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let l2 = b.build_load(a2, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_int_binary_operation(l1, l2, ir::IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg.graph, fg.entry)?.changed());
    // Both loads must have folded out of the reachable subgraph.
    let remaining_loads =
        crate::test_support::count_reachable((&fg).into(), |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}

