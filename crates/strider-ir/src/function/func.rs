//! [`Function`] — a [`Graph`] plus per-function overlay state (`entry`,
//! calling convention, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges, dedup cache).
//! [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, the const interner, and the
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

use rustc_hash::FxHashMap;
#[cfg(any(test, feature = "test-util"))]
use rustc_hash::FxHashSet;

use crate::graph::{Graph, NodeIdRemap};
use crate::node::{NodeId, NodeKind, ValueId};
use crate::IRWalker;
use crate::function::side_tables::SideTables;

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
/// the lifter's `container_of` ad-hoc fallback and the bulk
/// [`build_container_map`] sweep.
/// Deterministic ordering key for a tracked varnode: `(space, offset,
/// size)`.  [`Function::new`] sorts the tracked set by this before interning
/// so `InitialVnId` assignment — and every derived clobber-slot index — is
/// stable regardless of the order varnodes were collected from the CFG.
pub(crate) fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size)
}

pub fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
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

/// Build a `vn → largest containing tracked vn` map over `queries`, resolving
/// every REGISTER / UNIQUE query varnode against the `tracked` set with an
/// O(n log n) per-space stack sweep (never the O(n²) per-query rescan).
///
/// A query that is its own largest container maps to itself; a sub-register
/// slice maps to the largest tracked varnode that strictly encloses it (its
/// container).  Non-aliasable (CONST / RAM / code) query varnodes are omitted —
/// containment-by-offset is meaningless there — so a lookup miss on them falls
/// through to the caller's fallback (self).  The map is the O(1) fast path the
/// register-aliasing hot path reads on every register access; the lifter owns
/// it, built once per function from the raw collected varnode set plus every
/// calling-convention register.
///
/// This is the machine-register-container knowledge relocated out of the
/// target-agnostic IR: the shared linear scan behind ad-hoc lookups is
/// [`largest_container_in`], and this bulk builder reuses the same containment
/// rule in one sweep.
pub fn build_container_map(
    tracked: &[rsleigh::Vn],
    queries: impl IntoIterator<Item = rsleigh::Vn>,
) -> FxHashMap<rsleigh::Vn, rsleigh::Vn> {
    // Bucket the tracked set by space, `(off ascending, size descending)`, so a
    // wider enclosure is seen before the narrower slices it contains.
    let mut tracked_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> = FxHashMap::default();
    for v in tracked {
        if v.addr_space == rsleigh::VnSpace::REGISTER || v.addr_space == rsleigh::VnSpace::UNIQUE {
            tracked_by_space.entry(v.addr_space).or_default().push(*v);
        }
    }

    // Query varnodes to resolve, deduped and bucketed by space (only aliasable
    // spaces participate).
    let mut queries_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> = FxHashMap::default();
    let mut map: FxHashMap<rsleigh::Vn, rsleigh::Vn> = FxHashMap::default();
    for q in queries {
        if (q.addr_space == rsleigh::VnSpace::REGISTER || q.addr_space == rsleigh::VnSpace::UNIQUE)
            && !map.contains_key(&q)
        {
            // Mark seen so a repeated query is not re-pushed; real value filled
            // by the sweep below.
            map.insert(q, q);
            queries_by_space.entry(q.addr_space).or_default().push(q);
        }
    }

    for (space, mut qs) in queries_by_space {
        let Some(tracked_here) = tracked_by_space.get(&space) else {
            // No tracked varnodes in this space: every query is its own
            // container (the `map.insert(q, q)` above already holds).
            continue;
        };
        // Merge the tracked enclosures and the queries into one start-ordered
        // sweep.  For each query we want the largest tracked varnode whose range
        // encloses it.
        let mut opens: Vec<rsleigh::Vn> = tracked_here.clone();
        opens.sort_by_key(|v| (v.addr_off, std::cmp::Reverse(v.size)));
        qs.sort_by_key(|q| (q.addr_off, std::cmp::Reverse(q.size)));

        // For each query, scan the tracked opens whose start <= query start and
        // end >= query end, picking the max-size one.  A two-pointer sweep keeps
        // the active window; opens are small (a register file), so this is
        // O((t + q) log(t + q)).
        let mut active: Vec<rsleigh::Vn> = Vec::new();
        let mut ti = 0usize;
        for q in qs {
            let q_start = q.addr_off;
            let q_end = q_start.saturating_add(u64::from(q.size));
            // Admit every tracked open that starts at or before this query.
            while ti < opens.len() && opens[ti].addr_off <= q_start {
                active.push(opens[ti]);
                ti += 1;
            }
            // Drop opens that end before this query starts (they can enclose no
            // remaining query, since queries are start-ascending).
            active.retain(|c| c.addr_off.saturating_add(u64::from(c.size)) >= q_start);
            // Largest active open enclosing `q`.
            let container = active
                .iter()
                .filter(|c| c.addr_off.saturating_add(u64::from(c.size)) >= q_end)
                .max_by_key(|c| c.size)
                .copied()
                .unwrap_or(q);
            map.insert(q, container);
        }
    }
    map
}

/// A lifted function: structural [`Graph`] plus per-function overlay state.
///
/// [`Function::new`] is the single constructor: it builds the `Entry` node
/// (node 0), so every `Function` always has an entry — the type carries that
/// invariant (no `Option`).  The `InitialMemory` node is added by the
/// `FunctionBuilder`'s [`build_entry`](crate::FunctionBuilder::build_entry).
/// Production functions
/// come from `FunctionBuilder::build`; synthetic / test graphs call
/// [`Function::new`] with the trivial convention and add nodes via
/// [`Function::graph_mut`] on top of the skeleton.
///
/// A small set of read-only [`Graph`] accessors (e.g. `node_kind`,
/// `node_outputs`, `value_kind`) are forwarded as inherent methods on
/// `Function`; every other [`Graph`] method is reached explicitly through
/// [`Function::graph`] / [`Function::graph_mut`].
///
/// `Clone` produces a deep, independent copy (the graph, side-tables, and
/// interners all clone their owned state) — used by the Python binding's
/// `Function.clone()` so a caller can rewrite a copy non-destructively.
#[derive(Clone)]
pub struct Function {
    graph: Graph,
    /// The `Entry` node — always present (built by [`Function::new`]).
    entry: NodeId,

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
    /// for.  Read by post-lift analyses that decode multi-byte values
    /// (the optimizer's ROM-const evaluation and stack high/low-half
    /// splits).  Register-aliasing sub-register slicing itself lives in the
    /// lifter now, so this is no longer on that hot path.  A `Copy` scalar
    /// (so [`Self::compact`] needs no remap for it); defaults to
    /// little-endian on the [`Default`]-derived / synthetic-test path.
    pub(crate) endianness: strider_target::Endianness,
    /// The single interner for every tracked varnode
    /// ([`crate::node::InitialVnId`] → [`rsleigh::Vn`], value-deduped in
    /// deterministic `(space, offset, size)` order).  Single source of truth
    /// for the function's tracked-variable SET *and* the slot ordering of
    /// derived clobber lists (so the `i`-th `Call` clobber output still
    /// corresponds to the `i`-th derived clobber varnode) *and* the SSA-
    /// variable key the [`crate::FunctionBuilder`] uses during construction
    /// (via [`Self::vn_id_of`] / [`Self::vn_ids`]).  `InitialVnId` is a plain
    /// dense id whose assignment does not change when dead nodes are culled,
    /// so [`Self::compact`] leaves the interner untouched.  Read the ordered
    /// varnode list via [`Self::all_vns`], resolve an id via
    /// [`Self::initial_vn`], and resolve a varnode to its id via
    /// [`Self::vn_id_of`].
    pub(crate) vn_interner:
        entity_utils::EntityInterner<crate::node::InitialVnId, rsleigh::Vn>,

    // ── overlay tables ─────────────────────────────────────────────────────
    //
    // Per-function data keyed by arena ids but not part of the structural graph
    // identity, grouped into [`SideTables`]; defaulted in one line by
    // [`Self::new`] and remapped in one [`SideTables::remap`] call by
    // [`Self::compact`].  Surfaced through the typed accessors below.
    pub(crate) side_tables: SideTables,

    /// Every integer-constant value referenced by an `IntConst(id)` node.
    ///
    /// The single interner for ALL integer constants (I1..I512): a value that
    /// fits `u128` is held inline as `ConstValue::Bits`, a value that exceeds
    /// 128 bits as `ConstValue::Wide` (boxed little-endian limbs).  The
    /// constant's WIDTH is carried by the node's output `ValueKind`, not by the
    /// stored value, so `IntConst(42):I80` and `IntConst(42):I128` share one
    /// `ConstId`.  Interning (via [`Self::intern_int_const`] /
    /// [`Self::intern_int_const_limbs`]) dedups by value so two `IntConst(id)` nodes
    /// referencing the same logical value are structurally equal under
    /// [`Graph::create_node`]'s dedup cache.  An
    /// [`entity_utils::EntityInterner`] owns both the forward `ConstId → value`
    /// map and the reverse value-dedup index.  Rebuilt over the live ids by
    /// [`Self::compact`].
    pub(crate) const_interner:
        entity_utils::EntityInterner<crate::const_value::ConstId, crate::const_value::ConstValue>,
}

impl Function {
    /// Creates a `Function` with the `Entry` node (node 0) already built,
    /// carrying the calling-convention SSoT (`default_cc`, `endianness`,
    /// `all_vns`) at construction.  These three are the non-derivable inputs
    /// every register-list projection a `Call` / `Return` / `CallOther` needs is
    /// derived from, so requiring them here guarantees a `Function` is never
    /// observed in a half-initialised state (no build-then-assign window).
    /// Because the entry is built here, it is always present — [`Self::entry`]
    /// returns a `NodeId`, never an `Option`.  The `InitialMemory` node is added
    /// separately by the `FunctionBuilder`'s `build_entry`.
    pub fn new(
        default_cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
        tracked_vns: Vec<rsleigh::Vn>,
    ) -> Self {
        // Build the `Entry` node (node 0) directly on the empty graph.  It is an
        // asm-fingerprint-exempt initial-state kind, so it needs no contributor
        // attribution and is minted straight on the graph.  This is the SSoT for
        // "a function always has an entry" — every construction path goes through
        // here, so `entry` is a `NodeId`, never an `Option`.  The `InitialMemory`
        // node is the `FunctionBuilder`'s responsibility (`build_entry`), which
        // creates it and captures its memory token in one step.
        let mut graph = Graph::default();
        let entry = graph.create_node(
            crate::node::NodeKind::Entry,
            [],
            [crate::node::ValueKind::Control],
        );
        // The sort lives with the interner: the caller hands the (already
        // deduped + CC-seeded) tracked set in arbitrary order; sorting by
        // `(space, offset, size)` here makes `InitialVnId` assignment stable
        // and reproducible independent of CFG-collection order, so the `i`-th
        // interned id is the `i`-th tracked varnode.
        let mut tracked_vns = tracked_vns;
        tracked_vns.sort_by_key(vn_sort_key);
        let mut vn_interner: entity_utils::EntityInterner<crate::node::InitialVnId, rsleigh::Vn> =
            entity_utils::EntityInterner::default();
        for vn in tracked_vns {
            vn_interner.intern(vn);
        }
        Self {
            graph,
            entry,
            default_cc,
            endianness,
            vn_interner,
            side_tables: SideTables::default(),
            const_interner: entity_utils::EntityInterner::default(),
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

    /// Interns the integer value `value`, masked to `ty`'s width, returning its
    /// `ConstId`. The single canonicalisation point for ≤ u128 constants: equal
    /// (masked) values share one id regardless of declared type.
    pub fn intern_int_const(
        &mut self,
        value: u128,
        ty: crate::node::ValueType,
    ) -> crate::const_value::ConstId {
        let masked = value & ty.bit_mask_u128();
        self.const_interner
            .intern(crate::const_value::ConstValue::Bits(masked))
    }

    /// Interns a limbed integer value, canonicalising to `Bits` when the limbs
    /// fit `u128`. `limbs` is little-endian. A fits-`u128` value routes through
    /// [`Self::intern_int_const`] so it is masked to `ty`'s width — keeping the
    /// two builders symmetric (no unmasked `Bits` can slip in via the limb
    /// path). Genuinely-wide values (I256/I512) use the full declared width, so
    /// the `Wide` arm needs no sub-width masking.
    pub fn intern_int_const_limbs(
        &mut self,
        limbs: &[u64],
        ty: crate::node::ValueType,
    ) -> crate::const_value::ConstId {
        let cv = crate::const_value::ConstValue::Wide(limbs.to_vec().into_boxed_slice());
        match cv.fits_u128() {
            Some(v) => self.intern_int_const(v, ty),
            None => self.const_interner.intern(cv),
        }
    }

    /// Looks up a const value by id.  Panics on a dangling id — a node's
    /// `ConstId` is always valid by construction (the interner only grows until
    /// `compact`, which remaps), so the few defensive readers that tolerate a
    /// malformed graph (the validator's `DanglingConstId` guard, the debug
    /// renderers) probe `const_interner.get` directly.  Ids produced on a
    /// different function are not portable.
    pub(crate) fn const_value(&self, id: crate::const_value::ConstId) -> &crate::const_value::ConstValue {
        &self.const_interner[id]
    }

    /// Returns the function's entry node (always present — built by
    /// [`Function::new`]).
    #[inline]
    pub fn entry(&self) -> NodeId {
        self.entry
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
    /// Consumed by post-lift analyses that decode multi-byte values (the
    /// optimizer's ROM-const evaluation and stack high/low-half splits).
    #[inline]
    pub fn endianness(&self) -> strider_target::Endianness {
        self.endianness
    }

    /// The ordered tracked-varnode SSoT (the `vn_interner`'s values, in
    /// `InitialVnId` order).
    pub fn all_vns(&self) -> &[rsleigh::Vn] {
        self.vn_interner.values_as_slice()
    }

    /// Resolve an [`crate::node::InitialVnId`] (carried by an
    /// `InitialVar` node) back to its varnode.  Panics on an
    /// out-of-range id — every id in the graph is minted from this
    /// function's `vn_interner`, so a miss is a structural invariant break.
    pub fn initial_vn(&self, id: crate::node::InitialVnId) -> rsleigh::Vn {
        self.vn_interner[id]
    }

    /// Non-panicking [`Self::initial_vn`] — `None` if `id` is out of range.
    /// For diagnostic consumers (the dot dumpers) that must tolerate a
    /// partially-built graph; analysis code uses `initial_vn` and relies on
    /// the invariant.
    pub(crate) fn initial_vn_opt(&self, id: crate::node::InitialVnId) -> Option<rsleigh::Vn> {
        self.vn_interner.get(id).copied()
    }

    /// Resolve a tracked varnode to its [`crate::node::InitialVnId`], or
    /// `None` when `vn` is not a tracked variable.  The reverse of
    /// [`Self::initial_vn`]; the [`crate::FunctionBuilder`] uses it as its
    /// variable-table lookup during construction.
    pub(crate) fn vn_id_of(&self, vn: &rsleigh::Vn) -> Option<crate::node::InitialVnId> {
        self.vn_interner.key_of(vn)
    }

    /// Every tracked-varnode id, in `InitialVnId` (allocation) order.  The
    /// builder iterates this to create one `InitialVar` / `Phi` per tracked
    /// variable.
    pub(crate) fn vn_ids(&self) -> impl Iterator<Item = crate::node::InitialVnId> + '_ {
        self.vn_interner.keys()
    }

    /// Test-only: rebuild the tracked-varnode interner from `vns` (in order),
    /// so `InitialVnId(i)` resolves to `vns[i]`.  White-box validator / CC
    /// tests use it to declare the tracked set of a hand-built function.
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_all_vns(&mut self, vns: Vec<rsleigh::Vn>) {
        let mut interner: entity_utils::EntityInterner<crate::node::InitialVnId, rsleigh::Vn> =
            entity_utils::EntityInterner::default();
        for vn in vns {
            interner.intern(vn);
        }
        self.vn_interner = interner;
    }


    /// The shared call-clobber predicate: a register (resolved to its tracked
    /// container) is clobbered iff it is neither callee-saved under `cc` nor the
    /// stack pointer.  The callee-saved set is hashed once so the predicate is
    /// O(1) per probe (keeping the `call_*_for` derivations O(N), not O(N·M)).
    /// CC regs are resolved to their tracked container first so a sub-register
    /// ABI reg matches the wider tracked vn.
    ///
    /// TEST-SUPPORT ONLY: the prod lift path derives these CC register-list
    /// projections in `strider-lift` (`lift::cc_projection`).  These copies
    /// exist for the `RegisterSet` / `build_call_cc` test fixtures and the
    /// cross-crate CC-shape tests, which is why they are feature-gated.
    #[cfg(any(test, feature = "test-util"))]
    fn clobber_oracle(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> impl Fn(&rsleigh::Vn) -> bool + use<> {
        let stack_vn = cc.stack_vn;
        let callee_saved: FxHashSet<rsleigh::Vn> = cc
            .callee_saved_regs
            .iter()
            .map(|v| largest_container_in(self.all_vns(), v))
            .collect();
        move |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn
    }

    /// The convention's combined return-register list (integer ++ float),
    /// each resolved to its tracked container via [`largest_container_in`].
    ///
    /// The shared CC-ret-reg → container chain behind both
    /// [`Self::call_ret_vals_for`] (which keeps the tracked + clobbered ones)
    /// and [`Self::call_clobbered_for`] (which excludes them) — one place that
    /// owns "the ret regs, in container coordinates".
    #[cfg(any(test, feature = "test-util"))]
    fn combined_ret_containers<'a>(
        &'a self,
        cc: &'a strider_target::BuiltCallingConvention,
    ) -> impl Iterator<Item = rsleigh::Vn> + 'a {
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| largest_container_in(self.all_vns(), v))
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
    /// its tracked container via [`largest_container_in`] before membership
    /// is tested, and the resolved CONTAINER is emitted.  This keeps a
    /// sub-register ABI ret reg (e.g. `eax`) classified as the return
    /// value when the function tracks the wider container (`rax`) instead
    /// of silently dropping it.  Identity on full-width preset regs.
    #[cfg(any(test, feature = "test-util"))]
    pub fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let is_clobbered = self.clobber_oracle(cc);
        // The combined ret list is tiny (1-2 regs), so a linear probe against
        // `all_vns` is cheaper (and allocation-free) than hashing the whole
        // tracked register file to test 1-2 items.
        self.combined_ret_containers(cc)
            .filter(|c| self.vn_interner.contains(c) && is_clobbered(c))
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
    /// its tracked container via [`largest_container_in`] before it is used
    /// to exclude entries here, so a sub-register ABI ret reg (e.g. `eax`)
    /// whose tracked container is wider (`rax`) is correctly excluded from
    /// the clobber tail rather than mis-filed as a clobber.  Identity on
    /// full-width preset regs.
    ///
    /// To obtain the FULL combined set (ret-vals ++ clobbers) for callers
    /// that need the old single-list shape, chain the two accessors:
    /// `call_ret_vals_for(cc).into_iter().chain(call_clobbered_for(cc))`.
    #[cfg(any(test, feature = "test-util"))]
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
        let ret_vars: FxHashSet<rsleigh::Vn> = self.combined_ret_containers(cc).collect();
        // Only REGISTER / UNIQUE varnodes can be clobbered: clobbering a CONST /
        // RAM tracked temp is meaningless (and the dumb `build_call` rejects a
        // non-reg output vn).
        self.all_vns()
            .iter()
            .copied()
            .filter(|v| {
                matches!(
                    v.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                ) && is_clobbered(v)
                    && !ret_vars.contains(v)
            })
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
    #[cfg(any(test, feature = "test-util"))]
    #[inline]
    pub fn call_clobbered_regs(&self) -> Vec<rsleigh::Vn> {
        self.call_clobbered_for(&self.default_cc)
    }

    /// The function-default ret-val varnode list (derived against
    /// [`Self::default_cc`]).  Convenience for consumers that want the
    /// default-CC ret-val shape.
    #[cfg(any(test, feature = "test-util"))]
    #[inline]
    pub fn call_ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        self.call_ret_vals_for(&self.default_cc)
    }

    /// The calling convention's combined return-value register list
    /// (integer then float, in ABI order), at each register's declared
    /// width — no tracked-container projection.  The registers are read
    /// through the lifter's aliasing-aware read path at use sites, which
    /// resolves each declared register to its tracked container (and errors
    /// if none exists), so the raw declared list is
    /// the right shape: a wider register (e.g. `RSI`) is read at its full
    /// width rather than being narrowed to a tracked sub-register.
    #[inline]
    pub fn ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        let cc = &self.default_cc;
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
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

    // ── NodeId-keyed overlay accessors ────────────────────────────────────

    /// Returns the user-op name associated with a
    /// [`crate::node::NodeKind::CallOther`] node, or `None` if no name has
    /// been recorded for that node.
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.side_tables.call_other_names[node_id].as_deref()
    }

    /// Returns the source varnode a value represents, or `None`. Single
    /// value-keyed view over `value_vn`, which tags three populations: a
    /// lift-time `Phi`'s tracked varnode, a `Call`/`CallOther` ret-val
    /// output's return register, and a `Call`/`CallOther` clobber output's
    /// clobbered register.
    ///
    /// CONTRACT — `value_vn` holds TWO disjoint facts, distinguished only by
    /// the producing node's kind (they never collide because `Phi` outputs and
    /// `Call`/`CallOther` outputs are distinct `ValueId`s): a lift-time `Phi`'s
    /// source-level varnode tag, and a `Call`/`CallOther` ret-val / clobber
    /// output's register.  A reader must therefore filter by `producer(value)`'s
    /// kind before interpreting the tag — e.g. the jump-table classifier's
    /// Phi-of-IntConst arm must not mistake a clobber tag for a phi tag.
    #[inline]
    pub fn get_vn_for_value(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.side_tables
            .value_vn
            .get(&value)
            .map(|&id| self.initial_vn(id))
    }

    /// Tag `value` with the tracked varnode `vn`.  A no-op when `vn` is not a
    /// tracked varnode (no `VnId`): a source-register tag is only meaningful for
    /// a tracked vn, so an untracked one is left untagged rather than stored.
    #[inline]
    pub fn set_vn_for_value(&mut self, value: ValueId, vn: rsleigh::Vn) {
        if let Some(vn_id) = self.vn_id_of(&vn) {
            self.side_tables.value_vn.insert(value, vn_id);
        }
    }

    /// Records `cc` as the per-`Call` override calling convention for
    /// `node_id`.  Replaces any prior override.  Read back via [`Self::get_cc`]
    /// (whose `stack_args` is the call's effective stack-arg layout).
    #[inline]
    pub fn set_call_cc(&mut self, node_id: NodeId, cc: strider_target::BuiltCallingConvention) {
        self.side_tables.call_cc.insert(node_id, cc);
    }

    /// The **effective** calling convention for `node_id`: the per-`Call`
    /// override if one was recorded, else the function-default CC.  So
    /// `get_cc(call).stack_args` is the call's effective stack-arg layout with
    /// no override-vs-default branch at the call site.
    #[inline]
    pub fn get_cc(&self, node_id: NodeId) -> &strider_target::BuiltCallingConvention {
        self.side_tables
            .call_cc
            .get(&node_id)
            .unwrap_or(&self.default_cc)
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
        self.side_tables.arg_index_to_values
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
        self.side_tables.arg_index_to_values
            .entry(index)
            .or_default()
            .push(value);
    }

    /// Iterate over all registered argument indices (unordered).
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.side_tables.arg_index_to_values.keys().copied()
    }

    /// Drop registered argument carriers for every index `>= first`.
    ///
    /// Lets the stack-arg detection pass rebuild only the stack-arg portion of
    /// the table idempotently across the orchestrator's stable iterations,
    /// without disturbing the register-arg carriers recorded at builder entry
    /// (which occupy indices `0 .. first`).
    #[inline]
    pub fn clear_arg_values_from(&mut self, first: u32) {
        self.side_tables.arg_index_to_values.retain(|&index, _| index < first);
    }

    // ── stack_offsets accessors ───────────────────────────────────────────

    /// Returns the stack slot `(base, offset)` recorded for a Store/Load
    /// node, or `None` if the node has no recorded slot (non-stack node, or
    /// a phi-of-offsets address whose single concrete offset cannot be
    /// named).  `base` is the SP-derived terminal node the offset is
    /// relative to; the offset is only comparable against another access's
    /// offset when their bases match.
    #[inline]
    pub fn stack_offset(&self, id: NodeId) -> Option<(ValueId, i128)> {
        self.side_tables.stack_offsets[id]
    }

    /// Records a concrete stack slot `(base, offset)` for a Store/Load node.
    #[inline]
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i128) {
        self.side_tables.stack_offsets[id] = Some((base, offset));
    }

    // ── initial_var_index accessors ───────────────────────────────────────

    /// Iterates the `initial_var_index` as `(vn, node_id)` pairs.  Used by the
    /// validator to enforce that every entry still resolves to a live
    /// `InitialVar(id)` node with the matching varnode.  Resolves each
    /// [`crate::node::InitialVnId`] key back to its varnode at the boundary so
    /// callers stay `Vn`-facing.
    #[inline]
    pub(crate) fn initial_var_index_entries(
        &self,
    ) -> impl Iterator<Item = (rsleigh::Vn, NodeId)> + '_ {
        self.side_tables
            .initial_var_index
            .iter()
            .map(|(&vn_id, &id)| (self.initial_vn(vn_id), id))
    }

    /// Iterates the `value_vn` map as `(value, vn)` pairs.  Used by the
    /// validator to enforce that every key is a live value output.
    #[inline]
    pub(crate) fn value_vn_entries(&self) -> impl Iterator<Item = (ValueId, rsleigh::Vn)> + '_ {
        self.side_tables
            .value_vn
            .iter()
            .map(|(&value, &id)| (value, self.initial_vn(id)))
    }

    /// Returns the entry-stack-pointer node — the `InitialVar(stack_vn)` node,
    /// where `stack_vn` is the calling convention's stack pointer — or `None`
    /// when the function tracks no such node (`stack_vn` deduped into a wider
    /// container).  Its output value is the entry SP.
    ///
    /// O(1) via the `initial_var_index` accelerator.  This does **not** filter
    /// by liveness: the map can transiently hold a node culled-but-not-yet-
    /// compacted mid-pipeline, so a caller that cares whether the SP is actually
    /// read checks the node against its own live-set (every optimization runs in
    /// an [`crate::EditFunction`] that maintains one) — a culled `InitialVar(sp)`
    /// is never referenced by a live load anyway.
    pub fn initial_sp(&self) -> Option<NodeId> {
        let sp_id = self.vn_id_of(&self.default_cc.stack_vn)?;
        self.side_tables.initial_var_index.get(&sp_id).copied()
    }

    /// The `InitialVar(vn)` node's output value for a tracked varnode `vn`, or
    /// `None` when `vn` is not tracked (no [`crate::node::InitialVnId`], so no
    /// `InitialVar` node was created for it).
    ///
    /// O(1) via the `initial_var_index` accelerator.  The lifter uses it right
    /// after `set_entry_region` to record register-passed arguments: an
    /// arg-passing register resolves (lifter-side) to its tracked container,
    /// and this returns that container's entry value — the carrier for the
    /// argument's positional index.
    pub fn initial_var_value(&self, vn: &rsleigh::Vn) -> Option<ValueId> {
        let id = self.vn_id_of(vn)?;
        let node = *self.side_tables.initial_var_index.get(&id)?;
        self.graph.node_outputs(node).first().copied()
    }

    /// Returns the asm-instruction-address fingerprint of `node_id` as a
    /// sorted-deduplicated slice.  Returns an empty slice when no
    /// contributors have been recorded.
    #[inline]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.side_tables.asm_fingerprints[id].as_slice()
    }

    /// Unions `contributors` into `node_id`'s fingerprint.  Result is kept
    /// sorted and deduplicated.  Existing entries are never removed: this
    /// satisfies the no-shrink contract.  Empty `contributors` is a no-op.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        if contributors.is_empty() {
            return;
        }
        // Union `contributors` into the node's fingerprint, keeping it sorted
        // and deduplicated.  Both `m` (existing) and `k` (contributors) are
        // tiny (a handful of addresses), so the standard extend + sort + dedup
        // is the right wheel — no hand-rolled two-pointer merge needed.
        let fp = &mut self.side_tables.asm_fingerprints[node_id];
        fp.extend_from_slice(contributors);
        fp.sort_unstable();
        fp.dedup();
    }

    /// Unions the fingerprint of `src` into `dst`.  Self-extension
    /// (`src == dst`) is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        let src_slice: smallvec::SmallVec<[u64; 4]> =
            self.side_tables.asm_fingerprints[src].iter().copied().collect();
        self.extend_asm_fingerprint(dst, &src_slice);
    }

    /// Same as [`Graph::create_node`] plus unions the asm-fingerprint of
    /// every node in `contributors` into the resulting node.
    ///
    /// This is the canonical node-creation funnel for ALL mutable paths:
    /// `FunctionBuilder::create_node` (the lift-time path),
    /// [`crate::EditFunction::create_node_attributed`] (the rewrite /
    /// template-engine path), and any direct caller.
    ///
    /// `IntConst` no longer needs special-casing here: every `ConstId` is
    /// minted through [`Self::intern_int_const`] / [`Self::intern_int_const_limbs`]
    /// (which mask-to-width and canonicalise by value), so a constant arrives
    /// pre-canonical and passes straight through to the dedup cache.
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

    /// Compacts the arena down to the nodes reachable from `entry` via the
    /// control-aware walk (control-out forward + data-in backward), returning
    /// the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `ValueId` / `UseId` values are invalidated;
    /// callers holding any such id MUST rewrite it through the returned
    /// [`NodeIdRemap`].
    ///
    /// `Graph::retain_reachable` retains exactly the set it is handed; the IR's
    /// reachability follows forward-control + backward-data edges (so a `Region`
    /// reached only via control survives), so this passes the FULL control-aware
    /// walk.  That set is already closed under data inputs, satisfying the
    /// generic pass's backward-input precondition; its cacher rebuild re-keys the
    /// dedup cache over the survivors.
    ///
    /// "Reachable" is from [`Self::entry`] — the receiver owns the anchor, so it
    /// is read internally rather than passed (a non-entry root would be a misuse:
    /// "retain reachable" means "from the function's entry").
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::entry`] is `None`.  Otherwise infallible in
    /// practice; the `Result` is kept so a future invariant check has a typed
    /// channel.
    ///
    /// Crate-internal: this remaps only the graph and leaves the `Function`
    /// side-tables stale, so [`Self::compact`] (which remaps them) is the only
    /// safe public entry point.
    pub(crate) fn retain_reachable(&mut self) -> crate::Result<NodeIdRemap> {
        // Collect the reachable set into a `Vec` first: that ends the
        // immutable borrow before the mutable `graph_mut()` borrow below.
        let reachable: Vec<NodeId> = self.walk().collect();
        Ok(self.graph_mut().retain_reachable(reachable))
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
    /// Returns an error if the retain-reachable remap doesn't include the
    /// entry (invariant violation).
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        let entry = self.entry;
        let remap = self.retain_reachable()?;
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "Function::compact: entry {:?} missing from remap (invariant violation)",
                entry
            )
        })?;
        self.entry = new_entry;
        // Remap all the arena-id-keyed overlay tables through the old→new
        // translation produced by `retain_reachable`.  The `vn_interner` and
        // `default_cc` are untouched: the tracked-vn set does not change when
        // dead nodes are culled, so `InitialVnId` assignment is stable and the
        // interner needs no remap (which is why `initial_var_index`, now
        // `InitialVnId`-keyed, remaps only its NodeId payload).
        self.side_tables.remap(&remap);
        // GC the const interner over only the values referenced by surviving
        // `IntConst(id)` nodes, rewriting each survivor's id to the new dense
        // id, then re-key the graph's dedup cache over those rewritten ids.
        // The dedup cache keys on `NodeKind` (which carries the `ConstId`), so
        // the rewrite must precede the cache rebuild.  The rebuild is
        // unconditional: `retain_reachable` has already reassigned every
        // surviving node's id, so the cache (keyed on those ids) is stale
        // regardless of whether any constants were rewritten.
        self.gc_consts();
        self.graph.rebuild_cache();
        Ok(remap)
    }

    /// Rebuilds [`Self::const_interner`] over only the values referenced by
    /// surviving `IntConst(id)` nodes, rewriting each such node's id in place
    /// to the new id assigned by the rebuilt interner.  When no constant nodes
    /// survive — including the case where every constant was pruned by
    /// `retain_reachable` — the interner is simply reset to empty (a valid
    /// post-optimization state).
    ///
    /// Only safe to call after [`Graph::retain_reachable`] has settled
    /// the arena — at that point `self.graph.nodes.keys()` iterates only
    /// surviving nodes, so the live-id scan correctly excludes zombie
    /// references.
    fn gc_consts(&mut self) {
        use crate::const_value::ConstId;
        use crate::node::NodeKind;

        let mut live_old_ids: Vec<ConstId> = Vec::new();
        let mut const_nodes: Vec<NodeId> = Vec::new();
        for node in self.graph.all_node_ids() {
            if let NodeKind::IntConst(id) = *self.graph.node_kind(node) {
                const_nodes.push(node);
                live_old_ids.push(id);
            }
        }
        let mut new_interner: entity_utils::EntityInterner<
            ConstId,
            crate::const_value::ConstValue,
        > = entity_utils::EntityInterner::default();
        let mut old_to_new: FxHashMap<ConstId, ConstId> = FxHashMap::default();
        for old_id in live_old_ids {
            if old_to_new.contains_key(&old_id) {
                continue;
            }
            let value = self.const_interner[old_id].clone();
            let new_id = new_interner.intern(value);
            old_to_new.insert(old_id, new_id);
        }
        self.const_interner = new_interner;
        for node in const_nodes {
            if let NodeKind::IntConst(id) = self.graph.node_kind_mut(node)
                && let Some(&new_id) = old_to_new.get(id)
            {
                *id = new_id;
            }
        }
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
        let entry = self.entry;
        let node_to_arg_indices = crate::function::dot::build_arg_reverse_map(self);
        Ok(crate::function::dot::FunctionDotDumper {
            entry,
            function: self,
            sleigh,
            node_to_arg_indices,
        })
    }
}

/// `Function` is itself an [`crate::IRBuilder`]: it owns the canonical
/// [`Self::create_node_attributed`] funnel, so the blanket
/// [`crate::IRBuilderExt`] construction vocabulary (`build_int_const`,
/// `build_int_const_limbs`, …) is available directly on a `Function` for
/// synthetic / test construction.  Unlike [`crate::FunctionBuilder`] this
/// applies no ambient `lift_addr` stamp — callers that need fingerprint
/// attribution pass contributors explicitly.
impl crate::IRBuilder for Function {
    fn function_mut(&mut self) -> &mut Function {
        self
    }

    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = crate::node::ValueId>,
        O: IntoIterator<Item = crate::node::ValueKind>,
    {
        Function::create_node_attributed(self, kind, inputs, outputs, contributors)
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use crate::IRViewer;
    use crate::function::test_function;
    use crate::node::{NodeKind, ValueKind};

    #[test]
    fn function_new_builds_entry_and_initial_memory_skeleton() {
        let f = test_function();
        let ids: Vec<_> = f.graph().all_node_ids().collect();
        assert_eq!(ids.len(), 2, "new() builds exactly Entry + InitialMemory");
        assert!(matches!(f.node_kind(ids[0]), NodeKind::Entry));
        assert!(matches!(f.node_kind(ids[1]), NodeKind::InitialMemory));
        assert_eq!(f.entry(), ids[0], "entry() points at the Entry node (node 0)");
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = test_function();
        let n = f.entry();
        f.extend_asm_fingerprint(n, &[0xDEAD_BEEF]);
        assert_eq!(f.asm_fingerprint(n), &[0xDEAD_BEEF]);
    }

    #[test]
    fn arg_index_to_values_returns_empty_for_unregistered() {
        let f = test_function();
        assert!(f.arg_index_to_values(0).is_empty());
        assert!(f.arg_index_to_values(99).is_empty());
    }

    #[test]
    fn register_arg_value_supports_multiple_values_per_index() {
        let mut f = test_function();
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

        let mut f = test_function();
        let phi = f
            .graph_mut()
            .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
        let phi_value = f.node_outputs(phi)[0];
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        f.set_all_vns(vec![vn]); // only a tracked vn (with a VnId) can be tagged
        assert_eq!(f.get_vn_for_value(phi_value), None);
        f.set_vn_for_value(phi_value, vn);
        assert_eq!(f.get_vn_for_value(phi_value), Some(vn));
    }

    /// `arg_index_to_values` stores a carrier's value and `producer` recovers
    /// the carrier node.
    #[test]
    fn arg_index_to_values_recovers_carrier_node_via_producer() {
        use crate::node::ValueType;

        let mut f = test_function();
        let carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
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
    use crate::function::{test_function, test_initial_memory};
    use crate::node::{NodeId, NodeKind, ValueKind, ValueType};

    /// Interns `v` (masked to `ty`) and creates a single-output `IntConst`
    /// node, returning its `NodeId`.
    fn int_const_node(f: &mut Function, v: u128, ty: ValueType) -> NodeId {
        let id = f.intern_int_const(v, ty);
        f.graph_mut()
            .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)])
    }

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = test_function();
        let _zombie = int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::I64);
        let pre_count = f.graph().all_node_ids().count();

        let _remap = f.compact().expect("compact succeeds on a valid function");

        let post_count = f.graph().all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let entry_id = f.entry();
        let outs: Vec<_> = f.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(f.value_kind(outs[0]).is_control());
    }

    /// `compact` GCs the wide-const interner and remaps surviving
    /// `IntConst(Wide(id))` references.  A wide const on a DROPPED node
    /// (interned first → id 0) is collected, forcing the live wide const's
    /// id to shift; the survivor must still be a `Wide` `IntConst` whose
    /// remapped id resolves to its original value.  Without a correct GC +
    /// payload rewrite the survivor would dangle or read the wrong constant.
    #[test]
    fn compact_gcs_and_remaps_surviving_wide_const() {
        use crate::const_value::ConstValue;
        use crate::node::ValueType;

        // Genuinely-wide I256 value (high limb set ⇒ stays `Wide`).
        const LIVE_LIMBS: [u64; 4] = [
            0x1122_3344_5566_7788,
            0x99AA_BBCC_DDEE_FF00,
            0,
            0x8000_0000_0000_0000,
        ];

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // A wide const referenced only by a zombie (interned FIRST → id 0).
        let dropped_id = f.intern_int_const_limbs(&[0xAAAA_BBBB, 0, 0, 1], ValueType::I256);
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(dropped_id),
            [],
            [ValueKind::Typed(ValueType::I256)],
        );

        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();

        // The live wide const, referenced by a reachable Return.
        let live_id = f.intern_int_const_limbs(&LIVE_LIMBS, ValueType::I256);
        let wide_node = f.graph_mut().create_node(
            NodeKind::IntConst(live_id),
            [],
            [ValueKind::Typed(ValueType::I256)],
        );
        let [wide_value] = f.node_outputs_exact::<1>(wide_node).unwrap();
        f.graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value, wide_value], []);

        let remap = f.compact().expect("compact succeeds");

        let new_wide = remap
            .node_old_to_new(wide_node)
            .expect("the referenced wide const survives");
        let NodeKind::IntConst(new_id) = *f.node_kind(new_wide) else {
            panic!("expected IntConst(_), got {:?}", f.node_kind(new_wide));
        };
        assert_eq!(
            f.const_value(new_id),
            &ConstValue::Wide(LIVE_LIMBS.to_vec().into_boxed_slice()),
            "GC'd + remapped const id must still resolve to its value",
        );
    }

    /// A SURVIVING `stack_offsets` entry is remapped through compaction on
    /// BOTH coordinates: its key (`NodeId`) and its value's base
    /// (`ValueId`).  A zombie allocated before the live nodes forces a
    /// non-trivial id shift, so the test fails if either side is left
    /// unremapped.  (The drop-on-death side is pinned by
    /// `retain_reachable_drops_side_table_entry_for_dropped_node`.)
    #[test]
    fn compact_remaps_surviving_stack_offset_entry() {
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Zombie before the surviving nodes so their ids shift during compaction.
        let zombie = int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::I64);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let base = int_const_node(&mut f, 0x7000_u128, crate::node::ValueType::I64);
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
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        // Reachable IntConst whose Return-input consumer keeps it live.
        let surviving = int_const_node(&mut f, (0xCAFE_u64) as u128, crate::node::ValueType::I64);
        let [surv_value] = f.node_outputs_exact::<1>(surviving).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, surv_value], []);

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

        let mut f = test_function();
        // Entry + InitialMemory (auto-built) + a Return (minimal reachable graph).
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);

        // Zombie: a cacheable IntConst not connected to anything reachable.
        let zombie = int_const_node(&mut f, (0xC0FFEE_u64) as u128, crate::node::ValueType::I64);

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

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);

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
        f.set_all_vns(vec![dead_vn]); // only a tracked vn can be tagged
        f.set_vn_for_value(zombie_phi_value, dead_vn);
        assert_eq!(
            f.get_vn_for_value(zombie_phi_value),
            Some(dead_vn),
            "tag must be set before compact"
        );

        // Zombie IntConst node with a stack_offsets entry.
        let zombie_stack =
            int_const_node(&mut f, (0xBEEF_u64) as u128, crate::node::ValueType::I64);
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

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // A zombie created *before* the arg carrier so that compaction
        // reassigns the carrier's NodeId (the zombie's slot is dropped).
        let _zombie = int_const_node(&mut f, (0xDEAD_u64) as u128, crate::node::ValueType::I64);
        // The arg carrier: a register-arg-style InitialVar kept live by Return.
        let arg_node = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [arg_value] = f.node_outputs_exact::<1>(arg_node).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, arg_value], []);
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

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
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
        f.set_all_vns(vec![live_vn, dead_vn]); // only tracked vns can be tagged
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

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let live_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [live_value] = f.node_outputs_exact::<1>(live_carrier).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, live_value], []);

        // A pruned (unreachable) carrier for a different arg index.
        let dead_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(1)),
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

        let mut f = test_function();
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
        f.set_all_vns(vec![vn]); // only a tracked vn can be tagged
        assert_eq!(f.get_vn_for_value(clobber_value), None);
        f.set_vn_for_value(clobber_value, vn);
        // Recoverable per-output: the clobber output value carries its Vn.
        assert_eq!(f.get_vn_for_value(clobber_value), Some(vn));
        // Control / Memory outputs carry no clobber tag.
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[0]), None);
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[1]), None);
    }

    /// `call_cc` round-trips and its stack-arg offsets are what the derived
    /// `get_cc().stack_args` returns; compact remaps
    /// both the per-Call `call_cc` and the per-output clobber `value_vn`.
    #[test]
    fn compact_remaps_call_cc_and_clobber_value_vn() {
        use crate::node::ValueType;

        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().unwrap();
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .unwrap();

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // A zombie created before the Call so compaction reassigns ids.
        let _zombie = int_const_node(&mut f, (0xDEAD_u64) as u128, crate::node::ValueType::I64);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let target = int_const_node(&mut f, 0x1000_u128, crate::node::ValueType::I64);
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
        f.set_all_vns(vec![clob_vn]); // only a tracked vn can be tagged
        f.set_vn_for_value(clob, clob_vn);
        f.set_call_cc(call, cc.clone());
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, call_mem], []);

        // Pre-compact: round-trips.  The override differs from the trivial
        // default, so get_cc returns it and its stack_args derive from it.
        assert_ne!(f.get_cc(call), f.default_cc());
        assert_eq!(f.get_cc(call).stack_args, cc.stack_args,);
        assert_eq!(f.get_vn_for_value(clob), Some(clob_vn));

        let remap = f.compact().expect("compact must succeed");
        let new_call = remap
            .node_old_to_new(call)
            .expect("live Call must survive compaction");
        let new_clob = remap
            .value_old_to_new(clob)
            .expect("live clobber output value must survive compaction");

        // The override CC survives the NodeId remap; stack-arg offsets derive.
        assert_ne!(f.get_cc(new_call), f.default_cc());
        assert_eq!(f.get_cc(new_call).stack_args, cc.stack_args,);
        // The clobber tag survives the ValueId remap.
        assert_eq!(f.get_vn_for_value(new_clob), Some(clob_vn));
    }
}
