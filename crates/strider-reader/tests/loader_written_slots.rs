//! GOT slots whose load-time value only the dynamic loader computes, under
//! `RelocationMode::Applied { got_is_rom: true }`: each is a hole in the ROM
//! view rather than the bytes or the value the file suggests. The images are
//! `fixtures/loader/out/*`, built by `make -C fixtures/loader`.

use object::{Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::RelocationMode;
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
