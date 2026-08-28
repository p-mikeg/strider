//! Builds a CFG for every function symbol in every fixture ELF, in a debug
//! build so `strider-cfg`'s region-segmentation `debug_assert!`s fire: no
//! non-empty region has a phantom span, and `split_region` is never called on
//! an empty region nor takes the hole/round-down branch.  A violation panics; a
//! plain build error (a legit bail such as an unterminated function) is counted
//! and skipped.
//!
//! `build_cfg` only: nothing here lifts, optimises, or resolves, so it makes no
//! claim about `Strider::analyze`.  The orchestrator's own claims are covered
//! by `orchestrator_indirect_*` and `isa_mode_resolved_branch`.
//!
//! ```text
//! cargo test -p strider-orchestrator --test phantom_span_sweep -- --ignored --nocapture
//! ```

mod common;

use common::{ALL_ARCHES, Arch, driver_for_reader};
use object::{Object, ObjectSymbol, SymbolKind};

#[test]
#[ignore = "broad fixture sweep; run explicitly to exercise the phantom-span/split asserts across every function"]
fn build_cfg_phantom_span_asserts_hold_for_every_fixture_function() {
    let mut total = 0usize;
    let mut built = 0usize;
    for &arch in ALL_ARCHES {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out")
            .join(arch.name());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("elf") {
                continue;
            }
            let Ok(obj_owned) = strider_reader::load_elf(&path) else {
                continue;
            };
            let obj = obj_owned.file();
            let Ok(mem) = strider_reader::ElfFileMemReader::from_object(&obj) else {
                continue;
            };
            let (mut ana, _cc) = driver_for_reader(arch, mem);
            let addrs: Vec<u64> = obj
                .symbols()
                .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
                .map(|s| s.address())
                .collect();
            let cfg_opts = strider_cfg::CfgOptions {
                allow_code_before_start_addr: true,
                ..Default::default()
            };
            for raw in addrs {
                // ARM/Thumb symbols carry the Thumb bit in the address LSB.
                let addr = match arch {
                    Arch::Arm | Arch::ArmThumb => raw & !1u64,
                    _ => raw,
                };
                total += 1;
                if ana
                    .build_cfg(
                        strider_cfg::MachineInsnAddr::from(addr),
                        &cfg_opts,
                        &Default::default(),
                    )
                    .is_ok()
                {
                    built += 1;
                }
            }
        }
    }
    eprintln!(
        "phantom-span sweep: built {built}/{total} function CFGs across {} arches; no assert tripped",
        ALL_ARCHES.len()
    );
    assert!(
        total > 0,
        "sweep found no functions; run `make -C fixtures`"
    );
    // 2561/2720 = 94.2% when this floor was set; the rest are legit bails. A
    // drop past 90% is a segmentation regression, not fixture drift.
    let built_pct = 100.0 * built as f64 / total as f64;
    assert!(
        built_pct >= 90.0,
        "only {built}/{total} function CFGs built ({built_pct:.1}%), below the 90% floor"
    );
}
