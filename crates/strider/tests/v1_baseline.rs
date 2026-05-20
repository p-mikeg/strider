//! v1 IR snapshot baseline.
//!
//! This pins the v1 behavior contract: every (arch, case, function) tuple
//! built under `fixtures/out/<arch>/<case>.elf` is lifted + optimized through
//! the **v1 imperative pipeline** (`Strider::build_optimizer_pipeline` +
//! `LoadReadOnly`) and its post-optimization IR (rendered as DOT) is
//! snapshotted via `insta`.  Any later change to the lifter, the v1
//! optimizer, or the IR layout must NOT change these snapshots without
//! explicit review.
//!
//! Pinned to v1 explicitly (via [`common::analyze_v1`]): the production
//! default in [`common::analyze`] flips to v2 in Phase 8.5c, but
//! `v1_baseline` keeps using the explicit v1 entry point so the
//! historical v1 contract stays frozen.  The companion `v2_baseline`
//! pins the v2 shape via [`common::analyze_v2`].
//!
//! Lift failures (panics from `common::analyze_v1`) are themselves part of
//! the contract — they are captured as `LIFT_FAILED:<message>` snapshots
//! rather than silently skipped, so that future rewrites must reproduce
//! the same failures unless explicitly fixed.
//!
//! Function-name discovery walks the ELF symbol table via the `object`
//! crate; case discovery reads `fixtures/cases/*.c`.  Neither list is
//! hard-coded.
//!
//! Phase 0, Task 0.1 of the strider v2 rewrite plan
//! (`docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md`).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::Arch;
use object::{Object, ObjectSymbol, SymbolKind};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// All architectures with a fixture directory under `fixtures/out/`.
/// Kept in sync with the `Arch` enum + the `per_arch_test!` macro.
const ALL_ARCHES: &[Arch] = &[
    Arch::X86,
    Arch::X86Kernel,
    Arch::X64,
    Arch::Aarch64,
    Arch::Aarch64Be,
    Arch::Arm,
    Arch::ArmBe,
    Arch::ArmThumb,
    Arch::Mips32le,
    Arch::Mips32be,
    Arch::Mips64le,
    Arch::Mips64be,
    Arch::Ppc32be,
    Arch::Ppc32le,
    Arch::Ppc64be,
    Arch::Ppc64le,
];

fn fixtures_cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cases")
}

/// Enumerate every `.c` file under `fixtures/cases/` and return the
/// stem (e.g. `arithmetic.c` → `arithmetic`).  Sorted for determinism.
fn discover_cases() -> Vec<String> {
    let dir = fixtures_cases_dir();
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| {
        panic!("read_dir({dir:?}) failed: {e:?}")
    }) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("c") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.insert(stem.to_string());
        }
    }
    out.into_iter().collect()
}

/// Iterate the ELF symbol table at `path` and return every named global
/// text symbol (i.e. function entry points exposed in the symbol table).
/// Sorted for determinism.  Symbols starting with `_` (compiler runtime
/// helpers) are kept — the lift contract applies to them too.
fn exported_function_names(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("read({path:?}) failed: {e:?}"));
    let obj = object::File::parse(&*bytes)
        .unwrap_or_else(|e| panic!("object::File::parse({path:?}) failed: {e:?}"));
    let mut out = BTreeSet::new();
    for sym in obj.symbols() {
        if sym.kind() != SymbolKind::Text {
            continue;
        }
        if !sym.is_global() {
            continue;
        }
        if sym.size() == 0 {
            // Zero-size text symbols are typically labels (e.g. ARM literal
            // pools or `_start` marker aliases) and aren't full functions.
            continue;
        }
        let Ok(name) = sym.name() else { continue };
        if name.is_empty() {
            continue;
        }
        out.insert(name.to_string());
    }
    out.into_iter().collect()
}

/// Build the sleigh handle for `(arch, path)`.  Used only for the dot
/// dumper — `common::analyze_v1` builds its own internal sleigh.
fn sleigh_for(
    arch: Arch,
    path: &Path,
) -> rsleigh::Sleigh<reader::ElfFileMemReader> {
    let obj = reader::load_elf(path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let mem = reader::ElfFileMemReader::from_object(&obj)
        .expect("ElfFileMemReader::from_object");
    let sleigh_arch = arch.sleigh();
    rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("rsleigh::Sleigh::new")
}

fn to_dot_string(
    g: &ir::BuiltFunctionGraph,
    sleigh: &rsleigh::Sleigh<reader::ElfFileMemReader>,
) -> String {
    let dot = strider_ir::dot::GraphDot::new(g.dot_dumper(sleigh), strider_ir::dot::DotStyle::dark());
    dot.as_dot()
        .unwrap_or_else(|e| panic!("GraphDot::as_dot failed: {e:?}"))
}

#[test]
fn v1_baseline_snapshots() {
    let cases = discover_cases();
    assert!(!cases.is_empty(), "no fixture cases found");

    // Confine snapshot files to a single directory under tests/snapshots/.
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();

    for &arch in ALL_ARCHES {
        for case in &cases {
            let path = common::binary_path(arch, case);
            if !path.exists() {
                // Not every (arch, case) pair builds — that's fine, but we
                // do NOT silently skip lift failures (see catch_unwind below);
                // only missing binaries.
                continue;
            }

            let func_names = exported_function_names(&path);
            if func_names.is_empty() {
                continue;
            }

            // Build the sleigh once per binary — analyzing each function
            // tears down its own sleigh internally, but we still need one
            // here for the post-analysis dot dump.
            let sleigh = sleigh_for(arch, &path);

            for func_name in func_names {
                let arch_copy = arch;
                let case_copy = case.clone();
                let func_copy = func_name.clone();
                let result = std::panic::catch_unwind(move || {
                    // Pin to v1 explicitly so the production-default
                    // flip (Phase 8.5c) does not move these snapshots.
                    common::analyze_v1(arch_copy, &case_copy, &func_copy)
                });
                let snapshot_body = match result {
                    Ok(g) => to_dot_string(&g, &sleigh),
                    Err(payload) => {
                        let msg = if let Some(s) = payload.downcast_ref::<String>() {
                            s.as_str()
                        } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                            *s
                        } else {
                            "<non-string panic payload>"
                        };
                        format!("LIFT_FAILED:{}", msg)
                    }
                };
                let name = format!("{}__{}__{}", arch.name(), case, func_name);
                insta::assert_snapshot!(name, snapshot_body);
            }
        }
    }
}
