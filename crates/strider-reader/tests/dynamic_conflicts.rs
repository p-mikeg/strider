//! What the loader view holes or refuses rather than serve one reading of
//! tables the loaders could read differently, or that it does not model:
//! overlapping tables, a place relocated twice, a table a relocation
//! rewrites, an unreadable table, a `.dynsym` that is not `DT_SYMTAB`'s,
//! and an Android stream read at the wrong width or past the budget. Each
//! image is a `fixtures/loader/out/*` one with the patch its test
//! documents, or built here whole.

use object::{Object, ObjectSection, ObjectSymbol};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::RelocationMode;
use strider_reader::{ElfFileMemReader, OwnedElf, ReadOnlyMemory};

const APPLIED: RelocationMode = RelocationMode::Applied { got_is_rom: false };
const GOT_IS_ROM: RelocationMode = RelocationMode::Applied { got_is_rom: true };

fn path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/loader/out")
        .join(name)
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(path(name))
        .unwrap_or_else(|e| panic!("{name}: {e} (run `make -C fixtures/loader`)"))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// `size` little-endian bytes of `v` at `at`.
fn put(b: &mut [u8], at: usize, v: u64, size: usize) {
    b[at..at + size].copy_from_slice(&v.to_le_bytes()[..size]);
}

/// The little-endian `width`-byte word the ROM view serves at `addr`.
fn rom(reader: &ElfFileMemReader, addr: u64, width: usize) -> Result<u64, String> {
    let mut buf = vec![0u8; width];
    ReadOnlyMemory::read(reader, addr, &mut buf).map_err(|e| e.to_string())?;
    Ok(buf.iter().rev().fold(0, |a, &b| (a << 8) | u64::from(b)))
}

/// The little-endian `width`-byte word the fetch view serves at `addr`.
fn fetch(reader: &ElfFileMemReader, addr: u64, width: usize) -> Option<u64> {
    let mut buf = vec![0u8; width];
    let at = VnAddr {
        off: addr,
        space: VnSpace::RAM,
    };
    match MemReader::read(reader, at, &mut buf) {
        Ok(n) if n == width => Some(buf.iter().rev().fold(0, |a, &b| (a << 8) | u64::from(b))),
        _ => None,
    }
}

/// The little-endian image's dynamic array, at the file offset `off` of
/// `slots` entries of `word` bytes, rewritten by `edit` and closed by a
/// `DT_NULL`.
fn rewrite_dynamic(
    b: &mut [u8],
    off: usize,
    slots: usize,
    word: usize,
    edit: impl FnOnce(&mut Vec<(u64, u64)>),
) {
    let read = |b: &[u8], at: usize| {
        if word == 8 {
            le64(b, at)
        } else {
            u64::from(le32(b, at))
        }
    };
    let mut tags: Vec<(u64, u64)> = (0..slots)
        .map(|i| {
            (
                read(b, off + 2 * word * i),
                read(b, off + 2 * word * i + word),
            )
        })
        .take_while(|&(t, _)| t != 0)
        .collect();
    edit(&mut tags);
    tags.push((0, 0));
    assert!(tags.len() <= slots, "the array has room");
    for (i, &(t, v)) in tags.iter().enumerate() {
        put(b, off + 2 * word * i, t, word);
        put(b, off + 2 * word * i + word, v, word);
    }
}

/// `x64_relro_pie.elf`'s dynamic array: file offset 0xef0, 17 entries.
const RELRO_DYNAMIC: (usize, usize) = (0xef0, 17);
/// `p`, the relro word its one RELATIVE relocates, and `&x`, its addend.
const RELRO_P: u64 = 0x1ee8;

/// `x64_relro_pie.elf` with `DT_RELA` at 48 bytes of the dead `.eh_frame_hdr`
/// (0x320): a RELATIVE at `p` with addend 0x2004, where the file holds
/// 0x2000, then an `R_X86_64_NONE`. `DT_JMPREL` (RELA, 24 bytes) at
/// `0x320 + jmprel`, inside that table.
fn relro_overlap(jmprel: u64) -> OwnedElf {
    let mut b = bytes("x64_relro_pie.elf");
    let t = 0x320u64;
    for (i, w) in [RELRO_P, 8, 0x2004, 0, 0, 0, 0].into_iter().enumerate() {
        put(&mut b, t as usize + 8 * i, w, 8);
    }
    let (off, slots) = RELRO_DYNAMIC;
    rewrite_dynamic(&mut b, off, slots, 8, |tags| {
        for (tag, v) in tags.iter_mut() {
            match *tag {
                7 => *v = t,
                8 => *v = 48,
                _ => {}
            }
        }
        tags.extend([(23, t + jmprel), (2, 24), (20, 7)]);
    });
    OwnedElf::parse(b).unwrap()
}

/// `DT_JMPREL` at the RELA table's second entry is that entry: the place
/// is relocated once, to 0x2004, as every loader does. At 8 bytes past it
/// the table is read across entry boundaries, which the loaders walk
/// separately and this view does not model: `p` is a hole, in the ROM and
/// in the fetch view alike.
#[test]
fn tables_that_overlap_off_their_entry_boundaries_hole_every_place() {
    let aligned = relro_overlap(24);
    let reader = ElfFileMemReader::new(&aligned, GOT_IS_ROM).unwrap();
    assert_eq!(rom(&reader, RELRO_P, 8), Ok(0x2004), "aligned, ROM");
    assert_eq!(fetch(&reader, RELRO_P, 8), Some(0x2004), "aligned, read");

    let misaligned = relro_overlap(32);
    let reader = ElfFileMemReader::new(&misaligned, GOT_IS_ROM).unwrap();
    let got = rom(&reader, RELRO_P, 8);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("relocation of type 8 ")),
        "misaligned, ROM: {got:?}"
    );
    assert_eq!(fetch(&reader, RELRO_P, 8), None, "misaligned, read");
}

/// `x64_relro_pie.elf`'s RELATIVE (addend 0x2004) moved to `DT_JMPREL`,
/// and `DT_RELA` aimed at 24 bytes of the zero-filled tail its RW
/// `PT_LOAD`'s `p_memsz` is grown to. A table in no file bytes is one this
/// view cannot read, so no writable mapping is served, `read` included,
/// where it served `p` unrelocated. The control, with only the addend
/// changed, serves 0x2004.
#[test]
fn an_unreadable_table_leaves_no_writable_mapping_served() {
    let mut control = bytes("x64_relro_pie.elf");
    for (i, w) in [RELRO_P, 8, 0x2004].into_iter().enumerate() {
        put(&mut control, 0x2f0 + 8 * i, w, 8);
    }
    let mut b = control.clone();
    // The RW PT_LOAD is the fourth Elf64_Phdr; p_memsz at 0x28.
    let ph = 64 + 56 * 3;
    assert_eq!(le32(&b, ph), object::elf::PT_LOAD);
    put(&mut b, ph + 0x28, 0x160, 8);
    let (off, slots) = RELRO_DYNAMIC;
    rewrite_dynamic(&mut b, off, slots, 8, |tags| {
        for (tag, v) in tags.iter_mut() {
            match *tag {
                7 => *v = 0x2010,
                8 => *v = 24,
                _ => {}
            }
        }
        tags.extend([(23, 0x2f0), (2, 24), (20, 7)]);
    });
    let control = OwnedElf::parse(control).unwrap();
    let reader = ElfFileMemReader::new(&control, APPLIED).unwrap();
    assert_eq!(fetch(&reader, RELRO_P, 8), Some(0x2004), "control");
    let elf = OwnedElf::parse(b).unwrap();
    let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
    assert_eq!(fetch(&reader, RELRO_P, 8), None, "zero-filled DT_RELA");
}

/// `x86_textrel.elf`'s dynamic array: file offset 0xf80, 16 entries.
const TEXTREL_DYNAMIC: (usize, usize) = (0xf80, 16);
/// `getl`'s operand, the `R_386_32` against `loc`; `.rel.dyn`'s file
/// offset.
const TEXTREL_GETL: u64 = 0x1e5;
const TEXTREL_REL_DYN: usize = 0x1c4;

/// `x86_textrel.elf` with `BIND_NOW` and `DT_JMPREL` (REL, 8 bytes) at the
/// first entry of `.rel.dyn`: every loader applies that entry twice, its
/// implicit addend stacking to `2 * &loc`. This view walks an entry once,
/// so the place is a hole.
#[test]
fn a_rel_place_relocated_twice_is_a_hole() {
    let mut b = bytes("x86_textrel.elf");
    let (off, slots) = TEXTREL_DYNAMIC;
    rewrite_dynamic(&mut b, off, slots, 4, |tags| {
        for (tag, v) in tags.iter_mut() {
            if *tag == u64::from(object::elf::DT_FLAGS) {
                *v |= u64::from(object::elf::DF_BIND_NOW);
            }
        }
        tags.extend([(23, TEXTREL_REL_DYN as u64), (2, 8), (20, 17)]);
    });
    let elf = OwnedElf::parse(b).unwrap();
    let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
    assert_eq!(fetch(&reader, TEXTREL_GETL, 4), None, "getl's operand");
    let got = rom(&reader, TEXTREL_GETL, 4);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("relocation of type 1 ")),
        "{got:?}"
    );
}

/// `x86_textrel.elf`'s `.rel.dyn` rewritten so its first entry relocates
/// the second's `r_offset`, which holds `0x1f5 - &loc` and becomes `geti`'s
/// operand once relocated. A table a relocation rewrites is not modelled:
/// the TEXTREL image is refused, where it served `geti`'s operand as 0.
#[test]
fn a_relocation_inside_a_table_refuses_the_image() {
    let mut b = bytes("x86_textrel.elf");
    let at = TEXTREL_REL_DYN;
    let loc = 0x2000u64;
    for (i, w) in [
        at as u64 + 8,
        (2 << 8) | 1,
        0x1f5u64.wrapping_sub(loc) & 0xffff_ffff,
        (1 << 8) | 1,
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut b, at + 4 * i, w, 4);
    }
    let elf = OwnedElf::parse(b).unwrap();
    match ElfFileMemReader::new(&elf, APPLIED) {
        Err(e) => assert!(
            format!("{e:#}").contains("inside a relocation table"),
            "{e:#}"
        ),
        Ok(reader) => panic!("geti's operand served {:?}", fetch(&reader, 0x1f5, 4)),
    }
}

/// SLEB128 of `v`.
fn sleb(mut v: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0) {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// An ELF32 little-endian ARM ET_DYN, one RW `PT_LOAD` over its 4 KiB, with
/// `stream` as its `DT_ANDROID_REL` table at 0x200 and four words
/// 0x11111110.. at 0x300, its `.got`.
fn arm_android_rel(stream: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 0x1000];
    let (got, aps, dynamic) = (0x300usize, 0x200usize, 0x100usize);
    f[aps..aps + stream.len()].copy_from_slice(stream);
    for i in 0..4 {
        put(&mut f, got + 4 * i, 0x1111_1110 + i as u64, 4);
    }
    for (i, (t, v)) in [
        (0x6000_000f, aps as u64),
        (0x6000_0010, stream.len() as u64),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut f, dynamic + 8 * i, t, 4);
        put(&mut f, dynamic + 8 * i + 4, v, 4);
    }
    let shstr = b"\0.got\0.dynamic\0.shstrtab\0";
    f[0x400..0x400 + shstr.len()].copy_from_slice(shstr);
    // Elf32_Phdr: p_type, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz,
    // p_flags, p_align.
    let phdrs: [[u32; 8]; 2] = [
        [1, 0, 0, 0, 0x1000, 0x1000, 6, 0x1000],
        [
            2,
            dynamic as u32,
            dynamic as u32,
            dynamic as u32,
            24,
            24,
            6,
            4,
        ],
    ];
    for (i, p) in phdrs.iter().enumerate() {
        for (j, &v) in p.iter().enumerate() {
            put(&mut f, 0x34 + 32 * i + 4 * j, u64::from(v), 4);
        }
    }
    // Elf32_Shdr: name, type, flags, addr, offset, size, link, info, align,
    // entsize.
    let shdrs: [[u32; 10]; 4] = [
        [0; 10],
        [1, 1, 3, got as u32, got as u32, 16, 0, 0, 4, 4],
        [6, 6, 3, dynamic as u32, dynamic as u32, 24, 0, 0, 4, 8],
        [15, 3, 0, 0, 0x400, shstr.len() as u32, 0, 0, 1, 0],
    ];
    for (i, s) in shdrs.iter().enumerate() {
        for (j, &v) in s.iter().enumerate() {
            put(&mut f, 0x500 + 40 * i + 4 * j, u64::from(v), 4);
        }
    }
    f[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
    // e_type, e_machine, e_version, e_entry, e_phoff, e_shoff, e_flags,
    // e_ehsize, e_phentsize, e_phnum, e_shentsize, e_shnum, e_shstrndx.
    let header: [(usize, u64, usize); 13] = [
        (16, 3, 2),
        (18, 40, 2),
        (20, 1, 4),
        (24, 0, 4),
        (28, 0x34, 4),
        (32, 0x500, 4),
        (36, 0x0500_0000, 4),
        (40, 52, 2),
        (42, 32, 2),
        (44, 2, 2),
        (46, 40, 2),
        (48, 4, 2),
        (50, 3, 2),
    ];
    for (at, v, size) in header {
        put(&mut f, at, v, size);
    }
    f
}

/// A REL stream of two groups: a RELATIVE at 0x300, then an
/// `R_ARM_GLOB_DAT` against symbol 1 at 0x304, the first group's size
/// written `size`. bionic reads each field as a 32-bit `size_t`, so
/// `81 80 80 80 10`, 2^32 + 1 at 64 bits, is a group of one: the
/// `GLOB_DAT` is the second group's, and a hole.
#[test]
fn an_elf32_android_stream_reads_its_fields_at_32_bits() {
    let group = |size: &[u8]| {
        [
            b"APS2".as_slice(),
            &sleb(2),
            &sleb(0x300 - 4),
            size,
            &sleb(3),
            &sleb(4),
            &sleb(23),
            &sleb(1),
            &sleb(3),
            &sleb(4),
            &sleb((1 << 8) | 21),
        ]
        .concat()
    };
    for (what, size) in [
        ("size 1", sleb(1)),
        ("size 2^32 + 1", vec![0x81, 0x80, 0x80, 0x80, 0x10]),
    ] {
        let elf = OwnedElf::parse(arm_android_rel(&group(&size))).unwrap();
        let reader = ElfFileMemReader::new(&elf, GOT_IS_ROM).unwrap();
        let got = rom(&reader, 0x304, 4);
        assert!(
            got.as_ref()
                .is_err_and(|e| e.contains("relocation of type 21 ")),
            "{what}: {got:?}"
        );
    }
}

/// An ELF64 AArch64 ET_DYN of `len` bytes whose `DT_ANDROID_RELA` stream
/// names `2 * len - 4096` RELATIVE relocations in one group fixing every
/// field: under the old count of a byte an entry it decoded all of them,
/// 24 bytes each. Counted at a RELA entry's size, it is past the budget
/// and refused before any is decoded.
#[test]
fn an_android_stream_is_counted_at_its_entry_size() {
    let len = 1usize << 16;
    let n = 2 * len as i64 - 4096;
    let stream = [
        b"APS2".as_slice(),
        &sleb(n),
        &sleb(0x300),
        &sleb(n),
        &sleb(1 | 2 | 8 | 4),
        &sleb(0),
        &sleb(1027),
        &sleb(0),
    ]
    .concat();
    let mut f = vec![0u8; len];
    let (got, aps, dynamic) = (0x300usize, 0x200usize, 0x100usize);
    f[aps..aps + stream.len()].copy_from_slice(&stream);
    for (i, (t, v)) in [
        (0x6000_0011, aps as u64),
        (0x6000_0012, stream.len() as u64),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut f, dynamic + 16 * i, t, 8);
        put(&mut f, dynamic + 16 * i + 8, v, 8);
    }
    let shstr = b"\0.got\0.dynamic\0.shstrtab\0";
    f[0x400..0x400 + shstr.len()].copy_from_slice(shstr);
    // Elf64_Phdr: p_type, p_flags, then p_offset, p_vaddr, p_paddr,
    // p_filesz, p_memsz, p_align.
    for (i, (ty, off, size)) in [(1u64, 0u64, len as u64), (2, dynamic as u64, 48)]
        .into_iter()
        .enumerate()
    {
        let ph = 0x40 + 56 * i;
        put(&mut f, ph, ty, 4);
        put(&mut f, ph + 4, 6, 4);
        for (j, v) in [off, off, off, size, size, if ty == 1 { 0x1000 } else { 8 }]
            .into_iter()
            .enumerate()
        {
            put(&mut f, ph + 8 + 8 * j, v, 8);
        }
    }
    // Elf64_Shdr: name, type (4 each), flags, addr, offset, size (8 each),
    // link, info (4 each), align, entsize (8 each).
    type Shdr = (u64, u64, u64, u64, u64, u64, u64, u64);
    let shdrs: [Shdr; 4] = [
        (0, 0, 0, 0, 0, 0, 0, 0),
        (1, 1, 3, got as u64, got as u64, 32, 8, 8),
        (6, 6, 3, dynamic as u64, dynamic as u64, 48, 8, 16),
        (15, 3, 0, 0, 0x400, shstr.len() as u64, 1, 0),
    ];
    for (i, &(name, ty, flags, addr, off, size, align, ent)) in shdrs.iter().enumerate() {
        let sh = 0x500 + 64 * i;
        put(&mut f, sh, name, 4);
        put(&mut f, sh + 4, ty, 4);
        put(&mut f, sh + 8, flags, 8);
        put(&mut f, sh + 16, addr, 8);
        put(&mut f, sh + 24, off, 8);
        put(&mut f, sh + 32, size, 8);
        put(&mut f, sh + 48, align, 8);
        put(&mut f, sh + 56, ent, 8);
    }
    f[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (at, v, size) in [
        (16, 3, 2),
        (18, 183, 2),
        (20, 1, 4),
        (24, 0, 8),
        (32, 0x40, 8),
        (40, 0x500, 8),
        (48, 0, 4),
        (52, 64, 2),
        (54, 56, 2),
        (56, 2, 2),
        (58, 64, 2),
        (60, 4, 2),
        (62, 3, 2),
    ] {
        put(&mut f, at, v, size);
    }
    let elf = OwnedElf::parse(f).unwrap();
    let err = ElfFileMemReader::new(&elf, GOT_IS_ROM).expect_err("past the budget");
    assert!(format!("{err:#}").contains("relocation entries"), "{err:#}");
}

/// `x86_textrel.elf` with its `.dynsym` header's `sh_size` grown to 0x400,
/// past the end of the `PT_LOAD` mapping it though still in the file, and
/// with `DT_GNU_HASH`'s `symoffset` raised to 2^24, so the
/// table the hash names runs past them: either way the loader's table is
/// not the bytes the header holds, and `loc` does not resolve, where the
/// unpatched image patches `getl`'s operand to `&loc`.
#[test]
fn a_dynsym_must_hold_the_whole_dt_symtab_table() {
    let original = bytes("x86_textrel.elf");
    let obj = object::File::parse(&original[..]).unwrap();
    let dynsym = obj.section_by_name(".dynsym").unwrap().index().0;
    let gnu_hash = obj
        .section_by_name(".gnu.hash")
        .unwrap()
        .file_range()
        .unwrap()
        .0;
    let loc = obj
        .dynamic_symbols()
        .find(|s| s.name() == Ok("loc"))
        .unwrap()
        .address();
    let shoff = le32(&original, 0x20) as usize;
    let reader =
        ElfFileMemReader::new(&OwnedElf::parse(original.clone()).unwrap(), APPLIED).unwrap();
    assert_eq!(fetch(&reader, TEXTREL_GETL, 4), Some(loc), "unpatched");
    let mut long = original.clone();
    put(&mut long, shoff + 40 * dynsym + 20, 0x400, 4);
    let mut hashed = original;
    put(&mut hashed, gnu_hash as usize + 4, 0x0100_0000, 4);
    for (what, b) in [("sh_size", long), ("DT_GNU_HASH", hashed)] {
        let elf = OwnedElf::parse(b).unwrap();
        let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
        assert_eq!(fetch(&reader, TEXTREL_GETL, 4), None, "{what}");
    }
}

/// `x86_textrel.elf` with `.rel.dyn`'s second entry made a second
/// `R_386_32` against `loc` at the first's place, `getl`'s operand: the
/// loader stacks the two to `2 * &loc`, which this view does not, so the
/// place is a hole where it served `&loc`.
#[test]
fn a_place_two_entries_relocate_is_a_hole() {
    let mut b = bytes("x86_textrel.elf");
    put(&mut b, TEXTREL_REL_DYN + 8, TEXTREL_GETL, 4);
    put(&mut b, TEXTREL_REL_DYN + 12, (2 << 8) | 1, 4);
    let elf = OwnedElf::parse(b).unwrap();
    let reader = ElfFileMemReader::new(&elf, APPLIED).unwrap();
    assert_eq!(fetch(&reader, TEXTREL_GETL, 4), None, "getl's operand");
}
