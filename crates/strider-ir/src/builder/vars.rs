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
            self.function_mut().side_tables.initial_var_index.insert(vn_id, node_id);
        }
        // Record register-passed arguments unconditionally: each arg-passing
        // register's (largest-container) InitialVar is the carrier for its
        // positional index. We don't filter on use here — an argument the
        // function never reads is culled by DCE and dropped from the arg
        // table by `Function::compact`, so patterns won't find it.
        let arg_regs: Vec<rsleigh::Vn> = self.function.default_cc().arg_passing_regs.clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            // Resolve the arg register to its largest tracked container
            // before the var-table lookup: the var table is keyed only by
            // the deduped largest-container tracked varnodes, so a narrow
            // ABI arg alias (e.g. `edi`) must route through its container
            // (`rdi`) — mirroring `call_ret_vals_for` / `call_clobbered_for`.
            let key = self.function.container_of(reg);
            if let Some(vn_id) = self.function.vn_id_of(&key) {
                let value = initial_variables[vn_id];
                self.function_mut().register_arg_value(i as u32, value);
            }
        }
        self.link_region_variables(region_id, &initial_variables)
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
