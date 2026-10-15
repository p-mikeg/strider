//! Runs `Strider::analyze` on every function symbol of every fixture ELF
//! under a fixture directory (one per `fixtures/arch/*.mk`) and prints one
//! line per function: `<arch>/<case>::<symbol>@<addr> <digest>`. The digest
//! covers the final function's raw DOT, its asm-fingerprints, the CFG DOT, the
//! six report fields, and the error of a failed analysis. Two builds diff line
//! for line; the summed analyze wall time goes to stderr.
//!
//! ```text
//! cargo run --release -p strider-orchestrator --example opt_identity > digests.txt
//! ```
//!
//! `$OPT_IDENTITY_IMAGE=<elf>:<arch>[:<step>]` digests every `<step>`th
//! function symbol of that one image instead, through one reused `Strider`;
//! `<arch>` is a fixture directory name. `$OPT_IDENTITY_DUMP=<dir>` also
//! writes each function's digested text there.

use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dot::{DotStyle, GraphDot};
use strider_ir::IRWalker;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_reader::ElfFileMemReader;

#[allow(dead_code)]
#[path = "../tests/common/fixtures.rs"]
mod fixtures;

use fixtures::{FIXTURE_ARCHES, Fixture, fixture_arch};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
struct Totals {
    functions: usize,
    failed: usize,
    elapsed: Duration,
}

fn digest_one(
    strider: &mut Strider<ElfFileMemReader>,
    cc: &strider_target::BuiltCallingConvention,
    addr: u64,
    key: &str,
    dump: Option<&Path>,
    totals: &mut Totals,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let result = strider.analyze(
        addr,
        cc,
        &LiftOptions::default(),
        &OptOptions::default(),
        None,
    );
    totals.elapsed += t0.elapsed();
    totals.functions += 1;
    let mut text = String::new();
    match result {
        Err(e) => {
            totals.failed += 1;
            write!(text, "error: {e:#}")?;
        }
        Ok(r) => {
            let f = &r.function;
            text.push_str(&f.raw_dot()?);
            for node in f.walk() {
                let mut fp: Vec<u64> = f.side_tables().asm_fingerprint(node).into_iter().collect();
                fp.sort_unstable();
                writeln!(text, "fp n{} {fp:x?}", node.as_u32())?;
            }
            let cfg = GraphDot::new(r.cfg.dot_dumper(strider.sleigh()), DotStyle::dark_cfg());
            text.push_str(&cfg.as_dot()?);
            writeln!(
                text,
                "unresolved {:?}\nisa {:?}\ninterior {:?}\nunmapped {:?}\nundecodable {:?}\nunverified {:?}",
                r.unresolved_indirect_branches,
                r.isa_mode_conflicts,
                r.interior_branch_targets,
                r.unmapped_branch_targets,
                r.undecodable_branch_targets,
                r.unverified_seeded_sites,
            )?;
        }
    }
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    println!("{key} {:016x}", hasher.finish());
    if let Some(dir) = dump {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(key.replace(['/', ':'], "_")), &text)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let dump = std::env::var_os("OPT_IDENTITY_DUMP").map(PathBuf::from);
    let mut totals = Totals::default();
    if let Ok(spec) = std::env::var("OPT_IDENTITY_IMAGE") {
        let mut parts = spec.rsplitn(3, ':').collect::<Vec<_>>();
        parts.reverse();
        let (path, arch, step) = match parts[..] {
            [path, arch] => (path, arch, 1),
            [path, arch, step] => (path, arch, step.parse()?),
            _ => return Err("OPT_IDENTITY_IMAGE is <elf>:<arch>[:<step>]".into()),
        };
        let fixture = Fixture::at(fixture_arch(arch), PathBuf::from(path));
        let (mut strider, cc) = fixture.strider();
        for (addr, name) in fixture.functions().into_iter().step_by(step) {
            let key = format!("{name}@{addr:#x}");
            digest_one(&mut strider, &cc, addr, &key, dump.as_deref(), &mut totals)?;
        }
    } else {
        for arch in FIXTURE_ARCHES {
            for fixture in Fixture::all(arch) {
                for (addr, name) in fixture.functions() {
                    let (mut strider, cc) = fixture.strider();
                    let key = format!("{}/{}::{name}@{addr:#x}", arch.dir, fixture.case);
                    digest_one(&mut strider, &cc, addr, &key, dump.as_deref(), &mut totals)?;
                }
            }
        }
    }
    eprintln!(
        "{} functions, {} failed, analyze wall time {:.3?}",
        totals.functions, totals.failed, totals.elapsed
    );
    Ok(())
}
