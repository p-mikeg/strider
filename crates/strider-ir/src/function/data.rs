//! [`Function`] — a [`Graph`] plus per-function overlay state (`entry`,
//! calling convention, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges, dedup cache).
//! [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, the wide-const interner, and the
//! other `NodeId`-keyed side tables.
//!
//! Passes that only need structure take `&Graph`; passes that need the overlay
//! (most opt passes, the validator, dot rendering) take `&Function` or
//! `&mut Function`.
//!
//! A small set of read-only [`Graph`] accessors are forwarded as inherent
//! methods on [`Function`] (see the delegating `impl` block below); every
//! other [`Graph`] method is reached explicitly through [`Function::graph`] /
//! [`Function::graph_mut`].

use cranelift_entity::SecondaryMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::IRViewer;
use crate::IRWalker;
use crate::graph::{Graph, NodeIdRemap, SideTableRemap};
use crate::node::{NodeId, NodeKind, ValueId};

/// Largest varnode in `vns` (same REGISTER/UNIQUE space, offset-range
/// inclusion) that fully contains `vn`, or `vn` itself when none does.
///
/// Returns `*vn` unchanged when `vn` is not in an aliasable
/// (REGISTER/UNIQUE) space — containment-by-offset is meaningless for
/// CONST / RAM / code-space varnodes.  Otherwise it picks the largest
/// same-space element of `vns` whose `[off, off+size)` (saturating) range
/// fully encloses `vn`'s range, falling back to `*vn` when nothing does.
///
/// This is the single linear containment scan shared by
/// [`Function::container_of`]'s fallback and the bulk `vn_to_container`
/// map build in `FunctionBuilder::new`.
pub(crate) fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if vn.addr_space != rsleigh::VnSpace::REGISTER && vn.addr_space != rsleigh::VnSpace::UNIQUE {
        return *vn;
    }
    let start = vn.addr_off;
    let end = start.saturating_add(u64::from(vn.size));
    let mut best: Option<rsleigh::Vn> = None;
    for cand in vns {
        if cand.addr_space != vn.addr_space {
            continue;
        }
        let cs = cand.addr_off;
        let ce = cs.saturating_add(u64::from(cand.size));
        if cs > start || ce < end {
            continue;
        }
        if best.is_none_or(|b| b.size < cand.size) {
            best = Some(*cand);
        }
    }
    best.unwrap_or(*vn)
}

/// Drains an `FxHashMap` and rebuilds it through a per-entry translation,
/// keeping only entries `f` maps to `Some((new_key, new_payload))`.
///
/// The Vn-keyed / `ValueId`-keyed / index-keyed overlay maps in
/// [`Function::compact`] each remap a different facet (the key, the payload,
/// or a payload `Vec`), so they don't fit the `NodeId`-keyed
/// [`SideTableRemap`] shape — this folds their shared drain-rebuild loop
/// behind a single closure.
fn remap_hashmap<K, V, NK, NV>(
    map: &mut FxHashMap<K, V>,
    mut f: impl FnMut(K, V) -> Option<(NK, NV)>,
) -> FxHashMap<NK, NV>
where
    NK: std::hash::Hash + Eq,
{
    let mut dst = FxHashMap::with_capacity_and_hasher(map.len(), Default::default());
    for (old_key, old_payload) in map.drain() {
        if let Some((new_key, new_payload)) = f(old_key, old_payload) {
            dst.insert(new_key, new_payload);
        }
    }
    dst
}

/// A lifted function: structural [`Graph`] plus per-function overlay state.
///
/// `FunctionBuilder::build` is the canonical constructor.  For synthetic /
/// test graphs, use [`Function::new`] and populate via [`Function::graph_mut`]
/// and [`Function::set_entry`].
///
/// A small set of read-only [`Graph`] accessors (e.g. `node_kind`,
/// `node_outputs`, `value_kind`) are forwarded as inherent methods on
/// `Function`; every other [`Graph`] method is reached explicitly through
/// [`Function::graph`] / [`Function::graph_mut`].
#[derive(Default, Clone)]
pub struct Function {
    pub(crate) graph: Graph,
    entry: Option<NodeId>,

    // ── calling-convention overlay ─────────────────────────────────────────
    //
    // `default_cc` (the resolved convention) and `all_vns` (the ordered
    // tracked-varnode set) are the two genuinely-non-derivable inputs.
    // Every register-list projection a `Call` / `CallOther` / `Return`
    // node needs is *derived* from these two via the accessors below
    // (`call_clobbered_for`, `ret_val_regs`, `call_other_clobbered`) —
    // there are no cached projected lists, so a per-address-CC override
    // produces a correct per-call clobber set by deriving against that
    // call's effective CC over the same `all_vns`.
    /// The calling convention this function was built under.  Always a
    /// real value: production functions carry their resolved target ABI;
    /// synthetic test functions constructed via [`Self::new`] / the
    /// `Default` derive without a real CC carry the *trivial* convention
    /// ([`strider_target::BuiltCallingConvention::default`]) — empty reg
    /// lists with a synthetic `stack_vn` (a real, sized register at an
    /// out-of-range offset that matches no tracked register), so stack
    /// analyses no-op.  Pure ABI facts (`stack_vn`, `ret_stack_pop`,
    /// `preserves_memory`, link register) are read through this and
    /// surfaced by the [`Self::stack_vn`] / [`Self::ret_stack_pop`] /
    /// [`Self::preserves_memory`] accessors.  The convention's
    /// `arg_passing_regs` / `ret_val_regs` / `callee_saved_regs` drive the
    /// register-list derivations ([`Self::call_clobbered_for`],
    /// [`Self::ret_val_regs`]).
    pub(crate) default_cc: strider_target::BuiltCallingConvention,
    /// Target endianness of the architecture this function was lifted
    /// for.  Drives the bit-shift formula the builder's register-aliasing
    /// path uses when reading / writing a sub-register inside a wider
    /// container (see [`crate::FunctionBuilder::read_reg_vn`]).  A `Copy`
    /// scalar (so [`Self::compact`] needs no remap for it); defaults to
    /// little-endian on the [`Default`]-derived / synthetic-test path.
    pub(crate) endianness: strider_target::Endianness,
    /// Ordered list of every tracked varnode, in `InitialVar`-creation
    /// (allocation) order.  Single source of truth for the function's
    /// tracked-variable SET *and* the slot ordering of derived clobber
    /// lists (so the `i`-th `Call` clobber output still corresponds to
    /// the `i`-th derived clobber varnode).  `VarId` is a build-time-only
    /// SSA key on the [`crate::FunctionBuilder`]; this is the post-build
    /// replacement.  Holds plain `rsleigh::Vn`s (no arena ids), so
    /// [`Self::compact`] leaves it untouched.
    pub(crate) all_vns: Vec<rsleigh::Vn>,
    /// `original vn → its largest tracked container` map. Domain: every
    /// REGISTER/UNIQUE varnode in the pre-dedup tracked set *plus* every
    /// register the calling convention names (arg / ret / float-ret /
    /// stack / callee-saved), so a CC register narrower than its tracked
    /// container (ABI says `eax`, function tracks `rax`) resolves to the
    /// container. Codomain: an element of `all_vns`, or the key itself when
    /// no wider tracked vn contains it. Const / RAM vns are NOT canonicalized
    /// (left out of the map). Computed once in `FunctionBuilder::new`. Plain
    /// `rsleigh::Vn` keys/values (no arena ids), so `compact` leaves it
    /// untouched. Read through [`Self::container_of`].
    pub(crate) vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>,

    // ── overlay tables ─────────────────────────────────────────────────────
    //
    // These side tables hold per-function data that is keyed by NodeId (or, for
    // `value_vn`, by a node's output ValueId) but is not part of the
    // structural graph identity.  They are remapped by [`Self::compact`]
    // whenever the arena is compacted.
    /// User-op name resolved from Sleigh for [`crate::node::NodeKind::CallOther`]
    /// nodes.
    pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>,
    /// Per-node sorted-deduplicated list of machine-instruction addresses
    /// whose lifting or rewrite contributed to the node's value.
    // `SmallVec<[u64; 2]>` because the common case is 1–2 contributor
    // addresses per node.  Inlining those avoids a heap allocation per
    // non-empty entry — on graphs with thousands of nodes this drops
    // thousands of small allocations from the lift+optimize pipeline.
    // The mutation API (`extend_asm_fingerprint`,
    // `extend_asm_fingerprint_from`) keeps using `&[u64]` /
    // `impl IntoIterator<Item = u64>` so callers are unaffected.
    pub(crate) asm_fingerprints: SecondaryMap<NodeId, smallvec::SmallVec<[u64; 2]>>,
    /// The varnode a value *represents*, keyed by [`ValueId`].  Two
    /// disjoint populations share this one map:
    ///
    /// * A lift-time [`crate::node::NodeKind::Phi`]'s single output value →
    ///   the source-level varnode the phi tracks.  Absent entries mark
    ///   anonymous phis synthesised by opt passes (and every non-phi,
    ///   non-clobber value).
    /// * A [`crate::node::NodeKind::Call`] / [`crate::node::NodeKind::CallOther`]
    ///   clobber output value → the register that call clobbers.  Set for
    ///   every clobber output at build time (both the function-default and
    ///   the override / implicit-write paths), so a clobber output's
    ///   varnode is recovered with a single lookup, no slot arithmetic.
    ///
    /// Keyed by `ValueId` (not `NodeId`) so it remaps through the
    /// `ValueId` translation that [`Self::compact`] applies.
    pub(crate) value_vn: FxHashMap<ValueId, rsleigh::Vn>,
    /// Per-[`crate::node::NodeKind::Call`] or
    /// [`crate::node::NodeKind::CallOther`] descriptor, recorded at build
    /// time for non-default calls:
    ///
    /// - `Call` nodes built with a per-address CC override store
    ///   [`crate::CallDescriptor::Call`].
    /// - Modeled `CallOther` nodes store
    ///   [`crate::CallDescriptor::CallOther`] with the vn-resolved ABI.
    ///
    /// Sparse: the default Call (function-default CC) and unmodeled
    /// `CallOther` nodes have no entry.  Stack-arg offsets for override
    /// `Call`s are derived from the stored CC via
    /// [`Self::call_stack_args_override`].  The convenience accessor
    /// [`Self::call_cc`] returns `Some` only for the `Call` arm.
    pub(crate) call_descriptor: FxHashMap<NodeId, crate::CallDescriptor>,

    /// Maps each calling-convention argument index to the [`ValueId`](s) of
    /// the underlying carrier nodes' outputs:
    /// [`crate::node::NodeKind::InitialVar`] for register args,
    /// [`crate::node::NodeKind::Load`] for stack args.  Each carrier node has
    /// a single output, so the carrier node is recoverable losslessly via
    /// [`Graph::producer`].
    ///
    /// `Vec<ValueId>` per index because a stack slot may have multiple `Load`
    /// nodes at the same `sp+K` offset but different widths.  Register args
    /// have a `Vec` of size 1.
    ///
    /// Populated by `FunctionArgDetect`; empty until that pass runs.
    arg_index_to_values: FxHashMap<u32, Vec<ValueId>>,

    /// Stack slot for Store/Load nodes whose address decomposes to
    /// `base + K` for a single concrete `K`, where `base` is the SP-derived
    /// terminal node (`InitialVar(sp)` or an alignment-masked `sp & -16`).
    /// Stored as `(base, K)`: the offset `K` is only meaningful relative to
    /// its `base`, and two accesses are the same slot iff they share both.
    /// Populated by the `StackOffsetDetect` classifier.  The phi-of-offsets
    /// case (address is a phi of different constants per branch) is not
    /// recorded — consumers can re-decompose via `decompose_sp` if needed.
    stack_offsets: SecondaryMap<NodeId, Option<(ValueId, i64)>>,

    /// O(1) varnode → `InitialVar(vn)` node-id accelerator for
    /// indirect-resolve sites and the lifter's lazy `read_or_init_var`
    /// fallback.  Maintained at every canonical `InitialVar`
    /// creation site (the lift-time path and the orchestrator
    /// fallback) and remapped through [`NodeIdRemap`] by
    /// [`Self::compact`].
    initial_var_index: FxHashMap<rsleigh::Vn, NodeId>,

    /// Wide-integer constant values (I80, I128, I256, I512) referenced by
    /// `IntConst(IntPayload::Wide(id))` nodes.
    ///
    /// Values wider than 64 bits don't fit in `IntPayload::Small`; the IR
    /// stores them off-side here and the node carries a
    /// `crate::wide_const::WideConstId` index instead.  Interning
    /// (via [`Self::intern_wide_const`]) dedups by value so two
    /// `IntConst(Wide(id))` nodes referencing the same logical value are
    /// structurally equal under [`Graph::create_node`]'s dedup cache.
    /// An [`entity_utils::EntityInterner`] owns both the forward
    /// `WideConstId → value` map and the reverse value-dedup index.
    /// Rebuilt over the live ids by [`Self::compact`].
    pub(crate) wide_const_interner: entity_utils::EntityInterner<
        crate::wide_const::WideConstId,
        crate::wide_const::WideConstStorage,
    >,
}

impl Function {
    /// Creates a `Function` with an empty graph and no entry node, carrying
    /// the calling-convention SSoT (`default_cc`, `endianness`, `all_vns`)
    /// at construction.  These three are the non-derivable inputs every
    /// register-list projection a `Call` / `Return` / `CallOther` needs is
    /// derived from, so requiring them here guarantees a `Function` is never
    /// observed in a half-initialised state (no build-then-assign window).
    ///
    /// Synthetic / test graphs that don't care about a convention use
    /// [`Self::default`] (the trivial CC, little-endian, no tracked
    /// varnodes) — an equally-complete but convention-free starting point.
    pub fn new(
        default_cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
        all_vns: Vec<rsleigh::Vn>,
        vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
    ) -> Self {
        Self {
            default_cc,
            endianness,
            all_vns,
            vn_to_container,
            ..Self::default()
        }
    }

    /// Returns a shared reference to the underlying graph.
    #[inline]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns a mutable reference to the underlying graph.
    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Interns `value` and returns its `crate::wide_const::WideConstId`.
    /// Subsequent calls with an equal value return the same id — the
    /// dedup invariant the [`Graph::create_node`] cache relies on so
    /// two `IntConst(Wide(id))` nodes referencing the same logical value
    /// share a single `NodeId`.
    pub fn intern_wide_const(
        &mut self,
        value: crate::wide_const::WideConstStorage,
    ) -> crate::wide_const::WideConstId {
        self.wide_const_interner.intern(value)
    }

    /// Looks up a wide-const value by id.  The id must have been
    /// produced by `intern_wide_const` on this function; ids
    /// from other functions are not portable.
    pub fn wide_const(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> &crate::wide_const::WideConstStorage {
        &self.wide_const_interner[id]
    }

    /// Non-panicking variant of [`Self::wide_const`]: returns `None` for a
    /// dangling id rather than panicking.  The debug renderers use this so
    /// they can label a malformed graph (e.g. one inspected mid-rewrite)
    /// instead of aborting.
    pub fn wide_const_opt(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> Option<&crate::wide_const::WideConstStorage> {
        self.wide_const_interner.get(id)
    }

    /// Returns the entry node, if one has been recorded.
    #[inline]
    pub fn entry(&self) -> Option<NodeId> {
        self.entry
    }

    /// Records `entry` as the function's entry node.
    #[inline]
    pub fn set_entry(&mut self, entry: NodeId) {
        self.entry = Some(entry);
    }

    /// Read-only access to the calling convention this function was built
    /// under.  Always present: synthetic functions built without a real
    /// CC carry the trivial convention
    /// ([`strider_target::BuiltCallingConvention::default`]).
    #[inline]
    pub fn default_cc(&self) -> &strider_target::BuiltCallingConvention {
        &self.default_cc
    }

    /// Target endianness of the architecture this function was lifted for.
    /// Consumed by the builder's register-aliasing bit-shift formula
    /// ([`crate::FunctionBuilder::read_reg_vn`] /
    /// [`crate::FunctionBuilder::write_reg_vn`]).
    #[inline]
    pub fn endianness(&self) -> strider_target::Endianness {
        self.endianness
    }

    /// The ordered tracked-varnode SSoT.
    pub fn all_vns(&self) -> &[rsleigh::Vn] {
        &self.all_vns
    }

    /// Resolve `vn` to its largest tracked container.
    ///
    /// Fast path: the precomputed `vn_to_container` map (covers
    /// every original REGISTER/UNIQUE tracked vn + every CC register).
    /// Fallback: an on-the-fly containment scan of `all_vns` for ad-hoc
    /// REGISTER/UNIQUE vns not in the map. Returns `vn` unchanged when
    /// nothing tracked contains it, or when `vn` is not in an aliasable
    /// (REGISTER/UNIQUE) space.
    pub fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        if let Some(c) = self.vn_to_container.get(vn) {
            return *c;
        }
        largest_container_in(&self.all_vns, vn)
    }

    /// Derive the ret-val varnode list for a `Call` built under calling
    /// convention `cc`.  Returns only those tracked, clobbered varnodes
    /// that appear in the convention's combined return-register list
    /// (`ret_val_regs` then `ret_val_regs_float`), in ABI order.
    ///
    /// This is the first group of Call output slots past `[Control,
    /// Memory]`.  Together with [`Self::call_clobbered_for`] it partitions
    /// what was formerly a single clobber tail into two labeled groups —
    /// the slot ORDER is unchanged; only the conceptual split is new.
    ///
    /// Each CC register (ret-val, float-ret, callee-saved) is resolved to
    /// its tracked container via [`Self::container_of`] before membership
    /// is tested, and the resolved CONTAINER is emitted.  This keeps a
    /// sub-register ABI ret reg (e.g. `eax`) classified as the return
    /// value when the function tracks the wider container (`rax`) instead
    /// of silently dropping it.  Identity on full-width preset regs.
    /// The shared call-clobber predicate: a register (resolved to its tracked
    /// container) is clobbered iff it is neither callee-saved under `cc` nor the
    /// stack pointer.  The callee-saved set is hashed once so the predicate is
    /// O(1) per probe (keeping the `call_*_for` derivations O(N), not O(N·M)).
    /// CC regs are resolved to their tracked container first so a sub-register
    /// ABI reg matches the wider tracked vn.
    fn clobber_oracle(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> impl Fn(&rsleigh::Vn) -> bool + use<> {
        let stack_vn = self.default_cc.stack_vn;
        let callee_saved: FxHashSet<rsleigh::Vn> = cc
            .callee_saved_regs
            .iter()
            .map(|v| self.container_of(v))
            .collect();
        move |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn
    }

    pub fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let is_clobbered = self.clobber_oracle(cc);
        let tracked: FxHashSet<rsleigh::Vn> = self.all_vns.iter().copied().collect();
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| self.container_of(v))
            .filter(|c| tracked.contains(c) && is_clobbered(c))
            .collect()
    }

    /// Derive the call-clobbered varnode list for a `Call` built under
    /// calling convention `cc`, in the canonical slot order.
    ///
    /// Returns ONLY the non-ret caller-saved registers.  The ret-val
    /// registers (formerly the "ret_prefix" front-loaded by the old
    /// `build_call_clobbered_list`) are now emitted as a separate group
    /// by [`Self::call_ret_vals_for`].  A varnode is clobbered iff it is
    /// neither in `cc.callee_saved_regs` nor the function's stack pointer,
    /// AND it is not in the convention's combined ret-val register list.
    /// All elements are drawn from `all_vns` in allocation order.
    ///
    /// Each CC register (callee-saved, ret-val, float-ret) is resolved to
    /// its tracked container via [`Self::container_of`] before it is used
    /// to exclude entries here, so a sub-register ABI ret reg (e.g. `eax`)
    /// whose tracked container is wider (`rax`) is correctly excluded from
    /// the clobber tail rather than mis-filed as a clobber.  Identity on
    /// full-width preset regs.
    ///
    /// To obtain the FULL combined set (ret-vals ++ clobbers) for callers
    /// that need the old single-list shape, chain the two accessors:
    /// `call_ret_vals_for(cc).into_iter().chain(call_clobbered_for(cc))`.
    pub fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        // The clobber predicate (callee-saved + stack-vn exclusion) is shared
        // with `call_ret_vals_for` via `clobber_oracle`.  The output ORDER
        // (`all_vns` allocation order) is unchanged.
        let is_clobbered = self.clobber_oracle(cc);
        // The combined ret-reg list, resolved to tracked containers: the
        // ret-val group is emitted separately by `call_ret_vals_for`, so
        // exclude its containers here.
        let ret_vars: FxHashSet<rsleigh::Vn> = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| self.container_of(v))
            .collect();
        self.all_vns
            .iter()
            .copied()
            .filter(|v| is_clobbered(v) && !ret_vars.contains(v))
            .collect()
    }

    /// The function-default call-clobbered varnode list (derived against
    /// [`Self::default_cc`]).  Convenience for consumers that want the
    /// default-CC shape; per-address-override `Call`s derive against
    /// their own CC via [`Self::call_clobbered_for`].
    ///
    /// Returns only the NON-ret caller-saved registers.  To get the full
    /// set (ret-vals ++ clobbers), chain with
    /// `call_ret_vals_for(default_cc())`.
    #[inline]
    pub fn call_clobbered_regs(&self) -> Vec<rsleigh::Vn> {
        self.call_clobbered_for(&self.default_cc)
    }

    /// The function-default ret-val varnode list (derived against
    /// [`Self::default_cc`]).  Convenience for consumers that want the
    /// default-CC ret-val shape.
    #[inline]
    pub fn call_ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        self.call_ret_vals_for(&self.default_cc)
    }

    /// The calling convention's combined return-value register list
    /// (integer then float, in ABI order), at each register's declared
    /// width — no tracked-container projection.  The registers are read
    /// through the aliasing-aware [`crate::FunctionBuilder::read_reg_vn`]
    /// at use sites, which resolves each declared register to its tracked
    /// container (and errors if none exists), so the raw declared list is
    /// the right shape: a wider register (e.g. `RSI`) is read at its full
    /// width rather than being narrowed to a tracked sub-register.
    #[inline]
    pub fn ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        self.default_cc
            .ret_val_regs
            .iter()
            .chain(self.default_cc.ret_val_regs_float.iter())
            .copied()
            .collect()
    }

    /// Calling convention's stack-pointer varnode.  On the trivial CC
    /// carried by synthetic test functions this is a synthetic register
    /// at an out-of-range offset that matches no tracked register, so
    /// SP-keyed analyses simply find no matches.
    #[inline]
    pub(crate) fn stack_vn(&self) -> rsleigh::Vn {
        self.default_cc.stack_vn
    }

    /// The function-default `CallOther` clobber list: every tracked
    /// varnode except the stack pointer, in `all_vns` order.
    /// Reproduces the old build-time `call_other_clobbered` (`build()`
    /// filtered `var_table.values()` — same order as `all_vns` — by
    /// `!= stack_vn`).
    #[inline]
    pub fn call_other_clobbered_regs(&self) -> Vec<rsleigh::Vn> {
        let stack_vn = self.default_cc.stack_vn;
        self.all_vns
            .iter()
            .copied()
            .filter(|v| *v != stack_vn)
            .collect()
    }

    // ── NodeId-keyed overlay accessors ────────────────────────────────────

    /// Returns the user-op name associated with a
    /// [`crate::node::NodeKind::CallOther`] node, or `None` if no name has
    /// been recorded for that node.
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names[node_id].as_deref()
    }

    /// Associates a user-op name with a [`crate::node::NodeKind::CallOther`]
    /// node.  Replaces any prior value.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: String) {
        self.call_other_names[node_id] = Some(name);
    }

    /// Returns the source varnode a value represents, or `None`. Single
    /// value-keyed view over `value_vn`, which tags three populations: a
    /// lift-time `Phi`'s tracked varnode, a `Call`/`CallOther` ret-val
    /// output's return register, and a `Call`/`CallOther` clobber output's
    /// clobbered register.
    #[inline]
    pub fn get_vn_for_value(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.value_vn.get(&value).copied()
    }

    /// Records that `value` represents varnode `vn`. Replaces any prior value.
    ///
    /// Valid targets mirror the populations [`Self::get_vn_for_value`] reads: a
    /// `Phi`'s single typed output, or a `Call`/`CallOther` ret-val / clobber
    /// output. Not for control / memory / phi-token edges.
    #[inline]
    pub fn set_vn_for_value(&mut self, value: ValueId, vn: rsleigh::Vn) {
        self.value_vn.insert(value, vn);
    }

    /// Returns the [`crate::CallDescriptor`] recorded for `node_id`, or
    /// `None` when no descriptor has been recorded (default Call or unmodeled
    /// CallOther).
    #[inline]
    pub fn call_descriptor(&self, node_id: NodeId) -> Option<&crate::CallDescriptor> {
        self.call_descriptor.get(&node_id)
    }

    /// Records `descriptor` for `node_id`.  Replaces any prior value.
    #[inline]
    pub fn set_call_descriptor(&mut self, node_id: NodeId, descriptor: crate::CallDescriptor) {
        self.call_descriptor.insert(node_id, descriptor);
    }

    /// Convenience accessor: returns the override calling convention recorded
    /// for a `Call` node, or `None` when the Call uses the function-default CC
    /// or the node has a `CallOther` descriptor.
    ///
    /// Consumers that only need to distinguish "override CC present" from
    /// "function-default" can use this without importing [`crate::CallDescriptor`].
    #[inline]
    pub fn call_cc(&self, node_id: NodeId) -> Option<&strider_target::BuiltCallingConvention> {
        match self.call_descriptor.get(&node_id)? {
            crate::CallDescriptor::Call(cc) => Some(cc),
            crate::CallDescriptor::CallOther(_) => None,
        }
    }

    /// Records `cc` as the per-Call override calling convention for
    /// `node_id`, wrapping it in [`crate::CallDescriptor::Call`].  Replaces
    /// any prior descriptor.  Subsumes the stack-arg layout override (read
    /// back via [`Self::call_stack_args_override`]).
    ///
    /// Prefer [`Self::set_call_descriptor`] when the call site already has a
    /// `CallDescriptor` value; this wrapper exists for call sites that only
    /// deal with `BuiltCallingConvention`.
    #[inline]
    pub fn set_call_cc(&mut self, node_id: NodeId, cc: strider_target::BuiltCallingConvention) {
        self.call_descriptor
            .insert(node_id, crate::CallDescriptor::Call(cc));
    }

    /// The per-`Call` override convention's stack-arg layout, if this Call
    /// node was built with a CC override; `None` for default calls or a
    /// modeled `CallOther`.
    #[inline]
    pub fn call_stack_args_override(&self, node_id: NodeId) -> Option<strider_target::StackArgs> {
        match self.call_descriptor.get(&node_id)? {
            crate::CallDescriptor::Call(cc) => cc.stack_args,
            crate::CallDescriptor::CallOther(_) => None,
        }
    }

    // ── arg_index_to_values accessors ────────────────────────────────────

    /// All carrier output [`ValueId`]s registered for argument `index`.
    ///
    /// Returns `&[]` if no carriers have been registered for that index.
    /// Register args have a slice of length 1; stack args may have multiple
    /// entries (different-width [`crate::node::NodeKind::Load`]s at the same
    /// `sp+K` offset).  Each value's carrier node is recoverable via
    /// [`Graph::producer`].
    #[inline]
    pub fn arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.arg_index_to_values
            .get(&index)
            .map_or(&[], Vec::as_slice)
    }

    /// Register `value` (a carrier node's single output) as a carrier for
    /// argument `index`.
    ///
    /// Appends to the per-index `Vec`; multiple values per index are allowed
    /// (the stack-args case may register multiple `Load`s at different widths
    /// for the same offset).
    #[inline]
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry(index)
            .or_default()
            .push(value);
    }

    /// Iterate over all registered argument indices (unordered).
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_values.keys().copied()
    }

    /// Drop registered argument carriers for every index `>= first`.
    ///
    /// Lets the stack-arg detection pass rebuild only the stack-arg portion of
    /// the table idempotently across the orchestrator's stable iterations,
    /// without disturbing the register-arg carriers recorded at builder entry
    /// (which occupy indices `0 .. first`).
    #[inline]
    pub fn clear_arg_values_from(&mut self, first: u32) {
        self.arg_index_to_values.retain(|&index, _| index < first);
    }

    // ── stack_offsets accessors ───────────────────────────────────────────

    /// Returns the stack slot `(base, offset)` recorded for a Store/Load
    /// node, or `None` if the node has no recorded slot (non-stack node, or
    /// a phi-of-offsets address whose single concrete offset cannot be
    /// named).  `base` is the SP-derived terminal node the offset is
    /// relative to; the offset is only comparable against another access's
    /// offset when their bases match.
    #[inline]
    pub fn stack_offset(&self, id: NodeId) -> Option<(ValueId, i64)> {
        self.stack_offsets[id]
    }

    /// Records a concrete stack slot `(base, offset)` for a Store/Load node.
    #[inline]
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i64) {
        self.stack_offsets[id] = Some((base, offset));
    }

    // ── initial_var_index accessors ───────────────────────────────────────

    /// Returns the [`NodeId`] of the canonical `InitialVar(vn)` node for
    /// `vn`, or `None` if none is registered.  O(1).
    ///
    /// Callers that want to skip detached zombie nodes must validate the
    /// returned id themselves (typically by checking that the node's
    /// single output's use-list is non-empty via [`Graph::value_uses`]).
    #[inline]
    pub fn initial_var_for(&self, vn: rsleigh::Vn) -> Option<NodeId> {
        self.initial_var_index.get(&vn).copied()
    }

    /// Registers `(vn, node_id)` in the `InitialVar` index.  Replaces
    /// any prior entry for `vn`.  Callers must guarantee that
    /// `node_id`'s kind is `NodeKind::InitialVar(vn)` — the index is
    /// advisory and never re-checked.
    #[inline]
    pub fn register_initial_var(&mut self, vn: rsleigh::Vn, node_id: NodeId) {
        self.initial_var_index.insert(vn, node_id);
    }

    /// Returns the entry stack-pointer value — the output of the
    /// `InitialVar(stack_vn)` node, where `stack_vn` is the calling
    /// convention's stack pointer — or `None` when the function never
    /// reads it.
    ///
    /// Walks the **entry-reachable** graph (not the `initial_var_index`,
    /// which can hold a node culled-but-not-yet-compacted mid-pipeline)
    /// so a detached-zombie `InitialVar(sp)` is skipped.  Exactly one
    /// `InitialVar(stack_vn)` exists (builder invariant), so the search is
    /// order-independent.  Consumers (e.g. stack-arg detection) require
    /// every candidate's terminal SP base to equal this value.
    pub fn initial_sp_value(&self) -> Option<ValueId> {
        let stack_vn = self.default_cc.stack_vn;
        for n in self.reverse_postorder_filter(|k| matches!(k, NodeKind::InitialVar(_))) {
            if matches!(*self.node_kind(n), NodeKind::InitialVar(vn) if vn == stack_vn) {
                let [out] = self
                    .node_outputs_exact::<1>(n)
                    .expect("InitialVar has 1 output per node signature");
                return Some(out);
            }
        }
        None
    }

    /// Returns the asm-instruction-address fingerprint of `node_id` as a
    /// sorted-deduplicated slice.  Returns an empty slice when no
    /// contributors have been recorded.
    #[inline]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.asm_fingerprints[id].as_slice()
    }

    /// Unions `contributors` into `node_id`'s fingerprint.  Result is kept
    /// sorted and deduplicated.  Existing entries are never removed: this
    /// satisfies the no-shrink contract.  Empty `contributors` is a no-op.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        if contributors.is_empty() {
            return;
        }
        let existing = &mut self.asm_fingerprints[node_id];
        let mut needs_resort = false;
        for &addr in contributors {
            match existing.last() {
                None => existing.push(addr),
                Some(&last) if addr > last => existing.push(addr),
                Some(&last) if addr == last => { /* already present */ }
                Some(_) => {
                    existing.push(addr);
                    needs_resort = true;
                }
            }
        }
        if needs_resort {
            existing.sort_unstable();
            existing.dedup();
        }
    }

    /// Unions the fingerprint of `src` into `dst`.  Self-extension
    /// (`src == dst`) is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        let src_slice: smallvec::SmallVec<[u64; 4]> =
            self.asm_fingerprints[src].iter().copied().collect();
        self.extend_asm_fingerprint(dst, &src_slice);
    }

    /// Same as [`Graph::create_node`] plus unions the asm-fingerprint of
    /// every node in `contributors` into the resulting node, and masks any
    /// `IntConst(Small)` payload to its declared integer output width so
    /// every creation path produces the same canonical form:
    ///
    /// * narrow (≤ I64): payload stays `Small`, masked to the declared width.
    /// * wide (I80 / I128 / I256 / I512): promoted to `IntConst(Wide)` via
    ///   the wide-const interner so no inline `Small` payload holds > 64 bits.
    ///
    /// This is the canonical node-creation funnel for ALL mutable paths:
    /// `FunctionBuilder::create_node` (the lift-time path),
    /// [`crate::EditFunction::create_node_attributed`] (the rewrite /
    /// template-engine path), and any direct caller.  Routing every
    /// creation through here is what makes the IntConst-masking invariant
    /// hold workspace-wide without any knowledge of it in the generic
    /// dedup-cache layer.
    pub fn create_node_attributed(
        &mut self,
        kind: crate::node::NodeKind,
        inputs: impl IntoIterator<Item = crate::node::ValueId>,
        output_kinds: impl IntoIterator<Item = crate::node::ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        // Collect output kinds so we can inspect the declared type for the
        // IntConst normalisation below.
        let output_kinds: smallvec::SmallVec<[crate::node::ValueKind; 4]> =
            output_kinds.into_iter().collect();
        // Canonicalise the `IntConst` payload by VALUE, not by declared type:
        // the wide interner is reserved for values that genuinely exceed
        // `u64`; everything else lives inline as `Small` with the width
        // carried by the output `ValueKind`.
        // Mask an inline `u64` to the declared integer width.  Masking only
        // clears bits, so the result always fits `u64` for ANY declared type —
        // an inline value therefore stays `Small` regardless of width
        // (I80/I128/I256/I512 included).  Shared by the `Small` and the
        // `Wide`-fits-`u64` arms so both produce an identical canonical payload.
        let mask_inline = |v: u64| -> u64 {
            match output_kinds.first().and_then(|vk| vk.as_value()) {
                Some(ty) if ty.is_integer() => {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        (u128::from(v) & ty.bit_mask_u128()) as u64
                    }
                }
                _ => v,
            }
        };
        let kind = match kind {
            crate::node::NodeKind::IntConst(crate::node::IntPayload::Small(v)) => {
                crate::node::NodeKind::IntConst(crate::node::IntPayload::Small(mask_inline(v)))
            }
            // A `Wide` payload whose interned value fits `u64` is the inline
            // `Small` form in disguise — canonicalise it down so a given
            // (value, type) has exactly one representation (preserving dedup).
            // Genuinely-wide values pass through unchanged.
            crate::node::NodeKind::IntConst(crate::node::IntPayload::Wide(id)) => {
                match self.wide_const(id).as_u64() {
                    Some(v) => crate::node::NodeKind::IntConst(crate::node::IntPayload::Small(
                        mask_inline(v),
                    )),
                    None => crate::node::NodeKind::IntConst(crate::node::IntPayload::Wide(id)),
                }
            }
            other => other,
        };
        let node_id = self.graph.create_node(kind, inputs, output_kinds);
        for &src in contributors {
            self.extend_asm_fingerprint_from(node_id, src);
        }
        node_id
    }

    /// Compacts the arena down to the nodes reachable from `entry` via the
    /// control-aware walk (control-out forward + data-in backward), returning
    /// the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `ValueId` / `UseId` values are invalidated;
    /// callers holding any such id MUST rewrite it through the returned
    /// [`NodeIdRemap`].
    ///
    /// The generic `retain_reachable_roots` keeps the backward-input closure
    /// of its `roots`.  The IR's reachability also follows forward-control
    /// edges (so a `Region` reached only via control survives), so this seeds
    /// the generic compaction with the FULL control-aware reachable set: that
    /// set is already closed under data inputs, so its backward-input closure
    /// is itself — the generic pass then retains exactly the IR reachable set,
    /// and its cacher rebuild re-keys the dedup cache over the survivors.
    ///
    /// # Errors
    ///
    /// Currently infallible in practice; the `Result` is kept so a future
    /// invariant check has a typed channel and Python callers see a clean
    /// exception rather than a panic.
    pub fn retain_reachable(&mut self, entry: NodeId) -> crate::Result<NodeIdRemap> {
        // Collect the reachable set into a `Vec` first: that ends the
        // immutable borrow before the mutable `graph_mut()` borrow below.
        let reachable: Vec<NodeId> = self.walk_from(entry).collect();
        Ok(self.graph_mut().retain_reachable_roots(reachable))
    }

    /// Rebuilds the function's graph to retain only nodes reachable from
    /// [`Self::entry`].  The entry node id is remapped; the stored entry
    /// is updated to the new id.  Every `NodeId`-keyed overlay table
    /// (the `SecondaryMap` side-tables, `initial_var_index`, and
    /// `arg_index_to_values`) is remapped through the same translation;
    /// entries whose node did not survive compaction are dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::entry`] is `None`, or if the retain-
    /// reachable remap doesn't include the entry (invariant violation).
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        let entry = self
            .entry
            .ok_or_else(|| anyhow::anyhow!("Function::compact: entry node is not set"))?;
        let remap = self.retain_reachable(entry)?;
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "Function::compact: entry {:?} missing from remap (invariant violation)",
                entry
            )
        })?;
        self.entry = Some(new_entry);
        // Remap the NodeId-keyed overlay tables through the
        // old→new translation table produced by `retain_reachable`.
        self.call_other_names.remap_node_keyed(&remap);
        self.asm_fingerprints.remap_node_keyed(&remap);
        // `call_descriptor` is a sparse `FxHashMap<NodeId, _>` (calls are
        // rare and a descriptor payload can be large), so remap its KEYS
        // through the translation table, dropping entries whose Call /
        // CallOther node was pruned.
        let mut new_call_descriptor: FxHashMap<NodeId, crate::CallDescriptor> =
            FxHashMap::with_capacity_and_hasher(self.call_descriptor.len(), Default::default());
        for (old_node, descriptor) in self.call_descriptor.drain() {
            if let Some(new_node) = remap.node_old_to_new(old_node) {
                new_call_descriptor.insert(new_node, descriptor);
            }
        }
        self.call_descriptor = new_call_descriptor;
        // `stack_offsets` is the only NodeId-keyed side-table whose VALUE
        // also references a node — the slot `base` (a `ValueId`).  So
        // remap both the key (NodeId) and the value's base through the same
        // translation table.  An entry whose node or base didn't survive
        // compaction is dropped (the slot becomes "unknown", which is safe —
        // consumers treat a missing entry as non-stack).
        let mut new_stack_offsets: SecondaryMap<NodeId, Option<(ValueId, i64)>> =
            SecondaryMap::new();
        for (old_id, slot) in self.stack_offsets.iter() {
            let Some((old_base, off)) = *slot else {
                continue;
            };
            if let (Some(new_id), Some(new_base)) = (
                remap.node_old_to_new(old_id),
                remap.value_old_to_new(old_base),
            ) {
                new_stack_offsets[new_id] = Some((new_base, off));
            }
        }
        self.stack_offsets = new_stack_offsets;
        // `all_vns` is a `Vec<rsleigh::Vn>` with no node / value keys, and
        // `default_cc` holds `rsleigh::Vn` values (not arena ids), so
        // neither needs a remap.  (`default_cc` is always a real value —
        // the trivial CC for synthetic functions — never `None`.)
        // `vn_to_container` is likewise keyed and valued on plain
        // `rsleigh::Vn`s (not arena ids), so it needs no remap either.
        // `value_vn` is `FxHashMap<ValueId, Vn>` — keyed by a Phi's single
        // output value or a Call/CallOther clobber output value.  Translate
        // every key through the same `ValueId` remap; an entry whose value
        // did not survive compaction is dropped (the phi / clobber output
        // became unreachable).
        self.value_vn = remap_hashmap(&mut self.value_vn, |old_value, vn| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn))
        });
        // `initial_var_index` is `FxHashMap<Vn, NodeId>` — Vn-keyed, not
        // NodeId-keyed, so the standard `SecondaryMap` remap helper
        // doesn't fit.  Entries whose NodeId didn't survive compaction
        // (the InitialVar became unreachable and was dropped) are
        // silently elided — the orchestrator's `read_or_init_var`
        // fallback will lazily re-create them as needed.
        self.initial_var_index = remap_hashmap(&mut self.initial_var_index, |vn, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn, new_id))
        });
        // `arg_index_to_values` is `FxHashMap<u32, Vec<ValueId>>` —
        // index-keyed with `ValueId` payloads, so (like `initial_var_index`)
        // it needs an inline remap.  Carrier values whose value didn't
        // survive compaction are dropped; an index whose carriers all
        // vanished is removed entirely.
        self.arg_index_to_values =
            remap_hashmap(&mut self.arg_index_to_values, |index, old_values| {
                let mapped: Vec<ValueId> = old_values
                    .into_iter()
                    .filter_map(|old_value| remap.value_old_to_new(old_value))
                    .collect();
                (!mapped.is_empty()).then_some((index, mapped))
            });
        // GC the wide-const interner over only the values referenced by
        // surviving `IntConst(Wide(id))` nodes, rewriting each survivor's id
        // to the new dense id, then re-key the graph's dedup cache over those
        // rewritten ids.  The dedup cache keys on `NodeKind` (which carries
        // the `WideConstId`), so the rewrite must precede the cache rebuild —
        // exactly as it did when this GC lived inside `Graph::retain_reachable`
        // before the cache-rebuild step.
        if self.gc_wide_consts() {
            self.graph.rebuild_cache();
        }
        Ok(remap)
    }

    /// Rebuilds [`Self::wide_const_interner`] over only the values
    /// referenced by surviving `IntConst(Wide(id))` nodes, rewriting each
    /// such node's id in place to the new id assigned by the rebuilt
    /// interner.  Returns `true` iff at least one node's id was rewritten
    /// (so the caller knows whether the dedup cache must be re-keyed).
    /// Returns `false` when there are no surviving wide nodes — including
    /// the case where the graph previously had wide nodes that were all
    /// pruned by `retain_reachable`; in that case any stale interner
    /// entries are dropped and the cache needs no rebuild.
    ///
    /// Only safe to call after [`Graph::retain_reachable`] has settled
    /// the arena — at that point `self.graph.nodes.keys()` iterates only
    /// surviving nodes, so the live-id scan correctly excludes zombie
    /// references.
    fn gc_wide_consts(&mut self) -> bool {
        use crate::node::{IntPayload, NodeKind};
        use crate::wide_const::WideConstId;

        // Build the live-id set + collect every IntConst(Wide) node.
        let mut live_old_ids: Vec<WideConstId> = Vec::new();
        let mut wide_nodes: Vec<NodeId> = Vec::new();
        for node in self.graph.all_node_ids() {
            if let NodeKind::IntConst(IntPayload::Wide(id)) = *self.graph.node_kind(node) {
                wide_nodes.push(node);
                live_old_ids.push(id);
            }
        }
        if live_old_ids.is_empty() {
            // No surviving wide nodes — drop any stale interner entries
            // (e.g. zombie ids left by retain_reachable) and report that
            // no node id was rewritten, so the caller skips the rebuild.
            self.wide_const_interner = Default::default();
            return false;
        }

        // Rebuild the interner over only live values; `intern` dedups, so
        // distinct old ids that aliased one value collapse to one new id.
        let mut new_interner: entity_utils::EntityInterner<
            WideConstId,
            crate::wide_const::WideConstStorage,
        > = entity_utils::EntityInterner::default();
        let mut old_to_new: FxHashMap<WideConstId, WideConstId> = FxHashMap::default();
        for old_id in live_old_ids {
            if old_to_new.contains_key(&old_id) {
                continue;
            }
            let value = self.wide_const_interner[old_id].clone();
            let new_id = new_interner.intern(value);
            old_to_new.insert(old_id, new_id);
        }
        self.wide_const_interner = new_interner;

        // Rewrite the surviving IntConst(Wide(old_id)) nodes' payloads.
        for node in wide_nodes {
            if let NodeKind::IntConst(IntPayload::Wide(id)) = self.graph.node_kind_mut(node)
                && let Some(&new_id) = old_to_new.get(id)
            {
                *id = new_id;
            }
        }
        true
    }

    /// Returns a dot dumper for rendering this function's graph to HTML / DOT.
    ///
    /// # Errors
    ///
    /// Returns an error if `entry` is not set (i.e. the
    /// function has not been fully built).
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> crate::Result<crate::function::dot::FunctionDotDumper<'a, R>> {
        let entry = self
            .entry
            .ok_or_else(|| anyhow::anyhow!("Function::dot_dumper: entry node is not set"))?;
        let node_to_arg_indices = crate::function::dot::build_arg_reverse_map(self);
        Ok(crate::function::dot::FunctionDotDumper {
            entry,
            function: self,
            sleigh,
            node_to_arg_indices,
        })
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use super::Function;
    use crate::IRViewer;
    use crate::node::{NodeKind, ValueKind};

    #[test]
    fn function_new_carries_an_empty_graph() {
        let f = Function::default();
        assert_eq!(f.graph().all_node_ids().count(), 0);
        assert!(f.entry().is_none());
    }

    #[test]
    fn function_records_entry_via_set_entry() {
        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        f.set_entry(entry);
        assert_eq!(f.entry(), Some(entry));
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = Function::default();
        let n = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        f.extend_asm_fingerprint(n, &[0xDEAD_BEEF]);
        assert_eq!(f.asm_fingerprint(n), &[0xDEAD_BEEF]);
    }

    #[test]
    fn arg_index_to_values_returns_empty_for_unregistered() {
        let f = Function::default();
        assert!(f.arg_index_to_values(0).is_empty());
        assert!(f.arg_index_to_values(99).is_empty());
    }

    #[test]
    fn register_arg_value_supports_multiple_values_per_index() {
        let mut f = Function::default();
        let n1 = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let n2 = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let v1 = f.node_outputs(n1)[0];
        let v2 = f.node_outputs(n2)[0];

        // Register two values for arg index 3 (the stack-args multi-Load case).
        f.register_arg_value(3, v1);
        f.register_arg_value(3, v2);

        let values = f.arg_index_to_values(3);
        assert_eq!(values.len(), 2);
        assert!(values.contains(&v1));
        assert!(values.contains(&v2));

        // iter_arg_indices contains the registered index.
        assert!(f.iter_arg_indices().any(|i| i == 3));
    }

    /// `get_vn_for_value` round-trips via the ValueId-keyed map.
    #[test]
    fn get_vn_for_value_round_trips_via_value_key() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let phi = f
            .graph_mut()
            .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
        let phi_value = f.node_outputs(phi)[0];
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        assert_eq!(f.get_vn_for_value(phi_value), None);
        f.set_vn_for_value(phi_value, vn);
        assert_eq!(f.get_vn_for_value(phi_value), Some(vn));
    }

    /// `arg_index_to_values` stores a carrier's value and `producer` recovers
    /// the carrier node.
    #[test]
    fn arg_index_to_values_recovers_carrier_node_via_producer() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let arg_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(arg_vn),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let value = f.node_outputs(carrier)[0];
        f.register_arg_value(0, value);

        assert_eq!(f.arg_index_to_values(0), &[value]);
        assert_eq!(f.graph().producer(value), carrier);
    }
}

#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use super::Function;
    use crate::IRViewer;
    use crate::node::{IntPayload, NodeKind, ValueKind};

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xdead)),
            [],
            [ValueKind::Typed(crate::node::ValueType::I64)],
        );
        f.set_entry(entry);
        let pre_count = f.graph().all_node_ids().count();

        let _remap = f.compact().expect("compact succeeds on a valid function");

        let post_count = f.graph().all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let entry_id = f.entry().unwrap();
        let outs: Vec<_> = f.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(f.value_kind(outs[0]).is_control());
    }

    /// A SURVIVING `stack_offsets` entry is remapped through compaction on
    /// BOTH coordinates: its key (`NodeId`) and its value's base
    /// (`ValueId`).  A zombie allocated before the live nodes forces a
    /// non-trivial id shift, so the test fails if either side is left
    /// unremapped.  (The drop-on-death side is pinned by
    /// `retain_reachable_drops_side_table_entry_for_dropped_node`.)
    #[test]
    fn compact_remaps_surviving_stack_offset_entry() {
        use crate::node::ValueType;

        let mut f = Function::default();
        // Zombie FIRST so the surviving nodes' ids shift during compaction.
        let zombie = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xdead)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        f.set_entry(entry);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let base = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0x7000)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [base_value] = f.node_outputs_exact::<1>(base).unwrap();
        let ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, base_value], []);
        f.set_stack_offset(ret, base_value, -16);

        let remap = f.compact().expect("compact must succeed");

        assert!(
            remap.node_old_to_new(zombie).is_none(),
            "zombie must be dropped"
        );
        let new_ret = remap.node_old_to_new(ret).expect("Return survives");
        let new_base_value = remap
            .value_old_to_new(base_value)
            .expect("base value survives");
        assert_ne!(
            new_ret, ret,
            "the zombie ahead of it must shift the Return's id"
        );
        assert_eq!(
            f.stack_offset(new_ret),
            Some((new_base_value, -16)),
            "surviving stack_offsets entry must be remapped on key AND base"
        );
    }

    /// Asm-fingerprints survive compaction on every reachable node.
    /// Regression guard: a node remap must carry the fingerprint side-
    /// table through to its new NodeId.  Otherwise pattern queries
    /// against optimised IR lose contributor-asm attribution for any
    /// surviving node whose id was remapped.
    #[test]
    fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        // Reachable IntConst whose Return-input consumer keeps it live.
        let surviving = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xCAFE_u64)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [surv_value] = f.node_outputs_exact::<1>(surviving).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, surv_value], []);
        f.set_entry(entry);

        // Stamp three asm addresses on the surviving IntConst before compact.
        f.extend_asm_fingerprint(surviving, &[0x1000, 0x1004, 0x1008]);

        let remap = f.compact().expect("compact must succeed");
        let new_id = remap
            .node_old_to_new(surviving)
            .expect("surviving IntConst must remain after compact");
        assert_eq!(
            f.asm_fingerprint(new_id),
            &[0x1000, 0x1004, 0x1008],
            "surviving node's asm-fingerprint must transfer to its post-compact NodeId"
        );
    }

    /// A cacheable zombie node that has no live uses must be absent after
    /// `Function::compact`.  Regression guard against compaction skipping
    /// detached-but-still-arena-present nodes.
    #[test]
    fn retain_reachable_drops_zombie_node() {
        use crate::graph::NodeIdRemap;
        use crate::node::ValueType;

        let mut f = Function::default();
        // Entry + InitialMemory + a Return (minimal reachable graph).
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
        f.set_entry(entry);

        // Zombie: a cacheable IntConst not connected to anything reachable.
        let zombie = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xC0FFEE_u64)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );

        // Zombie must be in the arena before compact.
        let pre_ids: Vec<_> = f.graph().all_node_ids().collect();
        assert!(
            pre_ids.contains(&zombie),
            "zombie must be present before compact"
        );

        let _remap: NodeIdRemap = f.compact().expect("compact must succeed");

        // After compact the zombie NodeId is invalid; verify by checking
        // that the remap returns None for it (it was dropped).
        assert!(
            _remap.node_old_to_new(zombie).is_none(),
            "zombie must be dropped by compact"
        );
        // Node count must decrease.
        assert!(
            f.graph().all_node_ids().count() < pre_ids.len(),
            "compact must remove unreachable nodes"
        );
    }

    /// The `value_vn` and `stack_offsets` side-tables must NOT contain
    /// stale entries pointing to zombie (dropped) NodeIds after compaction.
    #[test]
    fn retain_reachable_drops_side_table_entry_for_dropped_node() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
        f.set_entry(entry);

        // Zombie Phi node with a value_vn entry.
        let zombie_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x88,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let zombie_phi_value = f.node_outputs(zombie_phi)[0];
        f.set_vn_for_value(zombie_phi_value, dead_vn);
        assert_eq!(
            f.get_vn_for_value(zombie_phi_value),
            Some(dead_vn),
            "tag must be set before compact"
        );

        // Zombie IntConst node with a stack_offsets entry.
        let zombie_stack = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xBEEF_u64)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let zombie_value = f.node_outputs(zombie_stack).iter().copied().next().unwrap();
        f.set_stack_offset(zombie_stack, zombie_value, -8);
        assert_eq!(
            f.stack_offset(zombie_stack),
            Some((zombie_value, -8)),
            "offset must be set before compact"
        );

        let remap = f.compact().expect("compact must succeed");

        // Both zombies must have been dropped.
        assert!(remap.node_old_to_new(zombie_phi).is_none());
        assert!(remap.node_old_to_new(zombie_stack).is_none());

        // Side-table entries for dropped nodes must not exist.  `value_vn`
        // is a `ValueId`-keyed `FxHashMap` rebuilt over only surviving values;
        // `stack_offset` is a `SecondaryMap<NodeId, Option<_>>` rebuilt over
        // only surviving nodes.  In both cases the dropped zombies' entries
        // are gone.  We verify indirectly: no surviving node carries the
        // tag/offset.
        let surviving_with_tag = f.graph().all_node_ids().any(|n| {
            f.node_outputs(n)
                .first()
                .copied()
                .and_then(|v| f.get_vn_for_value(v))
                == Some(dead_vn)
        });
        assert!(
            !surviving_with_tag,
            "dead_vn value_vn tag must not survive compaction"
        );
        let surviving_with_offset = f
            .graph()
            .all_node_ids()
            .any(|n| f.stack_offset(n).map(|(_, o)| o) == Some(-8));
        assert!(
            !surviving_with_offset,
            "stack_offset -8 must not survive compaction on a surviving node"
        );
    }

    /// The `arg_index_to_values` side-table must be remapped through the
    /// compaction translation, like every other overlay.
    /// Regression guard: the orchestrator's default finalize path runs the
    /// destructive pipeline (which removes nodes) and then `compact()`,
    /// while `FunctionArgDetect` (the pass that populates
    /// `arg_index_to_values`) runs only in the *stable* pipeline — so the
    /// carrier values stored before compaction must be translated to their
    /// post-compaction values, otherwise `function_arg(N)` pattern queries and
    /// dot rendering read stale / aliased values.
    #[test]
    fn compact_remaps_arg_index_to_values() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A zombie created *before* the arg carrier so that compaction
        // reassigns the carrier's NodeId (the zombie's slot is dropped).
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xDEAD_u64)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        // The arg carrier: a register-arg-style InitialVar kept live by Return.
        let arg_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let arg_node = f.graph_mut().create_node(
            NodeKind::InitialVar(arg_vn),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [arg_value] = f.node_outputs_exact::<1>(arg_node).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, arg_value], []);
        f.set_entry(entry);
        f.register_arg_value(0, arg_value);

        let remap = f.compact().expect("compact must succeed");
        let new_arg_value = remap
            .value_old_to_new(arg_value)
            .expect("the live arg carrier value must survive compaction");

        assert_eq!(
            f.arg_index_to_values(0),
            &[new_arg_value],
            "arg_index_to_values must carry the carrier's post-compaction value"
        );
        // Every stored carrier value's producer must be a live node.
        for &v in f.arg_index_to_values(0) {
            let node = f.graph().producer(v);
            assert!(
                f.graph().all_node_ids().any(|n| n == node),
                "arg carrier producer {node:?} must be a live post-compaction node"
            );
        }
    }

    /// After compact, a `value_vn` entry on an unreachable Phi is dropped while a
    /// reachable Phi's tag survives (keyed by the Phi's surviving value).
    #[test]
    fn compact_keeps_reachable_phi_tag_drops_unreachable() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A reachable Phi kept live by Return.
        let live_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [live_phi_value] = f.node_outputs_exact::<1>(live_phi).unwrap();
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, live_phi_value],
            [],
        );
        f.set_entry(entry);

        // Unreachable Phi (not wired to anything reachable).
        let dead_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);

        let live_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x88,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let dead_phi_value = f.node_outputs(dead_phi)[0];
        f.set_vn_for_value(live_phi_value, live_vn);
        f.set_vn_for_value(dead_phi_value, dead_vn);

        let remap = f.compact().expect("compact must succeed");
        let new_live_phi = remap
            .node_old_to_new(live_phi)
            .expect("reachable phi must survive compaction");
        let new_live_phi_value = remap
            .value_old_to_new(live_phi_value)
            .expect("reachable phi value must survive compaction");

        assert_eq!(
            f.get_vn_for_value(new_live_phi_value),
            Some(live_vn),
            "reachable phi's tag must survive compaction"
        );
        let _ = new_live_phi; // kept to document the remap usage
        assert!(
            remap.node_old_to_new(dead_phi).is_none(),
            "unreachable phi must be dropped"
        );
        // No surviving node carries the dead tag.
        assert!(
            !f.graph().all_node_ids().any(|n| f
                .node_outputs(n)
                .first()
                .copied()
                .and_then(|v| f.get_vn_for_value(v))
                == Some(dead_vn)),
            "dead phi tag must not survive compaction"
        );
    }

    /// After compact, a pruned arg carrier's value is dropped from
    /// `arg_index_to_values`; a surviving one is recoverable via `producer`.
    #[test]
    fn compact_drops_pruned_arg_value_keeps_surviving() {
        use crate::node::ValueType;

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let live_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let live_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(live_vn),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [live_value] = f.node_outputs_exact::<1>(live_carrier).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, live_value], []);
        f.set_entry(entry);

        // A pruned (unreachable) carrier for a different arg index.
        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x18,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let dead_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(dead_vn),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [dead_value] = f.node_outputs_exact::<1>(dead_carrier).unwrap();

        f.register_arg_value(0, live_value);
        f.register_arg_value(1, dead_value);

        f.compact().expect("compact must succeed");

        // arg 1's value was pruned → index removed entirely.
        assert!(
            f.arg_index_to_values(1).is_empty(),
            "pruned arg value must be dropped"
        );
        // arg 0 survives → producer recovers the live carrier.
        let surviving = f.arg_index_to_values(0);
        assert_eq!(surviving.len(), 1);
        let node = f.graph().producer(surviving[0]);
        assert!(matches!(f.node_kind(node), NodeKind::InitialVar(_)));
    }

    /// A Call clobber output's value maps to its clobbered varnode via
    /// `value_vn` (the `get_vn_for_value` accessor), recoverable per-output.
    #[test]
    fn clobber_output_value_maps_to_vn_via_value_vn() {
        use crate::node::ValueType;

        let mut f = Function::default();
        // A Call with one clobber output [Control, Memory, clobber].
        let call = f.graph_mut().create_node(
            NodeKind::Call,
            [],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::I64),
            ],
        );
        let clobber_value = f.node_outputs(call)[2];
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        assert_eq!(f.get_vn_for_value(clobber_value), None);
        f.set_vn_for_value(clobber_value, vn);
        // Recoverable per-output: the clobber output value carries its Vn.
        assert_eq!(f.get_vn_for_value(clobber_value), Some(vn));
        // Control / Memory outputs carry no clobber tag.
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[0]), None);
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[1]), None);
    }

    /// `call_cc` round-trips and its stack-arg offsets are what the derived
    /// `call_stack_args_override` accessor returns; compact remaps
    /// both the per-Call `call_cc` and the per-output clobber `value_vn`.
    #[test]
    fn compact_remaps_call_cc_and_clobber_value_vn() {
        use crate::node::ValueType;

        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().unwrap();
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .unwrap()
            .build(&regs)
            .unwrap();

        let mut f = Function::default();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A zombie created before the Call so compaction reassigns ids.
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0xDEAD_u64)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let target = f.graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(0x1000)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [target_value] = f.node_outputs_exact::<1>(target).unwrap();
        // Call with one clobber output, kept live by Return consuming its
        // ctrl/mem outputs.
        let call = f.graph_mut().create_node(
            NodeKind::Call,
            [entry_ctrl, mem_value, target_value],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::I64),
            ],
        );
        let [call_ctrl, call_mem, clob] = f.node_outputs_exact::<3>(call).unwrap();
        let clob_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        f.set_vn_for_value(clob, clob_vn);
        f.set_call_cc(call, cc.clone());
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, call_mem], []);
        f.set_entry(entry);

        // Pre-compact: round-trips.
        assert!(f.call_cc(call).is_some());
        assert_eq!(f.call_stack_args_override(call), cc.stack_args,);
        assert_eq!(f.get_vn_for_value(clob), Some(clob_vn));

        let remap = f.compact().expect("compact must succeed");
        let new_call = remap
            .node_old_to_new(call)
            .expect("live Call must survive compaction");
        let new_clob = remap
            .value_old_to_new(clob)
            .expect("live clobber output value must survive compaction");

        // call_cc survives the NodeId remap; stack-arg offsets still derive.
        assert!(f.call_cc(new_call).is_some());
        assert_eq!(f.call_stack_args_override(new_call), cc.stack_args,);
        // The clobber tag survives the ValueId remap.
        assert_eq!(f.get_vn_for_value(new_clob), Some(clob_vn));
    }
}
