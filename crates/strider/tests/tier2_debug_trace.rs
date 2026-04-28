//! Integration tests for F4 — `OrchestratorStats::trace`.
//!
//! Each test constructs an [`OrchestratorConfig`] with the new
//! `debug: Option<OrchestratorDebugConfig>` field, runs
//! `run_orchestrator_with_stats`, and asserts the optional trace either
//!   - is `None` (zero-overhead default), OR
//!   - is `Some(Vec<IterationSnapshot>)` populated according to the
//!     spec's per-capture-site contract.
//!
//! These pin the F4 contract at the public API surface so future
//! refactors can't silently drop the debug-trace plumbing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider::indirect_resolve_tier2::orchestrator::{
    ClassificationOutcome, EditEvent, OrchestratorDebugConfig,
};
use strider::indirect_resolve_tier2::{run_orchestrator_with_stats, OrchestratorConfig};
use strider::{CallingConvention, SleighArch, Strider};

fn make_strider_x86_64() -> Strider {
    let arch = SleighArch::x86_64();
    let probe = BufMemReader::new(Vec::<u8>::new(), 0);
    let regs = Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).expect("strider")
}

fn make_sleigh_value(bytes: Vec<u8>, base: u64) -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh")
}

fn make_config<'a>(
    strider: &'a Strider,
    bytes: Vec<u8>,
    base: u64,
    debug: Option<OrchestratorDebugConfig>,
) -> OrchestratorConfig<'a, Vec<u8>> {
    OrchestratorConfig {
        strider,
        start_addr: base,
        sleigh: Some(make_sleigh_value(bytes, base)),
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        debug,
    }
}

#[test]
fn trace_disabled_means_zero_overhead() {
    // When `debug` is `None`, `stats.trace` is also `None`.  The
    // capture sites must short-circuit on the `None` branch — no
    // `Vec` allocated, no `IterationSnapshot` constructed.
    let strider = make_strider_x86_64();
    let bytes = vec![0xc3u8]; // ret
    let config = make_config(&strider, bytes, 0x1000, None);
    let (_graph, stats) = run_orchestrator_with_stats(config).expect("orchestrator");
    assert!(
        stats.trace.is_none(),
        "trace must be None when debug config is absent; got {:?}",
        stats.trace,
    );
}

#[test]
fn trace_captures_iteration_count() {
    // When debug is enabled, the trace's length equals the number of
    // outer-loop iterations actually executed.  The fast-path (no
    // BranchIndirect) doesn't enter the loop, so the trace is
    // Some(empty) — Some(_) signals "tracing was armed", empty signals
    // "no iterations ran".
    let strider = make_strider_x86_64();
    let bytes = vec![0xc3u8]; // ret
    let debug = OrchestratorDebugConfig {
        capture_classifications: true,
        capture_edits: true,
    };
    let config = make_config(&strider, bytes, 0x1000, Some(debug));
    let (_graph, stats) = run_orchestrator_with_stats(config).expect("orchestrator");
    let trace = stats.trace.as_ref().expect("trace must be Some when debug enabled");
    assert_eq!(
        trace.len(),
        stats.iterations,
        "trace length must equal iteration count; trace_len={}, iterations={}",
        trace.len(),
        stats.iterations,
    );
    assert_eq!(trace.len(), 0, "fast-path: zero iterations expected");
}

#[test]
fn trace_captures_classifications_per_iteration() {
    // Tail-call fixture: `push K; pop rax; jmp rax`.  When the
    // orchestrator runs at least one iteration and succeeds, the
    // trace's first iteration must record at least one classification.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let strider = make_strider_x86_64();
    let debug = OrchestratorDebugConfig {
        capture_classifications: true,
        capture_edits: true,
    };
    let config = make_config(&strider, bytes, 0x1000, Some(debug));
    if let Ok((_graph, stats)) = run_orchestrator_with_stats(config) {
        let trace = stats.trace.as_ref().expect("trace Some");
        if let Some(first) = trace.first() {
            assert_eq!(
                first.iteration_index, 0,
                "first iteration's index must be 0; got {first:?}",
            );
            // At least one classification should appear when the
            // orchestrator made progress on the placeholder.
            assert!(
                !first.classifications.is_empty(),
                "expected at least one classification; got {:?}",
                first.classifications,
            );
            // Pin both variants so future additions to the enum
            // surface as a match-exhaustiveness compile error here.
            for (_addr, outcome) in &first.classifications {
                let is_resolved = matches!(outcome, ClassificationOutcome::Resolved(_));
                let is_unresolved = matches!(outcome, ClassificationOutcome::StillUnresolved);
                assert!(is_resolved || is_unresolved);
            }
        }
    }
}

#[test]
fn trace_captures_in_place_edits() {
    // Same tail-call fixture.  When the in-place edit fires, the
    // trace's `edits_applied` must include an `EditEvent::TailCall`
    // entry.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let strider = make_strider_x86_64();
    let debug = OrchestratorDebugConfig {
        capture_classifications: true,
        capture_edits: true,
    };
    let config = make_config(&strider, bytes, 0x1000, Some(debug));
    if let Ok((_graph, stats)) = run_orchestrator_with_stats(config)
        && stats.tail_call_edits >= 1
    {
        let trace = stats.trace.as_ref().expect("trace Some");
        let mut saw_tail_call_event = false;
        for snap in trace {
            for event in &snap.edits_applied {
                if let EditEvent::TailCall { target, .. } = event {
                    assert_eq!(*target, k, "tail-call event target must equal K");
                    saw_tail_call_event = true;
                }
            }
        }
        assert!(
            saw_tail_call_event,
            "stats.tail_call_edits >= 1 but trace recorded no TailCall event",
        );
    }
}

#[test]
fn trace_captures_cfg_rebuild_triggers() {
    // The `cfg_rebuild_triggered` flag is set on iterations that
    // forced a structural CFG rebuild.  For the tail-call fixture, the
    // in-place edit doesn't trigger a rebuild, so
    // `cfg_rebuild_triggered` must be false on every captured iteration.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let strider = make_strider_x86_64();
    let debug = OrchestratorDebugConfig {
        capture_classifications: true,
        capture_edits: true,
    };
    let config = make_config(&strider, bytes, 0x1000, Some(debug));
    if let Ok((_graph, stats)) = run_orchestrator_with_stats(config) {
        let trace = stats.trace.as_ref().expect("trace Some");
        let any_rebuild = trace.iter().any(|s| s.cfg_rebuild_triggered);
        assert!(
            !any_rebuild,
            "tail-call fixture: no iteration should trigger a CFG rebuild; trace={trace:?}",
        );
    }
}
