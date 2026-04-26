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
    /// Varnodes used to return values according to the calling convention,
    /// in ABI order (e.g. `[rax, rdx]` on x86_64).  The first `ret_val_vars.len()`
    /// value-typed outputs of every `Call` (output indices 2..) correspond to
    /// these varnodes in order; `Return` input slots 2.. correspond to these
    /// varnodes in order.
    pub(crate) ret_val_vars: Vec<rsleigh::Vn>,
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
    #[must_use] 
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

    /// Creates a new [`FunctionBuilder`] from a resolved calling convention.
    ///
    /// `all_used_variables` is the complete set of varnodes (registers /
    /// unique temporaries) that appear in the function.  The convention
    /// supplies the argument-passing, callee-saved, and stack-pointer sets;
    /// every variable not callee-saved (and not SP) is recorded as
    /// call-clobbered; SP is rebound at each call site via an explicit
    /// `Add(sp, ret_stack_pop)` node.
    ///
    /// # Errors
    ///
    /// Propagates whatever [`Self::new_raw`] would return — currently
    /// [`ErrorKind::UnsupportedOutputSize`] from the entry-block setup when
    /// a tracked variable's byte size has no matching `NodeOutputType`.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: &target::BuiltCallingConvention,
    ) -> Result<Self> {
        // Ensure all return registers (int + float) are tracked variables.
        // This keeps the data-flow chain from a float operation's output
        // (e.g. an aarch64 FloatAdd writes to s0, the 4-byte sub-register of q0)
        // connected to the Return node — without this step `q0` would not be
        // in the variable set, and the analyzer's register-aliasing logic
        // would never widen the s0 write into a q0 store visible to Return.
        for v in cc.ret_val_regs.iter().chain(cc.ret_val_regs_float.iter()) {
            if !all_used_variables.contains(v) {
                all_used_variables.push(*v);
            }
        }
        // Union of int + float return registers, in that order.  Pattern
        // queries that index `ret_val(0)` continue to find the first integer
        // ret slot; new queries can use `ret_val(N)` where N >= int-count to
        // reach float ret slots.
        let mut combined_ret_vars: Vec<rsleigh::Vn> = Vec::with_capacity(
            cc.ret_val_regs.len() + cc.ret_val_regs_float.len(),
        );
        combined_ret_vars.extend(cc.ret_val_regs.iter().copied());
        combined_ret_vars.extend(cc.ret_val_regs_float.iter().copied());
        Self::new_raw(
            all_used_variables,
            &cc.arg_passing_regs,
            &cc.callee_saved_regs,
            &combined_ret_vars,
            Some(cc.stack_ptr_vn),
            cc.ret_stack_pop,
        )
    }

    /// Low-level constructor that takes the convention-derived data as
    /// unpacked slices.  Used by synthetic tests that don't resolve a real
    /// calling convention against a Sleigh register table — production code
    /// should use [`FunctionBuilder::new`] with a [`target::BuiltCallingConvention`].
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::UnsupportedOutputSize`] when any tracked variable
    /// has a byte size with no matching `NodeOutputType` (the entry-block
    /// builder allocates an `InitialVar` per tracked variable).
    pub fn new_raw(
        all_used_variables: Vec<rsleigh::Vn>,
        arg_passing_vars: &[rsleigh::Vn],
        callee_saved_vars: &[rsleigh::Vn],
        ret_vars: &[rsleigh::Vn],
        stack_ptr_vn: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
    ) -> Result<Self> {
        // For overlapping varnodes in the same fixed-offset space, keep only
        // the largest enclosing one.  E.g. if both `rdi` and `edi` are
        // touched, drop `edi`.  Same applies to UNIQUE space — Sleigh's
        // MIPS lifter writes a 64-bit IntMul result to a unique varnode
        // and Copies a 4-byte slice of it to a register; without the filter
        // the 4-byte and 8-byte unique varnodes are treated as independent
        // SSA variables (BUG-1: MIPS MULT not lowered).
        //
        // CONST and code-space varnodes don't behave like fixed-offset
        // registers — they're addressed by literal value or runtime address,
        // so containment-by-offset is meaningless there.
        let is_aliasable_space = |s: rsleigh::VnSpace| {
            s == rsleigh::VnSpace::REGISTER || s == rsleigh::VnSpace::UNIQUE
        };
        let all_variables: Vec<_> = all_used_variables
            .iter()
            .filter(|v| {
                if !is_aliasable_space(v.addr.space) {
                    return true;
                }
                !all_used_variables.iter().any(|other| {
                    other != *v
                        && other.addr.space == v.addr.space
                        && other.addr.off <= v.addr.off
                        && other.addr.off + other.size as u64 >= v.addr.off + v.size as u64
                        && other.size > v.size
                })
            })
            .copied()
            .collect();
        // `call_cloberred_variables` is emitted as the Call node's value
        // outputs in order (slot `i + 2` ↔ `call_cloberred_variables[i]`).
        // Front-load it with the calling convention's return registers so
        // `.ret_output(0)` indexes into ABI ret slot 0 (e.g. rax on x86_64),
        // then append the remaining caller-clobbered registers.
        let call_cloberred_variables: Vec<_> = {
            let is_clobbered = |v: &rsleigh::Vn| {
                !callee_saved_vars.contains(v) && Some(*v) != stack_ptr_vn
            };
            let ret_prefix = ret_vars
                .iter()
                .copied()
                .filter(|v| all_variables.contains(v) && is_clobbered(v));
            let rest = all_variables
                .iter()
                .filter(|v| is_clobbered(v) && !ret_vars.contains(v))
                .copied();
            ret_prefix.chain(rest).collect()
        };
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
        let ret_val_vars: Vec<_> = ret_vars
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
            ret_val_vars,
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

    /// Returns the calling convention's return-value registers, in ABI order.
    /// Empty for synthetic test builds that didn't supply a convention.
    #[must_use] 
    pub fn ret_val_vars(&self) -> &[rsleigh::Vn] {
        &self.ret_val_vars
    }

    /// Finalises and returns the completed [`BuiltFunctionGraph`], after running
    /// structural validation on the built graph.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ValidationFailed`] wrapping a
    /// [`crate::validate::ValidationErrors`] bundle if the built graph fails
    /// any of validate's three layers (local typing, use-list consistency,
    /// graph-level invariants).
    pub fn build(self) -> crate::Result<crate::function::BuiltFunctionGraph> {
        let built = crate::function::BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.call_cloberred_variables.into_boxed_slice(),
            ret_val_regs: self.ret_val_vars.into_boxed_slice(),
        };
        crate::validate::validate(&built.graph, built.entry)?;
        Ok(built)
    }
}
