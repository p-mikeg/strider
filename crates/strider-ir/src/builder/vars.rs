use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{NodeKind, ValueId, ValueKind};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Returns the current `ValueId` for `var` in the active region, or
    /// `None` if the variable is not known.
    ///
    /// only consumer is sibling `builder/call.rs`; no
    /// external crate uses it.  Demoted to `pub(super)`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active. (Does
    /// not error when the variable is not tracked — that returns `Ok(None)`.)
    pub(super) fn read_variable_optional(&self, var: &rsleigh::Vn) -> Result<Option<ValueId>> {
        if let Some(variable_id) = self.var_table.key_of(var) {
            Ok(Some(self.read_variable_from_id(variable_id)?))
        } else {
            Ok(None)
        }
    }

    /// Returns the current `ValueId` for `variable` in the active region.
    ///
    /// Returns an error if the variable is not tracked or no region is active.
    ///
    /// # Errors
    ///
    /// Returns `VariableNotFound` when `variable` is not tracked
    /// by the builder, or `NoCurrentRegion` when no region is
    /// active.
    pub fn read_variable(&self, variable: &rsleigh::Vn) -> Result<ValueId> {
        let id = self
            .var_table
            .key_of(variable)
            .ok_or_else(|| anyhow!("variable {variable:?} not found in builder"))?;
        self.read_variable_from_id(id)
    }

    /// Writes `value` to `variable` in the active region.
    ///
    /// # Errors
    ///
    /// Returns `VariableNotFound` when `variable` is not tracked
    /// by the builder, or `NoCurrentRegion` when no region is
    /// active.
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: ValueId) -> Result<()> {
        let var_id = self
            .var_table
            .key_of(variable)
            .ok_or_else(|| anyhow!("variable {variable:?} not found in builder"))?;
        self.write_variable_from_id(var_id, value)
    }

    /// Wires `region_id` as the function entry: connects the entry control
    /// and memory edges and creates initial variable nodes for every tracked
    /// variable.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedOutputSize` when any tracked variable
    /// has a byte size with no matching [`crate::node::ValueType`].
    /// Other variants from `link_control_regions` /
    /// `link_memory_regions` / `link_region_variables` also
    /// propagate.
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        // `build_entry()` (called unconditionally by `new()`) sets the
        // entry, so this is an invariant — but return an error rather than
        // panicking if it is ever violated.
        let entry_node = self.function.entry().ok_or_else(|| {
            anyhow!("set_entry_region: entry node is not set (build_entry must run in new)")
        })?;
        let [entry_control] = self.function().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // Create initial variables
        let var_ids: Vec<_> = self.var_table.keys().collect();
        let mut initial_variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.var_table[var_id];
            let output_type = crate::node::ValueType::int_for_byte_size(var.size)?;
            let value =
                self.build_single_output_pure(NodeKind::InitialVar(var), [], output_type);
            initial_variables[var_id] = value;
            // `Function::all_vns` (the ordered tracked-varnode SSoT) is
            // populated eagerly in `new` from the same `var_table`
            // (VarId / allocation order), so this loop — which iterates
            // that same order — needs no per-`InitialVar` push.  The
            // register-list derivations read `all_vns` directly.
            // Register the InitialVar in the graph's O(1) Vn→NodeId
            // index so downstream consumers (the orchestrator's
            // `read_or_init_var` fallback) don't re-scan `preorder()`
            // to locate it.
            let (node_id, _slot) = self.function().value_definition(value);
            self.function_mut().register_initial_var(var, node_id);
        }
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Creates a new region in the graph with fresh `Region`,
    /// `MemPhi`, and per-variable `VarPhi` nodes.
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly created
    /// `Region` or `MemPhi` does not have its expected output shape
    /// (this would indicate a graph-construction bug, not a user error).
    /// Other variants from `build_vn_phi` propagate.
    pub fn create_region(&mut self) -> Result<RegionId> {
        let memory_node = self.create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;

        let control_node = self.create_node(
            NodeKind::Region,
            [],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [control, phi_token] = self.function().node_outputs_exact(control_node)?;

        // Wire the PhiToken as MemPhi.inputs[0], mirroring how
        // VarPhi nodes are linked.  This gives MemPhi a direct back-reference to
        // its Region so that dead-branch elimination and redundant-phi removal
        // can treat MemPhi and VarPhi identically (same positional logic, same
        // automatic discovery via value_uses(cs_phi_out)).
        self.function_mut().graph_mut().add_node_input(memory_node, phi_token);

        let var_ids: Vec<_> = self.var_table.keys().collect();
        let mut variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.var_table[var_id];
            variables[var_id] = self.build_vn_phi(var, phi_token, &[])?;
        }
        self.create_region_helper(control_node, control, memory_node, memory, variables)
    }
}
