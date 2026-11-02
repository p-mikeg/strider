use crate::IRViewer;
use crate::IRWalker;
use crate::function::side_tables::FunctionSideTables;
use crate::graph::{Graph, NodeIdRemap};
use crate::node::{NodeId, NodeKind, ValueId};

/// The ret-val + clobber varnode groups a `Call` under `cc` emits, over
/// `function`'s tracked varnodes.
#[cfg(any(test, feature = "test-util"))]
pub fn cc_ret_and_clobber_vns(
    function: &Function,
    cc: &strider_target::BuiltCallingConvention,
) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
    let all = function.all_vns();
    cc.ret_and_clobber_vns(all, |v| vn_container::largest_container_in(all, v))
}

/// A lifted function: structural [`Graph`] plus per-function overlay state.
/// Always has an entry node; `Clone` is a deep, independent copy.
///
/// The overlay is grouped by kind, so where a field lives says what it is:
/// the facts the function rests on, settled once and touched by no operation,
/// then every id-keyed payload table in [`FunctionSideTables`], then the memos the
/// analyses derive from the graph and [`Self::invalidate_analyses`] drops.
#[derive(Clone)]
pub struct Function {
    graph: Graph,

    // The facts.  None is keyed by an arena id, so [`Self::compact`] leaves
    // them alone, and none is derived from the graph, so
    // [`Self::invalidate_analyses`] never clears one.  A derived, clearable
    // verdict is a memo and belongs in `memos` instead.
    /// The convention this function was built under.
    default_cc: strider_target::BuiltCallingConvention,
    /// The byte order of each space's loads and stores.
    byte_order: strider_target::ByteOrder,
    /// The claims in force: every set [`Self::assume`] added, none before the
    /// first.  A claim is withdrawn by nothing.
    assumptions: crate::AssumptionOptions,
    /// The highest offset from the entry SP of a stack address held, at some
    /// `Call`, in a register the call clobbers, or `None` when no call held
    /// one.  Recorded once, by `FunctionBuilder::build`.  The call node carries
    /// no such value, so this is the only record that a callee may have
    /// received it.
    call_register_frame_reach: Option<i128>,

    side_tables: FunctionSideTables,

    /// What the analyses derived from the graph.  Written through a shared
    /// borrow, so an analysis holding `&Function` fills them in, and dropped
    /// whole by [`Self::invalidate_analyses`].
    pub(super) memos: std::cell::RefCell<crate::function::memos::FunctionMemos>,
}

impl Function {
    /// Builds the `Entry` node.  `InitialMemory` is the `FunctionBuilder`'s job.
    ///
    /// `tracked_vns` keeps only the largest of nested varnodes
    /// ([`vn_container::dedup_overlapping_largest`]), sorted by space shortcut,
    /// offset and size. A bare `Endianness` is the order of every space.
    pub fn new(
        default_cc: strider_target::BuiltCallingConvention,
        byte_order: impl Into<strider_target::ByteOrder>,
        tracked_vns: Vec<rsleigh::Vn>,
    ) -> Self {
        let mut graph = Graph::default();
        graph.create_node(
            crate::node::NodeKind::Entry,
            [],
            [crate::node::ValueKind::Control],
        );
        // Arbitrary order in, so sort: `InitialVnId` assignment is
        // reproducible.
        let mut tracked_vns = vn_container::dedup_overlapping_largest(&tracked_vns);
        tracked_vns.sort_by_key(|vn| (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size));
        let mut vn_interner = entity_utils::EntityInterner::default();
        for vn in tracked_vns {
            vn_interner.intern(vn);
        }
        Self {
            graph,
            default_cc,
            byte_order: byte_order.into(),
            // A function claims nothing until `assume` adds some.
            assumptions: crate::AssumptionOptions::none(),
            call_register_frame_reach: None,
            side_tables: FunctionSideTables::new(vn_interner),
            memos: std::cell::RefCell::default(),
        }
    }

    #[inline]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    #[inline]
    pub fn side_tables(&self) -> &FunctionSideTables {
        &self.side_tables
    }

    #[inline]
    pub fn side_tables_mut(&mut self) -> &mut FunctionSideTables {
        &mut self.side_tables
    }

    /// The claims in force: the union of every [`Self::assume`], none before
    /// the first.  Nothing withdraws one, so only a fresh lift drops one.
    /// The exceptions are
    /// [`msr_register_keeps_mode`](crate::AssumptionOptions::msr_register_keeps_mode)
    /// and
    /// [`fp_rounding_mode_is_nearest`](crate::AssumptionOptions::fp_rounding_mode_is_nearest),
    /// which only the lift reads: assumed after it, such a claim is listed
    /// here while the graph does not rest on it.
    #[inline]
    pub fn assumptions(&self) -> &crate::AssumptionOptions {
        &self.assumptions
    }

    /// Whether some `Call` saw a stack address in a register it clobbers.
    #[inline]
    pub fn frame_address_in_call_register(&self) -> bool {
        self.call_register_frame_reach.is_some()
    }

    /// The highest offset from the entry SP of a stack address some `Call`
    /// saw in a register it clobbers: `i128::MAX` when one has no constant
    /// offset from it, `None` when no call saw one.
    #[inline]
    pub fn call_register_frame_reach(&self) -> Option<i128> {
        self.call_register_frame_reach
    }

    /// Sets [`Self::call_register_frame_reach`].
    #[inline]
    pub(crate) fn set_call_register_frame_reach(&mut self, reach: Option<i128>) {
        self.call_register_frame_reach = reach;
    }

    /// Adds `claims` to the claims in force, withdrawing none, and drops the
    /// graph-derived memos when one is new: a decomposition made without an
    /// allocator reads its return as not-memory.  A region filter or
    /// [`Self::stack_offset`] then reads nothing until a run or
    /// `strider_opt::refresh_memory_classes`.  A function never given a claim
    /// is optimized at the sound floor.  The lift seeds its claims here, so
    /// [`msr_register_keeps_mode`](crate::AssumptionOptions::msr_register_keeps_mode)
    /// and
    /// [`fp_rounding_mode_is_nearest`](crate::AssumptionOptions::fp_rounding_mode_is_nearest),
    /// which only the lift reads, are recorded without effect when assumed
    /// after it.
    pub fn assume(&mut self, claims: &crate::AssumptionOptions) {
        if claims.omitted_by(&self.assumptions).is_empty() {
            return;
        }
        self.assumptions.union_with(claims);
        self.invalidate_analyses();
    }

    /// The root of [`Self::graph`]: the arena's first node.  [`Self::new`]
    /// mints it first and [`Self::compact`] keeps the walk's order, which
    /// yields it first.
    #[inline]
    pub fn entry(&self) -> NodeId {
        NodeId::from_u32(0)
    }

    #[inline]
    pub fn default_cc(&self) -> &strider_target::BuiltCallingConvention {
        &self.default_cc
    }

    /// The data byte order. A REGISTER-space access follows
    /// [`Self::byte_order`]'s instead.
    #[inline]
    pub fn endianness(&self) -> strider_target::Endianness {
        self.byte_order.data
    }

    /// The byte order of each space's loads and stores.
    #[inline]
    pub fn byte_order(&self) -> strider_target::ByteOrder {
        self.byte_order
    }

    /// Integer then float, in ABI order, at each register's declared width with
    /// NO tracked-container projection.
    #[inline]
    pub fn ret_val_regs(&self) -> Vec<rsleigh::Vn> {
        let cc = &self.default_cc;
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .copied()
            .collect()
    }

    /// The `(base, byte offset)` of a Store/Load's address.  `None` when `node`
    /// is not a Store/Load, or its address is not SP-rooted, or it has not been
    /// decomposed yet.  Offsets compare only when their bases match.
    pub fn stack_offset(&self, node: NodeId) -> Option<(ValueId, i128)> {
        let addr = self.access_address(node)?;
        // Stack only: a heap address resolves through the same interner but is a
        // different region, so it is not an SP offset.
        match self.memory_decomp(addr) {
            (crate::MemDecomp::Stack(_), slot) => slot,
            _ => None,
        }
    }

    /// Whether `value` is the entry SP, the output of `InitialVar(stack_vn)`.
    pub fn is_entry_sp(&self, value: ValueId) -> bool {
        matches!(
            *self.node_kind(self.producer(value)),
            NodeKind::InitialVar(id) if self.initial_vn(id) == self.default_cc.stack_ptr
        )
    }

    /// Retains only nodes reachable from [`Self::entry`], remapping every
    /// overlay table through the same translation and returning it.
    /// Pre-compaction `NodeId` / `ValueId` / `UseId` values are invalidated;
    /// callers holding one MUST rewrite it through the returned
    /// [`NodeIdRemap`].  Entries whose node did not survive are dropped.
    /// Never returns `Err`.
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        // The pre-order yields the entry first, so it stays node 0.
        let reachable: Vec<NodeId> = self.walk().collect();
        let remap = self.graph.retain_reachable_stale_cache(reachable);
        debug_assert_eq!(remap.node_old_to_new(self.entry()), Some(self.entry()));
        debug_assert!(matches!(self.node_kind(self.entry()), NodeKind::Entry));
        self.side_tables.remap(&remap);
        self.memos.get_mut().remap(&remap);
        // The dedup cache keys on `NodeKind`, which carries the `IntConstId`, so
        // the id rewrite MUST precede the cache rebuild.
        self.side_tables.gc_payload_tables(&mut self.graph);
        self.graph.rebuild_cache();
        Ok(remap)
    }
}

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
        let node_id = self.graph.create_node(kind, inputs, outputs);
        for &src in contributors {
            self.side_tables.extend_asm_fingerprint_from(node_id, src);
        }
        node_id
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use crate::IRViewer;
    use crate::function::{test_function, test_function_tracking};
    use crate::node::{NodeKind, ValueKind};

    #[test]
    fn function_new_builds_entry_and_initial_memory_skeleton() {
        let f = test_function();
        let ids: Vec<_> = f.graph().all_node_ids().collect();
        assert_eq!(ids.len(), 2, "new() builds exactly Entry + InitialMemory");
        assert!(matches!(f.node_kind(ids[0]), NodeKind::Entry));
        assert!(matches!(f.node_kind(ids[1]), NodeKind::InitialMemory));
        assert_eq!(
            f.entry(),
            ids[0],
            "entry() points at the Entry node (node 0)"
        );
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = test_function();
        let n = f.entry();
        f.side_tables_mut()
            .extend_asm_fingerprint(n, &[0xDEAD_BEEF]);
        assert_eq!(
            f.side_tables().asm_fingerprint(n),
            rustc_hash::FxHashSet::from_iter([0xDEAD_BEEF])
        );
    }

    #[test]
    fn arg_index_to_values_returns_empty_for_unregistered() {
        let f = test_function();
        assert!(f.side_tables().arg_index_to_values(0).is_empty());
        assert!(f.side_tables().arg_index_to_values(99).is_empty());
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

        // The stack-args multi-Load case: two values on one index.
        f.side_tables_mut().register_arg_value(3, v1);
        f.side_tables_mut().register_arg_value(3, v2);

        let values = f.side_tables().arg_index_to_values(3);
        assert_eq!(values.len(), 2);
        assert!(values.contains(&v1));
        assert!(values.contains(&v2));

        assert!(f.side_tables().iter_arg_indices().any(|i| i == 3));
    }

    #[test]
    fn get_vn_for_value_round_trips_via_value_key() {
        use crate::node::ValueType;

        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let mut f = test_function_tracking(vec![vn]);
        let phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::int(64))]);
        let phi_value = f.node_outputs(phi)[0];
        assert_eq!(f.get_vn_for_value(phi_value), None);
        f.set_vn_for_value(phi_value, vn);
        assert_eq!(f.get_vn_for_value(phi_value), Some(vn));
    }

    #[test]
    fn arg_index_to_values_recovers_carrier_node_via_producer() {
        use crate::node::ValueType;

        let mut f = test_function();
        let carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let value = f.node_outputs(carrier)[0];
        f.side_tables_mut().register_arg_value(0, value);

        assert_eq!(f.side_tables().arg_index_to_values(0), &[value]);
        assert_eq!(f.graph().producer(value), carrier);
    }

    fn reg(addr_off: u64, size: u32) -> rsleigh::Vn {
        rsleigh::Vn {
            addr_space: rsleigh::VnSpace::REGISTER,
            addr_off,
            size,
        }
    }

    /// `Function::new` keeps the largest of nested varnodes, whoever builds
    /// it: an `InitialVnId` for `eax` beside `rax` would split one register
    /// into two SSA variables.
    #[test]
    fn new_tracks_only_the_largest_of_nested_varnodes() {
        let f = test_function_tracking(vec![reg(0, 4), reg(8, 8), reg(0, 8)]);
        assert_eq!(f.all_vns(), [reg(0, 8), reg(8, 8)]);
    }

    /// Only containment collapses: two partially overlapping varnodes are two
    /// variables, and a varnode listed twice is tracked once.
    #[test]
    fn new_keeps_partial_overlaps_and_one_copy_of_a_duplicate() {
        let f = test_function_tracking(vec![reg(4, 8), reg(0, 8), reg(4, 8)]);
        assert_eq!(f.all_vns(), [reg(0, 8), reg(4, 8)]);
    }

    /// A claim in force keeps the memos; a new one drops them and joins the
    /// claims.
    #[test]
    fn assume_drops_the_memos_only_when_it_adds_a_claim() {
        let mut f = test_function();
        assert_eq!(
            f.assumptions(),
            &crate::AssumptionOptions::none(),
            "a fresh function claims nothing"
        );
        let v = f.node_outputs(f.entry())[0];
        let memoized =
            |f: &super::Function| f.memory_decomp(v).1.is_some() && f.frame_escape().is_some();
        f.set_stack_slot(v, v, 16);
        f.set_frame_escape(true);
        f.assume(&crate::AssumptionOptions::none());
        assert!(memoized(&f), "nothing added, the memos stand");
        let listed = crate::AssumptionOptions {
            noalias_allocators: [0x1000].into_iter().collect(),
            ..crate::AssumptionOptions::none()
        };
        f.assume(&listed);
        assert!(f.memory_decomp(v).1.is_none());
        assert_eq!(f.frame_escape(), None);
        assert_eq!(f.assumptions(), &listed);
        f.set_stack_slot(v, v, 16);
        f.set_frame_escape(true);
        f.assume(&listed);
        assert!(memoized(&f), "a claim in force adds nothing");
        f.assume(&crate::AssumptionOptions {
            stack_global_disjoint: true,
            ..crate::AssumptionOptions::none()
        });
        assert_eq!(
            f.assumptions(),
            &crate::AssumptionOptions {
                stack_global_disjoint: true,
                ..listed.clone()
            },
            "a second set joins the first, withdrawing nothing"
        );
        assert!(!memoized(&f), "and the new claim drops the memos");
    }
}

#[cfg(test)]
mod compact_tests {

    use super::Function;
    use crate::IRViewer;
    use crate::function::{test_function, test_function_tracking, test_initial_memory};
    use crate::node::{NodeId, NodeKind, ValueKind, ValueType};

    fn int_const_node(f: &mut Function, v: u128, ty: ValueType) -> NodeId {
        let id = f.intern_int_const(v, ty);
        f.graph_mut()
            .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)])
    }

    /// Every fact names no id and is derived from no graph, so compaction and
    /// invalidation leave all of them, and `assume` touches only the claims.
    /// The destructuring stops compiling when a field joins [`Function`], so
    /// the pinning below is extended with it or the new field is named `_`.
    #[test]
    fn compact_and_invalidation_keep_every_fact() {
        let mut f = test_function();
        let before = f.clone();
        let listed = crate::AssumptionOptions {
            noalias_allocators: [0x4000].into_iter().collect(),
            ..crate::AssumptionOptions::none()
        };
        f.set_call_register_frame_reach(Some(-8));
        f.assume(&listed);

        f.invalidate_analyses();
        f.compact().expect("compact succeeds on a valid function");

        let Function {
            graph: _,
            default_cc,
            byte_order,
            assumptions,
            call_register_frame_reach,
            side_tables: _,
            memos: _,
        } = &f;
        assert_eq!(*default_cc, before.default_cc);
        assert_eq!(*byte_order, before.byte_order);
        assert_eq!(*assumptions, listed);
        assert_eq!(*call_register_frame_reach, Some(-8));
    }

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = test_function();
        let _zombie = int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::int(64));
        let pre_count = f.graph().all_node_ids().count();

        let _remap = f.compact().expect("compact succeeds on a valid function");

        let post_count = f.graph().all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // Node 0 is still the Entry, carrying its Control output.
        let entry_id = f.entry();
        assert!(matches!(f.node_kind(entry_id), NodeKind::Entry));
        let outs: Vec<_> = f.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(f.value_kind(outs[0]).is_control());
    }

    /// `retain_reachable_stale_cache` leaves the dedup cache keyed on
    /// PRE-compaction ids, so `compact` has to rebuild it.  An `IntBinaryOp` shows it
    /// where a bare const cannot: the cache key hashes the input `ValueId`s,
    /// which compaction renumbers, so a stale table misses the survivor and the
    /// next create allocates a structural duplicate of it.
    #[test]
    fn compact_rekeys_the_dedup_cache_to_the_new_ids() {
        use crate::node::ValueType;

        let mut f = test_function();
        let entry = f.entry();
        let mem_node = test_initial_memory(&f);
        let mem = f.node_outputs(mem_node)[0];
        // Allocated FIRST, so dropping them shifts every surviving `ValueId`.
        for v in 0..4u128 {
            let _zombie = int_const_node(&mut f, 0xdead_0000 + v, ValueType::int(64));
        }
        let a_node = int_const_node(&mut f, 7, ValueType::int(64));
        let b_node = int_const_node(&mut f, 9, ValueType::int(64));
        let a = f.node_outputs(a_node)[0];
        let b = f.node_outputs(b_node)[0];
        let sum = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a, b],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let sum_value = f.node_outputs(sum)[0];
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem, sum_value], []);

        let remap = f.compact().expect("compact succeeds on a valid function");
        let sum = remap.node_old_to_new(sum).expect("the sum is reachable");
        let a = remap.value_old_to_new(a).expect("lhs is reachable");
        let b = remap.value_old_to_new(b).expect("rhs is reachable");

        let again = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a, b],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert_eq!(
            again, sum,
            "a deduping create after compact must find the survivor under its NEW ids"
        );
    }

    /// Collecting a wide const held only by a dropped node (interned first, so
    /// id 0) forces the live one's id to shift.
    #[test]
    fn compact_gcs_and_remaps_surviving_wide_const() {
        use dashu_int::UBig;

        use crate::node::ValueType;

        // High limb set, so it stays past the carrier.
        const LIVE_LIMBS: [u64; 4] = [
            0x1122_3344_5566_7788,
            0x99AA_BBCC_DDEE_FF00,
            0,
            0x8000_0000_0000_0000,
        ];

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Interned first, so dropping it shifts the surviving const's id.
        let dropped_id = f.intern_int_const(
            UBig::from_words(&[0xAAAA_BBBB, 0, 0, 1]),
            ValueType::int(256),
        );
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(dropped_id),
            [],
            [ValueKind::Typed(ValueType::int(256))],
        );

        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();

        // The live wide const, referenced by a reachable Return.
        let live_id = f.intern_int_const(UBig::from_words(&LIVE_LIMBS), ValueType::int(256));
        let wide_node = f.graph_mut().create_node(
            NodeKind::IntConst(live_id),
            [],
            [ValueKind::Typed(ValueType::int(256))],
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
            &UBig::from_words(&LIVE_LIMBS),
            "GC'd + remapped const id must still resolve to its value",
        );
    }

    /// A surviving `memory_offsets` entry must be remapped on BOTH coordinates:
    /// its key and its interned base.  The zombie allocated ahead of the live
    /// nodes forces a non-trivial id shift.
    #[test]
    fn compact_remaps_surviving_stack_offset_entry() {
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let zombie = int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::int(64));
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let base = int_const_node(&mut f, 0x7000_u128, crate::node::ValueType::int(64));
        let [base_value] = f.node_outputs_exact::<1>(base).unwrap();
        let key = int_const_node(&mut f, 0x8000_u128, crate::node::ValueType::int(64));
        let [key_value] = f.node_outputs_exact::<1>(key).unwrap();
        // Feed both into the Return so the walk keeps them.
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, base_value, key_value],
            [],
        );
        f.set_stack_slot(key_value, base_value, -16);

        let remap = f.compact().expect("compact must succeed");

        assert!(
            remap.node_old_to_new(zombie).is_none(),
            "zombie must be dropped"
        );
        let new_key_value = remap
            .value_old_to_new(key_value)
            .expect("key value survives");
        let new_base_value = remap
            .value_old_to_new(base_value)
            .expect("base value survives");
        assert_ne!(
            new_key_value, key_value,
            "the zombie ahead of it must shift the value ids"
        );
        assert_eq!(
            f.memory_slot_resolved(new_key_value),
            Some((new_base_value, -16)),
            "surviving memory_offsets entry must be remapped on key AND base"
        );
    }

    /// Compaction keeps each slot's region: a heap slot stays a heap slot and
    /// a stack slot a stack slot, both rebased.
    #[test]
    fn compact_remaps_surviving_heap_slot() {
        use crate::MemDecomp;
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::int(64));
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let value = |f: &mut Function, k: u128| {
            let node = int_const_node(f, k, crate::node::ValueType::int(64));
            f.node_outputs(node)[0]
        };
        let base = value(&mut f, 0x7000);
        let heap_key = value(&mut f, 0x8000);
        let stack_key = value(&mut f, 0x9000);
        f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, base, heap_key, stack_key],
            [],
        );
        f.set_heap_slot(heap_key, base, 24);
        f.set_stack_slot(stack_key, base, -8);

        let remap = f.compact().expect("compact must succeed");

        let new = |v| remap.value_old_to_new(v).expect("survives");
        let (heap_class, heap_slot) = f.memory_decomp(new(heap_key));
        assert!(matches!(heap_class, MemDecomp::Heap(_)), "{heap_class:?}");
        assert_eq!(heap_slot, Some((new(base), 24)));
        let (stack_class, stack_slot) = f.memory_decomp(new(stack_key));
        assert!(
            matches!(stack_class, MemDecomp::Stack(_)),
            "{stack_class:?}"
        );
        assert_eq!(stack_slot, Some((new(base), -8)));
    }

    /// A node remap must carry the fingerprint through to the new NodeId.
    #[test]
    fn compact_preserves_asm_fingerprint_on_surviving_node() {
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        // Kept live by its Return-input consumer.
        let surviving = int_const_node(
            &mut f,
            (0xCAFE_u64) as u128,
            crate::node::ValueType::int(64),
        );
        let [surv_value] = f.node_outputs_exact::<1>(surviving).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, surv_value], []);

        f.side_tables_mut()
            .extend_asm_fingerprint(surviving, &[0x1000, 0x1004, 0x1008]);

        let remap = f.compact().expect("compact must succeed");
        let new_id = remap
            .node_old_to_new(surviving)
            .expect("surviving IntConst must remain after compact");
        assert_eq!(
            f.side_tables().asm_fingerprint(new_id),
            rustc_hash::FxHashSet::from_iter([0x1000, 0x1004, 0x1008]),
            "surviving node's asm-fingerprint must transfer to its post-compact NodeId"
        );
    }

    /// Guards against compaction skipping detached-but-still-arena-present
    /// nodes.
    #[test]
    fn compact_drops_zombie_node() {
        use crate::graph::NodeIdRemap;

        let mut f = test_function();
        // Entry + InitialMemory + a Return: the minimal reachable graph.
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);

        // A cacheable IntConst wired to nothing reachable.
        let zombie = int_const_node(
            &mut f,
            (0xC0FFEE_u64) as u128,
            crate::node::ValueType::int(64),
        );

        let pre_ids: Vec<_> = f.graph().all_node_ids().collect();
        assert!(
            pre_ids.contains(&zombie),
            "zombie must be present before compact"
        );

        let _remap: NodeIdRemap = f.compact().expect("compact must succeed");

        // The zombie NodeId is invalid post-compact, so probe the remap instead.
        assert!(
            _remap.node_old_to_new(zombie).is_none(),
            "zombie must be dropped by compact"
        );
        assert!(
            f.graph().all_node_ids().count() < pre_ids.len(),
            "compact must remove unreachable nodes"
        );
    }

    /// `value_vn` and `memory_offsets` must hold no entries pointing at dropped
    /// nodes after compaction.
    #[test]
    fn compact_drops_side_table_entry_for_dropped_node() {
        use crate::node::ValueType;

        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x88,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let mut f = test_function_tracking(vec![dead_vn]);
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value], []);

        // A zombie Phi carrying a value_vn entry.
        let zombie_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::int(64))]);
        let zombie_phi_value = f.node_outputs(zombie_phi)[0];
        f.set_vn_for_value(zombie_phi_value, dead_vn);
        assert_eq!(
            f.get_vn_for_value(zombie_phi_value),
            Some(dead_vn),
            "tag must be set before compact"
        );

        // A zombie IntConst carrying a memory_offsets entry, keyed by its value.
        let zombie_stack = int_const_node(
            &mut f,
            (0xBEEF_u64) as u128,
            crate::node::ValueType::int(64),
        );
        let zombie_value = f.node_outputs(zombie_stack).iter().copied().next().unwrap();
        f.set_stack_slot(zombie_value, zombie_value, -8);
        assert_eq!(
            f.memory_slot_resolved(zombie_value),
            Some((zombie_value, -8)),
            "offset must be set before compact"
        );

        let remap = f.compact().expect("compact must succeed");

        assert!(remap.node_old_to_new(zombie_phi).is_none());
        assert!(remap.node_old_to_new(zombie_stack).is_none());

        // Dropped ids can't be probed directly, so verify indirectly: no
        // surviving value carries the tag or the offset.
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
        let surviving_with_offset = f.graph().all_node_ids().any(|n| {
            f.node_outputs(n)
                .first()
                .copied()
                .and_then(|v| f.memory_slot_resolved(v))
                .map(|(_, o)| o)
                == Some(-8)
        });
        assert!(
            !surviving_with_offset,
            "stack_offset -8 must not survive compaction on a surviving value"
        );
    }

    /// A surviving `arg_index_to_values` carrier value must be translated.
    #[test]
    fn compact_remaps_arg_index_to_values() {
        use crate::node::ValueType;

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Created BEFORE the arg carrier so compaction reassigns the carrier's
        // NodeId.
        let _zombie = int_const_node(
            &mut f,
            (0xDEAD_u64) as u128,
            crate::node::ValueType::int(64),
        );
        // A register-arg-style InitialVar kept live by Return.
        let arg_node = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [arg_value] = f.node_outputs_exact::<1>(arg_node).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, arg_value], []);
        f.side_tables_mut().register_arg_value(0, arg_value);

        let remap = f.compact().expect("compact must succeed");
        let new_arg_value = remap
            .value_old_to_new(arg_value)
            .expect("the live arg carrier value must survive compaction");

        assert_eq!(
            f.side_tables().arg_index_to_values(0),
            &[new_arg_value],
            "arg_index_to_values must carry the carrier's post-compaction value"
        );
        for &v in f.side_tables().arg_index_to_values(0) {
            let node = f.graph().producer(v);
            assert!(
                f.graph().all_node_ids().any(|n| n == node),
                "arg carrier producer {node:?} must be a live post-compaction node"
            );
        }
    }

    #[test]
    fn compact_keeps_reachable_phi_tag_drops_unreachable() {
        use crate::node::ValueType;

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
        let mut f = test_function_tracking(vec![live_vn, dead_vn]);
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Kept live by Return.
        let live_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::int(64))]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [live_phi_value] = f.node_outputs_exact::<1>(live_phi).unwrap();
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, live_phi_value],
            [],
        );

        // Wired to nothing reachable.
        let dead_phi =
            f.graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::int(64))]);

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
        let _ = new_live_phi;
        assert!(
            remap.node_old_to_new(dead_phi).is_none(),
            "unreachable phi must be dropped"
        );
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

    #[test]
    fn compact_drops_pruned_arg_value_keeps_surviving() {
        use crate::node::ValueType;

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let live_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let [live_value] = f.node_outputs_exact::<1>(live_carrier).unwrap();
        let _ret =
            f.graph_mut()
                .create_node(NodeKind::Return, [entry_ctrl, mem_value, live_value], []);

        // An unreachable carrier on a different arg index.
        let dead_carrier = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(1)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [dead_value] = f.node_outputs_exact::<1>(dead_carrier).unwrap();

        f.side_tables_mut().register_arg_value(0, live_value);
        f.side_tables_mut().register_arg_value(1, dead_value);

        f.compact().expect("compact must succeed");

        // arg 1's only value was pruned, so the index goes entirely.
        assert!(
            f.side_tables().arg_index_to_values(1).is_empty(),
            "pruned arg value must be dropped"
        );
        // arg 0 survives, so producer recovers the live carrier.
        let surviving = f.side_tables().arg_index_to_values(0);
        assert_eq!(surviving.len(), 1);
        let node = f.graph().producer(surviving[0]);
        assert!(matches!(f.node_kind(node), NodeKind::InitialVar(_)));
    }

    /// A carrier pruned from the middle of an index's list leaves the survivors
    /// in their registration order.
    #[test]
    fn compact_keeps_surviving_carrier_order() {
        use crate::node::{InitialVnId, ValueType};

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let carrier = |f: &mut Function, id: usize| {
            let node = f.graph_mut().create_node(
                NodeKind::InitialVar(InitialVnId::from_index(id)),
                [],
                [ValueKind::Typed(ValueType::int(64))],
            );
            f.node_outputs(node)[0]
        };
        let first = carrier(&mut f, 0);
        let dropped = carrier(&mut f, 1);
        let last = carrier(&mut f, 2);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        f.graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_value, last, first], []);
        for value in [first, dropped, last] {
            f.side_tables_mut().register_arg_value(0, value);
        }

        f.compact().expect("compact must succeed");

        let ids: Vec<_> = f
            .side_tables()
            .arg_index_to_values(0)
            .iter()
            .map(|&v| *f.node_kind(f.graph().producer(v)))
            .collect();
        assert_eq!(
            ids,
            [0, 2].map(|i| NodeKind::InitialVar(InitialVnId::from_index(i)))
        );
    }

    /// Compaction drops a dead call convention and renumbers the live one in
    /// place, exactly as it does for the constants and the switch tables.
    #[test]
    fn compact_gcs_unreferenced_call_cc() {
        use crate::node::CcId;

        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs();
        let live = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .unwrap();

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let call = f.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [entry_ctrl, mem_value],
            [ValueKind::Control, ValueKind::Memory],
        );
        // Interned first, then replaced, so id 0 is referenced by nothing.
        f.set_call_cc(call, strider_target::BuiltCallingConvention::default());
        f.set_call_cc(call, live.clone());
        assert_eq!(
            *f.node_kind(call),
            NodeKind::Call {
                cc: Some(CcId::from_u32(1))
            }
        );
        let [call_ctrl, call_mem] = f.node_outputs_exact::<2>(call).unwrap();
        f.graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, call_mem], []);

        let remap = f.compact().expect("compact must succeed");
        let new_call = remap.node_old_to_new(call).expect("the Call survives");

        assert_eq!(
            *f.node_kind(new_call),
            NodeKind::Call {
                cc: Some(CcId::from_u32(0))
            },
            "the live convention must renumber into the dead one's slot"
        );
        assert_eq!(f.get_cc(new_call).stack_args, live.stack_args);
        assert_eq!(f.cc(CcId::from_u32(1)), None, "the dead one must be gone");
    }

    #[test]
    fn clobber_output_value_maps_to_vn_via_value_vn() {
        use crate::node::ValueType;

        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let mut f = test_function_tracking(vec![vn]);
        // Outputs are [Control, Memory, clobber].
        let call = f.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::int(64)),
            ],
        );
        let clobber_value = f.node_outputs(call)[2];
        assert_eq!(f.get_vn_for_value(clobber_value), None);
        f.set_vn_for_value(clobber_value, vn);
        assert_eq!(f.get_vn_for_value(clobber_value), Some(vn));
        // Control / Memory outputs carry no clobber tag.
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[0]), None);
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[1]), None);
    }

    /// Compact must keep a `Call`'s override convention and remap the
    /// per-output clobber `value_vn` (ValueId-keyed).
    #[test]
    fn compact_remaps_call_cc_and_clobber_value_vn() {
        use crate::node::ValueType;

        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs();
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .unwrap();

        let clob_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let mut f = test_function_tracking(vec![clob_vn]);
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Created before the Call so compaction reassigns ids.
        let _zombie = int_const_node(
            &mut f,
            (0xDEAD_u64) as u128,
            crate::node::ValueType::int(64),
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let target = int_const_node(&mut f, 0x1000_u128, crate::node::ValueType::int(64));
        let [target_value] = f.node_outputs_exact::<1>(target).unwrap();
        // One clobber output; kept live by the Return consuming its ctrl/mem.
        let call = f.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [entry_ctrl, mem_value, target_value],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::int(64)),
            ],
        );
        let [call_ctrl, call_mem, clob] = f.node_outputs_exact::<3>(call).unwrap();
        f.set_vn_for_value(clob, clob_vn);
        f.set_call_cc(call, cc.clone());
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, call_mem], []);

        // The override differs from the trivial default, so get_cc returns it.
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

        assert_ne!(f.get_cc(new_call), f.default_cc());
        assert_eq!(f.get_cc(new_call).stack_args, cc.stack_args,);
        assert_eq!(f.get_vn_for_value(new_clob), Some(clob_vn));
    }
}
