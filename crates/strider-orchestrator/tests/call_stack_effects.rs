//! What a call does to the caller's stack pointer beyond the calling
//! convention's defaults.

mod common;
use common::Arch;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

const BASE: u64 = 0x1000;

fn analyze(arch: Arch, bytes: Vec<u8>) -> strider_ir::Function {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze");
    assert!(result.is_complete(), "no incompleteness is expected here");
    result.function
}

/// The SP-relative offset of the stack slot the first returned value loads.
fn returned_stack_slot(f: &strider_ir::Function) -> i128 {
    let v = common::returned(f)[0];
    let load = f.producer(v);
    assert!(
        matches!(f.node_kind(load), NodeKind::Load(_)),
        "return slot 0 is not a Load: {:?}",
        f.node_kind(load)
    );
    f.stack_offset(load)
        .unwrap_or_else(|| panic!("the returned Load's address is not SP-relative"))
        .1
}

#[test]
fn int_0x80_moves_no_stack_pointer_on_x86() {
    // mov eax,20; int 0x80; mov eax,[esp+4]; ret
    let f = analyze(
        Arch::X86,
        vec![
            0xb8, 0x14, 0, 0, 0, 0xcd, 0x80, 0x8b, 0x44, 0x24, 0x04, 0xc3,
        ],
    );
    assert_eq!(returned_stack_slot(&f), 4);
}

#[test]
fn int_0x80_moves_no_stack_pointer_on_x86_64() {
    // mov eax,39; int 0x80; mov rax,[rsp+8]; ret
    let f = analyze(
        Arch::X64,
        vec![
            0xb8, 0x27, 0, 0, 0, 0xcd, 0x80, 0x48, 0x8b, 0x44, 0x24, 0x08, 0xc3,
        ],
    );
    assert_eq!(returned_stack_slot(&f), 8);
}

#[test]
fn into_moves_no_stack_pointer() {
    // into; mov eax,[esp+4]; ret
    let f = analyze(Arch::X86, vec![0xce, 0x8b, 0x44, 0x24, 0x04, 0xc3]);
    assert_eq!(returned_stack_slot(&f), 4);
}
