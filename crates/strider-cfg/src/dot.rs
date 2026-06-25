use std::fmt::Write;

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
        // source, the taken side is resolved through `Cfg::region_if` — the
        // single source of truth for which successor is the if-true arm —
        // so this renderer never re-implements the containment rule itself.
        //
        // Track which CondBranch sources have already had their (single)
        // if-true edge labelled.  In the degenerate `if (c) goto L else
        // goto L` case one source has two parallel edges to this node and
        // `region_if` reports the same region for both arms — the first
        // edge is the taken side, the second falls through to if-false.
        // Without this guard both would render "if-true".
        let mut cond_true_labelled: std::collections::HashSet<NodeIndex> =
            std::collections::HashSet::new();
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
                RegionTerminator::CondBranch { .. } => {
                    // `region_if` resolves the source's if-true / if-false
                    // successors; this edge is the taken side when its
                    // target (this node) is the if-true region and that
                    // source has not yet labelled its taken edge.
                    let succ = self.cfg.region_if(src)?;
                    if succ.if_true_region == Some(node_id) && cond_true_labelled.insert(src) {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use dot::{DotStyle, GraphDot};
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use crate::{Builder, CfgOptions};

    /// Renders a CFG built from `bytes` (starting at `start`) to a raw DOT
    /// string.  Keeps the `Sleigh` alive for the duration of the render (the
    /// dumper borrows it for register-name resolution).
    fn dot_string(bytes: Vec<u8>, start: u64) -> String {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
        let cfg = Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
            .build()
            .expect("Builder::build on synthetic bytes");
        GraphDot::new(cfg.dot_dumper(&sleigh), DotStyle::dark_cfg())
            .as_dot()
            .expect("render dot")
    }

    #[test]
    fn degenerate_same_target_cond_branch_labels_one_true_one_false() {
        // `je +0` at 0x1000: both the taken and fall-through arms land on
        // 0x1002, so the CondBranch region has two parallel edges to one
        // successor region.  The dot renderer must label exactly one
        // "if-true" and one "if-false" (mirroring `region_if`), not both
        // edges "if-true".
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
