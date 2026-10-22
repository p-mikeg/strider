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
use strider_orchestrator::opt::AssumptionOptions;
use strider_orchestrator::{AnalyzeOptions, Reason};

mod common;

const BASE: u64 = 0x40_1000;
const TABLE: u64 = 0x40_2000;

/// The arms of every seated `Switch` and the unresolved sites, for `code` at
/// `BASE` whose table holds `arms`.
fn analyze(
    code: &str,
    arms: [u64; 4],
    assumptions: AssumptionOptions,
) -> (Vec<Vec<u64>>, Vec<u64>) {
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
    let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
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
    (switches, unresolved)
}

/// `stack_global_disjoint` puts the constant-address write off the slot, and
/// the narrowing honours it.
///
/// Mutation: narrow under `MemPurpose::Structural`.
#[test]
fn a_reload_past_a_global_store_is_the_bounded_index() {
    const ARMS: [u64; 4] = [0x40_101f, 0x40_1025, 0x40_102b, 0x40_1031];
    let code = "8b44240883f803772ec7042500304000010000008b442408ff24c500204000\
                b801000000c3b802000000c3b803000000c3b804000000c331c0c3";
    let (switches, unresolved) = analyze(code, ARMS, AssumptionOptions::default());
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [ARMS.to_vec()]);
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
/// Mutation: narrow under `MemPurpose::Forward`.
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
