use anyhow::anyhow;
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
    /// Advances through calls.
    cur_ctrl: ValueId,
    /// Advances through stores and calls.
    cur_memory: ValueId,
    variables: SecondaryMap<InitialVnId, ValueId>,
    /// One `Phi` output per `phi_vars` entry, gathering incoming values as
    /// predecessors are linked. Entries outside `phi_vars` are unset.
    initial_variables: SecondaryMap<InitialVnId, ValueId>,
    /// Variables that actually have a `Phi` here: every tracked variable on
    /// the eager path, only the IDF-placed ones on the pruned path.
    /// `link_region_variables` iterates exactly this set, so a non-phi
    /// variable is never linked; its value arrives by dominator inheritance
    /// (see [`FunctionBuilder::inherit_variables`]).
    phi_vars: Vec<InitialVnId>,
}

pub(crate) struct TerminatedRegion {
    pub(crate) control: ValueId,
    pub(crate) memory: ValueId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
    /// One call site for the pair, so a terminator-producing method cannot
    /// check one edge kind and forget the other.
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

    /// `pub` rather than `pub(crate)` for the lifter, which advances memory
    /// after a `build_call_other` whose ABI sets `clobbers_memory`.
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

    /// Iterating the TARGET's phi set rather than the source map's keys is
    /// what makes the pruned path correct: a variable with no phi at `region`
    /// is skipped, since its value reaches by dominator inheritance instead of
    /// a phi operand. Operand order follows the caller's per-edge order, so
    /// `phi.operand[i]` stays paired with `region`'s i-th control predecessor.
    pub(crate) fn link_region_variables(
        &mut self,
        region: RegionId,
        variables: &SecondaryMap<InitialVnId, ValueId>,
    ) -> Result<()> {
        // Cloned so the graph edits below don't hold a borrow of
        // `self.regions[region]`.
        let phi_vars = self.regions[region].phi_vars.clone();
        for var_id in phi_vars {
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.function().producer(region_variable_output_id);
            let current_variable = variables[var_id];
            self.function_mut()
                .graph_mut()
                .add_node_input(region_variable_id, current_variable);
        }
        Ok(())
    }

    /// `phi_vars` is the IDF-placed set on the lift path, or every tracked
    /// varnode for an ad-hoc build (`create_region_all`).
    ///
    /// The fresh phis also seed the region's current-value map. The lift path
    /// overwrites that map in dominator-tree order via
    /// [`Self::inherit_variables`], so the seeding matters only to callers
    /// that build a graph without running the inheritance walk (tests).
    pub fn create_region(&mut self, phi_vars: &[InitialVnId]) -> Result<RegionId> {
        let memory_node = self.create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;
        let control_node = self.create_node(
            NodeKind::Region,
            [],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [control, phi_token] = self.function().node_outputs_exact(control_node)?;
        // PhiToken goes in MemPhi.inputs[0] exactly as it does for a Phi, so
        // dead-branch and redundant-phi passes can treat the two alike.
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, phi_token);

        let mut initial_variables = SecondaryMap::new();
        for &vn_id in phi_vars {
            let var = self.function().initial_vn(vn_id);
            initial_variables[vn_id] = self.build_vn_phi(var, phi_token, &[])?;
        }
        let variables = initial_variables.clone();

        self.require_memory_kind(memory)?;
        self.require_control_kind(control)?;
        Ok(self.regions.push(Region {
            terminated: false,
            control_node,
            memory_node,
            cur_ctrl: control,
            cur_memory: memory,
            variables,
            initial_variables,
            phi_vars: phi_vars.to_vec(),
        }))
    }

    /// Every variable starts at its reaching value from the immediate
    /// dominator, which dom-tree order guarantees is already complete, then
    /// `region`'s own placed phis override their variables.
    pub fn inherit_variables(&mut self, region: RegionId, idom: RegionId) {
        let mut variables = self.regions[idom].variables.clone();
        let phi_vars = self.regions[region].phi_vars.clone();
        for var_id in phi_vars {
            variables[var_id] = self.regions[region].initial_variables[var_id];
        }
        self.regions[region].variables = variables;
    }

    /// Entry setup: the entry region has no value phis, so its `InitialVar`
    /// values are its variable values, the root everything else inherits from.
    pub(crate) fn set_region_variables(
        &mut self,
        region: RegionId,
        variables: SecondaryMap<InitialVnId, ValueId>,
    ) {
        self.regions[region].variables = variables;
    }

    pub fn write_variable_from_id(&mut self, var_id: InitialVnId, value: ValueId) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id] = value;
        Ok(())
    }

    pub(crate) fn read_variable_from_id(&self, var_id: InitialVnId) -> Result<ValueId> {
        let region_id = self.require_cur_region()?;
        Ok(self.regions[region_id].variables[var_id])
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
        // Take-and-restore instead of cloning, saving an O(num_vars) alloc
        // per link: `link_region_variables` only adds phi inputs, it never
        // reads this map. Every call site has `cur_region != region`, so the
        // temporarily empty slot is never observed.
        let source = std::mem::take(&mut self.regions[cur_region].variables);
        let res = self.link_region_variables(region, &source);
        self.regions[cur_region].variables = source;
        res?;
        Ok(())
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) -> Result<()> {
        let (ctrl, mem) = (
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
        );
        self.link_region(child_region, ctrl, mem, parent_region)
    }

    /// The `Control` value the region's terminator consumes. Tests only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn region_cur_ctrl(&self, region: RegionId) -> ValueId {
        self.regions[region].cur_ctrl
    }
}
