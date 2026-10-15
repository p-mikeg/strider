//! Pins what `Strider::analyze` makes of every function of the committed
//! fixtures under production defaults (`LiftOptions::default()`,
//! `OptOptions::default()`), one file per image:
//! `fixtures/golden/<arch>/<case>.txt`. A function's section is
//! `== <symbol> @<addr>`, each non-empty report channel, the CFG one region
//! per line (start, instruction count, terminator, successors) and the
//! optimised IR with registers named. An `analyze` error is pinned as its
//! message.
//!
//! A golden detects change; it does not judge it. Review a re-bless as a
//! diff:
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test -p strider-orchestrator --test golden
//! git diff fixtures/golden
//! ```
//!
//! Also guards the roster: `FIXTURE_ARCHES`, `Arch` and `fixtures/arch/*.mk`
//! name the same arches, and every image opens.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use common::ALL_ARCHES;
use common::fixtures::{FIXTURE_ARCHES, Fixture, fixture_arch, fixtures_root};
use strider_cfg::{Cfg, PcodeInsnAddr, RegionTerminator, ResolvedTarget};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{AnalyzeResult, LiftOptions};

/// `<addr>`, or `<addr>.<index>` past a machine instruction's first op.
fn pc(addr: &PcodeInsnAddr) -> String {
    match addr.insn_index {
        0 => format!("{:#x}", addr.machine_addr.addr),
        index => format!("{:#x}.{index}", addr.machine_addr.addr),
    }
}

fn resolved(target: &ResolvedTarget) -> String {
    match target.isa_bit {
        None => format!("{:#x}", target.addr),
        Some(bit) => format!("{:#x}/isa{}", target.addr, u8::from(bit)),
    }
}

fn terminator(terminator: &RegionTerminator) -> String {
    match terminator {
        RegionTerminator::Unconditional => "goto".to_owned(),
        RegionTerminator::CondBranch { true_target } => format!("cbranch {}", pc(true_target)),
        RegionTerminator::Return => "return".to_owned(),
        RegionTerminator::NoReturn => "noreturn".to_owned(),
        RegionTerminator::TailCall { target } => format!("tailcall {}", resolved(target)),
        RegionTerminator::Switch { targets, addr, .. } => {
            let arms: Vec<String> = targets.iter().map(resolved).collect();
            format!("switch@{} [{}]", pc(addr), arms.join(", "))
        }
        RegionTerminator::UnresolvedIndirectBranch { addr, .. } => {
            format!("unresolved@{}", pc(addr))
        }
    }
}

/// One line per region, by start.
fn cfg_lines(cfg: &Cfg, out: &mut String) {
    let graph = cfg.region_graph();
    let mut regions: Vec<_> = cfg.region_ids().collect();
    regions.sort_by_key(|&r| graph[r].start_addr);
    for r in regions {
        let region = &graph[r];
        let mut successors: Vec<PcodeInsnAddr> =
            graph.neighbors(r).map(|s| graph[s].start_addr).collect();
        successors.sort_unstable();
        let successors: Vec<String> = successors.iter().map(pc).collect();
        let _ = writeln!(
            out,
            "  {} {} insns {} -> [{}]",
            pc(&region.start_addr),
            region.insns.len(),
            terminator(&region.terminator),
            successors.join(", ")
        );
    }
}

/// Each non-empty report channel, by name.
fn report(result: &AnalyzeResult, out: &mut String) {
    let AnalyzeResult {
        cfg: _,
        function: _,
        unresolved_indirect_branches,
        isa_mode_conflicts,
        interior_branch_targets,
        unmapped_branch_targets,
        undecodable_branch_targets,
        unverified_seeded_sites,
    } = result;
    let channels = [
        ("unresolved_indirect_branches", unresolved_indirect_branches),
        ("isa_mode_conflicts", isa_mode_conflicts),
        ("interior_branch_targets", interior_branch_targets),
        ("unmapped_branch_targets", unmapped_branch_targets),
        ("undecodable_branch_targets", undecodable_branch_targets),
        ("unverified_seeded_sites", unverified_seeded_sites),
    ];
    for (name, sites) in channels {
        if !sites.is_empty() {
            let sites: Vec<String> = sites.iter().map(pc).collect();
            let _ = writeln!(out, "{name} [{}]", sites.join(", "));
        }
    }
}

/// `text`'s `endian` and `default_cc` lines, then the rest. `to_text` opens
/// with `endian`, `tracked`, `default_cc`.
fn split_shared(text: &str) -> (String, String) {
    let mut lines = text.split_inclusive('\n');
    let endian = lines.next().unwrap_or_default();
    let tracked = lines.next().unwrap_or_default();
    let cc = lines.next().unwrap_or_default();
    assert!(
        endian.starts_with("endian ")
            && tracked.starts_with("tracked ")
            && cc.starts_with("default_cc "),
        "to_text's header moved:\n{text}"
    );
    (
        format!("{endian}{cc}"),
        format!("{tracked}{}", lines.collect::<String>()),
    )
}

/// The image's golden text, analysing its functions in address order on one
/// engine.
fn golden_text(fixture: &Fixture) -> String {
    let (mut strider, cc) = fixture.strider();
    let mut shared: Option<String> = None;
    let mut body = String::new();
    for (addr, name) in fixture.functions() {
        let _ = writeln!(body, "\n== {name} @{addr:#x}");
        let result = match strider.analyze(
            addr,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        ) {
            Ok(result) => result,
            Err(e) => {
                let _ = writeln!(body, "error: {e:#}");
                continue;
            }
        };
        report(&result, &mut body);
        body.push_str("cfg\n");
        cfg_lines(&result.cfg, &mut body);
        let text = result.function.to_text_named(false, strider.sleigh_regs());
        let (head, rest) = split_shared(&text);
        if shared.is_none() {
            shared = Some(head);
        } else if shared.as_deref() != Some(head.as_str()) {
            body.push_str(&head);
        }
        body.push_str(&rest);
    }
    let mut out = format!(
        "# {}/{}: Strider::analyze, LiftOptions::default(), OptOptions::default()\n",
        fixture.arch.dir, fixture.case
    );
    out.push_str(shared.as_deref().unwrap_or_default());
    out.push_str(&body);
    out
}

/// `text` split at each `== ` line, keyed by that line; the file header is
/// keyed `(header)`.
fn sections(text: &str) -> BTreeMap<&str, &str> {
    let mut parts = text.split("\n== ");
    let mut map = BTreeMap::from([("(header)", parts.next().unwrap_or_default())]);
    for part in parts {
        map.insert(part.lines().next().unwrap_or_default(), part);
    }
    map
}

/// The sections that differ, then the first one as pinned and as it is now.
fn describe_change(path: &Path, expected: &str, actual: &str) -> String {
    let (was, now) = (sections(expected), sections(actual));
    let headers: BTreeSet<&str> = was.keys().chain(now.keys()).copied().collect();
    let differ: Vec<&str> = headers
        .into_iter()
        .filter(|h| was.get(h) != now.get(h))
        .collect();
    let first = differ.first().copied().unwrap_or_default();
    format!(
        "{}: {} section(s) differ: {}\n--- golden\n{}\n+++ now\n{}",
        path.display(),
        differ.len(),
        differ.join(" | "),
        was.get(first).copied().unwrap_or("(absent)"),
        now.get(first).copied().unwrap_or("(absent)"),
    )
}

/// Compares every image of `dir` with its golden, or writes it under
/// `UPDATE_EXPECT`, and fails naming each image that changed.
fn check_arch(dir: &str) {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut changed = Vec::new();
    for fixture in Fixture::all(fixture_arch(dir)) {
        let path = fixtures_root()
            .join("golden")
            .join(dir)
            .join(format!("{}.txt", fixture.case));
        let actual = golden_text(&fixture);
        let golden = expect_test::expect_file![path.clone()];
        if update {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("golden dir");
            golden.assert_eq(&actual);
        } else {
            let expected = golden.data();
            if expected != actual {
                changed.push(describe_change(&path, &expected, &actual));
            }
        }
    }
    assert!(
        changed.is_empty(),
        "{} golden file(s) differ; review, then bless with\n\
         UPDATE_EXPECT=1 cargo test -p strider-orchestrator --test golden\n\n{}",
        changed.len(),
        changed.join("\n\n")
    );
}

#[test]
fn x64() {
    check_arch("x64");
}

#[test]
fn aarch64be() {
    check_arch("aarch64be");
}

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
