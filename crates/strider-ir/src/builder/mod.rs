use anyhow::anyhow;
use cranelift_entity::packed_option::ReservedValue;
use cranelift_entity::{PrimaryMap, entity_impl};
use rustc_hash::FxHashMap;

use crate::error::Result;
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
    variable_to_id: &FxHashMap<rsleigh::Vn, VarId>,
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
    // bytes outside its range are unused.  Tie-break by `(size, addr_off)`
    // so the choice is deterministic across hash seeds when two equal-
    // size sub-registers exist (rare in practice — most sleigh specs
    // de-overlap during `new_raw`'s filter — but defensive against
    // FxHashMap's non-deterministic iteration order).
    variable_to_id
        .keys()
        .filter(|t| {
            t.addr_space == vn.addr_space
                && t.addr_off >= vn.addr_off
                && t.addr_off + t.size as u64 <= vn_end
        })
        .max_by_key(|t| (t.size, t.addr_off))
        .copied()
}

/// Filters `all_used_variables` down to the largest enclosing tracked
/// variable in each fixed-offset (REGISTER/UNIQUE) space.  E.g. if both
/// `rdi` and `edi` are touched, the `edi` entry is dropped.  CONST and
/// code-space varnodes are kept verbatim — containment-by-offset is
/// meaningless there.
///
/// MIPS-style example: Sleigh's MIPS lifter writes a 64-bit IntMul
/// result to a unique varnode then Copies a 4-byte slice to a register;
/// without this filter the 4-byte and 8-byte unique varnodes look like
/// independent SSA variables.  Keeping the wider varnode preserves the
/// data dependency.
fn dedup_overlapping_largest(all_used_variables: &[rsleigh::Vn]) -> Vec<rsleigh::Vn> {
    all_used_variables
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
        .collect()
}

/// Builds the call-clobbered variable list emitted as a `Call` node's
/// value outputs (slot `i + 2` ↔ `call_clobbered_variables[i]`).
///
/// Front-loads the calling convention's return registers so
/// `.ret_output(0)` indexes into ABI ret slot 0 (e.g. rax on x86_64),
/// then appends the remaining caller-clobbered registers.  The stack
/// pointer (rebound separately via `ret_stack_pop`) and callee-saved
/// registers are excluded.
fn build_call_clobbered_list(
    callee_saved_vars: &[rsleigh::Vn],
    stack_ptr_vn: Option<rsleigh::Vn>,
    ret_vars: &[rsleigh::Vn],
    all_variables: &[rsleigh::Vn],
) -> Vec<rsleigh::Vn> {
    let is_clobbered =
        |v: &rsleigh::Vn| !callee_saved_vars.contains(v) && Some(*v) != stack_ptr_vn;
    let ret_prefix = ret_vars
        .iter()
        .copied()
        .filter(|v| all_variables.contains(v) && is_clobbered(v));
    let rest = all_variables
        .iter()
        .filter(|v| is_clobbered(v) && !ret_vars.contains(v))
        .copied();
    ret_prefix.chain(rest).collect()
}

/// Incrementally constructs a sea-of-nodes IR function graph.
///
/// The builder tracks SSA-style per-region variable state: each variable has
/// exactly one current `NodeOutputId` inside the active region.  Reads and
/// writes go through this mapping so that the graph is always in a consistent
/// state.
pub struct FunctionBuilder {
    /// The sea-of-nodes graph being built.
    pub(crate) graph: Graph,
    /// The `Entry` node that serves as the root of the function.
    /// Set to a reserved sentinel before [`Self::build_entry`] runs;
    /// `build()` will refuse to finalise if it remains reserved.
    pub(crate) entry: NodeId,
    /// The single `Control` output of the `Entry` node.
    pub(crate) entry_control: NodeOutputId,
    /// The single `Memory` output of the `InitialMemory` node.
    pub(crate) entry_memory: NodeOutputId,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub(crate) variable_to_id: FxHashMap<rsleigh::Vn, VarId>,
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
    /// Function-default value of `CallingConvention::no_memory_clobber`
    /// (carried over via [`strider_target::BuiltCallingConvention::no_memory_clobber`]).
    /// When `true`, [`Self::build_call_with_cc`] suppresses the `Memory`
    /// output on the resulting `Call` node and does not advance the region's
    /// memory chain.  Per-call `override_cc` may override this.
    pub(crate) no_memory_clobber: bool,
    /// Lazy `tracked_vn → its largest containing tracked-vn` map.
    /// Populated on first call to [`Self::largest_container_for`];
    /// the variable set is fixed at construction so caching is safe.
    /// Lookup turns the per-call O(V) linear scan in
    /// `strider_lift::pcode_lift::ValueLifter::find_largest_fitting_register` into O(1).
    pub(crate) largest_container: std::cell::OnceCell<FxHashMap<rsleigh::Vn, rsleigh::Vn>>,
    /// Asm-instruction address attributed to every node `create_node`
    /// produces while this is `Some`.  The lifter / strider region driver
    /// sets it to `Some(addr)` immediately before each pcode insn (see
    /// [`Self::set_lift_addr`]) and back to `None` between insns.
    /// Region-setup helpers (`build_entry`, `build_function_args`,
    /// region/phi creation) leave it `None`, so synthesised structural
    /// nodes legitimately stay empty in the fingerprint side-table.
    pub(crate) lift_addr: Option<u64>,
}

/// Emits a `require_*` helper that returns `Err(anyhow!(...))` when its
/// argument fails the named predicate.  Three argument shapes:
///
/// * `@kind`    — `&self, output: NodeOutputId`; predicate runs on
///   `Graph::output_kind(output)` (a [`NodeOutputKind`]).
/// * `@kind_with_got` — same, but the error message also prints the
///   observed kind via a trailing `(got {kind:?})`.
/// * `@type_of` — `&self, output: NodeOutputId`; predicate runs on the
///   value's [`NodeOutputType`] via `get_output_type(output)?`.
/// * `@ty`      — `ty: NodeOutputType` (associated fn, no `self`);
///   predicate runs on `ty` directly.
///
/// `$label` is interpolated into the error message verbatim so each
/// helper preserves its existing diagnostic wording.
macro_rules! require_kind {
    (@kind $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, output: NodeOutputId) -> Result<()> {
            if !self.graph().output_kind(output).$pred() {
                return Err(anyhow!(concat!("output {:?} is not ", $label), output));
            }
            Ok(())
        }
    };
    (@kind_with_got $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, output: NodeOutputId) -> Result<()> {
            let kind = self.graph().output_kind(output);
            if !kind.$pred() {
                return Err(anyhow!(
                    concat!("output {:?} is not ", $label, " (got {:?})"),
                    output,
                    kind,
                ));
            }
            Ok(())
        }
    };
    (@type_of $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, output: NodeOutputId) -> Result<()> {
            if !self.get_output_type(output)?.$pred() {
                return Err(anyhow!(concat!("output {:?} is not ", $label), output));
            }
            Ok(())
        }
    };
    (@ty $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(ty: NodeOutputType) -> Result<()> {
            if !ty.$pred() {
                return Err(anyhow!(concat!("type {:?} is not ", $label), ty));
            }
            Ok(())
        }
    };
}

impl FunctionBuilder {
    /// Returns a reference to the underlying [`Graph`] without consuming
    /// the builder.  Pairs with [`Self::graph_mut`] and [`Self::entry`].
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns a mutable reference to the underlying [`Graph`] without
    /// consuming the builder.
    ///
    /// This is the primary entry point for in-place graph mutation (e.g.
    /// running an `opt::Optimizer` pass on a builder that we still want to
    /// use afterwards). Pairs with [`Self::entry`]: opt passes need
    /// `(graph, entry)` together because `entry` anchors the
    /// reachable-node walk the validator's local-typing check is scoped
    /// to.
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Returns the recorded entry [`NodeId`] of the function being
    /// built — the same id that [`Self::build`] would record on the
    /// produced [`crate::Function`]'s entry.
    ///
    /// CORRECTNESS — pairs with [`Self::graph_mut`]: opt passes that
    /// take `(graph, entry)` get a stable handle here.  The entry node
    /// id never changes once the builder's first region is registered,
    /// so callers may cache it across iterations.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.entry
    }

    pub(super) fn validate_value_inputs(&self, inputs: &[NodeOutputId]) -> Result<()> {
        for &v in inputs {
            self.require_value_kind(v)?;
        }
        Ok(())
    }

    // The seven `require_*` helpers below all share the same
    // kind-check + `anyhow!` shape.  They split into three argument
    // shapes — kind-check on `NodeOutputId` (via `Graph::output_kind`),
    // type-check on `NodeOutputId` (via `get_output_type?`), and
    // type-check on `NodeOutputType` (static, no `self`) — each emitted
    // by one arm of the `require_kind!` macro defined below this impl.
    require_kind!(@kind_with_got require_value_kind, is_value, "a value edge");
    require_kind!(@kind require_bool_value, is_bool, "a bool value");
    require_kind!(@kind require_phi_token_kind, is_phi_token, "a phi-token edge");
    require_kind!(@type_of require_integer_value, is_integer, "an integer value");
    require_kind!(@type_of require_float_value, is_float, "a float value");
    require_kind!(@ty require_integer_type, is_integer, "an integer type");
    require_kind!(@ty require_float_type, is_float, "a float type");

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
        cc: &strider_target::BuiltCallingConvention,
    ) -> Result<Self> {
        // Ensure all return registers (int + float) are tracked variables.
        // This keeps the data-flow chain from a float operation's output
        // (e.g. an aarch64 FloatAdd writes to s0, the 4-byte sub-register of q0)
        // connected to the Return node — without this step `q0` would not be
        // in the variable set, and the pcode-lift register-aliasing logic
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
        let mut builder = Self::new_raw(
            all_used_variables,
            &cc.arg_passing_regs,
            &cc.callee_saved_regs,
            &combined_ret_vars,
            Some(cc.stack_ptr_vn),
            cc.ret_stack_pop,
        )?;
        // Carry the function-default no_memory_clobber from the CC; per-call
        // override_cc can still override on individual Call sites.
        builder.no_memory_clobber = cc.no_memory_clobber;
        Ok(builder)
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
    /// should use [`FunctionBuilder::new`] with a [`strider_target::BuiltCallingConvention`].
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
        let all_variables = dedup_overlapping_largest(&all_used_variables);
        let call_clobbered_variables = build_call_clobbered_list(
            callee_saved_vars,
            stack_ptr_vn,
            ret_vars,
            &all_variables,
        );
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = FxHashMap::default();
        for variable in all_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        // For arg-passing and ret-val regs that `dedup_overlapping_largest`
        // dropped (because the function uses a different-width view of the
        // same physical register), `upgrade_to_tracked_for` rewires the
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
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id,
            arg_passing_vars,
            ret_val_vars,
            call_clobbered_variables,
            stack_ptr_vn,
            ret_stack_pop,
            // Default: synthetic builders don't preserve memory.  Production
            // code path goes through `new()` which copies the field from the
            // user-supplied CC after `new_raw` returns.
            no_memory_clobber: false,
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
    /// Used by `strider_lift::pcode_lift::ValueLifter::find_largest_fitting_register`
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
            // among all tracked variables in the same space.
            //
            // Algorithm: bucket variables by `addr_space`, sort each
            // bucket by `(addr_off ascending, size descending)`, then
            // single-pass an "open enclosures" stack — at each var,
            // pop enclosures whose end is to the left of the current
            // var's start, then the deepest remaining enclosure (the
            // first pushed, since later pushes are nested-or-equal
            // inside it under the sort order) is the largest container.
            //
            // Complexity: O(V log V) sort + O(V) stack pass per space.
            //
            // Range arithmetic uses `saturating_add` because some
            // Sleigh varnodes (notably ppc64 / aarch64be CR slices)
            // sit at very high offsets where `off + size` would
            // overflow `u64`.  Saturation is safe: a saturated
            // endpoint can only fail the containment test (it's the
            // weakest possible upper bound), never spuriously succeed.
            let vars: Vec<rsleigh::Vn> = self.variable_to_id.keys().copied().collect();
            let mut out: FxHashMap<rsleigh::Vn, rsleigh::Vn> =
                FxHashMap::with_capacity_and_hasher(vars.len(), Default::default());

            // Bucket by addr_space (FxHashMap iteration order is
            // stable per insertion in single-threaded code; the per-
            // space loop sorts deterministically afterwards).
            let mut by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> =
                FxHashMap::default();
            for v in vars {
                by_space.entry(v.addr_space).or_default().push(v);
            }

            for (_space, mut bucket) in by_space {
                // Sort: addr_off ascending, then size descending so
                // that for equal starts the wider container precedes
                // narrower ones (and pops correctly later).
                bucket.sort_by_key(|v| (v.addr_off, std::cmp::Reverse(v.size)));

                // `open` holds (end, vn) pairs for enclosures whose
                // range still strictly extends past the current
                // start.  The bottom of the stack is the deepest /
                // largest container thanks to the sort order.
                let mut open: Vec<(u64, rsleigh::Vn)> = Vec::new();
                for v in &bucket {
                    let v_start = v.addr_off;
                    let v_end = v_start.saturating_add(u64::from(v.size));
                    // Pop enclosures whose end is strictly to the left
                    // of `v`'s start — they can no longer contain `v`.
                    while let Some(&(end, _)) = open.last() {
                        if end < v_end {
                            // The top enclosure doesn't reach as far
                            // as `v`'s end either, so it's no longer a
                            // candidate enclosure for things following.
                            open.pop();
                        } else {
                            break;
                        }
                    }
                    // The bottom of `open` (the first entry) is the
                    // largest container reaching across `v` — by the
                    // sort order it was pushed when the widest start-
                    // tied entry appeared first.
                    let best = open
                        .first()
                        .map(|(_, vn)| *vn)
                        .filter(|cand| {
                            // Validate end: cand.end >= v_end already
                            // (we popped otherwise) and cand.start <=
                            // v.start (sort order guarantees).  But a
                            // candidate at the SAME start with a SAME
                            // size is `v` itself; only count it as a
                            // larger container if its size strictly
                            // exceeds `v`'s.
                            cand.size > v.size
                                || (cand.size == v.size && cand.addr_off < v.addr_off)
                        });
                    let chosen = best.unwrap_or(*v);
                    out.insert(*v, chosen);
                    open.push((v_end, *v));
                }
            }
            out
        });
        map.get(reg).copied()
    }

    /// Returns the [`rsleigh::Vn`] tracked at the given `VarId`, or
    /// `None` if `var_id` is not in the variable map.  Used by
    /// `strider-analyze` to convert per-region `(VarId, NodeOutputId)`
    /// pairs into the `Vn`-keyed maps the per-iteration region index
    /// stores.
    #[must_use]
    pub fn vn_of_var(&self, var_id: VarId) -> Option<rsleigh::Vn> {
        self.variables.get(var_id).copied()
    }

    /// Returns the calling convention's return-value registers, in ABI order.
    /// Empty for synthetic test builds that didn't supply a convention.
    #[must_use] 
    pub fn ret_val_vars(&self) -> &[rsleigh::Vn] {
        &self.ret_val_vars
    }

    /// Finalises and returns the completed [`crate::Function`],
    /// after running structural validation.
    ///
    /// # Errors
    ///
    /// Returns an [`anyhow::Error`] wrapping a
    /// [`crate::validate::ValidationErrors`] bundle if the built graph fails
    /// any of validate's three layers (local typing, use-list consistency,
    /// graph-level invariants).  Recover the bundle via
    /// `err.downcast_ref::<crate::validate::ValidationErrors>()`.
    pub fn build(self) -> crate::Result<crate::Function> {
        // Conservative CallOther clobber default: every tracked variable
        // except the stack pointer.  The order here matches the iteration
        // order used by `build_call_other_modeled` / `build_call_other_terminal`
        // so the i-th clobber output of a CallOther node corresponds to
        // `call_other_clobbered[i]`.
        let stack_ptr_vn = self.stack_ptr_vn;
        let call_other_clobbered: Box<[rsleigh::Vn]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();
        let entry = self.entry;
        let cc_metadata = crate::graph::CcMetadata {
            variables: self.variables,
            call_clobbered: self.call_clobbered_variables.into_boxed_slice(),
            ret_val_regs: self.ret_val_vars.into_boxed_slice(),
            call_other_clobbered,
            no_memory_clobber: self.no_memory_clobber,
        };
        let graph = self.graph;
        crate::validate::validate(&graph, entry)?;
        let mut function = crate::Function::from_built_graph(graph, entry);
        function.set_cc_metadata(cc_metadata);
        Ok(function)
    }
}
