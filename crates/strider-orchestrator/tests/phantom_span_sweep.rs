//! Builds a CFG for every function symbol in every fixture ELF, in a debug
//! build so `strider-cfg`'s region-segmentation `debug_assert!`s fire: no
//! non-empty region has a phantom span, and `split_region` is never called on
//! an empty region nor takes the hole/round-down branch.  A violation panics,
//! and every fixture function must build.
//!
//! `build_cfg` only: nothing here lifts, optimises, or resolves.
//!
//! ```text
//! cargo test -p strider-orchestrator --test phantom_span_sweep -- --ignored --nocapture
//! ```

mod common;

use common::fixtures::{FIXTURE_ARCHES, Fixture};

#[test]
#[ignore = "broad fixture sweep; run explicitly to exercise the phantom-span/split asserts across every function"]
fn build_cfg_phantom_span_asserts_hold_for_every_fixture_function() {
    let cfg_opts = strider_cfg::CfgOptions {
        bound: strider_cfg::FunctionBound::Open {
            code_before_start: true,
        },
        ..Default::default()
    };
    let mut total = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for arch in FIXTURE_ARCHES {
        for fixture in Fixture::all(arch) {
            let (mut strider, _cc) = fixture.strider();
            for (addr, _name) in fixture.functions() {
                total += 1;
                if let Err(e) = strider.build_cfg(addr, &cfg_opts) {
                    failed.push(format!("{}@{addr:#x}: {e:#}", fixture.path.display()));
                }
            }
        }
    }
    eprintln!(
        "phantom-span sweep: {total} function CFGs across {} arches; no assert tripped",
        FIXTURE_ARCHES.len()
    );
    assert!(
        failed.is_empty(),
        "{} of {total} function CFGs failed to build:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
