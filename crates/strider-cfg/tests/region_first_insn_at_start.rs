//! Every region's first instruction sits at the region's start, over every
//! function of every committed fixture: what lets the lift read a region's
//! start address in place of its first instruction's.

use object::{Object, ObjectSymbol, SymbolKind};
use strider_cfg::{CfgOptions, DecodeEngine};
use strider_target::SleighArch as Sa;

type ArchFn = fn() -> Sa;

const ARCHES: &[(&str, ArchFn)] = &[
    ("x86", Sa::x86),
    ("x86_kernel", Sa::x86),
    ("x64", Sa::x86_64),
    ("aarch64", Sa::aarch64),
    ("aarch64be", Sa::aarch64be),
    ("arm", Sa::arm),
    ("arm_be", Sa::arm_be),
    ("arm_be8", Sa::arm_be_kernel),
    ("arm_thumb", Sa::arm_thumb),
    ("mips32le", Sa::mipsle32),
    ("mips32be", Sa::mipsbe32),
    ("mips64le", Sa::mipsle64),
    ("mips64be", Sa::mipsbe64),
    ("ppc32be", Sa::ppc32be),
    ("ppc32le", Sa::ppc32le),
    ("ppc64be", Sa::ppc64be),
    ("ppc64le", Sa::ppc64le),
];

#[test]
fn every_fixture_regions_first_insn_is_at_its_start() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out");
    // One thread per arch: a Sleigh per image, built and run serially, is
    // most of this test's time.
    let root = &root;
    let regions: usize = std::thread::scope(|s| {
        let arches: Vec<_> = ARCHES
            .iter()
            .map(|&(dir, arch)| s.spawn(move || regions_checked(&root.join(dir), dir, arch)))
            .collect();
        arches
            .into_iter()
            .map(|t| t.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .sum()
    });
    assert!(regions > 5_000, "premise: the corpus was read ({regions})");
}

/// The non-empty regions of every function of every image under `dir`,
/// each checked to start at its first instruction.
fn regions_checked(dir_path: &std::path::Path, dir: &str, arch: ArchFn) -> usize {
    let mut regions = 0usize;
    let mut paths: Vec<_> = std::fs::read_dir(dir_path)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "elf"))
        .collect();
    paths.sort();
    for path in paths {
        let elf = strider_reader::load_elf(&path)
            .unwrap_or_else(|e| panic!("load_elf({}): {e:#}", path.display()));
        let file = elf.checked_file().expect("the mapped file is unchanged");
        let mut entries: Vec<u64> = file
            .symbols()
            .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
            .map(|s| s.address())
            .collect();
        entries.sort_unstable();
        entries.dedup();
        let mem = strider_reader::ElfFileMemReader::new(
            &elf,
            strider_reader::elf::RelocationMode::Unapplied,
        )
        .expect("mem reader");
        let mut engine = DecodeEngine::new(arch(), mem).expect("decode engine");
        for entry in entries {
            let Ok(cfg) = engine.build_cfg(entry.into(), &CfgOptions::default()) else {
                continue;
            };
            for region in cfg.regions() {
                if let Some(first) = region.insns.front() {
                    assert_eq!(
                        first.addr.machine_addr,
                        region.start_addr.machine_addr,
                        "{} {entry:#x}",
                        path.display()
                    );
                    regions += 1;
                }
            }
        }
    }
    regions
}
