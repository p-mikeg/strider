use cranelift_entity::packed_option::ReservedValue;
use cranelift_entity::{PrimaryMap, entity_impl};
use rustc_hash::FxHashMap;

use crate::error::Result;
use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};
use crate::region::Region;

mod build_trait;
pub use build_trait::IRBuilder;
mod builder_ext;
pub use builder_ext::IRBuilderExt;
mod call;
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
    /// (call_clobbered, ret_val_regs, call_other_clobbered) all come off
    /// the [`Function`]'s `default_cc` + `all_vns`.
    pub(crate) function: Function,
    /// Build-time-only SSA bookkeeping: the bidirectional `VarId ↔ Vn`
    /// tracked-variable table.  `VarId` is a build-time key that never
    /// escapes the builder; the finished [`Function`] records varnodes
    /// via the ordered `all_vns` list instead (snapshotted from this
    /// table in `new`, one entry per tracked variable).
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
    /// Returns a reference to the underlying [`Function`] (graph + overlay).
    /// Pairs with [`Self::function_mut`] and [`Self::entry`].
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
    #[allow(clippy::expect_used)] // build_entry() is called unconditionally by new()
    pub fn entry(&self) -> NodeId {
        self.function
            .entry()
            .expect("entry is always set by build_entry(), which new() calls unconditionally")
    }

    /// Creates a new [`FunctionBuilder`] from a resolved calling convention.
    /// This is the **sole** constructor; synthetic / test graphs build a
    /// [`strider_target::BuiltCallingConvention`] (see the
    /// `strider-ir-test-utils` crate) and call this too.
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
    /// Returns `UnsupportedOutputSize` when a tracked variable's byte size
    /// has no matching `ValueType` (the entry block allocates one
    /// `InitialVar` per tracked variable).
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
        // mints one at the call site.  Seeding them here freezes the tracked
        // variable SET at construction, so a leaf function that merely
        // forwards a call still has an `InitialVar` for each CC register the
        // Call must read.  A function that *does* touch a wider view of one
        // of these (e.g. reads `RDI` after `EDI` was seeded) is handled by
        // `dedup_overlapping_largest` below, which keeps the widest
        // enclosing varnode.
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

        // Hand the resolved CC straight through: every register-list
        // projection a Call / Return / CallOther needs is derived from
        // `(default_cc, all_vns)`, so there is no synthesised stand-in to
        // overwrite afterwards.
        let mut fb = FunctionBuilder {
            function: Function::new(cc.clone(), endianness, all_vns),
            var_table,
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
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

    /// Overrides the positional stack-argument offsets of the function's
    /// calling convention.  Production builds carry these in the
    /// `BuiltCallingConvention` passed to [`Self::new`]; the low-level
    /// [`Self::new_raw`] path synthesises an empty list, so synthetic test
    /// fixtures that exercise stack-argument detection use this to make the
    /// function the single source of truth for its own stack-arg layout.
    pub fn set_stack_arg_offsets(&mut self, offsets: Vec<i64>) {
        self.function.default_cc.stack_arg_offsets = offsets;
    }

    /// Returns the currently-attributed asm address (or `None` if no insn
    /// is active).
    #[inline]
    pub fn lift_addr(&self) -> Option<u64> {
        self.lift_addr
    }

    /// Creates a node in the graph with the given kind, inputs, and
    /// output kinds.  When [`Self::lift_addr`] is `Some(addr)`, also
    /// records `addr` in the resulting node's asm-fingerprint side-table
    /// entry; if `create_node` hits the dedup cache, the contributor is
    /// unioned into the existing entry.
    pub(crate) fn create_node(
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
    /// `strider-orchestrator` to convert per-region `(VarId, ValueId)`
    /// pairs into the `Vn`-keyed maps the per-iteration region index
    /// stores.
    pub fn vn_of_var(&self, var_id: VarId) -> Option<rsleigh::Vn> {
        self.var_table.get(var_id).copied()
    }

    /// Returns the calling convention's return-value registers, in ABI order
    /// (each upgraded to its tracked varnode).  Empty for synthetic test
    /// builds that didn't supply a convention.  Derived from
    /// [`crate::Function::ret_val_regs`].
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
        #[allow(clippy::expect_used)] // build_entry() is called unconditionally by new()
        let entry = self
            .function
            .entry()
            .expect("entry is always set by build_entry(), which new() calls unconditionally");
        crate::validate::validate(&self.function, entry)?;
        Ok(self.function)
    }
}
