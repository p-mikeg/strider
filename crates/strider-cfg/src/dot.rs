use std::fmt::Write;

use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Cfg;
use super::types::RegionTerminator;
use anyhow::anyhow;

use crate::Result;

impl Cfg {
    /// Names registers and spaces through `names`; a LOAD / STORE space id
    /// resolves through this CFG's own space ids.
    pub fn dot_dumper<'a>(&'a self, names: &'a strider_target::ArchNames) -> CfgDotDumper<'a> {
        CfgDotDumper {
            cfg: self,
            names,
            view: None,
        }
    }
}

pub struct CfgDotDumper<'a> {
    cfg: &'a Cfg,
    names: &'a strider_target::ArchNames,
    /// A neighborhood render; `None` dumps every region.
    view: Option<dot::View<NodeIndex>>,
}

impl CfgDotDumper<'_> {
    /// Renders only `view`, so a neighborhood goes through the same label and
    /// edge rules as a whole CFG rather than a second emitter.
    pub(crate) fn restricted(self, view: dot::View<NodeIndex>) -> Self {
        Self {
            view: Some(view),
            ..self
        }
    }
}

impl GraphDotDumper for CfgDotDumper<'_> {
    type Node = NodeIndex;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        self.cfg.region_graph.node_indices()
    }

    fn view(&self) -> Option<&dot::View<NodeIndex>> {
        self.view.as_ref()
    }

    fn dump_as_dot(
        &self,
        node_id: Self::Node,
        out: &mut dot::DotEmitter,
        _state: &mut Self::State,
    ) -> Result<()> {
        let dot_id = node_id.index().to_string();
        let node = self
            .cfg
            .region_graph
            .node_weight(node_id)
            .ok_or_else(|| anyhow!("invalid region index {node_id:?}"))?;
        let first_insn_index = node.start_addr.insn_index;
        let start_addr = node.start_addr.machine_addr.addr;

        let mut label = format!("Instruction(addr={start_addr:#x}, idx={first_insn_index})\n");

        for insn in &node.insns {
            let insn_addr = insn.addr.machine_addr.addr;
            let pretty = crate::insn_text(&insn.insn, &self.cfg.space_ids, self.names);
            write!(&mut label, "\\l{insn_addr:#x}: {pretty}").map_err(anyhow::Error::from)?;
        }
        write!(&mut label, "\\l").map_err(anyhow::Error::from)?;

        // The label carries hand-emitted `\l` breaks per instruction.
        out.node_raw_label(&dot_id, &label, "box", self.center_attrs(node_id));

        // Edges are unweighted, so label and style come from the SOURCE
        // region's terminator.  A `CondBranch` source defers to `region_if`
        // rather than re-implementing the containment rule here.
        //
        // The set tracks which sources already labelled their one if-true
        // edge.  In the degenerate `if (c) goto L else goto L` case a source
        // has two parallel edges here and `region_if` names the same region
        // for both arms; without the guard both would render "if-true".
        let mut cond_true_labelled: std::collections::HashSet<NodeIndex> =
            std::collections::HashSet::new();
        for edge in self
            .cfg
            .region_graph
            .edges_directed(node_id, petgraph::Incoming)
        {
            let src = edge.source();
            if !self.in_view(src) {
                continue;
            }
            let src_id = src.index().to_string();
            let src_region = self
                .cfg
                .region_graph
                .node_weight(src)
                .ok_or_else(|| anyhow!("dangling edge source {src:?}"))?;
            let (label, style) = match &src_region.terminator {
                RegionTerminator::CondBranch { .. } => {
                    let succ = self.cfg.region_if(src)?;
                    if succ.if_true_region == Some(node_id) && cond_true_labelled.insert(src) {
                        ("if-true", "dashed")
                    } else {
                        ("if-false", "dashed")
                    }
                }
                RegionTerminator::Switch { .. } => ("switch", "solid"),
                RegionTerminator::Unconditional => ("unconditional", "solid"),
                // These have no outgoing edge, so an edge from one is a
                // construction bug.  Render it visibly rather than failing the
                // whole dump.
                RegionTerminator::Return
                | RegionTerminator::NoReturn
                | RegionTerminator::TailCall { .. }
                | RegionTerminator::UnresolvedIndirectBranch { .. } => ("?", "solid"),
            };
            out.edge(&src_id, &dot_id, &[("label", label), ("style", style)]);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use dot::{DotStyle, GraphDot};
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use crate::{CfgOptions, DecodeEngine};

    fn dot_string(bytes: Vec<u8>, start: u64) -> String {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        let mut engine = DecodeEngine::new(arch, reader).expect("decode engine");
        let cfg = engine
            .build_cfg(start.into(), &CfgOptions::default())
            .expect("build_cfg on synthetic bytes");
        GraphDot::new(cfg.dot_dumper(arch.names()), DotStyle::dark_cfg())
            .as_dot()
            .expect("render dot")
    }

    #[test]
    fn degenerate_same_target_cond_branch_labels_one_true_one_false() {
        // `je +0` at 0x1000 puts both arms on 0x1002, giving the CondBranch
        // region two parallel edges to one successor.  Exactly one edge must
        // read "if-true", mirroring `region_if`.
        let dot = dot_string(vec![0x74, 0x00, 0xc3], 0x1000);
        let true_edges = dot.matches("if-true").count();
        let false_edges = dot.matches("if-false").count();
        assert_eq!(
            true_edges, 1,
            "expected exactly one if-true edge, dot:\n{dot}"
        );
        assert_eq!(
            false_edges, 1,
            "expected exactly one if-false edge, dot:\n{dot}"
        );
    }
}
