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

use object::write::{Object, Relocation, Symbol, SymbolId, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags, SymbolKind,
    SymbolScope, elf,
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

fn defined(
    obj: &mut Object<'_>,
    name: &str,
    section: object::write::SectionId,
    value: u64,
) -> SymbolId {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value,
        size: 0,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(section),
        flags: SymbolFlags::None,
    })
}

fn undefined(obj: &mut Object<'_>, name: &str) -> SymbolId {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Unknown,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Undefined,
        flags: SymbolFlags::None,
    })
}

fn reloc(
    obj: &mut Object<'_>,
    section: object::write::SectionId,
    offset: u64,
    symbol: SymbolId,
    addend: i64,
    r_type: u32,
) {
    obj.add_relocation(
        section,
        Relocation {
            offset,
            symbol,
            addend,
            flags: RelocationFlags::Elf { r_type },
        },
    )
    .expect("relocation");
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
