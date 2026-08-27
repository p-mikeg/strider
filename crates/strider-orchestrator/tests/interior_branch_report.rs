//! A branch into an instruction's interior cannot be seated exactly, so it has
//! to reach the caller on a report channel rather than look like a real edge.

use rsleigh::mem_readers::BufMemReader;

/// x86-64 at 0x1000: `movabs rax, 0` (ten bytes), `je 0x1005` -- five bytes
/// into that `movabs` -- then `ret`. No region can start at 0x1005, because
/// decoding from inside an instruction yields a different stream, so the taken
/// edge is seated on the region owning the bytes. That region starts at 0x1000,
/// making the CFG claim a self-loop the branch never takes.
#[test]
fn a_branch_into_an_instruction_interior_is_reported() {
    let base = 0x1000u64;
    let mut bytes: Vec<u8> = vec![0x48, 0xb8];
    bytes.extend_from_slice(&[0u8; 8]);
    bytes.extend_from_slice(&[0x74, 0xf9, 0xc3]);

    let sa = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        sa.sla_spec(),
        sa.pspec(),
        BufMemReader::new(bytes.clone(), base),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    let mut strider = strider_orchestrator::Strider::new(sa, sleigh, None).expect("Strider::new");
    let out = strider
        .analyze(
            base,
            &cc,
            &strider_orchestrator::LiftOptions::default(),
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .expect("analyze");

    assert_eq!(
        out.interior_branch_targets
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![0x1005],
        "an edge the CFG cannot seat exactly must reach the caller"
    );
}
