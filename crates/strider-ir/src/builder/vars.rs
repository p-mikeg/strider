use anyhow::anyhow;
use cranelift_entity::SecondaryMap;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{NodeKind, ValueId, ValueKind};
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
        let entry_node = self.function.entry();
        let [entry_control] = self.function().node_outputs_exact(entry_node)?;
        let entry_memory = self.entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // Create initial variables.  The tracked-varnode ids ARE the
        // `InitialVar` payloads (both come from the one `vn_interner`), so a
        // `vn_id` doubles as the SSA-variable key and the node payload — no
        // index translation.
        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        let mut initial_variables = SecondaryMap::new();
        for vn_id in vn_ids {
            let var = self.function().initial_vn(vn_id);
            let output_type = crate::node::ValueType::int_for_byte_size(var.size)?;
            let value = self.build_single_output_pure(NodeKind::InitialVar(vn_id), [], output_type);
            initial_variables[vn_id] = value;
            // Register the InitialVar in the graph's O(1) Vn→NodeId
            // index so downstream consumers (the orchestrator's
            // `read_or_init_var` fallback) don't re-scan `preorder()`
            // to locate it.
            let node_id = self.function().producer(value);
            self.function_mut()
                .side_tables
                .initial_var_index
                .insert(vn_id, node_id);
        }
        // Register-passed argument carriers are recorded by the LIFTER right
        // after this call (it owns the machine-register `container_of` map,
        // which resolves a narrow ABI arg alias like `edi` to its tracked
        // container `rdi`).  `set_entry_region` only wires the region and the
        // `InitialVar` nodes; the carrier's entry value is recoverable via
        // [`crate::Function::initial_var_value`].
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Test-only: record register-passed argument carriers on the arg table,
    /// mirroring what the LIFTER does in prod right after `set_entry_region`.
    ///
    /// Each arg-passing register resolves to its largest tracked container (via
    /// [`crate::function::largest_container_in`] over `all_vns` — the same
    /// containment rule the lifter's `container_of` map applies), and that
    /// container's `InitialVar` value is registered as the carrier for the
    /// argument's positional index.  Direct-builder tests (no lifter) call this
    /// after `set_entry_region` to reproduce the prod arg table.
    #[cfg(any(test, feature = "test-util"))]
    pub fn record_register_arg_carriers(&mut self) {
        let arg_regs: Vec<rsleigh::Vn> = self.function.default_cc().arg_passing_regs.clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            let container = crate::function::largest_container_in(self.function.all_vns(), reg);
            if let Some(value) = self.function.initial_var_value(&container) {
                self.function_mut().side_tables_mut().register_arg_value(i as u32, value);
            }
        }
    }

    /// Creates a new region in the graph with fresh `Region`,
    /// `MemPhi`, and per-variable `Phi` nodes.
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
        // Phi nodes are linked.  This gives MemPhi a direct back-reference to
        // its Region so that dead-branch elimination and redundant-phi removal
        // can treat MemPhi and Phi identically (same positional logic, same
        // automatic discovery via value_uses(cs_phi_out)).
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, phi_token);

        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        let mut variables = SecondaryMap::new();
        for vn_id in vn_ids {
            let var = self.function().initial_vn(vn_id);
            variables[vn_id] = self.build_vn_phi(var, phi_token, &[])?;
        }
        self.create_region_helper(control_node, control, memory_node, memory, variables)
    }
}
