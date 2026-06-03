use anyhow::anyhow;
use cranelift_entity::packed_option::ReservedValue;
use cranelift_entity::{PrimaryMap, entity_impl};
use rustc_hash::FxHashMap;

use crate::error::Result;
use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::region::Region;

mod call;
mod coerce;
mod nodes;
#[cfg(test)]
mod tests;
mod vars;
mod vn_io;

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

/// Errors unless `vn` is in REGISTER or UNIQUE space.
///
/// `Call` / `CallOther` / `Return` registers all flow through the
/// aliasing-aware [`FunctionBuilder::read_reg_vn`] /
/// [`FunctionBuilder::write_reg_vn`] path, which only models fixed-offset
/// register containment.  A RAM / CONST / code-space varnode there is a
/// bug (or an unmodeled ABI), so it fails closed with a clear message
/// rather than producing a malformed read/write.
pub(super) fn require_reg_or_unique(vn: &rsleigh::Vn) -> crate::error::Result<()> {
    match vn.addr_space {
        rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE => Ok(()),
        space => Err(anyhow::anyhow!(
            "varnode {vn:?} must be in REGISTER or UNIQUE space for a \
             call-class read/write (got {space:?})"
        )),
    }
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
                    && other.addr_off.saturating_add(other.size as u64)
                        >= v.addr_off.saturating_add(v.size as u64)
                    && other.size > v.size
            })
        })
        .copied()
        .collect()
}

/// Incrementally constructs a sea-of-nodes IR function graph.
///
/// The builder tracks SSA-style per-region variable state: each variable has
/// exactly one current `ValueId` inside the active region.  Reads and
/// writes go through this mapping so that the graph is always in a consistent
/// state.
///
/// All calling-convention data lives on the [`Function`]'s `default_cc`
/// (the resolved convention) and `all_vns` (the ordered tracked-varnode
/// SSoT); every register-list projection a Call / Return / CallOther
/// needs is derived from those two.  The builder holds only genuine
/// build-time scratch (region map, current region, the `InitialMemory`
/// output, the lazy largest-container cache, and the per-insn `lift_addr`
/// attribution).
pub struct FunctionBuilder {
    /// The function being built (structural graph + overlay side tables).
    /// Calling-convention state (stack_vn, ret_stack_pop,
    /// preserves_memory) plus the derived register-list projections
    /// (call_clobbered, ret_val_regs, arg_passing_vars,
    /// call_other_clobbered) all come off the [`Function`]'s `default_cc`
    /// + `all_vns`.
    pub(crate) function: Function,
    /// Build-time-only SSA bookkeeping: the bidirectional `VarId ↔ Vn`
    /// tracked-variable table.  `VarId` is a build-time key that never
    /// escapes the builder; the finished [`Function`] records varnodes
    /// via the ordered `all_vns` list instead (snapshotted from this
    /// table in `new_raw`, one entry per tracked variable).
    pub(crate) var_table: crate::graph::VarTable,
    /// The single `Memory` output of the `InitialMemory` node.
    pub(crate) entry_memory: ValueId,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    /// Lazy `tracked_vn → its largest containing tracked-vn` map.
    /// Populated on first call to [`Self::largest_container_for`];
    /// the variable set is fixed at construction so caching is safe.
    /// Lookup turns the per-call O(V) linear scan in
    /// `strider_lift::pcode_lift::ValueLifter::find_largest_fitting_register` into O(1).
    pub(crate) largest_container: std::cell::OnceCell<FxHashMap<rsleigh::Vn, rsleigh::Vn>>,
    /// Lazy cache of the function-default CC's derived `(ret_val_vns,
    /// clobber_vns)` lists.  Most `Call`s use the function-default CC, so
    /// recomputing `call_ret_vals_for(default)` / `call_clobbered_for(default)`
    /// per Call is wasted work.  Computed once on the first default Call from
    /// `Function::call_ret_vals_for(default_cc)` + `call_clobbered_for`, then
    /// reused.  Override-CC Calls (sparse) compute on demand (O(N) per the
    /// hashed-membership derivations).  Lives on the builder — construction-
    /// time only — so there is no `Sync` / `compact` concern, and the
    /// `OnceCell` (not `RefCell`) keeps the builder usable in `Sync` contexts.
    pub(crate) default_call_lists:
        std::cell::OnceCell<(Vec<rsleigh::Vn>, Vec<rsleigh::Vn>)>,
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
/// * `@kind`    — `&self, value: ValueId`; predicate runs on
///   `Graph::value_kind(value)` (a [`ValueKind`]).
/// * `@kind_with_got` — same, but the error message also prints the
///   observed kind via a trailing `(got {kind:?})`.
/// * `@type_of` — `&self, value: ValueId`; predicate runs on the
///   value's [`ValueType`] via `value_type(value)?`.
/// * `@ty`      — `ty: ValueType` (associated fn, no `self`);
///   predicate runs on `ty` directly.
///
/// `$label` is interpolated into the error message verbatim so each
/// helper preserves its existing diagnostic wording.
macro_rules! require_kind {
    (@kind $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, value: ValueId) -> Result<()> {
            if !self.function().value_kind(value).$pred() {
                return Err(anyhow!(concat!("output {:?} is not ", $label), value));
            }
            Ok(())
        }
    };
    (@kind_with_got $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, value: ValueId) -> Result<()> {
            let kind = self.function().value_kind(value);
            if !kind.$pred() {
                return Err(anyhow!(
                    concat!("output {:?} is not ", $label, " (got {:?})"),
                    value,
                    kind,
                ));
            }
            Ok(())
        }
    };
    (@type_of $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(&self, value: ValueId) -> Result<()> {
            if !self.value_type(value)?.$pred() {
                return Err(anyhow!(concat!("output {:?} is not ", $label), value));
            }
            Ok(())
        }
    };
    (@ty $name:ident, $pred:ident, $label:literal) => {
        pub(super) fn $name(ty: ValueType) -> Result<()> {
            if !ty.$pred() {
                return Err(anyhow!(concat!("type {:?} is not ", $label), ty));
            }
            Ok(())
        }
    };
}

impl FunctionBuilder {
    /// Returns a reference to the underlying [`Function`] (graph + overlay).
    /// Pairs with [`Self::function_mut`] and [`Self::entry`].
    #[must_use]
    pub fn function(&self) -> &Function {
        &self.function
    }

    /// Returns a mutable reference to the underlying [`Function`] (graph + overlay).
    ///
    /// This is the primary entry point for in-place graph mutation (e.g.
    /// running an `opt::Optimizer` pass on a builder that we still want to
    /// use afterwards). Pairs with [`Self::entry`]: opt passes need
    /// `(function, entry)` together because `entry` anchors the
    /// reachable-node walk the validator's local-typing check is scoped
    /// to.
    pub fn function_mut(&mut self) -> &mut Function {
        &mut self.function
    }

    /// Returns the recorded entry [`NodeId`] of the function being
    /// built — the same id that [`Self::build`] would record on the
    /// produced [`crate::Function`]'s entry.
    ///
    /// CORRECTNESS — pairs with [`Self::function_mut`]: opt passes that
    /// take `(function, entry)` get a stable handle here.  The entry node
    /// id never changes once the builder's first region is registered,
    /// so callers may cache it across iterations.
    #[must_use]
    #[allow(clippy::expect_used)] // build_entry() is called unconditionally by new_raw()
    pub fn entry(&self) -> NodeId {
        self.function
            .entry()
            .expect("entry is always set by build_entry(), which new_raw() calls unconditionally")
    }

    pub(super) fn validate_value_inputs(&self, inputs: &[ValueId]) -> Result<()> {
        for &v in inputs {
            self.require_value_kind(v)?;
        }
        Ok(())
    }

    // The seven `require_*` helpers below all share the same
    // kind-check + `anyhow!` shape.  They split into three argument
    // shapes — kind-check on `ValueId` (via `Graph::value_kind`),
    // type-check on `ValueId` (via `value_type?`), and
    // type-check on `ValueType` (static, no `self`) — each emitted
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
    /// a tracked variable's byte size has no matching `ValueType`.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: &strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
    ) -> Result<Self> {
        // Ensure every calling-convention register — the return registers
        // (int + float), the argument-passing registers, and the stack
        // pointer — is a tracked variable, even when the function body never
        // touches it directly.
        //
        // Return regs: keeps the data-flow chain from a float operation's
        // output (e.g. an aarch64 FloatAdd writes to s0, the 4-byte
        // sub-register of q0) connected to the Return node — without this
        // step `q0` would not be in the variable set, and the pcode-lift
        // register-aliasing logic would never widen the s0 write into a q0
        // store visible to Return.
        //
        // Arg-passing regs + stack pointer: every `Call` reads each
        // arg-passing register and the stack pointer through the aliasing-
        // aware `read_reg_vn`, which requires a tracked container, and never
        // mints one at the call site.  Seeding them here guarantees the
        // invariant the builder-side default-CC cache relies on: the tracked
        // variable SET is frozen at construction, so a leaf function that
        // merely forwards a call still has an `InitialVar` for each CC
        // register the Call must read.  A function that *does* touch a wider
        // view of one of these (e.g. reads `RDI` after `EDI` was seeded) is
        // handled by `new_raw`'s `dedup_overlapping_largest`, which keeps the
        // widest enclosing varnode.
        for v in cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(std::iter::once(&cc.stack_vn))
        {
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
            Some(cc.stack_vn),
            cc.ret_stack_pop,
            endianness,
        )?;
        // Embed the full CC so accessors (`preserves_memory`,
        // `stack_vn`, `ret_stack_pop`, `link_register_vn`, ...)
        // can delegate without duplicating these scalars.  Must happen
        // before any read of the function's ABI facts.
        builder.function.default_cc = cc.clone();
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
        Self::new_raw(vec![], &[], &[], &[], None, 0, strider_target::Endianness::Little)
    }

    /// Low-level constructor that takes the convention-derived data as
    /// unpacked slices.  Used by synthetic tests that don't resolve a real
    /// calling convention against a Sleigh register table — production code
    /// should use [`FunctionBuilder::new`] with a [`strider_target::BuiltCallingConvention`].
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedOutputSize` when any tracked variable
    /// has a byte size with no matching `ValueType` (the entry-block
    /// builder allocates an `InitialVar` per tracked variable), or
    /// propagates a `BuiltCallingConvention::try_new` validation error
    /// from the synthesised CC when `stack_vn` is `Some` (currently
    /// only fires for a negative `ret_stack_pop`).
    pub fn new_raw(
        all_used_variables: Vec<rsleigh::Vn>,
        arg_passing_vars: &[rsleigh::Vn],
        callee_saved_vars: &[rsleigh::Vn],
        ret_vars: &[rsleigh::Vn],
        stack_vn: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
        endianness: strider_target::Endianness,
    ) -> Result<Self> {
        let all_variables = dedup_overlapping_largest(&all_used_variables);
        let mut var_table = crate::graph::VarTable::default();
        for variable in all_variables {
            var_table.intern(variable);
        }
        // The ordered tracked-varnode SSoT (`Function::all_vns`).  Captured
        // eagerly from `var_table` in VarId / interning order — the same
        // order `set_entry_region` later iterates when creating one
        // `InitialVar` per tracked variable, so the `i`-th derived clobber
        // varnode still lines up with the `i`-th `Call` clobber output.
        // The tracked set is fixed at construction, so this snapshot is
        // stable for the function's lifetime.
        let all_vns: Vec<rsleigh::Vn> = var_table.values().copied().collect();

        // The register-list projections (`call_clobbered`, `ret_val_regs`,
        // `arg_passing_vars`, `call_other_clobbered`) are no longer stored:
        // they are DERIVED on demand from `Function::all_vns` +
        // `Function::default_cc` (see [`Function::call_clobbered_for`],
        // [`Function::ret_val_regs`], [`Function::arg_passing_vars`],
        // [`Function::call_other_clobbered_regs`]).  For the derivations to
        // reproduce the formerly-stored lists for a synthetic `new_raw`
        // build, the synthesised `default_cc` must carry the same
        // convention reg lists this constructor was handed:
        //
        // * `arg_passing_regs` / `callee_saved_regs` feed the arg-passing
        //   and clobber derivations: the raw `arg_passing_regs` are read
        //   via `read_reg_vn` at the Call site, and the `is_clobbered`
        //   filter mirrors the old `build_call_clobbered_list`.
        // * the combined `ret_vars` go in `ret_val_regs` (with
        //   `ret_val_regs_float` empty) so `ret_val_regs()` /
        //   `call_clobbered_for` see the same raw front-loaded ret list.
        //
        // Constructed by struct literal (not `try_new`) so synthetic test
        // fixtures aren't subjected to the ABI-disjointness validation —
        // `new_raw` is the unvalidated low-level path.  When `stack_vn` is
        // `None`, the trivial CC's synthetic `stack_vn` (a real, sized
        // register at an out-of-range offset) is used so SP-keyed analyses
        // no-op.  A synthetic-test Call must track its stack pointer: the
        // builder reads the SP through the variable table at the call site
        // and errors when it is absent (`build_call` no longer mints an SP
        // anchor).  Production callers go through [`Self::new`], which
        // overwrites this synthetic CC with the real one immediately after
        // `new_raw` returns.
        let trivial = strider_target::BuiltCallingConvention::default();
        let synthesised_cc = strider_target::BuiltCallingConvention {
            arg_passing_regs: arg_passing_vars.to_vec(),
            callee_saved_regs: callee_saved_vars.to_vec(),
            ret_val_regs: ret_vars.to_vec(),
            ret_val_regs_float: Vec::new(),
            stack_vn: stack_vn.unwrap_or(trivial.stack_vn),
            stack_arg_offsets: Vec::new(),
            ret_stack_pop,
            link_register_vn: None,
            preserves_memory: false,
        };

        let mut function = Function::new();
        function.default_cc = synthesised_cc;
        function.all_vns = all_vns;
        function.endianness = endianness;
        let mut fb = FunctionBuilder {
            function,
            var_table,
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
            largest_container: std::cell::OnceCell::new(),
            default_call_lists: std::cell::OnceCell::new(),
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
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        let addr = self.lift_addr;
        let node_id = self.function_mut().graph_mut().create_node(kind, inputs, output_kinds);
        if let Some(addr) = addr {
            self.function_mut().extend_asm_fingerprint(node_id, &[addr]);
        }
        node_id
    }

    /// Creates a single-output, pure (no side-effect) node and returns its
    /// output id.
    pub(super) fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_type: ValueType,
    ) -> ValueId {
        let node = self.create_node(kind, inputs, [ValueKind::Typed(output_type)]);
        self.function().node_outputs(node)[0]
    }

    /// Returns an iterator over all tracked varnodes.
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.var_table.values()
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
            let vars: Vec<rsleigh::Vn> = self.var_table.values().copied().collect();
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
    /// `strider-analyze` to convert per-region `(VarId, ValueId)`
    /// pairs into the `Vn`-keyed maps the per-iteration region index
    /// stores.
    #[must_use]
    pub fn vn_of_var(&self, var_id: VarId) -> Option<rsleigh::Vn> {
        self.var_table.get(var_id).copied()
    }

    /// Returns the calling convention's return-value registers, in ABI order
    /// (each upgraded to its tracked varnode).  Empty for synthetic test
    /// builds that didn't supply a convention.  Derived from
    /// [`crate::Function::ret_val_regs`].
    #[must_use]
    pub fn ret_val_vars(&self) -> Vec<rsleigh::Vn> {
        self.function.ret_val_regs()
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
        // The conservative CallOther clobber default (every tracked
        // variable except the stack pointer) is no longer stored — it is
        // derived on demand from `all_vns` + `default_cc.stack_vn` by
        // [`crate::Function::call_other_clobbered_regs`], in the same
        // `all_vns` (allocation) order the CallOther builders consume.
        #[allow(clippy::expect_used)] // build_entry() is called unconditionally by new_raw()
        let entry = self
            .function
            .entry()
            .expect("entry is always set by build_entry(), which new_raw() calls unconditionally");
        crate::validate::validate(&self.function, entry)?;
        Ok(self.function)
    }
}
