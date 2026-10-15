//! Every context var a preset's sla can `globalset` is named in exactly one of
//! the three per-preset lists, or is a justified exception.
//!
//! Those lists are the whole defence against a value one function commits
//! changing how a later one decodes on a reused engine, and until now nothing
//! checked them against the sla they describe: the sibling test in `arch.rs`
//! compares two hand-written lists with each other, which both bugs this
//! mechanism has had would have passed. This reads the shipped sla sources
//! instead.
//!
//! Over-approximates on purpose. It scans every `.sinc` / `.slaspec` in the
//! preset's language directory rather than following `@include` from its own
//! slaspec, so it can demand coverage for a var no constructor this preset
//! selects ever commits. That direction is safe: it asks for too much, never
//! too little.

use std::collections::BTreeSet;

use strider_target::ArchPreset;

/// The language directory under `externals/rsleigh/sleigh/processors` whose
/// files back each preset. `None` where a preset's sla commits nothing and the
/// directory need not be read.
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

/// A committed var covered by none of the three lists, with the reason.
/// PowerPC `linkreg` is flowing and committed at both `inst_start` and
/// `inst_next`, so no pin or re-derivation reaches it; it is sound only
/// because every constructor it selects emits identical p-code, which
/// `linkreg_is_pcode_neutral.rs` pins.
fn justified_exception(preset: ArchPreset, var: &str) -> bool {
    matches!(
        preset,
        ArchPreset::Ppc32Be | ArchPreset::Ppc32Le | ArchPreset::Ppc64Be | ArchPreset::Ppc64Le
    ) && var == "linkreg"
}

/// Every var named as the last argument of a `globalset(...)` in the directory.
fn committed_vars(dir: &str) -> BTreeSet<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../externals/rsleigh/sleigh/processors")
        .join(dir)
        .join("data/languages");
    let entries = std::fs::read_dir(&root).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; is the rsleigh submodule checked out?",
            root.display()
        )
    });
    let mut out = BTreeSet::new();
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("sinc" | "slaspec")
        ) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read sla source");
        for (at, _) in text.match_indices("globalset(") {
            let args = &text[at + "globalset(".len()..];
            let Some(args) = args.split(')').next() else {
                continue;
            };
            // `globalset(inst_next, condit)`, `globalset(Abs26, ISA_MODE)`: the
            // var is the last argument.
            let var = args.rsplit(',').next().unwrap_or("").trim();
            if !var.is_empty() && var.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.insert(var.to_string());
            }
        }
    }
    out
}

#[test]
fn every_committed_context_var_is_named_by_one_of_the_three_lists() {
    let mut checked = 0usize;
    for &preset in ArchPreset::ALL {
        let arch = preset.arch();
        let covered: BTreeSet<&str> = arch
            .flow_commit_vars()
            .iter()
            .map(|&(n, _)| n)
            .chain(arch.transient_decode_vars().iter().copied())
            .chain(arch.isa_mode_var())
            .collect();

        for var in committed_vars(language_dir(preset)) {
            checked += 1;
            assert!(
                covered.contains(var.as_str()) || justified_exception(preset, &var),
                "{preset:?}: the sla commits `{var}`, which no list covers \
                 (flow_commit_vars / transient_decode_vars / isa_mode_var = \
                 {covered:?}) and which is not a justified exception, so a \
                 value one function commits can change how a later one decodes",
            );
        }
    }
    assert!(
        checked > 0,
        "no `globalset` was found in any language directory, so this test \
         proved nothing",
    );
}
