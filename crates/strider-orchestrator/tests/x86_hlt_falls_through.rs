//! `hlt` waits for an interrupt and resumes at the next instruction, and the
//! interrupt handler may write memory meanwhile.

mod common;

use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker};

const BASE: u64 = 0x10000;

fn analyze(bytes: Vec<u8>) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
}

fn count(f: &Function, pred: impl Fn(&NodeKind) -> bool) -> usize {
    f.walk().filter(|&n| pred(f.node_kind(n))).count()
}

#[test]
fn an_idle_loop_body_returns_after_its_hlt() {
    // sti; hlt; cli; ret
    let r = analyze(vec![0xfb, 0xf4, 0xfa, 0xc3]);
    assert!(r.cfg.incomplete().is_empty(), "{:?}", r.cfg.incomplete());
    assert_eq!(count(&r.function, |k| matches!(k, NodeKind::Return)), 1);
}

#[test]
fn a_load_is_not_forwarded_across_hlt() {
    // mov eax, [rdi]; hlt; add eax, [rdi]; ret
    let r = analyze(vec![0x8b, 0x07, 0xf4, 0x03, 0x07, 0xc3]);
    assert_eq!(count(&r.function, |k| matches!(k, NodeKind::Load(_))), 2);
}

#[test]
fn a_jump_to_itself_still_never_returns() {
    // jmp .; ret
    let r = analyze(vec![0xeb, 0xfe, 0xc3]);
    assert_eq!(count(&r.function, |k| matches!(k, NodeKind::Return)), 0);
}

/// A LOCK prefix makes `hlt` undecodable (#UD), so the bytes are reported.
#[test]
fn lock_hlt_does_not_decode() {
    // je 0x10003; ret; lock hlt; ret
    let r = analyze(vec![0x74, 0x01, 0xc3, 0xf0, 0xf4, 0xc3]);
    let undecodable: Vec<u64> = r
        .incomplete()
        .of(strider_orchestrator::Reason::UndecodableTarget)
        .map(|a| a.machine_addr.addr)
        .collect();
    assert_eq!(undecodable, vec![BASE + 3]);
}
