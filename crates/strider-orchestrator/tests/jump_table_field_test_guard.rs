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
/// `jmp *0x2000(,%rax,8)`.
const JMP_RAX: [u8; 7] = [0xff, 0x24, 0xc5, 0x00, 0x20, 0x00, 0x00];
/// Everything after the dispatch.
const TAIL: &[u8] = &[
    0xb8, 0x0a, 0x00, 0x00, 0x00, 0xc3, // a0
    0xb8, 0x0b, 0x00, 0x00, 0x00, 0xc3, // a1
    0xb8, 0x0c, 0x00, 0x00, 0x00, 0xc3, // a2
    0xb8, 0x0d, 0x00, 0x00, 0x00, 0xc3, // a3
    0x31, 0xc0, 0xc3, // dflt
    0xb8, 0x14, 0x00, 0x00, 0x00, 0xc3, // o0
    0xb8, 0x15, 0x00, 0x00, 0x00, 0xc3, // o1
];

/// What the `jmp *0x2000(,%rax,8)` after `guard` became: its arms, or `None`
/// when it is reported unresolved.
fn dispatch(guard: &[u8]) -> Option<Vec<u64>> {
    dispatch_through([guard, JMP_RAX.as_slice()].concat())
}

/// [`dispatch`], for `head` ending in its own seven-byte dispatch.
fn dispatch_through(head: Vec<u8>) -> Option<Vec<u64>> {
    let at = |off: usize| BASE + (head.len() - 7 + off) as u64;
    let site = at(0);
    let real = [at(7), at(13), at(19), at(25)];
    let other = [at(34), at(40)];
    let cells: Vec<u64> = (0..256)
        .map(|i| if i < 4 { real[i] } else { other[i % 2] })
        .collect();
    let code = [head.as_slice(), TAIL].concat();
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

/// `lea (%rax,%rax,2),%ecx ; cmp $9,%ecx ; jae dflt`: a guard the range
/// analysis records, but on a value the index is not built from.
/// Mutation: exempt every recorded guard on a value of the field.
#[test]
fn a_read_guard_off_the_index_chain_defers_the_dispatch() {
    let test = [0x8d, 0x0c, 0x40, 0x83, 0xf9, 0x09, 0x73, 0x1f];
    assert_eq!(dispatch(&guarded(&test)), None);
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

/// The arms an ARM dispatch at 0x1000 seats: `words`, then a four-word
/// table at `table`, then five `bx lr`, the first four the arms.
fn arm_dispatch(mut words: Vec<u32>, table: u64, data: std::ops::Range<u64>) -> Option<Vec<u64>> {
    let first_arm = table + 16;
    let arms: Vec<u64> = (0..4).map(|i| first_arm + 4 * i).collect();
    words.extend(arms.iter().map(|&a| u32::try_from(a).expect("fits")));
    words.extend([0xe12f_ff1e; 5]);
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let len = code.len() as u64;
    let rom = Box::new(MockRom::strided(table, 4, arms, 4))
        as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::Arm, code, BASE, Some(rom));
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(len),
            data_ranges: strider_cfg::DataRanges::new(Some(data)),
            ..CfgOptions::default()
        },
        ..Default::default()
    };
    let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
    result.cfg.regions().find_map(|r| match &r.terminator {
        RegionTerminator::Switch { targets, .. } => {
            let mut got: Vec<u64> = targets.iter().map(|t| t.addr).collect();
            got.sort_unstable();
            Some(got)
        }
        _ => None,
    })
}

/// `ldr r1,[r1,#0x30] ; tst r1,#<bit> ; bne default ; lsr r2,r1,#30 ;
/// ldr pc,[pc,r2,lsl #2] ; nop`, then the table at 0x1018. A test of bit 29
/// cannot bound `r1 >> 30`; a test of bit 31 can.
/// Mutation: count a test of any bit of the field.
#[test]
fn a_test_of_bits_the_index_does_not_read_bounds_nothing() {
    let run = |tst: u32| {
        let words = vec![
            0xe591_1030,
            tst,
            0x1a00_000a,
            0xe1a0_2f21,
            0xe79f_f102,
            0xe320_f000,
        ];
        arm_dispatch(words, 0x1018, 0x1018..0x1028)
    };
    assert_eq!(
        run(0xe311_0202).as_deref(),
        Some([0x1028, 0x102c, 0x1030, 0x1034].as_slice()),
        "tst r1, #0x20000000",
    );
    assert_eq!(run(0xe311_0102), None, "tst r1, #0x80000000");
}

/// Linux 3.16 arm `isp1760_urb_enqueue`'s dispatch (its real instructions at
/// 0x1000, the table's four words after the `b`):
///
/// ```text
/// ldr   r1, [r1, #0x30]
/// lsr   r2, r1, #30
/// cmp   r2, #3
/// ldrls pc, [pc, r2, lsl #2]
/// b     default
/// ```
///
/// The dispatch's own bounds check tests the field and is read, so it is no
/// unread test, though it narrows nothing the shift had not.
/// Mutation: count a read guard on the field as an unread test.
#[test]
fn a_read_bounds_check_no_tighter_than_the_field_still_seats_its_arms() {
    let words = vec![
        0xe591_1030,
        0xe1a0_2f21,
        0xe352_0003,
        0x979f_f102,
        0xea00_0007,
    ];
    assert_eq!(
        arm_dispatch(words, 0x1014, 0x1014..0x1024).as_deref(),
        Some([0x1024, 0x1028, 0x102c, 0x1030].as_slice()),
    );
}

/// The labels of `head`'s layout: `[a0, a1, a2, a3, o0, o1]`.
fn labels(head_len: usize) -> [u64; 6] {
    let at = |off: usize| BASE + (head_len + off) as u64;
    [at(0), at(6), at(12), at(18), at(27), at(33)]
}

/// Asserts the dispatch after `head` is deferred, or seats only the labels
/// at `feasible` (indices into [`labels`]).
fn assert_only(head: Vec<u8>, feasible: &[usize]) {
    let names = labels(head.len());
    let got = dispatch_through(head);
    if let Some(arms) = got {
        let mut want: Vec<u64> = feasible.iter().map(|&i| names[i]).collect();
        want.sort_unstable();
        assert_eq!(arms, want, "an infeasible arm was seated");
    }
}

/// `cmp $2,%al ; jne dflt`: the dispatch is on the equality's true edge, so
/// only cell 2 is feasible.
/// Mutation: exempt an equality whichever edge reaches the dispatch.
#[test]
fn an_equality_the_dispatch_sits_under_bounds_the_field() {
    assert_only(
        [guarded(&[0x3c, 0x02, 0x75, 0x1f]), JMP_RAX.to_vec()].concat(),
        &[2],
    );
}

/// `movzwl 0x10(%rdi),%eax ; testb $0xfc,0x11(%rdi) ; jne dflt ; shr $8,%eax`:
/// the test loads the index's byte at its own address.
/// Mutation: count only a load at the cell's own address.
#[test]
fn a_test_of_an_overlapping_load_bounds_the_field() {
    let head = [
        0x0f, 0xb7, 0x47, 0x10, 0xf6, 0x47, 0x11, 0xfc, 0x75, 0x22, 0xc1, 0xe8, 0x08,
    ];
    assert_only([head.as_slice(), &JMP_RAX].concat(), &[0, 1, 2, 3]);
}
