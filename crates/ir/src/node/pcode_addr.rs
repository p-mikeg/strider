//! Fine-grained address identifying a single pcode instruction.
//!
//! One native machine instruction can lift to several pcode instructions.
//! [`PcodeInsnAddr`] identifies each one by combining the machine-instruction
//! address with an index into the pcode sequence it produces.
//!
//! This type is structurally identical to [`cfg::PcodeInsnAddr`] but lives
//! in the `ir` crate to avoid a dependency cycle (cfg already depends on ir).
//! Callers in `cfg`, `pcode-lift`, and `strider` build one of these from a
//! `cfg::PcodeInsnAddr` via [`PcodeInsnAddr::new`].

/// Fine-grained address identifying a single pcode instruction.
///
/// One native machine instruction can lift to several pcode instructions.
/// `PcodeInsnAddr` identifies each one by combining the machine-instruction
/// address with an index into the pcode sequence it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PcodeInsnAddr {
    /// Virtual address of the enclosing machine instruction.
    pub machine_addr: u64,
    /// Zero-based index of this pcode instruction within the machine
    /// instruction.
    pub insn_index: u64,
}

impl PcodeInsnAddr {
    /// Constructs a `PcodeInsnAddr` from raw `(machine_addr, insn_index)`
    /// coordinates.
    #[must_use]
    pub const fn new(machine_addr: u64, insn_index: u64) -> Self {
        Self {
            machine_addr,
            insn_index,
        }
    }
}
