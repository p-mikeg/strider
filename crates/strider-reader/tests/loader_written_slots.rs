//! GOT slots whose load-time value only the dynamic loader computes, under
//! `RelocationMode::Applied { got_is_rom: true }`: each is a hole in the ROM
//! view rather than the bytes or the value the file suggests. The images are
//! `fixtures/loader/out/*`, built by `make -C fixtures/loader`.

use object::{Object, ObjectSection, ObjectSymbol};
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
