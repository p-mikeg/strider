//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`
//! (and the v1 spec `2026-05-05-callother-classification-design.md`
//! for the original cfg/ir consumer split).

/// Per-user-op ABI describing register and memory effects beyond
/// what Sleigh's pcode insn already encodes.  Sleigh emits
/// `CALLOTHER(user_op_id, args…)` with a possible `output` field;
/// the ABI fills in the *implicit* (ISA-fixed, not in pcode)
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserOpAbi {
    /// Register names this op reads beyond Sleigh's pcode-explicit
    /// `inputs[1..]`.  Resolved to `rsleigh::Vn` by the strider
    /// layer at lift time and appended to the CallOther's value
    /// inputs.  Use the exact Sleigh register name (case-sensitive).
    pub implicit_reads: &'static [&'static str],

    /// Register names this op writes (or scratch-clobbers) beyond
    /// Sleigh's pcode-explicit `output`.  Each becomes one extra
    /// clobber output slot on the CallOther node; the strider layer
    /// rebinds the matching tracked variable to that slot.
    pub implicit_writes: &'static [&'static str],

    /// Whether this op advances the IR's memory edge (token).  True
    /// for ops whose effect on memory is observable to subsequent
    /// loads / stores (syscall, port I/O, cache writeback).  False
    /// for pure register-level computation (cpuid, rdtsc, NEON math).
    pub memory_edge: bool,
}

/// What `strider::handle_call_other` does for a given user-op name.
/// Single source of truth for all CallOther dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOpClass {
    /// True no-op.  No IR node emitted; control / memory unchanged;
    /// pcode-explicit output (if any) is ignored.
    NoOp,

    /// Trap — control flow ends here.  cfg terminates the region as
    /// `RegionTerminator::NoReturn`; ir's `build_call_other_terminal`
    /// emits a `[ctrl, mem]` → `[ctrl, mem]` CallOther whose outputs
    /// dangle.
    NoReturn,

    /// Op with a precise ABI describing its register footprint and
    /// memory effect beyond what Sleigh's pcode already encodes.
    Call(UserOpAbi),
}

/// Look up a user-op name in the classification table.
///
/// Strict-on-emission policy: the ir layer (`build_call_other_modeled`'s
/// caller) converts `None` into `UnknownUserOpError`.  The cfg builder
/// treats `None` as "fall through to today's behaviour" (insn stays in
/// the region) — the ir layer is the single strict gate.
//
// `match_same_arms`: each name is a separate diffable entry — combining
// arms via `|` would defeat the table's per-line diff property.
#[allow(clippy::match_same_arms)]
#[must_use]
pub fn classify(name: &str) -> Option<UserOpClass> {
    // ASCII-sorted within each group for diffability.
    match name {
        // ─── NoOp ─────────────────────────────────────────────────
        "DC_CVAC" => Some(UserOpClass::NoOp),
        "DataMemoryBarrier" => Some(UserOpClass::NoOp),
        "DataSynchronizationBarrier" => Some(UserOpClass::NoOp),
        "Hint_Prefetch" => Some(UserOpClass::NoOp),
        "InstructionSynchronizationBarrier" => Some(UserOpClass::NoOp),
        "LOCK" => Some(UserOpClass::NoOp),
        "UNLOCK" => Some(UserOpClass::NoOp),
        "Yield" => Some(UserOpClass::NoOp),
        "setEndianState" => Some(UserOpClass::NoOp),
        "setISAMode" => Some(UserOpClass::NoOp),

        // ─── NoReturn ─────────────────────────────────────────────
        "SoftwareBreakpoint" => Some(UserOpClass::NoReturn),
        "UndefinedInstructionException" => Some(UserOpClass::NoReturn),
        "invalidInstructionException" => Some(UserOpClass::NoReturn),
        "sysret" => Some(UserOpClass::NoReturn),
        "trap" => Some(UserOpClass::NoReturn),

        // ─── Call (precise ABI) ───────────────────────────────────

        // Linux x86_64 syscall ABI.
        "syscall" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            memory_edge: true,
        })),

        // Linux ARM SWI ABI.
        "swi" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            memory_edge: true,
        })),

        // ARM SMCCC (X0..X7 in, X0..X3 out).
        "CallHyperVisor" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            implicit_writes: &["x0", "x1", "x2", "x3"],
            memory_edge: true,
        })),
        "CallSecureMonitor" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            implicit_writes: &["x0", "x1", "x2", "x3"],
            memory_edge: true,
        })),

        // x86 CPUID — Sleigh emits CALLOTHER(cpuid, EAX) with no output.
        "cpuid" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["ECX"],
            implicit_writes: &["EAX", "EBX", "ECX", "EDX"],
            memory_edge: false,
        })),

        // x86 RDTSC — no inputs, writes EDX:EAX.
        "rdtsc" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &["EAX", "EDX"],
            memory_edge: false,
        })),

        // x86 RDPKRU — ECX must be 0; writes EAX, clears EDX.
        "rdpkru_u32" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &["ECX"],
            implicit_writes: &["EAX", "EDX"],
            memory_edge: false,
        })),

        // x86 port I/O — port + value are pcode-explicit; memory edge captures
        // the external port-state effect.
        "in" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: true,
        })),
        "out" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: true,
        })),

        // x86 SWAPGS — touches the synthetic GS_base MSR; no general-reg
        // effect, no memory edge (kernel-mode register swap).
        "swapgs" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),

        // ARM exclusive-monitor primitives — synthetic monitor flag,
        // pcode-handled.  LDREX/STREX themselves emit pcode loads/stores.
        "ExclusiveMonitorPass" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),
        "ExclusiveMonitorsStatus" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),

        // ARM unmodelled sysreg read — pcode-explicit encoding constant
        // and destination.  Empty ABI per c1 (lift succeeds; opaque value).
        "UnkSytemRegRead" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),

        // NEON / SVE / multi-precision — Sleigh's pcode is fully sufficient.
        "MP_INT_ABS" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),
        "NEON_rev64" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),
        "NEON_sqshl" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),
        "NEON_uaddlv" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),
        "SVE_fnmla" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        })),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_abi() -> UserOpAbi {
        UserOpAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
        }
    }

    #[test]
    fn known_noop_classifies_as_noop() {
        for n in [
            "setEndianState",
            "setISAMode",
            "DataMemoryBarrier",
            "DataSynchronizationBarrier",
            "DC_CVAC",
            "Hint_Prefetch",
            "InstructionSynchronizationBarrier",
            "LOCK",
            "UNLOCK",
            "Yield",
        ] {
            assert_eq!(classify(n), Some(UserOpClass::NoOp), "{n}");
        }
    }

    #[test]
    fn known_trap_classifies_as_noreturn() {
        for n in [
            "invalidInstructionException",
            "SoftwareBreakpoint",
            "UndefinedInstructionException",
            "sysret",
            "trap",
        ] {
            assert_eq!(classify(n), Some(UserOpClass::NoReturn), "{n}");
        }
    }

    #[test]
    fn syscall_has_linux_x86_64_abi() {
        let class = classify("syscall").expect("syscall classified");
        let UserOpClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(
            abi.implicit_reads,
            &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"]
        );
        assert_eq!(abi.implicit_writes, &["RAX", "RCX", "R11"]);
        assert!(abi.memory_edge);
    }

    #[test]
    fn cpuid_has_implicit_writes_to_four_regs() {
        let class = classify("cpuid").expect("cpuid classified");
        let UserOpClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &["ECX"]);
        assert_eq!(abi.implicit_writes, &["EAX", "EBX", "ECX", "EDX"]);
        assert!(!abi.memory_edge);
    }

    #[test]
    fn rdtsc_writes_edx_eax_no_memory_edge() {
        let class = classify("rdtsc").expect("rdtsc classified");
        let UserOpClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &["EAX", "EDX"]);
        assert!(!abi.memory_edge);
    }

    #[test]
    fn empty_abi_ops_use_call_with_empty_abi() {
        for n in [
            "NEON_rev64",
            "NEON_sqshl",
            "NEON_uaddlv",
            "SVE_fnmla",
            "MP_INT_ABS",
            "UnkSytemRegRead",
            "swapgs",
            "ExclusiveMonitorPass",
            "ExclusiveMonitorsStatus",
        ] {
            let class = classify(n).unwrap_or_else(|| panic!("{n} classified"));
            let UserOpClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(abi, empty_abi(), "{n}");
        }
    }

    #[test]
    fn smccc_ops_share_x0_x7_in_x0_x3_out() {
        for n in ["CallHyperVisor", "CallSecureMonitor"] {
            let class = classify(n).expect(n);
            let UserOpClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(
                abi.implicit_reads,
                &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
                "{n}"
            );
            assert_eq!(abi.implicit_writes, &["x0", "x1", "x2", "x3"], "{n}");
            assert!(abi.memory_edge, "{n}");
        }
    }

    #[test]
    fn port_io_has_memory_edge_no_implicit_regs() {
        for n in ["in", "out"] {
            let class = classify(n).expect(n);
            let UserOpClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(abi.implicit_reads, &[] as &[&str], "{n}");
            assert_eq!(abi.implicit_writes, &[] as &[&str], "{n}");
            assert!(abi.memory_edge, "{n}");
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify("nonexistent_op_xyzzy_abc"), None);
    }

    #[test]
    fn opaque_variant_does_not_exist() {
        // Compile-time guard: every variant of UserOpClass is matched
        // exhaustively here, so adding/removing a variant fails compile.
        for n in ["setISAMode", "invalidInstructionException", "cpuid"] {
            let class = classify(n).unwrap();
            match class {
                UserOpClass::NoOp | UserOpClass::NoReturn | UserOpClass::Call(_) => {}
            }
        }
    }
}
