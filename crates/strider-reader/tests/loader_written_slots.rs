//! GOT slots whose load-time value only the dynamic loader computes, under
//! `RelocationMode::Applied { got_is_rom: true }`: each is a hole in the ROM
//! view rather than the bytes or the value the file suggests. The images are
//! `fixtures/loader/out/*`, built by `make -C fixtures/loader`.

use object::{
    Object, ObjectSection, ObjectSegment, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::{LoadFilter, RegionSource, RelocationMode};
use strider_reader::{ElfFileMemReader, OwnedElf, ReadOnlyMemory};

const GOT_IS_ROM: RelocationMode = RelocationMode::Applied { got_is_rom: true };

fn open(name: &str) -> OwnedElf {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/loader/out")
        .join(name);
    strider_reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("{}: {e:#} (run `make -C fixtures/loader`)", path.display()))
}

/// The ROM view's `width`-byte word at `addr`, or its refusal.
fn rom_word(reader: &ElfFileMemReader, addr: u64, width: usize, le: bool) -> Result<u64, String> {
    let mut buf = vec![0u8; width];
    ReadOnlyMemory::read(reader, addr, &mut buf).map_err(|e| e.to_string())?;
    Ok(word(&buf, le))
}

/// The fetch view's `width`-byte word at `addr`.
fn fetch_word(reader: &ElfFileMemReader, addr: u64, width: usize, le: bool) -> u64 {
    let mut buf = vec![0u8; width];
    let n = MemReader::read(
        reader,
        VnAddr {
            off: addr,
            space: VnSpace::RAM,
        },
        &mut buf,
    )
    .unwrap_or_else(|e| panic!("fetch {addr:#x}: {e:?}"));
    assert_eq!(n, width, "fetch {addr:#x}");
    word(&buf, le)
}

fn word(bytes: &[u8], le: bool) -> u64 {
    let fold = |acc: u64, &b: &u8| (acc << 8) | u64::from(b);
    if le {
        bytes.iter().rev().fold(0, fold)
    } else {
        bytes.iter().fold(0, fold)
    }
}

fn assert_hole(got: Result<u64, String>, r_type: u32, what: &str) {
    match got {
        Err(e) if e.contains(&format!("relocation of type {r_type} ")) => {}
        other => panic!("{what}: expected a type-{r_type} hole, got {other:?}"),
    }
}

/// `.got`'s address and the file's word `i` of it.
fn got_word(elf: &OwnedElf, i: u64) -> (u64, u64) {
    let obj = elf.checked_file().unwrap();
    let got = obj.section_by_name(".got").expect(".got");
    let w = if obj.is_64() { 8 } else { 4 };
    let off = (i * w) as usize;
    let bytes = &got.data().unwrap()[off..off + w as usize];
    (got.address() + i * w, word(bytes, obj.is_little_endian()))
}

fn dynsym_value(elf: &OwnedElf, name: &str) -> u64 {
    let obj = elf.checked_file().unwrap();
    obj.dynamic_symbols()
        .find(|s| s.name() == Ok(name))
        .unwrap_or_else(|| panic!("{name} in .dynsym"))
        .address()
}

/// The MIPS executable's primary GOT, as `readelf -A` lists it: the lazy
/// resolver and the module pointer, then one global entry per imported
/// symbol, none of which any relocation record covers.
#[test]
fn a_mips_global_got_entry_for_an_imported_symbol_is_a_hole() {
    for name in ["mips32be_got.elf", "mips64le_got.elf"] {
        let elf = open(name);
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let obj = elf.checked_file().unwrap();
        let (w, le) = (if obj.is_64() { 8 } else { 4 }, obj.is_little_endian());
        // ext_fn and ext_var hold 0 in the file, ext_call its lazy stub.
        for (i, sym, file_word) in [
            (2, "ext_fn", 0),
            (3, "ext_var", 0),
            (4, "ext_call", dynsym_value(&elf, "ext_call")),
        ] {
            let (slot, held) = got_word(&elf, i);
            assert_eq!(held, file_word, "{name} {sym}: the fixture's file word");
            assert_hole(
                rom_word(&reader, slot, w, le),
                object::elf::R_MIPS_GLOB_DAT,
                &format!("{name} {sym}"),
            );
            assert_eq!(
                fetch_word(&reader, slot, w, le),
                file_word,
                "{name} {sym}: the writable fetch view serves the file's word"
            );
        }
        // The loader writes the lazy resolver and the module pointer.
        for (i, what) in [(0, "lazy resolver"), (1, "module pointer")] {
            let (slot, _) = got_word(&elf, i);
            assert_hole(
                rom_word(&reader, slot, w, le),
                object::elf::R_MIPS_GLOB_DAT,
                &format!("{name} {what}"),
            );
        }
    }
}

/// The library's global entry names a symbol it defines, which the file
/// already holds at `st_value`: it stays served.
#[test]
fn a_mips_global_got_entry_for_a_defined_symbol_is_served() {
    for name in ["mips32be_got_lib.elf", "mips64le_got_lib.elf"] {
        let elf = open(name);
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let obj = elf.checked_file().unwrap();
        let (w, le) = (if obj.is_64() { 8 } else { 4 }, obj.is_little_endian());
        let (slot, held) = got_word(&elf, 2);
        let ext_var = dynsym_value(&elf, "ext_var");
        assert_eq!(held, ext_var, "{name}: the fixture's file word");
        assert_eq!(rom_word(&reader, slot, w, le), Ok(ext_var), "{name}");
    }
}

/// The sites of the dynamic relocations of type `r_type`, against `sym` when
/// it is given.
fn dynamic_sites(elf: &OwnedElf, r_type: u32, sym: Option<&str>) -> Vec<u64> {
    let obj = elf.checked_file().unwrap();
    let dynsym = obj.dynamic_symbol_table();
    let sites: Vec<u64> = obj
        .dynamic_relocations()
        .expect("dynamic relocations")
        .filter(|(_, r)| {
            r.flags() == RelocationFlags::Elf { r_type }
                && sym.is_none_or(|name| match r.target() {
                    object::RelocationTarget::Symbol(i) => dynsym
                        .as_ref()
                        .and_then(|t| t.symbol_by_index(i).ok())
                        .is_some_and(|s| s.name() == Ok(name)),
                    _ => false,
                })
        })
        .map(|(site, _)| site)
        .collect();
    assert!(
        !sites.is_empty(),
        "an r_type {r_type} relocation against {sym:?}"
    );
    sites
}

fn dynamic_site(elf: &OwnedElf, r_type: u32, sym: Option<&str>) -> u64 {
    let sites = dynamic_sites(elf, r_type, sym);
    assert_eq!(sites.len(), 1, "r_type {r_type} against {sym:?}");
    sites[0]
}

/// `R_*_IRELATIVE` is `indirect(B + A)`: the loader stores what the resolver
/// returns, so the slot is a hole, RELA (the field holds 0) and REL (the
/// field holds the resolver's address) alike: f's `.got` slot and `f_ptr` in
/// `.data.rel.ro`.
#[test]
fn an_irelative_slot_is_a_hole() {
    for (name, r_type, w, file_word) in [
        (
            "x64_ifunc_pie.elf",
            object::elf::R_X86_64_IRELATIVE,
            8,
            Some(0),
        ),
        ("x86_ifunc_pie.elf", object::elf::R_386_IRELATIVE, 4, None),
    ] {
        let elf = open(name);
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let sites = dynamic_sites(&elf, r_type, None);
        assert_eq!(sites.len(), 2, "{name}: f's slot and f_ptr");
        for slot in sites {
            assert_hole(rom_word(&reader, slot, w, true), r_type, name);
            // The REL field's own `B + A` is the resolver's address.
            let resolver = dynsym_or_symtab(&elf, "resolve_f");
            assert_eq!(
                fetch_word(&reader, slot, w, true),
                file_word.unwrap_or(resolver),
                "{name} {slot:#x}: the writable fetch view serves the file's word"
            );
        }
    }
}

fn dynsym_or_symtab(elf: &OwnedElf, name: &str) -> u64 {
    let obj = elf.checked_file().unwrap();
    obj.symbols()
        .chain(obj.dynamic_symbols())
        .find(|s| s.name() == Ok(name))
        .unwrap_or_else(|| panic!("{name}"))
        .address()
}

/// A `GLOB_DAT` against a defined `STT_GNU_IFUNC` symbol, and the
/// `R_X86_64_64` of `f_ptr`: `S` is the resolver, and the loader stores what
/// it returns. A `GLOB_DAT` against a plain function beside them keeps its
/// value.
#[test]
fn a_glob_dat_against_a_defined_ifunc_is_a_hole() {
    let elf = open("x64_ifunc_lib.elf");
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let glob_dat = object::elf::R_X86_64_GLOB_DAT;
    assert_hole(
        rom_word(&reader, dynamic_site(&elf, glob_dat, Some("f")), 8, true),
        glob_dat,
        "f",
    );
    let abs = object::elf::R_X86_64_64;
    assert_hole(
        rom_word(&reader, dynamic_site(&elf, abs, Some("f")), 8, true),
        abs,
        "f_ptr",
    );
    assert_eq!(
        rom_word(&reader, dynamic_site(&elf, glob_dat, Some("h")), 8, true),
        Ok(dynsym_value(&elf, "h")),
        "h is no IFUNC"
    );
}

/// An unlinked object's GOT reference to a defined IFUNC still addresses its
/// synthetic slot, which is left unmapped as a weak undefined symbol's is:
/// the symbol's address there would be the resolver's. `f_ptr`'s
/// `R_X86_64_64` is not computed.
#[test]
fn an_et_rel_got_slot_for_a_defined_ifunc_is_unmapped() {
    let elf = open("x64_ifunc.o");
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let obj = elf.checked_file().unwrap();
    let text = obj.section_by_name(".text").expect(".text");
    let layout = strider_reader::elf::ElfSectionLayout::new(&obj);
    let base = layout.section_base(&text);
    // `jmp *slot(%rip)`, the displacement four bytes from the instruction end.
    let slot_of = |func: &str| {
        let sym = obj.symbol_by_name(func).expect(func);
        let field = base + sym.address() + 4 + 2;
        let disp = fetch_word(&reader, field, 4, true) as u32 as i32;
        (field + 4).wrapping_add_signed(i64::from(disp))
    };
    let f_slot = slot_of("call_f");
    let err = rom_word(&reader, f_slot, 8, true).expect_err("f's slot must be unmapped");
    assert!(err.contains("not mapped"), "{err}");
    let h = base + obj.symbol_by_name("h").expect("h").address();
    assert_eq!(rom_word(&reader, slot_of("call_h"), 8, true), Ok(h));
    let rel_ro = obj.section_by_name(".data.rel.ro").expect(".data.rel.ro");
    assert_hole(
        rom_word(&reader, layout.section_base(&rel_ro), 8, true),
        object::elf::R_X86_64_64,
        "f_ptr",
    );
}

/// In code a relocation against a defined IFUNC keeps the symbol's address,
/// as a FreeBSD kernel's `call memcpy` does: a hole there stops the decoder,
/// and the symbol is how the image names the function it calls. A data word
/// holding the IFUNC is still not computed, in the default view too.
#[test]
fn an_instruction_relocated_against_a_defined_ifunc_names_the_symbol() {
    let elf = open("x64_ifunc_nopic.o");
    let reader =
        ElfFileMemReader::new(&elf, RelocationMode::Applied { got_is_rom: false }).unwrap();
    let obj = elf.checked_file().unwrap();
    let layout = strider_reader::elf::ElfSectionLayout::new(&obj);
    let text = layout.section_base(&obj.section_by_name(".text").expect(".text"));
    let at = |name: &str| text + obj.symbol_by_name(name).expect(name).address();
    let f = at("f");
    // `jmp f`: endbr64, then e9 and the rel32.
    let field = at("call_f") + 5;
    let rel32 = fetch_word(&reader, field, 4, true) as u32 as i32;
    assert_eq!(
        (field + 4).wrapping_add_signed(i64::from(rel32)),
        f,
        "call_f"
    );
    // `mov $f, %eax`: endbr64, then b8 and the imm32.
    assert_eq!(fetch_word(&reader, at("addr_f") + 5, 4, true), f, "addr_f");
    let rodata = layout.section_base(&obj.section_by_name(".rodata").expect(".rodata"));
    assert_hole(
        rom_word(&reader, rodata, 8, true),
        object::elf::R_X86_64_64,
        "f_ptr",
    );
}

/// An unstripped static executable links `.rela.plt` to `.symtab`, there
/// being no `.dynsym`, and its IRELATIVE slots sit in `.got` under `-z now`:
/// they are holes as a dynamic image's are.
#[test]
fn a_static_executables_irelative_slot_is_a_hole() {
    let elf = open("x64_ifunc_static.elf");
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let obj = elf.checked_file().unwrap();
    assert!(obj.dynamic_symbol_table().is_none(), "no .dynsym");
    let irelative = object::elf::R_X86_64_IRELATIVE;
    // `.rela.plt`'s `sh_info` names `.got`, so `object` lists its entries
    // there, the `.data.rel.ro` one included.
    let sites: Vec<u64> = obj
        .section_by_name(".got")
        .expect(".got")
        .relocations()
        .filter(|(_, r)| r.flags() == RelocationFlags::Elf { r_type: irelative })
        .map(|(site, _)| site)
        .collect();
    assert_eq!(sites.len(), 2, "f's slot and f_ptr");
    for slot in sites {
        assert_hole(rom_word(&reader, slot, 8, true), irelative, "static");
    }
}

/// `name`'s bytes with `edit` applied, parsed as an image.
fn patched(name: &str, edit: impl FnOnce(&mut Vec<u8>, &object::File<'_>)) -> OwnedElf {
    let original = open(name);
    let obj = original.checked_file().unwrap();
    let mut bytes = std::fs::read(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/loader/out")
            .join(name),
    )
    .unwrap();
    edit(&mut bytes, &obj);
    OwnedElf::parse(bytes).unwrap()
}

/// The file offset of `.got` word `i` of a big-endian o32 image.
fn got_word_offset(obj: &object::File<'_>, i: u64) -> usize {
    let (off, _) = obj.section_by_name(".got").unwrap().file_range().unwrap();
    (off + 4 * i) as usize
}

/// A defined symbol's global entry holding anything but its `st_value` is
/// one the loader rewrites: glibc keeps such a word under lazy binding and
/// resolves it under BIND_NOW.
#[test]
fn a_mips_global_entry_off_its_st_value_is_a_hole() {
    let ext_var = dynsym_value(&open("mips32be_got_lib.elf"), "ext_var");
    let elf = patched("mips32be_got_lib.elf", |bytes, obj| {
        let at = got_word_offset(obj, 2);
        bytes[at..at + 4].copy_from_slice(&(ext_var as u32 + 4).to_be_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let (slot, _) = got_word(&elf, 2);
    assert_hole(
        rom_word(&reader, slot, 4, false),
        object::elf::R_MIPS_GLOB_DAT,
        "ext_var + 4",
    );
}

/// Without the GNU top bit, `got[1]` is no module pointer and the loader
/// leaves it alone.
#[test]
fn a_mips_second_got_word_without_the_gnu_bit_is_served() {
    let elf = patched("mips32be_got.elf", |bytes, obj| {
        let at = got_word_offset(obj, 1);
        bytes[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let (slot, _) = got_word(&elf, 1);
    assert_eq!(rom_word(&reader, slot, 4, false), Ok(0));
}

/// A dynamic MIPS image missing a tag that places its GOT says nothing about
/// which words the loader writes, so every `.got` word is a hole.
#[test]
fn a_mips_got_without_its_dynamic_tags_is_all_holes() {
    let elf = patched("mips32be_got_lib.elf", |bytes, obj| {
        let (off, size) = obj
            .section_by_name(".dynamic")
            .unwrap()
            .file_range()
            .unwrap();
        let entries = &mut bytes[off as usize..(off + size) as usize];
        let gotsym = entries
            .chunks_exact_mut(8)
            .find(|d| d[..4] == object::elf::DT_MIPS_GOTSYM.to_be_bytes())
            .expect("DT_MIPS_GOTSYM");
        gotsym[..4].copy_from_slice(&object::elf::DT_MIPS_RLD_VERSION.to_be_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let obj = elf.checked_file().unwrap();
    let words = obj.section_by_name(".got").unwrap().size() / 4;
    for i in 0..words {
        let (slot, _) = got_word(&elf, i);
        assert_hole(
            rom_word(&reader, slot, 4, false),
            object::elf::R_MIPS_GLOB_DAT,
            &format!(".got word {i}"),
        );
    }
}

/// The same image with `.got`'s `sh_size` claiming most of the address
/// space: the holes stop where the mapping holding the GOT ends, rather than
/// walking every word the header claims.
#[test]
fn a_mips_got_hole_stops_at_its_mapping_whatever_sh_size_claims() {
    let elf = patched("mips32be_got_lib.elf", |bytes, obj| {
        let (off, size) = obj
            .section_by_name(".dynamic")
            .unwrap()
            .file_range()
            .unwrap();
        let entries = &mut bytes[off as usize..(off + size) as usize];
        let gotsym = entries
            .chunks_exact_mut(8)
            .find(|d| d[..4] == object::elf::DT_MIPS_GOTSYM.to_be_bytes())
            .expect("DT_MIPS_GOTSYM");
        gotsym[..4].copy_from_slice(&object::elf::DT_MIPS_RLD_VERSION.to_be_bytes());
        // Elf32_Ehdr.e_shoff, then Elf32_Shdr.sh_size of `.got`.
        let shoff = u32::from_be_bytes(bytes[0x20..0x24].try_into().unwrap()) as usize;
        let got = obj.section_by_name(".got").unwrap().index().0;
        let at = shoff + got * 40 + 0x14;
        bytes[at..at + 4].copy_from_slice(&0xfff0_0000u32.to_be_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let obj = elf.checked_file().unwrap();
    let got = obj.section_by_name(".got").unwrap().address();
    assert_hole(
        rom_word(&reader, got, 4, false),
        object::elf::R_MIPS_GLOB_DAT,
        ".got word 0",
    );
    // The writable PT_LOAD holding `.got` ends the mapping.
    let end = obj
        .segments()
        .map(|s| (s.address(), s.address() + s.size()))
        .find(|&(lo, hi)| lo <= got && got < hi)
        .unwrap()
        .1;
    let past = rom_word(&reader, end, 4, false).expect_err("past the mapping");
    assert!(past.contains("not mapped"), "{past}");
}

/// `.rela.dyn` of an image whose `.dynsym` the link discarded: its `r_sym`
/// indexes that table, never `.symtab`, so a word naming a symbol is a hole
/// whichever table `sh_link` names. GNU ld 2.38 links it to `.symtab`; the
/// arm64 5.14 kernel's ld left 0, which the patch reproduces.
#[test]
fn a_dynamic_relocation_with_no_dynsym_is_a_hole() {
    let abs64 = object::elf::R_X86_64_64;
    let slots = |elf: &OwnedElf| {
        let obj = elf.checked_file().unwrap();
        assert!(obj.dynamic_symbol_table().is_none(), "no .dynsym");
        let slots = obj.symbol_by_name("slots").expect("slots").address();
        [slots, slots + 8]
    };
    let linked = open("x64_nodynsym.elf");
    let reader = ElfFileMemReader::new(&linked, GOT_IS_ROM).unwrap();
    for slot in slots(&linked) {
        assert_hole(rom_word(&reader, slot, 8, true), abs64, "linked to .symtab");
    }

    let unlinked = patched("x64_nodynsym.elf", |bytes, obj| {
        // Elf64_Ehdr.e_shoff, then Elf64_Shdr.sh_link of `.rela.dyn`.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let rela = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = shoff + rela * 64 + 0x28;
        bytes[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
    });
    let obj = unlinked.checked_file().unwrap();
    assert_eq!(
        obj.dynamic_relocations().map(|r| r.count()),
        Some(2),
        "link 0 walks as the dynamic table"
    );
    let reader = ElfFileMemReader::new(&unlinked, GOT_IS_ROM).unwrap();
    for slot in slots(&unlinked) {
        assert_hole(rom_word(&reader, slot, 8, true), abs64, "linked to 0");
    }
}

/// The PIE with `.data.rel.ro`'s `sh_size` reaching into `.dynamic`, which
/// stays writable: the served range stops where `.dynamic` starts, so none of
/// it is served from the ROM view.
#[test]
fn a_served_sh_size_past_the_next_section_serves_none_of_it() {
    let elf = patched("x64_ifunc_pie.elf", |bytes, obj| {
        // Elf64_Ehdr.e_shoff, then Elf64_Shdr.sh_size of `.data.rel.ro`.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let relro = obj.section_by_name(".data.rel.ro").unwrap().index().0;
        let at = shoff + relro * 64 + 0x20;
        bytes[at..at + 8].copy_from_slice(&0x1000u64.to_le_bytes());
    });
    let obj = elf.checked_file().unwrap();
    let relro = obj.section_by_name(".data.rel.ro").unwrap().address();
    let dynamic = obj.section_by_name(".dynamic").unwrap().address();
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let err = rom_word(&reader, dynamic, 8, true).expect_err(".dynamic is writable");
    assert!(err.contains("not mapped"), "{err}");
    // Served: `f_ptr`, the IFUNC word, is a hole rather than unmapped.
    assert_hole(
        rom_word(&reader, relro, 8, true),
        object::elf::R_X86_64_IRELATIVE,
        ".data.rel.ro",
    );
}

/// `name` with `.rela.dyn`'s `sh_link` set to `link`, which `object`'s
/// dynamic walk then skips, the image keeping its `.dynsym`.
fn relinked(name: &str, link: impl Fn(&object::File<'_>) -> u32) -> OwnedElf {
    patched(name, |bytes, obj| {
        // Elf64_Ehdr.e_shoff, then Elf64_Shdr.sh_link of `.rela.dyn`.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let rela = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = shoff + rela * 64 + 0x28;
        bytes[at..at + 4].copy_from_slice(&link(obj).to_le_bytes());
    })
}

/// A `.rela.dyn` linked to `.symtab` or to nothing in an image that has a
/// `.dynsym`: its entries are walked all the same. One naming no symbol is
/// applied, RELATIVE served and IRELATIVE a hole; one naming a symbol is a
/// hole, its index meaning `.dynsym` to the loader and `.symtab` to the link.
#[test]
fn a_dynamic_relocation_section_off_the_dynsym_is_still_walked() {
    let irelative = object::elf::R_X86_64_IRELATIVE;
    let pie = relinked("x64_ifunc_pie.elf", |_| 0);
    assert_eq!(
        pie.checked_file()
            .unwrap()
            .dynamic_relocations()
            .map(|r| r.count()),
        Some(0),
        "object walks none of it"
    );
    let reader = ElfFileMemReader::new(&pie, GOT_IS_ROM).unwrap();
    for slot in dynamic_sites(&open("x64_ifunc_pie.elf"), irelative, None) {
        assert_hole(rom_word(&reader, slot, 8, true), irelative, "pie, link 0");
    }
    // `p`'s R_X86_64_RELATIVE: B + A, B modelled 0, is `x`'s address.
    let relro = relinked("x64_relro_pie.elf", |_| 0);
    let obj = relro.checked_file().unwrap();
    let p = obj.section_by_name(".data.rel.ro").unwrap().address();
    let x = obj.symbol_by_name("x").unwrap().address();
    let reader = ElfFileMemReader::new(&relro, GOT_IS_ROM).unwrap();
    assert_eq!(rom_word(&reader, p, 8, true), Ok(x), "relro pie, link 0");

    let original = open("x64_ifunc_lib.elf");
    let sites: Vec<(u64, u32)> = original
        .checked_file()
        .unwrap()
        .dynamic_relocations()
        .unwrap()
        .map(|(site, r)| match r.flags() {
            RelocationFlags::Elf { r_type } => (site, r_type),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(sites.len(), 4);
    let symtab = |obj: &object::File<'_>| obj.section_by_name(".symtab").unwrap().index().0 as u32;
    for (lib, what) in [
        (relinked("x64_ifunc_lib.elf", symtab), "lib, link .symtab"),
        (relinked("x64_ifunc_lib.elf", |_| 0), "lib, link 0"),
    ] {
        let reader = ElfFileMemReader::new(&lib, GOT_IS_ROM).unwrap();
        for &(site, r_type) in &sites {
            assert_hole(rom_word(&reader, site, 8, true), r_type, what);
        }
    }
}

/// The same on MIPS, where every such entry is a hole: `p`'s
/// `R_MIPS_REL32` in `.data.rel.ro`, served as `x`'s address while
/// `.rel.dyn` links to `.dynsym`.
#[test]
fn a_mips_dynamic_relocation_section_off_the_dynsym_is_a_hole() {
    let name = "mips32be_relro.elf";
    let original = open(name);
    let obj = original.checked_file().unwrap();
    let p = obj.section_by_name(".data.rel.ro").unwrap().address();
    let reader = ElfFileMemReader::new(&original, GOT_IS_ROM).unwrap();
    assert_eq!(
        rom_word(&reader, p, 4, false),
        Ok(dynsym_value(&original, "x"))
    );

    let unlinked = patched(name, |bytes, obj| {
        // Elf32_Ehdr.e_shoff, then Elf32_Shdr.sh_link of `.rel.dyn`.
        let shoff = u32::from_be_bytes(bytes[0x20..0x24].try_into().unwrap()) as usize;
        let rel = obj.section_by_name(".rel.dyn").unwrap().index().0;
        let at = shoff + rel * 40 + 0x18;
        bytes[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
    });
    let reader = ElfFileMemReader::new(&unlinked, GOT_IS_ROM).unwrap();
    assert_hole(
        rom_word(&reader, p, 4, false),
        object::elf::R_MIPS_REL32,
        "link 0",
    );
    // The table's `R_MIPS_NONE` at 0 writes nothing: the ELF header stays.
    assert_eq!(rom_word(&reader, 0, 4, false), Ok(0x7f45_4c46));
}

/// How many patches can overlap each region of the ROM view, which holds a
/// served slot's holes too.
fn patch_counts(elf: &OwnedElf) -> Vec<usize> {
    elf.regions(
        RegionSource::Auto,
        LoadFilter::ImmutableAndRelocatedGot,
        true,
    )
    .unwrap()
    .iter()
    .map(|r| r.patch_count())
    .collect()
}

/// `name` with the header of section `over` replaced by a copy of
/// `table`'s, so two headers name one relocation table.
fn table_twice(name: &str, table: &str, over: &str) -> OwnedElf {
    patched(name, |bytes, obj| {
        // Elf64_Ehdr.e_shoff; each Elf64_Shdr is 64 bytes.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let at = |name: &str| shoff + obj.section_by_name(name).unwrap().index().0 * 64;
        let (from, to) = (at(table), at(over));
        bytes.copy_within(from..from + 64, to);
    })
}

/// A relocation table two section headers name is walked once: the patch
/// lists are those of the image with one header, for the dynamic table and
/// for an object's per-section one.
#[test]
fn a_relocation_table_named_twice_is_walked_once() {
    for (name, table) in [
        ("x64_ifunc_lib.elf", ".rela.dyn"),
        ("x64_ifunc.o", ".rela.text"),
    ] {
        let once = patch_counts(&open(name));
        assert!(once.iter().any(|&n| n != 0), "{name}: some patch");
        assert_eq!(
            patch_counts(&table_twice(name, table, ".comment")),
            once,
            "{name}"
        );
    }
}

/// `fixtures/out/x86/elf_relocs.elf` (lld) ends its relro PT_LOAD with
/// `.got` and maps `.got.plt` from the next. Patched: `.got`'s `sh_size`
/// reaches past `.got.plt`, and `.got.plt` loses `SHF_ALLOC`, so no section
/// start bounds the served range: the PT_LOAD holding `.got` does.
#[test]
fn a_served_range_ends_with_its_mapping() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x86/elf_relocs.elf");
    let mut bytes = std::fs::read(&path).expect("run `make -C fixtures`");
    let (got, got_plt) = {
        let obj = object::File::parse(&*bytes).unwrap();
        let index = |name: &str| obj.section_by_name(name).unwrap().index().0;
        let got_plt = obj.section_by_name(".got.plt").unwrap().address();
        (index(".got"), (index(".got.plt"), got_plt))
    };
    // Elf32_Ehdr.e_shoff; Elf32_Shdr is 40 bytes, sh_flags at 8, sh_size at
    // 0x14.
    let shoff = u32::from_le_bytes(bytes[0x20..0x24].try_into().unwrap()) as usize;
    let at = shoff + got * 40 + 0x14;
    bytes[at..at + 4].copy_from_slice(&0x2000u32.to_le_bytes());
    let at = shoff + got_plt.0 * 40 + 8;
    bytes[at..at + 4].copy_from_slice(&object::elf::SHF_WRITE.to_le_bytes());
    let elf = OwnedElf::parse(bytes).unwrap();
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let err = rom_word(&reader, got_plt.1, 4, true).expect_err(".got.plt is writable");
    assert!(err.contains("not mapped"), "{err}");
}

/// `x64_nodynsym.elf`'s second `R_X86_64_64` moved to the last 4 bytes of
/// its read-only mapping: the 8-byte field runs past the mapping, and the
/// hole still covers the 4 bytes mapped.
#[test]
fn a_hole_past_a_mappings_end_covers_what_is_mapped() {
    let end = |obj: &object::File<'_>| {
        let rodata = obj.section_by_name(".rodata").unwrap().address();
        obj.segments()
            .map(|s| (s.address(), s.address() + s.size()))
            .find(|&(lo, hi)| lo <= rodata && rodata < hi)
            .unwrap()
            .1
    };
    let elf = patched("x64_nodynsym.elf", |bytes, obj| {
        // The second Elf64_Rela's r_offset.
        let (off, _) = obj
            .section_by_name(".rela.dyn")
            .unwrap()
            .file_range()
            .unwrap();
        let at = off as usize + 24;
        bytes[at..at + 8].copy_from_slice(&(end(obj) - 4).to_le_bytes());
    });
    let field = end(&elf.checked_file().unwrap()) - 4;
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    assert_hole(
        rom_word(&reader, field, 4, true),
        object::elf::R_X86_64_64,
        "the mapped half",
    );
}

/// `x64_nodynsym.elf`'s first entry retyped `R_X86_64_64` against no symbol:
/// a word the walk does not compute is a hole over the whole word, as one
/// naming a symbol is.
#[test]
fn an_uncomputed_word_off_the_dynsym_is_a_hole_over_the_word() {
    let elf = patched("x64_nodynsym.elf", |bytes, obj| {
        // The first Elf64_Rela's r_info: r_sym 0, R_X86_64_64.
        let (off, _) = obj
            .section_by_name(".rela.dyn")
            .unwrap()
            .file_range()
            .unwrap();
        let at = off as usize + 8;
        bytes[at..at + 8].copy_from_slice(&u64::from(object::elf::R_X86_64_64).to_le_bytes());
    });
    let slots = elf
        .checked_file()
        .unwrap()
        .symbol_by_name("slots")
        .unwrap()
        .address();
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    for half in [slots, slots + 4] {
        assert_hole(
            rom_word(&reader, half, 4, true),
            object::elf::R_X86_64_64,
            &format!("{half:#x}"),
        );
    }
}

/// The lib with three more headers over `.rela.dyn`, which differ from it
/// only in `sh_link` (0, `.symtab`, `.dynstr`): which table the entries'
/// indices mean is in doubt, so every symbol entry is a hole, as in the
/// image relinked to 0, and the table is walked once.
#[test]
fn a_relocation_table_named_under_several_links_is_walked_once_unresolved() {
    let crafted = patched("x64_ifunc_lib.elf", |bytes, obj| {
        // Elf64_Ehdr.e_shoff; each Elf64_Shdr is 64 bytes, sh_link at 0x28.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let at = |name: &str| shoff + obj.section_by_name(name).unwrap().index().0 * 64;
        let symtab = obj.section_by_name(".symtab").unwrap().index().0 as u32;
        let dynstr = obj.section_by_name(".dynstr").unwrap().index().0 as u32;
        let table = at(".rela.dyn");
        for (over, link) in [
            (".comment", 0),
            (".note.gnu.property", symtab),
            (".eh_frame_hdr", dynstr),
        ] {
            let to = at(over);
            bytes.copy_within(table..table + 64, to);
            bytes[to + 0x28..to + 0x2c].copy_from_slice(&link.to_le_bytes());
        }
    });
    let unlinked = relinked("x64_ifunc_lib.elf", |_| 0);
    assert_eq!(patch_counts(&crafted), patch_counts(&unlinked));
    let reader = ElfFileMemReader::new(&crafted, GOT_IS_ROM).unwrap();
    for (site, r) in open("x64_ifunc_lib.elf")
        .checked_file()
        .unwrap()
        .dynamic_relocations()
        .unwrap()
    {
        let RelocationFlags::Elf { r_type } = r.flags() else {
            unreachable!()
        };
        assert_hole(
            rom_word(&reader, site, 8, true),
            r_type,
            &format!("{site:#x}"),
        );
    }
}

/// Elf64_Shdr fields of a section header copied from `.rela.dyn` over the
/// header of `over`.
struct TableCopy {
    over: &'static str,
    link: Option<u32>,
    flags: Option<u64>,
    /// `(sh_offset, sh_size)` in place of `.rela.dyn`'s.
    extent: Option<(u64, u64)>,
}

/// `name` with each of `copies` written over a section header.
fn table_copies(name: &str, copies: &[TableCopy]) -> OwnedElf {
    patched(name, |bytes, obj| {
        // Elf64_Ehdr.e_shoff; an Elf64_Shdr is 64 bytes: sh_flags at 8,
        // sh_offset at 0x18, sh_size at 0x20, sh_link at 0x28.
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let at = |name: &str| shoff + obj.section_by_name(name).unwrap().index().0 * 64;
        let table = at(".rela.dyn");
        for c in copies {
            let to = at(c.over);
            bytes.copy_within(table..table + 64, to);
            let mut put = |field: usize, v: u64, n: usize| {
                bytes[to + field..to + field + n].copy_from_slice(&v.to_le_bytes()[..n]);
            };
            if let Some((offset, size)) = c.extent {
                put(0x18, offset, 8);
                put(0x20, size, 8);
            }
            if let Some(link) = c.link {
                put(0x28, link.into(), 4);
            }
            if let Some(flags) = c.flags {
                put(8, flags, 8);
            }
        }
    })
}

/// A header nothing walks, not `SHF_ALLOC` and linked to `.symtab`, over
/// `.rela.dyn`'s table at a lower index: the table is still walked through
/// `.dynsym`, so `h`'s and `cpu_flag`'s slots hold their addresses.
#[test]
fn an_unwalked_header_over_a_table_does_not_displace_it() {
    let elf = open("x64_ifunc_lib.elf");
    let symtab = elf
        .checked_file()
        .unwrap()
        .section_by_name(".symtab")
        .unwrap()
        .index()
        .0 as u32;
    let decoyed = table_copies(
        "x64_ifunc_lib.elf",
        &[TableCopy {
            over: ".gnu.hash",
            link: Some(symtab),
            flags: Some(0),
            extent: None,
        }],
    );
    let reader = ElfFileMemReader::new(&decoyed, GOT_IS_ROM).unwrap();
    let glob_dat = object::elf::R_X86_64_GLOB_DAT;
    for name in ["h", "cpu_flag"] {
        assert_eq!(
            rom_word(&reader, dynamic_site(&elf, glob_dat, Some(name)), 8, true),
            Ok(dynsym_value(&elf, name)),
            "{name}"
        );
    }
}

/// A header at `.rela.dyn`'s second entry whose `sh_size` runs past the end
/// of the file: no walk reads it, and `.rela.dyn` is walked as before, so
/// `h`'s and `cpu_flag`'s slots hold their addresses.
#[test]
fn a_header_past_the_file_end_does_not_hide_a_table() {
    let elf = open("x64_ifunc_lib.elf");
    let (offset, _) = elf
        .checked_file()
        .unwrap()
        .section_by_name(".rela.dyn")
        .unwrap()
        .file_range()
        .unwrap();
    let decoyed = table_copies(
        "x64_ifunc_lib.elf",
        &[TableCopy {
            over: ".comment",
            link: None,
            flags: None,
            extent: Some((offset + 24, 1 << 40)),
        }],
    );
    let reader = ElfFileMemReader::new(&decoyed, GOT_IS_ROM).unwrap();
    let glob_dat = object::elf::R_X86_64_GLOB_DAT;
    for name in ["h", "cpu_flag"] {
        assert_eq!(
            rom_word(&reader, dynamic_site(&elf, glob_dat, Some(name)), 8, true),
            Ok(dynsym_value(&elf, name)),
            "{name}"
        );
    }
}

/// The lib with `.rela.dyn` copied over five more headers, each from its
/// own offset near the file's start: a crafted image whose
/// headers name more entries than twice what the file holds at 16 bytes an
/// entry is refused; one at that bound loads.
#[test]
fn headers_naming_more_entries_than_the_file_holds_are_refused() {
    let len = std::fs::metadata(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/loader/out/x64_ifunc_lib.elf"),
    )
    .unwrap()
    .len();
    let budget = 2 * len / 16;
    // `.rela.dyn`'s own 4 entries, then five copies that bring the total to
    // `total`.
    let with_total = |total: u64| {
        let mut left = total - 4;
        let copies = [
            ".gnu.hash",
            ".note.gnu.property",
            ".eh_frame_hdr",
            ".eh_frame",
            ".comment",
        ]
        .into_iter()
        .zip(0u64..)
        .map(|(over, i)| {
            // Each from its own entry, so no two repeat one table.
            let n = left.min((len - 24 * i) / 24);
            left -= n;
            TableCopy {
                over,
                link: None,
                flags: None,
                extent: Some((24 * i, 24 * n)),
            }
        })
        .collect::<Vec<_>>();
        assert_eq!(left, 0, "{total} entries fit the headers");
        let elf = table_copies("x64_ifunc_lib.elf", &copies);
        ElfFileMemReader::new(&elf, GOT_IS_ROM).map(|_| ())
    };
    with_total(budget).expect("at the bound");
    let err = with_total(budget + 1).expect_err("past the bound");
    assert!(format!("{err:#}").contains("relocation entries"), "{err:#}");
}

/// `x64_nodynsym.elf` with its text PT_LOAD grown to end where `.rodata`'s
/// starts, and the first `R_X86_64_64` moved 4 bytes before that boundary:
/// the field spans both mappings, and the hole covers it in each.
#[test]
fn a_hole_over_two_adjacent_mappings_covers_both() {
    let boundary = 0x11000u64;
    let elf = patched("x64_nodynsym.elf", |bytes, obj| {
        // The first Elf64_Phdr's p_filesz and p_memsz, at e_phoff + 0x20.
        let phoff = u64::from_le_bytes(bytes[0x20..0x28].try_into().unwrap()) as usize;
        let text = obj.section_by_name(".text").unwrap().address();
        let len = boundary - text;
        for field in [0x20, 0x28] {
            bytes[phoff + field..phoff + field + 8].copy_from_slice(&len.to_le_bytes());
        }
        // The first Elf64_Rela's r_offset.
        let (off, _) = obj
            .section_by_name(".rela.dyn")
            .unwrap()
            .file_range()
            .unwrap();
        let at = off as usize;
        bytes[at..at + 8].copy_from_slice(&(boundary - 4).to_le_bytes());
    });
    let obj = elf.checked_file().unwrap();
    assert_eq!(obj.section_by_name(".rodata").unwrap().address(), boundary);
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    for half in [boundary - 4, boundary] {
        assert_hole(
            rom_word(&reader, half, 4, true),
            object::elf::R_X86_64_64,
            &format!("{half:#x}"),
        );
    }
}

/// An `R_*_COPY` object, which `ld.so` fills from the library defining it:
/// every byte of it is a hole, not only its first word. The file holds zeros
/// where `tbl[3]` reads 4 at run time.
#[test]
fn a_copy_relocated_object_is_a_hole_over_its_whole_size() {
    for (name, r_type) in [
        ("x64_copy_pie.elf", object::elf::R_X86_64_COPY),
        ("aarch64_copy.elf", object::elf::R_AARCH64_COPY),
    ] {
        let elf = open(name);
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let site = dynamic_site(&elf, r_type, Some("tbl"));
        assert_eq!(site, dynsym_value(&elf, "tbl"), "{name}: the copy is tbl");
        assert_eq!(fetch_word(&reader, site + 12, 4, true), 0, "{name}: file");
        for off in (0..32).step_by(4) {
            assert_hole(
                rom_word(&reader, site + off, 4, true),
                r_type,
                &format!("{name} tbl+{off}"),
            );
        }
    }
}

/// lld's `--pack-dyn-relocs=android` table (`SHT_ANDROID_RELA`), which no
/// walk here decodes: the ROM serves no relocated slot, where each of
/// `ptrs`'s words would read the file's 0. Its unpacked twin serves them.
#[test]
fn an_android_packed_image_serves_no_relocated_slot() {
    let plain = open("aarch64_plain_rela.elf");
    let (ptrs, g1) = (
        dynsym_or_symtab(&plain, "ptrs"),
        dynsym_or_symtab(&plain, "g1"),
    );
    let reader = ElfFileMemReader::new(&plain, GOT_IS_ROM).unwrap();
    assert_eq!(rom_word(&reader, ptrs, 8, true), Ok(g1), "plain ptrs[0]");
    let packed = open("aarch64_android_rela.elf");
    let ptrs = dynsym_or_symtab(&packed, "ptrs");
    let reader = ElfFileMemReader::new(&packed, GOT_IS_ROM).unwrap();
    for i in 0..4 {
        let got = rom_word(&reader, ptrs + 8 * i, 8, true);
        assert!(got.is_err(), "packed ptrs[{i}]: {got:?}");
    }
}

/// Either sign of a packed table is enough: the image with its section
/// type rewritten to `SHT_PROGBITS` (the tags alone), and the one with its
/// `DT_ANDROID_RELA` / `DT_ANDROID_RELASZ` rewritten to `DT_DEBUG` (the
/// section alone).
#[test]
fn either_sign_of_an_android_packed_table_serves_no_relocated_slot() {
    let name = "aarch64_android_rela.elf";
    let tags_only = patched(name, |bytes, obj| {
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let index = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = shoff + 64 * index + 4;
        bytes[at..at + 4].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
    });
    let section_only = patched(name, |bytes, obj| {
        let (off, size) = obj
            .section_by_name(".dynamic")
            .unwrap()
            .file_range()
            .unwrap();
        for at in (off as usize..(off + size) as usize).step_by(16) {
            let tag = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
            if matches!(tag, 0x6000_0011 | 0x6000_0012) {
                let debug = u64::from(object::elf::DT_DEBUG);
                bytes[at..at + 8].copy_from_slice(&debug.to_le_bytes());
            }
        }
    });
    for (what, elf) in [("tags only", tags_only), ("section only", section_only)] {
        let ptrs = dynsym_or_symtab(&elf, "ptrs");
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let got = rom_word(&reader, ptrs, 8, true);
        assert!(got.is_err(), "{what}: {got:?}");
    }
}

/// `getl`'s and `geti`'s `mov 0x0,%eax` operands, the twin's `R_386_32`
/// sites against `loc` and `imp`.
fn textrel_sites(twin: &OwnedElf) -> (u64, u64) {
    let r_386_32 = object::elf::R_386_32;
    (
        dynamic_site(twin, r_386_32, Some("loc")),
        dynamic_site(twin, r_386_32, Some("imp")),
    )
}

const APPLIED: RelocationMode = RelocationMode::Applied { got_is_rom: false };

/// `llvm-objcopy --strip-sections` leaves `ld.so` its `PT_DYNAMIC`, which
/// still names the `.rel.dyn` that relocates code. No walk reads it without
/// the section headers, so a relocating load refuses the image rather than
/// decode `getl` as `return *(int *)0`.
#[test]
fn a_textrel_image_without_section_headers_is_refused() {
    let twin = open("x86_textrel.elf");
    let (loc_site, imp_site) = textrel_sites(&twin);
    let reader = ElfFileMemReader::new(&twin, APPLIED).unwrap();
    let loc = dynsym_value(&twin, "loc");
    assert_eq!(fetch_word(&reader, loc_site, 4, true), loc, "twin getl");
    let mut buf = [0u8; 4];
    let at = VnAddr {
        off: imp_site,
        space: VnSpace::RAM,
    };
    assert!(MemReader::read(&reader, at, &mut buf).is_err(), "twin geti");

    let elf = open("x86_textrel_noshdr.elf");
    assert_eq!(elf.checked_file().unwrap().sections().count(), 0);
    for mode in [APPLIED, GOT_IS_ROM] {
        match ElfFileMemReader::new(&elf, mode) {
            Err(e) => assert!(
                format!("{e:#}").contains("no section header covers"),
                "{e:#}"
            ),
            Ok(reader) => panic!(
                "{mode:?}: getl's operand served {:#x}",
                fetch_word(&reader, loc_site, 4, true)
            ),
        }
    }
    let unapplied = ElfFileMemReader::new(&elf, RelocationMode::Unapplied).unwrap();
    assert_eq!(fetch_word(&unapplied, loc_site, 4, true), 0, "file bytes");
}

/// The stripped image with `dropped` tags rewritten to `DT_DEBUG`, and with
/// `rwx` its code `PT_LOAD` made writable too.
fn textrel_noshdr_patched(dropped: &'static [u32], rwx: bool) -> OwnedElf {
    patched("x86_textrel_noshdr.elf", |bytes, _| {
        let phoff = u32::from_le_bytes(bytes[0x1c..0x20].try_into().unwrap()) as usize;
        let phnum = usize::from(u16::from_le_bytes(bytes[0x2c..0x2e].try_into().unwrap()));
        let mut dynamic = None;
        for ph in (0..phnum).map(|i| phoff + 32 * i) {
            let p_type = u32::from_le_bytes(bytes[ph..ph + 4].try_into().unwrap());
            let p_flags = u32::from_le_bytes(bytes[ph + 24..ph + 28].try_into().unwrap());
            if rwx && p_type == object::elf::PT_LOAD && p_flags & object::elf::PF_X != 0 {
                let rwx = object::elf::PF_R | object::elf::PF_W | object::elf::PF_X;
                bytes[ph + 24..ph + 28].copy_from_slice(&rwx.to_le_bytes());
            }
            if p_type == object::elf::PT_DYNAMIC {
                let off = u32::from_le_bytes(bytes[ph + 4..ph + 8].try_into().unwrap());
                let size = u32::from_le_bytes(bytes[ph + 16..ph + 20].try_into().unwrap());
                dynamic = Some((off as usize, size as usize));
            }
        }
        let (off, size) = dynamic.expect("PT_DYNAMIC");
        for at in (off..off + size).step_by(8) {
            let tag = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            if dropped.contains(&tag) {
                bytes[at..at + 4].copy_from_slice(&object::elf::DT_DEBUG.to_le_bytes());
            }
        }
    })
}

/// Each sign that the loader relocates code is enough on its own:
/// `DT_TEXTREL`, `DF_TEXTREL` in `DT_FLAGS`, and a writable and executable
/// `PT_LOAD`, each left alone by rewriting the others away.
#[test]
fn each_sign_of_relocated_code_without_section_headers_is_refused() {
    use object::elf::{DT_FLAGS, DT_TEXTREL};
    let (loc_site, _) = textrel_sites(&open("x86_textrel.elf"));
    for (what, elf) in [
        ("DT_TEXTREL", textrel_noshdr_patched(&[DT_FLAGS], false)),
        ("DF_TEXTREL", textrel_noshdr_patched(&[DT_TEXTREL], false)),
        ("RWX", textrel_noshdr_patched(&[DT_TEXTREL, DT_FLAGS], true)),
    ] {
        match ElfFileMemReader::new(&elf, APPLIED) {
            Err(e) => assert!(
                format!("{e:#}").contains("no section header covers"),
                "{what}: {e:#}"
            ),
            Ok(reader) => panic!(
                "{what}: getl's operand served {:#x}",
                fetch_word(&reader, loc_site, 4, true)
            ),
        }
    }
    let neither = textrel_noshdr_patched(&[DT_TEXTREL, DT_FLAGS], false);
    assert!(
        ElfFileMemReader::new(&neither, APPLIED).is_ok(),
        "no code sign"
    );
}

/// `.rela.dyn`'s header rewritten to `SHT_PROGBITS`: no walk reads the
/// `DT_RELA` table, so the ROM serves no `.data.rel.ro`, where `ptrs[0]`
/// would read the file's 0 rather than `g1`.
#[test]
fn a_dynamic_table_no_walked_header_covers_serves_no_relocated_slot() {
    let name = "aarch64_plain_rela.elf";
    let elf = open(name);
    let (ptrs, g1) = (dynsym_or_symtab(&elf, "ptrs"), dynsym_or_symtab(&elf, "g1"));
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    assert_eq!(rom_word(&reader, ptrs, 8, true), Ok(g1), "unpatched");
    let elf = patched(name, |bytes, obj| {
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let index = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = shoff + 64 * index + 4;
        bytes[at..at + 4].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let got = rom_word(&reader, ptrs, 8, true);
    assert!(got.is_err(), "{got:?}");
}

/// `.rela.dyn`'s `sh_offset` moved to the end of the file: the header's
/// `sh_addr` extent is `DT_RELA`'s, but no walk reads the bytes `ld.so` does
/// there, so it covers nothing.
#[test]
fn a_header_off_the_mapped_bytes_covers_no_dynamic_table() {
    let name = "aarch64_plain_rela.elf";
    let ptrs = dynsym_or_symtab(&open(name), "ptrs");
    let elf = patched(name, |bytes, obj| {
        let shoff = u64::from_le_bytes(bytes[0x28..0x30].try_into().unwrap()) as usize;
        let index = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = shoff + 64 * index + 0x18;
        let end = bytes.len() as u64;
        bytes[at..at + 8].copy_from_slice(&end.to_le_bytes());
    });
    let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
    let got = rom_word(&reader, ptrs, 8, true);
    assert!(got.is_err(), "{got:?}");
}

/// The sites of a mips64el image's `.rel.dyn` entries of type `r_type`:
/// `object` reads its `r_info` transposed, so the type is read here, the
/// entry's last byte.
fn mips64el_rel_sites(elf: &OwnedElf, r_type: u32) -> Vec<u64> {
    let obj = elf.checked_file().unwrap();
    let data = obj.section_by_name(".rel.dyn").unwrap().data().unwrap();
    data.chunks_exact(16)
        .filter(|e| u32::from(e[15]) == r_type)
        .map(|e| u64::from_le_bytes(e[..8].try_into().unwrap()))
        .collect()
}

/// Every TLS slot the loader fills is a hole at its full width: a 64-bit TP
/// offset, module id or module offset is 8 bytes, a TLS descriptor two
/// words. The upper half of a negative TP offset served the file's 0.
#[test]
fn a_tls_slot_is_a_hole_over_its_whole_width() {
    use object::elf as e;
    let images: [(&str, &[(u32, u64)]); 8] = [
        (
            "x64_tls.elf",
            &[
                (e::R_X86_64_TPOFF64, 8),
                (e::R_X86_64_DTPMOD64, 8),
                (e::R_X86_64_DTPOFF64, 8),
            ],
        ),
        (
            "x64_tlsdesc.elf",
            &[(e::R_X86_64_TPOFF64, 8), (e::R_X86_64_TLSDESC, 16)],
        ),
        (
            "aarch64_tls.elf",
            &[(e::R_AARCH64_TLS_TPREL, 8), (e::R_AARCH64_TLSDESC, 16)],
        ),
        (
            "aarch64_tls_trad.elf",
            &[
                (e::R_AARCH64_TLS_TPREL, 8),
                (e::R_AARCH64_TLS_DTPMOD, 8),
                (e::R_AARCH64_TLS_DTPREL, 8),
            ],
        ),
        (
            "ppc64le_tls.elf",
            &[
                (e::R_PPC64_TPREL64, 8),
                (e::R_PPC64_DTPMOD64, 8),
                (e::R_PPC64_DTPREL64, 8),
            ],
        ),
        (
            "mips64le_tls.elf",
            &[
                (e::R_MIPS_TLS_TPREL64, 8),
                (e::R_MIPS_TLS_DTPMOD64, 8),
                (e::R_MIPS_TLS_DTPREL64, 8),
            ],
        ),
        (
            "x86_tlsdesc.elf",
            &[(e::R_386_TLS_TPOFF, 4), (e::R_386_TLS_DESC, 8)],
        ),
        (
            "arm_tlsdesc.elf",
            &[(e::R_ARM_TLS_TPOFF32, 4), (e::R_ARM_TLS_DESC, 8)],
        ),
    ];
    for (name, slots) in images {
        let elf = open(name);
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let obj = elf.checked_file().unwrap();
        let mips = obj.architecture() == object::Architecture::Mips64;
        for &(r_type, width) in slots {
            let sites = if mips {
                mips64el_rel_sites(&elf, r_type)
            } else {
                dynamic_sites(&elf, r_type, None)
            };
            assert_eq!(sites.len(), 1, "{name} type {r_type}");
            for off in (0..width).step_by(4) {
                assert_hole(
                    rom_word(&reader, sites[0] + off, 4, obj.is_little_endian()),
                    r_type,
                    &format!("{name} type {r_type} +{off}"),
                );
            }
        }
    }
}

/// An ELF32 image's dynamic tag `tag`.
fn dynamic_tag32(obj: &object::File<'_>, tag: u32) -> u64 {
    let data = obj.section_by_name(".dynamic").unwrap().data().unwrap();
    let le = obj.is_little_endian();
    let read = |b: &[u8]| {
        let b: [u8; 4] = b.try_into().unwrap();
        if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    data.chunks_exact(8)
        .find(|d| read(&d[..4]) == tag)
        .map_or_else(
            || panic!("dynamic tag {tag:#x}"),
            |d| u64::from(read(&d[4..])),
        )
}

/// Under lazy binding the loader writes two GOT header words no relocation
/// covers: ARM's link map and resolver at `DT_PLTGOT + 4` / `+ 8`, the
/// second of which PLT0 jumps through, and the secure-PLT PPC32 pair after
/// `DT_PPC_GOT`. Both are holes; the `_DYNAMIC` word before them is served.
#[test]
fn a_lazy_got_header_word_the_loader_writes_is_a_hole() {
    use object::elf as e;
    for (name, base_tag, r_type) in [
        ("arm_lazy.elf", e::DT_PLTGOT, e::R_ARM_JUMP_SLOT),
        ("ppc32be_lazy.elf", e::DT_PPC_GOT, e::R_PPC_JMP_SLOT),
    ] {
        let elf = open(name);
        let obj = elf.checked_file().unwrap();
        let le = obj.is_little_endian();
        let base = dynamic_tag32(&obj, base_tag);
        let dynamic = obj.section_by_name(".dynamic").unwrap().address();
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        assert_eq!(rom_word(&reader, base, 4, le), Ok(dynamic), "{name} GOT[0]");
        for off in [4, 8] {
            assert_hole(
                rom_word(&reader, base + off, 4, le),
                r_type,
                &format!("{name} +{off}"),
            );
        }
    }
}

/// A `.dynsym`-walked field over two adjacent read-only mappings: the code
/// `PT_LOAD` split at `.eh_frame`, its `PT_NOTE` header rewritten to the
/// `PT_LOAD` of the rest, and the `R_X86_64_RELATIVE`'s `r_offset` moved 4
/// bytes before the split. No one mapping holds the field to patch, so both
/// halves are holes rather than the file's bytes.
#[test]
fn a_field_over_two_adjacent_read_only_mappings_is_a_hole() {
    let elf = patched("x64_relro_pie.elf", |bytes, obj| {
        let split = obj.section_by_name(".eh_frame").unwrap().address();
        let phoff = u64::from_le_bytes(bytes[0x20..0x28].try_into().unwrap()) as usize;
        let phnum = usize::from(u16::from_le_bytes(bytes[0x38..0x3a].try_into().unwrap()));
        let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let phdrs: Vec<usize> = (0..phnum).map(|i| phoff + 56 * i).collect();
        let code = *phdrs
            .iter()
            .find(|&&ph| u32_at(bytes, ph) == object::elf::PT_LOAD && u64_at(bytes, ph + 8) == 0)
            .unwrap();
        let note = *phdrs
            .iter()
            .find(|&&ph| u32_at(bytes, ph) == object::elf::PT_NOTE)
            .unwrap();
        let end = u64_at(bytes, code + 0x20);
        // Elf64_Phdr: p_type, p_flags, p_offset, p_vaddr, p_paddr, p_filesz,
        // p_memsz, p_align.
        for field in [0x20, 0x28] {
            bytes[code + field..code + field + 8].copy_from_slice(&split.to_le_bytes());
        }
        let rest = [
            (0x00, u64::from(object::elf::PT_LOAD), 4),
            (0x04, u64::from(object::elf::PF_R), 4),
            (0x08, split, 8),
            (0x10, split, 8),
            (0x18, split, 8),
            (0x20, end - split, 8),
            (0x28, end - split, 8),
            (0x30, 0x1000, 8),
        ];
        for (field, value, width) in rest {
            bytes[note + field..note + field + width]
                .copy_from_slice(&value.to_le_bytes()[..width]);
        }
        let (off, _) = obj
            .section_by_name(".rela.dyn")
            .unwrap()
            .file_range()
            .unwrap();
        let at = off as usize;
        bytes[at..at + 8].copy_from_slice(&(split - 4).to_le_bytes());
    });
    let split = elf
        .checked_file()
        .unwrap()
        .section_by_name(".eh_frame")
        .unwrap()
        .address();
    let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
    for half in [split - 4, split] {
        assert_hole(
            rom_word(&reader, half, 4, true),
            object::elf::R_X86_64_RELATIVE,
            &format!("{half:#x}"),
        );
    }
}
