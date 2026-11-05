//! Pins what `Strider::analyze` makes of every function of the committed
//! fixtures under production defaults, one file per image:
//! `fixtures/golden/<arch>/<case>.txt`. A function's section is
//! `== <symbol> @<addr>`, each incompleteness reason held, the CFG one region
//! per line (start, p-code op count, terminator, successors) and the
//! optimised IR with registers named. An `analyze` error is pinned as its
//! message.
//!
//! A golden detects change; beyond the `INTENT` rows, and the `GAPS` that
//! stand in for them on named arches, it does not judge it.
//! Review a re-bless as a diff:
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
use strider_orchestrator::{AnalyzeResult, Reason};

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

/// One line per region, by start. A switch's successors are printed only
/// where they are not its sorted arms.
fn cfg_lines(cfg: &Cfg, out: &mut String) {
    let graph = cfg.region_graph();
    let mut regions: Vec<_> = cfg.region_ids().collect();
    regions.sort_by_key(|&r| graph[r].start_addr);
    for r in regions {
        let region = &graph[r];
        let mut successors: Vec<PcodeInsnAddr> =
            graph.neighbors(r).map(|s| graph[s].start_addr).collect();
        successors.sort_unstable();
        let _ = write!(
            out,
            "  {} {} ops {}",
            pc(&region.start_addr),
            region.insns.len(),
            terminator(&region.terminator),
        );
        if let RegionTerminator::Switch { targets, .. } = &region.terminator {
            let mut arms: Vec<PcodeInsnAddr> = targets
                .iter()
                .map(|t| PcodeInsnAddr::at_machine_start(t.addr))
                .collect();
            arms.sort_unstable();
            if arms == successors {
                out.push('\n');
                continue;
            }
        }
        let successors: Vec<String> = successors.iter().map(pc).collect();
        let _ = writeln!(out, " -> [{}]", successors.join(", "));
    }
}

/// Each reason with its sites, by name.
fn report(result: &AnalyzeResult, out: &mut String) {
    for reason in Reason::ALL {
        let sites: Vec<String> = result.incomplete().of(reason).map(|a| pc(&a)).collect();
        if !sites.is_empty() {
            let _ = writeln!(out, "{} [{}]", reason.name(), sites.join(", "));
        }
    }
}

/// `text`'s `endian`, `default_cc` and `assumptions` lines, then the rest.
/// `to_text` opens with `endian`, `tracked`, `default_cc`, then
/// `assumptions` when a claim is on.
fn split_shared(text: &str) -> (String, String) {
    let mut lines = text.split_inclusive('\n').peekable();
    let endian = lines.next().unwrap_or_default();
    let tracked = lines.next().unwrap_or_default();
    let cc = lines.next().unwrap_or_default();
    assert!(
        endian.starts_with("endian ")
            && tracked.starts_with("tracked ")
            && cc.starts_with("default_cc "),
        "to_text's header moved:\n{text}"
    );
    let claims = lines
        .next_if(|line| line.starts_with("assumptions "))
        .unwrap_or_default();
    (
        format!("{endian}{cc}{claims}"),
        format!("{tracked}{}", lines.collect::<String>()),
    )
}

/// `(token, n)`: at least `n` of `token` in a function's section, none if
/// `n` is 0. A token is an IR op as printed (`call`, `load`, `iconst[0x61]`),
/// `a|b` summing both; `cfg.back_edge` counts the CFG's DFS back edges and
/// `cfg.return_path` sums each `return` region's in-edges, at least one each;
/// `cfg.switch` counts the CFG's seated `switch@` regions.
type Need = (&'static str, usize);

/// What each function must show on every arch: `(case, function, any)`, met
/// when every need of one alternative holds. Checked against the golden text
/// before it is compared or written, and printed in the case header.
#[rustfmt::skip]
const INTENT: &[(&str, &str, &[&[Need]])] = &[
    ("abi", "eight_int_args", &[&[("int.add", 7)]]),
    ("abi", "make_pair", &[&[("return", 1)]]),
    ("abi", "mixed_args", &[&[("load", 2), ("int.add", 5)]]),
    ("abi", "point_sum", &[&[("int.add", 1)]]),
    ("abi", "tail_caller", &[&[("call", 1)], &[("return", 1)]]),
    ("arithmetic", "add", &[&[("int.add", 1)]]),
    ("arithmetic", "ashr", &[&[("int.sshift_right", 1)]]),
    ("arithmetic", "bit_and", &[&[("int.and", 1)]]),
    ("arithmetic", "bit_not", &[&[("int.xor", 1), ("iconst[0xffffffff]|iconst[0xffffffffffffffff]", 1)]]),
    ("arithmetic", "bit_or", &[&[("int.or", 1)]]),
    ("arithmetic", "bit_xor", &[&[("int.xor", 1)]]),
    ("arithmetic", "lshr", &[&[("int.shift_right", 1)]]),
    ("arithmetic", "mul", &[&[("int.mul", 1)]]),
    ("arithmetic", "negate", &[&[("int.neg", 1)]]),
    ("arithmetic", "sdiv", &[&[("int.sdiv", 1)], &[("call", 1)]]),
    ("arithmetic", "shl", &[&[("int.shift_left", 1)]]),
    ("arithmetic", "smod", &[&[("int.srem", 1)], &[("int.sdiv", 1)], &[("call", 1)]]),
    ("arithmetic", "sub", &[&[("int.neg", 1)]]),
    ("arithmetic", "udiv", &[&[("int.div", 1)], &[("call", 1)]]),
    ("arithmetic", "umod", &[&[("int.rem", 1)], &[("int.div", 1)], &[("call", 1)]]),
    ("builtins", "clz32", BIT_COUNT),
    ("builtins", "clz64", BIT_COUNT),
    ("builtins", "ctz32", BIT_COUNT),
    ("builtins", "expect_branch", &[&[("if", 1)]]),
    ("builtins", "popcount32", BIT_COUNT),
    ("builtins", "popcount64", BIT_COUNT),
    ("calls", "apply_indirect", &[&[("call", 1)]]),
    ("calls", "fib_recursive", &[&[("call", 2), ("if", 1)]]),
    ("calls", "mutual_a", &[&[("call", 1), ("if", 1)]]),
    ("calls", "mutual_b", &[&[("call", 1), ("if", 1)]]),
    ("calls", "nested_3deep", &[&[("call", 1)]]),
    ("calls", "pass_through", &[&[("call", 1)]]),
    ("calls", "repeat_call_pair", &[&[("call", 2)]]),
    ("control", "abs_val", &[&[("if", 1)]]),
    ("control", "clamp", &[&[("if", 2)]]),
    ("control", "count_bits", &[&[("cfg.back_edge", 1), ("int.shift_right", 1)]]),
    ("control", "early_return", &[&[("cfg.back_edge", 1), ("cfg.return_path", 2)]]),
    ("control", "factorial", &[&[("cfg.back_edge", 1)]]),
    ("control", "max_val", &[&[("if", 1)]]),
    ("control", "nested_loops", &[&[("cfg.back_edge", 2)]]),
    ("control", "select_three", &[&[("if", 2)]]),
    ("control", "sum_to_n", &[&[("cfg.back_edge", 1)]]),
    ("floats", "f32_arith", &[&[("float.add|float.mul|float.div", 4)]]),
    ("floats", "f32_compare", &[&[("fcmp.less|fcmp.equal", 2)]]),
    ("floats", "f32_neg_abs", &[&[("float.neg", 1)], &[("int.xor", 1)]]),
    ("floats", "f32_to_f64", &[&[("float_to_float", 1)]]),
    ("floats", "f64_arith", &[&[("float.add|float.mul|float.div", 4)]]),
    ("floats", "f64_compare", &[&[("fcmp.less|fcmp.equal", 2)]]),
    ("floats", "f64_to_f32", &[&[("float_to_float", 1)]]),
    ("floats", "float_to_int", &[&[("float_to_int", 1)]]),
    // ppc32 converts by the magic-number subtraction, an add of its negation,
    // folded where the magic number is a double.
    ("floats", "int_to_float", &[&[("int_to_float", 1)], &[("float.add", 1), ("float.neg|fconst[0xc330000080000000]", 1), ("float_to_float", 1)]]),
    ("indirect_branch", "indirect_branch_resolved", RESOLVES),
    ("globals", "branch_on_const_string", &[&[("iconst[0x79]", 1)], &[("if", 0)]]),
    ("globals", "read_const_byte", &[&[("iconst[0x61]", 1)]]),
    ("globals", "read_const_int", &[&[("iconst[0x12345678]", 1)]]),
    ("globals", "runtime_const_idx", &[&[("load", 1), ("if", 1)]]),
    ("memory", "array_copy", &[&[("load", 1), ("store", 1), ("cfg.back_edge", 1)]]),
    ("memory", "array_fill", &[&[("store", 1), ("cfg.back_edge", 1)]]),
    ("memory", "array_sum", &[&[("load", 1), ("cfg.back_edge", 1)]]),
    ("memory", "pointer_chase", &[&[("load", 2)]]),
    ("memory", "struct_field_load", &[&[("load", 2)]]),
    ("memory", "struct_field_store", &[&[("store", 2)]]),
    ("memory", "tagged_union_read", &[&[("load", 1)]]),
    ("stack", "escape_via_ptr", &[&[("store", 1), ("call", 1)]]),
    ("stack", "inplace_swap", &[&[("load", 2), ("store", 2)]]),
    ("stack", "large_local_array", &[&[("store", 1), ("cfg.back_edge", 1)]]),
    ("stack", "recursive_stack_growth", &[&[("call", 1), ("store", 1)]]),
    ("stack", "volatile_three_writes", &[&[("store", 3)]]),
    ("switch", "dispatch_value", RESOLVES),
    ("switch_masked_loop", "masked_loop_switch", RESOLVES),
    ("switch_phi_index", "phi_index_dispatch", RESOLVES),
    ("switch_value_range", "dispatch_offset", RESOLVES),
    ("switch_value_range", "dispatch_unmasked", RESOLVES),
];

/// Every indirect branch resolves, and a table is seated.
const RESOLVES: &[&[Need]] = &[&[("unresolved_branch", 0), ("cfg.switch", 1)]];

/// Known unresolved dispatches: `(case, function, arches, why)`. On those
/// arches the row must instead report `unresolved_branch`, so a fix fails
/// here until its gap is dropped.
#[rustfmt::skip]
const GAPS: &[(&str, &str, &[&str], &str)] = &[
    ("indirect_branch", "indirect_branch_resolved", &["aarch64be"], "the labels are stored as one i128 and read back at a variable offset off an alignment-masked SP"),
    ("indirect_branch", "indirect_branch_resolved", &["arm_be8"], "a variable-index load from the two-slot stack array is not forwarded"),
    ("indirect_branch", "indirect_branch_resolved", MIPS64, GOT),
    ("indirect_branch", "indirect_branch_resolved", &["ppc32le", "ppc64be", "ppc64le"], DATA_REL_RO),
    ("switch", "dispatch_value", MIPS64, GOT),
    ("switch_masked_loop", "masked_loop_switch", &["mips32be", "mips32le"], "the `& 7` mask names entries past gcc's six-entry table"),
    ("switch_masked_loop", "masked_loop_switch", MIPS64, GOT),
    ("switch_phi_index", "phi_index_dispatch", &["aarch64", "arm", "arm_thumb", "mips32be", "mips32le", "mips64be", "mips64le", "ppc32le", "ppc64be"], DATA_REL_RO),
    ("switch_phi_index", "phi_index_dispatch", &["aarch64be", "arm_be"], "clang -O0 reloads the index from a stack slot through a memphi, which is not forwarded"),
    ("switch_value_range", "dispatch_offset", MIPS64, GOT),
    ("switch_value_range", "dispatch_unmasked", MIPS64, GOT),
];

/// `RESOLVES` rows a compiler lowers to a compare chain on the named arches:
/// `(case, function, arches, why)`. There the row must show no table and no
/// unresolved branch, so a rebuilt fixture that emits a table fails here
/// until its entry is dropped.
#[rustfmt::skip]
const CHAINS: &[(&str, &str, &[&str], &str)] = &[
    ("switch", "dispatch_value", &["aarch64"], COMPARE_CHAIN),
    ("switch_masked_loop", "masked_loop_switch", &["aarch64"], COMPARE_CHAIN),
    ("switch_value_range", "dispatch_offset", &["aarch64"], COMPARE_CHAIN),
    ("switch_value_range", "dispatch_unmasked", &["aarch64"], COMPARE_CHAIN),
];

const COMPARE_CHAIN: &str = "gcc emits a compare chain, no table";
const MIPS64: &[&str] = &["mips64be", "mips64le"];
const GOT: &str = "the table base is a GOT slot, unrelocated in the default ROM";
const DATA_REL_RO: &str = "the label table is in writable .data.rel.ro, which no ROM serves";

/// `function`'s row on `arch`, and the gap or chain it stands in for.
fn intent_on(
    arch: &str,
    case: &str,
    function: &str,
    any: &'static [&'static [Need]],
) -> (&'static [&'static [Need]], Option<&'static str>) {
    let on = |g: &&(&str, &str, &[&str], &'static str)| {
        g.0 == case && g.1 == function && g.2.contains(&arch)
    };
    if let Some(gap) = GAPS.iter().find(on) {
        (&[&[("unresolved_branch", 1)]], Some(gap.3))
    } else if let Some(chain) = CHAINS.iter().find(on) {
        (
            &[&[("cfg.switch", 0), ("unresolved_branch", 0)]],
            Some(chain.3),
        )
    } else {
        (any, None)
    }
}

/// A dedicated op, a library call, or the inlined bit sequence.
#[rustfmt::skip]
const BIT_COUNT: &[&[Need]] = &[
    &[("popcount|lzcount", 1)],
    &[("call", 1)],
    &[("int.and|int.or|int.xor|int.shift_left|int.shift_right|int.sshift_right|int.neg", 2)],
];

/// `any` as printed in the case header.
fn intent_text(any: &[&[Need]]) -> String {
    let need = |&(token, n): &Need| match n {
        0 => format!("no {token}"),
        1 => token.to_owned(),
        n => format!("{n} {token}"),
    };
    let all = |needs: &&[Need]| needs.iter().map(need).collect::<Vec<_>>().join(" and ");
    any.iter().map(all).collect::<Vec<_>>().join(" or ")
}

/// The op of an IR line: the word after `=`, else the first word.
fn op(line: &str) -> Option<&str> {
    if line.starts_with(' ') {
        return None;
    }
    let rhs = line.split_once(" = ").map_or(line, |(_, rhs)| rhs);
    rhs.split(' ').next()
}

/// `(cfg.back_edge, cfg.return_path)` of a section's CFG lines.
fn cfg_counts(section: &str) -> (usize, usize) {
    let regions: Vec<(&str, bool, Vec<&str>)> = section
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(|line| {
            let (head, list) = line.rsplit_once(" [").expect("a successor list");
            let mut words = head.split(' ');
            let start = words.next().expect("a start");
            let returns = words.nth(2) == Some("return");
            let successors = list
                .trim_end_matches(']')
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(|s| s.split('/').next().unwrap_or(s))
                .collect();
            (start, returns, successors)
        })
        .collect();
    let index: BTreeMap<&str, usize> = regions.iter().enumerate().map(|(i, r)| (r.0, i)).collect();
    let successors: Vec<Vec<usize>> = regions
        .iter()
        .map(|r| r.2.iter().filter_map(|s| index.get(s).copied()).collect())
        .collect();
    let mut in_edges = vec![0usize; regions.len()];
    for &s in successors.iter().flatten() {
        in_edges[s] += 1;
    }
    let return_paths = regions
        .iter()
        .zip(&in_edges)
        .filter(|(r, _)| r.1)
        .map(|(_, &n)| n.max(1))
        .sum();
    // 0 unvisited, 1 on the DFS stack, 2 done.
    let mut state = vec![0u8; regions.len()];
    let mut back_edges = 0;
    for root in 0..regions.len() {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        let mut stack = vec![(root, 0)];
        while let Some(top) = stack.last_mut() {
            let (region, next) = *top;
            match successors[region].get(next) {
                Some(&s) => {
                    top.1 += 1;
                    match state[s] {
                        0 => {
                            state[s] = 1;
                            stack.push((s, 0));
                        }
                        1 => back_edges += 1,
                        _ => {}
                    }
                }
                None => {
                    state[region] = 2;
                    stack.pop();
                }
            }
        }
    }
    (back_edges, return_paths)
}

/// How many `token` a section shows; see [`Need`].
fn count(section: &str, token: &str) -> usize {
    match token {
        "cfg.back_edge" => cfg_counts(section).0,
        "cfg.return_path" => cfg_counts(section).1,
        "cfg.switch" => section
            .lines()
            .filter(|line| line.starts_with("  ") && line.contains(" switch@"))
            .count(),
        _ => section
            .lines()
            .skip(1)
            .filter_map(op)
            .filter(|op| {
                let bare = op.split('[').next().unwrap_or(op);
                token.split('|').any(|t| t == *op || t == bare)
            })
            .count(),
    }
}

/// One line per `INTENT` row of `case` that `text` breaks on `arch` or has
/// no section for.
fn broken_intents(arch: &str, case: &str, text: &str) -> Vec<String> {
    let sections = sections(text);
    let mut broken = Vec::new();
    for &(_, function, any) in INTENT.iter().filter(|i| i.0 == case) {
        let (any, _) = intent_on(arch, case, function, any);
        let found: Vec<&str> = sections
            .iter()
            .filter(|(header, _)| header.split_once(" @").is_some_and(|h| h.0 == function))
            .map(|(_, section)| *section)
            .collect();
        let holds = |section: &&str| {
            any.iter().any(|all| {
                all.iter().all(|&(token, n)| match n {
                    0 => count(section, token) == 0,
                    n => count(section, token) >= n,
                })
            })
        };
        if found.is_empty() {
            broken.push(format!("{function}: no section"));
        } else if found.iter().any(|s| s.contains("\nerror: ")) {
            broken.push(format!("{function}: analyze failed"));
        } else if !found.iter().all(holds) {
            broken.push(format!("{function}: {} does not hold", intent_text(any)));
        }
    }
    broken
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
            &strider_orchestrator::AnalyzeOptions::default(),
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
        let text = result.function.to_text_named(false, strider.arch().names());
        let (head, rest) = split_shared(&text);
        if shared.is_none() {
            shared = Some(head);
        } else if shared.as_deref() != Some(head.as_str()) {
            body.push_str(&head);
        }
        body.push_str(&rest);
    }
    let mut out = format!(
        "# {}/{}: Strider::analyze, default options\n",
        fixture.arch.dir, fixture.case
    );
    for &(_, function, any) in INTENT.iter().filter(|i| i.0 == fixture.case) {
        match intent_on(fixture.arch.dir, &fixture.case, function, any) {
            (any, None) => {
                let _ = writeln!(out, "# {function}: {}", intent_text(any));
            }
            (any, Some(why)) => {
                let _ = writeln!(out, "# {function}: {} ({why})", intent_text(any));
            }
        }
    }
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
/// `UPDATE_EXPECT`, and fails naming each image that breaks an `INTENT` row,
/// each that changed and each golden left with no image.
fn check_arch(dir: &str) {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let golden_dir = fixtures_root().join("golden").join(dir);
    let mut broken = Vec::new();
    let mut changed = Vec::new();
    let mut cases = Vec::new();
    for fixture in Fixture::all(fixture_arch(dir)) {
        let file = format!("{}.txt", fixture.case);
        let shown = Path::new("fixtures/golden").join(dir).join(&file);
        let path = golden_dir.join(&file);
        let actual = golden_text(&fixture);
        let intents = broken_intents(dir, &fixture.case, &actual);
        cases.push(fixture.case);
        if !intents.is_empty() {
            for intent in intents {
                broken.push(format!("{}: {intent}", shown.display()));
            }
        } else if update {
            std::fs::create_dir_all(&golden_dir).expect("golden dir");
            expect_test::expect_file![path].assert_eq(&actual);
        } else {
            let expected = match std::fs::read_to_string(&path) {
                Ok(text) => text.replace("\r\n", "\n"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => panic!("{}: {e}", shown.display()),
            };
            if expected != actual {
                changed.push(describe_change(&shown, &expected, &actual));
            }
        }
    }
    for (case, function, _) in INTENT {
        if !cases.iter().any(|c| c == case) {
            broken.push(format!("{dir} has no {case} image for {function}"));
        }
    }
    assert!(
        broken.is_empty(),
        "{} INTENT row(s) no longer hold; a bless skips these images\n\n{}",
        broken.len(),
        broken.join("\n")
    );
    if golden_dir.is_dir() {
        let is_txt = |p: &Path| p.extension().is_some_and(|e| e == "txt");
        for stale in stems(&golden_dir, is_txt)
            .into_iter()
            .filter(|s| !cases.contains(s))
        {
            changed.push(format!(
                "fixtures/golden/{dir}/{stale}.txt: no fixture image; delete it"
            ));
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

/// One `#[test]` per `FIXTURE_ARCHES` row, named after its fixture
/// directory, plus that list as `ARCHES`.
macro_rules! golden_tests {
    ($($arch:ident),* $(,)?) => {
        const ARCHES: &[&str] = &[$(stringify!($arch)),*];
        $(
            #[test]
            fn $arch() {
                check_arch(stringify!($arch));
            }
        )*
    };
}

golden_tests!(
    x86, x86_kernel, x64, aarch64, aarch64be, arm, arm_be, arm_be8, arm_thumb, mips32le, mips32be,
    mips64le, mips64be, ppc32be, ppc32le, ppc64be, ppc64le,
);

/// A roster row with no test above never writes its golden directory, so
/// the list the macro takes must be the roster itself.
#[test]
fn every_roster_arch_has_a_test() {
    let mut named: Vec<&str> = ARCHES.to_vec();
    named.sort_unstable();
    assert_eq!(roster_dirs(), named, "a #[test] per FIXTURE_ARCHES row");
}

/// Only an arch's test writes its golden directory, so this fails until a
/// new roster row has a test above. A first bless can race that write;
/// re-run it.
#[test]
fn every_roster_arch_has_goldens() {
    let golden = stems(&fixtures_root().join("golden"), Path::is_dir);
    assert_eq!(roster_dirs(), golden, "a #[test] per FIXTURE_ARCHES row");
}

/// The stems of `dir`'s entries that pass `keep`, sorted.
fn stems(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| {
            let shown = dir.strip_prefix(fixtures_root()).unwrap_or(dir);
            panic!("{}: {e}", Path::new("fixtures").join(shown).display())
        })
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

#[test]
fn an_analyze_error_breaks_every_intent_row() {
    let text = "header\n== branch_on_const_string @0x1000\nerror: lift failed\n";
    assert_eq!(
        broken_intents("x64", "globals", text)
            .iter()
            .filter(|b| b.starts_with("branch_on_const_string"))
            .count(),
        1
    );
}

/// A function with no indirect branch left breaks a `RESOLVES` row.
#[test]
fn a_resolves_row_needs_a_seated_switch() {
    let section = |terminator: &str| {
        format!("header\n== dispatch_value @0x1000\ncfg\n  0x1000 4 ops {terminator} -> []\n")
    };
    let broken = |text: &str| {
        broken_intents("x64", "switch", text)
            .iter()
            .filter(|b| b.starts_with("dispatch_value"))
            .count()
    };
    assert_eq!(broken(&section("return")), 1);
    assert_eq!(broken(&section("switch@0x1000 [0x1010, 0x1020]")), 0);
}

#[test]
fn every_gap_is_an_intent_row_on_roster_arches() {
    let roster = roster_dirs();
    for &(case, function, arches, _) in GAPS.iter().chain(CHAINS) {
        assert!(
            INTENT.iter().any(|i| i.0 == case && i.1 == function),
            "{case}::{function} has no INTENT row"
        );
        for arch in arches {
            assert!(
                roster.iter().any(|r| r == arch),
                "{arch} is not a roster arch"
            );
        }
    }
}
