//! A jump-table arm the cfg cannot express is dropped from the seated table.
//! `analyze` must report the site rather than converge on the smaller answer.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTargets};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

/// `jmp rax` at 0x1000, then a `movabs rax, imm64` at 0x1002 whose immediate
/// byte at 0x1005 decodes cleanly as `ret`. Seating 0x1005 as an arm would put
/// a region inside that instruction, so the cfg drops it and keeps 0x1002.
#[test]
fn an_arm_the_cfg_cannot_seat_is_reported_unresolved() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
    // 0x1002: movabs rax, imm64, with 0xc3 (`ret`) as the byte at 0x1005.
    let mut movabs = vec![0x48, 0xb8, 0x00, 0xc3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    movabs.resize(10, 0);
    bytes.append(&mut movabs);
    bytes.push(0x90); // 0x100c: nop, sealing the movabs region
    bytes.push(0xc3); // 0x100d: ret

    let arch = strider_target::SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");

    let branch = PcodeInsnAddr::at_machine_start(base);
    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        branch,
        ResolvedTargets::Multiple(vec![0x1005.into(), 0x1002.into()]),
    );
    let lift_opts = LiftOptions {
        cfg: CfgOptions {
            known_targets: known,
            ..CfgOptions::default()
        },
        ..LiftOptions::default()
    };

    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(base, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("a dropped arm is a result, not an error");

    let seated: Vec<u64> = result
        .cfg
        .regions()
        .filter_map(|r| match &r.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => {
                Some(targets.iter().map(|t| t.addr).collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        seated,
        vec![0x1002],
        "0x1005 is off an instruction boundary"
    );
    assert!(
        result
            .unresolved_indirect_branches
            .iter()
            .any(|a| a.machine_addr.addr == base),
        "the site lost an arm, so it cannot be claimed complete; got {:?}",
        result.unresolved_indirect_branches,
    );
}
