use crate::{BoolBinaryOpKind, BoolUnaryOpKind, ExtendOpKind, 
    IntBinaryOpKind, IntCmpKind, IntUnaryOpKind, Var, node::{NodeId, NodeKind, NodeOutputId, NodeOutputType}};
use crate::error::{ErrorKind, Result};

// this is the readable foramt to work with the nodes 
pub enum NodeView {
    Entry,
    InitialMemory,
    InitialVar { var: rsleigh::Vn },

    Store { space: rsleigh::VnSpace, memory: NodeOutputId, addr: NodeOutputId, data: NodeOutputId },
    Load { space: rsleigh::VnSpace, memory: NodeOutputId, addr: NodeOutputId },


    // // branches
    ControlState { inputs: Vec<NodeOutputId> }, 
    MemState { cf_node: NodeId, inputs: NodeIdList },

    If { control: NodeOutputId, cond: NodeOutputId },
    IfCase { control: NodeOutputId, case: bool },
    Call { control: NodeOutputId, memory: NodeOutputId, target_addr: NodeOutputId, args: Vec<NodeOutputId> },
    PostCallMemState { call: NodeOutputId },
    PostCallVarState { var: Var, call: NodeOutputId },
    // PostCallCfState { call: NodeId },

    // // Int operations
    IntConst { value: u64, output_type: NodeOutputType },
    IntUnaryOp { input: NodeOutputId, op: IntUnaryOpKind, output_type: NodeOutputType },
    IntBinaryOp { lhs: NodeOutputId, rhs: NodeOutputId, op: IntBinaryOpKind, output_type: NodeOutputType },
    IntCmpOp { lhs: NodeOutputId, rhs: NodeOutputId, op: IntCmpKind, output_type: NodeOutputType },
    CastToInt { input: NodeOutputId, input_type: NodeOutputType, output_type: NodeOutputType },

    // // Bool operations
    BoolConst { value: bool },
    BoolUnaryOp { input: NodeOutputId, op: BoolUnaryOpKind },
    BoolBinaryOp { lhs: NodeOutputId, rhs: NodeOutputId, op: BoolBinaryOpKind },
    CastToBool { input: NodeOutputId, input_type: NodeOutputType, output_type: NodeOutputType },

    Truncate { input: NodeOutputId, input_type: NodeOutputType, output_type: NodeOutputType },
    ZeroExtend { input: NodeOutputId, input_type: NodeOutputType, output_type: NodeOutputType },
    Extend { input: NodeOutputId, kind: ExtendOpKind, input_type: NodeOutputType, output_type: NodeOutputType  },
}


/**
 *     // Initial state
    // General state
    ControlState,
    MemPhi,
    ControlPhi(Var),
    Return,
 */
impl crate::graph::Graph {

    fn get_output_type(&self, output: NodeOutputId) -> Result<NodeOutputType> {
        let kind = self.output_kind(output);
        kind.as_value().ok_or(ErrorKind::ExpectedValue(output, kind))
    }

    fn verify_bool_type(&self, outputs: &[NodeOutputId]) -> Result<()> {
        for &output in outputs {
            if !self.output_kind(output).is_bool() {
                return Err(ErrorKind::ExpectedBool(output).into());
            }
        }
        Ok(())
    }

    fn verify_control_kind(&self, outputs: &[NodeOutputId]) -> Result<()> {
        for &output in outputs {
            let kind = self.output_kind(output);
            if !kind.is_control() {
                return Err(ErrorKind::ExpectedControl(output, kind).into());
            }
        }
        Ok(())
    }

    fn verify_int_type(&self, outputs: &[NodeOutputId]) -> Result<()> {
        for &output in outputs {
            if !self.output_kind(output).is_integer() {
                return Err(ErrorKind::ExpectedInteger(output).into());
            }
        }
        Ok(())
    }

    fn verify_memory_kind(&self, outputs: &[NodeOutputId]) -> Result<()> {
        for &output in outputs {
            let kind = self.output_kind(output);
            if !kind.is_memory() {
                return Err(ErrorKind::ExpectedMemory(output, kind).into());
            }
        }
        Ok(())
    }

    pub fn node_view(&self, node_id: NodeId) -> Result<NodeView> {
        let view = match self.node_kind(node_id) {
            NodeKind::Entry => {
                let [] = self.node_inputs_exact(node_id);
                let [control] = self.node_outputs_exact(node_id);
                self.verify_control_kind(&[control])?;
                NodeView::Entry
            }
            NodeKind::BoolConst(v) => {
                let [] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_bool_type(&[output])?;
                NodeView::BoolConst { value: *v }
            }
            NodeKind::BoolUnaryOp(op) => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_bool_type(&[input, output])?;
                NodeView::BoolUnaryOp { input, op: *op }
            },
            NodeKind::BoolBinaryOp(op) => {
                let [lhs, rhs] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_bool_type(&[lhs, rhs, output])?;
                NodeView::BoolBinaryOp {lhs, rhs, op: *op }
            },
            NodeKind::CastToBool => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_inputs_exact(node_id);
                self.verify_bool_type(&[output])?;
                NodeView::CastToBool { input, input_type: self.get_output_type(input)?, output_type: self.get_output_type(output)? }
            },
            NodeKind::IntConst(v) => {
                let [] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[output])?;
                NodeView::IntConst { value: *v, output_type: self.get_output_type(output)? }
            }
            NodeKind::IntUnaryOp(op) => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[input, output])?;
                NodeView::IntUnaryOp { input, op: *op, output_type: self.get_output_type(output)? }
            },
            NodeKind::IntBinaryOp(op) => {
                let [lhs, rhs] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[lhs, rhs, output])?;
                NodeView::IntBinaryOp {lhs, rhs, op: *op, output_type: self.get_output_type(output)? }
            },
            NodeKind::IntCmpOp(op) => {
                let [lhs, rhs] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[lhs, rhs])?;
                self.verify_bool_type(&[output])?;
                NodeView::IntCmpOp {lhs, rhs, op: *op, output_type: self.get_output_type(output)? }
            },
            NodeKind::CastToInt => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_inputs_exact(node_id);
                self.verify_int_type(&[output])?;
                NodeView::CastToInt { input, input_type: self.get_output_type(input)?, output_type: self.get_output_type(output)? }
            },
            NodeKind::Truncate => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_inputs_exact(node_id);
                self.verify_int_type(&[input, output])?;
                NodeView::Truncate { input, input_type: self.get_output_type(input)?, output_type: self.get_output_type(output)? }
            },
            NodeKind::Extend(kind) => {
                let [input] = self.node_inputs_exact(node_id);
                let [output] = self.node_inputs_exact(node_id);
                self.verify_int_type(&[input, output])?;
                NodeView::Extend { input, kind: *kind, input_type: self.get_output_type(input)?, output_type: self.get_output_type(output)? }
            },
            NodeKind::InitialMemory => {
                let [] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_memory_kind(&[output])?;
                NodeView::InitialMemory
            },
            NodeKind::IfCase(case) => {
                let [input_control] = self.node_inputs_exact(node_id);
                let [output_control] = self.node_outputs_exact(node_id);
                self.verify_control_kind(&[input_control, output_control])?;
                NodeView::IfCase { control: input_control, case: *case }
            },
            NodeKind::If => {
                let [control, cond] = self.node_inputs_exact(node_id);
                let [true_ctrl_case, false_ctrl_case] = self.node_outputs_exact(node_id);
                self.verify_control_kind(&[true_ctrl_case, control, false_ctrl_case])?;
                self.verify_bool_type(&[cond])?;
                NodeView::If { control, cond }
            },
            NodeKind::Call => {
                let inputs: Vec<NodeOutputId> = self.node_inputs(node_id).into_iter().collect();
                let control = inputs[0];
                let memory = inputs[1];
                let target_addr = inputs[2];
                let args = inputs[3..].iter().copied().collect();
                self.verify_control_kind(&[control])?;
                self.verify_memory_kind(&[memory])?;
                self.verify_int_type(&[target_addr])?;
                NodeView::Call { control, memory, target_addr, args }
            }
            NodeKind::PostCallMemState => {
                let [call] = self.node_inputs_exact(node_id);
                let [memory] = self.node_outputs_exact(node_id);
                self.verify_control_kind(&[call])?;
                self.verify_memory_kind(&[memory])?;
                NodeView::PostCallMemState { call }
            },
            NodeKind::PostCallVarState(var) => {
                let [call] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_control_kind(&[call])?;
                self.verify_int_type(&[output])?;
                NodeView::PostCallVarState { var: *var, call }
            }
            NodeKind::InitialVar(var) => {
                let [] = self.node_inputs_exact(node_id);
                let [output] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[output])?;
                NodeView::InitialVar { var: *var }
            }
            NodeKind::Store(space) => {
                let [memory, addr, data] = self.node_inputs_exact(node_id);
                let [new_mem] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[addr, data])?;
                self.verify_memory_kind(&[memory, new_mem])?;
                NodeView::Store { space: *space, memory, addr, data }
            }
            NodeKind::Load(space) => {
                let [memory, addr] = self.node_inputs_exact(node_id);
                let [] = self.node_outputs_exact(node_id);
                self.verify_int_type(&[addr])?;
                self.verify_memory_kind(&[memory])?;
                NodeView::Load { space: *space, memory, addr }
            },
            // NodeKind::Return => {
            //     let inputs: Vec<NodeOutputId> = self.node_inputs(node_id).into_iter().collect();
            //     let control = inputs[0];

            //     let memory = inputs[1];

            // }
        };
        return Ok(view);
    }

}