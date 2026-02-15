use super::BuilderExt;
use crate::node::{NodeOutputId, NodeOutputType, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolBinaryOpKind {
    Xor,
    And,
    Or
}



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolUnaryOpKind {
    Neg
}

pub trait BoolBuilderExt: BuilderExt {
    fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        return self._build_single_output_pure(NodeKind::BoolConst(val),[], NodeOutputType::Bool);
    }

    fn get_as_bool(&mut self, output_id: NodeOutputId) -> Option<bool> {
        let node_id = self.graph().get_node_from_output(output_id);
        let output_type = self.get_output_type(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                Some(*val != 0) 
            },
            NodeKind::BoolConst(val) => {
                assert!(output_type.is_bool());
                Some(*val)
            },
            _ => None
        }
    }

    fn convert_to_bool_if_needed(&mut self, output_id: NodeOutputId) -> NodeOutputId {
        let output_kind = self.graph().output_kind(output_id);

        if let Some(bool_val) = self.get_as_bool(output_id) {
            return self.build_boolean_const(bool_val);
        }

        if output_kind.as_value() == Some(NodeOutputType::Bool) {
            return output_id;
        }
        
        // It doesn't make sense to cast phi to bool
        assert!(output_kind.is_value());

        return self._build_single_output_pure(NodeKind::CastToBool, [output_id], NodeOutputType::Bool);
    }

    fn build_boolean_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: BoolBinaryOpKind) -> NodeOutputId {
        // Convert the input to be of boolean type
        let converted_lhs_id = self.convert_to_bool_if_needed(lhs_id);
        let converted_rhs_id = self.convert_to_bool_if_needed(rhs_id);

        // Store the requested operation
        return self._build_single_output_pure(NodeKind::BoolBinaryOp(kind), 
            [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool);
    }


    fn build_boolean_unary_operation(&mut self, input_id: NodeOutputId, kind: BoolUnaryOpKind) -> NodeOutputId {
        // Convert the input to be of boolean type
        let converted_input_id = self.convert_to_bool_if_needed(input_id);

        // Store the requested operation
        return self._build_single_output_pure(NodeKind::BoolUnaryOp(kind), [converted_input_id], NodeOutputType::Bool);
    }

    fn build_boolean_xor(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId) -> NodeOutputId {
        self.build_boolean_operation(lhs_id, rhs_id, BoolBinaryOpKind::Xor)
    }

    fn build_boolean_and(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId) -> NodeOutputId {
        self.build_boolean_operation(lhs_id, rhs_id, BoolBinaryOpKind::And)
    }

    fn build_boolean_or(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId) -> NodeOutputId {
        self.build_boolean_operation(lhs_id, rhs_id, BoolBinaryOpKind::Or)
    }

    fn build_boolean_neg(&mut self, input_id: NodeOutputId) -> NodeOutputId {
        self.build_boolean_unary_operation(input_id, BoolUnaryOpKind::Neg)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FunctionBody, GraphBuilder};
    use super::*;

    fn check_boolean_output(builder: &GraphBuilder<'_>, output_id: NodeOutputId) {
        assert!(builder.get_output_type(output_id).is_bool());
    }

    fn get_const_value(builder: &GraphBuilder<'_>, output_id: NodeOutputId) -> Option<bool> {

        let node_id = builder.graph().get_node_from_output(output_id);
        match builder.graph().node_kind(node_id) {
            NodeKind::BoolConst(val) => Some(*val),
            _ => None
        }
    }

    #[test]
    fn test_build_const() {
        let mut builder = GraphBuilder(&mut FunctionBody::new_invalid());

        // Check that false const is actually false in the graph
        let false_const = builder.build_boolean_const(false);
        check_boolean_output(&builder, false_const);
        assert_eq!(get_const_value(&builder, false_const), Some(false));

        // Check that false const is actually true in the graph
        let true_const = builder.build_boolean_const(true);
        check_boolean_output(&builder, true_const);
        assert_eq!(get_const_value(&builder, true_const), Some(true));
    }

    fn test_boolean_unary_op(
        build: impl Fn(&mut GraphBuilder, NodeOutputId) -> NodeOutputId,
        eval: impl Fn(bool) -> bool,
        ) {
        let mut builder = GraphBuilder(&mut FunctionBody::new_invalid());

        let false_c = builder.build_boolean_const(false);
        let true_c  = builder.build_boolean_const(true);

        for &input_val in &[false, true] {
            let input_id = if input_val { true_c } else { false_c };
            let out = build(&mut builder, input_id);

            check_boolean_output(&builder, out);
            // TODO: add checks for neg of a generic node and one that requires conversion (later when we will be able to do it easier and more readable?)
            assert_eq!(
                get_const_value(&builder, out),
                Some(eval(input_val)),
                "op({}) failed",
                input_val
            );
        }
    }
    #[test]
    fn test_neg() {
        test_boolean_unary_op(
            |b, v|  b.build_boolean_neg(v),
            |a| !a,
        );
    }

    fn test_boolean_binop(
        build: impl Fn(&mut GraphBuilder, NodeOutputId, NodeOutputId) -> NodeOutputId,
        eval: impl Fn(bool, bool) -> bool,
        ) {
        let mut builder = GraphBuilder(&mut FunctionBody::new_invalid());

        let false_c = builder.build_boolean_const(false);
        let true_c  = builder.build_boolean_const(true);

        for &lhs in &[false, true] {
            for &rhs in &[false, true] {
                let lhs_id = if lhs { true_c } else { false_c };
                let rhs_id = if rhs { true_c } else { false_c };

                let out = build(&mut builder, lhs_id, rhs_id);

                check_boolean_output(&builder, out);
                // TODO: add checks for neg of a generic node and one that requires conversion (later when we will be able to do it easier and more readable?)
                assert_eq!(
                    get_const_value(&builder, out),
                    Some(eval(lhs, rhs)),
                    "op({}, {}) failed",
                    lhs, rhs
                );
            }
        }
    }


    #[test]
    fn test_and() {
        test_boolean_binop(
            |b, l, r| b.build_boolean_and(l, r),
            |a, b| a & b,
        );
    }

    #[test]
    fn test_or() {
        test_boolean_binop(
            |b, l, r| b.build_boolean_or(l, r),
            |a, b| a | b,
        );
    }

    #[test]
    fn test_xor() {
        test_boolean_binop(
            |b, l, r| b.build_boolean_xor(l, r),
            |a, b| a ^ b,
        );
    }
}