//! A jump table whose index is an incoming stack argument, bounded by one load
//! and reloaded for the dispatch past a write:
//!
//! ```text
//! 401000: 8b 44 24 08                    mov    0x8(%rsp),%eax
//! 401004: 83 f8 03                       cmp    $0x3,%eax
//! 401007: 77 2e                          ja     401037
//! 401009: c7 04 25 00 30 40 00 01 00 00 00  movl $0x1,0x403000
//! 401014: 8b 44 24 08                    mov    0x8(%rsp),%eax
//! 401018: ff 24 c5 00 20 40 00           jmp    *0x402000(,%rax,8)
//! 40101f: b8 01 00 00 00 c3              mov $0x1,%eax ; ret   (and three more)
//! 401037: 31 c0 c3                       xor %eax,%eax ; ret
//! ```
//!
//! Nothing stores to the slot, so neither load forwards. The two are one
//! value only once the reload's memory edge is narrowed past the write, and
//! only then does the bound check bound the index.

use strider_cfg::RegionTerminator;
use strider_ir_test_utils::MockRom;
use strider_orchestrator::opt::{AssumptionOptions, OptimizerPipeline};
use strider_orchestrator::{AnalyzeOptions, Reason};

use crate::common;

const BASE: u64 = 0x40_1000;
const TABLE: u64 = 0x40_2000;

/// The arms of every seated `Switch` and the unresolved sites, for `code` at
/// `BASE` whose table holds `arms`.
fn analyze(
    code: &str,
    arms: [u64; 4],
    assumptions: AssumptionOptions,
) -> (Vec<Vec<u64>>, Vec<u64>) {
    let (switches, unresolved, _) = analyze_with(code, arms, assumptions, None);
    (switches, unresolved)
}

/// [`analyze`] under `pipeline`, also counting the first stack argument's
/// carriers.
fn analyze_with(
    code: &str,
    arms: [u64; 4],
    assumptions: AssumptionOptions,
    pipeline: Option<OptimizerPipeline>,
) -> (Vec<Vec<u64>>, Vec<u64>, usize) {
    let code = (0..code.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&code[i..i + 2], 16).expect("hex"))
        .collect();
    let table = arms.iter().flat_map(|a| a.to_le_bytes()).collect();
    let rom = MockRom::raw_bytes(TABLE, table);
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::X64, code, BASE, Some(Box::new(rom)));
    let opts = AnalyzeOptions {
        assumptions,
        ..AnalyzeOptions::default()
    };
    let first_stack_arg = cc.arg_passing_regs.len() as u32;
    let result = strider
        .analyze(BASE, &cc, &opts, pipeline)
        .expect("analyze");
    let carriers = result
        .function
        .side_tables()
        .arg_index_to_values(first_stack_arg)
        .len();
    let unresolved = result
        .incomplete()
        .of(Reason::UnresolvedBranch)
        .map(|a| a.machine_addr.addr)
        .collect();
    let switches = result
        .cfg
        .regions()
        .filter_map(|region| match &region.terminator {
            RegionTerminator::Switch { targets, .. } => {
                let mut arms: Vec<u64> = targets.iter().map(|t| t.addr).collect();
                arms.sort_unstable();
                arms.dedup();
                Some(arms)
            }
            _ => None,
        })
        .collect();
    (switches, unresolved, carriers)
}

/// `stack_global_disjoint` puts the constant-address write off the slot, and
/// the narrowing honours it. Without `FunctionArgDetect`, whose narrowing
/// would merge the reads as well.
///
/// Mutation: `LoadForward` leaves the edge where it is.
#[test]
fn a_reload_past_a_global_store_is_the_bounded_index() {
    let (switches, unresolved, _) = analyze_with(
        GLOBAL_STORE,
        GLOBAL_STORE_ARMS,
        AssumptionOptions::default(),
        Some(default_less(true, false)),
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [GLOBAL_STORE_ARMS.to_vec()]);
}

/// A `call` in place of the write, in a frame no stack address escapes:
///
/// ```text
/// 401000: 48 83 ec 18     sub    $0x18,%rsp
/// 401004: 8b 44 24 08     mov    0x8(%rsp),%eax
/// 401008: 83 f8 03        cmp    $0x3,%eax
/// 40100b: 77 2c           ja     401039
/// 40100d: e8 ee 3f 00 00  call   405000
/// 401012: 8b 44 24 08     mov    0x8(%rsp),%eax
/// 401016: ff 24 c5 00 20 40 00  jmp *0x402000(,%rax,8)
/// 40101d: b8 01 00 00 00 eb 17  mov $0x1,%eax ; jmp 40103b  (and three more)
/// 401039: 31 c0           xor    %eax,%eax
/// 40103b: 48 83 c4 18 c3  add    $0x18,%rsp ; ret
/// ```
///
/// `escape_analysis` lets forwarding step through the call, but the narrowing
/// honours no call relaxation, so it stops there.
///
/// Mutation: narrow under `Policy::forward`.
#[test]
fn a_reload_past_a_call_stays_unbounded() {
    const ARMS: [u64; 4] = [0x40_101d, 0x40_1024, 0x40_102b, 0x40_1032];
    let code = "4883ec188b44240883f803772ce8ee3f00008b442408ff24c500204000\
                b801000000eb17b802000000eb10b803000000eb09b804000000eb02\
                31c04883c418c3";
    let assumptions = AssumptionOptions {
        escape_analysis: true,
        ..AssumptionOptions::default()
    };
    let (switches, unresolved) = analyze(code, ARMS, assumptions);
    assert_eq!(unresolved, [0x40_1016]);
    assert!(switches.is_empty(), "{switches:#x?}");
}

/// The default pipeline with `LoadForward` and `FunctionArgDetect` only where
/// asked, so a test sees one pass's narrowing alone.
fn default_less(load_forward: bool, arg_detection: bool) -> OptimizerPipeline {
    use strider_orchestrator::opt::*;
    let mut p = OptimizerPipeline::new();
    p.add(&ConstantFold);
    p.add(&LoadReadOnly);
    p.add(&KnownBits);
    p.add(&FlagCmpCanonicalize);
    p.add(&IfCondInversion);
    p.add(&PhiCollapse);
    p.add(&RegionCollapse);
    p.add(&DeadBranchElimination);
    if load_forward {
        p.add(&LoadForward);
    }
    p.add_post_pass(&CallStackArgCollect);
    if arg_detection {
        p.add_post_pass(&FunctionArgDetect);
    }
    p
}

const GLOBAL_STORE_ARMS: [u64; 4] = [0x40_101f, 0x40_1025, 0x40_102b, 0x40_1031];
const GLOBAL_STORE: &str = "8b44240883f803772ec7042500304000010000008b442408ff24c500204000\
                            b801000000c3b802000000c3b803000000c3b804000000c331c0c3";

/// Both reads are the first stack argument. Argument detection narrows the
/// reload past the global write under `stack_global_disjoint`, the two merge
/// into one carrier, and the bound check bounds the index.
///
/// Mutation: narrow `FunctionArgDetect` with `stack_global_disjoint` off.
#[test]
fn arg_detection_narrows_a_reload_past_a_global_store() {
    let (switches, unresolved, carriers) = analyze_with(
        GLOBAL_STORE,
        GLOBAL_STORE_ARMS,
        AssumptionOptions::default(),
        Some(default_less(false, true)),
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [GLOBAL_STORE_ARMS.to_vec()]);
    assert_eq!(carriers, 1);
}

/// At the floor the write may be the slot, so neither the detection nor the
/// narrowing steps past it: the first read is the argument's one carrier,
/// the reload none.
///
/// Mutation: honour `stack_global_disjoint` in every purpose whatever the
/// function claims.
#[test]
fn arg_detection_at_the_floor_keeps_the_reload_unbounded() {
    let (switches, unresolved, carriers) = analyze_with(
        GLOBAL_STORE,
        GLOBAL_STORE_ARMS,
        AssumptionOptions::none(),
        Some(default_less(false, true)),
    );
    assert_eq!(unresolved, [0x40_1018]);
    assert!(switches.is_empty(), "{switches:#x?}");
    assert_eq!(carriers, 1);
}

/// The bytes of [`a_reload_past_a_call_stays_unbounded`], reading the first
/// stack argument, `0x20(%rsp)`, in place of the local. Detection steps the
/// call under `assume_incoming_args_survive_calls`, so both reads are the
/// argument, but the narrowing stops at the call.
///
/// Mutation: narrow `FunctionArgDetect` under `Policy::incoming_args`.
#[test]
fn arg_detection_does_not_narrow_a_reload_past_a_call() {
    const ARMS: [u64; 4] = [0x40_101d, 0x40_1024, 0x40_102b, 0x40_1032];
    let code = "4883ec188b44242083f803772ce8ee3f00008b442420ff24c500204000\
                b801000000eb17b802000000eb10b803000000eb09b804000000eb02\
                31c04883c418c3";
    let (switches, unresolved, carriers) = analyze_with(
        code,
        ARMS,
        AssumptionOptions::default(),
        Some(default_less(false, true)),
    );
    assert_eq!(unresolved, [0x40_1016]);
    assert!(switches.is_empty(), "{switches:#x?}");
    assert_eq!(carriers, 2);
}
