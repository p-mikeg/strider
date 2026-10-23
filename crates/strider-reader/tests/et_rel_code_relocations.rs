//! An object file's relocations against instructions, GOT slots and symbols
//! the object does not define.
//!
//! Unapplied, each field decodes as a plausible reference into the object
//! itself: `call rel32 = 0` is a call to the next instruction, `bl 0` a branch
//! to itself, a zero `adrp` page this function's own page. Every object below
//! is built with `object::write` and read back through
//! `OwnedElf::regions(.., relocate = true)`; the expected targets are decoded
//! from the patched instructions by hand.

#[path = "common/mod.rs"]
mod common;

use common::write_object::{defined, reloc, undefined};
use object::write::{Object, Symbol, SymbolId, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope, elf,
};
use object::{Object as _, ObjectSymbol as _};
use strider_reader::MemRegionsLookupTable;
use strider_reader::elf::{ElfSectionLayout, LoadFilter};

fn relocated(bytes: &[u8]) -> MemRegionsLookupTable {
    MemRegionsLookupTable::new(common::relocated(bytes, LoadFilter::AllAllocatable))
}

fn word(table: &MemRegionsLookupTable, addr: u64, big_endian: bool) -> u32 {
    let mut buf = [0u8; 4];
    table.read_exact(addr, &mut buf).expect("mapped");
    if big_endian {
        u32::from_be_bytes(buf)
    } else {
        u32::from_le_bytes(buf)
    }
}

fn sext(v: u64, bits: u32) -> i64 {
    ((v << (64 - bits)) as i64) >> (64 - bits)
}

/// Where the ELF places the symbol named `name`, defined or not.
fn symbol_address(bytes: &[u8], name: &str) -> u64 {
    let obj = object::File::parse(bytes).expect("parse");
    let layout = ElfSectionLayout::new(&obj);
    let sym = obj.symbol_by_name(name).expect(name);
    layout
        .extern_address(sym.index().0)
        .unwrap_or_else(|| layout.symbol_address(&sym))
}

/// `h(x) { return ext_fn(x) + 1; }`: the `call` is to a symbol no section
/// defines, and must land on that symbol's own address, outside every mapping.
#[test]
fn a_call_to_an_undefined_function_targets_its_extern_address() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0xe8, 0, 0, 0, 0, 0xc3], 1);
    defined(&mut obj, "h", text, 0);
    let ext = undefined(&mut obj, "ext_fn");
    reloc(&mut obj, text, 1, ext, -4, elf::R_X86_64_PLT32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let call = common::section_base(&bytes, ".text");
    let disp = word(&table, call + 1, false) as i32;
    let target = (call + 5).wrapping_add_signed(i64::from(disp));
    assert_ne!(target, call + 5, "a call to the next instruction");
    assert_eq!(target, symbol_address(&bytes, "ext_fn"));
    assert!(
        table.read(target, &mut [0u8; 1]).is_none(),
        "an undefined symbol's address maps nothing"
    );
}

/// `mov g@GOTPCREL(%rip), %rax`: the displacement reaches a slot holding `g`'s
/// address.
#[test]
fn a_gotpcrel_load_reaches_a_slot_holding_the_symbol_address() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0x48, 0x8b, 0x05, 0, 0, 0, 0, 0xc3], 1);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 16], 8);
    let g = defined(&mut obj, "g", data, 8);
    reloc(&mut obj, text, 3, g, -4, elf::R_X86_64_REX_GOTPCRELX);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let disp = word(&table, insn + 3, false) as i32;
    let slot = (insn + 7).wrapping_add_signed(i64::from(disp));
    let mut held = [0u8; 8];
    table
        .read_exact(slot, &mut held)
        .expect("the GOT slot is mapped");
    assert_eq!(u64::from_le_bytes(held), symbol_address(&bytes, "g"));
}

/// A position-independent i386 function sets `%ebx` to the GOT with
/// `R_386_GOTPC` and addresses data from it with `R_386_GOTOFF`; the two must
/// agree on where the GOT is. Both keep their addend in the field.
#[test]
fn i386_gotpc_and_gotoff_address_the_symbol_together() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::I386, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    // add $2, %ebx (the GOTPC addend is the field's distance past the PC the
    // thunk loaded, the instruction start); lea 0(%ebx), %eax
    obj.append_section_data(
        text,
        &[0x81, 0xc3, 2, 0, 0, 0, 0x8d, 0x83, 0, 0, 0, 0, 0xc3],
        1,
    );
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 8], 4);
    let got = undefined(&mut obj, "_GLOBAL_OFFSET_TABLE_");
    let g = defined(&mut obj, "g", data, 4);
    reloc(&mut obj, text, 2, got, 0, elf::R_386_GOTPC);
    reloc(&mut obj, text, 8, g, 0, elf::R_386_GOTOFF);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let ebx = insn.wrapping_add(u64::from(word(&table, insn + 2, false)));
    let eax = ebx.wrapping_add(u64::from(word(&table, insn + 8, false))) & 0xffff_ffff;
    assert_eq!(eax, symbol_address(&bytes, "g"));
}

/// `bl callee` into another section, then `adrp` + `ldr :lo12:` of a `.data`
/// global.
#[test]
fn aarch64_call_and_page_relative_load_resolve() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Little);
    let callee_sec = obj.add_section(Vec::new(), b".text.callee".to_vec(), SectionKind::Text);
    obj.append_section_data(callee_sec, &0xd65f_03c0u32.to_le_bytes(), 4);
    let caller = obj.add_section(Vec::new(), b".text.caller".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    for insn in [0x9400_0000u32, 0x9000_0000, 0xf940_0000] {
        code.extend_from_slice(&insn.to_le_bytes());
    }
    obj.append_section_data(caller, &code, 4);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 0x1800], 8);
    let callee = defined(&mut obj, "callee", callee_sec, 0);
    let g = defined(&mut obj, "g", data, 0x1238);
    reloc(&mut obj, caller, 0, callee, 0, elf::R_AARCH64_CALL26);
    reloc(&mut obj, caller, 4, g, 0, elf::R_AARCH64_ADR_PREL_PG_HI21);
    reloc(&mut obj, caller, 8, g, 0, elf::R_AARCH64_LDST64_ABS_LO12_NC);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let p = common::section_base(&bytes, ".text.caller");
    let bl = u64::from(word(&table, p, false));
    let target = p.wrapping_add_signed(sext(bl & 0x03ff_ffff, 26) * 4);
    assert_eq!(target, symbol_address(&bytes, "callee"));

    let g_addr = symbol_address(&bytes, "g");
    let adrp = u64::from(word(&table, p + 4, false));
    let page = sext((((adrp >> 5) & 0x7_ffff) << 2) | ((adrp >> 29) & 3), 21);
    assert_eq!(
        ((p + 4) & !0xfff).wrapping_add_signed(page << 12),
        g_addr & !0xfff
    );
    let ldr = u64::from(word(&table, p + 8, false));
    assert_eq!(((ldr >> 10) & 0xfff) << 3, g_addr & 0xfff);
}

/// ARM `bl` into another section, and Thumb `bl`s to an ARM function (which
/// the link turns into a `blx`) and to a Thumb one. `SHT_REL`, so each
/// instruction holds its addend.
#[test]
fn arm_and_thumb_calls_resolve_with_interworking() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
    let arm_sec = obj.add_section(Vec::new(), b".text.arm".to_vec(), SectionKind::Text);
    obj.append_section_data(arm_sec, &0xe12f_ff1eu32.to_le_bytes(), 4);
    let thumb_sec = obj.add_section(Vec::new(), b".text.thumb".to_vec(), SectionKind::Text);
    obj.append_section_data(thumb_sec, &[0x70, 0x47, 0x00, 0xbf], 4);
    let caller = obj.add_section(Vec::new(), b".text.caller".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    code.extend_from_slice(&0xe320_f000u32.to_le_bytes()); // nop
    code.extend_from_slice(&0xebff_fffeu32.to_le_bytes()); // bl, A = -8
    // Two Thumb `bl`s, A = -4, each halfword little-endian.
    for _ in 0..2 {
        code.extend_from_slice(&[0xff, 0xf7, 0xfe, 0xff]);
    }
    obj.append_section_data(caller, &code, 4);
    let arm_fn = defined(&mut obj, "arm_fn", arm_sec, 0);
    let thumb_fn = defined(&mut obj, "thumb_fn", thumb_sec, 1);
    reloc(&mut obj, caller, 4, arm_fn, 0, elf::R_ARM_CALL);
    reloc(&mut obj, caller, 8, arm_fn, 0, elf::R_ARM_THM_PC22);
    reloc(&mut obj, caller, 12, thumb_fn, 0, elf::R_ARM_THM_PC22);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let p = common::section_base(&bytes, ".text.caller");
    let arm_addr = symbol_address(&bytes, "arm_fn");
    let thumb_addr = symbol_address(&bytes, "thumb_fn") & !1;

    let bl = u64::from(word(&table, p + 4, false));
    assert_eq!(bl >> 24, 0xeb, "still an ARM bl");
    let target = (p + 4 + 8).wrapping_add_signed(sext(bl & 0x00ff_ffff, 24) * 4);
    assert_eq!(target, arm_addr);

    let thumb_target = |site: u64| -> (u64, bool) {
        let raw = word(&table, site, false);
        let (hw1, hw2) = (u64::from(raw & 0xffff), u64::from(raw >> 16));
        let s = (hw1 >> 10) & 1;
        let i1 = !((hw2 >> 13) ^ s) & 1;
        let i2 = !((hw2 >> 11) ^ s) & 1;
        let off = sext(
            (s << 24) | (i1 << 23) | (i2 << 22) | ((hw1 & 0x3ff) << 12) | ((hw2 & 0x7ff) << 1),
            25,
        );
        let blx = hw2 & 0x1000 == 0;
        let pc = site + 4;
        let base = if blx { pc & !3 } else { pc };
        (base.wrapping_add_signed(off), blx)
    };
    assert_eq!(thumb_target(p + 8), (arm_addr, true), "a blx into ARM");
    assert_eq!(
        thumb_target(p + 12),
        (thumb_addr, false),
        "a bl within Thumb"
    );
}

/// PowerPC `bl` into another section and a `lis` / `addi` pair whose low half
/// is negative, so `@ha` carries.
#[test]
fn ppc_call_and_split_address_resolve() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::PowerPc, Endianness::Big);
    let callee_sec = obj.add_section(Vec::new(), b".text.callee".to_vec(), SectionKind::Text);
    obj.append_section_data(callee_sec, &0x4e80_0020u32.to_be_bytes(), 4);
    let caller = obj.add_section(Vec::new(), b".text.caller".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    for insn in [0x4800_0001u32, 0x3d20_0000, 0x3929_0000] {
        code.extend_from_slice(&insn.to_be_bytes());
    }
    obj.append_section_data(caller, &code, 4);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 0x9010], 4);
    let callee = defined(&mut obj, "callee", callee_sec, 0);
    let g = defined(&mut obj, "g", data, 0x9000);
    reloc(&mut obj, caller, 0, callee, 0, elf::R_PPC_REL24);
    reloc(&mut obj, caller, 6, g, 0, elf::R_PPC_ADDR16_HA);
    reloc(&mut obj, caller, 10, g, 0, elf::R_PPC_ADDR16_LO);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let p = common::section_base(&bytes, ".text.caller");
    let bl = u64::from(word(&table, p, true));
    assert_eq!(
        p.wrapping_add_signed(sext(bl & 0x03ff_fffc, 26)),
        symbol_address(&bytes, "callee")
    );
    let hi = u64::from(word(&table, p + 4, true) & 0xffff);
    let lo = u64::from(word(&table, p + 8, true) & 0xffff);
    assert_eq!(
        (hi << 16).wrapping_add_signed(sext(lo, 16)) & 0xffff_ffff,
        symbol_address(&bytes, "g")
    );
}

/// MIPS o32 `lui` / `addiu` against a global, the `SHT_REL` addend split across
/// the `R_MIPS_HI16` and the `R_MIPS_LO16` that follows it, and a `jal`.
#[test]
fn mips_hi16_lo16_pair_and_jal_resolve() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Mips, Endianness::Big);
    let callee_sec = obj.add_section(Vec::new(), b".text.callee".to_vec(), SectionKind::Text);
    obj.append_section_data(callee_sec, &0x03e0_0008u32.to_be_bytes(), 4);
    let caller = obj.add_section(Vec::new(), b".text.caller".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    // lui v0, 0; addiu v0, v0, -0x7ffc; jal 0; nop
    for insn in [0x3c02_0000u32, 0x2442_8004, 0x0c00_0000, 0] {
        code.extend_from_slice(&insn.to_be_bytes());
    }
    obj.append_section_data(caller, &code, 4);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 0x10], 4);
    let callee = defined(&mut obj, "callee", callee_sec, 0);
    let g = defined(&mut obj, "g", data, 0);
    reloc(&mut obj, caller, 0, g, 0, elf::R_MIPS_HI16);
    reloc(&mut obj, caller, 4, g, 0, elf::R_MIPS_LO16);
    reloc(&mut obj, caller, 8, callee, 0, elf::R_MIPS_26);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let p = common::section_base(&bytes, ".text.caller");
    let hi = u64::from(word(&table, p, true) & 0xffff);
    let lo = u64::from(word(&table, p + 4, true) & 0xffff);
    assert_eq!(
        (hi << 16).wrapping_add_signed(sext(lo, 16)) & 0xffff_ffff,
        symbol_address(&bytes, "g").wrapping_sub(0x7ffc)
    );
    let jal = u64::from(word(&table, p + 8, true));
    assert_eq!(
        ((p + 12) & !0x0fff_ffff) | ((jal & 0x03ff_ffff) << 2),
        symbol_address(&bytes, "callee")
    );
}

/// A relocation this loader does not compute (`R_X86_64_TPOFF32`, a
/// thread-pointer offset) leaves no plausible bytes in code: the field serves
/// nothing, so a decode across it fails. In writable data, which nothing
/// folds, the file-initial bytes stay.
#[test]
fn an_uncomputed_relocation_in_code_is_a_hole_and_in_data_is_left_alone() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0xb8, 0x11, 0x22, 0x33, 0x44, 0xc3], 1);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0x55u8; 8], 1);
    let tls = undefined(&mut obj, "tls_var");
    reloc(&mut obj, text, 1, tls, 0, elf::R_X86_64_TPOFF32);
    reloc(&mut obj, data, 0, tls, 0, elf::R_X86_64_TPOFF32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let mut buf = [0u8; 6];
    assert_eq!(
        table.read(insn, &mut buf),
        Some(1),
        "only the opcode is served"
    );
    assert_eq!(table.read(insn + 1, &mut buf), None);
    let err = table
        .read_exact(insn, &mut buf)
        .expect_err("a hole")
        .to_string();
    assert!(err.contains("relocation of type 23"), "got: {err}");
    assert_eq!(
        table.read(insn + 5, &mut buf[..1]),
        Some(1),
        "past the field"
    );

    let mut kept = [0u8; 8];
    table
        .read_exact(common::section_base(&bytes, ".data"), &mut kept)
        .expect("writable data stays mapped");
    assert_eq!(kept, [0x55; 8]);
}

/// A64 instructions are little-endian whatever `EI_DATA` says, so a patched
/// field on aarch64_be has to read back as a little-endian word. Written
/// big-endian the immediate lands in the wrong bits: `adrp` becomes `adr` and
/// `ldr w0, [x8, #:lo12:g]` a store.
#[test]
fn aarch64_big_endian_patches_instructions_little_endian() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Big);
    let callee_sec = obj.add_section(Vec::new(), b".text.callee".to_vec(), SectionKind::Text);
    // An instruction the object stores little-endian, as a BE aarch64
    // toolchain does; nothing relocates it, so it must read back unswapped.
    obj.append_section_data(callee_sec, &0xd65f_03c0u32.to_le_bytes(), 4);
    let caller = obj.add_section(Vec::new(), b".text.caller".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    for insn in [0x9400_0000u32, 0x9000_0000, 0xb940_0000] {
        code.extend_from_slice(&insn.to_le_bytes());
    }
    obj.append_section_data(caller, &code, 4);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 0x1800], 8);
    let callee = defined(&mut obj, "callee", callee_sec, 0);
    let g = defined(&mut obj, "g", data, 0x1234);
    reloc(&mut obj, caller, 0, callee, 0, elf::R_AARCH64_CALL26);
    reloc(&mut obj, caller, 4, g, 0, elf::R_AARCH64_ADR_PREL_PG_HI21);
    reloc(&mut obj, caller, 8, g, 0, elf::R_AARCH64_LDST32_ABS_LO12_NC);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    assert_eq!(
        word(&table, common::section_base(&bytes, ".text.callee"), false),
        0xd65f_03c0,
        "an unrelocated instruction is served exactly as the file holds it"
    );

    let p = common::section_base(&bytes, ".text.caller");
    let bl = u64::from(word(&table, p, false));
    assert_eq!(bl >> 26, 0x25, "a `bl`, not a byte-reversed word");
    let target = p.wrapping_add_signed(sext(bl & 0x03ff_ffff, 26) * 4);
    assert_eq!(target, symbol_address(&bytes, "callee"));

    let g_addr = symbol_address(&bytes, "g");
    let adrp = u64::from(word(&table, p + 4, false));
    assert_eq!(adrp >> 31, 1, "`adrp`, not the `adr` a swap turns it into");
    let page = sext((((adrp >> 5) & 0x7_ffff) << 2) | ((adrp >> 29) & 3), 21);
    assert_eq!(
        ((p + 4) & !0xfff).wrapping_add_signed(page << 12),
        g_addr & !0xfff
    );
    let ldr = u64::from(word(&table, p + 8, false));
    assert_eq!(ldr >> 22, 0x2e5, "a 32-bit `ldr`, not a store");
    assert_eq!(((ldr >> 10) & 0xfff) << 2, g_addr & 0xfff);
}

fn weak_undefined(obj: &mut Object<'_>, name: &str) -> SymbolId {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Unknown,
        scope: SymbolScope::Linkage,
        weak: true,
        section: SymbolSection::Undefined,
        flags: SymbolFlags::None,
    })
}

/// `if (w) w();` against an undefined weak `w`: the link or the module loader
/// resolves `w` to 0 as readily as to an address, so a synthetic non-null one
/// folds the test and drops the null path. The `mov $w, %ecx` immediate is
/// therefore not computed, while the `call w` displacement, which reveals no
/// address, keeps it.
#[test]
fn a_weak_undefined_symbols_address_is_not_folded_into_an_immediate() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    // mov $w, %ecx ; call w ; ret
    obj.append_section_data(text, &[0xb9, 0, 0, 0, 0, 0xe8, 0, 0, 0, 0, 0xc3], 1);
    defined(&mut obj, "f", text, 0);
    let w = weak_undefined(&mut obj, "w");
    reloc(&mut obj, text, 1, w, 0, elf::R_X86_64_32);
    reloc(&mut obj, text, 6, w, -4, elf::R_X86_64_PLT32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    assert_eq!(
        table.read(insn, &mut [0u8; 11]),
        Some(1),
        "only the opcode is served; the immediate is a hole"
    );

    let disp = word(&table, insn + 6, false) as i32;
    assert_eq!(
        (insn + 10).wrapping_add_signed(i64::from(disp)),
        symbol_address(&bytes, "w"),
        "a call keeps the extern address"
    );
}

/// The position-independent shape, `cmpq $0, w@GOTPCREL(%rip)`: the
/// displacement reaches `w`'s GOT slot, and that slot is left unmapped so the
/// load through it stays opaque and both arms of the test survive.
#[test]
fn a_weak_undefined_symbols_got_slot_stays_unmapped() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0x48, 0x83, 0x3d, 0, 0, 0, 0, 0, 0xc3], 1);
    defined(&mut obj, "f", text, 0);
    let w = weak_undefined(&mut obj, "w");
    reloc(&mut obj, text, 3, w, -5, elf::R_X86_64_GOTPCREL);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let mut whole = [0u8; 9];
    table
        .read_exact(insn, &mut whole)
        .expect("the instruction still decodes");
    let disp = word(&table, insn + 3, false) as i32;
    let slot = (insn + 8).wrapping_add_signed(i64::from(disp));
    assert_ne!(slot, insn + 8, "the displacement was computed");
    assert!(
        table.read(slot, &mut [0u8; 8]).is_none(),
        "a weak undefined symbol's slot holds no address"
    );
}

/// `-mcmodel=large -fPIC`: `movabs $_GLOBAL_OFFSET_TABLE_, %rcx` plus
/// `movabs $g@GOT, %rax` address the slot together, the second being
/// `G - GOT + A` in eight bytes.
#[test]
fn x86_64_got64_and_gotpc64_address_the_slot_together() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    // lea -7(%rip), %rax ; movabs $0, %rcx ; add %rax, %rcx ; movabs $0, %rax
    let mut code = vec![0x48, 0x8d, 0x05, 0xf9, 0xff, 0xff, 0xff];
    code.extend_from_slice(&[0x48, 0xb9]);
    code.extend_from_slice(&[0u8; 8]);
    code.extend_from_slice(&[0x48, 0x01, 0xc1, 0x48, 0xb8]);
    code.extend_from_slice(&[0u8; 8]);
    obj.append_section_data(text, &code, 1);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 8], 8);
    let got = undefined(&mut obj, "_GLOBAL_OFFSET_TABLE_");
    let g = defined(&mut obj, "g", data, 0);
    reloc(&mut obj, text, 9, got, 9, elf::R_X86_64_GOTPC64);
    reloc(&mut obj, text, 0x16, g, 0, elf::R_X86_64_GOT64);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let dword = |addr: u64| -> u64 {
        let mut buf = [0u8; 8];
        table.read_exact(addr, &mut buf).expect("mapped");
        u64::from_le_bytes(buf)
    };
    // `rax` holds the instruction's own address, the `lea`'s -7 from its end.
    let got_base = insn.wrapping_add(dword(insn + 9));
    let slot = got_base.wrapping_add(dword(insn + 0x16));
    let mut held = [0u8; 8];
    table.read_exact(slot, &mut held).expect("the GOT slot");
    assert_eq!(u64::from_le_bytes(held), symbol_address(&bytes, "g"));
}

/// `-fno-pic -fno-plt` calls `call *ext_fn@GOT`, a bare disp32 (ModRM
/// `mod = 0, rm = 5`) naming the slot's own address. Through a base register
/// the field is the slot's offset from the GOT: `call *ext_fn@GOT(%ebx)`,
/// `(%ebp)` (`mod = 2, rm = 5`), and, the linkers reading the byte before the
/// field as a ModRM whatever it is, an index-only SIB `(,%ecx,4)` and the
/// disp8 of `movl $ext_fn@GOT, 4(%ebx)`. `SHT_REL`, so the
/// `mov ext_fn@GOT+4, %eax` field holds its addend.
#[test]
fn i386_got32x_without_a_base_register_addresses_the_slot_itself() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::I386, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    // (the bytes before the field, its implicit addend, its type)
    let insns: [(&[u8], u8, u32); 6] = [
        (&[0xff, 0x15], 0, elf::R_386_GOT32X),
        (&[0xff, 0x93], 0, elf::R_386_GOT32X),
        (&[0xff, 0x95], 0, elf::R_386_GOT32X),
        (&[0x8b, 0x05], 4, elf::R_386_GOT32),
        (&[0xff, 0x14, 0x8d], 0, elf::R_386_GOT32),
        (&[0xc7, 0x43, 0x04], 0, elf::R_386_GOT32),
    ];
    let mut code = Vec::new();
    let mut fields = Vec::new();
    for (head, addend, _) in insns {
        code.extend_from_slice(head);
        fields.push(code.len() as u64);
        code.extend_from_slice(&[addend, 0, 0, 0]);
    }
    code.push(0xc3);
    obj.append_section_data(text, &code, 1);
    defined(&mut obj, "f", text, 0);
    undefined(&mut obj, "_GLOBAL_OFFSET_TABLE_");
    let ext = undefined(&mut obj, "ext_fn");
    for (&field, (_, _, r_type)) in fields.iter().zip(insns) {
        reloc(&mut obj, text, field, ext, 0, r_type);
    }
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let at = |i: usize| u64::from(word(&table, insn + fields[i], false));
    let slot = at(0);
    assert_eq!(
        u64::from(word(&table, slot, false)),
        symbol_address(&bytes, "ext_fn"),
        "the base-less field is the address of a slot holding ext_fn"
    );
    assert_eq!(at(3), slot + 4);
    let got = symbol_address(&bytes, "_GLOBAL_OFFSET_TABLE_");
    for i in [1, 2, 4, 5] {
        assert_eq!(
            got + at(i),
            slot,
            "{:x?}: through a base the field is the slot's offset from the GOT",
            insns[i].0
        );
    }
}

/// `lea ext_fn@GOT, %eax` passes the base-less test, and GNU ld gives it the
/// slot's offset from the GOT where lld gives the slot's address: the field
/// is a hole.
#[test]
fn an_i386_got_lea_is_a_hole() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::I386, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0x8d, 0x05, 0, 0, 0, 0, 0xc3], 1);
    defined(&mut obj, "f", text, 0);
    let ext = undefined(&mut obj, "ext_fn");
    reloc(&mut obj, text, 2, ext, 0, elf::R_386_GOT32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let err = table
        .read_exact(insn + 2, &mut [0u8; 4])
        .expect_err("a GOT lea is a hole")
        .to_string();
    assert!(
        err.contains(&format!("relocation of type {} ", elf::R_386_GOT32)),
        "{err}"
    );
}

/// A field in the first two bytes of its section has no opcode and ModRM
/// before it: the slot's offset from the GOT, whatever the byte before it.
#[test]
fn an_i386_got_field_at_a_sections_start_is_the_slots_offset() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::I386, Endianness::Little);
    let at0 = obj.add_section(Vec::new(), b".rodata.a".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(at0, &[0; 4], 4);
    let at1 = obj.add_section(Vec::new(), b".rodata.b".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(at1, &[0x05, 0, 0, 0, 0], 4);
    undefined(&mut obj, "_GLOBAL_OFFSET_TABLE_");
    let ext = undefined(&mut obj, "ext_fn");
    reloc(&mut obj, at0, 0, ext, 0, elf::R_386_GOT32);
    reloc(&mut obj, at1, 1, ext, 0, elf::R_386_GOT32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let got = symbol_address(&bytes, "_GLOBAL_OFFSET_TABLE_");
    let a = got
        + u64::from(word(
            &table,
            common::section_base(&bytes, ".rodata.a"),
            false,
        ));
    let b = got
        + u64::from(word(
            &table,
            common::section_base(&bytes, ".rodata.b") + 1,
            false,
        ));
    assert_eq!(a, b, "one slot");
    assert_eq!(
        u64::from(word(&table, a, false)),
        symbol_address(&bytes, "ext_fn")
    );
}

/// `ldr x0, :got:foo+16` asks for the slot holding `foo + 16`, which no slot
/// holds; `G + A` lands in the next symbol's slot. Its siblings with an
/// addend are holes too, and `ldr x1, :got:foo` reaches foo's slot.
#[test]
fn aarch64_a_got_reference_with_an_addend_is_a_hole() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    let mut code = Vec::new();
    // ldr x0, 0 ; adrp x0, 0 ; ldr x0, [x0] ; ldr x1, 0 ; ret
    for insn in [
        0x5800_0000u32,
        0x9000_0000,
        0xf940_0000,
        0x5800_0001,
        0xd65f_03c0,
    ] {
        code.extend_from_slice(&insn.to_le_bytes());
    }
    obj.append_section_data(text, &code, 4);
    let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    obj.append_section_data(data, &[0u8; 32], 8);
    let foo = defined(&mut obj, "foo", data, 0);
    reloc(&mut obj, text, 0, foo, 16, elf::R_AARCH64_GOT_LD_PREL19);
    reloc(&mut obj, text, 4, foo, 16, elf::R_AARCH64_ADR_GOT_PAGE);
    reloc(&mut obj, text, 8, foo, 16, elf::R_AARCH64_LD64_GOT_LO12_NC);
    reloc(&mut obj, text, 12, foo, 0, elf::R_AARCH64_GOT_LD_PREL19);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let p = common::section_base(&bytes, ".text");
    for (off, r_type) in [
        (0, elf::R_AARCH64_GOT_LD_PREL19),
        (4, elf::R_AARCH64_ADR_GOT_PAGE),
        (8, elf::R_AARCH64_LD64_GOT_LO12_NC),
    ] {
        let err = table
            .read_exact(p + off, &mut [0u8; 4])
            .expect_err("a GOT reference with an addend is a hole")
            .to_string();
        assert!(
            err.contains(&format!("relocation of type {r_type} ")),
            "+{off}: {err}"
        );
    }
    let ldr = u64::from(word(&table, p + 12, false));
    let slot = (p + 12).wrapping_add_signed(sext((ldr >> 5) & 0x7_ffff, 19) * 4);
    let mut held = [0u8; 8];
    table.read_exact(slot, &mut held).expect("foo's slot");
    assert_eq!(u64::from_le_bytes(held), symbol_address(&bytes, "foo"));
}

/// A writable section is left alone because nothing folds data the program
/// rewrites; a writable EXECUTABLE one is not data. `CodeAndReadOnly` keeps an
/// RWX mapping on purpose (firmware, `ld -N`), so its bytes reach the decoder,
/// and an uncomputed relocation there has to stay a hole rather than hand the
/// decoder a plausible wrong instruction with nothing said about it.
#[test]
fn an_uncomputed_relocation_in_writable_code_is_still_a_hole() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let rwx = obj.add_section(Vec::new(), b".text.rwx".to_vec(), SectionKind::Text);
    obj.append_section_data(rwx, &[0xb8, 0x11, 0x22, 0x33, 0x44, 0xc3], 1);
    // SectionKind::Text is RX; this is the RWX mapping the filter keeps.
    obj.section_mut(rwx).flags = object::SectionFlags::Elf {
        sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR | elf::SHF_WRITE),
    };
    let tls = undefined(&mut obj, "tls_var");
    reloc(&mut obj, rwx, 1, tls, 0, elf::R_X86_64_TPOFF32);
    let bytes = obj.write().expect("write");

    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text.rwx");
    let mut buf = [0u8; 6];
    let err = table
        .read_exact(insn, &mut buf)
        .expect_err("writable code keeps the hole")
        .to_string();
    assert!(err.contains("relocation of type 23"), "got: {err}");
    assert_eq!(
        table.read(insn, &mut buf),
        Some(1),
        "only the opcode before the field is served"
    );
}

/// `fixtures/loader/out/x64_abs32.o`: `R_X86_64_32`'s value zero-extends, so
/// `foo - 0x20000000`, which fits the field only signed, is a hole, and
/// `foo + 0x80000000`, which fits only unsigned, is patched, as GNU ld
/// refuses the one and links the other.
#[test]
fn an_x86_64_abs32_field_takes_only_a_zero_extending_value() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/loader/out/x64_abs32.o");
    let bytes = std::fs::read(&path).expect("run `make -C fixtures/loader`");
    let table = relocated(&bytes);
    let insn = common::section_base(&bytes, ".text");
    let foo = symbol_address(&bytes, "foo");
    assert!(foo < 0x2000_0000, "{foo:#x}: the first value is negative");

    let err = table
        .read_exact(insn + 1, &mut [0u8; 4])
        .expect_err("only fits signed")
        .to_string();
    assert!(
        err.contains(&format!("relocation of type {} ", elf::R_X86_64_32)),
        "{err}"
    );
    assert_eq!(
        u64::from(word(&table, insn + 6, false)),
        foo + 0x8000_0000,
        "fits unsigned"
    );
}

/// A fetch through `ElfFileMemReader` that lands on such a hole names the
/// relocation, as the lookup table does.
#[test]
fn a_fetch_on_an_uncomputed_relocation_names_it() {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0xb8, 0x11, 0x22, 0x33, 0x44, 0xc3], 1);
    let tls = undefined(&mut obj, "tls_var");
    reloc(&mut obj, text, 1, tls, 0, elf::R_X86_64_TPOFF32);
    let bytes = obj.write().expect("write");
    let insn = common::section_base(&bytes, ".text");

    let owned = strider_reader::elf::OwnedElf::parse(bytes).expect("parse");
    let reader = strider_reader::ElfFileMemReader::new(
        &owned,
        strider_reader::elf::RelocationMode::Applied { got_is_rom: false },
    )
    .expect("reader");
    let mut buf = [0u8; 4];
    let err = rsleigh::MemReader::read(
        &reader,
        rsleigh::VnAddr {
            space: rsleigh::VnSpace::RAM,
            off: insn + 1,
        },
        &mut buf,
    )
    .expect_err("a hole")
    .to_string();
    assert!(err.contains("relocation of type 23"), "got: {err}");
}
