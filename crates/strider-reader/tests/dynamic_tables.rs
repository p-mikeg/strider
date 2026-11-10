//! A linked image's relocations as the dynamic loader finds them: the last
//! `PT_DYNAMIC`, read at its `p_vaddr` through the `PT_LOAD`s, the last of
//! each tag, and the tables those tags place. Section headers name none of
//! it. Each image is a `fixtures/loader/out/*` one with the header patch its
//! test documents.

use object::{Object, ObjectSection, ObjectSymbol};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::RelocationMode;
use strider_reader::{ElfFileMemReader, OwnedElf};

mod common;
use common::{assert_hole, le32, le64, loader_fixture as path, rom_word};

const APPLIED: RelocationMode = RelocationMode::Applied { got_is_rom: false };
const GOT_IS_ROM: RelocationMode = RelocationMode::Applied { got_is_rom: true };

fn open(name: &str) -> OwnedElf {
    strider_reader::load_elf(path(name))
        .unwrap_or_else(|e| panic!("{name}: {e:#} (run `make -C fixtures/loader`)"))
}

fn patched(name: &str, edit: impl FnOnce(&mut Vec<u8>)) -> OwnedElf {
    let mut bytes = std::fs::read(path(name)).unwrap();
    edit(&mut bytes);
    OwnedElf::parse(bytes).unwrap()
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// The fetch view's 4 bytes at `addr`, `None` when it serves them not.
fn fetch_u32(reader: &ElfFileMemReader, addr: u64) -> Option<u32> {
    let mut buf = [0u8; 4];
    let at = VnAddr {
        off: addr,
        space: VnSpace::RAM,
    };
    match MemReader::read(reader, at, &mut buf) {
        Ok(4) => Some(u32::from_le_bytes(buf)),
        _ => None,
    }
}

/// `x86_textrel.elf`'s `R_386_32` sites in `getl` and `geti`, against the
/// defined `loc` and the imported `imp`, and `loc`'s address.
fn textrel() -> (u64, u64, u64) {
    let elf = open("x86_textrel.elf");
    let obj = elf.checked_file().unwrap();
    let mut sites = obj
        .section_by_name(".rel.dyn")
        .unwrap()
        .data()
        .unwrap()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|e| u64::from(le32(e, 0)))
        .collect::<Vec<_>>();
    sites.sort_unstable();
    let loc = obj
        .dynamic_symbols()
        .find(|s| s.name() == Ok("loc"))
        .unwrap()
        .address();
    (sites[0], sites[1], loc)
}

/// The Elf32_Phdr offsets of an ELF32 image.
fn phdrs32(b: &[u8]) -> Vec<usize> {
    let (phoff, phnum) = (le32(b, 0x1c) as usize, usize::from(b[0x2c]));
    (0..phnum).map(|i| phoff + 32 * i).collect()
}

/// The stripped TEXTREL image three ways the section-free loader view must
/// see through, `getl`'s and `geti`'s operands then holes, as nothing names
/// `.dynsym` to resolve `loc` and `imp`:
///
/// * `PT_DYNAMIC`'s `p_offset` past the end of the file;
/// * its `p_offset` at 0x80 bytes of zeros;
/// * a first, decoy `PT_DYNAMIC` over those zeros, and the real one in the
///   `PT_GNU_PROPERTY` header's place after it.
///
/// `ld.so` reads the array at `p_vaddr` and keeps the last `PT_DYNAMIC`, so
/// each image's `.rel.dyn` is still there to read.
#[test]
fn the_dynamic_array_is_read_where_the_loader_reads_it() {
    let (loc_site, imp_site, _) = textrel();
    let dynamic = |b: &[u8]| {
        *phdrs32(b)
            .iter()
            .find(|&&ph| le32(b, ph) == object::elf::PT_DYNAMIC)
            .unwrap()
    };
    let past_eof = patched("x86_textrel_noshdr.elf", |b| {
        let ph = dynamic(b);
        put32(b, ph + 4, 0x7fff_0000);
    });
    let at_zeros = patched("x86_textrel_noshdr.elf", |b| {
        assert!(b[0x400..0x480].iter().all(|&x| x == 0));
        let ph = dynamic(b);
        put32(b, ph + 4, 0x400);
    });
    let decoy_first = patched("x86_textrel_noshdr.elf", |b| {
        let ph = dynamic(b);
        let real: Vec<u8> = b[ph..ph + 32].to_vec();
        put32(b, ph + 4, 0x400);
        put32(b, ph + 8, 0);
        put32(b, ph + 12, 0);
        let property = *phdrs32(b)
            .iter()
            .find(|&&p| le32(b, p) == object::elf::PT_GNU_PROPERTY)
            .unwrap();
        assert!(property > ph);
        b[property..property + 32].copy_from_slice(&real);
    });
    for (what, elf) in [
        ("p_offset past the file", past_eof),
        ("p_offset at zeros", at_zeros),
        ("a decoy PT_DYNAMIC first", decoy_first),
    ] {
        let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
        for (site, sym) in [(loc_site, "loc"), (imp_site, "imp")] {
            assert_eq!(fetch_u32(&reader, site), None, "{what}: {sym}'s operand");
            assert_hole(
                rom_word(&reader, site, 4),
                object::elf::R_386_32,
                &format!("{what}: {sym}"),
            );
        }
    }
}

/// The TEXTREL image with its `.rel.dyn` header retyped, to `SHT_NULL` with
/// a first `DT_RELSZ 0` inserted before the array's other tags, and to
/// `SHT_RELA`, whose 12-byte entries do not tile its 16 bytes. `ld.so`
/// keeps the last `DT_RELSZ` and reads no section header, so `getl`'s
/// operand is `loc`'s address and `geti`'s a hole either way.
#[test]
fn the_tables_are_the_last_tags_whatever_the_headers_say() {
    let (loc_site, imp_site, loc) = textrel();
    let rel_dyn = |b: &[u8]| {
        let obj = object::File::parse(b).unwrap();
        let index = obj.section_by_name(".rel.dyn").unwrap().index().0;
        le32(b, 0x20) as usize + 40 * index
    };
    let dup_relsz = patched("x86_textrel.elf", |b| {
        let at = rel_dyn(b);
        put32(b, at + 4, object::elf::SHT_NULL);
        let obj = object::File::parse(&b[..]).unwrap();
        let (off, size) = obj
            .section_by_name(".dynamic")
            .unwrap()
            .file_range()
            .unwrap();
        let (off, size) = (off as usize, size as usize);
        let n = (0..size / 8).find(|&i| le32(b, off + 8 * i) == 0).unwrap();
        assert!(8 * (n + 2) <= size, "room for one more tag");
        b.copy_within(off..off + 8 * (n + 1), off + 8);
        put32(b, off, object::elf::DT_RELSZ);
        put32(b, off + 4, 0);
    });
    let as_rela = patched("x86_textrel.elf", |b| {
        let at = rel_dyn(b);
        put32(b, at + 4, object::elf::SHT_RELA);
    });
    for (what, elf) in [("a first DT_RELSZ 0", dup_relsz), ("SHT_RELA", as_rela)] {
        let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
        assert_eq!(
            fetch_u32(&reader, loc_site),
            Some(loc as u32),
            "{what}: getl"
        );
        assert_hole(
            rom_word(&reader, imp_site, 4),
            object::elf::R_386_32,
            &format!("{what}: geti"),
        );
    }
}

/// `ptrs`, `g1` and `g2` of `name`.
fn ptrs(name: &str) -> (u64, u64, u64) {
    let elf = open(name);
    let obj = elf.checked_file().unwrap();
    let sym = |n: &str| obj.symbol_by_name(n).unwrap().address();
    (sym("ptrs"), sym("g1"), sym("g2"))
}

/// `ptrs[0..4]` through the ROM view: `{g1, g2, imp, g1}`, the import a hole.
fn assert_ptrs(elf: &OwnedElf, name: &str, what: &str) {
    let (ptrs, g1, g2) = ptrs(name);
    let reader = ElfFileMemReader::new(elf, GOT_IS_ROM).unwrap();
    assert_eq!(rom_word(&reader, ptrs, 8), Ok(g1), "{what}: ptrs[0]");
    assert_eq!(rom_word(&reader, ptrs + 8, 8), Ok(g2), "{what}: ptrs[1]");
    assert_hole(
        rom_word(&reader, ptrs + 16, 8),
        object::elf::R_AARCH64_ABS64,
        &format!("{what}: ptrs[2]"),
    );
    assert_eq!(rom_word(&reader, ptrs + 24, 8), Ok(g1), "{what}: ptrs[3]");
}

/// lld's `--pack-dyn-relocs=android` table decoded where bionic reads it,
/// through `DT_ANDROID_RELA`: the packed image serves `ptrs` as its unpacked
/// twin does, also with `.rela.dyn` retyped `SHT_PROGBITS`, with
/// `PT_DYNAMIC`'s `p_offset` past the end of the file, and with both.
#[test]
fn an_android_packed_table_is_decoded_from_its_tags() {
    let name = "aarch64_android_rela.elf";
    assert_ptrs(
        &open("aarch64_plain_rela.elf"),
        "aarch64_plain_rela.elf",
        "plain",
    );
    let retype = |b: &mut Vec<u8>| {
        let obj = object::File::parse(&b[..]).unwrap();
        let index = obj.section_by_name(".rela.dyn").unwrap().index().0;
        let at = le64(b, 0x28) as usize + 64 * index + 4;
        put32(b, at, object::elf::SHT_PROGBITS);
    };
    let dyn_past_eof = |b: &mut Vec<u8>| {
        let (phoff, phnum) = (le64(b, 0x20) as usize, usize::from(b[0x38]));
        let ph = (0..phnum)
            .map(|i| phoff + 56 * i)
            .find(|&ph| le32(b, ph) == object::elf::PT_DYNAMIC)
            .unwrap();
        put64(b, ph + 8, 0x7fff_0000);
    };
    assert_ptrs(&open(name), name, "packed");
    assert_ptrs(&patched(name, retype), name, "retyped");
    assert_ptrs(&patched(name, dyn_past_eof), name, "p_offset past the file");
    assert_ptrs(
        &patched(name, |b| {
            retype(b);
            dyn_past_eof(b);
        }),
        name,
        "both",
    );
}

/// The ELF32 image's dynamic entries, each handed to `edit` as `(d_tag,
/// d_val)` to rewrite in place.
fn edit_dynamic32(b: &mut [u8], mut edit: impl FnMut(&mut u32, &mut u32)) {
    let ph = *phdrs32(b)
        .iter()
        .find(|&&ph| le32(b, ph) == object::elf::PT_DYNAMIC)
        .unwrap();
    let (off, size) = (le32(b, ph + 4) as usize, le32(b, ph + 16) as usize);
    for at in (off..off + size).step_by(8) {
        let (mut tag, mut val) = (le32(b, at), le32(b, at + 4));
        edit(&mut tag, &mut val);
        put32(b, at, tag);
        put32(b, at + 4, val);
    }
}

/// The ELF64 image's dynamic entries, as [`edit_dynamic32`].
fn edit_dynamic64(b: &mut [u8], mut edit: impl FnMut(&mut u64, &mut u64)) {
    let (phoff, phnum) = (le64(b, 0x20) as usize, usize::from(b[0x38]));
    let ph = (0..phnum)
        .map(|i| phoff + 56 * i)
        .find(|&ph| le32(b, ph) == object::elf::PT_DYNAMIC)
        .unwrap();
    let (off, size) = (le64(b, ph + 8) as usize, le64(b, ph + 32) as usize);
    for at in (off..off + size).step_by(16) {
        let (mut tag, mut val) = (le64(b, at), le64(b, at + 8));
        edit(&mut tag, &mut val);
        put64(b, at, tag);
        put64(b, at + 8, val);
    }
}

/// The stripped TEXTREL image with one tag edit making its table one the
/// loader view cannot read: the load is refused, as its code would be
/// served unrelocated. `DT_RELSZ` rewritten to `DT_DEBUG` (no size), to 15
/// (not a whole entry), `DT_RELENT` to 12, and `DT_REL` to an address in no
/// `PT_LOAD`.
#[test]
fn a_table_the_tags_place_but_no_loader_could_read_is_refused() {
    use object::elf::{DT_DEBUG, DT_REL, DT_RELENT, DT_RELSZ};
    type Edit = fn(&mut u32, &mut u32);
    let edits: [(&str, Edit); 4] = [
        ("no DT_RELSZ", |t, _| {
            if *t == DT_RELSZ {
                *t = DT_DEBUG;
            }
        }),
        ("DT_RELSZ 15", |t, v| {
            if *t == DT_RELSZ {
                *v = 15;
            }
        }),
        ("DT_RELENT 12", |t, v| {
            if *t == DT_RELENT {
                *v = 12;
            }
        }),
        ("DT_REL unmapped", |t, v| {
            if *t == DT_REL {
                *v = 0x7fff_0000;
            }
        }),
    ];
    for (what, edit) in edits {
        let elf = patched("x86_textrel_noshdr.elf", |b| edit_dynamic32(b, edit));
        match ElfFileMemReader::new(&elf, APPLIED) {
            Err(e) => assert!(format!("{e:#}").contains("relocates code"), "{what}: {e:#}"),
            Ok(_) => panic!("{what}: loaded"),
        }
    }
}

/// An Android packed table that does not decode, its `APS2` magic
/// rewritten, relocates something unknown: the ROM serves no `.got` or
/// `.data.rel.ro`. One whose count names more entries than the file could
/// hold is refused.
#[test]
fn an_android_table_that_does_not_decode_or_overflows_the_budget() {
    let name = "aarch64_android_rela.elf";
    let table = |b: &[u8]| {
        let mut at = None;
        let mut copy = b.to_vec();
        edit_dynamic64(&mut copy, |t, v| {
            if *t == 0x6000_0011 {
                at = Some(*v as usize);
            }
        });
        // The library maps its first PT_LOAD at file offset 0.
        at.unwrap()
    };
    let bad_magic = patched(name, |b| {
        let at = table(b);
        assert_eq!(&b[at..at + 4], b"APS2");
        b[at + 3] = b'X';
    });
    let (ptrs, _, _) = ptrs(name);
    let reader = ElfFileMemReader::new(&bad_magic, GOT_IS_ROM).unwrap();
    let got = rom_word(&reader, ptrs, 8);
    assert!(got.is_err(), "bad magic: {got:?}");

    // `APS2`, a count of 2^40 as SLEB128, a start offset of 0, then one
    // group of 2^40 members with every field fixed.
    let huge = patched(name, |b| {
        let at = table(b);
        let stream = [
            b'A', b'P', b'S', b'2', 0x80, 0x80, 0x80, 0x80, 0x80, 0x20, 0x00, 0x80, 0x80, 0x80,
            0x80, 0x80, 0x20, 0x03, 0x08, 0x00,
        ];
        b[at..at + stream.len()].copy_from_slice(&stream);
    });
    let err = ElfFileMemReader::new(&huge, GOT_IS_ROM).expect_err("past the budget");
    assert!(format!("{err:#}").contains("relocation entries"), "{err:#}");
}

/// `x64_copy_pie.elf`'s `R_X86_64_COPY` of `tbl` with its symbol index
/// zeroed (no size known), and with `tbl`'s `.dynsym` `st_size` rewritten to
/// 2^48 - 1 and to 13. With no size, or one past the mapping, the hole runs
/// to the end of what the ROM serves; 13 bytes hole exactly those.
#[test]
fn a_copy_object_is_holed_by_its_symbols_size_within_its_mapping() {
    let name = "x64_copy_pie.elf";
    let (tbl, rela, dynsym) = {
        let elf = open(name);
        let obj = elf.checked_file().unwrap();
        let file_off = |s: &str| obj.section_by_name(s).unwrap().file_range().unwrap().0 as usize;
        let tbl = obj
            .dynamic_symbols()
            .find(|s| s.name() == Ok("tbl"))
            .unwrap();
        (
            tbl.address(),
            file_off(".rela.dyn"),
            (file_off(".dynsym"), tbl.index().0),
        )
    };
    let st_size = |size: u64| move |b: &mut Vec<u8>| put64(b, dynsym.0 + 24 * dynsym.1 + 16, size);
    let no_symbol = patched(name, |b| {
        // The one Elf64_Rela's r_info: r_sym 0, R_X86_64_COPY.
        put64(b, rela + 8, u64::from(object::elf::R_X86_64_COPY));
    });
    let copy = object::elf::R_X86_64_COPY;
    for (what, elf, holed) in [
        ("no symbol", no_symbol, 32),
        (
            "st_size 2^48 - 1",
            patched(name, st_size((1 << 48) - 1)),
            32,
        ),
        ("st_size 13", patched(name, st_size(13)), 13),
    ] {
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        for off in 0..32 {
            let got = rom_word(&reader, tbl + off, 1);
            if off < holed {
                assert_hole(got, copy, &format!("{what}: tbl+{off}"));
            } else {
                assert_eq!(got, Ok(0), "{what}: tbl+{off}");
            }
        }
    }
}

/// A `DT_RELA` table running past `u64::MAX` inside a `PT_LOAD` that does
/// too: the load is refused, without an overflow panic on the way.
#[test]
fn a_dynamic_table_wrapping_the_address_space_does_not_panic() {
    const V: u64 = 0xffff_ffff_ffff_ff80;
    let mut seg = vec![0u8; 0x100];
    // DT_RELA at V + 0x60 (three 24-byte entries, the third wrapping),
    // DT_RELASZ, DT_RELAENT, DT_NULL.
    for (i, (tag, val)) in [(7u64, V + 0x60), (8, 72), (9, 24), (0, 0)]
        .into_iter()
        .enumerate()
    {
        put64(&mut seg, 16 * i, tag);
        put64(&mut seg, 16 * i + 8, val);
    }
    let mut elf = b"\x7fELF\x02\x01\x01".to_vec();
    elf.resize(16, 0);
    for (v, width) in [
        (3, 2),  // ET_DYN
        (62, 2), // EM_X86_64
        (1, 4),
        (0, 8),
        (64, 8), // e_phoff
        (0, 8),
        (0, 4),
        (64, 2),
        (56, 2),
        (2, 2), // e_phnum
        (64, 2),
        (0, 2),
        (0, 2),
    ] {
        elf.extend_from_slice(&u64::to_le_bytes(v)[..width]);
    }
    // PT_LOAD RW and PT_DYNAMIC, both at V from file offset 0x1000.
    for (p_type, filesz, align) in [(1u32, 0x100u64, 0x1000u64), (2, 0x40, 8)] {
        elf.extend_from_slice(&p_type.to_le_bytes());
        elf.extend_from_slice(&6u32.to_le_bytes());
        for v in [0x1000, V, V, filesz, filesz, align] {
            elf.extend_from_slice(&u64::to_le_bytes(v));
        }
    }
    elf.resize(0x1000, 0);
    elf.extend_from_slice(&seg);

    let elf = OwnedElf::parse(elf).unwrap();
    assert!(ElfFileMemReader::new(&elf, APPLIED).is_err());
}
