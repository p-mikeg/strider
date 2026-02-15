use super::BuilderExt;
use super::BoolBuilderExt;
use crate::node::{NodeOutputId, NodeOutputType, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtendOpKind {
   ZeroExtend,
   SignExtend
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntCmpKind {
    Equal,
    Sless,
    SlessEqual,
    Less,
    LessEqual,
    Carry,
    Scarry,
    Borrow,
    Sborrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntBinaryOpKind {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Div,
    Sdiv,
    Rem,
    Srem,
    ShiftRight,
    SShiftRight,
    ShiftLeft,
    Mul
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntUnaryOpKind {
    Neg,
    Not,
}

pub trait IntBuilderExt: BuilderExt + BoolBuilderExt {
    fn build_int_const(&mut self, val: u64, output_type: NodeOutputType) -> NodeOutputId {
        return self._build_single_output_pure(NodeKind::IntConst(val),[], output_type);
    }

    fn build_uint64_const(&mut self, val: u64) -> NodeOutputId {
        return self.build_int_const(val, NodeOutputType::U64);
    }

    fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Option<u64> {
        let node_id = self.graph().get_node_from_output(output_id);
        let output_type = self.get_output_type(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                output_type.get_unsigned_int(*val)
            },
            NodeKind::BoolConst(val) => {
                assert!(output_type.is_bool());
                Some(*val as u64)
            },
            _ => None
        }
    }

    fn get_as_signed_int(&self, output_id: NodeOutputId) -> Option<i64> {
        let output_type = self.get_output_type(output_id);
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                output_type.get_signed_int(*val)
            },
            _ => None
        }
    }

    fn get_as_int(&self, output_id: NodeOutputId) -> Option<(u64, i64)> {
        let unsigned_val = self.get_as_unsigned_int(output_id);
        let signed_val = self.get_as_signed_int(output_id);
        if let Some(val) = unsigned_val {
            // If unsigbed exists - so should sign and the opposite
            Some((val, signed_val.unwrap()))
        } else {
            None
        }
    }

    fn truncate_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);

        // Truncate const values by changing their return type
        if let Some(val) = self.get_as_unsigned_int(output_id) {
            return self.build_int_const(val, output_type);
        }
        
        // No need to truncate values that are already less than the requested amount
        if curr_output_type.byte_size() <= output_type.byte_size() {
            return output_id;
        }

        return self._build_single_output_pure(NodeKind::Truncate, [output_id], output_type);
    }

    fn extend_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType, kind: ExtendOpKind) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);

        // If it is a const - we can extend ourselves
        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id) {
            return match kind {
                ExtendOpKind::SignExtend => self.build_int_const(signed_val as u64, output_type),
                ExtendOpKind::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
        }
        assert!(output_type.is_integer());
        
        // No need to extend values that are already more than the requested amount
        if curr_output_type.byte_size() >= output_type.byte_size() {
            return output_id;
        }
        return self._build_single_output_pure(NodeKind::Extend(kind), [output_id], output_type);
    }

    fn convert_to_int_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);
        if curr_output_type.is_integer() {
            // In the case we need to truncate or extend the input (u64 to u32 for example)
            let truncate_id = self.truncate_if_needed(output_id, output_type);
            let extend_id = self.extend_if_needed(truncate_id, output_type, ExtendOpKind::ZeroExtend);
            return extend_id;
        }

        return self._build_single_output_pure(NodeKind::CastToInt, [output_id], output_type);
    }

    fn build_int_binary_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: IntBinaryOpKind, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self._build_single_output_pure(NodeKind::IntBinaryOp(kind), [converted_lhs_id, converted_rhs_id], output_type);
    }

    fn build_int_unary_operation(&mut self, input_id: NodeOutputId, kind: IntUnaryOpKind, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_input_id = self.convert_to_int_if_needed(input_id, output_type);

        // Store the requested operation
        return self._build_single_output_pure(NodeKind::IntUnaryOp(kind), [converted_input_id], output_type);
    }

    fn build_int_cmp_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: IntCmpKind, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self._build_single_output_pure(NodeKind::IntCmpOp(kind), [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool);
    }

    fn build_int_add(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Add, output_type)
    }

    fn build_int_sub(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Sub, output_type)
    }

    fn build_int_and(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::And, output_type)
    }

    fn build_int_div(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Div, output_type)
    }

    fn build_int_mul(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Mul, output_type)
    }

    fn build_int_or(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Or, output_type)
    }

    fn build_int_rem(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Rem, output_type)
    }

    fn build_int_srem(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Srem, output_type)
    }

    fn build_int_xor(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Xor, output_type)
    }

    fn build_int_shift_left(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::ShiftLeft, output_type)
    }

    fn build_int_shift_right(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::ShiftRight, output_type)
    }

    fn build_int_sshift_right(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::SShiftRight, output_type)
    }

    fn build_int_sdiv(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_binary_operation(lhs, rhs, IntBinaryOpKind::Sdiv, output_type)
    }

    fn build_int_equal(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Equal, output_type)
    }

    fn build_int_sless(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Sless, output_type)
    }

    fn build_int_sless_equal(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::SlessEqual, output_type)
    }

    fn build_int_less(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Less, output_type)
    }

    fn build_int_less_equal(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::LessEqual, output_type)
    }

    fn build_int_carry(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Carry, output_type)
    }

    fn build_int_scarry(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Scarry, output_type)
    }

    fn build_int_borrow(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Borrow, output_type)
    }

    fn build_int_sborrow(&mut self, lhs: NodeOutputId, rhs: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_cmp_operation(lhs, rhs, IntCmpKind::Sborrow, output_type)
    }

    fn build_int_neg(&mut self, input: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_unary_operation(input, IntUnaryOpKind::Neg, output_type)
    }

    fn build_int_not(&mut self, input: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.build_int_unary_operation(input, IntUnaryOpKind::Not, output_type)
    }

    fn build_int_zextend(&mut self, input: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.extend_if_needed(input, output_type, ExtendOpKind::ZeroExtend)
    }

    fn build_int_sextend(&mut self, input: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.extend_if_needed(input, output_type, ExtendOpKind::SignExtend)
    }

    fn build_int_truncate(&mut self, input: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        self.truncate_if_needed(input, output_type)
    }

    fn build_int_popcount(&mut self, input: NodeOutputId, input_type: NodeOutputType) -> NodeOutputId {
        let converted = self.convert_to_int_if_needed(input, input_type);
        // Popcount is only stored in 1 byte
        self._build_single_output_pure(NodeKind::Popcount, [converted], 1.into())
    }
}
