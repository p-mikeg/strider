use anyhow::anyhow;
use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{SecondaryMap, entity_impl};

use crate::IRViewer;
use crate::Result;
use crate::builder::checks::require_memory_kind;
use crate::builder::{FunctionBuilder, IRBuilder};
use crate::graph::Graph;
use crate::node::InitialVnId;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);

pub(crate) struct Region {
    terminated: bool,
    control_node: NodeId,
    memory_node: NodeId,
    /// Advances through Call and CallOther.
    cur_ctrl: ValueId,
    /// Advances through Store, Call, and CallOther.
    cur_memory: ValueId,
    /// Current SSA value per variable. Unset means no definition reaches here.
    variables: SecondaryMap<InitialVnId, PackedOption<ValueId>>,
    /// One `Phi` output per variable with a phi here, in creation order.
    phis: Vec<(InitialVnId, ValueId)>,
}

pub(crate) struct TerminatedRegion {
    pub(crate) control: ValueId,
    pub(crate) memory: ValueId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
    /// Errors if no region is set or the region is already terminated.
    pub(crate) fn require_cur_region(&self) -> Result<RegionId> {
        let region_id = self.cur_region.ok_or_else(|| {
            anyhow!("no current region is set; call set_region or set_entry_region first")
        })?;
        if self.regions[region_id].terminated {
            let id = region_id.as_u32();
            return Err(anyhow!("attempted to insert into terminated region {id}"));
        }
        Ok(region_id)
    }

    pub(crate) fn cur_region_control(&self) -> Result<ValueId> {
        Ok(self.regions[self.require_cur_region()?].cur_ctrl)
    }

    pub(crate) fn cur_region_memory(&self) -> Result<ValueId> {
        Ok(self.regions[self.require_cur_region()?].cur_memory)
    }

    pub(crate) fn advance_cur_region_ctrl(&mut self, ctrl: ValueId) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_ctrl = ctrl;
        Ok(())
    }

    pub fn advance_cur_region_memory(&mut self, memory: ValueId) -> Result<()> {
        require_memory_kind(self.function(), memory)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_memory = memory;
        Ok(())
    }

    pub(crate) fn terminate_cur_region(&mut self) -> Result<TerminatedRegion> {
        let region_id = self.require_cur_region()?;
        let r = &mut self.regions[region_id];
        r.terminated = true;
        let (control, memory) = (r.cur_ctrl, r.cur_memory);
        debug_assert!(self.function().value_kind(control).is_control());
        debug_assert!(self.function().value_kind(memory).is_memory());
        Ok(TerminatedRegion {
            control,
            memory,
            region_id,
        })
    }

    #[inline]
    pub fn set_region(&mut self, region: RegionId) {
        self.cur_region = Some(region);
    }

    /// Appends one operand per `region` phi, taken from `variables`. Operand
    /// order follows the call order, pairing `phi.operand[i]` with `region`'s
    /// i-th control predecessor.
    ///
    /// `variables` is read raw, an absent entry yielding `ValueId(0)`, so the
    /// callers pass the entry region's complete `InitialVar` seed.
    pub(crate) fn link_region_variables(
        &mut self,
        region: RegionId,
        variables: &SecondaryMap<InitialVnId, ValueId>,
    ) -> Result<()> {
        append_phi_operands(self.function.graph_mut(), &self.regions[region].phis, |v| {
            Ok(variables[v])
        })
    }

    /// Mints a `Region` / `MemPhi` pair plus one `Phi` per `phi_vars` entry,
    /// which also seed the region's current-value map.
    pub fn create_region(&mut self, phi_vars: &[InitialVnId]) -> Result<RegionId> {
        let memory_node = self.create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;
        let control_node = self.create_node(
            NodeKind::Region,
            [],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [control, phi_token] = self.function().node_outputs_exact(control_node)?;
        // PhiToken goes in MemPhi.inputs[0] exactly as it does for a Phi.
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, phi_token);

        let mut phis = Vec::with_capacity(phi_vars.len());
        let mut variables = SecondaryMap::new();
        for &vn_id in phi_vars {
            let var = self.function().initial_vn(vn_id);
            let value = self.build_vn_phi(var, phi_token)?;
            phis.push((vn_id, value));
            variables[vn_id] = value.into();
        }

        // Minted two statements up with these exact kinds.
        debug_assert!(self.function().value_kind(memory).is_memory());
        debug_assert!(self.function().value_kind(control).is_control());
        Ok(self.regions.push(Region {
            terminated: false,
            control_node,
            memory_node,
            cur_ctrl: control,
            cur_memory: memory,
            variables,
            phis,
        }))
    }

    /// Seeds `region`'s variables from `idom`'s, then overrides the ones with
    /// a phi placed at `region`. Requires `idom` to be complete, so call in
    /// dominator-tree order.
    pub fn inherit_variables(&mut self, region: RegionId, idom: RegionId) {
        self.seed_variables(region, self.regions[idom].variables.clone());
    }

    /// Sets `region`'s current-value map to `base`, then points each
    /// phi-carrying variable at its own `Phi` output: the value `base` holds
    /// for it is one of the phi's operands, not the region's current value.
    pub(crate) fn seed_variables(
        &mut self,
        region: RegionId,
        base: SecondaryMap<InitialVnId, PackedOption<ValueId>>,
    ) {
        let Region {
            variables, phis, ..
        } = &mut self.regions[region];
        *variables = base;
        for &(var_id, value) in &*phis {
            variables[var_id] = value.into();
        }
    }

    pub(crate) fn write_variable_from_id(
        &mut self,
        var_id: InitialVnId,
        value: ValueId,
    ) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id] = value.into();
        Ok(())
    }

    pub(crate) fn read_variable_from_id(&self, var_id: InitialVnId) -> Result<ValueId> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id]
            .expand()
            .ok_or_else(|| {
                anyhow!(
                    "no definition of variable {var_id:?} reaches region {}",
                    region_id.as_u32()
                )
            })
    }

    /// Appends `control` to `region`'s `Region` and `memory` to its `MemPhi`.
    pub(crate) fn link_edges(&mut self, region: RegionId, control: ValueId, memory: ValueId) {
        let r = &self.regions[region];
        let (control_node, memory_node) = (r.control_node, r.memory_node);
        let graph = self.function.graph_mut();
        graph.add_node_input(control_node, control);
        graph.add_node_input(memory_node, memory);
    }

    pub(crate) fn link_region(
        &mut self,
        region: RegionId,
        control: ValueId,
        memory: ValueId,
        cur_region: RegionId,
    ) -> Result<()> {
        self.link_edges(region, control, memory);
        // `cur_region`'s current variable map, so a self-loop (`cur_region ==
        // region`, an entry that is its own loop header) takes the values
        // reaching the back edge. An unset variable means `cur_region` is one
        // the dominator pre-order rename walk never reached.
        let reaching = &self.regions[cur_region].variables;
        append_phi_operands(
            self.function.graph_mut(),
            &self.regions[region].phis,
            |var_id| {
                reaching[var_id].expand().ok_or_else(|| {
                    anyhow!(
                        "no definition of variable {var_id:?} reaches region {} on its edge \
                         into region {}",
                        cur_region.as_u32(),
                        region.as_u32()
                    )
                })
            },
        )
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) -> Result<()> {
        let (ctrl, mem) = (
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
        );
        self.link_region(child_region, ctrl, mem, parent_region)
    }

    /// The `Control` value the region's terminator consumes.
    pub fn region_cur_ctrl(&self, region: RegionId) -> ValueId {
        self.regions[region].cur_ctrl
    }
}

/// Appends `operand(var)` to each of `phis` in order, stopping at the first
/// error.
fn append_phi_operands(
    graph: &mut Graph,
    phis: &[(InitialVnId, ValueId)],
    mut operand: impl FnMut(InitialVnId) -> Result<ValueId>,
) -> Result<()> {
    for &(var_id, phi_value) in phis {
        let value = operand(var_id)?;
        let phi_node = graph.producer(phi_value);
        graph.add_node_input(phi_node, value);
    }
    Ok(())
}
