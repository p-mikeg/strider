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
/// pointer as one address, which a caller would hand to the decoder as code.
#[test]
fn an_unaligned_offset_into_the_opd_is_not_a_descriptor() {
    let fx = build_ppc64_opd_elf(1);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let opd = OpdTable::new(&obj).expect("an ELFv1 image with an .opd");
    for skew in [1, 4, 7] {
        assert_eq!(opd.entry_at(fx.descriptor_addr + skew), None, "skew {skew}");
    }
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
    use common::write_object::{defined, reloc};
    use object::write::Object;
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::PowerPc64, Endianness::Big);
    let code = obj.add_section(Vec::new(), b".text.func".to_vec(), SectionKind::Text);
    obj.append_section_data(code, &0x4e80_0020u32.to_be_bytes(), 4);
    let opd_sec = obj.add_section(Vec::new(), b".opd".to_vec(), SectionKind::Data);
    obj.append_section_data(opd_sec, &[0u8; 24], 8);
    let entry = defined(&mut obj, ".L.func", code, 0);
    defined(&mut obj, "func", opd_sec, 0);
    reloc(&mut obj, opd_sec, 0, entry, 0, object::elf::R_PPC64_ADDR64);
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
}

/// A ppc64 ET_REL of `n` descriptors, descriptor `i` entering its code at
/// `4 * i`.
fn opd_object(n: u64) -> Vec<u8> {
    use common::write_object::{defined, reloc};
    use object::write::Object;
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::PowerPc64, Endianness::Big);
    let code = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    let opd = obj.add_section(Vec::new(), b".opd".to_vec(), SectionKind::Data);
    for i in 0..n {
        obj.append_section_data(code, &0x4e80_0020u32.to_be_bytes(), 4);
        obj.append_section_data(opd, &[0u8; 24], 8);
        let entry = defined(&mut obj, &format!(".L.f{i}"), code, 4 * i);
        defined(&mut obj, &format!("f{i}"), opd, 24 * i);
        reloc(&mut obj, opd, 24 * i, entry, 0, object::elf::R_PPC64_ADDR64);
    }
    obj.write().expect("write")
}

/// `bytes` with a header per `(sh_offset, sh_size)` appended, each a copy
/// of `.rela.opd`'s otherwise, in a section header table moved to the end.
fn with_rela_opd_copies(mut bytes: Vec<u8>, copies: &[(u64, u64)]) -> Vec<u8> {
    use object::ObjectSection as _;
    let index = object::File::parse(&bytes[..])
        .unwrap()
        .section_by_name(".rela.opd")
        .unwrap()
        .index()
        .0;
    // Elf64_Ehdr: e_shoff at 0x28, e_shnum at 0x3c; big-endian.
    let shoff = u64::from_be_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
    let shnum = usize::from(u16::from_be_bytes(bytes[0x3c..0x3e].try_into().unwrap()));
    let mut table = bytes[shoff..shoff + 64 * shnum].to_vec();
    let header = table[64 * index..64 * (index + 1)].to_vec();
    for &(offset, size) in copies {
        let mut copy = header.clone();
        copy[0x18..0x20].copy_from_slice(&offset.to_be_bytes());
        copy[0x20..0x28].copy_from_slice(&size.to_be_bytes());
        table.extend_from_slice(&copy);
    }
    let new_shoff = bytes.len().next_multiple_of(8) as u64;
    bytes.resize(new_shoff as usize, 0);
    bytes.extend_from_slice(&table);
    bytes[0x28..0x30].copy_from_slice(&new_shoff.to_be_bytes());
    let shnum = u16::try_from(shnum + copies.len()).unwrap();
    bytes[0x3c..0x3e].copy_from_slice(&shnum.to_be_bytes());
    bytes
}

/// `.rela.opd`'s `(sh_offset, sh_size)`.
fn rela_opd(bytes: &[u8]) -> (u64, u64) {
    use object::ObjectSection as _;
    object::File::parse(bytes)
        .unwrap()
        .section_by_name(".rela.opd")
        .unwrap()
        .file_range()
        .unwrap()
}

/// The `.opd` relocations count each distinct table once against the
/// relocation entry budget. Distinct overlapping slices of one `.rela.opd`
/// that name more entries than the budget, work the slices' count times
/// their length, follow no descriptor; 2000 exact repeats of a 16-entry
/// table, three times the budget if each were counted, are the one table
/// and still resolve every descriptor.
#[test]
fn opd_relocations_are_bounded_by_their_distinct_tables() {
    let bytes = opd_object(64);
    let (off, _) = rela_opd(&bytes);
    let slices: Vec<(u64, u64)> = (0..64u64)
        .flat_map(|a| (1..=64 - a).map(move |b| (off + 24 * a, 24 * b)))
        .collect();
    let bytes = with_rela_opd_copies(bytes, &slices);
    let file = object::File::parse(&bytes[..]).expect("parse");
    assert!(OpdTable::new(&file).is_none(), "past the budget");

    let bytes = opd_object(16);
    let table = rela_opd(&bytes);
    let bytes = with_rela_opd_copies(bytes, &vec![table; 2000]);
    let file = object::File::parse(&bytes[..]).expect("parse");
    let layout = strider_reader::elf::ElfSectionLayout::new(&file);
    let opd = OpdTable::new(&file).expect("exact repeats are one table");
    let f15 = layout.symbol_address(&file.symbol_by_name("f15").unwrap());
    let code = common::section_base(&bytes, ".text");
    assert_eq!(opd.entry_at(f15), Some(code + 60));
}
