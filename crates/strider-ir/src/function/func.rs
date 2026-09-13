use rustc_hash::FxHashMap;

use crate::IRViewer;
use crate::IRWalker;
use crate::function::side_tables::SideTables;
use crate::graph::{Graph, NodeIdRemap};
use crate::node::const_value::ConstId;
use crate::node::{CcId, NodeId, NodeKind, SwitchTableId, ValueId};

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
    /// Case addresses of each `Switch(id)`, one per control output.
    switch_tables: cranelift_entity::PrimaryMap<SwitchTableId, Vec<u64>>,
    /// Override conventions referenced by `Call { cc: Some(id) }`, deduped by
    /// value.
    call_ccs: entity_utils::EntityInterner<CcId, strider_target::BuiltCallingConvention>,
    /// Sleigh user-op name by `CallOther` `user_op_id`.
    call_other_names: FxHashMap<u64, String>,
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
            switch_tables: cranelift_entity::PrimaryMap::new(),
            call_ccs: entity_utils::EntityInterner::default(),
            call_other_names: FxHashMap::default(),
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
    ///
    /// `ty` must be an integer type; a float one masks to zero, so every value
    /// interns as `0`. Not asserted: the resulting `IntConst` carries a float
    /// output type, which [`crate::validate`] rejects, and a caller reaching
    /// here from a user-supplied rewrite needs that as a catchable error
    /// rather than a panic. [`crate::IRBuilderExt::build_int_const`] checks up
    /// front and returns `Err`.
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
    ///
    /// Trimmed to the limbs `ty` actually spans first: dedup is by stored
    /// representation, so an over-long or over-wide spelling of a value would
    /// otherwise intern as a second, unequal constant.
    ///
    /// A non-integer `ty` masks to zero, exactly as in
    /// [`intern_int_const`](Self::intern_int_const).
    pub fn intern_int_const_limbs(
        &mut self,
        limbs: &[u64],
        ty: crate::node::ValueType,
    ) -> crate::node::const_value::ConstId {
        let limb_count = ty.byte_size().div_ceil(8);
        let mut trimmed: Vec<u64> = limbs.iter().copied().take(limb_count).collect();
        trimmed.resize(limb_count, 0);
        if let Some(last) = trimmed.last_mut() {
            // Bits above the declared width are not part of the value.
            let spare = limb_count * 64 - ty.bit_width();
            *last &= u64::MAX >> spare;
        }
        let cv = crate::node::const_value::ConstValue::Wide(trimmed.into_boxed_slice());
        match cv.fits_u128() {
            Some(v) => self.intern_int_const(v, ty),
            None => self.const_interner.intern(cv),
        }
    }

    /// Panics on an OUT-OF-RANGE id. An id minted by another function's
    /// interner that happens to be in range returns this one's value for that
    /// index instead, which `validate`'s `DanglingConstId` also cannot see.
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

    /// The effective convention for `node_id`: a `Call`'s override if it
    /// carries one, else the function default.
    ///
    /// # Panics
    ///
    /// On a `Call` whose [`CcId`] this function never minted, which `validate`
    /// reports as `DanglingCcId`.
    #[inline]
    pub fn get_cc(&self, node_id: NodeId) -> &strider_target::BuiltCallingConvention {
        match *self.node_kind(node_id) {
            NodeKind::Call { cc: Some(id) } => &self.call_ccs[id],
            _ => &self.default_cc,
        }
    }

    /// `None` for an id this function never minted.
    #[inline]
    pub fn cc(&self, id: CcId) -> Option<&strider_target::BuiltCallingConvention> {
        self.call_ccs.get(id)
    }

    /// Replaces any prior override on `call`.
    ///
    /// # Panics
    ///
    /// If `call` is not a `Call`.
    pub fn set_call_cc(&mut self, call: NodeId, cc: strider_target::BuiltCallingConvention) {
        let id = self.call_ccs.intern(cc);
        match self.graph.node_kind_mut(call) {
            NodeKind::Call { cc } => *cc = Some(id),
            other => panic!("set_call_cc on {call:?}, a {other:?}"),
        }
    }

    /// The case addresses of a `Switch`, empty for any other node or an id
    /// this function never minted.
    #[inline]
    pub fn switch_targets(&self, node_id: NodeId) -> &[u64] {
        match *self.node_kind(node_id) {
            NodeKind::Switch(id) => self.switch_table(id).unwrap_or(&[]),
            _ => &[],
        }
    }

    /// `None` for an id this function never minted.
    #[inline]
    pub fn switch_table(&self, id: SwitchTableId) -> Option<&[u64]> {
        self.switch_tables.get(id).map(Vec::as_slice)
    }

    /// A fresh table for a `Switch` about to be created.
    pub fn add_switch_table(&mut self, targets: Vec<u64>) -> SwitchTableId {
        self.switch_tables.push(targets)
    }

    /// The Sleigh name of a `CallOther`'s user-op, `None` for any other node
    /// or an unnamed op.
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        match *self.node_kind(node_id) {
            NodeKind::CallOther { user_op_id } => {
                self.call_other_names.get(&user_op_id).map(String::as_str)
            }
            _ => None,
        }
    }

    /// Names every `CallOther { user_op_id }`, present and future.
    pub fn set_call_other_name(&mut self, user_op_id: u64, name: impl Into<String>) {
        self.call_other_names.insert(user_op_id, name.into());
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
    /// rewrite it through the returned [`NodeIdRemap`].
    ///
    /// Leaves the side-tables AND the dedup cache stale, so only
    /// [`Self::compact`], which settles both, may call it.
    fn retain_reachable(&mut self) -> NodeIdRemap {
        // Collect into a `Vec` first to end the immutable borrow before
        // `graph_mut()`.
        let reachable: Vec<NodeId> = self.walk().collect();
        self.graph_mut().retain_reachable_stale_cache(reachable)
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
        // Settled before the entry check, not after: `retain_reachable` leaves
        // the cache keyed on PRE-compaction ids, and a `?` out of here would
        // hand a caller a `Function` whose next deduping create probes the new
        // arena with an old `NodeId`.
        let new_entry = remap.node_old_to_new(entry);
        self.side_tables.remap(&remap);
        // The dedup cache keys on `NodeKind`, which carries the `ConstId`, so
        // the id rewrite MUST precede the cache rebuild.
        self.gc_payload_tables();
        self.graph.rebuild_cache();
        self.entry = new_entry.ok_or_else(|| {
            anyhow::anyhow!(
                "Function::compact: entry {entry:?} missing from remap (invariant violation)"
            )
        })?;
        Ok(remap)
    }

    /// Rebuilds [`Self::const_interner`], the switch tables and the call
    /// conventions over only the entries surviving nodes reference, rewriting
    /// each node's id in place.
    ///
    /// Only safe after [`Graph::retain_reachable_stale_cache`] has settled the arena.
    fn gc_payload_tables(&mut self) {
        let mut consts: entity_utils::EntityInterner<
            ConstId,
            crate::node::const_value::ConstValue,
        > = entity_utils::EntityInterner::default();
        let mut const_ids: FxHashMap<ConstId, ConstId> = FxHashMap::default();
        let mut tables: cranelift_entity::PrimaryMap<SwitchTableId, Vec<u64>> =
            cranelift_entity::PrimaryMap::new();
        let mut table_ids: cranelift_entity::SecondaryMap<SwitchTableId, Option<SwitchTableId>> =
            cranelift_entity::SecondaryMap::new();
        let mut ccs: entity_utils::EntityInterner<CcId, strider_target::BuiltCallingConvention> =
            entity_utils::EntityInterner::default();
        let mut cc_ids: cranelift_entity::SecondaryMap<CcId, Option<CcId>> =
            cranelift_entity::SecondaryMap::new();
        let nodes: Vec<NodeId> = self.graph.all_node_ids().collect();
        for node in nodes {
            match self.graph.node_kind_mut(node) {
                NodeKind::IntConst(id) => {
                    let old = *id;
                    *id = *const_ids
                        .entry(old)
                        .or_insert_with(|| consts.intern(self.const_interner[old].clone()));
                }
                NodeKind::Switch(id) => {
                    let old = *id;
                    *id = *table_ids[old]
                        .get_or_insert_with(|| tables.push(self.switch_tables[old].clone()));
                }
                NodeKind::Call { cc: Some(id) } => {
                    let old = *id;
                    *id =
                        *cc_ids[old].get_or_insert_with(|| ccs.intern(self.call_ccs[old].clone()));
                }
                _ => {}
            }
        }
        self.const_interner = consts;
        self.switch_tables = tables;
        self.call_ccs = ccs;
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
            regs: std::borrow::Cow::Owned(sleigh.regs()?),
            node_to_arg_indices,
            nodes: None,
            center: None,
            errors: FxHashMap::default(),
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

    /// `retain_reachable` leaves the dedup cache keyed on PRE-compaction ids,
    /// so `compact` has to rebuild it on every exit.  An `IntBinaryOp` shows it
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
            let _zombie = int_const_node(&mut f, 0xdead_0000 + v, ValueType::I64);
        }
        let a_node = int_const_node(&mut f, 7, ValueType::I64);
        let b_node = int_const_node(&mut f, 9, ValueType::I64);
        let a = f.node_outputs(a_node)[0];
        let b = f.node_outputs(b_node)[0];
        let sum = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a, b],
            [ValueKind::Typed(ValueType::I64)],
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
            [ValueKind::Typed(ValueType::I64)],
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
        // Interned first, so dropping it shifts the surviving const's id.
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
            NodeKind::Call { cc: None },
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

    /// Compact must keep a `Call`'s override convention and remap the
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
            NodeKind::Call { cc: None },
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
