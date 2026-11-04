use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::Result;
use crate::builder::IRBuilderExt;
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

    /// Wires `region_id` as the entry, its current values the new
    /// `InitialVar`s. Errors on a variable size with no
    /// [`crate::node::ValueType`].
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        let mut packed = SecondaryMap::new();
        for (var_id, &value) in initial_variables.iter() {
            packed[var_id] = value.into();
        }
        // An entry that is also a loop header gets each `InitialVar` as its
        // phis' entry-edge operand.
        self.seed_variables(region_id, packed);
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Links `region_id` to the entry control and memory, then builds one
    /// `InitialVar` per tracked variable.
    pub(crate) fn wire_entry_and_build_initial_vars(
        &mut self,
        region_id: RegionId,
    ) -> Result<SecondaryMap<InitialVnId, ValueId>> {
        let entry_node = self.function.entry();
        let [entry_control] = self.function().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_edges(region_id, entry_control, entry_memory);

        // A `vn_id` is both the variable key and the `InitialVar` payload.
        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        let mut initial_variables = SecondaryMap::new();
        for vn_id in vn_ids {
            let var = self.function().initial_vn(vn_id);
            let output_type = crate::node::ValueType::int_from_bytes(var.size)?;
            let value = self.build_single_output_pure(NodeKind::InitialVar(vn_id), [], output_type);
            initial_variables[vn_id] = value;
            let node_id = self.function().producer(value);
            self.function_mut().side_tables_mut().initial_var_index[vn_id] = Some(node_id);
        }
        Ok(initial_variables)
    }
}
