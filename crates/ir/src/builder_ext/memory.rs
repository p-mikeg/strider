use super::builder::BuilderExt;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltStore {
    pub(crate) node: NodeId,
    pub(crate) memory: NodeOutputId
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltMemory {
    pub(crate) node: NodeId,
    pub(crate) memory: NodeOutputId
}

pub trait MemoryBuilderExt: BuilderExt {
    fn build_memory_phi(&mut self, inputs: &[NodeOutputId]) -> BuiltMemory {
        let mem_node = self.create_node(
            NodeKind::MemSelector, 
            inputs.iter().copied(),
            [NodeOutputKind::Memory]
        );
        BuiltMemory { 
            node: mem_node, 
            memory: self.graph().node_outputs(mem_node)[0]
        }
    }

    fn build_initial_memory_node(&mut self) -> NodeOutputId {
        let mem_node = self.create_node(
            NodeKind::InitialMemory, 
            [],
            [NodeOutputKind::Memory]
        );
        self.graph().node_outputs_exact::<1>(mem_node)[0]
    }

    fn build_post_call_memory(&mut self, control_node: NodeOutputId) -> NodeOutputId {
        assert!(self.graph().output_kind(control_node).is_memory());
        let mem_node = self.create_node(
            NodeKind::PostCallMemState, 
            [control_node],
            [NodeOutputKind::Memory]
        );
        self.graph().node_outputs_exact::<1>(mem_node)[0]
    }

    fn build_load(&mut self, memory: NodeOutputId, addr: NodeOutputId, 
        space: rsleigh::VnSpace, output_type: NodeOutputType) -> NodeOutputId {
        assert!(self.graph().output_kind(memory).is_memory());
        self._build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type)
    }


    fn build_store(&mut self, memory: NodeOutputId, 
        addr: NodeOutputId, data: NodeOutputId,
        space: rsleigh::VnSpace,
    ) -> BuiltStore {
        assert!(self.graph().output_kind(memory).is_memory());
        let node = self.create_node(NodeKind::Store(space), [memory, addr, data], [NodeOutputKind::Memory]);
        BuiltStore { node, memory: self.graph().node_outputs(node)[0] }
    }
    // TODO: implement load + store
}