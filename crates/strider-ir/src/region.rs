use anyhow::anyhow;
use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{SecondaryMap, entity_impl};

use crate::IRViewer;
use crate::builder::FunctionBuilder;
use crate::error::Result;
use crate::node::InitialVnId;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Errors unless `res`'s control and memory edges both have their expected
    /// kind.
    pub(crate) fn require_terminator_kinds(&self, res: &TerminatedRegion) -> Result<()> {
        self.require_control_kind(res.control)?;
        self.require_memory_kind(res.memory)
    }

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
        self.require_control_kind(ctrl)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_ctrl = ctrl;
        Ok(())
    }

    pub fn advance_cur_region_memory(&mut self, memory: ValueId) -> Result<()> {
        self.require_memory_kind(memory)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_memory = memory;
        Ok(())
    }

    pub(crate) fn terminate_cur_region(&mut self) -> Result<TerminatedRegion> {
        let region_id = self.require_cur_region()?;
        let control = self.regions[region_id].cur_ctrl;
        let memory = self.regions[region_id].cur_memory;
        self.regions[region_id].terminated = true;
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
        let operands: Vec<ValueId> = self.regions[region]
            .phis
            .iter()
            .map(|&(var_id, _)| variables[var_id])
            .collect();
        self.append_phi_operands(region, &operands)
    }

    /// One operand per `region` phi, read out of `source`'s current values.
    ///
    /// Errors on a variable `source` has no value for, which is a region the
    /// rename walk never reached: `translate_regions` walks the dominator
    /// pre-order, which excludes the regions unreachable from the entry, while
    /// `link_region_edges` walks every CFG edge.
    fn reaching_phi_operands(&self, region: RegionId, source: RegionId) -> Result<Vec<ValueId>> {
        self.regions[region]
            .phis
            .iter()
            .map(|&(var_id, _)| {
                self.regions[source].variables[var_id]
                    .expand()
                    .ok_or_else(|| {
                        anyhow!(
                            "no definition of variable {var_id:?} reaches region {} on its edge \
                             into region {}",
                            source.as_u32(),
                            region.as_u32()
                        )
                    })
            })
            .collect()
    }

    fn append_phi_operands(&mut self, region: RegionId, operands: &[ValueId]) -> Result<()> {
        let phis = self.regions[region].phis.len();
        if operands.len() != phis {
            return Err(anyhow!(
                "region {} carries {phis} phis but the incoming edge supplies {} operands",
                region.as_u32(),
                operands.len()
            ));
        }
        for (i, &operand) in operands.iter().enumerate() {
            let phi_value = self.regions[region].phis[i].1;
            let phi_node = self.function().producer(phi_value);
            self.function_mut()
                .graph_mut()
                .add_node_input(phi_node, operand);
        }
        Ok(())
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
            let value = self.build_vn_phi(var, phi_token, &[])?;
            phis.push((vn_id, value));
            variables[vn_id] = value.into();
        }

        self.require_memory_kind(memory)?;
        self.require_control_kind(control)?;
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
        let mut variables = self.regions[idom].variables.clone();
        for &(var_id, value) in &self.regions[region].phis {
            variables[var_id] = value.into();
        }
        self.regions[region].variables = variables;
    }

    /// Overwrites `region`'s current-value map wholesale.
    pub(crate) fn set_region_variables(
        &mut self,
        region: RegionId,
        variables: SecondaryMap<InitialVnId, ValueId>,
    ) {
        let mut packed = SecondaryMap::new();
        for (var_id, &value) in variables.iter() {
            packed[var_id] = value.into();
        }
        self.regions[region].variables = packed;
    }

    /// Repoints every phi-carrying variable's current value at its own `Phi`
    /// output, undoing a blanket [`Self::set_region_variables`]: the value that
    /// seed supplied is one of the phi's operands, not the region's current
    /// value.
    pub(crate) fn seed_phi_vars_from_phis(&mut self, region: RegionId) {
        let r = &mut self.regions[region];
        for i in 0..r.phis.len() {
            let (var_id, value) = r.phis[i];
            r.variables[var_id] = value.into();
        }
    }

    pub fn write_variable_from_id(&mut self, var_id: InitialVnId, value: ValueId) -> Result<()> {
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

    pub(crate) fn link_control_regions(
        &mut self,
        region: RegionId,
        control: ValueId,
    ) -> Result<()> {
        self.require_control_kind(control)?;
        let control_node = self.regions[region].control_node;
        self.function_mut()
            .graph_mut()
            .add_node_input(control_node, control);
        Ok(())
    }

    pub(crate) fn link_memory_regions(&mut self, region: RegionId, memory: ValueId) -> Result<()> {
        self.require_memory_kind(memory)?;
        let memory_node = self.regions[region].memory_node;
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, memory);
        Ok(())
    }

    pub(crate) fn link_region(
        &mut self,
        region: RegionId,
        control: ValueId,
        memory: ValueId,
        cur_region: RegionId,
    ) -> Result<()> {
        self.link_control_regions(region, control)?;
        self.link_memory_regions(region, memory)?;
        // `cur_region`'s current variable map, so a self-loop (`cur_region ==
        // region`, an entry that is its own loop header) takes the values
        // reaching the back edge.
        let operands = self.reaching_phi_operands(region, cur_region)?;
        self.append_phi_operands(region, &operands)
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
