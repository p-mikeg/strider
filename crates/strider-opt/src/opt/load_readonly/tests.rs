use super::*;
use crate::Result;
use crate::test_support::{assert_returns_const, make_fn, run_pass_rom};
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
    assert!(run_pass_rom(&LoadReadOnly, &mut fg, &rom)?);
    assert_returns_const(&fg, 42);

    let folded = fg.producer(fg.returned());
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

/// `Return(Load(space, addr):ty)` in `endian`, the address an `IntConst`.
fn load_fn(
    endian: Endianness,
    addr: u64,
    space: rsleigh::VnSpace,
    ty: ValueType,
) -> Result<strider_ir::Function> {
    make_empty_fn_endian(endian, |b| {
        let addr = b.build_int_const(addr, ValueType::int(64))?;
        b.build_load(addr, space, ty)
    })
}

/// A load folds to the bytes the image holds at its address, decoded per
/// `Function::endianness` (the optimizer's job, not the reader's) across the
/// whole type, past 16 bytes too, and masked to it.
#[test]
fn a_const_load_folds_to_the_decoded_bytes() -> Result<()> {
    use strider_ir::UBig;
    let bytes: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(0x11)).collect();
    let rom = MockRom::raw_bytes(0x1000, bytes.clone());
    let decode = |endian, n: usize| {
        let run = bytes[..n].iter();
        let fold = |acc: UBig, &byte: &u8| (acc << 8usize) + UBig::from(byte);
        match endian {
            Endianness::Little => run.rev().fold(UBig::ZERO, fold),
            Endianness::Big => run.fold(UBig::ZERO, fold),
        }
    };
    for endian in [Endianness::Little, Endianness::Big] {
        for bits in [8, 32, 128, 256, 512] {
            let ty = ValueType::int(bits);
            let mut fg = load_fn(endian, 0x1000, rsleigh::VnSpace::RAM, ty)?;
            assert!(
                run_pass_rom(&LoadReadOnly, &mut fg, &rom)?,
                "{endian:?} {ty}"
            );
            assert_eq!(
                fg.int_const(fg.returned()),
                Some(&decode(endian, ty.byte_size())),
                "{endian:?} {ty}"
            );
        }
    }
    let mut fg = load_fn(
        Endianness::Little,
        0x2000,
        rsleigh::VnSpace::RAM,
        ValueType::int(8),
    )?;
    assert!(run_pass_rom(&LoadReadOnly, &mut fg, &test_rom())?);
    assert_returns_const(&fg, 0xFF);
    Ok(())
}

/// A load the rom cannot answer stays: an unmapped address, an address that
/// is no constant (`0x1000 + 0`, which LoadReadOnly alone does not fold), a
/// read wider than the rom answers, and a `Load(REGISTER)` at an address
/// this rom answers for any, which only the pass's RAM filter refuses.
#[test]
fn an_unanswerable_load_stays() -> Result<()> {
    let i64 = ValueType::int(64);
    let ram = rsleigh::VnSpace::RAM;
    let mut cases = vec![
        (
            "unmapped",
            load_fn(Endianness::Little, 0xDEAD, ram, i64)?,
            test_rom(),
        ),
        (
            "oversize",
            load_fn(Endianness::Little, 0x1000, ram, i64)?,
            MockRom::limited(0x1000, 1, 42),
        ),
        (
            "register space",
            load_fn(Endianness::Little, 0x1000, rsleigh::VnSpace::REGISTER, i64)?,
            MockRom::always_answer(0xdeadbeef),
        ),
    ];
    let non_const = make_fn(|b| {
        let base = b.build_int_const(0x1000u64, i64)?;
        let addr = b.build_const_binop(0u64, base, strider_ir::IntBinaryOp::Add, i64)?;
        b.build_load(addr, ram, i64)
    })?;
    cases.push(("non-const address", non_const, test_rom()));
    for (case, mut fg, rom) in cases {
        assert!(!run_pass_rom(&LoadReadOnly, &mut fg, &rom)?, "{case}");
        assert!(fg.has_kind(|k| matches!(k, NodeKind::Load(_))), "{case}");
    }
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
    assert!(run_pass_rom(&LoadReadOnly, &mut fg, &rom)?);
    let remaining_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}
