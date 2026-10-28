use strider_orchestrator::AnalyzeOptions;

use crate::common;

/// Minimal x86_64 function: `mov rax, 42; ret`.
fn x86_64_mov_then_ret_bytes() -> (Vec<u8>, u64) {
    // 48 c7 c0 2a 00 00 00   mov rax, 42
    // c3                     ret
    let bytes = vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    (bytes, entry)
}

fn run_with(compact: bool) -> strider_ir::Function {
    let (bytes, entry) = x86_64_mov_then_ret_bytes();
    let opts = AnalyzeOptions {
        compact,
        ..AnalyzeOptions::default()
    };
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, entry, None);
    strider.analyze(entry, &cc, &opts, None).unwrap().function
}

/// `compact` drops exactly the unreachable arena entries: the compacted node
/// count equals the reachable count of BOTH functions, and is strictly smaller
/// than the uncompacted arena, so a no-op `compact` fails here.
#[test]
fn compact_drops_exactly_the_unreachable_nodes() {
    use strider_ir::IRWalker;

    let compact_function = run_with(true);
    let noncompact_function = run_with(false);
    let compact_count = compact_function.graph().all_node_ids().count();
    let noncompact_count = noncompact_function.graph().all_node_ids().count();
    let reachable = noncompact_function.walk().count();
    assert!(
        compact_count < noncompact_count,
        "compact={compact_count} must be strictly smaller than \
         non-compact={noncompact_count}, or compaction did nothing"
    );
    assert_eq!(
        compact_count, reachable,
        "compacted arena must hold exactly the reachable nodes"
    );
    assert_eq!(
        compact_function.walk().count(),
        reachable,
        "compaction must not change the reachable set"
    );
}

#[test]
fn compact_preserves_reachable_pattern_matches() {
    use strider_pattern::{Matcher, ret};

    let compact_function = run_with(true);
    let noncompact_function = run_with(false);

    let pat = ret().build();
    let compact_matches = Matcher::new(&compact_function)
        .find_all(&pat)
        .unwrap()
        .len();
    let noncompact_matches = Matcher::new(&noncompact_function)
        .find_all(&pat)
        .unwrap()
        .len();
    assert_eq!(
        compact_matches, noncompact_matches,
        "ret() match count must be invariant under compaction"
    );
}

/// What `Default` is documented to be, and its tables are one shared static
/// rather than an allocation per call.
#[test]
fn default_analyze_options_compact_and_share_their_empty_tables() {
    let (a, b) = (AnalyzeOptions::default(), AnalyzeOptions::default());
    assert!(a.compact);
    assert!(a.resolve_indirect_branches);
    assert_eq!(
        a.assumptions,
        strider_orchestrator::opt::AssumptionOptions::default()
    );
    assert_eq!(
        a.cfg.bound,
        strider_cfg::FunctionBound::Open {
            code_before_start: false
        }
    );
    assert!(a.cfg.known_targets.is_empty() && a.per_address_ccs.is_empty());
    assert!(std::ptr::eq(a.per_address_ccs, b.per_address_ccs));
    assert!(std::ptr::eq(a.cfg.known_targets, b.cfg.known_targets));
}
