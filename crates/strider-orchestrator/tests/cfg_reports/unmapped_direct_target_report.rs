//! A direct branch out of the mapped image is reported, not a failure: the
//! regions that did decode survive and the target is reported.

use crate::common;

const BASE: u64 = 0x1000;
const TARGET: u64 = 0x4000_1005;

#[test]
fn a_direct_branch_out_of_the_image_is_reported_not_failed() {
    // 0x1000: jmp 0x40001005, then `ret` filler.
    let mut bytes = vec![0xe9, 0x00, 0x00, 0x00, 0x40];
    bytes.extend_from_slice(&[0xc3; 8]);

    let result = common::analyze_bytes(common::Arch::X64, bytes, BASE, None, BASE);

    assert_eq!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnmappedTarget)
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![TARGET],
    );
    assert!(
        !result.incomplete().is_empty(),
        "a CFG that stops at the edge of the image carries a caveat",
    );
}
