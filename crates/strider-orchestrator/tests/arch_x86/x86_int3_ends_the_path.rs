//! `int3` and `int1` end the path and are reported: most are padding after a
//! call that never returns, and a handler that resumes (a debugger, the
//! kernel's `int3_selftest`) is not something the decode can know. `int 3`
//! (`cd 03`), the `int imm8` encoding of the same trap, falls through its
//! handler call, as every `int imm8` does.

use crate::common;

use crate::common::Arch;
use strider_cfg::Reason;
use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker};

fn analyze(arch: Arch, bytes: Vec<u8>, base: u64) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, base, None);
    strider
        .analyze(base, &cc, &Default::default(), None)
        .expect("analyze")
}

fn count(f: &Function, pred: impl Fn(&NodeKind) -> bool) -> usize {
    f.walk().filter(|&n| pred(f.node_kind(n))).count()
}

fn reported_at(r: &strider_orchestrator::AnalyzeResult, addr: u64) -> bool {
    r.cfg
        .incomplete()
        .iter()
        .any(|(a, why)| a.machine_addr.addr == addr && why == Reason::UnresolvedBranch)
}

#[test]
fn a_breakpoint_ends_the_path_and_is_reported() {
    const BASE: u64 = 0x10000;
    for arch in [Arch::X64, Arch::X86] {
        for (name, trap) in [("int3", 0xcc), ("int1", 0xf1)] {
            // mov eax, 7; <trap>; ret
            let r = analyze(arch, vec![0xb8, 7, 0, 0, 0, trap, 0xc3], BASE);
            assert!(
                reported_at(&r, BASE + 5),
                "{arch:?} {name}: {:?}",
                r.cfg.incomplete()
            );
        }
        // mov eax, 7; int 3; ret
        let r = analyze(arch, vec![0xb8, 7, 0, 0, 0, 0xcd, 0x03, 0xc3], BASE);
        assert!(
            r.cfg.incomplete().is_empty(),
            "{arch:?} int 3: {:?}",
            r.cfg.incomplete()
        );
        assert_eq!(count(&r.function, |k| matches!(k, NodeKind::Return)), 1);
    }
}

/// x86 Linux 6.6 (clang) `devtmpfsd` ends `call devtmpfs_work_loop; int3 x3`,
/// a call that never returns, and `sock_inuse_exit_net` follows. The path
/// stops at the first `int3` and says so; it must not run into the next
/// function.
#[test]
fn devtmpfsd_stops_at_its_padding() {
    const BASE: u64 = 0xc12b_34dc;
    let bytes = vec![
        0x56, 0x89, 0xc1, 0xe8, 0x84, 0x5f, 0x11, 0x00, 0x89, 0xc6, 0xb8, 0xe8, 0xcc, 0x3d, 0xc1,
        0xe8, 0x14, 0xb3, 0xd8, 0xff, 0x85, 0xf6, 0x74, 0x04, 0x89, 0xf0, 0x5e, 0xc3, 0xe8, 0x57,
        0x5c, 0xee, 0xff, 0xcc, 0xcc, 0xcc, 0x8b, 0x80, 0xd4, 0x00, 0x00, 0x00, 0xe9, 0x6d, 0x24,
        0xdc, 0xff, 0x90, 0x8b, 0x50, 0x58, 0xb8, 0x84, 0x01, 0x31, 0xc1, 0xe9, 0x7b, 0xd3, 0xe1,
        0xff, 0xcc, 0xcc, 0xcc, 0x8b, 0x40, 0x48, 0xe9, 0xf4, 0xcb, 0xe1, 0xff,
    ];
    let r = analyze(Arch::X86, bytes, BASE);
    assert!(reported_at(&r, 0xc12b_34fd), "{:?}", r.cfg.incomplete());
    let next = strider_cfg::MachineInsnAddr::from(0xc12b_3500u64);
    assert!(
        r.cfg
            .region_graph()
            .node_weights()
            .all(|reg| reg.start_addr.machine_addr != next),
        "decoded into sock_inuse_exit_net"
    );
}

/// x64 Linux 5.14 (el9) `__tdx_hypercall` ends `call __tdx_hypercall_failed;
/// int3 x7`, then `__pfx___tdcall`'s nops and `__tdcall`, whose `tdcall` the
/// sla cannot decode. The path stops at the padding, so the lift succeeds.
#[test]
fn tdx_hypercall_stops_before_tdcall() {
    const BASE: u64 = 0xffff_ffff_81d5_77e0;
    let mut bytes = vec![
        0x53, 0x48, 0x89, 0xfe, 0x48, 0x89, 0xfb, 0x48, 0xc7, 0x07, 0xcc, 0xff, 0x00, 0x00, 0x31,
        0xff, 0xe8, 0xbb, 0x00, 0x00, 0x00, 0x48, 0x85, 0xc0, 0x75, 0x0a, 0x48, 0x8b, 0x43, 0x20,
        0x5b, 0xe9, 0xec, 0x94, 0x01, 0x00, 0xe8, 0xb7, 0xff, 0xff, 0xff,
    ];
    bytes.extend([0xcc; 7]);
    bytes.extend([0x90; 16]);
    bytes.extend([
        0x48, 0x89, 0xf8, 0x48, 0x8b, 0x0e, 0x48, 0x8b, 0x56, 0x08, 0x4c, 0x8b, 0x46, 0x10, 0x4c,
        0x8b, 0x4e, 0x18, 0x4c, 0x8b, 0x56, 0x20, 0x4c, 0x8b, 0x5e, 0x28, 0x66, 0x0f, 0x01, 0xcc,
        0xe9, 0xad, 0x94, 0x01, 0x00,
    ]);
    let r = analyze(Arch::X64, bytes, BASE);
    assert!(
        reported_at(&r, 0xffff_ffff_81d5_7809),
        "{:?}",
        r.cfg.incomplete()
    );
}
