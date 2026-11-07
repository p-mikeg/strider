//! Loading and reading a relocated image named by `STRIDER_BENCH_ELF` (a
//! kernel-size image with dynamic relocations); unset, nothing runs.
//!
//! ```text
//! STRIDER_BENCH_ELF=/path/to/vmlinux cargo bench -p strider-reader --bench read
//! ```

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use object::{Object as _, ObjectSection as _, SectionFlags};
use strider_reader::elf::{LoadFilter, RegionSource, RelocationMode};
use strider_reader::{ElfFileMemReader, MemRegionsLookupTable, load_elf};

/// At most this many reads of each kind per iteration.
const PROBES: usize = 1 << 20;

fn read_benches(c: &mut Criterion) {
    let Some(path) = std::env::var_os("STRIDER_BENCH_ELF") else {
        eprintln!("STRIDER_BENCH_ELF is unset: no relocated image to bench");
        return;
    };
    let elf = load_elf(&path).expect("open STRIDER_BENCH_ELF");
    let obj = elf.checked_file().expect("parse STRIDER_BENCH_ELF");
    // The first `PROBES` of every 4th executable address, in section order,
    // as instruction fetch reads them.
    let code: Vec<u64> = obj
        .sections()
        .filter(|s| {
            matches!(s.flags(), SectionFlags::Elf { sh_flags }
                if sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0)
        })
        .flat_map(|s| (s.address()..s.address().saturating_add(s.size())).step_by(4))
        .take(PROBES)
        .collect();
    // The first `PROBES` relocation sites, the reads that cross a patch.
    let sites: Vec<u64> = obj
        .dynamic_relocations()
        .into_iter()
        .flatten()
        .map(|(addr, _)| addr)
        .take(PROBES)
        .collect();
    assert!(
        !sites.is_empty(),
        "{path:?} walks no dynamic relocation; bench a relocated image"
    );
    let load = || {
        MemRegionsLookupTable::new(
            elf.regions(RegionSource::Auto, LoadFilter::AllAllocatable, true)
                .expect("relocated regions"),
        )
    };
    let table = load();

    let mut group = c.benchmark_group("relocated_image");
    group.sample_size(10);
    group.bench_function("load", |b| b.iter(|| black_box(load())));
    group.bench_function("fetch_code", |b| {
        b.iter(|| {
            let mut buf = [0u8; 4];
            for &addr in &code {
                black_box(table.read(addr, &mut buf));
            }
        });
    });
    group.bench_function("read_sites", |b| {
        b.iter(|| {
            let mut buf = [0u8; 8];
            for &addr in &sites {
                black_box(table.read(addr, &mut buf));
            }
        });
    });
    group.bench_function("file_reader", |b| {
        b.iter(|| {
            black_box(ElfFileMemReader::new(&elf, RelocationMode::Unapplied).expect("file reader"))
        });
    });
    group.finish();
}

criterion_group!(benches, read_benches);
criterion_main!(benches);
