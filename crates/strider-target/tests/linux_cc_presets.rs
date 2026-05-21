//! Tests for the Linux kernel + syscall calling-convention presets
//! added in `docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md`.
//!
//! For each preset, the test:
//!   1. Constructs the preset.
//!   2. Builds it against the matching arch's Sleigh register table.
//!   3. Asserts the resolved arg-passing varnodes round-trip back to the
//!      documented register names, and that `link_register_vn` is `None`
//!      for syscall presets (kernel-mode return is via `sysret` / `eret`).
//!
//! The arg-name-vs-literal-list checks (kernel-internal x86 + the five
//! syscall presets) are collapsed into one table-driven test so a typo
//! in any one of them surfaces in the aggregated failure report.  The
//! kernel-aliases-another-CC tests and the few preset-specific
//! invariants (e.g. x86_64 syscall replaces RCX with R10) stay
//! standalone — their shape doesn't fit the literal-list table.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_target::{CallingConvention, SleighArch};

/// Probe the arch's Sleigh against an empty memory reader to extract
/// the register table.  No real binary is needed — the register table
/// is fixed by the `.sla` spec.
fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

/// Returns the resolved register names for the preset's
/// `arg_passing_regs`, in positional order.  We resolve back through
/// `vn_to_name` so a register that was supplied under a different
/// alias (e.g. `lr` for AArch64's `x30`) shows up under the
/// canonical Sleigh name and the assertion is exact.
fn arg_reg_names(cc: CallingConvention, regs: &rsleigh::SleighRegs) -> Vec<String> {
    cc.build(regs)
        .expect("build")
        .arg_passing_regs
        .iter()
        .map(|vn| {
            regs.vn_to_name(*vn)
                .expect("every resolved arg vn must round-trip to a name")
                .to_string()
        })
        .collect()
}

// ── Table-driven arg-name vs literal-list checks ─────────────────────────────

/// One row of the literal-list table: a labelled preset constructor, the
/// arch its register table comes from, the expected canonical arg-reg
/// names in positional order, and whether the preset is a syscall ABI
/// (syscall presets additionally require `link_register_vn.is_none()`
/// because kernel-mode return is via `sysret` / `eret`, not a link
/// register).
struct Case {
    name: &'static str,
    ctor: fn() -> CallingConvention,
    arch: fn() -> SleighArch,
    expected: &'static [&'static str],
    is_syscall: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "x86_linux_kernel",
        ctor: CallingConvention::x86_linux_kernel,
        arch: SleighArch::x86,
        expected: &["EAX", "EDX", "ECX"],
        is_syscall: false,
    },
    Case {
        name: "x86_linux_syscall",
        ctor: CallingConvention::x86_linux_syscall,
        arch: SleighArch::x86,
        expected: &["EBX", "ECX", "EDX", "ESI", "EDI", "EBP"],
        is_syscall: true,
    },
    Case {
        name: "x86_64_linux_syscall",
        ctor: CallingConvention::x86_64_linux_syscall,
        arch: SleighArch::x86_64,
        expected: &["RDI", "RSI", "RDX", "R10", "R8", "R9"],
        is_syscall: true,
    },
    Case {
        name: "aarch64_linux_syscall",
        ctor: CallingConvention::aarch64_linux_syscall,
        arch: SleighArch::aarch64,
        expected: &["x0", "x1", "x2", "x3", "x4", "x5"],
        is_syscall: true,
    },
    Case {
        name: "arm_linux_syscall",
        ctor: CallingConvention::arm_linux_syscall,
        arch: SleighArch::arm,
        expected: &["r0", "r1", "r2", "r3", "r4", "r5", "r6"],
        is_syscall: true,
    },
    Case {
        name: "mips_linux_syscall_o32",
        ctor: CallingConvention::mips_linux_syscall_o32,
        arch: SleighArch::mipsle32,
        expected: &["a0", "a1", "a2", "a3"],
        is_syscall: true,
    },
];

#[test]
fn arg_passing_regs_match_per_preset() {
    let mut failures = Vec::new();
    for case in CASES {
        let regs = regs_for((case.arch)());
        let cc = (case.ctor)();
        let names = arg_reg_names(cc, &regs);
        let expected: Vec<String> = case.expected.iter().map(|s| s.to_string()).collect();
        if names != expected {
            failures.push(format!(
                "{}: arg_passing_regs got {:?}, expected {:?}",
                case.name, names, expected
            ));
        }
        if case.is_syscall {
            let built = cc.build(&regs).expect("build");
            if built.link_register_vn.is_some() {
                failures.push(format!(
                    "{}: syscall preset must have link_register_vn == None",
                    case.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

// ── Kernel-internal CCs that alias another CC ────────────────────────────────
// These probe a CC-to-CC equivalence rather than a literal name list,
// so their shape doesn't fit the table above.

#[test]
fn x86_64_linux_kernel_aliases_systemv() {
    let regs = regs_for(SleighArch::x86_64());
    assert_eq!(
        arg_reg_names(CallingConvention::x86_64_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::x86_64_systemv(), &regs),
        "x86_64 kernel-internal CC is identical to SystemV"
    );
}

#[test]
fn aarch64_linux_kernel_aliases_aapcs64() {
    let regs = regs_for(SleighArch::aarch64());
    assert_eq!(
        arg_reg_names(CallingConvention::aarch64_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::aarch64_aapcs64(), &regs),
    );
}

#[test]
fn arm_linux_kernel_aliases_aapcs() {
    let regs = regs_for(SleighArch::arm());
    assert_eq!(
        arg_reg_names(CallingConvention::arm_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::arm_aapcs(), &regs),
    );
}

#[test]
fn mips_linux_kernel_o32_aliases_o32() {
    let regs = regs_for(SleighArch::mipsle32());
    assert_eq!(
        arg_reg_names(CallingConvention::mips_linux_kernel_o32(), &regs),
        arg_reg_names(CallingConvention::mips_o32(), &regs),
    );
}

#[test]
fn mips_linux_kernel_n64_aliases_n64() {
    let regs = regs_for(SleighArch::mipsle64());
    assert_eq!(
        arg_reg_names(CallingConvention::mips_linux_kernel_n64(), &regs),
        arg_reg_names(CallingConvention::mips_n64(), &regs),
    );
}

// ── Preset-specific invariants ───────────────────────────────────────────────
// Extra assertions that don't fit the per-preset arg-name table.

#[test]
fn x86_linux_syscall_ret_stack_pop_is_zero() {
    let regs = regs_for(SleighArch::x86());
    let built = CallingConvention::x86_linux_syscall().build(&regs).unwrap();
    assert_eq!(built.ret_stack_pop, 0);
}

#[test]
fn x86_64_linux_syscall_uses_r10_not_rcx() {
    let regs = regs_for(SleighArch::x86_64());
    let names = arg_reg_names(CallingConvention::x86_64_linux_syscall(), &regs);
    assert!(
        !names.iter().any(|n| n == "RCX"),
        "syscall ABI must replace RCX with R10 (RCX is clobbered by `syscall`)"
    );
}

#[test]
fn mips_linux_syscall_n64_extends_args_to_six() {
    let regs = regs_for(SleighArch::mipsle64());
    let names = arg_reg_names(CallingConvention::mips_linux_syscall_n64(), &regs);
    // N64 syscall extends to 6 regs (a0..a5) — Sleigh's mips64 spec
    // names $4..$7 as a0..a3 and $8..$9 as t0..t1, so the assertion
    // checks that 6-arg shape under those names.
    assert_eq!(names.len(), 6);
    assert_eq!(&names[..4], &["a0", "a1", "a2", "a3"]);
}
