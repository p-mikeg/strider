//! End-to-end test for `RunConfig::compact`.
//!
//! Drives `strider::run` on a small inline-byte function under both
//! compact=true and compact=false; asserts the compact graph has no
//! more node ids than the non-compact graph AND identical
//! pattern-match counts on a representative query.

#![allow(clippy::unwrap_used)]

use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, RunConfig, SleighArch, Strider};

/// Minimal x86_64 function: `mov rax, 42; ret`.
fn x86_64_call_then_ret_bytes() -> (Vec<u8>, u64) {
    // 48 c7 c0 2a 00 00 00     mov rax, 42
    // c3                        ret
    let bytes = vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    (bytes, entry)
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

fn run_with(compact: bool) -> ir::BuiltFunctionGraph {
    let strider = make_strider();
    let (bytes, entry) = x86_64_call_then_ret_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();
    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact,
        per_address_ccs: std::collections::HashMap::new(),
    };
    strider::run(config).unwrap()
}

#[test]
fn compact_yields_no_more_node_ids_than_non_compact() {
    let compact_graph = run_with(true);
    let noncompact_graph = run_with(false);
    let compact_count = compact_graph.graph.all_node_ids().count();
    let noncompact_count = noncompact_graph.graph.all_node_ids().count();
    assert!(
        compact_count <= noncompact_count,
        "compact={compact_count} must not exceed non-compact={noncompact_count}"
    );
}

#[test]
fn compact_preserves_reachable_pattern_matches() {
    use pattern::{Matcher, Pat, ret};

    let compact_graph = run_with(true);
    let noncompact_graph = run_with(false);

    let pat: Pat = ret().into();
    let compact_matches = Matcher::new(&compact_graph).find_all(&pat).len();
    let noncompact_matches = Matcher::new(&noncompact_graph).find_all(&pat).len();
    assert_eq!(
        compact_matches, noncompact_matches,
        "ret() match count must be invariant under compaction"
    );
}
