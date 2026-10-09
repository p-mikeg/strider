//! ppc64 ELFv1 `.opd` function descriptors.
//!
//! `st_value` of an `STT_FUNC` symbol there is a descriptor, not code, so
//! decoding from it fails on the first instruction. The entry is recoverable
//! from the descriptor's first doubleword.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::build_ppc64_opd_elf;
use object::{Object as _, ObjectSymbol as _};
use strider_reader::elf::OpdTable;

#[test]
fn an_elfv1_function_symbol_resolves_through_its_descriptor() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let sym = obj.symbol_by_name("func").expect("func");
    assert_eq!(
        sym.address(),
        fx.descriptor_addr,
        "an ELFv1 st_value is the descriptor"
    );

    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    assert_eq!(opd.entry_at(sym.address()), Some(fx.entry_addr));
}

/// The unspecified ABI level is ELFv1, which is what binutils reads it as and
/// what an image carrying an `.opd` at all has to be.
#[test]
fn an_unspecified_abi_level_still_follows_descriptors() {
    let fx = build_ppc64_opd_elf(0);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("ABI level 0 is ELFv1");
    assert_eq!(opd.entry_at(fx.descriptor_addr), Some(fx.entry_addr));
}

/// ELFv2 has no descriptors: `st_value` is the code address, and following
/// bytes at it as one would corrupt every symbol.
#[test]
fn an_elfv2_image_has_no_descriptor_table() {
    let fx = build_ppc64_opd_elf(2);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    assert!(OpdTable::new(&obj).is_none());
}

#[test]
fn an_address_outside_the_opd_is_not_a_descriptor() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    assert_eq!(opd.entry_at(fx.entry_addr), None, "a code address");
    assert_eq!(opd.entry_at(0), None, "below the table");
    assert_eq!(
        opd.entry_at(fx.descriptor_addr + 24),
        None,
        "past the last descriptor"
    );
}

/// An offset inside a descriptor word reads half the entry and half the TOC
/// pointer as one address, and `function_entry` would hand that to the decoder
/// as code.
#[test]
fn an_unaligned_offset_into_the_opd_is_not_a_descriptor() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    for skew in [1, 4, 7] {
        assert_eq!(opd.entry_at(fx.descriptor_addr + skew), None, "skew {skew}");
    }

    let addr = fx.descriptor_addr + 4;
    let elf = strider_reader::OwnedElf::parse(fx.bytes).expect("parse");
    assert_eq!(
        elf.function_entry(addr).expect("unaligned"),
        addr,
        "an address that is not a descriptor word passes through"
    );
}

/// The one-call form, for a caller resolving a single address.
#[test]
fn function_entry_follows_a_descriptor_and_passes_code_through() {
    let fx = build_ppc64_opd_elf(1);
    let elf = strider_reader::OwnedElf::parse(fx.bytes).expect("parse");
    assert_eq!(
        elf.function_entry(fx.descriptor_addr).expect("descriptor"),
        fx.entry_addr
    );
    assert_eq!(
        elf.function_entry(fx.entry_addr).expect("code address"),
        fx.entry_addr
    );
}

/// Off ppc64 there are no descriptors to follow, whatever `e_flags` says.
#[test]
fn a_non_ppc64_image_has_no_descriptor_table() {
    let bytes = common::elf_fixture::simple_text_elf(0x1000, &[0x90; 16]);
    let obj = object::File::parse(&bytes[..]).expect("parse");
    assert!(OpdTable::new(&obj).is_none());
}

/// An ET_DYN's `.opd` words read zero until `ld.so` applies their
/// `R_PPC64_RELATIVE`s. Zero is not an entry, so the address passes through
/// rather than resolving to 0.
#[test]
fn an_unrelocated_descriptor_is_not_an_entry() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    assert_eq!(opd.entry_at(fx.unrelocated_addr), None);

    let addr = fx.unrelocated_addr;
    let elf = strider_reader::OwnedElf::parse(fx.bytes).expect("parse");
    assert_eq!(elf.function_entry(addr).expect("unrelocated"), addr);
}

/// The descriptor's TOC word is 8-byte aligned like its entry word, and a
/// symbol landing on one is followed the same way: the section carries no
/// stride saying which words start a descriptor.
#[test]
fn a_descriptors_toc_word_is_followed_like_an_entry() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    assert_eq!(opd.entry_at(fx.descriptor_addr + 8), Some(fx.toc_addr));
    assert_eq!(
        opd.entry_at(fx.descriptor_addr + 16),
        None,
        "the environment word reads zero, which is not an entry"
    );
}

/// An ET_REL's `.opd` holds zeros; each descriptor's entry exists only as the
/// `R_PPC64_ADDR64` on its first word, so the table is built out of those.
/// Without it every function symbol of a ppc64 BE object names its descriptor
/// and decodes descriptor bytes.
#[test]
fn an_et_rel_descriptor_resolves_through_its_relocation() {
    use object::write::{Object, Relocation, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags,
        SymbolKind, SymbolScope,
    };

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::PowerPc64, Endianness::Big);
    let code = obj.add_section(Vec::new(), b".text.func".to_vec(), SectionKind::Text);
    obj.append_section_data(code, &0x4e80_0020u32.to_be_bytes(), 4);
    let opd_sec = obj.add_section(Vec::new(), b".opd".to_vec(), SectionKind::Data);
    obj.append_section_data(opd_sec, &[0u8; 24], 8);
    let mut sym = |name: &str, section, value| {
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
    };
    let entry = sym(".L.func", code, 0);
    sym("func", opd_sec, 0);
    obj.add_relocation(
        opd_sec,
        Relocation {
            offset: 0,
            symbol: entry,
            addend: 0,
            flags: RelocationFlags::Elf {
                r_type: object::elf::R_PPC64_ADDR64,
            },
        },
    )
    .expect("relocation");
    let bytes = obj.write().expect("write");

    let file = object::File::parse(&bytes[..]).expect("parse");
    let layout = strider_reader::elf::ElfSectionLayout::new(&file);
    let func = file.symbol_by_name("func").expect("func");
    let descriptor = layout.symbol_address(&func);
    let code_base = common::section_base(&bytes, ".text.func");
    assert_eq!(
        descriptor,
        common::section_base(&bytes, ".opd"),
        "the symbol names the descriptor, not the code"
    );
    assert_ne!(descriptor, code_base);

    let opd = OpdTable::new(&file).expect("an ELFv1 object with an .opd");
    assert_eq!(opd.entry_at(descriptor), Some(code_base));
    assert_eq!(opd.entry_at(descriptor + 8), None, "the TOC word");

    let elf = strider_reader::OwnedElf::parse(bytes).expect("parse");
    assert_eq!(
        elf.function_entry(descriptor).expect("descriptor"),
        code_base
    );
}
