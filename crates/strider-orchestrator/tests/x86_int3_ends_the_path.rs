//! `int3` and `int1` trap to their handler and resume at the next
//! instruction, as `int 3` (`cd 03`) does: the handler is a call through the
//! vector `swi` names, and control falls through after it.

mod common;

use common::Arch;
use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker};

const BASE: u64 = 0x10000;

fn analyze(arch: Arch, bytes: Vec<u8>) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
}

fn count(f: &Function, pred: impl Fn(&NodeKind) -> bool) -> usize {
    f.walk().filter(|&n| pred(f.node_kind(n))).count()
}

#[test]
fn a_breakpoint_returns_to_the_next_instruction() {
    for arch in [Arch::X64, Arch::X86] {
        for (name, trap) in [
            ("int3", &[0xcc][..]),
            ("int1", &[0xf1]),
            ("int 3", &[0xcd, 0x03]),
        ] {
            // mov eax, 7; <trap>; ret
            let mut bytes = vec![0xb8, 7, 0, 0, 0];
            bytes.extend_from_slice(trap);
            bytes.push(0xc3);
            let r = analyze(arch, bytes);
            assert!(
                r.cfg.incomplete().is_empty(),
                "{arch:?} {name}: {:?}",
                r.cfg.incomplete()
            );
            let f = &r.function;
            assert_eq!(
                count(f, |k| matches!(k, NodeKind::Return)),
                1,
                "{arch:?} {name}"
            );
            assert_eq!(
                count(f, |k| matches!(k, NodeKind::Call { .. })),
                1,
                "{arch:?} {name}"
            );
        }
    }
}
