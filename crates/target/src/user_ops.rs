//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-05-callother-classification-design.md`.

/// What `ir::FunctionBuilder::build_call_other` does for a given
/// user-op name.  Also consulted by `cfg::region_builder` to know
/// whether to terminate a region on a `NoReturn` CallOther.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOpClass {
    /// True no-op in the IR's data-flow / control-flow / memory model.
    /// `build_call_other` skips the node entirely; control and memory
    /// pass through unchanged; the pcode insn's output varnode (if
    /// any) is ignored.  Cfg behaviour: same as today (insn stays in
    /// the region; loop continues).
    NoOp,

    /// Trap instruction whose semantic effect is "execution does not
    /// continue past this point" (Linux `BUG()` / `BUG_ON()` /
    /// `WARN()`-class).  `build_call_other` emits a CallOther node
    /// (with control + memory inputs only — no clobber outputs, no
    /// value output, dangling control + memory outputs).  Cfg
    /// behaviour: terminates the region as `RegionTerminator::NoReturn`
    /// before processing the trailing `BranchIndirect`.
    NoReturn,

    /// Known opaque user-op (cpuid, syscall, lock-prefix, …).
    /// `build_call_other` emits today's CallOther shape with the
    /// conservative "every tracked variable except SP" clobber set.
    /// Cfg behaviour: same as today.
    Opaque,
}

/// Look up a user-op name in the classification table.  Returns
/// `None` for unknown names.
///
/// Strict-on-emission policy: the IR builder converts `None` into
/// `UnknownUserOpError` (hard fail).  The cfg builder treats `None`
/// as "fall through to today's behaviour" (insn stays in the region;
/// loop continues) — the IR layer is the single strict gate.
///
/// Names are expected to be unambiguous across the arches we
/// currently support.  If a future name collides between arches with
/// different semantics, promote the API to
/// `(arch, name) -> UserOpClass`.
#[must_use]
pub fn classify(name: &str) -> Option<UserOpClass> {
    // ASCII-sorted within each group for diffability.
    match name {
        // ── No-ops (decoder context bits; no IR-visible effect) ──
        "setEndianState" => Some(UserOpClass::NoOp),
        "setISAMode" => Some(UserOpClass::NoOp),

        // ── Noreturn traps (Linux BUG_ON / WARN-class) ──
        "SoftwareBreakpoint" => Some(UserOpClass::NoReturn), // aarch64 brk #imm
        "UndefinedInstructionException" => Some(UserOpClass::NoReturn), // ARM 32-bit UDF
        "invalidInstructionException" => Some(UserOpClass::NoReturn), // x86/x86_64 ud2

        // ── Opaque (test-required + initial real-world set) ──
        "CallHyperVisor" => Some(UserOpClass::Opaque),
        "CallSecureMonitor" => Some(UserOpClass::Opaque),
        "DC_CVAC" => Some(UserOpClass::Opaque),
        "DataMemoryBarrier" => Some(UserOpClass::Opaque),
        "DataSynchronizationBarrier" => Some(UserOpClass::Opaque),
        "ExclusiveMonitorPass" => Some(UserOpClass::Opaque),
        "ExclusiveMonitorsStatus" => Some(UserOpClass::Opaque),
        "Hint_Prefetch" => Some(UserOpClass::Opaque),
        "InstructionSynchronizationBarrier" => Some(UserOpClass::Opaque),
        "LOCK" => Some(UserOpClass::Opaque),
        "MP_INT_ABS" => Some(UserOpClass::Opaque),
        "NEON_rev64" => Some(UserOpClass::Opaque),
        "NEON_sqshl" => Some(UserOpClass::Opaque),
        "NEON_uaddlv" => Some(UserOpClass::Opaque),
        "SVE_fnmla" => Some(UserOpClass::Opaque),
        "UNLOCK" => Some(UserOpClass::Opaque),
        "UnkSytemRegRead" => Some(UserOpClass::Opaque),
        "Yield" => Some(UserOpClass::Opaque),
        "cpuid" => Some(UserOpClass::Opaque),
        "in" => Some(UserOpClass::Opaque),
        "out" => Some(UserOpClass::Opaque),
        "rdpkru_u32" => Some(UserOpClass::Opaque),
        "rdtsc" => Some(UserOpClass::Opaque),
        "swapgs" => Some(UserOpClass::Opaque),
        "swi" => Some(UserOpClass::Opaque),
        "syscall" => Some(UserOpClass::Opaque),
        "sysret" => Some(UserOpClass::Opaque),
        "trap" => Some(UserOpClass::Opaque),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_noop_classifies_as_noop() {
        assert_eq!(classify("setISAMode"), Some(UserOpClass::NoOp));
        assert_eq!(classify("setEndianState"), Some(UserOpClass::NoOp));
    }

    #[test]
    fn known_trap_classifies_as_noreturn() {
        assert_eq!(
            classify("invalidInstructionException"),
            Some(UserOpClass::NoReturn),
        );
        assert_eq!(
            classify("SoftwareBreakpoint"),
            Some(UserOpClass::NoReturn),
        );
        assert_eq!(
            classify("UndefinedInstructionException"),
            Some(UserOpClass::NoReturn),
        );
    }

    #[test]
    fn known_opaque_classifies_as_opaque() {
        assert_eq!(classify("cpuid"), Some(UserOpClass::Opaque));
        assert_eq!(classify("syscall"), Some(UserOpClass::Opaque));
        assert_eq!(classify("rdtsc"), Some(UserOpClass::Opaque));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify("nonexistent_op_xyzzy_abc"), None);
    }
}
