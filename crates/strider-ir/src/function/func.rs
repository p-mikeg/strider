use rustc_hash::FxHashMap;

use crate::IRViewer;
use crate::IRWalker;
use crate::function::side_tables::SideTables;
use crate::graph::{Graph, NodeIdRemap};
use crate::node::const_value::ConstId;
use crate::node::{NodeId, NodeKind, ValueId};

/// Deterministic ordering key for a tracked varnode.
pub(crate) fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size)
}

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
#[derive(Clone)]
pub struct Function {
    graph: Graph,
    entry: NodeId,

    /// The convention this function was built under.
    default_cc: strider_target::BuiltCallingConvention,
    endianness: strider_target::Endianness,
    /// Every tracked varnode, value-deduped, in `(space, offset, size)` order.
    vn_interner: entity_utils::EntityInterner<crate::node::InitialVnId, rsleigh::Vn>,

    side_tables: SideTables,

    /// Every integer-constant value referenced by an `IntConst(id)` node,
    /// deduped by value.
    pub(crate) const_interner: entity_utils::EntityInterner<
        crate::node::const_value::ConstId,
        crate::node::const_value::ConstValue,
    >,
}

impl Function {
    /// Builds the `Entry` node.  `InitialMemory` is the `FunctionBuilder`'s job.
    pub fn new(
        default_cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
        tracked_vns: Vec<rsleigh::Vn>,
    ) -> Self {
        let mut graph = Graph::default();
        let entry = graph.create_node(
            crate::node::NodeKind::Entry,
            [],
            [crate::node::ValueKind::Control],
        );
        // `tracked_vns` arrives in arbitrary order; sort so `InitialVnId`
        // assignment is reproducible.
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

    #[inline]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    #[inline]
    pub fn side_tables(&self) -> &SideTables {
        &self.side_tables
    }

    #[inline]
    pub fn side_tables_mut(&mut self) -> &mut SideTables {
        &mut self.side_tables
    }

    /// Interns `value` masked to `ty`'s width.
    pub fn intern_int_const(
        &mut self,
        value: u128,
        ty: crate::node::ValueType,
    ) -> crate::node::const_value::ConstId {
        let masked = value & ty.bit_mask_u128();
        self.const_interner
            .intern(crate::node::const_value::ConstValue::Bits(masked))
    }

    /// Interns a constant given as little-endian `limbs`.
    pub fn intern_int_const_limbs(
        &mut self,
        limbs: &[u64],
        ty: crate::node::ValueType,
    ) -> crate::node::const_value::ConstId {
        let cv = crate::node::const_value::ConstValue::Wide(limbs.to_vec().into_boxed_slice());
        match cv.fits_u128() {
            Some(v) => self.intern_int_const(v, ty),
            None => self.const_interner.intern(cv),
        }
    }

    /// Panics on an id not minted by this function's interner.
    pub(crate) fn const_value(
        &self,
        id: crate::node::const_value::ConstId,
    ) -> &crate::node::const_value::ConstValue {
        &self.const_interner[id]
    }

    #[inline]
    pub fn entry(&self) -> NodeId {
        self.entry
    }

    #[inline]
    pub fn default_cc(&self) -> &strider_target::BuiltCallingConvention {
        &self.default_cc
    }

    #[inline]
    pub fn endianness(&self) -> strider_target::Endianness {
        self.endianness
    }

    /// The tracked varnodes in `InitialVnId` order.
    pub fn all_vns(&self) -> &[rsleigh::Vn] {
        self.vn_interner.values_as_slice()
    }

    /// Panics on an out-of-range id.
    pub fn initial_vn(&self, id: crate::node::InitialVnId) -> rsleigh::Vn {
        self.vn_interner[id]
    }

    /// Non-panicking [`Self::initial_vn`].
    pub(crate) fn initial_vn_opt(&self, id: crate::node::InitialVnId) -> Option<rsleigh::Vn> {
        self.vn_interner.get(id).copied()
    }

    /// `None` when `vn` is not tracked.
    pub fn vn_id_of(&self, vn: &rsleigh::Vn) -> Option<crate::node::InitialVnId> {
        self.vn_interner.key_of(vn)
    }

    /// In allocation order.
    pub fn vn_ids(&self) -> impl Iterator<Item = crate::node::InitialVnId> + '_ {
        self.vn_interner.keys()
    }

    /// Rebuilds the tracked-varnode interner so `InitialVnId(i)` resolves to
    /// `vns[i]`.
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_all_vns(&mut self, vns: Vec<rsleigh::Vn>) {
        let mut interner: entity_utils::EntityInterner<crate::node::InitialVnId, rsleigh::Vn> =
            entity_utils::EntityInterner::default();
        for vn in vns {
            interner.intern(vn);
        }
        self.vn_interner = interner;
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

    /// The stack-pointer varnode of this function's convention.
    #[inline]
    pub(crate) fn stack_vn(&self) -> rsleigh::Vn {
        self.default_cc.stack_vn
    }

    /// At most one vn per value, so the meaning of the tag follows from the
    /// producing node's kind.
    #[inline]
    pub fn get_vn_for_value(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.side_tables
            .value_vn
            .get(&value)
            .map(|&id| self.initial_vn(id))
    }

    /// A no-op when `vn` is untracked.
    #[inline]
    pub fn set_vn_for_value(&mut self, value: ValueId, vn: rsleigh::Vn) {
        if let Some(vn_id) = self.vn_id_of(&vn) {
            self.side_tables.value_vn.insert(value, vn_id);
        }
    }

    /// The effective convention for `node_id`: the per-`Call` override if one
    /// was recorded, else the function default.
    #[inline]
    pub fn get_cc(&self, node_id: NodeId) -> &strider_target::BuiltCallingConvention {
        self.side_tables
            .call_cc
            .get(&node_id)
            .unwrap_or(&self.default_cc)
    }

    /// The `(base, byte offset)` of a Store/Load's address.  `None` when `node`
    /// is not a Store/Load, or its address is not SP-rooted, or it has not been
    /// decomposed yet.  Offsets compare only when their bases match.
    pub fn stack_offset(&self, node: NodeId) -> Option<(ValueId, i128)> {
        // Address is input slot 1 of both Store (`[mem, addr, data]`) and Load
        // (`[mem, addr]`).  Read it via `.get`, not the arity-panicking
        // `store_addr`/`load_addr`: whole-graph sweeps reach malformed and dead
        // nodes.
        if !matches!(self.node_kind(node), NodeKind::Store(_) | NodeKind::Load(_)) {
            return None;
        }
        let addr = self.node_inputs(node).get(1).copied()?;
        // Stack only: a heap address resolves through the same interner but is a
        // different region, so it is not an SP offset.
        match self.side_tables().memory_decomp(addr) {
            (crate::MemDecomp::Stack(_), slot) => slot,
            _ => None,
        }
    }

    /// The region tag (`Stack` / `Heap`) of a Store/Load's decomposed address,
    /// or `Unknown` for a non-access or an address not yet decomposed.
    pub fn access_memory_class(&self, node: NodeId) -> crate::MemDecomp {
        if !matches!(self.node_kind(node), NodeKind::Store(_) | NodeKind::Load(_)) {
            return crate::MemDecomp::Unknown;
        }
        match self.node_inputs(node).get(1).copied() {
            Some(addr) => self.side_tables().memory_class(addr),
            None => crate::MemDecomp::Unknown,
        }
    }

    /// The `initial_var_index` entries with each key resolved to its varnode.
    #[inline]
    pub(crate) fn initial_var_index_entries(
        &self,
    ) -> impl Iterator<Item = (rsleigh::Vn, NodeId)> + '_ {
        self.side_tables
            .initial_var_index
            .iter()
            .map(|(&vn_id, &id)| (self.initial_vn(vn_id), id))
    }

    /// The `value_vn` entries with each tag resolved to its varnode.
    #[inline]
    pub(crate) fn value_vn_entries(&self) -> impl Iterator<Item = (ValueId, rsleigh::Vn)> + '_ {
        self.side_tables
            .value_vn
            .iter()
            .map(|(&value, &id)| (value, self.initial_vn(id)))
    }

    /// The `InitialVar(stack_vn)` node, whose output is the entry SP, or `None`
    /// when the function tracks no such node (`stack_vn` deduped into a wider
    /// container).  Does NOT filter by liveness: the returned node may already
    /// have been culled.
    pub fn initial_sp(&self) -> Option<NodeId> {
        let sp_id = self.vn_id_of(&self.default_cc.stack_vn)?;
        self.side_tables.initial_var_index.get(&sp_id).copied()
    }

    /// The entry value of `vn`'s `InitialVar` node.  `None` when `vn` is
    /// untracked.
    pub fn initial_var_value(&self, vn: &rsleigh::Vn) -> Option<ValueId> {
        let id = self.vn_id_of(vn)?;
        let node = *self.side_tables.initial_var_index.get(&id)?;
        self.graph.node_outputs(node).first().copied()
    }

    /// [`Graph::create_node`] plus a union of every contributor's
    /// asm-fingerprint into the new node.
    pub fn create_node_attributed(
        &mut self,
        kind: crate::node::NodeKind,
        inputs: impl IntoIterator<Item = crate::node::ValueId>,
        output_kinds: impl IntoIterator<Item = crate::node::ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        let node_id = self.graph.create_node(kind, inputs, output_kinds);
        for &src in contributors {
            self.side_tables_mut()
                .extend_asm_fingerprint_from(node_id, src);
        }
        node_id
    }

    /// Compacts the arena down to the nodes reachable from [`Self::entry`],
    /// returning the old-to-new translation table.  Pre-compaction `NodeId` /
    /// `ValueId` / `UseId` values are invalidated; callers holding one MUST
    /// rewrite it through the returned [`NodeIdRemap`].  Leaves the side-tables
    /// stale; use [`Self::compact`] to remap those too.
    pub(crate) fn retain_reachable(&mut self) -> NodeIdRemap {
        // Collect into a `Vec` first to end the immutable borrow before
        // `graph_mut()`.
        let reachable: Vec<NodeId> = self.walk().collect();
        self.graph_mut().retain_reachable(reachable)
    }

    /// Retains only nodes reachable from [`Self::entry`], updating the stored
    /// entry and remapping every overlay table through the same translation.
    /// Entries whose node did not survive are dropped.
    ///
    /// # Errors
    ///
    /// Errors if the remap does not include the entry (invariant violation).
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        let entry = self.entry;
        let remap = self.retain_reachable();
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "Function::compact: entry {:?} missing from remap (invariant violation)",
                entry
            )
        })?;
        self.entry = new_entry;
        self.side_tables.remap(&remap);
        // The dedup cache keys on `NodeKind`, which carries the `ConstId`, so
        // the const rewrite MUST precede the cache rebuild.
        self.gc_consts();
        self.graph.rebuild_cache();
        Ok(remap)
    }

    /// Rebuilds [`Self::const_interner`] over only the values referenced by
    /// surviving `IntConst(id)` nodes, rewriting each node's id in place.
    ///
    /// Only safe after [`Graph::retain_reachable`] has settled the arena.
    fn gc_consts(&mut self) {
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
            crate::node::const_value::ConstValue,
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
            nodes: None,
            center: None,
        })
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
        f.set_all_vns(vec![vn]); // only a tracked vn can be tagged
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
            [ValueKind::Typed(ValueType::I64)],
        );
        let value = f.node_outputs(carrier)[0];
        f.side_tables_mut().register_arg_value(0, value);

        assert_eq!(f.side_tables().arg_index_to_values(0), &[value]);
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
        // The remapped entry id still carries the Control output.
        let entry_id = f.entry();
        let outs: Vec<_> = f.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(f.value_kind(outs[0]).is_control());
    }

    /// Collecting a wide const held only by a dropped node (interned first, so
    /// id 0) forces the live one's id to shift.
    #[test]
    fn compact_gcs_and_remaps_surviving_wide_const() {
        use crate::node::ValueType;
        use crate::node::const_value::ConstValue;

        // High limb set, so it stays `Wide`.
        const LIVE_LIMBS: [u64; 4] = [
            0x1122_3344_5566_7788,
            0x99AA_BBCC_DDEE_FF00,
            0,
            0x8000_0000_0000_0000,
        ];

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
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

    /// A surviving `memory_offsets` entry must be remapped on BOTH coordinates:
    /// its key and its interned base.  The zombie allocated ahead of the live
    /// nodes forces a non-trivial id shift.
    #[test]
    fn compact_remaps_surviving_stack_offset_entry() {
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let zombie = int_const_node(&mut f, 0xdead_u128, crate::node::ValueType::I64);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let base = int_const_node(&mut f, 0x7000_u128, crate::node::ValueType::I64);
        let [base_value] = f.node_outputs_exact::<1>(base).unwrap();
        let key = int_const_node(&mut f, 0x8000_u128, crate::node::ValueType::I64);
        let [key_value] = f.node_outputs_exact::<1>(key).unwrap();
        // Feed both into the Return so the walk keeps them.
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_value, base_value, key_value],
            [],
        );
        f.side_tables_mut()
            .set_stack_slot(key_value, base_value, -16);

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
            f.side_tables().memory_slot_resolved(new_key_value),
            Some((new_base_value, -16)),
            "surviving memory_offsets entry must be remapped on key AND base"
        );
    }

    /// A node remap must carry the fingerprint through to the new NodeId.
    #[test]
    fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        // Kept live by its Return-input consumer.
        let surviving = int_const_node(&mut f, (0xCAFE_u64) as u128, crate::node::ValueType::I64);
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
    fn retain_reachable_drops_zombie_node() {
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
        let zombie = int_const_node(&mut f, (0xC0FFEE_u64) as u128, crate::node::ValueType::I64);

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

        // A zombie Phi carrying a value_vn entry.
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

        // A zombie IntConst carrying a memory_offsets entry, keyed by its value.
        let zombie_stack =
            int_const_node(&mut f, (0xBEEF_u64) as u128, crate::node::ValueType::I64);
        let zombie_value = f.node_outputs(zombie_stack).iter().copied().next().unwrap();
        f.side_tables_mut()
            .set_stack_slot(zombie_value, zombie_value, -8);
        assert_eq!(
            f.side_tables().memory_slot_resolved(zombie_value),
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
                .and_then(|v| f.side_tables().memory_slot_resolved(v))
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
        let _zombie = int_const_node(&mut f, (0xDEAD_u64) as u128, crate::node::ValueType::I64);
        // A register-arg-style InitialVar kept live by Return.
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

        let mut f = test_function();
        let entry = f.entry();
        let mem = test_initial_memory(&f);
        // Kept live by Return.
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

        // Wired to nothing reachable.
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
            [ValueKind::Typed(ValueType::I64)],
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
            [ValueKind::Typed(ValueType::I64)],
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

    #[test]
    fn clobber_output_value_maps_to_vn_via_value_vn() {
        use crate::node::ValueType;

        let mut f = test_function();
        // Outputs are [Control, Memory, clobber].
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
        assert_eq!(f.get_vn_for_value(clobber_value), Some(vn));
        // Control / Memory outputs carry no clobber tag.
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[0]), None);
        assert_eq!(f.get_vn_for_value(f.node_outputs(call)[1]), None);
    }

    /// Compact must remap both the per-Call `call_cc` (NodeId-keyed) and the
    /// per-output clobber `value_vn` (ValueId-keyed).
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
        // Created before the Call so compaction reassigns ids.
        let _zombie = int_const_node(&mut f, (0xDEAD_u64) as u128, crate::node::ValueType::I64);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let target = int_const_node(&mut f, 0x1000_u128, crate::node::ValueType::I64);
        let [target_value] = f.node_outputs_exact::<1>(target).unwrap();
        // One clobber output; kept live by the Return consuming its ctrl/mem.
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
        f.side_tables_mut().set_call_cc(call, cc.clone());
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

    #[test]
    fn switch_targets_survive_compact() {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let r = b.create_region_all().unwrap();
        b.set_entry_region_all(r).unwrap();
        b.set_region(r);
        b.build_return(None, &[]).unwrap();
        let mut f = b.build().unwrap();
        let node = f.entry(); // any live NodeId
        f.side_tables_mut()
            .set_switch_targets(node, vec![0x1000, 0x1020]);
        assert_eq!(f.side_tables().switch_targets(node), &[0x1000, 0x1020]);
        f.compact().unwrap();
        // Entry survives, so its targets must be remapped rather than dropped.
        let new_node = f.entry();
        assert_eq!(f.side_tables().switch_targets(new_node), &[0x1000, 0x1020]);
    }
}
