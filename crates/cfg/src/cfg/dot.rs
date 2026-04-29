use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::RegionEdgeKind;
use super::Cfg;
use anyhow::anyhow;

use crate::error::Result;

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Renders a varnode to a printable name. Fetches Sleigh registers per
    /// call. Used by the `test_api` forwarder for ad-hoc callers; the DOT
    /// dumper bypasses this and calls [`vn_to_name_with_regs`] directly so
    /// it only fetches the register list once per render.
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        match vn.addr.space {
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(anyhow::Error::from)?;
                vn_to_name_with_regs(&regs, vn)
            }
            _ => vn_to_name_non_register(vn),
        }
    }

    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    #[must_use]
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
    }
}

/// Render `vn` to a name, using a pre-fetched [`rsleigh::SleighRegs`] for
/// REGISTER lookups. The non-REGISTER spaces don't need `regs`.
///
/// Currently `regs` is fetched once per node rendered by the DOT dumper.
/// If profiling later flags this as hot, lift the fetch into per-dump
/// state by changing `CfgDotDumper`'s `GraphDotDumper::State` from `()`
/// to `SleighRegs`.
fn vn_to_name_with_regs(regs: &rsleigh::SleighRegs, vn: &rsleigh::Vn) -> Result<String> {
    if vn.addr.space == rsleigh::VnSpace::REGISTER {
        return Ok(regs
            .vn_to_name(*vn)
            .ok_or_else(|| anyhow!("invalid register vn {vn:?}"))?
            .to_string());
    }
    vn_to_name_non_register(vn)
}

/// Render `vn` for non-REGISTER spaces. REGISTER input is a caller-routing
/// bug (the caller should have gone through [`vn_to_name_with_regs`])
/// and yields [`ErrorKind::InvalidRegVn`].
fn vn_to_name_non_register(vn: &rsleigh::Vn) -> Result<String> {
    let offset = vn.addr.off;
    let size = vn.size;
    match vn.addr.space {
        rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
        rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
        rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
        rsleigh::VnSpace::REGISTER => {
            // Caller error: should have routed through with-regs path.
            Err(anyhow!("invalid register vn {vn:?}"))
        }
        s => Err(anyhow!("unsupported varnode space for display: {s:?}")),
    }
}

#[doc(hidden)]
pub mod test_api {
    //! Test-only forwarder for `Cfg::vn_to_name`.

    use super::Cfg;
    use crate::error::Result;

    /// # Errors
    /// Propagates errors from the underlying `Cfg::vn_to_name` (invalid reg vn,
    /// unsupported varnode space, or Sleigh lookup failure).
    pub fn vn_to_name<R: rsleigh::MemReader>(
        cfg: &Cfg<R>,
        vn: &rsleigh::Vn,
    ) -> Result<String> {
        cfg.vn_to_name(vn)
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

        let regs = self.0.sleigh.regs().map_err(anyhow::Error::from)?;

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
                .map(|vn| vn_to_name_with_regs(&regs, vn))
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
