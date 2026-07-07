use anyhow::anyhow;
use cranelift_entity::{SecondaryMap, entity_impl};

use crate::IRViewer;
use crate::builder::FunctionBuilder;
use crate::error::Result;
use crate::node::InitialVnId;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

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
///   `Phi` outputs; these receive incoming values as predecessor
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
    variables: SecondaryMap<InitialVnId, ValueId>,
    /// `Phi` outputs — one per **`phi_vars`** variable — that gather incoming
    /// values from predecessor regions (filled in as predecessors are linked).
    /// Entries for variables NOT in `phi_vars` are unset (no phi node exists).
    initial_variables: SecondaryMap<InitialVnId, ValueId>,
    /// The variables that actually have a `Phi` at this region.  The eager
    /// [`FunctionBuilder::create_region`] lists EVERY tracked variable here; the
    /// pruned [`FunctionBuilder::create_region`] lists only the Cytron
    /// IDF-placed variables.  `link_region_variables` iterates exactly this set,
    /// so a non-phi variable is never linked (its value flows in by dominator
    /// inheritance instead — see [`FunctionBuilder::inherit_variables`]).
    phi_vars: Vec<InitialVnId>,
}

/// The result of terminating the current region: the final control and memory
/// tokens, plus the region id (needed to link successors).
pub(crate) struct TerminatedRegion {
    pub(crate) control: ValueId,
    pub(crate) memory: ValueId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
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
    /// `crates/strider-lift/src/lift/insn/mod.rs`
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

    /// Adds each predecessor value from `variables` to the matching `Phi` of
    /// `region` — but only for the variables that actually HAVE a phi there
    /// (`region.phi_vars`).  Iterating the target's phi set (not the source
    /// map's keys) is what makes the pruned path correct: a variable with no
    /// phi at `region` is simply skipped (its value reaches by dominator
    /// inheritance, not by a phi operand).  Operand order follows the caller's
    /// per-edge order, so `phi.operand[i]` stays paired with `region`'s
    /// `i`-th control predecessor.
    pub(crate) fn link_region_variables(
        &mut self,
        region: RegionId,
        variables: &SecondaryMap<InitialVnId, ValueId>,
    ) -> Result<()> {
        // Clone the small phi-var list so the `&mut self` graph edits below
        // don't conflict with the borrow of `self.regions[region]`.
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

    /// Creates a new region: a fresh `Region` + `MemPhi` skeleton and one value
    /// `Phi` per variable in `phi_vars`, wires the `MemPhi`'s `phi_token`
    /// back-edge to its `Region`, and registers the region.  `phi_vars` is the
    /// Cytron IDF-placed set for a join (production), or every tracked varnode
    /// for an ad-hoc build (the `strider-ir-test-utils` `create_region_all`
    /// convenience).
    ///
    /// The freshly-built phis are recorded as the region's current variable
    /// values, so a `read` in the region resolves to its `Phi` immediately.  The
    /// pruned-SSA lift path then OVERWRITES this current-value map in
    /// dominator-tree order via [`Self::inherit_variables`] (a placed variable
    /// takes its phi; a non-placed one inherits the immediate dominator's
    /// reaching value), so the seeding is transparent to production and only
    /// load-bearing for callers that build a graph WITHOUT running the
    /// inheritance walk (i.e. tests).
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly created `Region` / `MemPhi`
    /// lacks its expected output shape (a graph-construction bug, not a user
    /// error).  Other variants from `build_vn_phi` propagate.
    pub fn create_region(&mut self, phi_vars: &[InitialVnId]) -> Result<RegionId> {
        // Skeleton: a `MemPhi` for the memory token and a `Region` for control.
        let memory_node = self.create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;
        let control_node = self.create_node(
            NodeKind::Region,
            [],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [control, phi_token] = self.function().node_outputs_exact(control_node)?;
        // Wire the PhiToken as MemPhi.inputs[0] (mirrors how Phi nodes link), a
        // back-reference so dead-branch / redundant-phi passes treat MemPhi and
        // Phi identically.
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, phi_token);

        // One value `Phi` per placed variable; `variables` (the current-value
        // map) is either seeded from those phis (eager) or left for inheritance.
        let mut initial_variables = SecondaryMap::new();
        for &vn_id in phi_vars {
            let var = self.function().initial_vn(vn_id);
            initial_variables[vn_id] = self.build_vn_phi(var, phi_token, &[])?;
        }
        // Seed the current-value map with the fresh phis (see the doc: the
        // pruned lift path overwrites this via `inherit_variables`).
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

    /// Fills `region`'s current-value map for the pruned path: every variable
    /// starts at its reaching value from the immediate dominator `idom` (which
    /// is fully processed first, in dom-tree order), then each of `region`'s
    /// own placed phis overrides that variable.  After this, processing
    /// `region`'s instructions reads/writes against `variables` exactly as the
    /// eager path does.
    pub fn inherit_variables(&mut self, region: RegionId, idom: RegionId) {
        let mut variables = self.regions[idom].variables.clone();
        let phi_vars = self.regions[region].phi_vars.clone();
        for var_id in phi_vars {
            variables[var_id] = self.regions[region].initial_variables[var_id];
        }
        self.regions[region].variables = variables;
    }

    /// Directly seeds `region`'s current-value map — the pruned entry setup,
    /// where the entry region has no value phis so its `InitialVar` values ARE
    /// its variable values (the root every dominated region inherits from).
    pub(crate) fn set_region_variables(
        &mut self,
        region: RegionId,
        variables: SecondaryMap<InitialVnId, ValueId>,
    ) {
        self.regions[region].variables = variables;
    }

    /// Writes `value` to variable `var_id` in the active region.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active.
    pub fn write_variable_from_id(&mut self, var_id: InitialVnId, value: ValueId) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id] = value;
        Ok(())
    }

    /// Reads the current value of variable `var_id` from the active region.
    pub(crate) fn read_variable_from_id(&self, var_id: InitialVnId) -> Result<ValueId> {
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
        self.function_mut()
            .graph_mut()
            .add_node_input(control_node, control);
        Ok(())
    }

    /// Adds `memory` as an incoming memory edge to `region`'s `MemPhi` node.
    pub(crate) fn link_memory_regions(&mut self, region: RegionId, memory: ValueId) -> Result<()> {
        self.require_memory_kind(memory)?;
        let memory_node = self.regions[region].memory_node;
        self.function_mut()
            .graph_mut()
            .add_node_input(memory_node, memory);
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
    /// `Control` `ValueId` consumed by the region's terminator.  Used only by
    /// tests to wire up synthetic graphs.
    #[cfg(any(test, feature = "test-util"))]
    pub fn region_cur_ctrl(&self, region: RegionId) -> ValueId {
        self.regions[region].cur_ctrl
    }
}
