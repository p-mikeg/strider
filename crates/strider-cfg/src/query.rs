use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionTerminator};
use super::{Cfg, RegionId};
use anyhow::anyhow;

use crate::{DataRanges, FunctionBound, Result};

/// Whether `target` lies outside both the function `bound` describes from
/// `start_addr` and every `fragments` range. A `Size` window that overflows
/// is `[start_addr, u64::MAX]`.
pub(crate) fn is_addr_tail_call(
    target: u64,
    start_addr: u64,
    bound: FunctionBound,
    fragments: &DataRanges,
) -> bool {
    let outside = match bound {
        FunctionBound::Size(size) => {
            target < start_addr
                || start_addr
                    .checked_add(size.get())
                    .is_some_and(|end| target >= end)
        }
        FunctionBound::Open { code_before_start } => !code_before_start && target < start_addr,
    };
    outside && !fragments.overlaps(target, target.saturating_add(1))
}

/// The user-op a `CallOther` names: its CONST input 0, as an index into
/// `names`. `None` for any other operand shape.
#[must_use]
pub fn user_op_of<'n>(insn: &rsleigh::Insn, names: &'n [String]) -> Option<(u32, &'n str)> {
    let id = insn
        .inputs
        .first()
        .filter(|vn| vn.addr_space == rsleigh::VnSpace::CONST)?
        .addr_off;
    let id = u32::try_from(id).ok()?;
    names.get(id as usize).map(|name| (id, name.as_str()))
}

/// A direct `Call`'s target address: its first operand, when that names the
/// code space.
#[must_use]
pub fn direct_call_target(insn: &rsleigh::Insn, code_space: rsleigh::VnSpace) -> Option<u64> {
    if insn.opcode != rsleigh::Opcode::Call {
        return None;
    }
    insn.inputs
        .first()
        .filter(|vn| vn.addr_space == code_space)
        .map(|vn| vn.addr_off)
}

pub struct IfRegionSuccessors {
    pub if_true_region: Option<NodeIndex>,
    /// The fall-through side.
    pub if_false_region: Option<NodeIndex>,
}

impl Cfg {
    /// The taken side is the successor STARTING at `true_target`, else the one
    /// CONTAINING it (a target off an instruction boundary, seated onto its
    /// owner and reported `InteriorTarget`); the other edge is the
    /// fall-through. `if (c) goto L else goto L` answers L for both; a
    /// non-`CondBranch` region `None` for both.
    ///
    /// Start wins over containment: a size bound cutting the last instruction
    /// leaves the fall-through's span overhanging a stub that starts there.
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionSuccessors> {
        let region = self
            .region_graph
            .node_weight(region_id)
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let RegionTerminator::CondBranch { true_target } = region.terminator else {
            return Ok(IfRegionSuccessors {
                if_true_region: None,
                if_false_region: None,
            });
        };
        let mut starts_at_taken = None;
        let mut contains_taken = None;
        let mut rest = None;
        for edge in self
            .region_graph
            .edges_directed(region_id, petgraph::Outgoing)
        {
            let target = edge.target();
            let successor = &self.region_graph[target];
            // One edge per bucket, for the both-arms-one-region case.
            if successor.start_addr == true_target && starts_at_taken.is_none() {
                starts_at_taken = Some(target);
            } else if successor.contains_addr(true_target) && contains_taken.is_none() {
                contains_taken = Some(target);
            } else {
                rest = Some(target);
            }
        }
        let (if_true_region, if_false_region) = match starts_at_taken {
            Some(taken) => (Some(taken), contains_taken.or(rest)),
            None => (contains_taken, rest),
        };
        Ok(IfRegionSuccessors {
            if_true_region,
            if_false_region,
        })
    }

    /// Unordered.
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.region_graph.node_weights()
    }

    /// Unordered; a predecessor with parallel edges is yielded once per edge.
    pub fn region_predecessors(&self, region_id: RegionId) -> impl Iterator<Item = RegionId> + '_ {
        self.region_graph
            .neighbors_directed(region_id, petgraph::Incoming)
    }

    /// Unordered.
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.region_graph.node_indices()
    }

    /// Every successor of `switch_region`, keyed by its full `start_addr`: a
    /// wired arm begins exactly at its target, a machine-instruction start, so
    /// a successor starting mid-pcode at that address does not answer for it.
    pub fn switch_arm_regions(
        &self,
        switch_region: RegionId,
    ) -> rustc_hash::FxHashMap<super::types::PcodeInsnAddr, RegionId> {
        self.region_graph
            .neighbors(switch_region)
            .map(|s| (self.region_graph[s].start_addr, s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use petgraph::stable_graph::StableDiGraph;
    use strider_target::SleighArch;

    use super::*;
    use crate::CfgOptions;
    use crate::test_support::*;
    use crate::types::{PcodeInsnAddr, Region};

    #[test]
    fn is_addr_tail_call_by_bound() {
        let open = |code_before_start| FunctionBound::Open { code_before_start };
        let size = FunctionBound::size(0x100);
        let none = DataRanges::default();
        let cold = DataRanges::new([0x0800..0x0810, 0x1200..0x1210]);
        for (target, bound, fragments, tail) in [
            (0x0800, open(false), &none, true),
            (0x0800, open(true), &none, false),
            (0x1200, open(false), &none, false),
            (0x0800, size, &none, true),
            (0x10ff, size, &none, false),
            (0x1100, size, &none, true),
            (0x0800, open(false), &cold, false),
            (0x080f, size, &cold, false),
            (0x0810, size, &cold, true),
            (0x1200, size, &cold, false),
            (0x1210, size, &cold, true),
        ] {
            assert_eq!(
                is_addr_tail_call(target, 0x1000, bound, fragments),
                tail,
                "{target:#x} {bound:?}"
            );
        }
    }

    #[test]
    fn is_addr_tail_call_overflowing_window_top_addr_is_in_range() {
        // The window cannot wrap, so it clamps to `[start, u64::MAX]` and
        // every target at or above start, u64::MAX included, is in range.
        let start = u64::MAX - 0x100;
        // Overflows when added to start.
        let bound = FunctionBound::size(0x1000);
        assert!(
            !is_addr_tail_call(u64::MAX, start, bound, &DataRanges::default()),
            "u64::MAX is the top of the non-wrapping window, must be in-range"
        );
        assert!(
            !is_addr_tail_call(start + 0x10, start, bound, &DataRanges::default()),
            "interior of an overflowing window must be in-range"
        );
        assert!(
            is_addr_tail_call(start - 1, start, bound, &DataRanges::default()),
            "below start is still a tail call"
        );
    }

    fn real_cfg(case: &str, fn_name: &str) -> Cfg {
        use object::{Object, ObjectSymbol};

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out/x64")
            .join(format!("{case}.elf"));
        assert!(
            path.exists(),
            "{} is not built; run `make -C fixtures`",
            path.display()
        );
        let elf = strider_reader::load_elf(&path)
            .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
        let obj = elf.checked_file().expect("the mapped file is unchanged");
        let mem = strider_reader::ElfFileMemReader::new(
            &elf,
            strider_reader::elf::RelocationMode::Unapplied,
        )
        .expect("ElfFileMemReader::new");
        let arch = SleighArch::x86_64();
        let entry_addr = obj
            .symbol_by_name(fn_name)
            .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
            .address();
        crate::DecodeEngine::new(arch, mem)
            .expect("decode engine")
            .build_cfg(entry_addr.into(), &CfgOptions::default())
            .unwrap_or_else(|e| panic!("build_cfg for {fn_name:?}: {e:?}"))
    }

    #[test]
    fn region_if_both_successors_present_on_abs_val() {
        let cfg = real_cfg("control", "abs_val");
        let has_pair = cfg.region_ids().any(|id| {
            let s = cfg.region_if(id).unwrap();
            s.if_true_region.is_some() && s.if_false_region.is_some()
        });
        assert!(
            has_pair,
            "abs_val: no region with both if-true and if-false successors"
        );
    }

    #[test]
    fn region_if_absent_on_linear_entry() {
        let cfg = real_cfg("arithmetic", "add");
        let s = cfg.region_if(cfg.entry).unwrap();
        assert!(s.if_true_region.is_none());
        assert!(s.if_false_region.is_none());
    }

    fn make_cond_region(start_machine: u64, true_target: PcodeInsnAddr) -> Region {
        let mut r = make_region(&[(start_machine, 0)]);
        r.terminator = RegionTerminator::CondBranch { true_target };
        r
    }

    #[test]
    fn region_if_resolves_polarity_by_true_target() {
        // The taken side is decided by containment, not edge insertion order.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3000, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0)]));
        // Fall-through edge FIRST, to prove order-independence.
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = make_cfg(graph, src);

        let s = cfg.region_if(src).unwrap();
        assert_eq!(
            s.if_true_region,
            Some(taken),
            "taken side = true_target match"
        );
        assert_eq!(s.if_false_region, Some(fallthrough));
    }

    #[test]
    fn region_if_matches_taken_successor_by_containment_not_start() {
        // A hand-built region spanning [0x3000, 0x3010] with the branch
        // targeting the interior 0x3008, to pin that region_if matches the taken
        // arm by containment rather than start-address equality.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3008, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0), (0x3010, 0)]));
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = make_cfg(graph, src);

        let s = cfg.region_if(src).unwrap();
        assert_eq!(
            s.if_true_region,
            Some(taken),
            "interior true_target must match the taken successor by containment"
        );
        assert_eq!(s.if_false_region, Some(fallthrough));
    }

    #[test]
    fn region_if_both_arms_same_region_returns_that_region_for_both() {
        // Degenerate `if (c) goto L else goto L`: both edges point at one
        // region, which must be reported for both sides.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x2000, 0)));
        let both = graph.add_node(make_region(&[(0x2000, 0)]));
        graph.add_edge(src, both, ());
        graph.add_edge(src, both, ());

        let cfg = make_cfg(graph, src);

        let s = cfg.region_if(src).unwrap();
        assert_eq!(s.if_true_region, Some(both));
        assert_eq!(s.if_false_region, Some(both));
    }

    #[test]
    fn switch_arm_regions_keys_each_arm_by_its_own_start() {
        // Three switch arms; each keys to its own successor, and an address no
        // arm starts at is absent. The map is built from outgoing edges, so
        // only successors of `src` are considered.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let arm_a = graph.add_node(make_region(&[(0x2000, 0)]));
        let arm_b = graph.add_node(make_region(&[(0x3000, 0)]));
        let arm_c = graph.add_node(make_region(&[(0x4000, 0)]));
        // A non-successor region starting at a would-be target, to prove the
        // lookup is edge-scoped rather than global.
        let stranger = graph.add_node(make_region(&[(0x5000, 0)]));
        graph.add_edge(src, arm_a, ());
        graph.add_edge(src, arm_b, ());
        graph.add_edge(src, arm_c, ());

        let cfg = make_cfg(graph, src);

        let arms = cfg.switch_arm_regions(src);
        assert_eq!(arms.get(&addr(0x3000, 0)), Some(&arm_b));
        assert_eq!(arms.get(&addr(0x2000, 0)), Some(&arm_a));
        assert_eq!(arms.get(&addr(0x4000, 0)), Some(&arm_c));
        assert_eq!(
            arms.get(&addr(0x5000, 0)),
            None,
            "a region not wired as a successor of `src` is not an arm"
        );
        let _ = stranger;
    }

    /// A switch target is always a machine-instruction START
    /// (`PcodeInsnAddr::at_machine_start`), so an arm is the successor whose own
    /// start is that address at pcode index 0. A successor starting MID-pcode at
    /// the same machine address (a `CondBranch` into a pcode sequence) is a
    /// different region and must not answer for it.
    #[test]
    fn switch_arm_regions_does_not_key_a_mid_pcode_successor_at_the_machine_start() {
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let mid_pcode = graph.add_node(make_region(&[(0x2000, 3)]));
        graph.add_edge(src, mid_pcode, ());

        let cfg = make_cfg(graph, src);

        assert_eq!(
            cfg.switch_arm_regions(src).get(&addr(0x2000, 0)),
            None,
            "no successor starts at 0x2000's pcode index 0, so the site has no arm"
        );
        let _ = mid_pcode;
    }

    /// A `CallOther`'s user-op is its CONST input 0 as a table index; any other
    /// shape names none.
    #[test]
    fn user_op_of_reads_a_const_index_into_the_table() {
        let names = ["LOCK".to_owned(), "UNLOCK".to_owned()];
        let call_other = |input: Option<rsleigh::Vn>| rsleigh::Insn {
            opcode: rsleigh::Opcode::CallOther,
            output: None,
            inputs: input.into_iter().collect(),
        };
        let at = |addr_space, addr_off| rsleigh::Vn {
            addr_space,
            addr_off,
            size: 4,
        };
        let of = |insn| user_op_of(&insn, &names);
        assert_eq!(
            of(call_other(Some(at(rsleigh::VnSpace::CONST, 1)))),
            Some((1, "UNLOCK"))
        );
        assert_eq!(of(call_other(Some(at(rsleigh::VnSpace::CONST, 2)))), None);
        assert_eq!(
            of(call_other(Some(at(rsleigh::VnSpace::REGISTER, 1)))),
            None
        );
        assert_eq!(
            of(call_other(Some(at(rsleigh::VnSpace::CONST, 1 << 32)))),
            None
        );
        assert_eq!(of(call_other(None)), None);
    }

    /// A direct `Call`'s target is its operand only in the code space; a
    /// `CallIndirect` names no address.
    #[test]
    fn direct_call_target_reads_a_code_space_operand_only() {
        let op = |opcode, addr_space| rsleigh::Insn {
            opcode,
            output: None,
            inputs: vec![rsleigh::Vn {
                addr_space,
                addr_off: 0x10,
                size: 8,
            }]
            .into(),
        };
        let code = rsleigh::VnSpace::RAM;
        assert_eq!(
            direct_call_target(&op(rsleigh::Opcode::Call, code), code),
            Some(0x10)
        );
        assert_eq!(
            direct_call_target(&op(rsleigh::Opcode::Call, rsleigh::VnSpace::REGISTER), code),
            None
        );
        assert_eq!(
            direct_call_target(&op(rsleigh::Opcode::CallIndirect, code), code),
            None
        );
    }
}
