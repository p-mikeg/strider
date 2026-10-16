//! Pins every byte each fixture and crafted image serves, under every region
//! source, load filter and relocation setting, and through both
//! `ElfFileMemReader` views. One row per (image, view) in
//! `tests/data/read_digest.txt`: an FNV-1a over each served address's 1- and
//! 8-byte reads and the relocation type of a hole there, or `Err` when the
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
use std::ops::Range;
use std::path::{Path, PathBuf};

use common::elf_fixture::{self as fx, RelOpts, RelPlacement, RelTable};
use object::Endianness::{Big, Little};
use object::elf;
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::elf::{LoadFilter, RegionSource};
use strider_reader::{
    ElfFileMemReader, MemRegion, MemRegionsLookupTable, OwnedElf, ReadOnlyMemory,
};

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

/// The addresses some region covers, as sorted disjoint ranges.
fn covered_ranges(regions: &[MemRegion]) -> Vec<Range<u64>> {
    merged(
        regions
            .iter()
            .map(|r| r.start_addr()..r.end_addr())
            .collect(),
    )
}

/// `all` as sorted disjoint ranges.
fn merged(mut all: Vec<Range<u64>>) -> Vec<Range<u64>> {
    all.sort_by_key(|r| r.start);
    let mut out: Vec<Range<u64>> = Vec::new();
    for r in all {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Hashes every address of `ranges` that serves a byte or holds a hole.
fn probe(
    ranges: &[Range<u64>],
    read: impl Fn(u64, &mut [u8]) -> Option<usize>,
    hole_at: impl Fn(u64) -> Option<u32>,
) -> u64 {
    let mut h = Fnv::default();
    for addr in ranges.iter().cloned().flatten() {
        let mut one = [0u8; 1];
        let n = read(addr, &mut one);
        let hole = hole_at(addr);
        if n.is_none() && hole.is_none() {
            continue;
        }
        let mut eight = [0u8; 8];
        let m = read(addr, &mut eight);
        h.eat(&addr.to_le_bytes());
        h.eat_opt(n.map(|n| &one[..n]));
        h.eat_opt(m.map(|m| &eight[..m]));
        h.eat_opt(hole.map(u32::to_le_bytes).as_ref().map(|b| &b[..]));
    }
    h.0
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
                    let ranges = covered_ranges(&regions);
                    every_view.extend(ranges.iter().cloned());
                    let table = MemRegionsLookupTable::new(regions);
                    probe(
                        &ranges,
                        |a, b| table.read(a, b),
                        |a| table.unmodelled_relocation_at(a),
                    )
                });
                let view = format!("{source_name}.{filter_name}.{reloc_name}");
                row(out, image, &view, digest.map_err(|e| refusal(&e)));
            }
        }
        // The barrier a ROM view built from these regions subtracts.
        let writable = elf.writable_ranges(source).map(|ranges| {
            let mut h = Fnv::default();
            for (start, end) in ranges {
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
    // Every address some view maps, so a reader serving more than the views
    // shows.
    let ranges = merged(every_view);
    let obj = elf.checked_file().expect("parsed above");
    let reader = ElfFileMemReader::from_object(&obj);
    let reader = reader.as_ref().map_err(refusal);
    let fetch = reader
        .clone()
        .map(|r| probe(&ranges, |a, b| MemReader::read(r, ram(a), b).ok(), |_| None));
    let rom = reader.map(|r| {
        probe(
            &ranges,
            |a, b| ReadOnlyMemory::read(r, a, b).is_ok().then_some(b.len()),
            |_| None,
        )
    });
    row(out, image, "file_reader.fetch", fetch);
    row(out, image, "file_reader.rom", rom);
}

/// `fixtures/out/<arch>/*.{elf,o}`, sorted.
fn fixture_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for arch in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(arch.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "elf" || e == "o") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The overlapping, colliding and copy-amplifying shapes, and one relocation
/// per `relocs` entry under every placement.
fn crafted() -> Vec<(String, Vec<u8>)> {
    use object::elf::{PF_R, PF_W, PF_X};
    let mut out: Vec<(String, Vec<u8>)> = vec![
        (
            "overlapping_loads_rela".into(),
            fx::build_overlapping_loads_rela_elf(64, 512),
        ),
        (
            "overlapping_loads_rx_rw".into(),
            fx::build_overlapping_loads_elf(&[
                (PF_R | PF_X, 0x100, 0xaa),
                (PF_R | PF_W, 0x80, 0xbb),
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
    let root = manifest.join("../../fixtures/out");
    let files = fixture_files(&root);
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
