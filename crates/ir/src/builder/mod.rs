use crate::error::{ErrorKind, Result};
use crate::function::FunctionGraph;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::region::Region;
use cranelift_entity::{PrimaryMap, entity_impl};
use std::collections::HashMap;

mod call;
mod coerce;
mod nodes;
#[cfg(test)]
mod tests;
mod vars;

/// A dense, typed identifier for a tracked variable (varnode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(u32);
entity_impl!(VarId);

/// Incrementally constructs a sea-of-nodes IR function graph.
///
/// The builder tracks SSA-style per-region variable state: each variable has
/// exactly one current `NodeOutputId` inside the active region.  Reads and
/// writes go through this mapping so that the graph is always in a consistent
/// state.
pub struct FunctionBuilder {
    pub(crate) function: FunctionGraph,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub(crate) variable_to_id: HashMap<rsleigh::Vn, VarId>,
    /// Variables clobbered by any call instruction (everything not
    /// callee-saved, and excluding the stack pointer which is rebound
    /// separately with the `ret_stack_pop` adjust).
    pub(crate) call_cloberred_variables: Vec<rsleigh::Vn>,
    /// Variables used to pass arguments according to the calling convention.
    pub(crate) arg_passing_vars: Vec<rsleigh::Vn>,
    /// Stack pointer varnode — when present, it is excluded from the
    /// `call_cloberred_variables` set and rebound at every `Call` to
    /// `Add(pre_call_sp, IntConst(ret_stack_pop))`.  `None` in synthetic
    /// tests that don't model stack-aware calling conventions.
    pub(crate) stack_ptr_vn: Option<rsleigh::Vn>,
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  0 on link-register ISAs, pointer size on stack-push ISAs.
    /// Ignored when `stack_ptr_vn` is `None`.
    pub(crate) ret_stack_pop: i64,
}

impl FunctionBuilder {
    /// Returns a reference to the underlying [`FunctionGraph`].
    pub fn body(&self) -> &FunctionGraph {
        &self.function
    }

    /// Returns a mutable reference to the underlying [`FunctionGraph`].
    pub fn body_mut(&mut self) -> &mut FunctionGraph {
        &mut self.function
    }

    pub(crate) fn graph(&self) -> &Graph {
        &self.body().graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut Graph {
        &mut self.function.graph
    }

    pub(super) fn validate_value_inputs(&self, inputs: &[NodeOutputId]) -> Result<()> {
        for &v in inputs {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(v, kind).into());
            }
        }
        Ok(())
    }

    /// Creates a new [`FunctionBuilder`] with the given variable set and
    /// calling-convention registers.
    ///
    /// `all_used_variables` is the complete set of varnodes (registers /
    /// unique temporaries) that appear in the function.  Variables not in
    /// `callee_saved_vars` (and not the stack pointer itself) are recorded
    /// as call-clobbered; SP is rebound at each call site via an explicit
    /// `Add(sp, ret_stack_pop)` node.
    pub fn new(
        all_used_variables: Vec<rsleigh::Vn>,
        arg_passing_vars: &[rsleigh::Vn],
        callee_saved_vars: &[rsleigh::Vn],
        _ret_vars: &[rsleigh::Vn],
        stack_ptr_vn: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
    ) -> Result<Self> {
        // For register varnodes, keep only the largest enclosing register.
        // e.g. if both `rdi` and `edi` are clobbered, drop `edi` because
        // clobbering `rdi` already implies `edi`.
        let all_variables: Vec<_> = all_used_variables
            .iter()
            .filter(|v| {
                if v.addr.space != rsleigh::VnSpace::REGISTER {
                    return true;
                }
                !all_used_variables.iter().any(|other| {
                    other != *v
                        && other.addr.space == rsleigh::VnSpace::REGISTER
                        && other.addr.off <= v.addr.off
                        && other.addr.off + other.size as u64 >= v.addr.off + v.size as u64
                        && other.size > v.size
                })
            })
            .copied()
            .collect();
        let call_cloberred_variables: Vec<_> = all_variables
            .iter()
            .filter(|v| !callee_saved_vars.contains(v) && Some(**v) != stack_ptr_vn)
            .copied()
            .collect();
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        let arg_passing_vars: Vec<_> = arg_passing_vars
            .iter()
            .copied()
            .filter(|vn| variable_to_id.contains_key(vn))
            .collect();

        let mut fb = FunctionBuilder {
            function: FunctionGraph::new_invalid(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id,
            arg_passing_vars,
            call_cloberred_variables,
            stack_ptr_vn,
            ret_stack_pop,
        };
        fb.build_entry()?;
        Ok(fb)
    }

    /// Creates a node in the graph with the given kind, inputs, and output kinds.
    pub(super) fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.graph_mut().create_node(kind, inputs, output_kinds)
    }

    /// Creates a single-output, pure (no side-effect) node and returns its
    /// output id.
    pub(super) fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
        self.graph().node_outputs(node)[0]
    }

    /// Returns an iterator over all tracked varnodes.
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    /// Finalises and returns the completed [`BuiltFunctionGraph`], after running
    /// structural validation on the built graph.
    pub fn build(self) -> crate::Result<crate::function::BuiltFunctionGraph> {
        let built = crate::function::BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.call_cloberred_variables.into_boxed_slice(),
        };
        crate::validate::validate(&built.graph, built.entry)?;
        Ok(built)
    }
}
