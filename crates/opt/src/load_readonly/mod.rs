use ir::BuiltFunctionGraph;
use ir::node::NodeKind;
use reader::ReadOnlyMemory;

use crate::pipeline::{OptimizationResult, Optimizer};

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// Wrap a concrete memory implementation and add this optimizer to the pipeline:
///
/// ```ignore
/// pipeline.add(LoadReadOnly(my_rom));
/// ```
pub struct LoadReadOnly<M>(pub M);

impl<M: ReadOnlyMemory + 'static> Optimizer for LoadReadOnly<M> {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;

        for node_id in nodes {
            let kind = *function.graph.node_kind(node_id);
            let NodeKind::Load(space) = kind else {
                continue;
            };

            // Load inputs: [memory_token, addr].
            let inputs = function.graph.node_inputs(node_id);
            if inputs.len() < 2 {
                continue;
            }
            let addr_input = inputs[1];
            let Some(addr) = function.int_const_val(addr_input) else {
                continue;
            };

            // Load output: the single value output carries the loaded data type.
            let [data_out] = function.graph.node_outputs_exact::<1>(node_id)?;
            let Some(ty) = function.graph.output_kind(data_out).as_value() else {
                continue;
            };
            let size = ty.byte_size();

            let Some(loaded) = self.0.read(space, addr, size) else {
                continue;
            };

            let Some(masked) = ty.get_unsigned_int(loaded) else {
                continue;
            };
            let new_out = function.make_int_const(masked, ty)?;
            result |= OptimizationResult::from_changed(function.replace_all_uses(data_out, new_out)?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
