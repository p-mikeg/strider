use dot::GraphDotDumper;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Cfg;
use super::types::RegionTerminator;
use anyhow::anyhow;

use crate::Result;

impl Cfg {
    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    ///
    /// Register names are resolved from `sleigh`; the CFG no longer owns a
    /// Sleigh handle, so the caller threads the one that built it.
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> CfgDotDumper<'a, R> {
        CfgDotDumper { cfg: self, sleigh }
    }
}

pub struct CfgDotDumper<'a, R: rsleigh::MemReader> {
    cfg: &'a Cfg,
    sleigh: &'a rsleigh::Sleigh<R>,
}

impl<R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'_, R> {
    type Node = NodeIndex;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        self.cfg.region_graph.node_indices()
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
            .cfg
            .region_graph
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
        let regs = self.sleigh.regs()?;
        for insn in &node.insns {
            let insn_addr = insn.addr.machine_addr.addr;
            let pretty = insn.insn.ctx_fmt(self.sleigh, &regs);
            write!(&mut label, "\\l{insn_addr:#x}: {pretty}").map_err(anyhow::Error::from)?;
        }
        write!(&mut label, "\\l").map_err(anyhow::Error::from)?;

        // Add node
        out.node(&dot_id, &label, "box", &[]);

        // Incoming edges.  Edges are unweighted; the label + style are
        // derived from the SOURCE region's terminator.  For a `CondBranch`
        // source, the taken side is the edge whose target (this node) starts
        // at the terminator's `true_target`.
        for edge in self
            .cfg
            .region_graph
            .edges_directed(node_id, petgraph::Incoming)
        {
            let src = edge.source();
            let src_id = src.index().to_string();
            let src_region = self
                .cfg
                .region_graph
                .node_weight(src)
                .ok_or_else(|| anyhow!("dangling edge source {src:?}"))?;
            let (label, style) = match &src_region.terminator {
                RegionTerminator::CondBranch { true_target } => {
                    // The taken side is the successor that CONTAINS `true_target`
                    // (start_addr can sit below the first instruction — see
                    // `Cfg::region_if`).
                    if node.contains_addr(*true_target) {
                        ("if-true", "dashed")
                    } else {
                        ("if-false", "dashed")
                    }
                }
                RegionTerminator::Switch { .. } => ("switch", "solid"),
                RegionTerminator::Unconditional => ("unconditional", "solid"),
                // These terminators have no outgoing edge; an edge from one is
                // a construction bug, but render it visibly rather than
                // failing the whole dump.
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
