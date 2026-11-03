//! Family facts checked against the vendored sla sources.

use std::path::{Path, PathBuf};

use strider_target::ArchPreset;

/// The language directory under `externals/rsleigh/sleigh/processors` whose
/// files back each preset.
fn language_dir(preset: ArchPreset) -> &'static str {
    match preset.family_name() {
        "x86" => "x86",
        "arm32" => "ARM",
        "aarch64" => "AARCH64",
        "mips" => "MIPS",
        "ppc" => "PowerPC",
        other => panic!("{preset:?}: no language directory for family {other}"),
    }
}

fn language_files(dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../externals/rsleigh/sleigh/processors")
        .join(dir)
        .join("data/languages");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
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

/// `has_delay_slots` is true exactly when some source under the preset's
/// language directory uses `delayslot(`. Over-approximates like the context
/// var scan: it does not follow `@include`.
#[test]
fn has_delay_slots_matches_delayslot_use_in_the_sla() {
    for &preset in ArchPreset::ALL {
        let uses = language_files(language_dir(preset)).iter().any(|path| {
            std::fs::read_to_string(path)
                .expect("read sla source")
                .contains("delayslot(")
        });
        assert_eq!(
            preset.arch().has_delay_slots(),
            uses,
            "{preset:?}: has_delay_slots disagrees with `delayslot(` in {}",
            language_dir(preset),
        );
    }
}
