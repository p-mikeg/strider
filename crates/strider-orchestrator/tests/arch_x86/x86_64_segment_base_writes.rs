//! `swapgs` and `wrgsbase` move the base every later `gs:` address reads.

use crate::common;

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x10000;
/// `mov rax, gs:[0]`
const LOAD_GS0_RAX: [u8; 9] = [0x65, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0];
/// `mov rdx, gs:[0]`
const LOAD_GS0_RDX: [u8; 9] = [0x65, 0x48, 0x8b, 0x14, 0x25, 0, 0, 0, 0];

/// The address operand of each `Load`, in walk order.
fn load_addresses(bytes: Vec<u8>) -> Vec<strider_ir::node::ValueId> {
    let f = common::analyze_bytes(common::Arch::X64, bytes, BASE, None, BASE).function;
    f.walk()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Load(_)))
        .map(|n| f.node_inputs(n)[1])
        .collect()
}

#[test]
fn a_gs_access_after_a_base_write_reads_the_new_base() {
    // swapgs; wrgsbase rdi
    for write in [&[0x0f, 0x01, 0xf8][..], &[0xf3, 0x48, 0x0f, 0xae, 0xdf]] {
        let bytes = [&LOAD_GS0_RAX[..], write, &LOAD_GS0_RDX, &[0xc3]].concat();
        let addrs = load_addresses(bytes);
        assert_eq!(addrs.len(), 2, "{write:x?}");
        assert_ne!(addrs[0], addrs[1], "{write:x?}: both loads read one base");
    }
}
