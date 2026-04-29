//! Per-node provenance fingerprints — the set of pcode instructions whose
//! lifting (directly or transitively, through value-flow) contributed to a
//! given IR node.
//!
//! The contract every value-producing node upholds is:
//!
//! > A node's fingerprint is the union of every pcode address that flowed
//! > into it, either as the address that lifted the node directly or as the
//! > fingerprint of any input.
//!
//! This is the foundation for "proof of work" pattern queries (a match's
//! fingerprint shows exactly which disassembled instructions justified the
//! match), surgical region splits in the IR cache (split a region's body at
//! pcode address K by partitioning nodes on whether their fingerprint contains
//! K-or-later), and provenance debugging.
//!
//! The implementation lives in [`crate::Graph`] as a side-table; this module
//! defines the data type the side-table holds.

use smallvec::SmallVec;

/// Fine-grained address identifying a single pcode instruction.
///
/// One native machine instruction can lift to several pcode instructions.
/// `PcodeInsnAddr` identifies each one by combining the machine-instruction
/// address with an index into the pcode sequence it produces.
///
/// This type is structurally identical to [`cfg::PcodeInsnAddr`] but lives
/// in the `ir` crate to avoid a dependency cycle (cfg already depends on ir).
/// Callers in `cfg`, `pcode-lift`, and `strider` build one of these from a
/// `cfg::PcodeInsnAddr` via [`PcodeInsnAddr::new`].
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

/// The set of pcode instructions that contributed to a node.
///
/// CONTRACT: every value-producing IR node has a fingerprint that is the
/// union of:
///   - the pcode instruction that DIRECTLY constructed it (lift time), AND
///   - the fingerprints of all input nodes whose values flowed into it.
///
/// Optimizer rewrites that fold N input nodes into one output node MUST union
/// the inputs' fingerprints into the output's fingerprint (handled centrally
/// in `rewrite_rule` and at every `Graph::create_node` call by auto-merging
/// from inputs).  This is what makes `Fingerprint` a sound "proof of work" —
/// every pcode instruction reachable through the value-flow chain to this
/// node is recorded.
///
/// Backed by `SmallVec<[PcodeInsnAddr; 4]>` so the common case (≤ 4 ancestor
/// instructions) stays inline without heap allocation.  Addresses are kept in
/// sorted order with duplicates removed; merging is O(n + m) on the existing
/// sorted runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fingerprint {
    /// Sorted, deduplicated pcode addresses.  Sorted-and-deduped so that
    /// merge is a tight linear-time set-union and equality is structural.
    addrs: SmallVec<[PcodeInsnAddr; 4]>,
}

impl Fingerprint {
    /// Constructs a fingerprint containing exactly one pcode address.
    #[must_use]
    pub fn from_single(addr: PcodeInsnAddr) -> Self {
        let mut addrs = SmallVec::new();
        addrs.push(addr);
        Self { addrs }
    }

    /// Returns the union of two fingerprints.
    ///
    /// Both inputs are sorted-and-deduped by construction; the merge walks
    /// them in lockstep and emits each unique address once.  Output is also
    /// sorted-and-deduped, preserving the invariant.
    #[must_use]
    pub fn merge(a: &Fingerprint, b: &Fingerprint) -> Self {
        let mut out: SmallVec<[PcodeInsnAddr; 4]> = SmallVec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.addrs.len() && j < b.addrs.len() {
            let av = a.addrs[i];
            let bv = b.addrs[j];
            match av.cmp(&bv) {
                std::cmp::Ordering::Less => {
                    out.push(av);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(bv);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    out.push(av);
                    i += 1;
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&a.addrs[i..]);
        out.extend_from_slice(&b.addrs[j..]);
        Self { addrs: out }
    }

    /// Returns the union of an arbitrary iterator of fingerprints.
    ///
    /// Implemented as a left-fold over `merge`; for N input fingerprints of
    /// total size T this is O(N · T) in the worst case but typically much
    /// less — most folds produce small fingerprints.
    pub fn merge_many<'a>(fps: impl IntoIterator<Item = &'a Fingerprint>) -> Self {
        let mut acc = Fingerprint::default();
        for fp in fps {
            acc = Fingerprint::merge(&acc, fp);
        }
        acc
    }

    /// Returns `true` if this fingerprint contains `addr`.
    #[must_use]
    pub fn contains(&self, addr: PcodeInsnAddr) -> bool {
        self.addrs.binary_search(&addr).is_ok()
    }

    /// Returns an iterator over the addresses in this fingerprint.
    ///
    /// The iterator yields each address exactly once, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = PcodeInsnAddr> + '_ {
        self.addrs.iter().copied()
    }

    /// Returns the number of unique addresses in this fingerprint.
    #[must_use]
    pub fn len(&self) -> usize {
        self.addrs.len()
    }

    /// Returns `true` if this fingerprint is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }
}


#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod fingerprint_tests;
