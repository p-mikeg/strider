use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Returns the current `NodeOutputId` for `var` in the active region, or
    /// `None` if the variable is not known.
    ///
    /// only consumer is sibling `builder/call.rs`; no
    /// external crate uses it.  Demoted to `pub(super)`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active. (Does
    /// not error when the variable is not tracked — that returns `Ok(None)`.)
    pub(super) fn read_variable_optional(&self, var: &rsleigh::Vn) -> Result<Option<NodeOutputId>> {
        if let Some(variable_id) = self.variable_to_id.get(var) {
            Ok(Some(self.read_variable_from_id(*variable_id)?))
        } else {
            Ok(None)
        }
    }

    /// Returns the current `NodeOutputId` for `variable` in the active region.
    ///
    /// Returns an error if the variable is not tracked or no region is active.
    ///
    /// # Errors
    ///
    /// Returns `VariableNotFound` when `variable` is not tracked
    /// by the builder, or `NoCurrentRegion` when no region is
    /// active.
    pub fn read_variable(&self, variable: &rsleigh::Vn) -> Result<NodeOutputId> {
        let &id = self
            .variable_to_id
            .get(variable)
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
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: NodeOutputId) -> Result<()> {
        let var_id = *self
            .variable_to_id
            .get(variable)
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
    /// has a byte size with no matching [`crate::node::NodeOutputType`].
    /// Other variants from `link_control_regions` /
    /// `link_memory_regions` / `link_region_variables` also
    /// propagate.
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        #[allow(clippy::expect_used)] // build_entry() is called unconditionally by new_raw()
        let entry_node = self
            .graph
            .entry()
            .expect("entry is always set by build_entry(), which new_raw() calls unconditionally");
        let [entry_control] = self.graph().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // Create initial variables
        let var_ids: Vec<_> = self.variables.keys().collect();
        let mut initial_variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.variables[var_id];
            let output_type = var.size.try_into()?;
            let out =
                self.build_single_output_pure(NodeKind::InitialVar(var), [], output_type);
            initial_variables[var_id] = out;
            // Register the InitialVar in the graph's O(1) Vn→NodeId
            // index so downstream consumers (the orchestrator's
            // `read_or_init_var` fallback) don't re-scan `preorder()`
            // to locate it.
            let (node_id, _slot) = self.graph().output_definition(out);
            self.graph_mut().register_initial_var(var, node_id);
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
    /// Other variants from `build_control_phi` propagate.
    pub fn create_region(&mut self) -> Result<RegionId> {
        let memory_node = self.create_node(NodeKind::MemPhi, [], [NodeOutputKind::Memory(None)]);
        let [memory] = self.graph().node_outputs_exact(memory_node)?;

        let control_node = self.create_node(
            NodeKind::Region,
            [],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let [control, phi_token] = self.graph().node_outputs_exact(control_node)?;

        // Wire the PhiToken as MemPhi.inputs[0], mirroring how
        // VarPhi nodes are linked.  This gives MemPhi a direct back-reference to
        // its Region so that dead-branch elimination and redundant-phi removal
        // can treat MemPhi and VarPhi identically (same positional logic, same
        // automatic discovery via output_uses(cs_phi_out)).
        self.graph_mut().add_node_input(memory_node, phi_token)?;

        let var_ids: Vec<_> = self.variables.keys().collect();
        let mut variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.variables[var_id];
            variables[var_id] = self.build_control_phi(var, phi_token, &[])?;
        }
        self.create_region_helper(control_node, control, memory_node, memory, variables)
    }
}
