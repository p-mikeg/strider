//! Both loadable-region presets share one section walker, so the propagation /
//! empty-data / overflow contracts are pinned through whichever preset's filter
//! accepts the synthetic section under test.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use strider_reader::elf::LoadFilter;

fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

/// A one-section `ET_REL` object, which routes the loader down the section
/// walk. `payload` reserves real bytes and points `sh_offset` at them; `None` points
/// it past EOF, so `data()` fails for the section.
fn one_section_rel(name: &[u8], sh_flags: u64, sh_addr: u64, payload: Option<&[u8]>) -> Vec<u8> {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(name);
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        let data_off = payload.map(|p| w.reserve(p.len(), 1));
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        if let Some(p) = payload {
            w.write(p);
        }
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags,
            sh_addr,
            // `None` is past EOF on every buffer these tests build; a
            // `Some` is a real offset into the payload two callers pass.
            sh_offset: data_off.map_or(0xdead_beef, |o| o as u64),
            sh_size: payload.map_or(4, |p| p.len() as u64),
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    buf
}

#[test]
fn elf_code_and_readonly_sections_include_text_and_rodata_exclude_data_and_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),   // exec     -> include
        SectionSpec::rodata(0x2000, vec![3, 4]), // ro data  -> include
        SectionSpec::data(0x3000, vec![5, 6]),   // writable -> exclude
        SectionSpec::bss(0x4000, 16),            // NOBITS   -> exclude (empty data)
    ]);
    let regions = common::regions(&bytes, LoadFilter::CodeAndReadOnly);

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    let base = |name| common::section_base(&bytes, name);
    assert!(addrs.contains(&base(".text")), ".text must be included");
    assert!(addrs.contains(&base(".rodata")), ".rodata must be included");
    assert!(!addrs.contains(&base(".data")), ".data must be excluded");
    assert!(!addrs.contains(&base(".bss")), ".bss must be excluded");
    assert_eq!(regions.len(), 2);
}

/// Each preset with flags only it accepts: read-only for `CodeAndReadOnly`,
/// writable for `AllAllocatable`.
fn presets() -> [(LoadFilter, u64); 2] {
    let alloc = u64::from(object::elf::SHF_ALLOC);
    [
        (LoadFilter::CodeAndReadOnly, alloc),
        (
            LoadFilter::AllAllocatable,
            alloc | u64::from(object::elf::SHF_WRITE),
        ),
    ]
}

/// A failing `section.data()` on an accepted section propagates rather than
/// skipping; only NOBITS (`Ok(&[])`) skips. The section's offset is past EOF.
#[test]
fn presets_propagate_a_data_error() {
    for (filter, flags) in presets() {
        let buf = one_section_rel(b".broken", flags, 0x1000, None);
        let err = common::try_regions(&buf, filter).expect_err("an accepted malformed section");
        assert!(
            err.to_string().contains("failed to parse ELF"),
            "{filter:?}: {err}"
        );
    }
}

/// An `sh_addr + sh_size` overflow propagates as `MemRegion::new`'s error:
/// `sh_addr` is `u64::MAX - 1` with 4 bytes that fit in the file.
#[test]
fn presets_propagate_a_region_overflow() {
    for (filter, flags) in presets() {
        let buf = one_section_rel(b".overflow", flags, u64::MAX - 1, Some(&[0; 4]));
        let msg = common::try_regions(&buf, filter)
            .expect_err("addr+len overflows")
            .to_string();
        assert!(
            msg.contains("would overflow u64")
                && msg.contains(&format!("{:#x}", u64::MAX - 1))
                && msg.contains("length 4"),
            "{filter:?}: {msg}"
        );
    }
}

/// A rejected section's `data()` is never read, so a malformed one is an
/// empty `Ok`.
#[test]
fn code_and_readonly_preset_skips_rejected_malformed_section() {
    let buf = one_section_rel(
        b".broken",
        u64::from(object::elf::SHF_ALLOC) | u64::from(object::elf::SHF_WRITE),
        0x1000,
        None,
    );
    // Writable, so the code+rodata preset rejects it before reading.
    let regions = common::try_regions(&buf, LoadFilter::CodeAndReadOnly)
        .expect("filter-rejected malformed section must not surface an error");
    assert!(regions.is_empty(), "nothing was accepted");
}

/// Two sections sharing a `sh_addr` are rebased apart, not collapsed.
///
/// A `.o` commonly has `.text`, `.text.startup`, `.text.foo` all at VMA 0
/// pre-link. Each gets its own base and serves its own data; collapsing them
/// leaves every one after the first unreachable, its address answering with
/// the winner's bytes.
#[test]
fn et_rel_sections_sharing_a_start_are_rebased_apart() {
    let bytes = build_elf_with_sections(&[
        SectionSpec {
            name: b".first",
            addr: 0x1000,
            data: vec![0xaa],
            exec: true,
            writable: false,
            nobits: false,
            tls: false,
            sh_size: None,
        },
        SectionSpec {
            name: b".second",
            addr: 0x1000,
            data: vec![0xbb],
            exec: false,
            writable: false,
            nobits: false,
            tls: false,
            sh_size: None,
        },
    ]);
    let obj = parse(&bytes);
    let layout = strider_reader::elf::ElfSectionLayout::new(&obj);
    let regions = common::regions(&bytes, LoadFilter::CodeAndReadOnly);
    assert_eq!(regions.len(), 2, "both sections stay reachable");
    let table = strider_reader::MemRegionsLookupTable::new(regions);

    // `.first` keeps the declared VMA; `.second` follows it, 1-byte aligned.
    for (name, want) in [(".first", 0xaau8), (".second", 0xbbu8)] {
        let sec = {
            use object::Object as _;
            obj.section_by_name(name).expect(name)
        };
        let base = layout.section_base(&sec);
        let mut buf = [0u8; 1];
        assert_eq!(table.read(base, &mut buf), Some(1), "{name} at {base:#x}");
        assert_eq!(buf[0], want, "{name} must serve its own byte");
    }
    let sec_base = |name: &str| {
        use object::Object as _;
        layout.section_base(&obj.section_by_name(name).unwrap())
    };
    assert_eq!(
        sec_base(".second"),
        sec_base(".first") + 1,
        "the loser of the collision is placed just past the winner"
    );
}

// The allocatable preset accepts every `SHF_ALLOC` section, so it adds
// `.data` / `.got.plt` / `.data.rel.ro` to the code+rodata footprint. These
// re-pin the shared walker contracts through its filter, so a future
// filter-vs-walker refactor cannot drop one preset unnoticed.

#[test]
fn allocatable_sections_include_text_rodata_data_and_exclude_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),
        SectionSpec::rodata(0x2000, vec![3, 4]),
        SectionSpec::data(0x3000, vec![5, 6]),
        SectionSpec::bss(0x4000, 16),
    ]);
    let regions = common::regions(&bytes, LoadFilter::AllAllocatable);

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    let base = |name| common::section_base(&bytes, name);
    assert!(addrs.contains(&base(".text")), ".text must be included");
    assert!(addrs.contains(&base(".rodata")), ".rodata must be included");
    assert!(addrs.contains(&base(".data")), ".data must be included");
    assert!(
        !addrs.contains(&base(".bss")),
        ".bss (NOBITS) must be excluded"
    );
    assert_eq!(regions.len(), 3);
}

/// An RWX mapping is fetchable but is not runtime-immutable, so it belongs in
/// the fetch image and not in a `ReadOnlyMemory` view: `LoadReadOnly` folds a
/// constant-address load without consulting the memory chain, and a store then
/// reload of a mutable global would fold to the file-initial byte.
#[test]
fn read_only_presets_exclude_a_writable_executable_mapping() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::rwx(0x1000, vec![0x90; 8]),
        SectionSpec::rodata(0x2000, vec![1, 2, 3, 4]),
    ]);

    let fetch: Vec<u64> = common::regions(&bytes, LoadFilter::CodeAndReadOnly)
        .iter()
        .map(|r| r.start_addr())
        .collect();
    let base = |name| common::section_base(&bytes, name);
    assert_eq!(
        fetch,
        vec![base(".text"), base(".rodata")],
        "RWX stays fetchable"
    );

    for (name, regions) in [
        (
            "auto-dispatch immutable regions",
            common::regions(&bytes, LoadFilter::ImmutableOnly),
        ),
        (
            "sections-only immutable regions",
            strider_reader::OwnedElf::parse(bytes.clone())
                .unwrap()
                .regions(
                    strider_reader::elf::RegionSource::Sections,
                    strider_reader::elf::LoadFilter::ImmutableOnly,
                    false,
                )
                .unwrap(),
        ),
        (
            "relocated immutable regions",
            common::load_readonly_with_relocations(&bytes),
        ),
    ] {
        let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
        assert_eq!(
            addrs,
            vec![base(".rodata")],
            "{name} must exclude the RWX mapping"
        );
    }
}

/// Section dedup is on the loaded address, so N headers naming ONE file range
/// at N addresses are N regions. Each is a window into the one image, so no
/// header's bytes are copied however many share the blob; the staggered shape
/// shifts every window one byte on.
#[test]
fn many_sections_over_one_file_range_load_as_windows() {
    use strider_reader::ReadOnlyMemory as _;
    for stagger in [0, 1] {
        let bytes = common::elf_fixture::build_shared_file_range_elf(64, 4096, stagger);
        let regions = common::regions(&bytes, LoadFilter::CodeAndReadOnly);
        assert_eq!(regions.len(), 64, "stagger {stagger}");
        let elf = strider_reader::OwnedElf::parse(bytes).expect("parse");
        let reader = strider_reader::ElfFileMemReader::new(
            &elf,
            strider_reader::elf::RelocationMode::Unapplied,
        )
        .expect("a window per header, over the one blob");
        for region in &regions {
            let mut buf = [0u8; 4];
            reader
                .read(region.end_addr() - 4, &mut buf)
                .expect("every header's window serves its bytes");
            assert_eq!(buf, [0x90; 4]);
        }
    }
}

/// Two PT_LOADs at one `p_vaddr` are one region, the widest: the lookup table
/// keeps a single region per start address, so the narrower one winning would
/// make the bytes past its end unfetchable.
#[test]
fn equal_vaddr_pt_loads_collapse_to_the_widest() {
    let bytes = common::elf_fixture::build_equal_vaddr_loads_elf(&[0x20, 0x4]);
    let regions = common::regions(&bytes, LoadFilter::CodeAndReadOnly);
    let base = common::elf_fixture::EQUAL_VADDR_LOAD_BASE;
    assert_eq!(
        regions
            .iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect::<Vec<_>>(),
        vec![(base, base + 0x20)]
    );

    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 0x20];
    table
        .read_exact(base, &mut got)
        .expect("every mapped byte is fetchable");
    assert_eq!(got, std::array::from_fn::<u8, 0x20, _>(|i| i as u8));
}

/// Two read-only PT_LOADs that one writable mapping cuts both leave a piece
/// at its end. The image keeps the later piece, as a lookup table does, so
/// every region a relocation can patch is one a read can reach, and a read at
/// that start serves the later piece's bytes.
#[test]
fn pieces_a_cut_leaves_at_one_start_collapse_to_the_last() {
    use object::elf::{PF_R, PF_W};
    let bytes = common::elf_fixture::build_loads_elf(&[
        (PF_R, 0x1000, 0x40, 0xaa),
        (PF_R, 0x1010, 0x40, 0xbb),
        (PF_R | PF_W, 0x1000, 0x20, 0xcc),
    ]);
    for relocate in [false, true] {
        let regions = strider_reader::OwnedElf::parse(bytes.clone())
            .expect("parse")
            .regions(
                strider_reader::elf::RegionSource::Auto,
                LoadFilter::ImmutableOnly,
                relocate,
            )
            .expect("regions");
        assert_eq!(
            regions
                .iter()
                .map(|r| (r.start_addr(), r.end_addr()))
                .collect::<Vec<_>>(),
            vec![(0x1020, 0x1050)],
            "relocate={relocate}"
        );
        assert_eq!(common::region_bytes(&regions[0]), vec![0xbb; 0x30]);
        let mut byte = [0u8; 1];
        strider_reader::MemRegionsLookupTable::new(regions)
            .read_exact(0x1020, &mut byte)
            .expect("the kept piece");
        assert_eq!(byte, [0xbb], "relocate={relocate}");
    }
}

/// A mapping ending exactly at 2^64 cannot map its last byte, whose exclusive
/// end no `u64` holds. It loses that byte; the rest of the image still loads.
#[test]
fn a_mapping_ending_at_the_top_of_the_address_space_loads_all_but_its_last_byte() {
    let top = u64::MAX - 3;
    let bytes = common::elf_fixture::simple_text_elf(top, &[0x11, 0x22, 0x33, 0x44]);
    for relocate in [false, true] {
        let regions = strider_reader::OwnedElf::parse(bytes.clone())
            .expect("parse")
            .regions(
                strider_reader::elf::RegionSource::Sections,
                LoadFilter::CodeAndReadOnly,
                relocate,
            )
            .expect("the image loads");
        let table = strider_reader::MemRegionsLookupTable::new(regions);
        let mut buf = [0u8; 3];
        table
            .read_exact(top, &mut buf)
            .expect("the first three bytes");
        assert_eq!(buf, [0x11, 0x22, 0x33]);
        assert_eq!(table.read(u64::MAX, &mut [0u8; 1]), None);
    }
}

/// The section walk serves no section a writable PT_LOAD maps, whatever its
/// `sh_flags` say: a non-RELRO image's `.rodata` in an `RW` segment.
#[test]
fn the_section_walk_keeps_a_writable_segment_out_of_rom() {
    use object::elf::{PF_R, PF_W};
    let bytes = common::elf_fixture::build_loads_elf(&[(PF_R | PF_W, 0x1000, 0x20, 0xaa)]);
    let elf = strider_reader::OwnedElf::parse(bytes).expect("parse");
    for relocate in [false, true] {
        let rom = elf
            .regions(
                strider_reader::elf::RegionSource::Sections,
                LoadFilter::ImmutableOnly,
                relocate,
            )
            .expect("regions");
        assert!(rom.is_empty(), "relocate={relocate}: {rom:?}");
    }
    let code = elf
        .regions(
            strider_reader::elf::RegionSource::Sections,
            LoadFilter::CodeAndReadOnly,
            false,
        )
        .expect("regions");
    assert_eq!(code.len(), 1, "the executable section stays fetchable");
}
