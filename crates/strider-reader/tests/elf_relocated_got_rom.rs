//! `LoadFilter::ImmutableAndRelocatedGot`: the opt-in that serves a relocated
//! `.got` / `.data.rel.ro` as read-only memory.
//!
//! Every test here has an `ImmutableOnly` half, since the whole point of the
//! opt-in is that nothing about the default view moves.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::elf_fixture::SectionSpec;
use object::{Object, ObjectSection};
use strider_reader::elf::{self, LoadFilter};
use strider_reader::{MemRegion, MemRegionsLookupTable};

const DEFAULT: LoadFilter = LoadFilter::ImmutableOnly;
const OPT_IN: LoadFilter = LoadFilter::ImmutableAndRelocatedGot;

fn read_u32_be(regions: &[MemRegion], addr: u64) -> Option<u32> {
    let mut buf = [0u8; 4];
    MemRegionsLookupTable::new(regions.to_vec())
        .read_exact(addr, &mut buf)
        .ok()
        .map(|()| u32::from_be_bytes(buf))
}

fn read_u64_le(regions: &[MemRegion], addr: u64) -> Option<u64> {
    let mut buf = [0u8; 8];
    MemRegionsLookupTable::new(regions.to_vec())
        .read_exact(addr, &mut buf)
        .ok()
        .map(|()| u64::from_le_bytes(buf))
}

/// `ranges` sorted, with everything overlapping or touching merged.
fn merge(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// Every region's extent, ascending and merged.
fn spans(regions: &[MemRegion]) -> Vec<(u64, u64)> {
    merge(
        regions
            .iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect(),
    )
}

/// `a` minus `b`, both merged and ascending.
fn subtract(a: &[(u64, u64)], b: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for &(lo, hi) in a {
        let mut lo = lo;
        for &(c_lo, c_hi) in b {
            if c_lo >= hi || c_hi <= lo {
                continue;
            }
            if c_lo > lo {
                out.push((lo, c_lo));
            }
            lo = lo.max(c_hi);
        }
        if lo < hi {
            out.push((lo, hi));
        }
    }
    out
}

/// Whether any region serves `[addr, addr + len)` whole.
fn serves(regions: &[MemRegion], addr: u64, len: usize) -> bool {
    MemRegionsLookupTable::new(regions.to_vec())
        .read_exact(addr, &mut vec![0u8; len])
        .is_ok()
}

/// The `.data.rel.ro` slot of an ET_DYN with one `R_MIPS_REL32` over it: the
/// relocation is applied either way, but only the opt-in leaves a region to
/// read it through.
#[test]
fn the_opt_in_serves_a_relocated_data_rel_ro_slot_the_default_does_not() {
    let fx = common::elf_fixture::build_mips32be_rel32_elf();

    let default = common::relocated(&fx.bytes, DEFAULT);
    assert!(
        !serves(&default, fx.slot_addr, 4),
        "the default ROM must not serve a writable `.data.rel.ro`"
    );

    let opted_in = common::relocated(&fx.bytes, OPT_IN);
    assert_eq!(
        read_u32_be(&opted_in, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "the opt-in must serve the slot at its RELOCATED value"
    );
}

/// The LP64 twin: an `R_X86_64_GLOB_DAT` slot reads back as the resolved
/// symbol address over all eight bytes, little-endian.
#[test]
fn the_opt_in_serves_a_relocated_glob_dat_slot_at_its_full_width() {
    // GLOB_DAT writes S, so the upper half of the file bytes must be cleared:
    // a four-byte or byte-swapped write leaves some of `init` behind.
    let init: u64 = 0x1234_5678_0000;
    let fx = common::elf_fixture::build_x86_64_glob_dat_elf(init.to_le_bytes().to_vec());
    assert_eq!(
        read_u64_le(
            &common::regions(&fx.bytes, LoadFilter::AllAllocatable),
            fx.slot_addr
        ),
        Some(init),
        "unrelocated, the slot is still its file bytes"
    );

    let default = common::relocated(&fx.bytes, DEFAULT);
    assert!(
        !serves(&default, fx.slot_addr, 8),
        "the default ROM must not serve a writable `.data.rel.ro`"
    );

    let opted_in = common::relocated(&fx.bytes, OPT_IN);
    assert_eq!(
        read_u64_le(&opted_in, fx.slot_addr),
        Some(fx.sym_addr),
        "the opt-in must serve the slot at its RELOCATED value"
    );
}

/// The opt-in claims the loader has relocated those slots, and withholding
/// relocations denies it: served unrelocated, the slot reads as its file
/// bytes, 0 on a PIE where the address lives in the addend, and a load
/// through it folds to that 0.
#[test]
fn the_opt_in_is_refused_without_relocations() {
    let fx = common::elf_fixture::build_mips32be_rel32_elf();
    let err = common::try_regions(&fx.bytes, OPT_IN)
        .expect_err("the opt-in must be refused unrelocated")
        .to_string();
    assert!(
        err.contains("ImmutableAndRelocatedGot") && err.contains("relocate=false"),
        "{err}"
    );
    // The default view is unaffected, relocated or not.
    assert!(!serves(
        &common::regions(&fx.bytes, DEFAULT),
        fx.slot_addr,
        4
    ));
}

/// `.got.plt` is rewritten by lazy binding, so it stays out of the ROM even
/// under the opt-in; `.data` and `.bss` stay out because they are just
/// writable.
#[test]
fn the_opt_in_takes_got_and_data_rel_ro_and_no_other_writable_section() {
    let got = vec![0x11u8; 8];
    let got_plt = vec![0x22u8; 8];
    let rel_ro = vec![0x33u8; 8];
    let data = vec![0x44u8; 8];
    let bytes = common::elf_fixture::build_elf_with_sections(&[
        SectionSpec::text(0, vec![0x90; 8]),
        SectionSpec::got(0, got.clone()),
        SectionSpec::got_plt(0, got_plt),
        SectionSpec::data_rel_ro_local(0, rel_ro.clone()),
        SectionSpec::data(0, data),
        SectionSpec::bss(0, 8),
    ]);
    let at = |name: &str| common::section_base(&bytes, name);

    for (filter, want) in [(DEFAULT, false), (OPT_IN, true)] {
        let regions = common::relocated(&bytes, filter);
        assert_eq!(serves(&regions, at(".got"), 8), want, "{filter:?} .got");
        assert_eq!(
            serves(&regions, at(".data.rel.ro.local"), 8),
            want,
            "{filter:?} .data.rel.ro.local"
        );
        for name in [".got.plt", ".data", ".bss"] {
            assert!(
                !serves(&regions, at(name), 8),
                "{filter:?} must not serve {name}"
            );
        }
        assert!(serves(&regions, at(".text"), 8), "{filter:?} .text");
    }

    let opted_in = common::relocated(&bytes, OPT_IN);
    let mut buf = vec![0u8; 8];
    MemRegionsLookupTable::new(opted_in.clone())
        .read_exact(at(".got"), &mut buf)
        .expect("`.got` is served");
    assert_eq!(buf, got, "`.got` must serve its OWN bytes");
    MemRegionsLookupTable::new(opted_in)
        .read_exact(at(".data.rel.ro.local"), &mut buf)
        .expect("`.data.rel.ro.local` is served");
    assert_eq!(buf, rel_ro);
}

/// A relocation the applier cannot compute must stay a hole over a served GOT
/// slot: the ROM answers from these regions alone, so file bytes there would
/// fold to a value no loader ever writes. `R_MIPS_26` on a data slot is
/// unmodelled, and covers the first four of the slot's eight bytes, so the
/// region is provably there and provably holed.
#[test]
fn an_unmodelled_relocation_over_a_served_slot_is_a_hole() {
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Big,
        is_64: false,
        e_machine: object::elf::EM_MIPS,
        r_type: object::elf::R_MIPS_26,
        defined_symbol: true,
        slot_init: vec![0xaa; 8],
    });
    let regions = common::relocated(&fx.bytes, OPT_IN);
    assert!(
        serves(&regions, fx.slot_addr + 4, 4),
        "the opt-in serves the slot's region"
    );
    assert!(
        !serves(&regions, fx.slot_addr, 4),
        "the unmodelled field must read as a hole, not {:?}",
        read_u32_be(&regions, fx.slot_addr)
    );
}

/// A stripped image has no `.got` section header to name, so the opt-in has
/// nothing to add and must degenerate to the default view.
#[test]
fn the_opt_in_is_the_default_view_when_no_got_section_is_named() {
    let bytes = common::elf_fixture::simple_text_elf(0x1000, &[0x90; 16]);
    assert_eq!(
        common::relocated(&bytes, OPT_IN)
            .iter()
            .map(|r| (r.start_addr(), common::region_bytes(r)))
            .collect::<Vec<_>>(),
        common::relocated(&bytes, DEFAULT)
            .iter()
            .map(|r| (r.start_addr(), common::region_bytes(r)))
            .collect::<Vec<_>>(),
    );
}

/// The real MIPS64 PIC image: `.got` sits in the one RW PT_LOAD next to
/// `.ctors`, `.dtors`, `.data`, `.rld_map`, `.sdata` and `.bss`, and the
/// opt-in must lift exactly `.got` out of it.
#[test]
fn a_linked_mips_pic_image_serves_only_its_got_under_the_opt_in() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/mips64le/indirect_branch.elf");
    // The only linked PIC image here, so a skip would leave the case untested.
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    let extent = |name: &str| {
        let sec = obj
            .section_by_name(name)
            .unwrap_or_else(|| panic!("no section {name}"));
        (sec.address(), sec.size() as usize)
    };

    let default = owned
        .regions(elf::RegionSource::Auto, DEFAULT, true)
        .expect("regions");
    let opted_in = owned
        .regions(elf::RegionSource::Auto, OPT_IN, true)
        .expect("regions");

    let (got_addr, got_len) = extent(".got");
    assert!(!serves(&default, got_addr, got_len), "default .got");
    assert!(serves(&opted_in, got_addr, got_len), "opt-in .got");

    for name in [".ctors", ".dtors", ".data", ".rld_map", ".sdata", ".bss"] {
        let (addr, len) = extent(name);
        assert!(
            !serves(&opted_in, addr, len),
            "the opt-in must not serve {name}"
        );
    }

    // Everything the default served is still served, byte for byte.
    let before = MemRegionsLookupTable::new(default.clone());
    let after = MemRegionsLookupTable::new(opted_in);
    for region in &default {
        let len = (region.end_addr() - region.start_addr()) as usize;
        let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
        before
            .read_exact(region.start_addr(), &mut a)
            .expect("the default view reads its own region");
        after
            .read_exact(region.start_addr(), &mut b)
            .unwrap_or_else(|e| panic!("the opt-in dropped {:#x}: {e}", region.start_addr()));
        assert_eq!(a, b, "bytes at {:#x} moved", region.start_addr());
    }
}

/// Every built fixture, relocated: the opt-in may only ADD addresses to the
/// default view, and every address it adds must be inside a `.got` or
/// `.data.rel.ro*` extent. One assertion over the whole fixture tree, so a
/// section name creeping into the served set is caught wherever it appears.
#[test]
fn across_every_fixture_the_opt_in_only_adds_got_and_data_rel_ro_addresses() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out");
    let mut seen = 0usize;
    let mut arches: Vec<PathBuf> = match std::fs::read_dir(&out) {
        Ok(dirs) => dirs.flatten().map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    arches.sort();
    for arch in arches {
        let mut images: Vec<PathBuf> = std::fs::read_dir(&arch)
            .expect("an arch directory")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "elf" || e == "o"))
            .collect();
        images.sort();
        for path in images {
            let owned = strider_reader::load_elf(&path).expect("load_elf");
            let obj = owned.checked_file().expect("the mapped file is unchanged");
            let layout = elf::ElfSectionLayout::new(&obj);
            let allowed: Vec<(u64, u64)> = merge(
                obj.sections()
                    .filter(|s| {
                        s.name().is_ok_and(|n| {
                            n == ".got" || n == ".data.rel.ro" || n.starts_with(".data.rel.ro.")
                        })
                    })
                    .map(|s| {
                        let base = layout.section_base(&s);
                        (base, base.saturating_add(s.size()))
                    })
                    .collect(),
            );

            for source in [elf::RegionSource::Auto, elf::RegionSource::Sections] {
                let default = owned.regions(source, DEFAULT, true).expect("regions");
                let opted_in = owned.regions(source, OPT_IN, true).expect("regions");
                let before = MemRegionsLookupTable::new(default.clone());
                let after = MemRegionsLookupTable::new(opted_in.clone());
                let name = path.display();

                for region in &default {
                    let len = (region.end_addr() - region.start_addr()) as usize;
                    let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
                    let got_a = before.read(region.start_addr(), &mut a);
                    let got_b = after.read(region.start_addr(), &mut b);
                    assert_eq!(
                        (got_a, a),
                        (got_b, b),
                        "{name}: bytes at {:#x} moved",
                        region.start_addr()
                    );
                }

                for (lo, hi) in subtract(&spans(&opted_in), &spans(&default)) {
                    assert!(
                        allowed.iter().any(|&(s, e)| s <= lo && hi <= e),
                        "{name}: the opt-in added [{lo:#x}, {hi:#x}), \
                         in no .got / .data.rel.ro: {allowed:#x?}"
                    );
                }
            }
            seen += 1;
        }
    }
    assert!(seen > 0, "no fixture is built; run `make -C fixtures`");
}
