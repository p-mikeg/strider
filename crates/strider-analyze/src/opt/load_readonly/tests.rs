use crate::opt::pipeline::Optimizer;
use crate::opt::test_support::{make_fn, return_kind};
use super::*;
use crate::opt::error::Result;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::MockRom;

// ── tiny ROM fixture ──────────────────────────────────────────────────────────

fn test_rom() -> MockRom {
    MockRom::fixed_table(&[(0x1000, 42), (0x2000, 0xFF)])
}

// ── original tests ────────────────────────────────────────────────────────────

#[test]
fn load_from_rom_const_addr() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    let entry = fg.entry().unwrap();
    assert!(LoadReadOnly::new(std::sync::Arc::new(test_rom())).optimize(&mut fg, entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(42));
    Ok(())
}

#[test]
fn load_non_rom_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xDEADu64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    let entry = fg.entry().unwrap();
    assert!(!LoadReadOnly::new(std::sync::Arc::new(test_rom())).optimize(&mut fg, entry)?.changed());
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
            b.build_int_binary_operation(base, off, strider_ir::IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    // addr is an Add node, not a const → LoadReadOnly must not fire.
    let entry = fg.entry().unwrap();
    assert!(!LoadReadOnly::new(std::sync::Arc::new(test_rom())).optimize(&mut fg, entry)?.changed());
    Ok(())
}

// ── comprehensive tests ───────────────────────────────────────────────────────

/// Loading more bytes than the ROM provides (read returns None) leaves the
/// Load node intact.
#[test]
fn load_oversize_read_no_change() -> Result<()> {
    // Only single-byte reads at 0x1000 are supported.
    let rom = MockRom::limited(0x1000, 1, 42);
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        // Request 8 bytes — limited ROM returns None.
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    })?;
    let entry = fg.entry().unwrap();
    assert!(!LoadReadOnly::new(std::sync::Arc::new(rom)).optimize(&mut fg, entry)?.changed());
    Ok(())
}

/// The pass gates on `Load(VnSpace::RAM)` at the call site, so a
/// `Load(REGISTER, ...)` must not fold even if the rom would
/// happily answer the address.
#[test]
fn load_other_space_no_change() -> Result<()> {
    let rom = MockRom::always_answer(0xdeadbeef);
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::REGISTER, NodeOutputType::U64)
    })?;
    let entry = fg.entry().unwrap();
    assert!(!LoadReadOnly::new(std::sync::Arc::new(rom)).optimize(&mut fg, entry)?.changed());
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
    let entry = fg.entry().unwrap();
    assert!(LoadReadOnly::new(std::sync::Arc::new(test_rom())).optimize(&mut fg, entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
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
        b.build_int_binary_operation(l1, l2, strider_ir::IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    let entry = fg.entry().unwrap();
    assert!(LoadReadOnly::new(std::sync::Arc::new(test_rom())).optimize(&mut fg, entry)?.changed());
    // Both loads must have folded out of the reachable subgraph.
    let remaining_loads =
        fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}

/// `LoadReadOnly` must fold a constant-address Load even when `AliasSplit`
/// has already run on the same graph and partitioned the memory chain.
///
/// The graph has a Stack-relative store before a `Call` barrier (so
/// `StackOffsetDetect` stamps an offset for that store) and a ROM Load
/// at 0x1000 after the barrier (no SP-relative address — side-table
/// untouched).  `LoadReadOnly` only inspects the Load's *address*
/// operand, never the memory input, so it folds the Load to
/// `IntConst(42)` regardless.
#[test]
fn load_readonly_fires_after_stack_offset_detect() -> Result<()> {
    use crate::opt::{StackOffsetDetect, Optimizer as _};
    use strider_ir_test_utils::{make_sp_fn, sp_vn_x86, SENTINEL_LIFT_ADDR};

    let sp = sp_vn_x86();

    // Build: stack-store (SP-4) → call 0xCAFE → load 0x1000 → return loaded.
    let mut fg = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let stack_addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x55u64, NodeOutputType::U32)?;
        b.build_store(stack_addr, data, rsleigh::VnSpace::RAM)?;

        let call_tgt = b.build_int_const(0xCAFEu64, NodeOutputType::U32)?;
        b.build_call(call_tgt)?;

        // ROM load at constant address — non-SP-rooted, no side-table
        // entry stamped.  LoadReadOnly sees the constant address and
        // folds the load.
        let rom_addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        let loaded = b.build_load(rom_addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;
    let entry = fg.entry().unwrap();

    let split_result = StackOffsetDetect::new(sp).optimize(&mut fg, entry)?;
    assert!(
        split_result.changed(),
        "StackOffsetDetect must stamp the stack-store offset"
    );

    assert!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))) >= 1,
        "ROM Load must survive StackOffsetDetect"
    );

    // LoadReadOnly folds the constant-address Load to IntConst(42).
    let rom = std::sync::Arc::new(test_rom());
    let fold_result = LoadReadOnly::new(rom).optimize(&mut fg, entry)?;
    assert!(fold_result.changed(), "LoadReadOnly must fold the ROM Load");

    // No Load nodes remain after folding.
    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))),
        0,
        "all Load nodes must be folded"
    );

    Ok(())
}

