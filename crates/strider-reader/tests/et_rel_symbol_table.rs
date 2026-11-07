//! An object file's relocations index the symbol table its `sh_link` names.
//!
//! `object`'s `dynamic_symbol_table` is the first `SHT_DYNSYM` section in the
//! file whatever the `e_type`, so an `ET_REL` carrying one must still resolve
//! against `.symtab`.
//!
//! The two objects below differ in one section header's `sh_type`
//! (`SHT_STRTAB` vs `SHT_DYNSYM`). Both hold `.text` = `e8 00 00 00 00`
//! (`call rel32`) with an `R_X86_64_PLT32` against `.symtab[1]` = `target`
//! (`.text.target`, `st_value` 0); the decoy table's entry 1 is `st_value`
//! 0x7777 in the same section. They must patch to the same target.

#[path = "common/mod.rs"]
mod common;

use common::write_object::{defined, reloc};
use object::write::Object;
use object::{
    Architecture, BinaryFormat, Endianness, Object as _, ObjectKind, ObjectSymbol as _,
    ObjectSymbolTable as _, SectionIndex, SectionKind, SymbolIndex, elf,
};

/// `.text.target`'s index: `object::write` seats it after `.text` and
/// `.rela.text`.
const TARGET_SHNDX: u16 = 3;

/// The object, with its decoy table's section typed `decoy_type`.
fn object_with_decoy(decoy_type: u32) -> Vec<u8> {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0xe8, 0, 0, 0, 0], 16);
    let target_section = obj.add_section(Vec::new(), b".text.target".to_vec(), SectionKind::Text);
    obj.append_section_data(target_section, &[0xc3], 16);
    let target = defined(&mut obj, "target", target_section, 0);
    reloc(&mut obj, text, 1, target, -4, elf::R_X86_64_PLT32);

    // Two `Elf64_Sym`: the null entry, then a GLOBAL FUNC at 0x7777 in
    // `.text.target`. `object::write` leaves the decoy's `sh_link` and
    // `sh_entsize` 0, which `object` still reads as a symbol table, as
    // `decoy_entry` checks.
    let mut decoy = vec![0u8; 24];
    decoy.extend_from_slice(&0u32.to_le_bytes()); // st_name
    decoy.push((elf::STB_GLOBAL << 4) | elf::STT_FUNC); // st_info
    decoy.push(0); // st_other
    decoy.extend_from_slice(&TARGET_SHNDX.to_le_bytes());
    decoy.extend_from_slice(&0x7777u64.to_le_bytes()); // st_value
    decoy.extend_from_slice(&0u64.to_le_bytes()); // st_size
    let section = obj.add_section(Vec::new(), b".decoy".to_vec(), SectionKind::Elf(decoy_type));
    obj.append_section_data(section, &decoy, 8);
    obj.write().expect("write the object")
}

/// Entry 1 of the table `object` reads as the file's dynamic symbols.
fn decoy_entry(bytes: &[u8]) -> Option<(u64, SectionIndex)> {
    let obj = object::File::parse(bytes).expect("parse");
    let table = obj.dynamic_symbol_table()?;
    let sym = table.symbol_by_index(SymbolIndex(1)).expect("entry 1");
    Some((sym.address(), sym.section_index().expect("defined")))
}

/// The `call`'s rel32 field, one byte into `.text`, read back through the
/// relocating reader.
fn patched_rel32(bytes: &[u8]) -> i32 {
    let field_addr = common::section_base(bytes, ".text") + 1;
    let mut field = [0u8; 4];
    assert!(
        common::load_with_relocations(bytes)
            .iter()
            .any(|r| r.read(field_addr, &mut field) == Some(4)),
        "no region covers the call's rel32 field"
    );
    i32::from_le_bytes(field)
}

#[test]
fn an_object_files_relocations_ignore_a_decoy_dynsym() {
    let without = object_with_decoy(elf::SHT_STRTAB);
    let with = object_with_decoy(elf::SHT_DYNSYM);
    let differing = without.iter().zip(&with).filter(|(a, b)| a != b).count();
    assert_eq!(
        (with.len(), differing),
        (without.len(), 1),
        "the objects differ in the one byte of sh_type"
    );
    let file = object::File::parse(&*with).expect("parse");
    assert_eq!(file.kind(), ObjectKind::Relocatable, "an ET_REL");
    let target = file.symbol_by_name("target").expect("target");
    assert_eq!(target.index(), SymbolIndex(1), "target is .symtab[1]");
    let target_section = target.section_index().expect("defined");
    assert_eq!(
        decoy_entry(&without),
        None,
        "an SHT_STRTAB decoy is no symbol table"
    );
    assert_eq!(
        decoy_entry(&with),
        Some((0x7777, target_section)),
        "the decoy is a dynamic table whose entry 1 sits elsewhere in .text.target"
    );

    let (rel_without, rel_with) = (patched_rel32(&without), patched_rel32(&with));
    assert_eq!(
        rel_with, rel_without,
        "a decoy .dynsym changed which symbol the relocation resolved against"
    );
    // `.text.target` is rebased above `.text`, so the displacement reaches it.
    assert_eq!(rel_without, 0xb);
}
