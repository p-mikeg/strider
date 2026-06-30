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
/// also computing, in the SAME O(n log n) sweep, every aliasable input
/// varnode's largest tracked container (the SP-relative `vn_to_container`
/// SSoT). Returns `(survivors_in_input_order, vn_to_container)`:
///
/// * **survivors** — the deduped tracked set in INPUT order, so `VarId`
///   assignment downstream stays the input-order-derived one the sort in
///   [`FunctionBuilder::new`] then re-canonicalises. A varnode is dropped iff
///   some STRICTLY larger same-space varnode encloses its byte range.
/// * **vn_to_container** — aliasable-only: a survivor maps to itself; a
///   dropped sub-register view maps to its largest strict encloser (always a
///   survivor, since only survivors are pushed as open enclosures). Non-
///   aliasable spaces (CONST / code) are kept verbatim and absent from the map
///   — containment-by-offset is meaningless there.
///
/// Computing the container at drop time reuses the same per-space stack sweep
/// the dedup needs, so it replaces the former separate `vn → container` build
/// plus the O(V²) per-view `largest_container_in` rescan. Picking the
/// MAX-size enclosing open (not merely the first) makes it correct even for
/// crossing partial-overlap enclosers (`[0,12)` and `[2,20)` both enclosing
/// `[5,9)` → the wider `[2,20)`), the case a naive first-open stack sweep got
/// wrong.
fn dedup_and_container_map(
    all_used_variables: &[rsleigh::Vn],
) -> (Vec<rsleigh::Vn>, FxHashMap<rsleigh::Vn, rsleigh::Vn>) {
    // Range end with saturating arithmetic (high-offset CR slices on ppc64 /
    // aarch64be can push `addr_off + size` past `u64::MAX`).
    fn end_of(v: &rsleigh::Vn) -> u64 {
        v.addr_off.saturating_add(u64::from(v.size))
    }

    // Bucket aliasable inputs by space, carrying each entry's original index.
    let mut by_space: FxHashMap<rsleigh::VnSpace, Vec<(usize, rsleigh::Vn)>> = FxHashMap::default();
    for (i, v) in all_used_variables.iter().enumerate() {
        if is_aliasable_space(v.addr_space) {
            by_space.entry(v.addr_space).or_default().push((i, *v));
        }
    }

    let mut dropped = vec![false; all_used_variables.len()];
    let mut vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn> =
        FxHashMap::with_capacity_and_hasher(all_used_variables.len(), Default::default());
    for (_space, mut bucket) in by_space {
        // addr_off ascending, then size descending so a wider enclosure is
        // seen before the narrower slices it contains.
        bucket.sort_by_key(|(_, v)| (v.addr_off, std::cmp::Reverse(v.size)));

        // Open enclosures whose range still extends past the current start,
        // kept as `(end, vn)` and holding only SURVIVORS. Sorted-by-start
        // arrival means any open entry has `off <= v.off`; one with
        // `end >= v_end` AND `size > v.size` strictly encloses `v`.
        let mut open: Vec<(u64, rsleigh::Vn)> = Vec::new();
        for (idx, v) in bucket {
            let v_end = end_of(&v);
            // Drop opens whose range ends before this entry STARTS: by the
            // addr_off-ascending sort every remaining entry starts at or after
            // `v.addr_off`, so such an open can enclose neither `v` nor any
            // later entry (whose end is ≥ its own start ≥ v.addr_off).  Opens
            // surviving this prune may still enclose a later, even-narrower
            // entry that shares `v`'s start, so they stay live.
            open.retain(|&(end, _)| end >= v.addr_off);
            // Largest STRICTLY-larger enclosing open (`off ≤ v.off` by sort,
            // `end ≥ v_end`, `size > v.size`): if present, `v` is a subsumed
            // sub-register view whose container is that open; else `v` is the
            // largest in its chain (its own container) and joins the opens.
            let container = open
                .iter()
                .filter(|&&(end, c)| end >= v_end && c.size > v.size)
                .max_by_key(|&&(_, c)| c.size)
                .map(|&(_, c)| c);
            match container {
                Some(c) => {
                    dropped[idx] = true;
                    vn_to_container.insert(v, c);
                }
                None => {
                    vn_to_container.insert(v, v);
                    open.push((v_end, v));
                }
            }
        }
    }

    let survivors = all_used_variables
        .iter()
        .enumerate()
        .filter(|(i, _)| !dropped[*i])
        .map(|(_, v)| *v)
        .collect();
    (survivors, vn_to_container)
}

/// Deterministic ordering key for a tracked varnode: `(space, offset,
/// size)`.  Sorting `all_vns` by this in `FunctionBuilder::new` makes
/// `VarId` assignment — and every derived clobber-slot index — stable
/// regardless of the order varnodes were collected from the CFG.  The
/// builder owns this so the lifter need not pre-sort.
fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size)
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
/// output, and the per-insn `lift_addr` attribution).  Varnode-container
/// resolution is delegated to the persisted [`Function::container_of`].
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
    /// Asm-instruction address attributed to every node `create_node`
    /// produces while this is `Some`.  The lifter / strider region driver
    /// sets it to `Some(addr)` immediately before each pcode insn (see
    /// [`Self::set_lift_addr`]) and back to `None` between insns.
    /// Region-setup helpers (`build_entry`, region/phi creation) leave it
    /// `None`, so synthesised structural
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
    ///
    /// The [`crate::IRBuilder::function_mut`] trait method exposes this same
    /// access through the generic builder seam (identical body); concrete
    /// `FunctionBuilder` call sites resolve to this inherent method.
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
    pub fn entry(&self) -> NodeId {
        self.function.entry()
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
        // `dedup_and_container_map` below, which keeps the widest
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

        // Single sweep: drop strictly-enclosed views AND record every
        // aliasable input varnode's largest container (REGISTER/UNIQUE only;
        // CONST is left to the graph's structural dedup cache and RAM
        // load/store is deliberately not deduped).
        let (mut all_variables, mut vn_to_container) =
            dedup_and_container_map(&all_used_variables);
        // FunctionBuilder owns vn ordering: sort the deduped tracked set
        // by (space, offset, size) so VarId assignment is deterministic
        // independent of CFG-collection order.  The lifter no longer sorts.
        all_variables.sort_by_key(vn_sort_key);
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

        // `dedup_and_container_map` already mapped every aliasable varnode
        // in `all_used_variables`. The only keys it can't have seen are
        // callee-saved registers a leaf function never touches (so they were
        // never in the used set); resolve those few against `all_vns`.
        for vn in cc
            .callee_saved_regs
            .iter()
            .copied()
            .filter(|v| is_aliasable_space(v.addr_space))
        {
            vn_to_container
                .entry(vn)
                .or_insert_with(|| crate::function::largest_container_in(&all_vns, &vn));
        }

        // Hand the resolved CC straight through: every register-list
        // projection a Call / Return / CallOther needs is derived from
        // `(default_cc, all_vns)`, so there is no synthesised stand-in to
        // overwrite afterwards.
        let mut fb = FunctionBuilder {
            function: Function::new(cc.clone(), endianness, all_vns, vn_to_container),
            var_table,
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
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

    /// Sets the function-default convention's stack-argument layout.
    /// Prod sets stack args through the lift path; used only by tests.
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_stack_args(&mut self, stack_args: Option<strider_target::StackArgs>) {
        self.function.default_cc.stack_args = stack_args;
    }

    /// Returns the currently-attributed asm address (or `None` if no insn
    /// is active).  The setter `set_lift_addr` is prod; this read-back is
    /// used only by tests.
    #[inline]
    #[cfg(test)]
    pub fn lift_addr(&self) -> Option<u64> {
        self.lift_addr
    }

    /// Creates a node in the graph with the given kind, inputs, and
    /// output kinds.  When the attributed lift address is `Some(addr)`, also
    /// records `addr` in the resulting node's asm-fingerprint side-table
    /// entry; if `create_node` hits the dedup cache, the contributor is
    /// unioned into the existing entry.
    ///
    /// Routes through [`Function::create_node_attributed`] so that
    /// integer-constant canonicalisation — masking `IntConst(Small)` to the
    /// declared width, plus the `Small` → `Wide` promotion for I80/I128/I256/I512
    /// — is applied on every creation path (not just `EditFunction` / the
    /// template engine).
    pub(crate) fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        let addr = self.lift_addr;
        // Empty contributors slice: no extra fingerprint merge beyond the
        // lift_addr stamp applied below.
        let node_id = self
            .function_mut()
            .create_node_attributed(kind, inputs, output_kinds, &[]);
        if let Some(addr) = addr {
            self.function_mut().extend_asm_fingerprint(node_id, &[addr]);
        }
        node_id
    }

    /// Returns an iterator over all tracked varnodes.  Used only by tests to
    /// assert builder canonicalisation.
    #[cfg(test)]
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.var_table.values()
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
        crate::validate::validate(&self.function)?;
        Ok(self.function)
    }
}
