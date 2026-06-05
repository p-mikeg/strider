use anyhow::anyhow;
use cranelift_entity::{SecondaryMap, entity_impl};

use crate::builder::FunctionBuilder;
use crate::builder::VarId;
use crate::error::Result;
use crate::node::{NodeId, ValueId};

/// A unique identifier for a basic-block region in the IR graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);

/// All state associated with a single basic-block region.
///
/// A region owns:
/// - A `Region` node (and its output) that acts as the region header.
/// - A `MemPhi` node (and its output) that selects the memory token at the join.
/// - A current variable map (`variables`) that is updated by writes.
/// - An initial variable map (`initial_variables`) recording the
///   `VarPhi` outputs; these receive incoming values as predecessor
///   regions are linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Region {
    /// `true` once a terminator (branch / return) has been emitted.
    terminated: bool,
    /// The `Region` node that represents the entry of this region.
    control_node: NodeId,
    /// The `MemPhi` node that selects the memory token for this region.
    memory_node: NodeId,
    /// The current control edge inside this region (advances through calls).
    cur_ctrl: ValueId,
    /// The current memory token inside this region (advances through stores/calls).
    cur_memory: ValueId,
    /// Current SSA value of each variable in this region.
    variables: SecondaryMap<VarId, ValueId>,
    /// `VarPhi` outputs — one per variable — that gather incoming values
    /// from predecessor regions (filled in as predecessors are linked).
    initial_variables: SecondaryMap<VarId, ValueId>,
}

/// The result of terminating the current region: the final control and memory
/// tokens, plus the region id (needed to link successors).
pub(crate) struct TerminatedRegion {
    pub(crate) control: ValueId,
    pub(crate) memory: ValueId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
    /// Returns `Ok(())` if `value` has `Control` kind; otherwise an error.
    pub(crate) fn require_control_kind(&self, value: ValueId) -> Result<()> {
        let kind = self.function().value_kind(value);
        if !kind.is_control() {
            return Err(anyhow!(
                "output {value:?} is not a control edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Returns `Ok(())` if `value` has `Memory` kind; otherwise an error.
    pub(crate) fn require_memory_kind(&self, value: ValueId) -> Result<()> {
        let kind = self.function().value_kind(value);
        if !kind.is_memory() {
            return Err(anyhow!(
                "output {value:?} is not a memory edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Validates that `res.control` and `res.memory` are control/memory
    /// edges respectively — the documented invariant for any
    /// terminator-producing builder method (Return / Branch /
    /// IndirectBranch / CondBranch / CallOther-terminal).  Single
    /// callsite for the (control + memory) pair so a typo can't drop
    /// one of the two checks.
    pub(crate) fn require_terminator_kinds(&self, res: &TerminatedRegion) -> Result<()> {
        self.require_control_kind(res.control)?;
        self.require_memory_kind(res.memory)
    }

    /// Returns the id of the current region, or an error if no region is set
    /// or if the region has already been terminated.
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

    /// Returns the current control-flow edge of the active region.
    pub(crate) fn cur_region_control(&self) -> Result<ValueId> {
        Ok(self.regions[self.require_cur_region()?].cur_ctrl)
    }

    /// Returns the current memory token of the active region.
    pub(crate) fn cur_region_memory(&self) -> Result<ValueId> {
        Ok(self.regions[self.require_cur_region()?].cur_memory)
    }

    /// Advances the control edge of the active region to `ctrl`.
    pub(crate) fn advance_cur_region_ctrl(&mut self, ctrl: ValueId) -> Result<()> {
        self.require_control_kind(ctrl)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_ctrl = ctrl;
        Ok(())
    }

    /// Advances the memory token of the active region to `memory`.
    ///
    /// `pub` (rather than `pub(crate)`) so the strider layer can advance
    /// memory after a `build_call_other` call whose ABI's
    /// `clobbers_memory` flag is set — see
    /// `crates/strider-orchestrator/src/strider/insn/mod.rs`
    /// `handle_call_other`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active and
    /// `WrongOutputKind` when `memory` is not a `Memory` edge.
    pub fn advance_cur_region_memory(&mut self, memory: ValueId) -> Result<()> {
        self.require_memory_kind(memory)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_memory = memory;
        Ok(())
    }

    /// Marks the active region as terminated without emitting a
    /// separate terminator node.  Called internally by
    /// [`crate::builder::call::FunctionBuilder::build_call_kind`] when
    /// `terminate = true` (the `NoReturn`-class `CallOther` path) so
    /// that the CallOther node itself acts as the region exit.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active.
    pub(crate) fn mark_cur_region_terminated(&mut self) -> Result<()> {
        self.terminate_cur_region().map(|_| ())
    }

    /// Marks the active region as terminated and returns its final control
    /// and memory tokens.
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

    /// Sets the active region to `region`.
    #[inline]
    pub fn set_region(&mut self, region: RegionId) {
        self.cur_region = Some(region);
    }

    /// Adds incoming variable values from `variables` to the `VarPhi`
    /// nodes of `region`.
    pub(crate) fn link_region_variables(
        &mut self,
        region: RegionId,
        variables: &SecondaryMap<VarId, ValueId>,
    ) -> Result<()> {
        for var_id in variables.keys() {
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.function().producer(region_variable_output_id);
            let current_variable = variables[var_id];
            self.function_mut()
                .graph_mut()
                .add_node_input(region_variable_id, current_variable);
        }
        Ok(())
    }

    /// Allocates a new [`Region`] entry and registers it in the region map.
    pub(crate) fn create_region_helper(
        &mut self,
        control_node: NodeId,
        control_id: ValueId,
        memory_node: NodeId,
        memory_id: ValueId,
        initial_variables: SecondaryMap<VarId, ValueId>,
    ) -> Result<RegionId> {
        self.require_memory_kind(memory_id)?;
        self.require_control_kind(control_id)?;
        Ok(self.regions.push(Region {
            terminated: false,
            control_node,
            memory_node,
            cur_ctrl: control_id,
            cur_memory: memory_id,
            variables: initial_variables.clone(),
            initial_variables,
        }))
    }

    /// Writes `value` to variable `var_id` in the active region.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active.
    pub fn write_variable_from_id(&mut self, var_id: VarId, value: ValueId) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id] = value;
        Ok(())
    }

    /// Reads the current value of variable `var_id` from the active region.
    pub(crate) fn read_variable_from_id(&self, var_id: VarId) -> Result<ValueId> {
        let region_id = self.require_cur_region()?;
        Ok(self.regions[region_id].variables[var_id])
    }

    /// Adds `control` as an incoming control edge to `region`'s `Region` node.
    pub(crate) fn link_control_regions(
        &mut self,
        region: RegionId,
        control: ValueId,
    ) -> Result<()> {
        self.require_control_kind(control)?;
        let control_node = self.regions[region].control_node;
        self.function_mut().graph_mut().add_node_input(control_node, control);
        Ok(())
    }

    /// Adds `memory` as an incoming memory edge to `region`'s `MemPhi` node.
    pub(crate) fn link_memory_regions(
        &mut self,
        region: RegionId,
        memory: ValueId,
    ) -> Result<()> {
        self.require_memory_kind(memory)?;
        let memory_node = self.regions[region].memory_node;
        self.function_mut().graph_mut().add_node_input(memory_node, memory);
        Ok(())
    }

    /// Links `region` as a successor of `cur_region`.
    pub(crate) fn link_region(
        &mut self,
        region: RegionId,
        control: ValueId,
        memory: ValueId,
        cur_region: RegionId,
    ) -> Result<()> {
        self.link_control_regions(region, control)?;
        self.link_memory_regions(region, memory)?;
        // Avoid cloning `cur_region.variables`: `link_region_variables`
        // doesn't mutate the `variables` map (it only adds inputs to
        // `region.initial_variables` phi nodes via `graph_mut`).  Use
        // `mem::take` to move the map out, link against the borrowed
        // map, then restore — saves an `O(num_vars)` allocation per
        // region link on functions with many tracked variables.
        // `cur_region != region` is a structural invariant of every
        // call site (a region can't be its own successor's predecessor
        // via this path), so the temporary empty slot is never read.
        let source = std::mem::take(&mut self.regions[cur_region].variables);
        let res = self.link_region_variables(region, &source);
        self.regions[cur_region].variables = source;
        res?;
        Ok(())
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    ///
    /// # Errors
    ///
    /// Propagates the variants from `link_region` —
    /// `ExpectedControl` / `ExpectedMemory`
    /// when `parent_region`'s snapshotted edges are mistyped, plus any
    /// `add_node_input` errors when wiring per-variable phi inputs.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) -> Result<()> {
        let (ctrl, mem) = (
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
        );
        self.link_region(child_region, ctrl, mem, parent_region)
    }

    /// Returns the current control-output of `region` — i.e. the
    /// `Control` `ValueId` consumed by the region's terminator.
    /// At cache-population time this is the region's exit control.
    pub fn region_cur_ctrl(&self, region: RegionId) -> ValueId {
        self.regions[region].cur_ctrl
    }

    /// Returns an iterator over `(VarId, ValueId)` pairs for
    /// `region`'s exit-boundary variable values — the value of each
    /// tracked variable at the region's terminator.  Used by the
    /// cache to populate `exit_vn_to_value`.
    pub fn region_exit_variables(
        &self,
        region: RegionId,
    ) -> impl Iterator<Item = (VarId, ValueId)> + '_ {
        self.regions[region]
            .variables
            .iter()
            .map(|(var_id, &value)| (var_id, value))
    }
}
