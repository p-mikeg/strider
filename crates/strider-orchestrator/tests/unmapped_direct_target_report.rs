//! A direct branch out of the mapped image is a result, not an error: the
//! regions that did decode survive and the target is reported.

use rsleigh::mem_readers::BufMemReader;
use strider_orchestrator::Strider;

const BASE: u64 = 0x1000;
const TARGET: u64 = 0x4000_1005;

#[test]
fn a_direct_branch_out_of_the_image_is_reported_not_raised() {
    let arch = strider_target::SleighArch::x86_64();
    // 0x1000: jmp 0x40001005, then `ret` filler.
    let mut bytes = vec![0xe9, 0x00, 0x00, 0x00, 0x40];
    bytes.extend_from_slice(&[0xc3; 8]);
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, BASE),
    )
    .expect("sleigh");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&sleigh.regs().expect("regs"))
        .expect("cc");

    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("an unmapped direct target must not fail the whole function");

    assert_eq!(
        result
            .unmapped_branch_targets
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![TARGET],
    );
    assert!(
        !result.is_complete(),
        "a CFG that stops at the edge of the image carries a caveat",
    );
}
