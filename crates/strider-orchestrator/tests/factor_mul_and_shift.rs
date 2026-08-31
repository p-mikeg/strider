//! Factoring `x` out of a sum reaches every mul/shift pairing, not just the
//! ones written with two multiplies.
//!
//! A shift by C is a multiply by 2^C, so a compiler is free to emit `x*3 + x*4`
//! as one `imul` and one `shl`. Pairing mul with mul and `x` with shift while
//! leaving mul-with-shift and shift-with-shift alone left those sums unfactored.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;

/// The coefficient of the single surviving `Mul`, or `None` if the sum did not
/// collapse to one.
fn folded_coefficient(bytes: Vec<u8>) -> Option<u128> {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, BASE),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let r = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let f = &r.function;
    let muls: Vec<_> = f
        .graph()
        .all_node_ids()
        .filter(|&n| {
            matches!(
                f.node_kind(n),
                strider_ir::node::NodeKind::IntBinaryOp(strider_ir::node::IntBinaryOp::Mul)
            )
        })
        .collect();
    let [mul] = muls[..] else { return None };
    f.node_inputs(mul).iter().find_map(|v| f.int_const_u128(v))
}

/// Each body computes a multiple of `rdi` and clears the other return
/// registers, so the only surviving arithmetic is the sum under test.
#[test]
fn a_sum_of_scaled_copies_folds_to_one_multiply() {
    // Assembled with `as --64`; the scratch is %rcx, which SysV does not return.
    let cases: [(&str, &[u8], u128); 6] = [
        // imul $3,%rdi,%rax ; imul $5,%rdi,%rcx ; add %rcx,%rax
        (
            "x*3 + x*5",
            &[
                0x48, 0x6b, 0xc7, 0x03, 0x48, 0x6b, 0xcf, 0x05, 0x48, 0x01, 0xc8, 0x31, 0xc9, 0x31,
                0xd2, 0xc3,
            ],
            8,
        ),
        // imul $3,%rdi,%rax ; mov %rdi,%rcx ; shl $2,%rcx ; add %rcx,%rax
        (
            "x*3 + (x<<2)",
            &[
                0x48, 0x6b, 0xc7, 0x03, 0x48, 0x89, 0xf9, 0x48, 0xc1, 0xe1, 0x02, 0x48, 0x01, 0xc8,
                0x31, 0xc9, 0x31, 0xd2, 0xc3,
            ],
            7,
        ),
        // mov %rdi,%rax ; shl $2,%rax ; mov %rdi,%rcx ; shl $3,%rcx ; add %rcx,%rax
        (
            "(x<<2) + (x<<3)",
            &[
                0x48, 0x89, 0xf8, 0x48, 0xc1, 0xe0, 0x02, 0x48, 0x89, 0xf9, 0x48, 0xc1, 0xe1, 0x03,
                0x48, 0x01, 0xc8, 0x31, 0xc9, 0x31, 0xd2, 0xc3,
            ],
            12,
        ),
        // imul $9,%rdi,%rax ; mov %rdi,%rcx ; shl $1,%rcx ; sub %rcx,%rax
        (
            "x*9 - (x<<1)",
            &[
                0x48, 0x6b, 0xc7, 0x09, 0x48, 0x89, 0xf9, 0x48, 0xd1, 0xe1, 0x48, 0x29, 0xc8, 0x31,
                0xc9, 0x31, 0xd2, 0xc3,
            ],
            7,
        ),
        // imul $7,%rdi,%rax ; imul $3,%rdi,%rcx ; sub %rcx,%rax
        (
            "x*7 - x*3",
            &[
                0x48, 0x6b, 0xc7, 0x07, 0x48, 0x6b, 0xcf, 0x03, 0x48, 0x29, 0xc8, 0x31, 0xc9, 0x31,
                0xd2, 0xc3,
            ],
            4,
        ),
        // mov %rdi,%rax ; mov %rdi,%rcx ; shl $2,%rcx ; sub %rcx,%rax   (1 - 4 = -3)
        (
            "x - (x<<2)",
            &[
                0x48, 0x89, 0xf8, 0x48, 0x89, 0xf9, 0x48, 0xc1, 0xe1, 0x02, 0x48, 0x29, 0xc8, 0x31,
                0xc9, 0x31, 0xd2, 0xc3,
            ],
            u128::from(u64::MAX - 2),
        ),
    ];
    for (name, bytes, want) in cases {
        assert_eq!(
            folded_coefficient(bytes.to_vec()),
            Some(want),
            "{name} must fold to a single multiply by {want}",
        );
    }
}
