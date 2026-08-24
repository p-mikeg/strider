#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! The ET_REL synthetic section layout.
//!
//! `gcc -O2` leaves `.text`, `.text.startup` and `.rodata` all at `sh_addr == 0`
//! in a `.o`. The loader rebases the colliding ones so each is reachable at its
//! own address, serving its own bytes; a LINKED image, whose `sh_addr`s are
//! real, passes through untouched and must load byte-identically to its
//! recorded fingerprint.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::elf_fixture::{SectionSpec, SymbolSpec, build_elf_with_sections_and_symbols};
use object::{Object, ObjectSection, ObjectSymbol};
use strider_reader::elf::{self, ElfSectionLayout};
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

fn fixture(arch: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(name)
}

/// FNV-1a over every region's `(start, len, bytes)`, so one number pins a whole
/// loaded image.
fn digest(regions: &[MemRegion]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for r in regions {
        for b in r.start_addr().to_le_bytes() {
            eat(b);
        }
        let bytes = common::region_bytes(r);
        for b in (bytes.len() as u64).to_le_bytes() {
            eat(b);
        }
        for b in bytes {
            eat(b);
        }
    }
    h
}

fn all_loader_digests(owned: &strider_reader::OwnedElf) -> Vec<u64> {
    use elf::LoadFilter::{AllAllocatable, CodeAndReadOnly, ImmutableOnly};
    use elf::RegionSource::{Auto, Sections};
    let obj = owned.file();
    let regions = |source, filter, relocate| owned.regions(source, filter, relocate).unwrap();
    vec![
        digest(&elf::elf_get_loadable_regions(&obj).unwrap()),
        digest(&elf::elf_get_readonly_regions(&obj).unwrap()),
        digest(&elf::elf_get_loadable_regions_including_writable(&obj).unwrap()),
        digest(&regions(Sections, CodeAndReadOnly, false)),
        digest(&regions(Sections, AllAllocatable, false)),
        digest(&regions(Sections, ImmutableOnly, false)),
        digest(&regions(Auto, AllAllocatable, true)),
        digest(&regions(Auto, ImmutableOnly, true)),
    ]
}

/// Golden fingerprints for every entry point over three linked fixtures,
/// including the two that force the section walk on an image that has PT_LOAD
/// segments.
#[test]
fn linked_images_load_identically_to_before_the_et_rel_rebase() {
    let golden: [(&str, &str, [u64; 8]); 3] = [
        (
            "x64",
            "switch.elf",
            [
                15379296049503614269,
                15379296049503614269,
                1759349359742348395,
                13567922309565440120,
                11001421298295061515,
                13567922309565440120,
                1759349359742348395,
                15379296049503614269,
            ],
        ),
        (
            "aarch64",
            "arithmetic.elf",
            [
                2886907034106557727,
                2886907034106557727,
                7886025370851864316,
                908230149637798672,
                7537148421931574303,
                908230149637798672,
                7886025370851864316,
                2886907034106557727,
            ],
        ),
        (
            "mips64le",
            "switch.elf",
            [
                11072776634073155066,
                11072776634073155066,
                9598661585797864237,
                9270942079008536722,
                6504926835391262960,
                9270942079008536722,
                9598661585797864237,
                11072776634073155066,
            ],
        ),
    ];
    for (arch, name, expected) in golden {
        let path = fixture(arch, name);
        if !path.exists() {
            // A missing fixture must be VISIBLE: a silent return reports as a pass
            // and this file is the only coverage the ET_REL loader has.
            eprintln!(
                "SKIP {}: {} is not built; run `make -C fixtures`",
                module_path!(),
                path.display()
            );
            continue;
        }
        let owned = strider_reader::load_elf(&path).unwrap();
        let obj = owned.file();
        let layout = ElfSectionLayout::new(&obj);
        for sec in obj.sections() {
            assert_eq!(
                layout.section_base(&sec),
                sec.address(),
                "{arch}/{name}: a linked image must keep every section at its sh_addr"
            );
        }
        assert_eq!(
            all_loader_digests(&owned),
            expected.to_vec(),
            "{arch}/{name}: linked image bytes changed"
        );
    }
}

fn nonempty_alloc_sections<'d, 'f>(
    obj: &'f object::File<'d>,
) -> Vec<object::read::Section<'d, 'f>> {
    obj.sections()
        .filter(|s| {
            let object::SectionFlags::Elf { sh_flags } = s.flags() else {
                return false;
            };
            sh_flags & u64::from(object::elf::SHF_ALLOC) != 0
                && s.data().is_ok_and(|d| !d.is_empty())
        })
        .collect()
}

/// Every non-empty allocatable section must be reachable and serve ITS OWN
/// bytes. First-wins VMA dedup drops every section after the first at
/// `sh_addr == 0`, leaving a losing section's address range answering with the
/// winner's bytes.
#[test]
fn every_allocatable_section_of_an_object_file_serves_its_own_bytes() {
    for arch in ["x64", "x86", "aarch64", "arm", "mips64le", "ppc64le"] {
        for case in ["tzcount", "switch", "globals", "elf_relocs"] {
            let path = fixture(arch, &format!("{case}.o"));
            if !path.exists() {
                // A missing fixture must be VISIBLE: a silent return reports as a pass
                // and this file is the only coverage the ET_REL loader has.
                eprintln!(
                    "SKIP {}: {} is not built; run `make -C fixtures`",
                    module_path!(),
                    path.display()
                );
                continue;
            }
            let owned = strider_reader::load_elf(&path).unwrap();
            let obj = owned.file();
            let layout = ElfSectionLayout::new(&obj);
            let table = MemRegionsLookupTable::new(
                owned
                    .regions(
                        elf::RegionSource::Sections,
                        elf::LoadFilter::AllAllocatable,
                        false,
                    )
                    .unwrap(),
            );
            for sec in nonempty_alloc_sections(&obj) {
                let want = sec.data().unwrap();
                let base = layout.section_base(&sec);
                let mut got = vec![0u8; want.len()];
                table.read_exact(base, &mut got).unwrap_or_else(|e| {
                    panic!(
                        "{arch}/{case}.o: section {:?} unreachable at its base {base:#x}: {e}",
                        sec.name().unwrap_or("?")
                    )
                });
                assert_eq!(
                    got,
                    want,
                    "{arch}/{case}.o: section {:?} at {base:#x} serves another section's bytes",
                    sec.name().unwrap_or("?")
                );
            }
        }
    }
}

/// `tzcount.o` defines `tzcount` in `.text` and `main` in `.text.startup`, both
/// at `sh_addr == 0`. Resolving `main` through the layout must land on
/// `.text.startup`'s own first instruction, not on `tzcount`'s.
#[test]
fn object_file_symbols_resolve_into_their_own_section() {
    let path = fixture("x64", "tzcount.o");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).unwrap();
    let obj = owned.file();
    let layout = ElfSectionLayout::new(&obj);
    let table = MemRegionsLookupTable::new(elf::elf_get_loadable_regions(&obj).unwrap());

    let mut addrs = Vec::new();
    for name in ["tzcount", "main"] {
        let sym = obj.symbol_by_name(name).expect("symbol");
        let addr = layout.symbol_address(&sym);
        let sec = obj.section_by_index(sym.section_index().unwrap()).unwrap();
        let want = &sec.data().unwrap()[..sym.size() as usize];
        let mut got = vec![0u8; want.len()];
        table
            .read_exact(addr, &mut got)
            .unwrap_or_else(|e| panic!("{name} at {addr:#x} is unmapped: {e}"));
        assert_eq!(
            got,
            want,
            "{name} at {addr:#x} must serve {:?}'s bytes",
            sec.name().unwrap_or("?")
        );
        addrs.push(addr);
    }
    assert_ne!(
        addrs[0], addrs[1],
        "tzcount and main must not share an address"
    );
}

/// `switch.o`'s `.rodata` holds two 8-entry jump tables and 16 `.rela.rodata`
/// entries, and shares VMA 0 with `.text`. Under first-wins dedup `.rodata`
/// loses to `.text` and the table is neither readable nor relocated: `owners`
/// keeps only the winner, skipping all 16 relocations.
#[test]
fn object_file_jump_table_is_mapped_and_relocated() {
    let path = fixture("x64", "switch.o");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).unwrap();
    let obj = owned.file();
    let layout = ElfSectionLayout::new(&obj);
    let table = MemRegionsLookupTable::new(common::load_with_relocations(&obj));

    let rodata = obj.section_by_name(".rodata").expect(".rodata");
    let rodata_base = layout.section_base(&rodata);
    let initial = rodata.data().unwrap();
    assert_eq!(initial.len(), 64, "two 8-entry x 4-byte tables");

    let mut patched = 0usize;
    for (offset, reloc) in rodata.relocations() {
        let site = rodata_base + offset;
        let mut raw = [0u8; 4];
        table
            .read_exact(site, &mut raw)
            .unwrap_or_else(|e| panic!("jump-table slot at {site:#x} unmapped: {e}"));
        let value = u32::from_le_bytes(raw);

        let object::RelocationTarget::Symbol(idx) = reloc.target() else {
            panic!("unexpected relocation target");
        };
        let sym = obj.symbol_by_index(idx).unwrap();
        // R_X86_64_PC32: S + A - P, with S rebased through the layout.
        let expected = layout
            .symbol_address(&sym)
            .wrapping_add(reloc.addend() as u64)
            .wrapping_sub(site) as u32;
        assert_eq!(
            value, expected,
            "slot at {site:#x} not patched to S + A - P"
        );
        patched += usize::from(raw != initial[offset as usize..offset as usize + 4]);

        // The table's own contract: `table_base + entry` is the branch target.
        // Each table is 8 x 4 bytes, so the run containing a slot starts at its
        // offset rounded down to 0x20.
        let table_base = rodata_base + (offset & !0x1f);
        let target_sec = obj.section_by_index(sym.section_index().unwrap()).unwrap();
        let lo = layout.section_base(&target_sec);
        let hi = lo + target_sec.size();
        let resolved = table_base.wrapping_add(u64::from(value) as i32 as i64 as u64);
        assert!(
            (lo..hi).contains(&resolved),
            "slot at {site:#x} branches to {resolved:#x}, outside {:?} [{lo:#x}, {hi:#x})",
            target_sec.name().unwrap_or("?")
        );
    }
    assert_eq!(patched, 16, "all 16 .rela.rodata entries must be applied");
}

/// The rebase must not smuggle a writable section into the read-only view:
/// `elf_relocs.o`'s `.data.rel.ro.local` collides with `.text` at VMA 0 and now
/// gets its own address, which must still answer no ROM read.
#[test]
fn a_rebased_writable_section_is_still_not_read_only_memory() {
    let path = fixture("x64", "elf_relocs.o");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).unwrap();
    let obj = owned.file();
    let layout = ElfSectionLayout::new(&obj);
    let reader = elf::ElfFileMemReader::from_object(&obj).unwrap();

    let data = obj
        .section_by_name(".data.rel.ro.local")
        .expect(".data.rel.ro.local");
    let base = layout.section_base(&data);
    assert!(
        ReadOnlyMemory::read(&reader, base, &mut [0u8; 8]).is_err(),
        "writable section at {base:#x} must never answer a ROM read"
    );
}

/// `SHT_NOBITS` carries no file bytes but still occupies `sh_size` of address
/// space, so the layout watermark is sized from `sh_size`. Sizing it from
/// `data().len()` gives `.bss` no space and no rebase, leaving it at
/// `sh_addr == 0` on top of `.text`.
#[test]
fn a_nobits_section_occupies_its_own_address_space() {
    let bytes = build_elf_with_sections_and_symbols(
        &[
            SectionSpec::text(0, vec![0x90; 0x20]),
            SectionSpec::bss(0, 0x100),
            SectionSpec::rodata(0, vec![0xaa; 8]),
        ],
        &[],
    );
    let obj = object::File::parse(&bytes[..]).unwrap();
    let layout = ElfSectionLayout::new(&obj);
    let base = |name: &str| layout.section_base(&obj.section_by_name(name).unwrap());
    let (text, bss, rodata) = (base(".text"), base(".bss"), base(".rodata"));

    assert!(
        bss >= text + 0x20,
        ".bss at {bss:#x} overlaps .text [{text:#x}, {:#x})",
        text + 0x20
    );
    assert!(
        rodata >= bss + 0x100,
        ".rodata at {rodata:#x} overlaps .bss [{bss:#x}, {:#x})",
        bss + 0x100
    );
}

/// A `.bss` object symbol resolves through its section's synthetic base. Plain
/// `st_value + 0` lands in whatever the watermark walk placed at the low
/// addresses, normally `.text`.
#[test]
fn a_bss_symbol_does_not_resolve_into_text() {
    let bytes = build_elf_with_sections_and_symbols(
        &[
            SectionSpec::text(0, vec![0x90; 0x20]),
            SectionSpec::bss(0, 0x100),
        ],
        &[
            SymbolSpec {
                name: b"getbss",
                section: 0,
                value: 0,
                size: 0x20,
            },
            SymbolSpec {
                name: b"big_bss",
                section: 1,
                value: 0,
                size: 0x100,
            },
        ],
    );
    let obj = object::File::parse(&bytes[..]).unwrap();
    let layout = ElfSectionLayout::new(&obj);
    let text = layout.section_base(&obj.section_by_name(".text").unwrap());
    let bss = layout.section_base(&obj.section_by_name(".bss").unwrap());

    let addr = |name: &str| layout.symbol_address(&obj.symbol_by_name(name).unwrap());
    assert_eq!(addr("getbss"), text, "a .text symbol still sits in .text");
    assert_eq!(addr("big_bss"), bss, "a .bss symbol sits in .bss");
    assert!(
        !(text..text + 0x20).contains(&addr("big_bss")),
        "big_bss at {:#x} landed inside .text",
        addr("big_bss")
    );
}

/// `.bss` has no bytes to serve, so its address range must stay unmapped: a
/// read there is an error, never another section's bytes and never zeros.
#[test]
fn an_unmapped_bss_address_answers_no_read() {
    let bytes = build_elf_with_sections_and_symbols(
        &[
            SectionSpec::text(0, vec![0x90; 0x20]),
            SectionSpec::bss(0, 0x100),
        ],
        &[],
    );
    let obj = object::File::parse(&bytes[..]).unwrap();
    let layout = ElfSectionLayout::new(&obj);
    let bss = layout.section_base(&obj.section_by_name(".bss").unwrap());

    let owned = strider_reader::OwnedElf::parse(bytes.clone()).unwrap();
    let table = MemRegionsLookupTable::new(
        owned
            .regions(
                elf::RegionSource::Sections,
                elf::LoadFilter::AllAllocatable,
                false,
            )
            .unwrap(),
    );
    assert!(
        table.read_exact(bss, &mut [0u8; 8]).is_err(),
        "NOBITS at {bss:#x} must not be mapped"
    );

    let reader = elf::ElfFileMemReader::from_object(&obj).unwrap();
    assert!(
        ReadOnlyMemory::read(&reader, bss, &mut [0u8; 8]).is_err(),
        "NOBITS at {bss:#x} must never answer a ROM read"
    );
}

/// gABI: an ET_REL `st_value` is an offset from the start of the section
/// `st_shndx` names, so a symbol's address is that section's base plus it.
/// Every ordinary `.o` has `sh_addr == 0`, which hides the missing term.
#[test]
fn an_et_rel_symbol_address_includes_its_sections_sh_addr() {
    let bytes = build_elf_with_sections_and_symbols(
        &[SectionSpec::text(0x1000, vec![0x90; 0x20])],
        &[SymbolSpec {
            name: b"f",
            section: 0,
            value: 0x10,
            size: 0x10,
        }],
    );
    let obj = object::File::parse(&bytes[..]).unwrap();
    let layout = ElfSectionLayout::new(&obj);
    let text = layout.section_base(&obj.section_by_name(".text").unwrap());
    assert_eq!(text, 0x1000, "a non-colliding section keeps its sh_addr");
    assert_eq!(
        layout.symbol_address(&obj.symbol_by_name("f").unwrap()),
        text + 0x10
    );
}

/// A linked image's `st_value` is already the virtual address, so the layout
/// must pass it through untouched.
#[test]
fn a_linked_image_symbol_address_is_its_st_value() {
    let path = fixture("x64", "switch.elf");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).unwrap();
    let obj = owned.file();
    let layout = ElfSectionLayout::new(&obj);
    let mut checked = 0usize;
    for sym in obj.symbols() {
        assert_eq!(layout.symbol_address(&sym), sym.address());
        checked += 1;
    }
    assert!(checked > 0, "switch.elf has a symbol table");
}
