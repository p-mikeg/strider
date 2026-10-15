//! Every context var a preset's sla can `globalset` is covered by one of the
//! three per-preset lists, by the mechanism that var's FLOW CLASS admits, or is
//! a justified exception.
//!
//! Those lists are the whole defence against a value one function commits
//! changing how a later one decodes on a reused engine, and nothing checked
//! them against the sla they describe. Both bugs this mechanism has had were a
//! var in the wrong place: ARM `TEEMode` tagged `NoFlow` when the sla declares
//! it flowing (erased to the pspec default, destroying a ThumbEE region), and
//! then `TEEMode` dropped altogether (an earlier build's mode believed). A
//! name-membership check catches only the second. This checks the partition:
//! which list, with which flow class.
//!
//! Over-approximates on purpose: it scans every `.sinc` / `.slaspec` under the
//! preset's language directory rather than following `@include`, so it can
//! demand coverage for a var no constructor this preset selects. That asks for
//! too much, never too little.

use std::collections::{BTreeMap, BTreeSet};

use strider_target::{ArchPreset, FlowCommit};

/// The language directory under `externals/rsleigh/sleigh/processors` whose
/// files back each preset.
fn language_dir(preset: ArchPreset) -> &'static str {
    match preset {
        ArchPreset::X86 | ArchPreset::X86_64 => "x86",
        ArchPreset::Arm | ArchPreset::ArmBe | ArchPreset::ArmBeKernel | ArchPreset::ArmThumb => {
            "ARM"
        }
        ArchPreset::Aarch64 | ArchPreset::Aarch64Be => "AARCH64",
        ArchPreset::MipsBe32
        | ArchPreset::MipsLe32
        | ArchPreset::MipsBe64
        | ArchPreset::MipsLe64 => "MIPS",
        ArchPreset::Ppc32Be | ArchPreset::Ppc32Le | ArchPreset::Ppc64Be | ArchPreset::Ppc64Le => {
            "PowerPC"
        }
    }
}

/// What each directory commits today. Pinned so an extractor that stops
/// recognising a form fails loudly instead of reporting a smaller set: a
/// global "found something" counter cannot notice ARM going silent while MIPS
/// keeps it non-zero.
fn expected(dir: &str) -> &'static [&'static str] {
    match dir {
        "x86" | "AARCH64" => &[],
        "ARM" => &["LRset", "TEEMode", "TMode", "condit"],
        "MIPS" => &["ISA_MODE", "PAIR_INSTRUCTION_FLAG", "ext_delay"],
        "PowerPC" => &["linkreg"],
        other => panic!("no expected set recorded for language dir {other}"),
    }
}

/// A committed var covered by none of the three lists, with the reason.
/// PowerPC `linkreg` is flowing and committed at both `inst_start` and
/// `inst_next`, so no pin or re-derivation reaches it; it is sound only
/// because the lift is the same either way, which
/// `linkreg_is_pcode_neutral.rs` pins.
fn justified_exception(dir: &str, var: &str) -> bool {
    dir == "PowerPC" && var == "linkreg"
}

fn language_files(dir: &str) -> Vec<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../externals/rsleigh/sleigh/processors")
        .join(dir)
        .join("data/languages");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    // Recursive: a subdirectory include is legal Sleigh, and `x86`, `ARM` and
    // `PowerPC` already carry an `old/` subdirectory.
    while let Some(at) = stack.pop() {
        let entries = std::fs::read_dir(&at).unwrap_or_else(|e| {
            panic!(
                "{}: {e}; is the rsleigh submodule checked out?",
                at.display()
            )
        });
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("sinc" | "slaspec")
            ) {
                out.push(path);
            }
        }
    }
    assert!(!out.is_empty(), "no sla sources under {}", root.display());
    out
}

/// Every var named as the second argument of a `globalset`.
///
/// Rejects rather than skips anything it cannot parse: the grammar is
/// `globalset '(' symbol ',' CONTEXTSYM ')'` (`slghparse.y`), but the lexer
/// discards whitespace, newlines and `#` comments between those tokens and
/// expands `$(MACRO)` first, so a form this does not recognise must fail the
/// test instead of vanishing from the set.
fn committed_vars(dir: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for path in language_files(dir) {
        let text = std::fs::read_to_string(&path).expect("read sla source");
        for (at, _) in text.match_indices("globalset") {
            let rest = &text[at + "globalset".len()..];
            let Some(open) = rest.find('(') else { continue };
            // Only whitespace may sit between the keyword and the paren.
            if !rest[..open].chars().all(char::is_whitespace) {
                continue;
            }
            let args = &rest[open + 1..];
            let Some(close) = args.find(')') else {
                panic!("{}: unterminated globalset argument list", path.display());
            };
            let args = &args[..close];
            let var = args.rsplit(',').next().unwrap_or("").trim();
            // A trailing `#` comment inside the list is legal; strip it.
            let var = var.rsplit('\n').next().unwrap_or(var).trim();
            assert!(
                !var.is_empty() && var.chars().all(|c| c.is_alphanumeric() || c == '_'),
                "{}: cannot read the committed var out of `globalset({args})`; \
                 a form this parser does not recognise must not be silently \
                 dropped from the covered set",
                path.display(),
            );
            out.insert(var.to_string());
        }
    }
    out
}

/// Each context field the directory declares, mapped to whether the sla marks
/// it `noflow`. `NAME = (hi,lo) [signed] [noflow]`.
fn noflow_by_var(dir: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    for path in language_files(dir) {
        let text = std::fs::read_to_string(&path).expect("read sla source");
        let Some(at) = text.find("define context") else {
            continue;
        };
        for line in text[at..].lines().skip(1) {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with("define ") || line.starts_with("attach ") {
                break;
            }
            let Some((name, rest)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let Some(after) = rest.split_once(')') else {
                continue;
            };
            out.insert(name.to_string(), after.1.contains("noflow"));
        }
    }
    out
}

#[test]
fn every_committed_context_var_is_covered_by_the_right_mechanism() {
    for &preset in ArchPreset::ALL {
        let dir = language_dir(preset);
        let arch = preset.arch();

        let committed = committed_vars(dir);
        assert_eq!(
            committed,
            expected(dir)
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
            "{dir}: the set of `globalset` vars drifted; either the sla changed \
             or the parser stopped seeing a form it used to see",
        );

        let flow_commit: BTreeMap<&str, FlowCommit> =
            arch.flow_commit_vars().iter().copied().collect();
        let transient: BTreeSet<&str> = arch.transient_decode_vars().iter().copied().collect();
        let mode = arch.isa_mode_var();

        // One mechanism per var, or the next reader cannot tell which one is
        // responsible.
        for name in flow_commit.keys() {
            assert!(
                !transient.contains(name) && Some(*name) != mode,
                "{preset:?}: `{name}` is in more than one list",
            );
        }
        for name in &transient {
            assert!(Some(*name) != mode, "{preset:?}: `{name}` is in two lists");
        }

        let noflow = noflow_by_var(dir);
        for var in &committed {
            let flows = match noflow.get(var.as_str()) {
                Some(is_noflow) => !is_noflow,
                None => panic!("{dir}: `{var}` is committed but never declared"),
            };

            if let Some(class) = flow_commit.get(var.as_str()) {
                // `rederive_flow` erases a `NoFlow` var to the pspec default
                // and a `Flowing` one to the predecessor's value. Either tag on
                // the wrong var is a miscompile, in opposite directions.
                let want = if flows {
                    FlowCommit::Flowing
                } else {
                    FlowCommit::NoFlow
                };
                assert_eq!(
                    *class,
                    want,
                    "{preset:?}: `{var}` is declared {} in the sla but tagged \
                     {class:?} in flow_commit_vars",
                    if flows { "flowing" } else { "noflow" },
                );
            } else if transient.contains(var.as_str()) {
                // `hold_transients` clears to the pspec default with NO
                // re-lift, including at a region start, which would destroy a
                // flowing value the predecessor legitimately committed.
                assert!(
                    !flows,
                    "{preset:?}: `{var}` is declared FLOWING in the sla but sits \
                     in transient_decode_vars, which clears it without a \
                     re-lift",
                );
            } else {
                assert!(
                    Some(var.as_str()) == mode || justified_exception(dir, var),
                    "{preset:?}: the sla commits `{var}`, which no list covers \
                     and which is not a justified exception, so a value one \
                     function commits can change how a later one decodes",
                );
            }
        }
    }
}
