//! A byte read through a pointer indexes a table at its full width only when
//! nothing that dominates the dispatch tests it in a shape the range analysis
//! cannot read. A mask test, a shift, a sign test, or a guard on a reload of
//! the same field may be what sizes the table, and enumerating the whole byte
//! then seats the cells past the table's end.
//!
//! Every case is x86-64 at 0x1000, `jmp *0x2000(,%rax,8)`, then the same
//! tail: four real arms, a `dflt`, and two other labels `o0`, `o1` that fill
//! the 252 cells after the table's four, as the next table in `.rodata`
//! would:
//!
//! ```text
//! a0: mov $10,%eax ; ret
//! a1: mov $11,%eax ; ret
//! a2: mov $12,%eax ; ret
//! a3: mov $13,%eax ; ret
//! dflt: xor %eax,%eax ; ret
//! o0: mov $20,%eax ; ret
//! o1: mov $21,%eax ; ret
//! ```

use strider_cfg::{CfgOptions, FunctionBound, RegionTerminator};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

mod common;

const BASE: u64 = 0x1000;
const TABLE: u64 = 0x2000;
/// `jmp *0x2000(,%rax,8)` and everything after it.
const DISPATCH_AND_TAIL: &[u8] = &[
    0xff, 0x24, 0xc5, 0x00, 0x20, 0x00, 0x00, // jmp *0x2000(,%rax,8)
    0xb8, 0x0a, 0x00, 0x00, 0x00, 0xc3, // a0
    0xb8, 0x0b, 0x00, 0x00, 0x00, 0xc3, // a1
    0xb8, 0x0c, 0x00, 0x00, 0x00, 0xc3, // a2
    0xb8, 0x0d, 0x00, 0x00, 0x00, 0xc3, // a3
    0x31, 0xc0, 0xc3, // dflt
    0xb8, 0x14, 0x00, 0x00, 0x00, 0xc3, // o0
    0xb8, 0x15, 0x00, 0x00, 0x00, 0xc3, // o1
];

/// What the dispatch after `guard` became: its arms, or `None` when it is
/// reported unresolved.
fn dispatch(guard: &[u8]) -> Option<Vec<u64>> {
    let at = |off: usize| BASE + (guard.len() + off) as u64;
    let site = at(0);
    let real = [at(7), at(13), at(19), at(25)];
    let other = [at(34), at(40)];
    let cells: Vec<u64> = (0..256)
        .map(|i| if i < 4 { real[i] } else { other[i % 2] })
        .collect();
    let code = [guard, DISPATCH_AND_TAIL].concat();
    let len = code.len() as u64;
    let rom = Box::new(MockRom::strided(TABLE, 8, cells, 8))
        as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, code, BASE, Some(rom));
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(len),
            ..CfgOptions::default()
        },
        ..Default::default()
    };
    let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
    let unresolved = result
        .incomplete()
        .of(Reason::UnresolvedBranch)
        .any(|a| a.machine_addr.addr == site);
    let arms = result.cfg.regions().find_map(|r| match &r.terminator {
        RegionTerminator::Switch { targets, addr, .. } if addr.machine_addr.addr == site => {
            let mut arms: Vec<u64> = targets.iter().map(|t| t.addr).collect();
            arms.sort_unstable();
            Some(arms)
        }
        _ => None,
    });
    assert!(
        unresolved != arms.is_some(),
        "seated and reported at once: {arms:?}"
    );
    arms
}

const MOVZBL_FIELD: [u8; 4] = [0x0f, 0xb6, 0x47, 0x10]; // movzbl 0x10(%rdi),%eax

fn guarded(test: &[u8]) -> Vec<u8> {
    [MOVZBL_FIELD.as_slice(), test].concat()
}

/// `testb $0xfc,%al ; jne dflt`.
/// Mutation: answer a field's width as its range whatever tests it.
#[test]
fn a_mask_test_on_a_field_defers_the_dispatch() {
    assert_eq!(dispatch(&guarded(&[0xa8, 0xfc, 0x75, 0x1f])), None);
}

/// `mov %eax,%ecx ; shr $2,%ecx ; jne dflt`.
#[test]
fn a_shift_test_on_a_field_defers_the_dispatch() {
    assert_eq!(
        dispatch(&guarded(&[0x89, 0xc1, 0xc1, 0xe9, 0x02, 0x75, 0x1f])),
        None
    );
}

/// `mov %eax,%ecx ; and $0xfc,%ecx ; cmp $0,%ecx ; jne dflt`.
#[test]
fn a_masked_compare_on_a_field_defers_the_dispatch() {
    let test = [
        0x89, 0xc1, 0x81, 0xe1, 0xfc, 0x00, 0x00, 0x00, 0x83, 0xf9, 0x00, 0x75, 0x1f,
    ];
    assert_eq!(dispatch(&guarded(&test)), None);
}

/// `testb %al,%al ; js dflt`.
#[test]
fn a_sign_test_on_a_field_defers_the_dispatch() {
    assert_eq!(dispatch(&guarded(&[0x84, 0xc0, 0x78, 0x1f])), None);
}

/// `cmpb $3,0x10(%rdi) ; ja dflt ; push %rbx ; movzbl 0x10(%rdi),%eax`: the
/// push may alias the field, so the guard does not bound the reload.
/// Mutation: count only a test of the index load itself.
#[test]
fn a_guard_on_an_unmerged_reload_of_the_field_defers_the_dispatch() {
    let guard = [
        0x80, 0x7f, 0x10, 0x03, 0x77, 0x24, 0x53, 0x0f, 0xb6, 0x47, 0x10,
    ];
    assert_eq!(dispatch(&guard), None);
}

/// `cmpb $3,%al ; ja dflt`, which the range analysis reads.
#[test]
fn a_read_guard_on_a_field_seats_the_four_arms() {
    let arms = dispatch(&guarded(&[0x3c, 0x03, 0x77, 0x1f])).expect("seated");
    assert_eq!(arms, [0x100f, 0x1015, 0x101b, 0x1021]);
}

/// `cmpb $7,%al ; je dflt` excludes one value, which `ruled_out` reads, and
/// bounds nothing; with no test at all the byte is unbounded too. Either way
/// every cell is machine-feasible.
/// Mutation: drop the `Equal` exemption.
#[test]
fn an_equality_test_or_no_test_leaves_the_whole_byte() {
    for guard in [guarded(&[0x3c, 0x07, 0x74, 0x1f]), MOVZBL_FIELD.to_vec()] {
        let arms = dispatch(&guard).expect("seated");
        assert_eq!(arms.len(), 6, "{arms:x?}");
    }
}
