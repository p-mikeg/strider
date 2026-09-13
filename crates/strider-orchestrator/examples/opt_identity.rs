//! Runs `Strider::analyze` on every function symbol of every fixture ELF and
//! prints one line per function: `<arch>/<case>::<symbol>@<addr> <digest>`. The
//! digest covers the final function's raw DOT, its asm-fingerprints, the CFG
//! DOT, the six report fields, and the error of a failed analysis. Two builds
//! diff line for line; the summed analyze wall time goes to stderr.
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
use object::{Object, ObjectSymbol, SymbolKind};
use strider_ir::IRWalker;
use strider_orchestrator::opt::{OptOptions, ReadOnlyMemory};
use strider_orchestrator::{LiftOptions, Strider};
use strider_reader::ElfFileMemReader;
use strider_target::{CallingConvention as Cc, SleighArch as Sa};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

type ArchEntry = (&'static str, fn() -> Sa, fn() -> Cc);

const ARCHES: &[ArchEntry] = &[
    ("x86", Sa::x86, Cc::x86_cdecl),
    ("x86_kernel", Sa::x86, Cc::x86_linux_kernel),
    ("x64", Sa::x86_64, Cc::x86_64_systemv),
    ("aarch64", Sa::aarch64, Cc::aarch64_aapcs64),
    ("aarch64be", Sa::aarch64be, Cc::aarch64_aapcs64),
    ("arm", Sa::arm, Cc::arm_aapcs),
    ("arm_be", Sa::arm_be, Cc::arm_aapcs),
    ("arm_thumb", Sa::arm_thumb, Cc::arm_aapcs),
    ("mips32le", Sa::mipsle32, Cc::mips_o32),
    ("mips32be", Sa::mipsbe32, Cc::mips_o32),
    ("mips64le", Sa::mipsle64, Cc::mips_n64),
    ("mips64be", Sa::mipsbe64, Cc::mips_n64),
    ("ppc32be", Sa::ppc32be, Cc::powerpc_sysv32),
    ("ppc32le", Sa::ppc32le, Cc::powerpc_sysv32),
    ("ppc64be", Sa::ppc64be, Cc::powerpc64_elf_v2),
    ("ppc64le", Sa::ppc64le, Cc::powerpc64_elf_v2),
];

#[derive(Default)]
struct Totals {
    functions: usize,
    failed: usize,
    elapsed: Duration,
}

/// Function symbols sorted by address, one per address.
fn function_symbols(obj: &object::File<'_>) -> Vec<(u64, String)> {
    let mut symbols: Vec<(u64, String)> = obj
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
        .map(|s| (s.address(), s.name().unwrap_or_default().to_owned()))
        .collect();
    symbols.sort();
    symbols.dedup_by_key(|s| s.0);
    symbols
}

fn new_strider(
    obj: &object::File<'_>,
    sa: Sa,
    cc: fn() -> Cc,
) -> Result<(
    Strider<ElfFileMemReader>,
    strider_target::BuiltCallingConvention,
)> {
    let sleigh = rsleigh::Sleigh::new(
        sa.sla_spec(),
        sa.pspec(),
        ElfFileMemReader::from_object(obj)?,
    )?;
    let cc = cc().build(&sleigh.regs()?)?;
    let rom: Box<dyn ReadOnlyMemory> = Box::new(ElfFileMemReader::from_object(obj)?);
    Ok((Strider::new(sa, sleigh, Some(rom))?, cc))
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

fn arch_entry(name: &str) -> Result<(Sa, fn() -> Cc)> {
    ARCHES
        .iter()
        .find(|a| a.0 == name)
        .map(|a| (a.1(), a.2))
        .ok_or_else(|| format!("unknown arch {name:?}").into())
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
        let (sa, cc) = arch_entry(arch)?;
        let owned = strider_reader::load_elf(path)?;
        let obj = owned.checked_file().expect("the mapped file is unchanged");
        let (mut strider, cc) = new_strider(&obj, sa, cc)?;
        for (addr, name) in function_symbols(&obj).into_iter().step_by(step) {
            let key = format!("{name}@{addr:#x}");
            digest_one(&mut strider, &cc, addr, &key, dump.as_deref(), &mut totals)?;
        }
    } else {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out");
        for &(arch_name, ..) in ARCHES {
            let (sa, cc) = arch_entry(arch_name)?;
            let mut cases: Vec<PathBuf> = std::fs::read_dir(root.join(arch_name))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "elf"))
                .collect();
            cases.sort();
            for path in cases {
                let case = path.file_stem().unwrap_or_default().to_string_lossy();
                let owned = strider_reader::load_elf(&path)?;
                let obj = owned.checked_file().expect("the mapped file is unchanged");
                for (addr, name) in function_symbols(&obj) {
                    let (mut strider, cc) = new_strider(&obj, sa, cc)?;
                    let key = format!("{arch_name}/{case}::{name}@{addr:#x}");
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
