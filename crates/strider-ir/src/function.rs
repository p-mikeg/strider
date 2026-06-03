//! [`Function`] — a [`Graph`] plus per-function overlay state (`entry`,
//! calling convention, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges/wide_const interning, dedup
//! cache).  [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, and the other four `NodeId`-keyed
//! side tables.
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

use crate::graph::{Graph, NodeIdRemap, SideTableRemap};
use crate::node::{NodeId, ValueId};

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
#[derive(Default)]
pub struct Function {
    pub(crate) graph: Graph,
    entry: Option<NodeId>,

    // ── calling-convention overlay ─────────────────────────────────────────
    //
    // `default_cc` (the resolved convention) and `all_vns` (the ordered
    // tracked-varnode set) are the two genuinely-non-derivable inputs.
    // Every register-list projection a `Call` / `CallOther` / `Return`
    // node needs is *derived* from these two via the accessors below
    // (`call_clobbered_for`, `ret_val_regs`, `arg_passing_vars`,
    // `call_other_clobbered`) — there are no cached projected lists, so a
    // per-address-CC override produces a correct per-call clobber set by
    // deriving against that call's effective CC over the same `all_vns`.

    /// The calling convention this function was built under.  Always a
    /// real value: production functions carry their resolved target ABI;
    /// synthetic test functions constructed via
    /// [`crate::FunctionBuilder::new_raw`] (or [`Self::new`] / the
    /// `Default` derive) without a real CC carry the *trivial* convention
    /// ([`strider_target::BuiltCallingConvention::default`]) — empty reg
    /// lists with a synthetic `stack_vn` (a real, sized register at an
    /// out-of-range offset that matches no tracked register), so stack
    /// analyses no-op.  Pure ABI facts (`stack_vn`, `ret_stack_pop`,
    /// `preserves_memory`, link register) are read through this and
    /// surfaced by the [`Self::stack_vn`] / [`Self::ret_stack_pop`] /
    /// [`Self::preserves_memory`] accessors.  The convention's
    /// `arg_passing_regs` / `ret_val_regs` / `callee_saved_regs` drive the
    /// register-list derivations ([`Self::call_clobbered_for`],
    /// [`Self::ret_val_regs`], [`Self::arg_passing_vars`]).
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
    // The wider lifter contract (`set_asm_fingerprint`,
    // `extend_asm_fingerprint`) keeps using `&[u64]` /
    // `impl IntoIterator<Item = u64>` so callers are unaffected.
    pub(crate) asm_fingerprints:
        SecondaryMap<NodeId, smallvec::SmallVec<[u64; 2]>>,
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
    /// [`Self::call_stack_arg_offsets_override`].  The convenience accessor
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
}

impl Function {
    /// Creates a `Function` with an empty graph and no entry node.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a shared reference to the underlying graph.
    #[inline]
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns a mutable reference to the underlying graph.
    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    // ── Forwarded read-only Graph accessors ──────────────────────────────
    //
    // These delegate verbatim to the inner [`Graph`]; they exist so the
    // common read accessors stay callable directly on a `&Function` without
    // an explicit `.graph()` hop.  Every other [`Graph`] method is reached
    // through [`Self::graph`] / [`Self::graph_mut`].

    /// Delegates to the inner graph's [`Graph::node_kind`].
    #[inline]
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &crate::node::NodeKind {
        self.graph.node_kind(node_id)
    }

    /// Delegates to the inner graph's [`Graph::node_inputs`].
    #[inline]
    #[must_use]
    pub fn node_inputs(&self, node_id: NodeId) -> crate::iterators::Inputs<'_> {
        self.graph.node_inputs(node_id)
    }

    /// Delegates to the inner graph's [`Graph::node_outputs`].
    #[inline]
    #[must_use]
    pub fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.graph.node_outputs(node_id)
    }

    /// Delegates to the inner graph's [`Graph::node_outputs_exact`].
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `N` outputs.
    #[inline]
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[ValueId; N]> {
        self.graph.node_outputs_exact(node_id)
    }

    /// Delegates to the inner graph's [`Graph::value_kind`].
    #[inline]
    #[must_use]
    pub fn value_kind(&self, value_id: ValueId) -> crate::node::ValueKind {
        self.graph.value_kind(value_id)
    }

    /// Delegates to the inner graph's [`Graph::producer`].
    #[inline]
    #[must_use]
    pub fn producer(&self, value_id: ValueId) -> NodeId {
        self.graph.producer(value_id)
    }

    /// Delegates to the inner graph's [`Graph::kind_of_value`].
    #[inline]
    #[must_use]
    pub fn kind_of_value(&self, value_id: ValueId) -> &crate::node::NodeKind {
        self.graph.kind_of_value(value_id)
    }

    /// Delegates to the inner graph's [`Graph::value_definition`].
    #[inline]
    #[must_use]
    pub fn value_definition(&self, value_id: ValueId) -> (NodeId, u32) {
        self.graph.value_definition(value_id)
    }

    /// Returns the entry node, if one has been recorded.
    #[inline]
    #[must_use]
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
    #[must_use]
    pub fn default_cc(&self) -> &strider_target::BuiltCallingConvention {
        &self.default_cc
    }

    /// Target endianness of the architecture this function was lifted for.
    /// Consumed by the builder's register-aliasing bit-shift formula
    /// ([`crate::FunctionBuilder::read_reg_vn`] /
    /// [`crate::FunctionBuilder::write_reg_vn`]).
    #[inline]
    #[must_use]
    pub fn endianness(&self) -> strider_target::Endianness {
        self.endianness
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
    #[must_use]
    pub fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let stack_vn = self.default_cc.stack_vn;
        // Hash the per-element membership probes so the derivation stays
        // O(N) rather than O(N·M): `callee_saved_regs` is consulted per
        // candidate, and `all_vns` is consulted per candidate.  Both
        // checks keep their previous semantics (set membership) exactly,
        // so the output order (ABI order over the ret list) and
        // membership are byte-identical.
        let callee_saved: FxHashSet<rsleigh::Vn> =
            cc.callee_saved_regs.iter().copied().collect();
        let tracked: FxHashSet<rsleigh::Vn> = self.all_vns.iter().copied().collect();
        let is_clobbered = |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn;
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .copied()
            .filter(|v| tracked.contains(v) && is_clobbered(v))
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
    /// All elements are drawn from [`Self::all_vns`] in allocation order.
    ///
    /// To obtain the FULL combined set (ret-vals ++ clobbers) for callers
    /// that need the old single-list shape, chain the two accessors:
    /// `call_ret_vals_for(cc).into_iter().chain(call_clobbered_for(cc))`.
    #[must_use]
    pub fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let stack_vn = self.default_cc.stack_vn;
        // Hashed membership probes keep the per-element filter O(1) so the
        // whole derivation is O(N) instead of O(N·M): `callee_saved_regs`
        // and the combined ret-reg list (used to EXCLUDE ret regs from the
        // clobber tail) are each turned into an `FxHashSet`.  The output
        // ORDER (`all_vns` allocation order) and MEMBERSHIP are unchanged —
        // only the lookup data structure differs.
        let callee_saved: FxHashSet<rsleigh::Vn> =
            cc.callee_saved_regs.iter().copied().collect();
        let is_clobbered = |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn;
        // The combined ret-reg list (raw): the ret-val group is emitted
        // separately by `call_ret_vals_for`, so exclude it here.
        let ret_vars: FxHashSet<rsleigh::Vn> = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .copied()
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
    #[must_use]
    pub fn call_clobbered_regs(&self) -> Vec<rsleigh::Vn> {
        self.call_clobbered_for(&self.default_cc)
    }

    /// The function-default ret-val varnode list (derived against
    /// [`Self::default_cc`]).  Convenience for consumers that want the
    /// default-CC ret-val shape.
    #[inline]
    #[must_use]
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
    #[must_use]
    pub fn ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        self.default_cc
            .ret_val_regs
            .iter()
            .chain(self.default_cc.ret_val_regs_float.iter())
            .copied()
            .collect()
    }

    /// The calling convention's arg-passing register list, at each
    /// register's declared width (no tracked-container projection).  Call
    /// sites read each register via the aliasing-aware
    /// [`crate::FunctionBuilder::read_reg_vn`], which resolves the declared
    /// register to its tracked container (and errors when a CC register has
    /// no tracked footprint — the intended "CC reg must exist" invariant).
    #[inline]
    #[must_use]
    pub fn arg_passing_vars(&self) -> Vec<rsleigh::Vn> {
        self.default_cc.arg_passing_regs.clone()
    }

    /// Calling convention's stack-pointer varnode.  On the trivial CC
    /// carried by synthetic test functions this is a synthetic register
    /// at an out-of-range offset that matches no tracked register, so
    /// SP-keyed analyses simply find no matches.
    #[inline]
    #[must_use]
    pub(crate) fn stack_vn(&self) -> rsleigh::Vn {
        self.default_cc.stack_vn
    }


    /// The function-default `CallOther` clobber list: every tracked
    /// varnode except the stack pointer, in [`Self::all_vns`] order.
    /// Reproduces the old build-time `call_other_clobbered` (`build()`
    /// filtered `var_table.values()` — same order as `all_vns` — by
    /// `!= stack_vn`).
    #[inline]
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> Vec<rsleigh::Vn> {
        let stack_vn = self.default_cc.stack_vn;
        self.all_vns
            .iter()
            .copied()
            .filter(|v| *v != stack_vn)
            .collect()
    }

    /// Iterate the function's tracked varnodes, in `InitialVar`-creation
    /// (allocation) order.  Yields one entry per tracked variable.
    #[inline]
    pub fn tracked_vns(&self) -> impl Iterator<Item = rsleigh::Vn> + '_ {
        self.all_vns.iter().copied()
    }

    // ── NodeId-keyed overlay accessors ────────────────────────────────────

    /// Returns the user-op name associated with a
    /// [`crate::node::NodeKind::CallOther`] node, or `None` if no name has
    /// been recorded for that node.
    #[inline]
    #[must_use]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names[node_id].as_deref()
    }

    /// Associates a user-op name with a [`crate::node::NodeKind::CallOther`]
    /// node.  Replaces any prior value.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: String) {
        self.call_other_names[node_id] = Some(name);
    }

    /// Returns the source-level varnode tag for `node_id` if it is a
    /// [`crate::node::NodeKind::Phi`] created at lift time tracking a specific
    /// varnode, or `None` for anonymous phis (synthesised by opt passes) or
    /// non-phi nodes.
    #[inline]
    #[must_use]
    pub fn phi_var_tag(&self, node_id: NodeId) -> Option<rsleigh::Vn> {
        // The tag is keyed by the Phi's single output `ValueId`.  A `Phi`
        // (and the synthetic fake-token nodes the indirect resolver tags)
        // always has at least one output, so the first output is the key.
        let value = self.graph.node_outputs(node_id).first().copied()?;
        self.value_vn.get(&value).copied()
    }

    /// Sets the source-level varnode tag for `node_id`.  Callers must
    /// guarantee that `node_id`'s kind is [`crate::node::NodeKind::Phi`].
    #[inline]
    pub fn set_phi_var_tag(&mut self, node_id: NodeId, vn: rsleigh::Vn) {
        let value = self.graph.node_outputs(node_id)[0];
        self.value_vn.insert(value, vn);
    }

    /// Returns the varnode a clobber-output `value` represents, or `None`
    /// when `value` is not a clobber output (or a tagged phi value).
    ///
    /// This is the single lookup that recovers the register a
    /// [`crate::node::NodeKind::Call`] / [`crate::node::NodeKind::CallOther`]
    /// clobber output writes — set at build time for every clobber output.
    #[inline]
    #[must_use]
    pub fn clobbered_vn(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.value_vn.get(&value).copied()
    }

    /// Records that `value` represents varnode `vn` (a Call / CallOther
    /// clobber output's clobbered register).  Replaces any prior value.
    #[inline]
    pub fn set_clobbered_vn(&mut self, value: ValueId, vn: rsleigh::Vn) {
        self.value_vn.insert(value, vn);
    }

    /// Returns the [`crate::CallDescriptor`] recorded for `node_id`, or
    /// `None` when no descriptor has been recorded (default Call or unmodeled
    /// CallOther).
    #[inline]
    #[must_use]
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
    #[must_use]
    pub fn call_cc(&self, node_id: NodeId) -> Option<&strider_target::BuiltCallingConvention> {
        match self.call_descriptor.get(&node_id)? {
            crate::CallDescriptor::Call(cc) => Some(cc),
            crate::CallDescriptor::CallOther(_) => None,
        }
    }

    /// Records `cc` as the per-Call override calling convention for
    /// `node_id`, wrapping it in [`crate::CallDescriptor::Call`].  Replaces
    /// any prior descriptor.  Subsumes the stack-arg offsets override (read
    /// back via [`Self::call_stack_arg_offsets_override`]).
    ///
    /// Prefer [`Self::set_call_descriptor`] when the call site already has a
    /// `CallDescriptor` value; this wrapper exists for call sites that only
    /// deal with `BuiltCallingConvention`.
    #[inline]
    pub fn set_call_cc(
        &mut self,
        node_id: NodeId,
        cc: strider_target::BuiltCallingConvention,
    ) {
        self.call_descriptor
            .insert(node_id, crate::CallDescriptor::Call(cc));
    }

    /// Returns the per-Call stack-arg offsets override for `node_id`, or
    /// `None` if the Call uses the function-default CC's stack-arg offsets.
    ///
    /// Derived from the `Call` arm of the stored [`crate::CallDescriptor`]:
    /// the offsets are the override CC's `stack_arg_offsets`.  Returns `None`
    /// for `CallOther` descriptors (they have no stack-arg offsets).
    #[inline]
    #[must_use]
    pub fn call_stack_arg_offsets_override(&self, node_id: NodeId) -> Option<&[i64]> {
        match self.call_descriptor.get(&node_id)? {
            crate::CallDescriptor::Call(cc) => Some(cc.stack_arg_offsets.as_slice()),
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
    #[must_use]
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

    /// Drop every registered argument carrier.
    ///
    /// Lets the arg-detection pass rebuild the side-table idempotently from
    /// the live graph: it can be re-run on the same `Function` (e.g. on each
    /// stable iteration of the orchestrator's fixed-point loop) without
    /// accumulating duplicate carrier values.
    #[inline]
    pub fn clear_arg_values(&mut self) {
        self.arg_index_to_values.clear();
    }

    // ── stack_offsets accessors ───────────────────────────────────────────

    /// Returns the stack slot `(base, offset)` recorded for a Store/Load
    /// node, or `None` if the node has no recorded slot (non-stack node, or
    /// a phi-of-offsets address whose single concrete offset cannot be
    /// named).  `base` is the SP-derived terminal node the offset is
    /// relative to; the offset is only comparable against another access's
    /// offset when their bases match.
    #[must_use]
    #[inline]
    pub fn stack_offset(&self, id: NodeId) -> Option<(ValueId, i64)> {
        self.stack_offsets[id]
    }

    /// Records a concrete stack slot `(base, offset)` for a Store/Load node.
    #[inline]
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i64) {
        self.stack_offsets[id] = Some((base, offset));
    }

    /// Iterates over all `(NodeId, base, offset)` triples in the side-table.
    #[inline]
    pub fn stack_offsets(&self) -> impl Iterator<Item = (NodeId, ValueId, i64)> + '_ {
        self.stack_offsets
            .iter()
            .filter_map(|(id, slot)| slot.map(|(base, off)| (id, base, off)))
    }

    // ── initial_var_index accessors ───────────────────────────────────────

    /// Returns the [`NodeId`] of the canonical `InitialVar(vn)` node for
    /// `vn`, or `None` if none is registered.  O(1).
    ///
    /// Callers that want to skip detached zombie nodes must validate the
    /// returned id themselves (typically by checking that the node's
    /// single output's use-list is non-empty via [`Graph::value_uses`]).
    #[inline]
    #[must_use]
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

    /// Returns the asm-instruction-address fingerprint of `node_id` as a
    /// sorted-deduplicated slice.  Returns an empty slice when no
    /// contributors have been recorded.
    #[inline]
    #[must_use]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.asm_fingerprints[id].as_slice()
    }

    /// Replaces `node_id`'s fingerprint with `addrs`.
    ///
    /// Sorts and deduplicates `addrs` first so callers cannot accidentally
    /// install an unsorted entry.  This is the test-only / synthetic-graph
    /// entry point: production passes use
    /// [`Self::extend_asm_fingerprint`] / [`Self::extend_asm_fingerprint_from`]
    /// to preserve the superset-only invariant.
    #[inline]
    pub fn set_asm_fingerprint(&mut self, id: NodeId, mut addrs: Vec<u64>) {
        addrs.sort_unstable();
        addrs.dedup();
        self.asm_fingerprints[id] = addrs.into_iter().collect();
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
    /// every node in `contributors` into the resulting node.
    pub fn create_node_attributed(
        &mut self,
        kind: crate::node::NodeKind,
        inputs: impl IntoIterator<Item = crate::node::ValueId>,
        output_kinds: impl IntoIterator<Item = crate::node::ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        let node_id = self.graph.create_node(kind, inputs, output_kinds);
        for &src in contributors {
            self.extend_asm_fingerprint_from(node_id, src);
        }
        node_id
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from [`Function::entry`].  Yields an empty walk on a
    /// function whose entry has not yet been set.
    #[must_use]
    pub fn walk(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph_opt(&self.graph, self.entry)
    }

    /// Returns the entry-reachable nodes in **global reverse-post-order**
    /// (entry-first), filtered to those whose [`crate::node::NodeKind`]
    /// satisfies `pred`.
    ///
    /// Derives the entry from [`Self::entry`]; yields an empty iterator
    /// when the entry has not yet been set.  The reachable SET is the
    /// same as [`Self::walk`]'s; only the ORDER is canonicalised to RPO
    /// (every producer precedes its consumers), so passes that seed a
    /// worklist or scan in this order see operands before consumers.
    pub fn rpo_filter<'a>(
        &'a self,
        pred: impl Fn(&crate::node::NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        crate::walk::rpo_reachable_opt(&self.graph, self.entry)
            .into_iter()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Reachable preorder filtered by a predicate over the node's kind.
    pub fn walk_kind<'a, P>(
        &'a self,
        mut pred: P,
    ) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.walk()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Counts reachable nodes whose [`crate::node::NodeKind`] satisfies
    /// `predicate`.  Walks in pre-order from [`Self::entry`].
    pub fn count_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> usize {
        self.walk()
            .filter(|nid| predicate(self.graph.node_kind(*nid)))
            .count()
    }

    /// Returns `true` when at least one reachable node satisfies
    /// `predicate`.  Short-circuits at the first match.
    pub fn has_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> bool {
        self.walk().any(|nid| predicate(self.graph.node_kind(nid)))
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
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Function::compact: entry node is not set")
        })?;
        let remap = self.graph.retain_reachable(entry)?;
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
                remap.output_old_to_new(old_base),
            ) {
                new_stack_offsets[new_id] = Some((new_base, off));
            }
        }
        self.stack_offsets = new_stack_offsets;
        // `all_vns` is a `Vec<rsleigh::Vn>` with no node / value keys, and
        // `default_cc` holds `rsleigh::Vn` values (not arena ids), so
        // neither needs a remap.  (`default_cc` is always a real value —
        // the trivial CC for synthetic functions — never `None`.)
        // `value_vn` is `FxHashMap<ValueId, Vn>` — keyed by a Phi's single
        // output value or a Call/CallOther clobber output value.  Translate
        // every key through the same `ValueId` remap; an entry whose value
        // did not survive compaction is dropped (the phi / clobber output
        // became unreachable).
        let mut new_value_vn: FxHashMap<ValueId, rsleigh::Vn> =
            FxHashMap::with_capacity_and_hasher(
                self.value_vn.len(),
                Default::default(),
            );
        for (old_value, vn) in self.value_vn.drain() {
            if let Some(new_value) = remap.output_old_to_new(old_value) {
                new_value_vn.insert(new_value, vn);
            }
        }
        self.value_vn = new_value_vn;
        // `initial_var_index` is `FxHashMap<Vn, NodeId>` — Vn-keyed, not
        // NodeId-keyed, so the standard `SecondaryMap` remap helper
        // doesn't fit.  Entries whose NodeId didn't survive compaction
        // (the InitialVar became unreachable and was dropped) are
        // silently elided — the orchestrator's `read_or_init_var`
        // fallback will lazily re-create them as needed.
        let mut new_index: FxHashMap<rsleigh::Vn, NodeId> =
            FxHashMap::with_capacity_and_hasher(self.initial_var_index.len(), Default::default());
        for (vn, old_id) in self.initial_var_index.drain() {
            if let Some(new_id) = remap.node_old_to_new(old_id) {
                new_index.insert(vn, new_id);
            }
        }
        self.initial_var_index = new_index;
        // `arg_index_to_values` is `FxHashMap<u32, Vec<ValueId>>` —
        // index-keyed with `ValueId` payloads, so (like `initial_var_index`)
        // it needs an inline remap.  Carrier values whose value didn't
        // survive compaction are dropped; an index whose carriers all
        // vanished is removed entirely.
        let mut new_arg_index: FxHashMap<u32, Vec<ValueId>> =
            FxHashMap::with_capacity_and_hasher(self.arg_index_to_values.len(), Default::default());
        for (index, old_values) in self.arg_index_to_values.drain() {
            let mapped: Vec<ValueId> = old_values
                .into_iter()
                .filter_map(|old_value| remap.output_old_to_new(old_value))
                .collect();
            if !mapped.is_empty() {
                new_arg_index.insert(index, mapped);
            }
        }
        self.arg_index_to_values = new_arg_index;
        Ok(remap)
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
    ) -> crate::Result<crate::function_dot::FunctionDotDumper<'a, R>> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Function::dot_dumper: entry node is not set")
        })?;
        let node_to_arg_indices = crate::function_dot::build_arg_reverse_map(self);
        Ok(crate::function_dot::FunctionDotDumper {
            entry,
            function: self,
            sleigh,
            node_filter: None,
            node_to_arg_indices,
        })
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use super::Function;
    use crate::node::{NodeKind, ValueKind};

    #[test]
    fn function_new_carries_an_empty_graph() {
        let f = Function::new();
        assert_eq!(f.graph().all_node_ids().count(), 0);
        assert!(f.entry().is_none());
    }

    #[test]
    fn function_records_entry_via_set_entry() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        f.set_entry(entry);
        assert_eq!(f.entry(), Some(entry));
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = Function::new();
        let n = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        f.set_asm_fingerprint(n, vec![0xDEAD_BEEF]);
        assert_eq!(f.asm_fingerprint(n), &[0xDEAD_BEEF]);
    }

    #[test]
    fn arg_index_to_values_returns_empty_for_unregistered() {
        let f = Function::new();
        assert!(f.arg_index_to_values(0).is_empty());
        assert!(f.arg_index_to_values(99).is_empty());
    }

    #[test]
    fn register_arg_value_supports_multiple_values_per_index() {
        let mut f = Function::new();
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

    /// `phi_var_tag` round-trips via the ValueId-keyed map.
    #[test]
    fn phi_var_tag_round_trips_via_value_key() {
        use crate::node::ValueType;

        let mut f = Function::new();
        let phi = f
            .graph_mut()
            .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        assert_eq!(f.phi_var_tag(phi), None);
        f.set_phi_var_tag(phi, vn);
        assert_eq!(f.phi_var_tag(phi), Some(vn));
    }

    /// `arg_index_to_values` stores a carrier's value and `producer` recovers
    /// the carrier node.
    #[test]
    fn arg_index_to_values_recovers_carrier_node_via_producer() {
        use crate::node::ValueType;

        let mut f = Function::new();
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
    use crate::node::{NodeKind, ValueKind};

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xdead),
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

    /// Asm-fingerprints survive compaction on every reachable node.
    /// Regression guard: a node remap must carry the fingerprint side-
    /// table through to its new NodeId.  Otherwise pattern queries
    /// against optimised IR lose contributor-asm attribution for any
    /// surviving node whose id was remapped.
    #[test]
    fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
        use crate::node::ValueType;

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        // Reachable IntConst whose Return-input consumer keeps it live.
        let surviving = f.graph_mut().create_node(
            NodeKind::IntConst(0xCAFE_u128),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [surv_value] = f.node_outputs_exact::<1>(surviving).unwrap();
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, surv_value],
            [],
        );
        f.set_entry(entry);

        // Stamp three asm addresses on the surviving IntConst before compact.
        f.set_asm_fingerprint(surviving, vec![0x1000, 0x1004, 0x1008]);

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
        use crate::node::ValueType;
        use crate::graph::NodeIdRemap;

        let mut f = Function::new();
        // Entry + InitialMemory + a Return (minimal reachable graph).
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
        f.set_entry(entry);

        // Zombie: a cacheable IntConst not connected to anything reachable.
        let zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xC0FFEE_u64 as u128),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );

        // Zombie must be in the arena before compact.
        let pre_ids: Vec<_> = f.graph().all_node_ids().collect();
        assert!(pre_ids.contains(&zombie), "zombie must be present before compact");

        let _remap: NodeIdRemap = f.compact().expect("compact must succeed");

        // After compact the zombie NodeId is invalid; verify by checking
        // that the remap returns None for it (it was dropped).
        assert!(_remap.node_old_to_new(zombie).is_none(), "zombie must be dropped by compact");
        // Node count must decrease.
        assert!(
            f.graph().all_node_ids().count() < pre_ids.len(),
            "compact must remove unreachable nodes"
        );
    }

    /// The `phi_var_tag` and `stack_offsets` side-tables must NOT contain
    /// stale entries pointing to zombie (dropped) NodeIds after compaction.
    #[test]
    fn retain_reachable_drops_side_table_entry_for_dropped_node() {
        use crate::node::ValueType;

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
        f.set_entry(entry);

        // Zombie Phi node with a phi_var_tag entry.
        let zombie_phi = f.graph_mut().create_node(
            NodeKind::Phi,
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x88,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        f.set_phi_var_tag(zombie_phi, dead_vn);
        assert_eq!(
            f.phi_var_tag(zombie_phi),
            Some(dead_vn),
            "tag must be set before compact"
        );

        // Zombie IntConst node with a stack_offsets entry.
        let zombie_stack = f.graph_mut().create_node(
            NodeKind::IntConst(0xBEEF_u64 as u128),
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

        // Side-table entries for dropped nodes must not exist.  `phi_var_tag`
        // is a `ValueId`-keyed `FxHashMap` rebuilt over only surviving values;
        // `stack_offset` is a `SecondaryMap<NodeId, Option<_>>` rebuilt over
        // only surviving nodes.  In both cases the dropped zombies' entries
        // are gone.  We verify indirectly: no surviving node carries the
        // tag/offset.
        let surviving_with_tag = f
            .graph().all_node_ids()
            .any(|n| f.phi_var_tag(n) == Some(dead_vn));
        assert!(
            !surviving_with_tag,
            "dead_vn phi_var_tag must not survive compaction"
        );
        let surviving_with_offset = f
            .graph().all_node_ids()
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

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A zombie created *before* the arg carrier so that compaction
        // reassigns the carrier's NodeId (the zombie's slot is dropped).
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xDEAD_u128),
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
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value, arg_value], []);
        f.set_entry(entry);
        f.register_arg_value(0, arg_value);

        let remap = f.compact().expect("compact must succeed");
        let new_arg_value = remap
            .output_old_to_new(arg_value)
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

    /// After compact, a `phi_var_tag` on an unreachable Phi is dropped while a
    /// reachable Phi's tag survives (keyed by the Phi's surviving value).
    #[test]
    fn compact_keeps_reachable_phi_tag_drops_unreachable() {
        use crate::node::ValueType;

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A reachable Phi kept live by Return.
        let live_phi = f
            .graph_mut()
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
        let dead_phi = f
            .graph_mut()
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
        f.set_phi_var_tag(live_phi, live_vn);
        f.set_phi_var_tag(dead_phi, dead_vn);

        let remap = f.compact().expect("compact must succeed");
        let new_live_phi = remap
            .node_old_to_new(live_phi)
            .expect("reachable phi must survive compaction");

        assert_eq!(
            f.phi_var_tag(new_live_phi),
            Some(live_vn),
            "reachable phi's tag must survive compaction"
        );
        assert!(
            remap.node_old_to_new(dead_phi).is_none(),
            "unreachable phi must be dropped"
        );
        // No surviving node carries the dead tag.
        assert!(
            !f.graph().all_node_ids().any(|n| f.phi_var_tag(n) == Some(dead_vn)),
            "dead phi tag must not survive compaction"
        );
    }

    /// After compact, a pruned arg carrier's value is dropped from
    /// `arg_index_to_values`; a surviving one is recoverable via `producer`.
    #[test]
    fn compact_drops_pruned_arg_value_keeps_surviving() {
        use crate::node::ValueType;

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
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
        let _ret = f
            .graph_mut()
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
    /// `value_vn` (the `clobbered_vn` accessor), recoverable per-output.
    #[test]
    fn clobber_output_value_maps_to_vn_via_value_vn() {
        use crate::node::ValueType;

        let mut f = Function::new();
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
        assert_eq!(f.clobbered_vn(clobber_value), None);
        f.set_clobbered_vn(clobber_value, vn);
        // Recoverable per-output: the clobber output value carries its Vn.
        assert_eq!(f.clobbered_vn(clobber_value), Some(vn));
        // Control / Memory outputs carry no clobber tag.
        assert_eq!(f.clobbered_vn(f.node_outputs(call)[0]), None);
        assert_eq!(f.clobbered_vn(f.node_outputs(call)[1]), None);
    }

    /// `call_cc` round-trips and its stack-arg offsets are what the derived
    /// `call_stack_arg_offsets_override` accessor returns; compact remaps
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

        let mut f = Function::new();
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        // A zombie created before the Call so compaction reassigns ids.
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xDEAD_u128),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let target = f.graph_mut().create_node(
            NodeKind::IntConst(0x1000),
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
        f.set_clobbered_vn(clob, clob_vn);
        f.set_call_cc(call, cc.clone());
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, call_mem], []);
        f.set_entry(entry);

        // Pre-compact: round-trips.
        assert!(f.call_cc(call).is_some());
        assert_eq!(
            f.call_stack_arg_offsets_override(call),
            Some(cc.stack_arg_offsets.as_slice()),
        );
        assert_eq!(f.clobbered_vn(clob), Some(clob_vn));

        let remap = f.compact().expect("compact must succeed");
        let new_call = remap
            .node_old_to_new(call)
            .expect("live Call must survive compaction");
        let new_clob = remap
            .output_old_to_new(clob)
            .expect("live clobber output value must survive compaction");

        // call_cc survives the NodeId remap; stack-arg offsets still derive.
        assert!(f.call_cc(new_call).is_some());
        assert_eq!(
            f.call_stack_arg_offsets_override(new_call),
            Some(cc.stack_arg_offsets.as_slice()),
        );
        // The clobber tag survives the ValueId remap.
        assert_eq!(f.clobbered_vn(new_clob), Some(clob_vn));
    }
}
