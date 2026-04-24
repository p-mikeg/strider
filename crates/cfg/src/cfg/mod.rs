mod builder;
mod dot;
mod options;
mod query;
mod types;

pub use builder::Builder;
pub use builder::test_api;
pub use options::OptionsBuilder;

#[doc(hidden)]
pub use dot::test_api as dot_test_api;

#[doc(hidden)]
pub use builder::region_builder_test_api;
pub use query::IfRegionState;
pub use types::{PcodeInsnAddr, Region, RegionEdgeKind};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed Control Flow Graph for a single function.
///
/// Produced by [`Builder::build`].  The graph is a [`petgraph::stable_graph::StableDiGraph`]
/// where each node is a [`Region`] (basic block) and each edge is a
/// [`RegionEdgeKind`] (the type of control transfer).
#[derive(Debug)]
pub struct Cfg<R: rsleigh::MemReader> {
    /// The Sleigh context used during construction.  Retained so that
    /// register names can be resolved for visualisation.
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;

#[cfg(test)]
mod tests {
    use super::*;
    use super::types::{MachineInsnAddr, RegionInstruction};

    // ── MachineInsnAddr ───────────────────────────────────────────────────────

    fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: insn,
        }
    }

    fn make_region(addrs: &[(u64, u64)]) -> Region {
        use std::collections::VecDeque;
        let start = addr(addrs[0].0, addrs[0].1);
        let insns: VecDeque<_> = addrs
            .iter()
            .map(|&(m, i)| RegionInstruction {
                addr: addr(m, i),
                insn: rsleigh::Insn {
                    opcode: rsleigh::Opcode::Copy,
                    output: None,
                    inputs: vec![],
                },
            })
            .collect();
        Region {
            start_addr: start,
            insns,
            ends_with_tail_call: false,
        }
    }

    /// `MachineInsnAddr` must implement `From<u64>` and round-trip correctly.
    #[test]
    fn machine_insn_addr_from_u64() {
        let a: MachineInsnAddr = 0x1000u64.into();
        assert_eq!(a.addr, 0x1000);
    }

    /// Addresses derived from different `u64` values must compare correctly.
    #[test]
    fn machine_insn_addr_ordering() {
        let lo: MachineInsnAddr = 0x100u64.into();
        let hi: MachineInsnAddr = 0x200u64.into();
        assert!(lo < hi);
        assert!(hi > lo);
        assert_eq!(lo, lo);
    }

    // ── PcodeInsnAddr ordering ────────────────────────────────────────────────

    /// The primary sort key is the machine address: a higher machine address
    /// sorts after a lower one regardless of pcode instruction index.
    #[test]
    fn pcode_addr_orders_by_machine_addr_first() {
        assert!(addr(200, 0) > addr(100, 99));
    }

    /// When machine addresses are equal the pcode index is the tiebreaker.
    #[test]
    fn pcode_addr_orders_by_insn_index_when_machine_addr_equal() {
        assert!(addr(100, 1) > addr(100, 0));
        assert!(addr(100, 5) > addr(100, 4));
        assert_eq!(addr(100, 3), addr(100, 3));
    }

    /// Ordering must be antisymmetric: `a < b` implies `b > a`.
    #[test]
    fn pcode_addr_ordering_is_antisymmetric() {
        let a = addr(0x400, 2);
        let b = addr(0x400, 5);
        assert!(a < b);
        assert!(b > a);
    }

    /// Equal addresses compare equal under both `==` and `Ord`.
    #[test]
    fn pcode_addr_equality() {
        let a = addr(0x1000, 7);
        let b = addr(0x1000, 7);
        assert_eq!(a, b);
        assert!((a >= b));
        assert!((a <= b));
    }

    // ── RegionEdgeKind ────────────────────────────────────────────────────────

    /// All four edge-kind variants must be pairwise distinct.
    #[test]
    fn region_edge_kind_variants_are_distinct() {
        let kinds = [
            RegionEdgeKind::Fallthrough,
            RegionEdgeKind::Branch,
            RegionEdgeKind::IfCaseTrue,
            RegionEdgeKind::IfCaseFalse,
        ];
        for i in 0..kinds.len() {
            for j in (i + 1)..kinds.len() {
                assert_ne!(kinds[i], kinds[j]);
            }
        }
    }

    // ── Region::contains_addr ─────────────────────────────────────────────────

    /// The start address is inside the region.
    #[test]
    fn region_contains_addr_at_start() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1000, 0)));
    }

    /// The last instruction address is inside the region.
    #[test]
    fn region_contains_addr_at_end() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1010, 0)));
    }

    /// An address that lies strictly between start and end is inside the region.
    /// (`contains_addr` uses the lexicographic range, not the instruction list.)
    #[test]
    fn region_contains_addr_in_interior() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1008, 0)));
    }

    /// A pcode-index sub-address between two instructions is inside the region.
    #[test]
    fn region_contains_addr_pcode_interior() {
        // insns at (0x1000,0) and (0x1000,3); index 1 is in between
        let r = make_region(&[(0x1000, 0), (0x1000, 3)]);
        assert!(r.contains_addr(addr(0x1000, 1)));
    }

    /// An address strictly before the start is outside the region.
    #[test]
    fn region_contains_addr_before_start() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(!r.contains_addr(addr(0x0ff8, 0)));
    }

    /// An address strictly after the last instruction is outside the region.
    #[test]
    fn region_contains_addr_after_end() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(!r.contains_addr(addr(0x1014, 0)));
    }
}
