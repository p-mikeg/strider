//! An object file's relocations index the symbol table its `sh_link` names.
//!
//! `object`'s `dynamic_symbol_table` is the first `SHT_DYNSYM` SECTION in the
//! file, populated whatever the `e_type`. Preferring it meant an `ET_REL`
//! carrying any such section resolved every relocation against the wrong
//! table, patching in an unrelated symbol's `st_value`. The patch is silent,
//! and the attacker chooses both the value and the section it is rebased
//! against.
//!
//! The two objects below differ in one section header's `sh_type`
//! (`SHT_STRTAB` vs `SHT_DYNSYM`). Both hold `.text` = `e8 00 00 00 00`
//! (`call rel32`) with an `R_X86_64_PLT32` against `.symtab[1]` = `target`
//! (`.text.target`, `st_value` 0); the decoy table's entry 1 is `st_value`
//! 0x7777 in the same section. They must patch to the same target.

use object::write::{Object, Relocation, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, Object as _, ObjectSymbol as _, ObjectSymbolTable as _,
    RelocationFlags, SectionIndex, SectionKind, SymbolFlags, SymbolIndex, SymbolKind, SymbolScope,
    elf,
};
use strider_reader::OwnedElf;
use strider_reader::elf::{ElfSectionLayout, LoadFilter, RegionSource};

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
    let target = obj.add_symbol(Symbol {
        name: b"target".to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(target_section),
        flags: SymbolFlags::None,
    });
    obj.add_relocation(
        text,
        Relocation {
            offset: 1,
            symbol: target,
            addend: -4,
            flags: RelocationFlags::Elf {
                r_type: elf::R_X86_64_PLT32,
            },
        },
    )
    .expect("relocation");

    // Two `Elf64_Sym`: the null entry, then a GLOBAL FUNC at 0x7777 in
    // `.text.target`.
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

/// The `call`'s rel32 field, read back through the relocating reader.
fn patched_rel32(bytes: &[u8]) -> i32 {
    let elf = OwnedElf::parse(bytes.to_vec()).expect("parse");
    let obj = elf.checked_file().expect("parse");
    // `.text` is rebased off the synthetic ET_REL image base; the field is the
    // `call`'s rel32, one byte into it.
    let text = obj.section_by_name(".text").expect(".text");
    let field_addr = ElfSectionLayout::new(&obj).section_base(&text) + 1;
    let regions = elf
        .regions(RegionSource::Auto, LoadFilter::AllAllocatable, true)
        .expect("regions");
    let mut field = [0u8; 4];
    assert!(
        regions
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
    let target_section = object::File::parse(&*with)
        .expect("parse")
        .symbol_by_name("target")
        .expect("target")
        .section_index()
        .expect("defined");
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

    let without = patched_rel32(&without);
    let with = patched_rel32(&with);
    assert_eq!(
        with, without,
        "a decoy .dynsym changed which symbol the relocation resolved against"
    );
    // `.text.target` is rebased above `.text`, so the displacement reaches it.
    assert_eq!(without, 0xb);
}
