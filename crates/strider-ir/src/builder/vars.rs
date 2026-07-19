use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{InitialVnId, NodeKind, ValueId};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Errors when `variable` is untracked or no region is active.
    pub fn read_variable(&self, variable: &rsleigh::Vn) -> Result<ValueId> {
        let id = self
            .function
            .vn_id_of(variable)
            .ok_or_else(|| anyhow!("variable {variable:?} not found in builder"))?;
        self.read_variable_from_id(id)
    }

    /// Errors when `variable` is untracked or no region is active.
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: ValueId) -> Result<()> {
        let var_id = self
            .function
            .vn_id_of(variable)
            .ok_or_else(|| anyhow!("variable {variable:?} not found in builder"))?;
        self.write_variable_from_id(var_id, value)
    }

    /// Wires `region_id` as the function entry. Having no predecessors, it
    /// takes the freshly built `InitialVar`s directly as its current variable
    /// values, making it the dominator-tree root every other region inherits
    /// from via [`FunctionBuilder::inherit_variables`].
    ///
    /// Errors when a tracked variable's byte size has no matching
    /// [`crate::node::ValueType`].
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        self.set_region_variables(region_id, initial_variables.clone());
        // The entry normally carries no phis, but it can also be a join (a
        // loop header), and any phi placed there still needs wiring.
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Shared by [`Self::set_entry_region`] and [`Self::set_entry_region_all`],
    /// which differ only in whether the `InitialVar`s become the region's
    /// current values.
    pub(crate) fn wire_entry_and_build_initial_vars(
        &mut self,
        region_id: RegionId,
    ) -> Result<SecondaryMap<InitialVnId, ValueId>> {
        let entry_node = self.function.entry();
        let [entry_control] = self.function().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // Tracked-varnode ids and `InitialVar` payloads share one interner, so
        // a `vn_id` doubles as SSA-variable key and node payload with no index
        // translation. Register-passed argument carriers are recorded by the
        // lifter right after this, since only it owns the `container_of` map
        // that resolves a narrow ABI alias like `edi` to its container `rdi`.
        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        let mut initial_variables = SecondaryMap::new();
        for vn_id in vn_ids {
            let var = self.function().initial_vn(vn_id);
            let output_type = crate::node::ValueType::int_for_byte_size(var.size)?;
            let value = self.build_single_output_pure(NodeKind::InitialVar(vn_id), [], output_type);
            initial_variables[vn_id] = value;
            // Index it so downstream consumers don't re-scan `preorder()` to
            // find it.
            let node_id = self.function().producer(value);
            self.function_mut()
                .side_tables_mut()
                .initial_var_index
                .insert(vn_id, node_id);
        }
        Ok(initial_variables)
    }
}
