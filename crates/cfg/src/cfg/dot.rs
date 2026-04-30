use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::RegionEdgeKind;
use super::Cfg;
use anyhow::anyhow;

use crate::error::Result;

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    #[must_use]
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
    }
}

#[doc(hidden)]
pub mod test_api {
    //! Test-only forwarder for varnode-name rendering.

    use super::Cfg;
    use crate::error::Result;

    /// # Errors
    /// Propagates errors from the underlying renderer (invalid reg vn,
    /// unsupported varnode space, or Sleigh lookup failure).
    pub fn vn_to_name<R: rsleigh::MemReader>(
        cfg: &Cfg<R>,
        vn: &rsleigh::Vn,
    ) -> Result<String> {
        ir::dot::label::vn_to_display_name(&cfg.sleigh, vn)
    }
}

pub struct CfgDotDumper<'a, R: rsleigh::MemReader>(&'a Cfg<R>);

impl<R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'_, R> {
    type Node = NodeIndex;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        self.0.graph.node_indices()
    }

    fn dump_as_dot(
        &self,
        node_id: Self::Node,
        out: &mut dot::DotEmitter,
        _state: &mut Self::State,
    ) -> Result<()> {
        use std::fmt::Write;

        let dot_id = node_id.index().to_string();
        let node = self
            .0
            .graph
            .node_weight(node_id)
            .ok_or_else(|| anyhow!("invalid region index {node_id:?}"))?;
        let first_insn_index = node.start_addr.insn_index;
        let start_addr = node.start_addr.machine_addr.addr;

        // Build node label once
        let mut label = format!("Instruction(addr={start_addr:#x}, idx={first_insn_index})\n");

        for insn in &node.insns {
            let variables: Vec<String> = insn
                .insn
                .output
                .iter()
                .chain(insn.insn.inputs.iter())
                .map(|vn| ir::dot::label::vn_to_display_name(&self.0.sleigh, vn))
                .collect::<Result<_>>()?;
            let insn_addr = insn.addr.machine_addr.addr;
            write!(&mut label, "\\l{insn_addr:#x}: {:?}", insn.insn.opcode)
                .map_err(anyhow::Error::from)?;
            if !variables.is_empty() {
                write!(&mut label, ", {}", variables.join(", ")).map_err(anyhow::Error::from)?;
            }
        }
        write!(&mut label, "\\l").map_err(anyhow::Error::from)?;

        // Add node
        out.node(&dot_id, &label, "box", &[]);

        // Incoming edges
        for edge in self.0.graph.edges_directed(node_id, petgraph::Incoming) {
            let src_id = edge.source().index().to_string();
            let edge_label = format!("{:?}", edge.weight());
            let edge_style = match edge.weight() {
                RegionEdgeKind::Branch => "bold",
                RegionEdgeKind::Fallthrough => "solid",
                RegionEdgeKind::IfCaseFalse | RegionEdgeKind::IfCaseTrue => "dashed",
            };
            out.edge(
                &src_id,
                &dot_id,
                &[("label", edge_label.as_str()), ("style", edge_style)],
            );
        }

        Ok(())
    }
}
