//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load only when the nearest
//! may-aliasing memory definition is an exact-match store: same address
//! class, base and offset, with stored bytes fully covering the load's
//! range (a wider store is reshaped via `Truncate` / `ShiftRight`).
//! Anything else blocks: a non-exact overlapping store, a `MemPhi` whose arms
//! reach different definitions, or `InitialMemory`.  A `MemPhi` whose arms
//! agree is transparent.
//!
//! A `Call` blocks unless its convention declares `preserves_memory`; a
//! `CallOther` always blocks, since only a user-op its ABI row declares
//! `clobbers_memory` reaches the chain.  A `Call` also steps through when
//! `escape_analysis`
//! proves the frame private and the slot is outside the call's
//! outgoing-argument window, and when it is a listed `noalias_allocators`
//! callee and the probed location is a PRIVATE-frame stack slot outside that
//! same window, or a different allocation.
//!
//! The pass never synthesizes a value-`Phi`, so no merge of two stored values
//! forwards.  A stack address decomposes only once `PhiCollapse` has folded the
//! region phis over SP.

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer};

use crate::Result;
use crate::const_eval::LoadSlice;
use crate::mem_analysis::{MemQuery, Policy, ReachingDef};

pub struct LoadForward;

// Not `PROPAGATE`: a fresh forward reaches a consumer only through an address
// edge, which the next pipeline iteration picks up; re-enqueueing every
// consumer to catch it costs more than the deferral.
//
// One query per sweep: the pass only rewires `Load` memory edges and
// redirects `Load` outputs, so every memory def keeps its inputs and the
// index stands; `try_forward_load` drops the answers when a redirect moves a
// store's address.
impl crate::peephole::PeepholePass for LoadForward {
    type Sweep = MemQuery;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    fn try_rewrite(
        &self,
        query: &mut MemQuery,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        try_forward_load(edit, root, query)?;
        Ok(None)
    }
}

/// Two nearest-clobber queries per load, each a climb of the memory dominator
/// tree over the query's index, one dominator at a time where the memory
/// graph is irreducible and the index carries no position order.
/// [`MemQuery::reach_and_narrow`] moves the load's edge onto the structural clobber,
/// which later sweeps start from.
fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    query: &MemQuery,
) -> Result<()> {
    let (load_value, load_ty) = edit.single_value_output(load)?;

    // Shortens the load's memory edge so future walks skip the proven-disjoint
    // run.  A moved edge is a change: the load may now be a structural twin
    // of another, and the fixed point must not exit before that merge is seen.
    let reaching = query.reach_and_narrow(edit, load, Policy::forward(edit.function()));

    // Exact match: same location, stored bytes covering the load's range.
    let ReachingDef::Match(clobber_node) = reaching else {
        return Ok(());
    };

    let store_data = edit.store_data(clobber_node);
    let store_data_ty = edit
        .value_type_opt(store_data)
        .expect("Store data input is a value");
    let NodeKind::Load(space) = *edit.node_kind(load) else {
        unreachable!("LoadForward rewrites a load");
    };
    let order = edit.function().byte_order();
    let forwarded = match crate::const_eval::load_slice(order, space, store_data_ty, load_ty) {
        Some(LoadSlice::Same) => store_data,
        Some(LoadSlice::Narrow(shift)) => {
            slice_to_load(edit, store_data, store_data_ty, load, load_ty, shift)?
        }
        None => return Ok(()),
    };

    // A redirected address slot reads its new value's class, which no
    // memoised answer has seen.  Through any other use the classes hold: a
    // value the redirect feeds keeps its own, already read, class.
    if edit
        .function()
        .graph()
        .value_uses(load_value)
        .any(|(user, slot)| {
            matches!(edit.node_kind(user), NodeKind::Store(_))
                && slot as usize == NodeKind::ACCESS_ADDR_SLOT
        })
    {
        query.forget_answers();
    }
    // The store and the load's address are the forward's proof, and the
    // automatic cull takes the Load once the redirect leaves it dead, so the
    // replacement absorbs the store and the address's producer first. The
    // nodes below that producer keep their addresses only where they survive.
    let forwarded_node = edit.producer(forwarded);
    edit.absorb_node_fingerprint(forwarded_node, clobber_node);
    let addr = edit.load_addr(load);
    edit.absorb_fingerprint(forwarded, addr);
    edit.replace_value(load_value, forwarded);
    Ok(())
}

/// Reshapes a wider store's value down to the load width: shifted right
/// `shift` bits, the slice [`crate::const_eval::load_slice`] names, then
/// truncated.
fn slice_to_load(
    edit: &mut crate::EditFunction<'_>,
    store_data: ValueId,
    store_data_ty: ValueType,
    load: NodeId,
    load_ty: ValueType,
    shift: u32,
) -> Result<ValueId> {
    let shifted = if shift == 0 {
        store_data
    } else {
        let shift_const = edit.build_int_const(u128::from(shift), store_data_ty)?;
        // `build_int_const` carries no contributor stamp, so attribute the
        // const by hand; every reachable node needs a fingerprint.
        let shift_const_node = edit.producer(shift_const);
        edit.absorb_node_fingerprint(shift_const_node, load);
        let shr = edit.create_node_attributed(
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
            [store_data, shift_const],
            [ValueKind::Typed(store_data_ty)],
            &[load],
        );
        let [value] = edit.node_outputs_exact::<1>(shr)?;
        value
    };
    let trunc = edit.create_node_attributed(
        NodeKind::Truncate,
        [shifted],
        [ValueKind::Typed(load_ty)],
        &[load],
    );
    let [value] = edit.node_outputs_exact::<1>(trunc)?;
    Ok(value)
}

#[cfg(test)]
mod tests;
