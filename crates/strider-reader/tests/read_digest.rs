//! Pins every byte each fixture and crafted image serves, under every region
//! source, load filter and relocation setting, and through both
//! `ElfFileMemReader` views, plus each region source's writable ranges. One
//! row per (image, view) in `tests/data/read_digest.txt`: an FNV-1a over each
//! served address's 1-, 2-, 4- and 8-byte reads (or why each served nothing)
//! and the relocation type of a hole there, or `Err` and the refusal when the
//! load fails.
//!
//! Release only: it reads every mapped address of every image. Re-bless, then
//! review the diff:
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test -p strider-reader --release --test read_digest
//! git diff crates/strider-reader/tests/data/read_digest.txt
//! ```

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use common::elf_fixture::{self as fx, RelOpts, RelPlacement, RelTable};
use object::Endianness::{Big, Little};
use object::elf;
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::{LoadFilter, RegionSource, RelocationMode};
use strider_reader::{ElfFileMemReader, MemRegionsLookupTable, OwnedElf, ReadOnlyMemory};

const SOURCES: [(&str, RegionSource); 2] = [
    ("auto", RegionSource::Auto),
    ("sections", RegionSource::Sections),
];

const FILTERS: [(&str, LoadFilter); 4] = [
    ("code_ro", LoadFilter::CodeAndReadOnly),
    ("immutable", LoadFilter::ImmutableOnly),
    ("immutable_got", LoadFilter::ImmutableAndRelocatedGot),
    ("all", LoadFilter::AllAllocatable),
];

/// FNV-1a, 64-bit.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn eat(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// Tags `None` apart from an empty read.
    fn eat_opt(&mut self, v: Option<&[u8]>) {
        match v {
            None => self.eat(&[0]),
            Some(b) => {
                self.eat(&[1, b.len() as u8]);
                self.eat(b);
            }
        }
    }
}

/// Hashes every address of `ranges` that is `present` or holds a hole: the
/// 1-, 2-, 4- and 8-byte reads there, each the bytes it served or the reason
/// it served none, and the hole's relocation type.
fn probe(
    ranges: &[(u64, u64)],
    present: impl Fn(u64) -> bool,
    read: impl Fn(u64, &mut [u8]) -> Result<usize, u8>,
    hole_at: impl Fn(u64) -> Option<u32>,
) -> u64 {
    let mut h = Fnv::default();
    for addr in ranges.iter().flat_map(|&(lo, hi)| lo..hi) {
        let hole = hole_at(addr);
        if !present(addr) && hole.is_none() {
            continue;
        }
        h.eat(&addr.to_le_bytes());
        for len in [1, 2, 4, 8] {
            let mut buf = [0u8; 8];
            let buf = &mut buf[..len];
            match read(addr, buf) {
                Ok(n) => h.eat_opt(Some(&buf[..n])),
                Err(why) => h.eat(&[2, why]),
            }
        }
        h.eat_opt(hole.map(u32::to_le_bytes).as_ref().map(|b| &b[..]));
    }
    h.0
}

/// Why `ElfFileMemReader`'s ROM view refused a read. The trait reports only
/// an error, so this reads its message.
fn rom_refusal(e: &anyhow::Error) -> u8 {
    let msg = e.to_string();
    if msg.contains("relocation of type") {
        2
    } else if msg.contains("not mapped") || msg.contains("spans past mapped memory") {
        3
    } else if msg.contains("overflows the address space") {
        4
    } else {
        panic!("unclassified ROM refusal: {msg}")
    }
}

fn ram(off: u64) -> VnAddr {
    VnAddr {
        off,
        space: VnSpace::RAM,
    }
}

/// The first line of `e`, which names the refusal.
fn refusal(e: &anyhow::Error) -> String {
    e.to_string().lines().next().unwrap_or_default().to_owned()
}

fn row(out: &mut String, image: &str, view: &str, digest: Result<u64, String>) {
    match digest {
        Ok(h) => writeln!(out, "{image} {view} {h:016x}"),
        Err(e) => writeln!(out, "{image} {view} Err {e}"),
    }
    .expect("write to a String");
}

/// Every view's row for `elf`.
fn rows(out: &mut String, image: &str, elf: anyhow::Result<OwnedElf>) {
    let elf = match elf {
        Ok(elf) => elf,
        Err(e) => return row(out, image, "parse", Err(refusal(&e))),
    };
    let mut every_view = Vec::new();
    for (source_name, source) in SOURCES {
        for (filter_name, filter) in FILTERS {
            for (reloc_name, relocate) in [("raw", false), ("reloc", true)] {
                if filter == LoadFilter::ImmutableAndRelocatedGot && !relocate {
                    continue;
                }
                let digest = elf.regions(source, filter, relocate).map(|regions| {
                    let ranges = common::spans(&regions);
                    every_view.extend(ranges.iter().cloned());
                    let table = MemRegionsLookupTable::new(regions);
                    probe(
                        &ranges,
                        |a| table.read(a, &mut [0]).is_some(),
                        |a, b| table.read(a, b).ok_or(0),
                        |a| table.unmodelled_relocation_at(a),
                    )
                });
                let view = format!("{source_name}.{filter_name}.{reloc_name}");
                row(out, image, &view, digest.map_err(|e| refusal(&e)));
            }
        }
        // The barrier a ROM view built from these regions subtracts.
        let writable = elf.image(source, RelocationMode::Unapplied).map(|loaded| {
            let mut h = Fnv::default();
            for &(start, end) in loaded.writable.as_slice() {
                h.eat(&start.to_le_bytes());
                h.eat(&end.to_le_bytes());
            }
            h.0
        });
        row(
            out,
            image,
            &format!("{source_name}.writable"),
            writable.map_err(|e| refusal(&e)),
        );
    }
    // Every address some view maps.
    let ranges = common::merge_ranges(every_view);
    let reader = ElfFileMemReader::new(&elf, RelocationMode::Unapplied);
    let reader = reader.as_ref().map_err(refusal);
    let fetched = |r: &ElfFileMemReader, a| MemReader::read(r, ram(a), &mut [0]).is_ok();
    let fetch = reader.clone().map(|r| {
        probe(
            &ranges,
            |a| fetched(r, a),
            |a, b| MemReader::read(r, ram(a), b).map_err(|_| 0),
            |_| None,
        )
    });
    // Over the fetch image, so a ROM refusal there is hashed with its reason.
    let rom = reader.map(|r| {
        probe(
            &ranges,
            |a| fetched(r, a),
            |a, b| {
                ReadOnlyMemory::read(r, a, b)
                    .map(|()| b.len())
                    .map_err(|e| rom_refusal(&e))
            },
            |_| None,
        )
    });
    row(out, image, "file_reader.fetch", fetch);
    row(out, image, "file_reader.rom", rom);
}

/// The overlapping, colliding and shared-file-range shapes, and one relocation
/// per `relocs` entry under every placement.
fn crafted() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = vec![
        (
            "overlapping_loads_rela".into(),
            fx::build_overlapping_loads_rela_elf(64, 512),
        ),
        (
            "overlapping_loads_rx_rw".into(),
            fx::build_overlapping_loads_elf(&[
                (elf::PF_R | elf::PF_X, 0x100, 0xaa),
                (elf::PF_R | elf::PF_W, 0x80, 0xbb),
            ]),
        ),
        (
            "equal_vaddr_loads".into(),
            fx::build_equal_vaddr_loads_elf(&[0x20, 0x4]),
        ),
        (
            "shared_file_range".into(),
            fx::build_shared_file_range_elf(8, 64, 0),
        ),
        (
            "shared_file_range_staggered".into(),
            fx::build_shared_file_range_elf(8, 64, 1),
        ),
        (
            "mips64be_rel32_64".into(),
            fx::build_mips64_rel32_elf(Big).bytes,
        ),
        (
            "mips64el_rel32_64".into(),
            fx::build_mips64_rel32_elf(Little).bytes,
        ),
        (
            "mips64el_transposed_kind".into(),
            fx::build_mips64el_transposed_kind_elf().bytes,
        ),
    ];
    let relocs = [
        (
            "i386_32",
            Little,
            false,
            elf::EM_386,
            elf::R_386_32,
            true,
            vec![0x2c, 0, 0, 0],
            RelTable::Rel,
        ),
        (
            "i386_32_crel",
            Little,
            false,
            elf::EM_386,
            elf::R_386_32,
            true,
            vec![0x2c, 0, 0, 0],
            RelTable::Crel(None),
        ),
        (
            "i386_32_stn_undef",
            Little,
            false,
            elf::EM_386,
            elf::R_386_32,
            false,
            vec![0x2c, 0, 0, 0],
            RelTable::Rel,
        ),
        (
            "i386_gotoff_stn_undef",
            Little,
            false,
            elf::EM_386,
            elf::R_386_GOTOFF,
            false,
            vec![0x2c, 0, 0, 0],
            RelTable::Rel,
        ),
        (
            "i386_relative",
            Little,
            false,
            elf::EM_386,
            elf::R_386_RELATIVE,
            false,
            vec![0x88, 0x11, 0, 0],
            RelTable::Rel,
        ),
        (
            "x86_64_glob_dat",
            Little,
            true,
            elf::EM_X86_64,
            elf::R_X86_64_GLOB_DAT,
            true,
            vec![0; 8],
            RelTable::Rel,
        ),
        (
            "x86_64_64_crel_addend",
            Little,
            true,
            elf::EM_X86_64,
            elf::R_X86_64_64,
            true,
            vec![0x2c, 0, 0, 0, 0, 0, 0, 0],
            RelTable::Crel(Some(-8)),
        ),
        (
            "mips32be_rel32",
            Big,
            false,
            elf::EM_MIPS,
            elf::R_MIPS_REL32,
            true,
            vec![0; 4],
            RelTable::Rel,
        ),
        (
            "mips32be_rel32_stn_undef",
            Big,
            false,
            elf::EM_MIPS,
            elf::R_MIPS_REL32,
            false,
            vec![0; 4],
            RelTable::Rel,
        ),
        (
            "mips64be_rel32_kseg",
            Big,
            true,
            elf::EM_MIPS,
            elf::R_MIPS_REL32,
            true,
            vec![0x80, 0, 0, 0],
            RelTable::Rel,
        ),
        (
            "mips64be_copy_ssym",
            Big,
            true,
            elf::EM_MIPS,
            (1 << 24) | elf::R_MIPS_COPY,
            true,
            vec![0; 8],
            RelTable::Rel,
        ),
        (
            "mips64el_copy",
            Little,
            true,
            elf::EM_MIPS,
            elf::R_MIPS_COPY,
            true,
            vec![0; 8],
            RelTable::Rel,
        ),
        (
            "mips64el_26",
            Little,
            true,
            elf::EM_MIPS,
            elf::R_MIPS_26,
            true,
            vec![0; 4],
            RelTable::Rel,
        ),
        (
            "ppc64_rel32",
            Big,
            true,
            elf::EM_PPC64,
            elf::R_PPC64_REL32,
            true,
            vec![0; 4],
            RelTable::Rel,
        ),
    ];
    let placements = [
        ("rw", false, false),
        ("rx", true, false),
        ("rw_outer", false, true),
        ("rx_outer", true, true),
    ];
    for (name, endian, is_64, e_machine, r_type, defined_symbol, slot_init, table) in relocs {
        for (place, slot_exec, outer_load) in placements {
            let fixture = fx::build_rel_elf_placed(
                RelOpts {
                    endian,
                    is_64,
                    e_machine,
                    r_type,
                    defined_symbol,
                    slot_init: slot_init.clone(),
                },
                RelPlacement {
                    slot_exec,
                    outer_load,
                    table,
                    ..Default::default()
                },
            );
            out.push((format!("{name}.{place}"), fixture.bytes));
        }
    }
    out
}

/// Each row's digest, keyed by its `<image> <view>`.
fn by_image_view(digest: &str) -> BTreeMap<&str, &str> {
    digest
        .lines()
        .map(|l| {
            let view_end = l.match_indices(' ').nth(1).map_or(l.len(), |(i, _)| i);
            (&l[..view_end], l[view_end..].trim_start())
        })
        .collect()
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "release only: reads every mapped address of every image"
)]
fn every_image_serves_its_recorded_bytes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = common::fixtures_out();
    let files = common::fixture_images();
    assert!(
        !files.is_empty(),
        "{} holds no fixtures; run `make -C fixtures`",
        root.display()
    );
    let mut actual = String::new();
    for path in files {
        let image = path
            .strip_prefix(&root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        rows(&mut actual, &image, OwnedElf::open(&path));
    }
    for (name, bytes) in crafted() {
        rows(
            &mut actual,
            &format!("crafted/{name}"),
            OwnedElf::parse(bytes),
        );
    }

    let path = manifest.join("tests/data/read_digest.txt");
    if std::env::var_os("UPDATE_EXPECT").is_some() {
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("data dir");
        std::fs::write(&path, &actual).expect("write the digest");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let old = by_image_view(&expected);
    let new = by_image_view(&actual);
    let changed: Vec<String> = old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|k| old.get(*k) != new.get(*k))
        .map(|k| {
            let side = |m: &BTreeMap<&str, &str>| m.get(k).copied().unwrap_or("(none)").to_owned();
            format!("{k} {} -> {}", side(&old), side(&new))
        })
        .collect();
    assert!(
        changed.is_empty(),
        "{} digest row(s) differ; review, then bless with\n\
         UPDATE_EXPECT=1 cargo test -p strider-reader --release --test read_digest\n\n{}",
        changed.len(),
        changed.join("\n")
    );
}
