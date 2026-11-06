use cranelift_entity::{PrimaryMap, SecondaryMap};
use dashu_int::UBig;
use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::IRViewer;
use crate::graph::{Graph, NodeIdRemap};
use crate::node::IntConstId;
use crate::node::{CcId, NodeId, NodeKind, SwitchTableId, ValueId};

/// Up to two limbs (128 bits) are inline, so a small constant allocates nothing.
const _: () = assert!(size_of::<UBig>() == 24);

/// The ABI class a function argument was passed in. Stack arguments continue
/// the `Integer` index space after the register ones; indices are positional
/// within a class, so an unregistered carrier leaves a gap.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ArgClass {
    Integer,
    Float,
}

/// Per-function tables keyed by node, value or payload id. [`Function::compact`]
/// remaps or renumbers them; invalidation keeps them.
///
/// [`Function`]: crate::Function
/// [`Function::compact`]: crate::Function::compact
#[derive(Clone)]
pub struct FunctionSideTables {
    /// Instruction addresses recorded on each node: its own, each dedup hit's,
    /// and every node absorbed into it (a live alias). A superset of the
    /// contributors: a shared constant carries every address that used it.
    asm_fingerprints: UnionDag<NodeId, u64>,
    /// The tracked varnode a value represents, at most one per value.
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
    /// Per-class CC argument index to the carrier nodes' output values
    /// (`InitialVar` for register args, `Load` for stack args).
    arg_index_to_values: FxHashMap<(ArgClass, u32), Vec<ValueId>>,
    /// The `InitialVar(id)` node of each id; `validate` re-checks reachable
    /// entries.
    pub(crate) initial_var_index: SecondaryMap<crate::node::InitialVnId, Option<NodeId>>,
    /// Unsigned values of `IntConst(id)` nodes, deduped after
    /// `crate::eval::mask` to the node's output width.
    pub(crate) const_interner: EntityInterner<IntConstId, UBig>,
    /// Case addresses of each `Switch(id)`, one per control output.
    switch_tables: PrimaryMap<SwitchTableId, Vec<u64>>,
    /// Override conventions of `Call { cc: Some(id) }`, deduped by value.
    call_ccs: EntityInterner<CcId, strider_target::BuiltCallingConvention>,
    /// Sleigh user-op name by `CallOther` `user_op_id`.
    call_other_names: FxHashMap<u64, String>,
    /// Every tracked varnode in `InitialVnId` order: (space shortcut, offset, size).
    vn_interner: EntityInterner<crate::node::InitialVnId, rsleigh::Vn>,
}

impl FunctionSideTables {
    pub(crate) fn new(vn_interner: EntityInterner<crate::node::InitialVnId, rsleigh::Vn>) -> Self {
        Self {
            asm_fingerprints: UnionDag::default(),
            value_vn: FxHashMap::default(),
            arg_index_to_values: FxHashMap::default(),
            initial_var_index: SecondaryMap::new(),
            const_interner: EntityInterner::default(),
            switch_tables: PrimaryMap::new(),
            call_ccs: EntityInterner::default(),
            call_other_names: FxHashMap::default(),
            vn_interner,
        }
    }

    #[inline]
    fn class_values(&self, class: ArgClass, index: u32) -> &[ValueId] {
        self.arg_index_to_values
            .get(&(class, index))
            .map_or(&[], Vec::as_slice)
    }

    #[inline]
    fn class_indices(&self, class: ArgClass) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_values
            .keys()
            .filter(move |(c, _)| *c == class)
            .map(|(_, index)| *index)
    }

    /// Carriers of the `index`-th integer-class argument.
    #[inline]
    pub fn arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.class_values(ArgClass::Integer, index)
    }

    /// Carriers of the `index`-th float-class argument.
    #[inline]
    pub fn float_arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.class_values(ArgClass::Float, index)
    }

    /// Appends `value` to the carriers recorded for integer-class `index`.
    #[inline]
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry((ArgClass::Integer, index))
            .or_default()
            .push(value);
    }

    /// Drops the integer-class carriers at `first_index` and above, keeping the
    /// register-passed ones below it.
    #[inline]
    pub fn clear_arg_values_from(&mut self, first_index: u32) {
        self.arg_index_to_values
            .retain(|(class, index), _| *class != ArgClass::Integer || *index < first_index);
    }

    /// Appends `value` to the carriers recorded for float-class `index`.
    #[inline]
    pub fn register_float_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry((ArgClass::Float, index))
            .or_default()
            .push(value);
    }

    /// Unordered.
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.class_indices(ArgClass::Integer)
    }

    /// Unordered.
    #[inline]
    pub fn iter_float_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.class_indices(ArgClass::Float)
    }

    /// Every carrier value of either class. Unordered.
    #[inline]
    pub fn arg_carrier_values(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.arg_index_to_values.values().flatten().copied()
    }

    /// Unordered.
    pub fn asm_fingerprint(&self, id: NodeId) -> FxHashSet<u64> {
        let mut set = FxHashSet::default();
        self.asm_fingerprints.for_each(id, |addr| {
            set.insert(addr);
        });
        set
    }

    #[inline]
    pub fn asm_fingerprint_is_empty(&self, id: NodeId) -> bool {
        self.asm_fingerprints.is_empty(id)
    }

    /// Unions `contributors` in. Existing entries are never removed.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        for &addr in contributors {
            self.asm_fingerprints.extend(node_id, addr);
        }
    }

    /// Links `src`'s fingerprint under `dst`'s, a live alias once `src`'s is
    /// non-empty (see [`UnionDag::union`]).
    pub(crate) fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Rebuilds the constant interner, the switch tables and the call
    /// conventions over the entries surviving nodes reference, rewriting each
    /// node's id in place. Call only after `Graph::retain_reachable_stale_cache`.
    pub(crate) fn gc_payload_tables(&mut self, graph: &mut Graph) {
        // Exhaustive, so a new field must state whether it is collected.
        let Self {
            // Translated by `Self::remap`.
            asm_fingerprints: _,
            value_vn: _,
            arg_index_to_values: _,
            initial_var_index: _,
            const_interner,
            switch_tables,
            call_ccs,
            // Keyed by `user_op_id`, which names every `CallOther`, live or not.
            call_other_names: _,
            // The function's own tracked varnodes, not a node's.
            vn_interner: _,
        } = self;
        let mut consts: EntityInterner<IntConstId, UBig> = EntityInterner::default();
        let mut const_ids: SecondaryMap<IntConstId, Option<IntConstId>> = SecondaryMap::new();
        let mut tables: PrimaryMap<SwitchTableId, Vec<u64>> = PrimaryMap::new();
        let mut table_ids: SecondaryMap<SwitchTableId, Option<SwitchTableId>> = SecondaryMap::new();
        let mut ccs: EntityInterner<CcId, strider_target::BuiltCallingConvention> =
            EntityInterner::default();
        let mut cc_ids: SecondaryMap<CcId, Option<CcId>> = SecondaryMap::new();
        let nodes: Vec<NodeId> = graph.all_node_ids().collect();
        for node in nodes {
            match graph.node_kind_mut(node) {
                NodeKind::IntConst(id) => {
                    let old = *id;
                    *id = *const_ids[old]
                        .get_or_insert_with(|| consts.intern(const_interner[old].clone()));
                }
                NodeKind::Switch(id) => {
                    let old = *id;
                    *id = *table_ids[old]
                        .get_or_insert_with(|| tables.push(switch_tables[old].clone()));
                }
                NodeKind::Call { cc: Some(id) } => {
                    let old = *id;
                    *id = *cc_ids[old].get_or_insert_with(|| ccs.intern(call_ccs[old].clone()));
                }
                NodeKind::Call { cc: None }
                | NodeKind::Entry
                | NodeKind::InitialMemory
                | NodeKind::InitialVar(_)
                | NodeKind::Region
                | NodeKind::MemPhi
                | NodeKind::Phi
                | NodeKind::If
                | NodeKind::Return
                | NodeKind::IndirectBranch
                | NodeKind::Unreachable
                | NodeKind::Load(_)
                | NodeKind::Store(_)
                | NodeKind::IntUnaryOp(_)
                | NodeKind::IntBinaryOp(_)
                | NodeKind::IntCmpOp(_)
                | NodeKind::Truncate
                | NodeKind::Popcount
                | NodeKind::Lzcount
                | NodeKind::Extend(_)
                | NodeKind::FloatConst(_)
                | NodeKind::FloatBinaryOp(_)
                | NodeKind::FloatUnaryOp(_)
                | NodeKind::FloatCmpOp(_)
                | NodeKind::IntToFloat
                | NodeKind::FloatToInt
                | NodeKind::FloatToFloat
                | NodeKind::IntBitsToFloat
                | NodeKind::FloatBitsToInt
                | NodeKind::CallOther { .. }
                | NodeKind::CPoolRef
                | NodeKind::New => {}
            }
        }
        *const_interner = consts;
        *switch_tables = tables;
        *call_ccs = ccs;
    }

    /// Remaps arena ids after a compaction, dropping entries that did not survive.
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // Exhaustive, so a new field must state what compaction does to it.
        let Self {
            asm_fingerprints,
            value_vn,
            arg_index_to_values,
            initial_var_index,
            // Payload ids, renumbered by `Self::gc_payload_tables`.
            const_interner: _,
            switch_tables: _,
            call_ccs: _,
            // Keyed by a `user_op_id` and an `InitialVnId`, both stable
            // across compaction.
            call_other_names: _,
            vn_interner: _,
        } = self;
        asm_fingerprints.remap(|old| remap.node_old_to_new(old));
        *value_vn = value_vn
            .drain()
            .filter_map(|(value, id)| Some((remap.value_old_to_new(value)?, id)))
            .collect();
        for node in initial_var_index.values_mut() {
            *node = node.and_then(|n| remap.node_old_to_new(n));
        }
        // An index whose carriers all vanish is dropped.
        arg_index_to_values.retain(|_, values| {
            values.retain_mut(|v| remap.value_old_to_new(*v).map(|new| *v = new).is_some());
            !values.is_empty()
        });
    }
}

impl crate::Function {
    /// Interns `value` masked to `ty`'s width.
    ///
    /// `ty` must be an integer type; a float one masks every value to `0` and
    /// [`crate::validate`] rejects the node.
    pub fn intern_int_const(
        &mut self,
        value: impl Into<UBig>,
        ty: crate::node::ValueType,
    ) -> crate::node::IntConstId {
        let v = crate::eval::mask(&value.into(), ty);
        self.side_tables_mut().const_interner.intern(v)
    }

    /// Panics on an out-of-range id. An in-range id minted by another
    /// function's interner silently returns this one's value, which
    /// `validate`'s `DanglingConstId` cannot see either.
    pub(crate) fn const_value(&self, id: crate::node::IntConstId) -> &UBig {
        &self.side_tables().const_interner[id]
    }

    /// The tracked varnodes in `InitialVnId` order.
    pub fn all_vns(&self) -> &[rsleigh::Vn] {
        self.side_tables().vn_interner.values_as_slice()
    }

    /// Panics on an out-of-range id.
    pub fn initial_vn(&self, id: crate::node::InitialVnId) -> rsleigh::Vn {
        self.side_tables().vn_interner[id]
    }

    /// Non-panicking [`Self::initial_vn`].
    pub(crate) fn initial_vn_opt(&self, id: crate::node::InitialVnId) -> Option<rsleigh::Vn> {
        self.side_tables().vn_interner.get(id).copied()
    }

    /// `None` when `vn` is not tracked.
    pub fn vn_id_of(&self, vn: &rsleigh::Vn) -> Option<crate::node::InitialVnId> {
        self.side_tables().vn_interner.key_of(vn)
    }

    /// In allocation order.
    pub(crate) fn vn_ids(&self) -> impl Iterator<Item = crate::node::InitialVnId> + '_ {
        self.side_tables().vn_interner.keys()
    }

    /// The tracked varnode `value` represents, if any.
    #[inline]
    pub fn get_vn_for_value(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.side_tables()
            .value_vn
            .get(&value)
            .map(|&id| self.initial_vn(id))
    }

    /// A no-op when `vn` is untracked.
    #[inline]
    pub(crate) fn set_vn_for_value(&mut self, value: ValueId, vn: rsleigh::Vn) {
        if let Some(vn_id) = self.vn_id_of(&vn) {
            self.side_tables_mut().value_vn.insert(value, vn_id);
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
        let NodeKind::Call { cc: Some(id) } = *self.node_kind(node_id) else {
            return self.default_cc();
        };
        &self.side_tables().call_ccs[id]
    }

    /// `None` for an id this function never minted.
    #[inline]
    pub fn cc(&self, id: CcId) -> Option<&strider_target::BuiltCallingConvention> {
        self.side_tables().call_ccs.get(id)
    }

    /// Replaces any prior override on `call`.
    ///
    /// # Panics
    ///
    /// If `call` is not a `Call`.
    pub fn set_call_cc(&mut self, call: NodeId, cc: strider_target::BuiltCallingConvention) {
        let id = self.side_tables_mut().call_ccs.intern(cc);
        let kind = self.graph_mut().node_kind_mut(call);
        let NodeKind::Call { cc } = kind else {
            panic!("set_call_cc on {call:?}, a {kind:?}");
        };
        *cc = Some(id);
    }

    /// The case addresses of a `Switch`, empty for any other node or an id
    /// this function never minted.
    #[inline]
    pub fn switch_targets(&self, node_id: NodeId) -> &[u64] {
        let NodeKind::Switch(id) = *self.node_kind(node_id) else {
            return &[];
        };
        self.switch_table(id).unwrap_or(&[])
    }

    /// `None` for an id this function never minted.
    #[inline]
    pub(crate) fn switch_table(&self, id: SwitchTableId) -> Option<&[u64]> {
        self.side_tables().switch_tables.get(id).map(Vec::as_slice)
    }

    /// A fresh table for a `Switch` about to be created.
    pub fn add_switch_table(&mut self, targets: Vec<u64>) -> SwitchTableId {
        self.side_tables_mut().switch_tables.push(targets)
    }

    /// The name of a `CallOther`'s user-op, Sleigh's or `read_volatile`;
    /// `None` for any other node or an unnamed op.
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        let NodeKind::CallOther { user_op_id } = *self.node_kind(node_id) else {
            return None;
        };
        self.side_tables()
            .call_other_names
            .get(&user_op_id)
            .map(String::as_str)
    }

    /// Names every `CallOther { user_op_id }`, present and future.
    pub fn set_call_other_name(&mut self, user_op_id: u64, name: impl Into<String>) {
        self.side_tables_mut()
            .call_other_names
            .insert(user_op_id, name.into());
    }

    /// The output of `vn`'s `InitialVar` node; `None` when `vn` is untracked.
    pub fn initial_var_value(&self, vn: &rsleigh::Vn) -> Option<ValueId> {
        let id = self.vn_id_of(vn)?;
        let node = self.side_tables().initial_var_index[id]?;
        self.graph().node_outputs(node).first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::test_function;
    use crate::node::ValueType;
    use cranelift_entity::EntityRef;

    // The union is order-independent, deduplicating and never dropping an entry.
    #[test]
    fn extend_asm_fingerprint_unions_deduped() {
        let mut st = FunctionSideTables::new(EntityInterner::default());
        let n = NodeId::new(3);

        // Seed from empty; contributors may arrive unsorted.
        st.extend_asm_fingerprint(n, &[40, 10, 30, 20, 50]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([10, 20, 30, 40, 50])
        );

        // Merge into a non-empty fp: unsorted, one duplicate (20), rest new.
        st.extend_asm_fingerprint(n, &[35, 20, 5, 45]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );

        // Idempotent: merging a subset changes nothing.
        st.extend_asm_fingerprint(n, &[10, 50]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );

        // Empty contributors is a no-op.
        st.extend_asm_fingerprint(n, &[]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );
    }

    // Interning: one value, one `IntConstId`, masked to the declared
    // width whatever spelling reached it.

    /// A user-supplied rewrite can reach `intern_int_const` with a float type;
    /// it must not panic (a Python `PanicException` escapes `except
    /// Exception`), and `validate` reports the mistake.
    #[test]
    fn intern_int_const_on_a_float_type_does_not_panic() {
        let mut f = test_function();
        let id = f.intern_int_const(0u64, ValueType::float(64));
        assert_eq!(*f.const_value(id), UBig::ZERO);
    }

    /// No unmasked value may slip in via a `from_words` spelling.
    #[test]
    fn interning_from_words_masks_to_width() {
        let mut f = test_function();
        // Bit 69 is inside I80's width, bit 84 is above it; only 69 survives.
        let hi: u64 = (1 << 5) | (1 << 20);
        let value = u128::from(hi) << 64;
        let limbed = f.intern_int_const(UBig::from_words(&[0, hi]), ValueType::int(80));
        let scalar = f.intern_int_const(value, ValueType::int(80));
        assert_eq!(
            limbed, scalar,
            "a from_words spelling must mask to width like a scalar one"
        );
        let masked = value & ValueType::int(80).int_mask_u128().unwrap();
        assert_eq!(u128::try_from(f.const_value(limbed)), Ok(masked));
        assert_eq!(
            f.const_value(limbed),
            &UBig::from_words(&[0, 1 << 5]),
            "past the carrier the limbs carry the masked value"
        );
    }

    /// Past the carrier there is no scalar reading, and the limbs stand.
    #[test]
    fn values_past_the_u128_carrier_keep_their_limbs() {
        let mut f = test_function();
        // Bit 128 is the first bit no `u128` carries.
        let id = f.intern_int_const(UBig::from_words(&[0, 0, 1, 0]), ValueType::int(256));
        assert!(u128::try_from(f.const_value(id)).is_err());
        assert_eq!(*f.const_value(id), UBig::ONE << 128);
    }

    /// Bits above the declared width are not part of the value, on either
    /// path, so an over-wide spelling dedups with the masked one.
    #[test]
    fn interning_masks_to_the_declared_width_on_both_paths() {
        let cases: [(&str, ValueType); 4] = [
            ("i1", ValueType::I1),
            ("i64", ValueType::int(64)),
            ("i72", ValueType::int(72)),
            ("i128", ValueType::int(128)),
        ];
        for (label, ty) in cases {
            let mut f = test_function();
            let masked = ty.int_mask_u128().unwrap();
            let saturated = f.intern_int_const(u128::MAX, ty);
            let exact = f.intern_int_const(masked, ty);
            let limbed = f.intern_int_const(UBig::from_words(&[u64::MAX; 8]), ty);
            assert_eq!(saturated, exact, "{label}: the mask is applied first");
            assert_eq!(saturated, limbed, "{label}: the limb path masks too");
            assert_eq!(
                u128::try_from(f.const_value(saturated)),
                Ok(masked),
                "{label}"
            );
        }
    }
}
