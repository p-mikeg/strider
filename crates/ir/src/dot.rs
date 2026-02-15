use std::collections::HashMap;
use rsleigh::MemReader;

use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};


pub struct GraphDotDumper<'a, R:MemReader> {
    pub(crate) entry: NodeId,
    pub(crate) graph: &'a Graph,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>
}

impl<'a, R: MemReader> GraphDotDumper<'a, R> {
    fn vn_to_name(&self, vn: &rsleigh::Vn) -> String {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => format!("{offset:#x}:{size}"),
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().unwrap();
                regs.vn_to_name(*vn).unwrap().to_string()
            },
            rsleigh::VnSpace::RAM => format!("ram[{offset:#x}]:{size}"),
            rsleigh::VnSpace::UNIQUE => format!("unique[{offset:#x}]:{size}"),
            _ => unreachable!()
        }
    }

    fn pretty_label(&self, node: NodeId) -> String {
        let kind = self.graph.node_kind(node);
        match kind {
            NodeKind::InitialVar(var) => format!("initial vn\n{}", self.vn_to_name(&var)),
            NodeKind::ControlSelector(var) => format!("selector vn\n{}", self.vn_to_name(&var)),
            NodeKind::PostCallVarState(var) => format!("post call vn\n{}", self.vn_to_name(&var)),
            _ => kind.as_str()
        }
    }

    fn edge_color(&self, output: NodeOutputId) -> &'static str {
        match self.graph.output_kind(output) {
            NodeOutputKind::Control => "aqua",
            NodeOutputKind::Memory => "pink",
            NodeOutputKind::ControlSelector => "white",
            NodeOutputKind::OutputType(..) => "yellow"
        }
    }
    fn create_dot_const(&self, node: NodeId, node_dot_id: String, out: &mut dot::DotEmitter) {
        assert!(self.graph.node_kind(node).is_const());
        out.node(&node_dot_id, &self.graph.node_kind(node).as_str(), "ellipse", &[]);
    }

}
pub struct GraphDotDumperState {
    visited_node_id: HashMap<NodeId, String>,
    next_unique_id: u32
}

impl GraphDotDumperState {
    fn get_new_unique_id(&mut self, node_id: NodeId) -> String {
        let next_unique_id = self.next_unique_id;
        let string_id = format!("{next_unique_id}");
        self.visited_node_id.insert(node_id, string_id.clone());
        self.next_unique_id += 1;

        return string_id;
    }

    fn get_dot_id(&mut self, graph: &Graph, node_id: NodeId) -> String {
        if graph.node_kind(node_id).is_const() {
            return self.get_new_unique_id(node_id);
        }
        if let Some(node_string_id) = self.visited_node_id.get(&node_id) {
            return node_string_id.to_string();
        } 
        return self.get_new_unique_id(node_id);

    }
}

impl <'a, R:MemReader> dot::GraphDotDumper for GraphDotDumper<'a, R> {
    type Node = NodeId;
    type Error = std::io::Error;
    type State = GraphDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {
            visited_node_id: HashMap::new(),
            next_unique_id: 0
        }
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        crate::walk::walk_graph(self.graph, self.entry)

    }

    fn dump_as_dot(&self, node: Self::Node, out: &mut dot::DotEmitter, state: &mut Self::State) -> core::result::Result<(), Self::Error> {
        if self.graph.node_kind(node).is_const() {
            return Ok(());
        }
        let cur_node_dot_id = &state.get_dot_id(&self.graph, node);


        out.node(&cur_node_dot_id, &self.pretty_label(node), "box", &[]);

        for (_idx, parent) in self.graph.node_inputs(node).into_iter().enumerate()  {
            let parent_id = self.graph.get_node_from_output(parent);
            let parent_dot_id = state.get_dot_id(&self.graph, parent_id);
            out.edge(
                &parent_dot_id,
                &cur_node_dot_id,
                &[("color", self.edge_color(parent))]
            );
            if self.graph.node_kind(parent_id).is_const() {
                self.create_dot_const(parent_id, parent_dot_id, out);
            }
        }
        Ok(())
    }
}