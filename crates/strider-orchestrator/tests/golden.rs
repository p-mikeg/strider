//! Guards the fixture roster: `FIXTURE_ARCHES`, `Arch` and
//! `fixtures/arch/*.mk` name the same arches, and every image opens.

mod common;

use std::path::Path;

use common::ALL_ARCHES;
use common::fixtures::{FIXTURE_ARCHES, Fixture, fixtures_root};

/// The stems of `dir`'s entries that pass `keep`, sorted.
fn stems(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|p| keep(p.as_path()))
        .map(|p| {
            p.file_stem()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn roster_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = FIXTURE_ARCHES.iter().map(|a| a.dir.to_owned()).collect();
    dirs.sort();
    dirs
}

/// `make` builds one directory per `fixtures/arch/*.mk`; each is a roster row.
#[test]
fn the_roster_is_fixtures_arch_mk() {
    let root = fixtures_root();
    let mk = stems(&root.join("arch"), |p| {
        p.extension().is_some_and(|e| e == "mk")
    });
    assert_eq!(
        roster_dirs(),
        mk,
        "FIXTURE_ARCHES against fixtures/arch/*.mk"
    );
    let out = stems(&root.join("out"), Path::is_dir);
    assert_eq!(roster_dirs(), out, "FIXTURE_ARCHES against fixtures/out/");
}

#[test]
fn every_arch_preset_has_a_fixture_dir() {
    for preset in strider_target::ArchPreset::ALL {
        assert!(
            FIXTURE_ARCHES
                .iter()
                .any(|a| (a.sleigh)().preset() == *preset),
            "{preset:?} has no fixtures/arch/*.mk"
        );
    }
}

/// `per_arch_test!` and the sweeps run `Arch`, so it must be the roster.
#[test]
fn every_roster_arch_is_an_arch() {
    let arches: Vec<&str> = ALL_ARCHES.iter().map(|a| a.name()).collect();
    let rows: Vec<&str> = FIXTURE_ARCHES.iter().map(|a| a.dir).collect();
    assert_eq!(arches, rows);
}

#[test]
fn every_fixture_has_functions() {
    for arch in FIXTURE_ARCHES {
        for fixture in Fixture::all(arch) {
            assert!(
                !fixture.functions().is_empty(),
                "{} has no sized function symbol",
                fixture.path.display()
            );
        }
    }
}
