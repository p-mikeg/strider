use std::collections::HashSet;

use crate::opt::{OptimizationResult, Optimizer};

fn remove_selectors(function: &mut ir::BuiltFunctionGraph, node_id: ir::node::NodeId) -> OptimizationResult {
    match function.graph.node_kind(node_id) {
        ir::node::NodeKind::ControlSelector(..) => {
            let inputs: HashSet<_> = function.graph.node_inputs(node_id).into_iter().collect();
            // More than 1 input for this selector
            if inputs.len() > 2 {
                return OptimizationResult::NoChange;
            }
            let input = function.graph.node_inputs(node_id)[1];
            let [output] = function.graph.node_outputs_exact::<1>(node_id);
            
            let mut cursor = function.graph.output_use_cursor(output);
            while let Some((_, _)) = cursor.current() {
                cursor.replace_current_with(input);
            }

            OptimizationResult::Changed
        },
        ir::node::NodeKind::MemSelector => {
            let inputs: HashSet<_> = function.graph.node_inputs(node_id).into_iter().collect();
            // More than 1 input for this selector
            if inputs.len() > 1 {
                return OptimizationResult::NoChange;
            }
            let input = function.graph.node_inputs(node_id)[0];
            let [output] = function.graph.node_outputs_exact::<1>(node_id);
            
            let mut cursor = function.graph.output_use_cursor(output);
            while let Some((_, _)) = cursor.current() {
                cursor.replace_current_with(input);
            }

            OptimizationResult::Changed
        },
        ir::node::NodeKind::ControlState => {
            let node_inputs: Vec<_> = function.graph.node_inputs(node_id).into_iter().collect();
            if node_inputs.len() != 1 {
                return OptimizationResult::NoChange;
            }
            let [output, selector] = function.graph.node_outputs_exact::<2>(node_id);
            let inputs: Vec<_> = function.graph.output_uses(selector).into_iter().collect();
            // More than 1 input for this selector
            if inputs.len() > 0 {
                return OptimizationResult::NoChange;
            }
            let node_inputs: Vec<_> = function.graph.node_inputs(node_id).into_iter().collect();
            if node_inputs.len() != 1 {
                return OptimizationResult::NoChange;
            }
            let [input] = function.graph.node_inputs_exact::<1>(node_id);
            
            let mut cursor = function.graph.output_use_cursor(output);
            while let Some((_, _)) = cursor.current() {
                cursor.replace_current_with(input);
            }

            OptimizationResult::Changed
        },
        _ => OptimizationResult::NoChange
    }
}

pub struct RedundantSelectors;

impl Optimizer for RedundantSelectors {
    fn optimize(&self, function: &mut ir::BuiltFunctionGraph) -> OptimizationResult {
        let mut res = OptimizationResult::NoChange;
        let graph_nodes: Vec<_> = function.preorder().collect();
        for node_id in graph_nodes {
            res |= remove_selectors(function, node_id);
        }
        res
    }
}
