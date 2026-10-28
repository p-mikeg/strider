//! `hvc #imm` and `smc #imm` decode in A32 and Thumb-2 and lift to the
//! SMCCC calls the CallOther table describes: `r0` after one is the
//! conduit's result, not the function id that went in.

use crate::common;

use crate::common::Arch;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

const BASE: u64 = 0x1000;

fn r0_producer(arch: Arch, bytes: Vec<u8>, entry: u64) -> String {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider
        .analyze(entry, &cc, &Default::default(), None)
        .expect("analyze");
    assert!(result.incomplete().is_empty(), "{:?}", result.incomplete());
    let f = result.function;
    let producer = f.producer(common::returned(&f)[0]);
    match f.node_kind(producer) {
        NodeKind::CallOther { .. } => f
            .call_other_name(producer)
            .expect("a named user op")
            .to_owned(),
        other => format!("{other:?}"),
    }
}

#[test]
fn hvc_and_smc_return_the_smccc_result_in_r0() {
    let mut wrong = Vec::new();
    // mov r0, #0x80000001; <op>; bx lr
    for (name, op, want) in [
        ("hvc #0", 0xe140_0070, "software_hvc"),
        ("smc #0", 0xe160_0070, "software_smc"),
        ("hvc #0x1234", 0xe141_2374, "software_hvc"),
    ] {
        let words = [0xe3a0_0106, op, 0xe12f_ff1e];
        let bytes = common::words_image(&words, strider_target::Endianness::Little);
        let got = r0_producer(Arch::Arm, bytes, BASE);
        if got != want {
            wrong.push(format!("{name}: r0 comes from {got}"));
        }
    }
    // mov.w r0, #1; <op>; bx lr
    for (name, op, want) in [
        ("hvc.w #0", [0xf7e0, 0x8000], "software_hvc"),
        ("smc #0", [0xf7f0, 0x8000], "software_smc"),
    ] {
        let halves = [0xf04f, 0x0001, op[0], op[1], 0x4770];
        let bytes = halves.iter().flat_map(|h: &u16| h.to_le_bytes()).collect();
        let got = r0_producer(Arch::ArmThumb, bytes, BASE | 1);
        if got != want {
            wrong.push(format!("{name} (thumb): r0 comes from {got}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
