use super::*;
use crate::Result;
use crate::pipeline::OptCtx;
use crate::test_support::{assert_returns_const, make_fn};
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{MockRom, make_empty_fn_endian};
use strider_target::Endianness;

fn test_rom() -> MockRom {
    MockRom::fixed_table(&[(0x1000, 42), (0x2000, 0xFF)])
}

/// The address is part of the proof for the folded value, so the new
/// `IntConst` must inherit it; without the absorb, the culled address cone's
/// asm is lost.
#[test]
fn load_fold_absorbs_address_fingerprint() -> Result<()> {
    use strider_ir::IRViewer;
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_empty_fn};
    const ADDR_ADDR: u64 = 0xC0DE_0003;

    let mut fg = make_empty_fn(|b| {
        // The address const gets a distinct addr while everything else gets
        // the sentinel, so ADDR_ADDR on the folded IntConst can only have
        // come from the address cone.
        b.set_lift_addr(Some(ADDR_ADDR));
        let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
    })?;

    let rom = test_rom();
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    assert_returns_const(&fg, 42);

    let folded = fg.producer(crate::test_support::return_value(fg.graph())?);
    assert!(
        fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&ADDR_ADDR),
        "LoadReadOnly must absorb the load address's asm-fingerprint into the \
         folded constant (proof of which bytes were read); got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

#[test]
fn load_non_rom_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xDEADu64, ValueType::int(64))?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
    })?;
    let rom = test_rom();
    assert!(!crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
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
        // `LoadReadOnly` runs alone below, so `0x1000 + 0` stays an Add.
        let base = b.build_int_const(0x1000u64, ValueType::int(64))?;
        let off = b.build_int_const(0u64, ValueType::int(64))?;
        let addr = b.build_int_binary_operation(
            base,
            off,
            strider_ir::IntBinaryOp::Add,
            ValueType::int(64),
        )?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
    })?;
    let rom = test_rom();
    assert!(!crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    Ok(())
}

/// The same four mapped bytes must fold differently per endianness: the
/// byte-to-integer decode belongs to the optimizer and reads
/// `Function::endianness`, not the reader.
#[test]
fn const_load_decodes_per_context_endianness() -> Result<()> {
    let rom = MockRom::raw_bytes(0x1000, vec![0x01, 0x02, 0x03, 0x04]);

    let build = |endian| {
        make_empty_fn_endian(endian, |b| {
            let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
            b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))
        })
    };

    let mut le = build(Endianness::Little)?;
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut le, &OptCtx::new(Some(&rom)))?.changed());
    assert_returns_const(&le, 0x0403_0201);

    let mut be = build(Endianness::Big)?;
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut be, &OptCtx::new(Some(&rom)))?.changed());
    assert_returns_const(&be, 0x0102_0304);

    Ok(())
}

/// The widened fold path: `Endianness::read_uint` must decode the full
/// `u128`, not truncate to the low 8 bytes.
#[test]
fn const_load_16_bytes_folds_to_i128_both_endians() -> Result<()> {
    // Distinct bytes so a truncation bug is visible.
    let raw: Vec<u8> = vec![
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let rom = MockRom::raw_bytes(0x1000, raw.clone());

    let build = |endian| {
        make_empty_fn_endian(endian, |b| {
            let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
            b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(128))
        })
    };

    let raw_arr: [u8; 16] = raw.clone().try_into().unwrap();

    let mut le = build(Endianness::Little)?;
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut le, &OptCtx::new(Some(&rom)))?.changed());
    // I128 constants are interned, so read through `int_const_u128` rather
    // than comparing NodeKind.
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
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut be, &OptCtx::new(Some(&rom)))?.changed());
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

/// The decode spans the whole type, so a 32- or 64-byte load folds to the
/// bytes the image holds. `load_oversize_read_no_change` covers the
/// read-failure path.
#[test]
fn const_load_wider_than_16_bytes_folds() -> Result<()> {
    use crate::test_support::return_value;
    use strider_ir::{IRViewer, UBig};

    let rom = MockRom::raw_bytes(0x1000, (0..64u8).collect());

    for ty in [ValueType::int(256), ValueType::int(512)] {
        let mut fg = make_fn(|b| {
            let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
            b.build_load(addr, rsleigh::VnSpace::RAM, ty)
        })?;
        assert!(
            crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed(),
            "{ty:?}: the whole type is decoded",
        );
        let want: UBig = (0..ty.byte_size())
            .rev()
            .fold(UBig::ZERO, |acc, i| (acc << 8usize) + UBig::from(i as u8));
        assert_eq!(
            fg.int_const(return_value(fg.graph())?).cloned(),
            Some(want),
            "{ty:?}: little-endian bytes 0..n",
        );
    }
    Ok(())
}

#[test]
fn load_oversize_read_no_change() -> Result<()> {
    // The ROM answers only 1-byte reads at 0x1000; the load asks for 8.
    let rom = MockRom::limited(0x1000, 1, 42);
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
    })?;
    assert!(!crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    Ok(())
}

/// A `Load(REGISTER, ...)` must not fold even though this rom answers any
/// address.
#[test]
fn load_other_space_no_change() -> Result<()> {
    let rom = MockRom::always_answer(0xdeadbeef);
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.build_load(addr, rsleigh::VnSpace::REGISTER, ValueType::int(64))
    })?;
    assert!(!crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    Ok(())
}

#[test]
fn load_u8_masks_to_byte() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x2000u64, ValueType::int(64))?;
        b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(8))
    })?;
    let rom = test_rom();
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    assert_returns_const(&fg, 0xFF);
    Ok(())
}

#[test]
fn multiple_loads_fold_in_one_pass() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a1 = b.build_int_const(0x1000u64, ValueType::int(64))?;
        let a2 = b.build_int_const(0x2000u64, ValueType::int(64))?;
        let l1 = b.build_load(a1, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let l2 = b.build_load(a2, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_int_binary_operation(l1, l2, strider_ir::IntBinaryOp::Add, ValueType::int(64))
    })?;
    let rom = test_rom();
    assert!(crate::pipeline::run_one(&LoadReadOnly, &mut fg, &OptCtx::new(Some(&rom)))?.changed());
    let remaining_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}
