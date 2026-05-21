use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::RegionEdgeKind;
use super::Cfg;
use anyhow::anyhow;

use crate::cfg::Result;

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    #[must_use]
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
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

        // rsleigh's `Insn::ctx_fmt(sleigh, regs)` produces
        // `<Opcode> <vn0>, <vn1>, …` with register names resolved via
        // the sleigh register table — exactly what we want for human
        // inspection.  Resolving `regs` is not free (FFI walk over the
        // arch's register table), so we cache it once per `dump_as_dot`
        // invocation rather than per-instruction.
        let regs = self.0.sleigh.regs()?;
        for insn in &node.insns {
            let insn_addr = insn.addr.machine_addr.addr;
            let pretty = insn.insn.ctx_fmt(&self.0.sleigh, &regs);
            write!(&mut label, "\\l{insn_addr:#x}: {pretty}")
                .map_err(anyhow::Error::from)?;
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
