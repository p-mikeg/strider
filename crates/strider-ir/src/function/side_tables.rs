use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::NodeIdRemap;
use crate::node::{NodeId, ValueId};

/// The ABI class a function argument was passed in; stack arguments continue
/// the `Integer` index space after the register ones. Indices are positional
/// within a class, gaps preserved: an unregistered carrier leaves its index
/// empty rather than shifting the later ones down.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ArgClass {
    Integer,
    Float,
}

/// Drains an `FxHashMap` and rebuilds it through a per-entry translation,
/// keeping only entries `f` maps to `Some((new_key, new_payload))`.
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

/// Every id-keyed payload table of a [`Function`] and nothing else.  None of
/// it is part of the graph's structural identity.
///
/// What makes a field one of these is that compaction has to say something
/// about it: an arena id the remap translates, or a payload id
/// [`Function::compact`] renumbers.  Nothing clears them; the derived,
/// clearable verdicts are the memos [`Function`] holds instead.
///
/// [`Function`]: crate::Function
/// [`Function::compact`]: crate::Function::compact
#[derive(Clone)]
pub struct SideTables {
    /// Machine-instruction addresses whose lifting or rewrite contributed to
    /// each node's value.
    asm_fingerprints: UnionDag<NodeId, u64>,
    /// The tracked varnode a value represents, at most one per value.
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
    /// Per-class CC argument index to the carrier nodes' output values
    /// (`InitialVar` for register args, `Load` for stack args).
    arg_index_to_values: FxHashMap<(ArgClass, u32), Vec<ValueId>>,
    /// `InitialVnId` to `InitialVar(id)` node index. Accessors trust it; the
    /// validator re-checks reachable entries against the node's kind
    /// (`StaleInitialVarIndex`).
    pub(crate) initial_var_index: FxHashMap<crate::node::InitialVnId, NodeId>,
    /// Every integer-constant value referenced by an `IntConst(id)` node,
    /// deduped by value.
    pub(crate) const_interner:
        EntityInterner<crate::node::const_value::ConstId, crate::node::const_value::ConstValue>,
    /// Case addresses of each `Switch(id)`, one per control output.
    pub(crate) switch_tables: cranelift_entity::PrimaryMap<crate::node::SwitchTableId, Vec<u64>>,
    /// Override conventions referenced by `Call { cc: Some(id) }`, deduped by
    /// value.
    pub(crate) call_ccs: EntityInterner<crate::node::CcId, strider_target::BuiltCallingConvention>,
    /// Sleigh user-op name by `CallOther` `user_op_id`.
    pub(crate) call_other_names: FxHashMap<u64, String>,
    /// Every tracked varnode, value-deduped, in `(space, offset, size)` order.
    pub(crate) vn_interner: EntityInterner<crate::node::InitialVnId, rsleigh::Vn>,
}

impl SideTables {
    /// Crate-private so no caller can wipe the tables through
    /// [`Function::side_tables_mut`](crate::Function::side_tables_mut).
    /// `vn_interner` is the one table seeded at birth: `InitialVnId`
    /// numbering is fixed by [`Function::new`](crate::Function::new).
    pub(crate) fn new(vn_interner: EntityInterner<crate::node::InitialVnId, rsleigh::Vn>) -> Self {
        Self {
            asm_fingerprints: UnionDag::default(),
            value_vn: FxHashMap::default(),
            arg_index_to_values: FxHashMap::default(),
            initial_var_index: FxHashMap::default(),
            const_interner: EntityInterner::default(),
            switch_tables: cranelift_entity::PrimaryMap::new(),
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

    /// Unions `contributors` in.  Existing entries are never removed.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        for &addr in contributors {
            self.asm_fingerprints.extend(node_id, addr);
        }
    }

    /// Links `src`'s fingerprint under `dst`'s. A live alias, not a copy:
    /// contributors added to `src` afterwards are `dst`'s too.
    pub(crate) fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Remaps every arena-id key / value after a compaction; an entry whose
    /// node or value did not survive is dropped.
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // Every field named, so one added later states what compaction does
        // to it.
        let Self {
            asm_fingerprints,
            value_vn,
            arg_index_to_values,
            initial_var_index,
            // Payload ids, not arena ids: `Function::gc_payload_tables`
            // renumbers these against the compacted graph.
            const_interner: _,
            switch_tables: _,
            call_ccs: _,
            // Keyed by a `user_op_id` and an `InitialVnId`, both stable
            // across compaction.
            call_other_names: _,
            vn_interner: _,
        } = self;
        asm_fingerprints.remap(|old| remap.node_old_to_new(old));
        // `InitialVnId`s are stable across compaction, so only the arena-id
        // coordinate of the next two maps needs translating.
        *value_vn = remap_hashmap(value_vn, |old_value, vn_id| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn_id))
        });
        *initial_var_index = remap_hashmap(initial_var_index, |vn_id, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn_id, new_id))
        });
        // An index whose carriers all vanish is dropped.
        *arg_index_to_values = remap_hashmap(arg_index_to_values, |key, old_values| {
            let mapped: Vec<ValueId> = old_values
                .into_iter()
                .filter_map(|old_value| remap.value_old_to_new(old_value))
                .collect();
            (!mapped.is_empty()).then_some((key, mapped))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::EntityRef;

    // The union is order-independent, deduplicating and never dropping an entry.
    #[test]
    fn extend_asm_fingerprint_unions_deduped() {
        let mut st = SideTables::new(EntityInterner::default());
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
}
