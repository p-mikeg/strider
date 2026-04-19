use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::RegionEdgeKind;
use super::Cfg;
use crate::error::{Error, ErrorKind, Result};

impl<R: rsleigh::MemReader> Cfg<R> {
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(ErrorKind::SleighError)?;
                Ok(regs
                    .vn_to_name(*vn)
                    .ok_or(ErrorKind::InvalidRegVn(*vn))?
                    .to_string())
            }
            rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
            rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
            s => Err(ErrorKind::UnsupportedVnSpaceDisplay(s).into()),
        }
    }

    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
    }
}

pub struct CfgDotDumperState;
pub struct CfgDotDumper<'a, R: rsleigh::MemReader>(&'a Cfg<R>);

impl<'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R> {
    type Node = NodeIndex;
    type Error = Error;
    type State = CfgDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {}
    }

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
            .ok_or(ErrorKind::InvalidRegion(node_id))?;
        let first_insn_index = node
            .insns
            .front()
            .ok_or(ErrorKind::EmptyRegion(node.clone()))?
            .addr
            .insn_index;
        let start_addr = node.start_addr.machine_addr.addr;

        // Build node label once
        let mut label = format!("Instruction(addr={start_addr:#x}, idx={first_insn_index})\n");

        for insn in node.insns.iter() {
            let variables: Vec<String> = insn
                .insn
                .output
                .iter()
                .chain(insn.insn.inputs.iter())
                .map(|vn| self.0.vn_to_name(vn))
                .collect::<Result<_>>()?;
            let insn_addr = insn.addr.machine_addr.addr;
            write!(&mut label, "\\l{insn_addr:#x}: {:?}", insn.insn.opcode)
                .map_err(ErrorKind::FormatError)?;
            if !variables.is_empty() {
                write!(&mut label, ", {}", variables.join(", ")).map_err(ErrorKind::FormatError)?;
            }
        }
        write!(&mut label, "\\l").map_err(ErrorKind::FormatError)?;

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
