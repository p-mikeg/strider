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
/// applied, IRELATIVE being a hole; one naming a symbol is a hole, its index
/// meaning `.dynsym` to the loader and `.symtab` to the link.
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
}

/// Every region's patch-window length, from its `Debug` form.
fn patch_windows(elf: &OwnedElf) -> Vec<usize> {
    elf.regions(RegionSource::Auto, LoadFilter::AllAllocatable, true)
        .unwrap()
        .iter()
        .map(|r| {
            let dbg = format!("{r:?}");
            let n = dbg.split("patch_window: ").nth(1).expect("patch_window");
            n.trim_end_matches(" }").parse().unwrap()
        })
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
        let once = patch_windows(&open(name));
        assert!(once.iter().any(|&n| n != 0), "{name}: some patch");
        assert_eq!(
            patch_windows(&table_twice(name, table, ".comment")),
            once,
            "{name}"
        );
    }
}
