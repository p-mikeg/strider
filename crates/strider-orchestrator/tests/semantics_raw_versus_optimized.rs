//! The optimiser must not change what a function computes.
//!
//! Structural diffing cannot answer that: the pipeline rewrites through
//! algebraic identities, so raw and optimised graphs for the same code look
//! nothing alike. This runs both on `strider_ir_test_utils::interp` instead, in
//! a few concrete environments, and every return slot, every call's target,
//! arguments and visible memory, and every byte written outside the function's
//! own frame has to come out the same.
//!
//! An environment the interpreter cannot run (a `CallOther`, an unresolved
//! indirect branch), or whose raw run yields a slot it cannot produce (a
//! float), is skipped rather than silently passing. Every test asserts a floor
//! on how many functions were really compared; the named ones also allow no
//! unproducible raw slot at all.

mod common;

use std::collections::BTreeMap;

use common::fixtures::Fixture;
use common::{Arch, lift_for_pipeline};
use strider_ir::{Function, ReadOnlyMemory};
use strider_ir_test_utils::interp::{self, CallRecord, EntryVar, Env, Outcome, Slot, mix};

/// Where the stack pointer is planted, and how wide a window either side of it
/// counts as the function's own frame. A spill there dies with the frame, so
/// the window is left out of every memory comparison. Anything written through
/// a pointer argument lands outside it. The base fits a 32-bit stack pointer.
const STACK_BASE: u64 = 0x7fff_1000;
const FRAME_HALF_WIDTH: u64 = 0x1_0000;

/// Control nodes one run may pass. A loop bounded by a small argument ends
/// well inside it; one bounded by a wide argument does not, and that run is
/// not compared.
const STEP_BUDGET: usize = 4096;

/// `(seed, small)`. A small environment draws every entry register but the
/// stack pointer from `0..16`, so a loop bounded by an argument ends; a wide
/// one reaches the high bits.
const ENVS: [(u64, bool); 4] = [(1, true), (2, true), (3, false), (4, false)];

/// The interpreter recurses along dataflow chains, which a long function
/// makes deep.
const INTERP_STACK: usize = 64 << 20;

/// Runs `body` on a thread with an [`INTERP_STACK`] stack. A `Function` is
/// not `Sync`, so the whole test runs there rather than each interpreter call.
fn on_interp_stack(body: impl FnOnce() + Send) {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK)
            .spawn_scoped(s, body)
            .expect("spawn the interpreter thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

fn entry_value(e: EntryVar, stack_vn: rsleigh::Vn, seed: u64, small: bool) -> u128 {
    if e.vn == stack_vn {
        return u128::from(STACK_BASE);
    }
    let x = mix(seed ^ e.vn.addr_off.rotate_left(17)) ^ mix(u64::from(e.vn.size) << 40);
    if small {
        u128::from(x % 16)
    } else {
        u128::from(x) | (u128::from(mix(x)) << 64)
    }
}

fn environment<'a>(
    entry: &'a dyn Fn(EntryVar) -> u128,
    rom: &'a dyn ReadOnlyMemory,
    seed: u64,
) -> Env<'a> {
    Env {
        entry,
        rom: Some(rom),
        seed,
        step_budget: STEP_BUDGET,
        private: Some(STACK_BASE - FRAME_HALF_WIDTH..=STACK_BASE + FRAME_HALF_WIDTH),
    }
}

/// What comparing one function came to.
enum Verdict {
    Compared,
    Skipped(String),
    Failed(String),
}

/// What was left unchecked across the functions compared.
#[derive(Default)]
struct Unchecked {
    /// Environments whose raw run diverged.
    diverged: usize,
    /// Raw return slots and call arguments the interpreter could not produce.
    slots: usize,
}

/// How much one environment's comparison looked at.
#[derive(Default)]
struct Coverage {
    /// Known raw slots, calls and written bytes that matched.
    checked: usize,
    /// Raw slots the interpreter could not produce.
    unchecked: usize,
}

/// Runs `raw` and `opt` in every environment of [`ENVS`]. An environment
/// counts only when it checked something and left no raw slot unchecked.
fn compare(
    raw: &Function,
    opt: &Function,
    rom: &dyn ReadOnlyMemory,
    unchecked: &mut Unchecked,
) -> Verdict {
    let stack_vn = raw.default_cc().stack_vn;
    let mut compared = false;
    let mut skipped = String::new();
    for (seed, small) in ENVS {
        let entry = |e: EntryVar| entry_value(e, stack_vn, seed, small);
        let env = environment(&entry, rom, seed);
        let want = match interp::run(raw, &env) {
            Ok(Outcome::Diverge(_)) => {
                unchecked.diverged += 1;
                skipped = "the raw run diverged".to_owned();
                continue;
            }
            Ok(outcome) => outcome,
            // An optimised graph that runs where the raw one cannot is no
            // divergence: folding float constants can remove what stopped the
            // raw run.
            Err(why) => {
                skipped = why.0;
                continue;
            }
        };
        let got = match interp::run(opt, &env) {
            Ok(outcome) => outcome,
            Err(why) => return Verdict::Failed(format!("ran raw but not optimised: {}", why.0)),
        };
        let coverage = match agree(&want, &got) {
            Ok(coverage) => coverage,
            Err(e) => return Verdict::Failed(format!("environment {seed}: {e}")),
        };
        unchecked.slots += coverage.unchecked;
        if coverage.unchecked > 0 {
            skipped = "raw slots the interpreter cannot produce".to_owned();
        } else if coverage.checked == 0 {
            skipped = "nothing to check".to_owned();
        } else {
            compared = true;
        }
    }
    if compared {
        Verdict::Compared
    } else {
        Verdict::Skipped(skipped)
    }
}

/// `Err` when the optimised run differs from the raw one, else what the
/// comparison covered. The optimiser may refine an undefined raw value, and
/// may append stack arguments to a call.
fn agree(raw: &Outcome, opt: &Outcome) -> Result<Coverage, String> {
    let mut coverage = Coverage::default();
    match (raw, opt) {
        (
            Outcome::Return {
                values: raw_values,
                writes: raw_writes,
                calls: raw_calls,
            },
            Outcome::Return {
                values: opt_values,
                writes: opt_writes,
                calls: opt_calls,
            },
        ) => {
            if raw_values.len() != opt_values.len() {
                return Err(format!(
                    "{} return slots raw vs {} optimised",
                    raw_values.len(),
                    opt_values.len()
                ));
            }
            calls_agree(raw_calls, opt_calls, &mut coverage)?;
            slots_agree("return slot", raw_values, opt_values, &mut coverage)?;
            writes_agree("at the return", raw_writes, opt_writes, &mut coverage)?;
        }
        (
            Outcome::Unreachable {
                writes: raw_writes,
                calls: raw_calls,
            },
            Outcome::Unreachable {
                writes: opt_writes,
                calls: opt_calls,
            },
        ) => {
            calls_agree(raw_calls, opt_calls, &mut coverage)?;
            writes_agree("at the trap", raw_writes, opt_writes, &mut coverage)?;
        }
        _ => {
            return Err(format!(
                "raw ended {}, optimised {}",
                ending(raw),
                ending(opt)
            ));
        }
    }
    Ok(coverage)
}

fn ending(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Return { .. } => "in a return",
        Outcome::Unreachable { .. } => "in a trap",
        Outcome::Diverge(_) => "by diverging",
    }
}

/// Pairs each raw slot with the optimised one at its index.
fn slots_agree(
    what: &str,
    raw: &[Slot],
    opt: &[Slot],
    coverage: &mut Coverage,
) -> Result<(), String> {
    for (i, (r, o)) in raw.iter().zip(opt).enumerate() {
        match r.admits(o) {
            Some(true) => coverage.checked += usize::from(matches!(r, Slot::Known(_))),
            Some(false) => return Err(format!("{what} {i}: {r:x?} raw vs {o:x?} optimised")),
            None => coverage.unchecked += 1,
        }
    }
    Ok(())
}

fn calls_agree(
    raw: &[CallRecord],
    opt: &[CallRecord],
    coverage: &mut Coverage,
) -> Result<(), String> {
    if raw.len() != opt.len() {
        return Err(format!(
            "{} calls raw vs {} optimised",
            raw.len(),
            opt.len()
        ));
    }
    for (k, (r, o)) in raw.iter().zip(opt).enumerate() {
        if r.target != o.target {
            return Err(format!(
                "call {k}: target {:#x} raw vs {:#x} optimised",
                r.target, o.target
            ));
        }
        if o.args.len() < r.args.len() {
            return Err(format!(
                "call {k}: {} arguments raw vs {} optimised",
                r.args.len(),
                o.args.len()
            ));
        }
        coverage.checked += 1;
        slots_agree(&format!("call {k} argument"), &r.args, &o.args, coverage)?;
        writes_agree(&format!("at call {k}"), &r.writes, &o.writes, coverage)?;
    }
    Ok(())
}

fn writes_agree(
    at: &str,
    raw: &BTreeMap<u64, u8>,
    opt: &BTreeMap<u64, u8>,
    coverage: &mut Coverage,
) -> Result<(), String> {
    if raw != opt {
        return Err(format!("memory {at}: {raw:x?} raw vs {opt:x?} optimised"));
    }
    coverage.checked += raw.len();
    Ok(())
}

/// The arches the named-function tests run on.
const PILOT: [Arch; 5] = [
    Arch::X64,
    Arch::Aarch64,
    Arch::Arm,
    Arch::Mips32be,
    Arch::Ppc32be,
];

fn sweep_named(cases: &[(&str, &[&str])], floor: usize) {
    on_interp_stack(|| sweep_named_here(cases, floor));
}

fn sweep_named_here(cases: &[(&str, &[&str])], floor: usize) {
    let mut compared = 0usize;
    let mut unchecked = Unchecked::default();
    let mut unchecked_in = Vec::new();
    let mut skipped = Vec::new();
    let mut failures = Vec::new();
    for arch in PILOT {
        for &(case, names) in cases {
            for &name in names {
                let at = format!("{}/{case}::{name}", arch.name());
                let (outcome, _cc, rom) = lift_for_pipeline(arch, case, name);
                let raw = outcome.function;
                let mut opt = raw.clone();
                common::optimize(&mut opt, &rom)
                    .unwrap_or_else(|e| panic!("{at}: optimizer pipeline: {e:?}"));
                let slots_before = unchecked.slots;
                match compare(&raw, &opt, &rom, &mut unchecked) {
                    Verdict::Compared => compared += 1,
                    Verdict::Skipped(why) => skipped.push(format!("{at}: {why}")),
                    Verdict::Failed(e) => failures.push(format!("{at}: {e}")),
                }
                if unchecked.slots > slots_before {
                    unchecked_in.push(at);
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        compared >= floor,
        "only {compared} functions were actually run, expected at least {floor} \
         ({} environments diverged); skipped:\n{}",
        unchecked.diverged,
        skipped.join("\n")
    );
    assert_eq!(
        unchecked.slots,
        0,
        "raw slots the interpreter cannot produce, in:\n{}",
        unchecked_in.join("\n")
    );
}

/// Straight-line integer arithmetic: the shapes the peephole rules rewrite
/// most aggressively. ARM divides through `__aeabi_*` calls.
#[test]
fn arithmetic_survives_the_pipeline() {
    sweep_named(
        &[(
            "arithmetic",
            &[
                "add", "sub", "mul", "bit_and", "bit_or", "bit_xor", "bit_not", "shl", "lshr",
                "ashr", "negate", "udiv", "umod", "sdiv", "smod",
            ],
        )],
        75,
    );
}

/// Branches and merges, where the optimiser rewrites conditions and folds
/// phis.
#[test]
fn branching_control_flow_survives_the_pipeline() {
    sweep_named(
        &[("control", &["abs_val", "max_val", "clamp", "select_three"])],
        20,
    );
}

/// Loads and stores, where load forwarding and store elimination rewire the
/// memory chain.
#[test]
fn memory_effects_survive_the_pipeline() {
    sweep_named(
        &[(
            "memory",
            &[
                "pointer_chase",
                "struct_field_load",
                "struct_field_store",
                "tagged_union_read",
            ],
        )],
        20,
    );
}

/// A guard against the machinery agreeing because it computed nothing: two
/// different functions must come out different.
#[test]
fn the_comparison_can_tell_two_functions_apart() {
    on_interp_stack(|| {
        let (add, _, rom) = lift_for_pipeline(Arch::X64, "arithmetic", "add");
        let (sub, _, _) = lift_for_pipeline(Arch::X64, "arithmetic", "sub");
        let mut sub = sub.function;
        common::optimize(&mut sub, &rom).expect("the pipeline runs");
        let verdict = compare(&add.function, &sub, &rom, &mut Unchecked::default());
        assert!(
            matches!(&verdict, Verdict::Failed(e) if e.contains(": return slot ")),
            "add and sub agreed on their return slot"
        );
    });
}

/// Below this share of compared functions an arch's sweep fails. The rest are
/// skipped by reason and printed.
const SWEEP_FLOOR_PERCENT: usize = 79;

/// `analyze` refusals the fixtures produce, as `arch/case::function`.
const KNOWN_ANALYZE_ERRORS: &[&str] = &[
    // The symbol's recorded size ends before the hand-written `_start` does,
    // so the size-bounded lift runs off it.
    "aarch64/zero_pcode_holes::_start",
];

#[derive(Default)]
struct Tally {
    attempted: usize,
    compared: usize,
    unchecked: Unchecked,
    unchecked_in: Vec<String>,
    skipped: BTreeMap<String, usize>,
    analyze_errors: Vec<String>,
    failures: Vec<String>,
}

/// The function symbols of `obj`, one per address, preferring one with a
/// recorded size, as `ElfLifter.functions()` lists them.
fn functions(obj: &object::File<'_>) -> BTreeMap<u64, (String, u64)> {
    use object::{Object, ObjectSymbol, SymbolKind};
    let mut out: BTreeMap<u64, (String, u64)> = BTreeMap::new();
    for sym in obj.symbols() {
        if sym.kind() != SymbolKind::Text || !sym.is_definition() {
            continue;
        }
        let Ok(name) = sym.name() else { continue };
        let slot = out
            .entry(sym.address())
            .or_insert_with(|| (name.to_owned(), sym.size()));
        if slot.1 == 0 && sym.size() > 0 {
            *slot = (name.to_owned(), sym.size());
        }
    }
    out
}

/// Analyses every function of `arch`'s fixtures with production defaults and
/// compares the optimised result with the raw lift of the same CFG.
fn sweep_arch(arch: Arch) {
    on_interp_stack(|| {
        let mut tally = Tally::default();
        for fixture in Fixture::all(arch.fixture()) {
            sweep_fixture(&fixture, &mut tally);
        }
        report(arch, &tally);
    });
}

fn sweep_fixture(fixture: &Fixture, tally: &mut Tally) {
    let obj = fixture.file();
    let (mut strider, cc) = fixture.strider();
    let data_ranges =
        strider_cfg::DataRanges::new(strider_reader::elf::mapping_symbol_data_ranges(&obj));
    for (addr, (name, size)) in functions(&obj) {
        tally.attempted += 1;
        let at = format!("{}/{}::{name}", fixture.arch.dir, fixture.case);
        // As the Python loader analyses a named symbol: its recorded size
        // bounds the lift, and `$d` mapping symbols mark literal pools.
        let opts = strider_orchestrator::LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: (size > 0).then_some(size),
                data_ranges: data_ranges.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let options = strider_orchestrator::opt::OptOptions::default();
        let result = match strider.analyze(addr, &cc, &opts, &options, None) {
            Ok(result) => result,
            Err(e) => {
                tally.analyze_errors.push(format!("{at}: {e:#}"));
                continue;
            }
        };
        let raw = strider
            .lifter()
            .build_ir_with(&result.cfg, cc.clone(), &opts)
            .unwrap_or_else(|e| panic!("{at}: lifting the analysed cfg: {e:#}"))
            .function;
        let rom = strider.rom().expect("a fixture Strider carries its ROM");
        let slots_before = tally.unchecked.slots;
        match compare(&raw, &result.function, rom, &mut tally.unchecked) {
            Verdict::Compared => tally.compared += 1,
            Verdict::Skipped(why) => *tally.skipped.entry(why).or_default() += 1,
            Verdict::Failed(e) => tally.failures.push(format!(
                "{at}: {e}\nRAW:\n{}\nOPTIMISED:\n{}",
                raw.to_text(false),
                result.function.to_text(false)
            )),
        }
        if tally.unchecked.slots > slots_before {
            tally.unchecked_in.push(at);
        }
    }
}

fn report(arch: Arch, tally: &Tally) {
    let name = arch.name();
    eprintln!(
        "{name}: {} functions, {} compared, {} analyze errors, {} environments \
         diverged, {} unchecked raw slots in {} functions",
        tally.attempted,
        tally.compared,
        tally.analyze_errors.len(),
        tally.unchecked.diverged,
        tally.unchecked.slots,
        tally.unchecked_in.len(),
    );
    for (why, n) in &tally.skipped {
        eprintln!("  skipped {n}: {why}");
    }
    assert!(
        tally.failures.is_empty(),
        "{} functions changed meaning:\n{}",
        tally.failures.len(),
        tally.failures.join("\n=====\n")
    );
    let prefix = format!("{name}/");
    let known: Vec<&str> = KNOWN_ANALYZE_ERRORS
        .iter()
        .copied()
        .filter(|k| k.starts_with(&prefix))
        .collect();
    let refused = |k: &str, e: &str| e.starts_with(&format!("{k}: "));
    let unexpected: Vec<&str> = tally
        .analyze_errors
        .iter()
        .map(String::as_str)
        .filter(|e| !known.iter().any(|k| refused(k, e)))
        .collect();
    assert!(
        unexpected.is_empty(),
        "analyze refused:\n{}",
        unexpected.join("\n")
    );
    let stale: Vec<&str> = known
        .iter()
        .copied()
        .filter(|k| !tally.analyze_errors.iter().any(|e| refused(k, e)))
        .collect();
    assert!(
        stale.is_empty(),
        "no longer refused, drop from KNOWN_ANALYZE_ERRORS: {stale:?}"
    );
    assert!(
        tally.compared * 100 >= tally.attempted * SWEEP_FLOOR_PERCENT,
        "{name}: only {} of {} functions compared",
        tally.compared,
        tally.attempted
    );
}

/// One test per arch, so the harness runs them in parallel.
macro_rules! fixture_sweeps {
    ($($test:ident => $arch:ident),+ $(,)?) => {
        $(
            #[test]
            #[cfg_attr(
                debug_assertions,
                ignore = "release-only: analyses every function of the arch's fixtures"
            )]
            fn $test() {
                sweep_arch(Arch::$arch);
            }
        )+
        /// The arches the sweep runs.
        #[allow(dead_code)]
        const SWEPT: &[Arch] = &[$(Arch::$arch),+];
    };
}

fixture_sweeps! {
    every_x64_function_survives_the_pipeline => X64,
    every_aarch64_function_survives_the_pipeline => Aarch64,
    every_arm_function_survives_the_pipeline => Arm,
    every_mips32be_function_survives_the_pipeline => Mips32be,
    every_ppc32be_function_survives_the_pipeline => Ppc32be,
}
