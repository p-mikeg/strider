use std::iter;

use super::builder::BuilderExt;
use super::memory::MemoryBuilderExt;
use crate::node::{NodeId, NodeOutputId, NodeKind, NodeOutputKind, Var};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltControl {
    pub(crate) node: NodeId,
    pub(crate) control: NodeOutputId,
    pub(crate) selector: NodeOutputId
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltEntry {
    pub(crate) entry: NodeId,
    pub(crate) control: NodeOutputId,
    pub(crate) memory: NodeOutputId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltCall {
    pub(crate) call: NodeId,
    pub(crate) control: NodeOutputId,
    pub(crate) memory: NodeOutputId,
    pub(crate) ret_val: NodeOutputId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltControlPhi {
    pub(crate) node: NodeId,
    pub(crate) output: NodeOutputId,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltIf {
    pub(crate) true_ctrl: NodeOutputId,
    pub(crate) false_ctrl: NodeOutputId
}

pub trait ControlBuilderExt: BuilderExt + MemoryBuilderExt {
    fn build_control_node(&mut self, incoming: &[NodeOutputId]) -> BuiltControl {
        let node = self.create_node(
            NodeKind::ControlState,
            incoming.iter().copied(),
            [NodeOutputKind::Control, NodeOutputKind::ControlSelector],
        );
        let [control, selector] = self.graph().node_outputs_exact(node);

        BuiltControl {
            node,
            control,
            selector,
        }
    }

    fn build_return(&mut self, ctrl: NodeOutputId, value: Option<NodeOutputId>, ret_vars: impl Iterator<Item = NodeOutputId>) {
        assert!(self.graph().output_kind(ctrl).is_control());
        self.create_node(NodeKind::Return, iter::once(ctrl).chain(value).chain(ret_vars), []);
    }

    fn build_entry(&mut self) -> BuiltEntry {
        let entry = self.create_node(
            NodeKind::Entry,
            [],
            vec![NodeOutputKind::Control]);
        let [control] = self.graph().node_outputs_exact(entry);
        let memory = self.build_initial_memory_node();

        BuiltEntry {
            entry, control, memory
        }
    }


    fn build_control_phi(&mut self, var: Var, selector: NodeOutputId, incoming_values: &[NodeOutputId],
    ) -> BuiltControlPhi {
        assert!(self.graph().output_kind(selector).is_control_selector());

        let phi = self.create_node(
            NodeKind::ControlSelector(var),
            iter::once(selector).chain(incoming_values.iter().copied()),
            [NodeOutputKind::OutputType(var.size.into())],
        );
        BuiltControlPhi {
            node: phi,
            output: self.graph().node_outputs(phi)[0],
        }
    }

    fn build_if(&mut self, ctrl: NodeOutputId, cond: NodeOutputId) -> BuiltIf {
        assert!(self.graph().output_kind(ctrl).is_control());

        let brcond = self.create_node(
            NodeKind::If,
            [ctrl, cond],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl_id, false_ctrl_ctrl_id] = self.graph().node_outputs_exact(brcond);

        let true_ctrl_node = self.create_node(
            NodeKind::IfCase(true),
            [true_ctrl_id],
            [NodeOutputKind::Control],
        );

        let false_ctrl_node = self.create_node(
            NodeKind::IfCase(false),
            [false_ctrl_ctrl_id],
            [NodeOutputKind::Control],
        );

        let [true_ctrl_id] = self.graph().node_outputs_exact(true_ctrl_node);
        let [false_ctrl_id] = self.graph().node_outputs_exact(false_ctrl_node);

        BuiltIf {
            true_ctrl: true_ctrl_id,
            false_ctrl: false_ctrl_id
        }
    }

    fn build_call(&mut self, control: NodeOutputId, mem_state: NodeOutputId, target_addr: NodeOutputId, 
            inputs: &[NodeOutputId]) -> BuiltCall {
        assert!(self.graph().output_kind(control).is_control());
        assert!(self.graph().output_kind(mem_state).is_memory());
        
        // Add the call node itself
        let call = self.create_node(
            NodeKind::Call,
            iter::once(control)
                .chain(iter::once(mem_state)).chain(iter::once(target_addr)).chain(inputs.iter().copied()),
            [NodeOutputKind::Control, NodeOutputKind::Memory, 
            NodeOutputKind::OutputType(crate::node::NodeOutputType::U64)],
        );

        let [control_after_call, memory_after_call, ret_val] = self.graph().node_outputs_exact(call);

        BuiltCall {
            call: call,
            control: control_after_call,
            memory: memory_after_call,
            ret_val: ret_val
        }
    }

    fn build_post_call_var(&mut self, call: NodeOutputId, var: Var) -> NodeOutputId {
        self._build_single_output_pure(NodeKind::PostCallVarState(var), [call],var.size.into())
    }
}