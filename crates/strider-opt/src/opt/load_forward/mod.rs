//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load when the nearest may-aliasing
//! memory def is a store at the same address whose bytes cover the load (a
//! wider store is sliced with `ShiftRight` / `Truncate`).  Anything else
//! blocks, including a `MemPhi` whose arms reach different defs; no value
//! `Phi` is synthesized.  Which calls a load passes is
//! [`crate::mem_analysis::Policy`]'s.

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer};

use crate::Result;
use crate::const_eval::LoadSlice;
use crate::mem_analysis::{MemQuery, Policy, ReachingDef};

pub struct LoadForward;

// Not `PROPAGATE`: a forward reaches a consumer only through an address
// edge, which the next pipeline iteration picks up.  One query per sweep: the
// pass changes only `Load` edges and outputs, so every memory def stands.
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

/// Narrows `load`'s memory edge, then forwards an exact-match store's value.
fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    query: &MemQuery,
) -> Result<()> {
    let (load_value, load_ty) = edit.single_value_output(load)?;

    let reaching = query.reach_and_narrow(edit, load, Policy::forward(edit.function()));

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

    // A store address redirected onto a new value has a class no memoised
    // answer has seen.
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
    // The store and the load's address are the forward's proof.
    let forwarded_node = edit.producer(forwarded);
    edit.absorb_node_fingerprint(forwarded_node, clobber_node);
    let addr = edit.load_addr(load);
    edit.absorb_fingerprint(forwarded, addr);
    edit.replace_value(load_value, forwarded);
    Ok(())
}

/// A wider store's value shifted right `shift` bits and truncated to the
/// load.
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
        // `build_int_const` stamps no fingerprint.
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
