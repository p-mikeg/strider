use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{InitialVnId, NodeKind, ValueId};
use crate::region::RegionId;

impl FunctionBuilder {
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
            .function
            .vn_id_of(variable)
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
            .function
            .vn_id_of(variable)
            .ok_or_else(|| anyhow!("variable {variable:?} not found in builder"))?;
        self.write_variable_from_id(var_id, value)
    }

    /// Wires `region_id` as the function entry: connects the entry control and
    /// memory edges, creates an `InitialVar` node for every tracked variable,
    /// and — since the entry region has no predecessors — stores those
    /// `InitialVar`s DIRECTLY as its current variable values.  This is the
    /// dominator-tree root every other region inherits from via
    /// [`FunctionBuilder::inherit_variables`].
    ///
    /// The sole production entry setup (the pruned-SSA lift path): the entry
    /// region normally carries no value phis, but any phi that WAS placed there
    /// (a rare entry-is-also-a-join, e.g. a loop header) is still wired.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedOutputSize` when any tracked variable has a byte
    /// size with no matching [`crate::node::ValueType`].  Other variants from
    /// `link_control_regions` / `link_memory_regions` / `link_region_variables`
    /// also propagate.
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        // The entry region has no predecessors, so the InitialVars ARE its
        // current variable values.
        self.set_region_variables(region_id, initial_variables.clone());
        // Wire any phi that WAS placed at the entry (only if the entry is a
        // join — rare, e.g. an entry that is also a loop header).
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Wires the entry region's control + memory edges and builds one
    /// `InitialVar` node per tracked variable (registering each in the O(1)
    /// `Vn`→`NodeId` index), returning the `vn_id`→`InitialVar` map.  Shared by
    /// [`Self::set_entry_region`] and [`Self::set_entry_region_all`], which
    /// differ only in whether those `InitialVar`s become the region's current
    /// values.
    pub(crate) fn wire_entry_and_build_initial_vars(
        &mut self,
        region_id: RegionId,
    ) -> Result<SecondaryMap<InitialVnId, ValueId>> {
        let entry_node = self.function.entry();
        let [entry_control] = self.function().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // The tracked-varnode ids ARE the `InitialVar` payloads (both come from
        // the one `vn_interner`), so a `vn_id` doubles as the SSA-variable key
        // and the node payload — no index translation.  Register-passed
        // argument carriers are recorded by the LIFTER right after entry setup
        // (it owns the machine-register `container_of` map that resolves a
        // narrow ABI arg alias like `edi` to its tracked container `rdi`); the
        // carrier's entry value is recoverable via
        // [`crate::Function::initial_var_value`].
        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        let mut initial_variables = SecondaryMap::new();
        for vn_id in vn_ids {
            let var = self.function().initial_vn(vn_id);
            let output_type = crate::node::ValueType::int_for_byte_size(var.size)?;
            let value = self.build_single_output_pure(NodeKind::InitialVar(vn_id), [], output_type);
            initial_variables[vn_id] = value;
            // Register the InitialVar in the graph's O(1) Vn→NodeId index so
            // downstream consumers (the orchestrator's `read_or_init_var`
            // fallback) don't re-scan `preorder()` to locate it.
            let node_id = self.function().producer(value);
            self.function_mut()
                .side_tables_mut()
                .initial_var_index
                .insert(vn_id, node_id);
        }
        Ok(initial_variables)
    }
}
