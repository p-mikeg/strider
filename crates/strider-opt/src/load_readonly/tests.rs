use super::*;
use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizerTestExt, PostOptimizerTestExt};
use crate::test_support::{make_fn, return_kind};
use strider_ir::IRWalker;
use strider_ir::node::{IntPayload, NodeKind, ValueType};
use strider_ir_test_utils::{MockRom, make_empty_fn_endian};
use strider_target::Endianness;

/// A `ReadOnlyMemory` that serves a fixed run of RAW bytes starting at
/// `base`.  Fills the caller buffer with the raw mapped bytes (no
/// endianness swap) — the decode is the optimizer's job now.  Errors
/// (the all-or-nothing contract) when any requested byte lies outside
/// the configured run.
struct RawBytesRom {
    base: u64,
    bytes: Vec<u8>,
}

impl ReadOnlyMemory for RawBytesRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let start = addr
            .checked_sub(self.base)
            .and_then(|o| usize::try_from(o).ok())
            .ok_or_else(|| anyhow::anyhow!("addr {addr:#x} below base"))?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| anyhow::anyhow!("read length overflow"))?;
        let src = self
            .bytes
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("read past end of mapped bytes"))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

// ── tiny ROM fixture ──────────────────────────────────────────────────────────

fn test_rom() -> MockRom {
    MockRom::fixed_table(&[(0x1000, 42), (0x2000, 0xFF)])
}

// ── original tests ────────────────────────────────────────────────────────────

#[test]
fn load_from_rom_const_addr() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
    })?;
    let rom = test_rom();
    assert!(
        LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntConst(IntPayload::Small(42))
    );
    Ok(())
}

/// Proof-completeness: the load ADDRESS is part of the proof for the folded
/// value (which bytes got read depends on the address).  After `LoadReadOnly`
/// folds a constant-address Load, the resulting `IntConst` must carry the
/// address const's asm-fingerprint.  Without the absorb, the culled address
/// cone's asm is lost.  Over-tainting is intentional (superset-only contract).
#[test]
fn load_fold_absorbs_address_fingerprint() -> Result<()> {
    use strider_ir::IRViewer;
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_empty_fn};
    const ADDR_ADDR: u64 = 0xC0DE_0003;

    let mut fg = make_empty_fn(|b| {
        // The ADDRESS const carries a distinct addr; the Load (and everything
        // else) carries the sentinel — so an absorbed ADDR_ADDR on the folded
        // IntConst can only have come from the address cone.
        b.set_lift_addr(Some(ADDR_ADDR));
        let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
    })?;

    let rom = test_rom();
    assert!(
        LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntConst(IntPayload::Small(42))
    );

    let folded = fg.producer(crate::test_support::return_value(fg.graph())?);
    assert!(
        fg.asm_fingerprint(folded).contains(&ADDR_ADDR),
        "LoadReadOnly must absorb the load address's asm-fingerprint into the \
         folded constant (proof of which bytes were read); got {:?}",
        fg.asm_fingerprint(folded)
    );
    Ok(())
}

#[test]
fn load_non_rom_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xDEADu64, ValueType::I64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
    })?;
    let rom = test_rom();
    assert!(
        !LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    // Load node should still be present.
    assert!(
        fg.graph()
            .all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Load(_)))
    );
    Ok(())
}

#[test]
fn load_non_const_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        // addr = 0x1000 + 0 — a non-trivial expression that constant_fold
        // would simplify, but we don't run constant_fold here.
        let base = b.build_int_const(0x1000u64, ValueType::I64)?;
        let off = b.build_int_const(0u64, ValueType::I64)?;
        let addr =
            b.build_int_binary_operation(base, off, strider_ir::IntBinaryOp::Add, ValueType::I64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
    })?;
    // addr is an Add node, not a const → LoadReadOnly must not fire.
    let rom = test_rom();
    assert!(
        !LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    Ok(())
}

// ── endianness-aware decode (item 5) ──────────────────────────────────────────

/// The SAME four raw mapped bytes `[0x01,0x02,0x03,0x04]` must fold to
/// `0x04030201` under little-endian and `0x01020304` under big-endian.
/// This proves the byte→integer decode now lives in the optimizer and
/// respects the function's `Function::endianness`, not the reader.
#[test]
fn const_load_decodes_per_context_endianness() -> Result<()> {
    let rom = RawBytesRom {
        base: 0x1000,
        bytes: vec![0x01, 0x02, 0x03, 0x04],
    };

    // The decode endianness is the function's own, so build the LE and BE
    // variants accordingly.
    let build = |endian| {
        make_empty_fn_endian(endian, |b| {
            let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
            b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)
        })
    };

    let mut le = build(Endianness::Little)?;
    assert!(
        LoadReadOnly
            .run_one(&mut le, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    assert_eq!(
        return_kind(le.graph())?,
        NodeKind::IntConst(IntPayload::Small(0x0403_0201))
    );

    let mut be = build(Endianness::Big)?;
    assert!(
        LoadReadOnly
            .run_one(&mut be, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    assert_eq!(
        return_kind(be.graph())?,
        NodeKind::IntConst(IntPayload::Small(0x0102_0304))
    );

    Ok(())
}

/// A 16-byte (I128) constant-address load from a ROM serving 16 raw
/// bytes must fold to the full `IntConst` value, decoded per the
/// context endianness.  This exercises the widened fold path: the
/// reader fills a 16-byte buffer and `Endianness::read_uint` decodes
/// the full `u128` (no truncation to the low 8 bytes).
#[test]
fn const_load_16_bytes_folds_to_i128_both_endians() -> Result<()> {
    // 16 distinct raw bytes so a truncation bug would be visible.
    let raw: Vec<u8> = vec![
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let rom = RawBytesRom {
        base: 0x1000,
        bytes: raw.clone(),
    };

    let build = |endian| {
        make_empty_fn_endian(endian, |b| {
            let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
            b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I128)
        })
    };

    let raw_arr: [u8; 16] = raw.clone().try_into().unwrap();

    let mut le = build(Endianness::Little)?;
    assert!(
        LoadReadOnly
            .run_one(&mut le, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    // I128 constants are interner-backed (IntPayload::Wide); read the value
    // through the int_const_u128 funnel rather than comparing NodeKind directly.
    {
        use crate::test_support::return_value;
        use strider_ir::IRViewer;
        let ret = return_value(le.graph())?;
        assert_eq!(
            le.int_const_u128(ret),
            Some(u128::from_le_bytes(raw_arr)),
            "LE: folded I128 load must equal u128::from_le_bytes(raw)"
        );
    }

    let mut be = build(Endianness::Big)?;
    assert!(
        LoadReadOnly
            .run_one(&mut be, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    {
        use crate::test_support::return_value;
        use strider_ir::IRViewer;
        let ret = return_value(be.graph())?;
        assert_eq!(
            be.int_const_u128(ret),
            Some(u128::from_be_bytes(raw_arr)),
            "BE: folded I128 load must equal u128::from_be_bytes(raw)"
        );
    }

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
        let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
        // Request 8 bytes — limited ROM returns None.
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
    })?;
    assert!(
        !LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    Ok(())
}

/// The pass gates on `Load(VnSpace::RAM)` at the call site, so a
/// `Load(REGISTER, ...)` must not fold even if the rom would
/// happily answer the address.
#[test]
fn load_other_space_no_change() -> Result<()> {
    let rom = MockRom::always_answer(0xdeadbeef);
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
        b.build_load(addr, rsleigh::VnSpace::REGISTER, ValueType::I64)
    })?;
    assert!(
        !LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    Ok(())
}

/// Loading at I8 from a ROM cell that returns 0xFF: the optimizer applies
/// `ty.get_unsigned_int(loaded)`, so 0xFF in I8 stays 0xFF.
#[test]
fn load_u8_masks_to_byte() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x2000u64, ValueType::I64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I8)
    })?;
    let rom = test_rom();
    assert!(
        LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntConst(IntPayload::Small(0xFF))
    );
    Ok(())
}

/// Multiple loads at different addresses fold independently in one pass.
#[test]
fn multiple_loads_fold_in_one_pass() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a1 = b.build_int_const(0x1000u64, ValueType::I64)?;
        let a2 = b.build_int_const(0x2000u64, ValueType::I64)?;
        let l1 = b.build_load(a1, rsleigh::VnSpace::RAM, ValueType::I64)?;
        let l2 = b.build_load(a2, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_int_binary_operation(l1, l2, strider_ir::IntBinaryOp::Add, ValueType::I64)
    })?;
    let rom = test_rom();
    assert!(
        LoadReadOnly
            .run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?
            .changed()
    );
    // Both loads must have folded out of the reachable subgraph.
    let remaining_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}

/// `LoadReadOnly` must fold a constant-address Load even when `StackOffsetDetect`
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
    use crate::StackOffsetDetect;
    use crate::pipeline::OptimizerTestExt;
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_sp_fn, stack_vn_x86};

    let sp = stack_vn_x86();

    // Build: stack-store (SP-4) → call 0xCAFE → load 0x1000 → return loaded.
    let mut fg = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let stack_addr = b.build_sub_as_add_neg(sp_v, four, ValueType::I32)?;
        let data = b.build_int_const(0x55u64, ValueType::I32)?;
        b.build_store(stack_addr, data, rsleigh::VnSpace::RAM)?;

        let call_tgt = b.build_int_const(0xCAFEu64, ValueType::I32)?;
        b.build_call(call_tgt, None)?;

        // ROM load at constant address — non-SP-rooted, no side-table
        // entry stamped.  LoadReadOnly sees the constant address and
        // folds the load.
        let rom_addr = b.build_int_const(0x1000u64, ValueType::I64)?;
        let loaded = b.build_load(rom_addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Collapse the single-predecessor `read_variable(sp)` phi so the SP
    // address is a bare `InitialVar(sp) + k` terminal (as it is once the
    // pipeline's PhiCollapse has run in production) before the SP-aware pass.
    let mut pre = crate::OptimizerPipeline::new();
    pre.add(crate::PhiCollapse);
    pre.add(crate::RegionCollapse);
    pre.run(&mut fg, &mut OptCtx::new(None))?;

    StackOffsetDetect.run_one(&mut fg, &mut OptCtx::new(None))?;
    assert!(
        fg.graph()
            .all_node_ids()
            .any(|n| fg.stack_offset(n).is_some()),
        "StackOffsetDetect must stamp the stack-store offset"
    );

    assert!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))) >= 1,
        "ROM Load must survive StackOffsetDetect"
    );

    // LoadReadOnly folds the constant-address Load to IntConst(42).
    let rom = test_rom();
    let fold_result = LoadReadOnly.run_one(&mut fg, &mut OptCtx::new(Some(&rom)))?;
    assert!(fold_result.changed(), "LoadReadOnly must fold the ROM Load");

    // No Load nodes remain after folding.
    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))),
        0,
        "all Load nodes must be folded"
    );

    Ok(())
}
