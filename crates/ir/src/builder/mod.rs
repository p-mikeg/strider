use anyhow::anyhow;
use cranelift_entity::{PrimaryMap, entity_impl};
use std::collections::HashMap;

use crate::error::Result;
use crate::function::FunctionGraph;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::region::Region;

mod call;
mod coerce;
mod nodes;
#[cfg(test)]
mod tests;
mod vars;

/// A dense, typed identifier for a tracked variable (varnode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(u32);
entity_impl!(VarId);

/// Returns `true` for varnode spaces whose offsets are addressed as fixed
/// byte ranges (REGISTER, UNIQUE).  CONST and code-space varnodes don't
/// behave like fixed-offset registers, so containment-by-offset is
/// meaningless there.
fn is_aliasable_space(s: rsleigh::VnSpace) -> bool {
    s == rsleigh::VnSpace::REGISTER || s == rsleigh::VnSpace::UNIQUE
}

/// Maps a calling-convention varnode `vn` to a tracked variable in
/// `variable_to_id`.  Returns the input verbatim if it's already tracked;
/// otherwise tries two fallbacks in order:
///
/// 1. **Cover** — the smallest tracked variable in the same space whose
///    byte range fully covers `vn`.  Useful when the function uses a
///    wider view of the same physical register (e.g. MIPS-O32 lists `f0`
///    as 4-byte but a `double`-returning function writes the 8-byte
///    combined `f0/f1` view).
/// 2. **Contained-in sub-register** — when no cover exists, the LARGEST
///    tracked variable in the same space whose byte range is fully
///    contained in `vn`'s range.  Useful when the function reads only a
///    sub-register (e.g. x86_64 SysV passes `int a` in `RDI`, but the
///    callee only reads `EDI` — the 4-byte sub-register is what the
///    function actually consumed, so it's safe to use as the
///    arg-passing-var).  Bigger sub-registers win because they preserve
///    more information about the value.
///
/// Returns `None` for non-aliasable spaces (CONST, code) or when no
/// tracked variable overlaps `vn` at all.
fn upgrade_to_tracked_for(
    variable_to_id: &HashMap<rsleigh::Vn, VarId>,
    vn: rsleigh::Vn,
) -> Option<rsleigh::Vn> {
    if variable_to_id.contains_key(&vn) {
        return Some(vn);
    }
    if !is_aliasable_space(vn.addr_space) {
        return None;
    }
    let vn_end = vn.addr_off + vn.size as u64;

    // Smallest tracked container that COVERS vn (existing behaviour).
    if let Some(cover) = variable_to_id
        .keys()
        .filter(|t| {
            t.addr_space == vn.addr_space
                && t.addr_off <= vn.addr_off
                && t.addr_off + t.size as u64 >= vn_end
        })
        .min_by_key(|t| t.size)
        .copied()
    {
        return Some(cover);
    }

    // Sub-register fallback: largest tracked variable CONTAINED IN vn's
    // byte range - the function only reads that sub-register, so the
    // bytes outside its range are unused.
    variable_to_id
        .keys()
        .filter(|t| {
            t.addr_space == vn.addr_space
                && t.addr_off >= vn.addr_off
                && t.addr_off + t.size as u64 <= vn_end
        })
        .max_by_key(|t| t.size)
        .copied()
}

/// Incrementally constructs a sea-of-nodes IR function graph.
///
/// The builder tracks SSA-style per-region variable state: each variable has
/// exactly one current `NodeOutputId` inside the active region.  Reads and
/// writes go through this mapping so that the graph is always in a consistent
/// state.
pub struct FunctionBuilder {
    pub(crate) function: FunctionGraph,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub(crate) variable_to_id: HashMap<rsleigh::Vn, VarId>,
    /// Variables clobbered by any call instruction (everything not
    /// callee-saved, and excluding the stack pointer which is rebound
    /// separately with the `ret_stack_pop` adjust).
    pub(crate) call_clobbered_variables: Vec<rsleigh::Vn>,
    /// Variables used to pass arguments according to the calling convention.
    pub(crate) arg_passing_vars: Vec<rsleigh::Vn>,
    /// Varnodes used to return values according to the calling convention,
    /// in ABI order (e.g. `[rax, rdx]` on x86_64).  The first `ret_val_vars.len()`
    /// value-typed outputs of every `Call` (output indices 2..) correspond to
    /// these varnodes in order; `Return` input slots 2.. correspond to these
    /// varnodes in order.
    pub(crate) ret_val_vars: Vec<rsleigh::Vn>,
    /// Stack pointer varnode — when present, it is excluded from the
    /// `call_clobbered_variables` set and rebound at every `Call` to
    /// `Add(pre_call_sp, IntConst(ret_stack_pop))`.  `None` in synthetic
    /// tests that don't model stack-aware calling conventions.
    pub(crate) stack_ptr_vn: Option<rsleigh::Vn>,
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  0 on link-register ISAs, pointer size on stack-push ISAs.
    /// Ignored when `stack_ptr_vn` is `None`.
    pub(crate) ret_stack_pop: i64,
    /// Lazy `tracked_vn → its largest containing tracked-vn` map.
    /// Populated on first call to [`Self::largest_container_for`];
    /// the variable set is fixed at construction so caching is safe.
    /// Lookup turns the per-call O(V) linear scan in
    /// `pcode_lift::find_largest_fitting_register` into O(1).
    pub(crate) largest_container: std::cell::OnceCell<HashMap<rsleigh::Vn, rsleigh::Vn>>,
    /// Asm-instruction address attributed to every node `create_node`
    /// produces while this is `Some`.  The lifter / strider region driver
    /// sets it to `Some(addr)` immediately before each pcode insn (see
    /// [`Self::set_lift_addr`]) and back to `None` between insns.
    /// Region-setup helpers (`build_entry`, `build_function_args`,
    /// region/phi creation) leave it `None`, so synthesised structural
    /// nodes legitimately stay empty in the fingerprint side-table.
    pub(crate) lift_addr: Option<u64>,
}

impl FunctionBuilder {
    /// Returns a reference to the underlying [`FunctionGraph`].
    #[must_use] 
    pub fn body(&self) -> &FunctionGraph {
        &self.function
    }

    /// Returns a mutable reference to the underlying [`FunctionGraph`].
    pub fn body_mut(&mut self) -> &mut FunctionGraph {
        &mut self.function
    }

    pub(crate) fn graph(&self) -> &Graph {
        &self.body().graph
    }

    /// Returns a mutable reference to the underlying [`Graph`] without
    /// consuming the builder.
    ///
    /// This is the primary entry point for in-place graph mutation (e.g.
    /// running an `opt::Optimizer` pass on a builder that we still want to
    /// use afterwards). The returned reference borrows the same `Graph` as
    /// [`Self::body`] / [`Self::body_mut`], so any mutations are immediately
    /// visible through every accessor. Pairs with [`Self::entry`]: opt
    /// passes need `(graph, entry)` together because `entry` anchors the
    /// reachable-node walk the validator's Layer A is scoped to.
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.function.graph
    }

    /// Returns the recorded entry [`NodeId`] of the function being
    /// built — the same id that [`Self::build`] would copy into the
    /// produced [`crate::function::BuiltFunctionGraph`].
    ///
    /// CORRECTNESS — pairs with [`Self::graph_mut`]: opt passes that
    /// take `(graph, entry)` get a stable handle here.  The entry node
    /// id never changes once the builder's first region is registered,
    /// so callers may cache it across iterations.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.function.entry
    }

    pub(super) fn validate_value_inputs(&self, inputs: &[NodeOutputId]) -> Result<()> {
        for &v in inputs {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(anyhow!("output {v:?} is not a value edge (got {kind:?})"));
            }
        }
        Ok(())
    }

    /// Creates a new [`FunctionBuilder`] from a resolved calling convention.
    ///
    /// `all_used_variables` is the complete set of varnodes (registers /
    /// unique temporaries) that appear in the function.  The convention
    /// supplies the argument-passing, callee-saved, and stack-pointer sets;
    /// every variable not callee-saved (and not SP) is recorded as
    /// call-clobbered; SP is rebound at each call site via an explicit
    /// `Add(sp, ret_stack_pop)` node.
    ///
    /// # Errors
    ///
    /// Propagates whatever [`Self::new_raw`] would return — currently
    /// `UnsupportedOutputSize` from the entry-block setup when
    /// a tracked variable's byte size has no matching `NodeOutputType`.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: &target::BuiltCallingConvention,
    ) -> Result<Self> {
        // Ensure all return registers (int + float) are tracked variables.
        // This keeps the data-flow chain from a float operation's output
        // (e.g. an aarch64 FloatAdd writes to s0, the 4-byte sub-register of q0)
        // connected to the Return node — without this step `q0` would not be
        // in the variable set, and the analyzer's register-aliasing logic
        // would never widen the s0 write into a q0 store visible to Return.
        for v in cc.ret_val_regs.iter().chain(cc.ret_val_regs_float.iter()) {
            if !all_used_variables.contains(v) {
                all_used_variables.push(*v);
            }
        }
        // Union of int + float return registers, in that order.  Pattern
        // queries that index `ret_val(0)` continue to find the first integer
        // ret slot; new queries can use `ret_val(N)` where N >= int-count to
        // reach float ret slots.
        let mut combined_ret_vars: Vec<rsleigh::Vn> = Vec::with_capacity(
            cc.ret_val_regs.len() + cc.ret_val_regs_float.len(),
        );
        combined_ret_vars.extend(cc.ret_val_regs.iter().copied());
        combined_ret_vars.extend(cc.ret_val_regs_float.iter().copied());
        Self::new_raw(
            all_used_variables,
            &cc.arg_passing_regs,
            &cc.callee_saved_regs,
            &combined_ret_vars,
            Some(cc.stack_ptr_vn),
            cc.ret_stack_pop,
        )
    }

    /// Builds an "empty" function: no tracked variables, no calling-convention
    /// plumbing, no stack pointer, no ret-stack-pop.  Convenience for tests
    /// and small synthetic IRs.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new_raw`] (currently never produces an error for the
    /// empty input set, but `Result` is preserved for forward-compatibility).
    pub fn empty() -> Result<Self> {
        Self::new_raw(vec![], &[], &[], &[], None, 0)
    }

    /// Low-level constructor that takes the convention-derived data as
    /// unpacked slices.  Used by synthetic tests that don't resolve a real
    /// calling convention against a Sleigh register table — production code
    /// should use [`FunctionBuilder::new`] with a [`target::BuiltCallingConvention`].
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedOutputSize` when any tracked variable
    /// has a byte size with no matching `NodeOutputType` (the entry-block
    /// builder allocates an `InitialVar` per tracked variable).
    pub fn new_raw(
        all_used_variables: Vec<rsleigh::Vn>,
        arg_passing_vars: &[rsleigh::Vn],
        callee_saved_vars: &[rsleigh::Vn],
        ret_vars: &[rsleigh::Vn],
        stack_ptr_vn: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
    ) -> Result<Self> {
        // For overlapping varnodes in the same fixed-offset space, keep only
        // the largest enclosing one.  E.g. if both `rdi` and `edi` are
        // touched, drop `edi`.  Same applies to UNIQUE space — Sleigh's
        // MIPS lifter writes a 64-bit IntMul result to a unique varnode
        // and Copies a 4-byte slice of it to a register; without the filter
        // the 4-byte and 8-byte unique varnodes are treated as independent
        // SSA variables (MIPS MULT writes a 64-bit unique then a Copy
        // reads a narrow slice; the overlap filter keeps the wider
        // varnode).
        //
        // CONST and code-space varnodes don't behave like fixed-offset
        // registers — they're addressed by literal value or runtime address,
        // so containment-by-offset is meaningless there.
        let all_variables: Vec<_> = all_used_variables
            .iter()
            .filter(|v| {
                if !is_aliasable_space(v.addr_space) {
                    return true;
                }
                !all_used_variables.iter().any(|other| {
                    other != *v
                        && other.addr_space == v.addr_space
                        && other.addr_off <= v.addr_off
                        && other.addr_off + other.size as u64 >= v.addr_off + v.size as u64
                        && other.size > v.size
                })
            })
            .copied()
            .collect();
        // `call_clobbered_variables` is emitted as the Call node's value
        // outputs in order (slot `i + 2` ↔ `call_clobbered_variables[i]`).
        // Front-load it with the calling convention's return registers so
        // `.ret_output(0)` indexes into ABI ret slot 0 (e.g. rax on x86_64),
        // then append the remaining caller-clobbered registers.
        let call_clobbered_variables: Vec<_> = {
            let is_clobbered = |v: &rsleigh::Vn| {
                !callee_saved_vars.contains(v) && Some(*v) != stack_ptr_vn
            };
            let ret_prefix = ret_vars
                .iter()
                .copied()
                .filter(|v| all_variables.contains(v) && is_clobbered(v));
            let rest = all_variables
                .iter()
                .filter(|v| is_clobbered(v) && !ret_vars.contains(v))
                .copied();
            ret_prefix.chain(rest).collect()
        };
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        // For arg-passing and ret-val regs that the overlap filter dropped
        // (because the function uses a different-width view of the same
        // physical register), `upgrade_to_tracked_for` rewires the
        // convention's varnode to the closest tracked variable in two
        // directions:
        //
        // 1. **Cover** (wider): e.g. MIPS-O32 lists `f0` as 4-byte but a
        //    double-returning function writes the 8-byte combined f0/f1
        //    view.  The 4-byte ret-reg upgrades to the 8-byte tracked
        //    container so the Return node still reads the float chain.
        //
        // 2. **Contained-in sub-register** (narrower):
        //    On x86_64 SysV `arg_passing_regs[0] = RDI` (8-byte), but
        //    `int forward_1(int a)` only reads `EDI` (4-byte sub-reg).
        //    With no covering tracked variable, the 4-byte sub-register
        //    is the only data the function actually consumed — using it
        //    as the arg-passing-var loses no information and keeps the
        //    Call node's arg(0) slot wired so pattern queries
        //    `call().arg(0, function_arg(0))` continue to match.
        let arg_passing_vars: Vec<_> = arg_passing_vars
            .iter()
            .filter_map(|vn| upgrade_to_tracked_for(&variable_to_id, *vn))
            .collect();
        let ret_val_vars: Vec<_> = ret_vars
            .iter()
            .filter_map(|vn| upgrade_to_tracked_for(&variable_to_id, *vn))
            .collect();

        let mut fb = FunctionBuilder {
            function: FunctionGraph::new_invalid(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id,
            arg_passing_vars,
            ret_val_vars,
            call_clobbered_variables,
            stack_ptr_vn,
            ret_stack_pop,
            largest_container: std::cell::OnceCell::new(),
            lift_addr: None,
        };
        fb.build_entry()?;
        Ok(fb)
    }

    /// Sets the asm-instruction address attributed to every subsequent
    /// `create_node` call until reset to `None` or replaced.  The
    /// strider per-region driver calls this before each pcode insn.
    /// Region-setup helpers (e.g. [`Self::build_entry`]) leave it `None`.
    #[inline]
    pub fn set_lift_addr(&mut self, addr: Option<u64>) {
        self.lift_addr = addr;
    }

    /// Returns the currently-attributed asm address (or `None` if no insn
    /// is active).
    #[inline]
    #[must_use]
    pub fn lift_addr(&self) -> Option<u64> {
        self.lift_addr
    }

    /// Creates a node in the graph with the given kind, inputs, and
    /// output kinds.  When [`Self::lift_addr`] is `Some(addr)`, also
    /// records `addr` in the resulting node's asm-fingerprint side-table
    /// entry; if `create_node` hits the dedup cache, the contributor is
    /// unioned into the existing entry.
    pub(super) fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        let addr = self.lift_addr;
        let node_id = self.graph_mut().create_node(kind, inputs, output_kinds);
        if let Some(addr) = addr {
            self.graph_mut().extend_asm_fingerprint(node_id, &[addr]);
        }
        node_id
    }

    /// Creates a single-output, pure (no side-effect) node and returns its
    /// output id.
    pub(super) fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
        self.graph().node_outputs(node)[0]
    }

    /// Returns an iterator over all tracked varnodes.
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    /// Returns the largest tracked variable in the same fixed-offset
    /// space (REGISTER or UNIQUE) that fully contains `reg`, or
    /// `None` if no tracked variable covers it.
    ///
    /// Result-cached: the lookup table is computed once on first
    /// call (O(V²) one-shot) and consulted in O(1) thereafter.
    /// Used by `pcode_lift::ValueLifter::find_largest_fitting_register`
    /// on every register read/write to apply Sleigh's container-
    /// aliasing rule (e.g. `al` → `rax` on x86_64).
    ///
    /// Returns `None` for varnodes outside REGISTER/UNIQUE space —
    /// containment-by-offset isn't meaningful for CONST or memory.
    /// Callers (currently `find_largest_fitting_register`) gate on
    /// the space themselves before calling.
    #[must_use]
    pub fn largest_container_for(&self, reg: &rsleigh::Vn) -> Option<rsleigh::Vn> {
        let map = self.largest_container.get_or_init(|| {
            // For each tracked variable, find its largest container
            // among all tracked variables in the same space.  O(V²)
            // up front; amortised across every subsequent lookup.
            //
            // Range arithmetic uses `saturating_add` because some
            // Sleigh varnodes (notably ppc64 / aarch64be CR slices)
            // sit at very high offsets where `off + size` would
            // overflow `u64`.  Saturation is safe: a saturated
            // endpoint can only fail the containment test (it's the
            // weakest possible upper bound), never spuriously
            // succeed.
            let vars: Vec<&rsleigh::Vn> = self.variable_to_id.keys().collect();
            let mut out: HashMap<rsleigh::Vn, rsleigh::Vn> = HashMap::with_capacity(vars.len());
            for v in &vars {
                let v_start = v.addr_off;
                let v_end = v_start.saturating_add(u64::from(v.size));
                let mut best: rsleigh::Vn = **v;
                for other in &vars {
                    if other.addr_space != v.addr_space {
                        continue;
                    }
                    let s = other.addr_off;
                    let e = s.saturating_add(u64::from(other.size));
                    if s > v_start || e < v_end {
                        continue;
                    }
                    if other.size > best.size {
                        best = **other;
                    }
                }
                out.insert(**v, best);
            }
            out
        });
        map.get(reg).copied()
    }

    /// Returns the [`rsleigh::Vn`] tracked at the given [`VarId`], or
    /// `None` if `var_id` is not in the variable map.  Used by the
    /// `strider` crate to convert per-region `(VarId, NodeOutputId)`
    /// pairs (returned by [`Self::region_initial_variables`]) into the
    /// `Vn`-keyed maps the per-iteration region index stores.
    #[must_use]
    pub fn vn_of_var(&self, var_id: VarId) -> Option<rsleigh::Vn> {
        self.variables.get(var_id).copied()
    }

    /// Returns the [`VarId`] for `vn`, or `None` if `vn` is not a
    /// tracked variable.  The inverse of [`Self::vn_of_var`].
    #[must_use]
    pub fn var_of_vn(&self, vn: &rsleigh::Vn) -> Option<VarId> {
        self.variable_to_id.get(vn).copied()
    }

    /// Returns the calling convention's return-value registers, in ABI order.
    /// Empty for synthetic test builds that didn't supply a convention.
    #[must_use] 
    pub fn ret_val_vars(&self) -> &[rsleigh::Vn] {
        &self.ret_val_vars
    }

    /// Finalises and returns the completed [`BuiltFunctionGraph`], after running
    /// structural validation on the built graph.
    ///
    /// # Errors
    ///
    /// Returns `ValidationFailed` wrapping a
    /// [`crate::validate::ValidationErrors`] bundle if the built graph fails
    /// any of validate's three layers (local typing, use-list consistency,
    /// graph-level invariants).
    pub fn build(self) -> crate::Result<crate::function::BuiltFunctionGraph> {
        // Conservative CallOther clobber default: every tracked variable
        // except the stack pointer.  The order here matches the iteration
        // order used by `build_call_other` so the i-th clobber output of a
        // CallOther node corresponds to `call_other_clobbered[i]`.
        let stack_ptr_vn = self.stack_ptr_vn;
        let call_other_clobbered: Box<[rsleigh::Vn]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();
        let built = crate::function::BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.call_clobbered_variables.into_boxed_slice(),
            ret_val_regs: self.ret_val_vars.into_boxed_slice(),
            call_other_clobbered,
        };
        crate::validate::validate(&built.graph, built.entry)?;
        Ok(built)
    }
}
