use std::cell::Cell;

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{Function, IRBuilderExt, IRViewer, UBig};

use crate::Result;
use crate::const_eval::LoadSlice;
use crate::mem_analysis::{Loc, MemQuery, Policy, ReachingDef, narrow_load_to};

/// Store-to-load forwarding. Moves each load's memory edge onto its nearest
/// clobber under `Policy::strict`. A load whose nearest clobber under
/// `Policy::forward` is a store at its address covering its bytes takes the
/// stored value, a wider store sliced. A `MemPhi` whose arms agree once a
/// constant `If`'s untaken arms are dropped reads as that arm. No value
/// `Phi` is synthesized.
pub struct LoadForward;

/// One sweep's memory query and [`settle`] memo.
#[derive(Default)]
pub(crate) struct Sweep {
    query: MemQuery,
    /// A memory value to the earliest one it equals.
    settled: FxHashMap<ValueId, ValueId>,
    /// A load's memory edge and location to where the settle loop leaves it.
    ends: FxHashMap<(ValueId, Loc), ValueId>,
}

// Not `PROPAGATE`: a forward reaches a consumer only through an address
// edge, which the next pipeline iteration picks up. One query per sweep: the
// pass changes only `Load` edges and outputs, so every memory def stands.
impl crate::peephole::PeepholePass for LoadForward {
    type Sweep = Sweep;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    fn try_rewrite(
        &self,
        sweep: &mut Sweep,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        try_forward_load(edit, root, sweep)?;
        Ok(None)
    }
}

#[cfg(test)]
thread_local! {
    /// Rounds of [`try_forward_load`]'s settle loop.
    pub(crate) static SETTLE_ROUNDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Narrows `load`'s memory edge, then forwards the store at its address.
fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    sweep: &mut Sweep,
) -> Result<()> {
    let (load_value, load_ty) = edit.single_value_output(load)?;

    let query = &sweep.query;
    let policy = Policy::forward(edit.function());
    let loc = Loc::read_by(edit.function(), load);
    let mut reaching = query.reach_and_narrow(edit, load, policy);
    let mut path = Vec::new();
    // Each round lifts the edge onto a strict dominator.
    while !matches!(reaching, ReachingDef::Match(_)) {
        #[cfg(test)]
        SETTLE_ROUNDS.with(|c| c.set(c.get() + 1));
        let mem = edit.load_mem(load);
        // Outside the reach no arm is known to dominate its merge.
        if !query.reaches(edit.function(), edit.producer(mem)) {
            break;
        }
        if let Some(&end) = sweep.ends.get(&(mem, loc)) {
            if end != mem {
                narrow_load_to(edit, load, edit.producer(end));
                reaching = query.reach_and_narrow(edit, load, policy);
            }
            break;
        }
        path.push(mem);
        let earlier = settle(&mut sweep.settled, edit.function(), mem);
        if earlier == mem {
            break;
        }
        narrow_load_to(edit, load, edit.producer(earlier));
        reaching = query.reach_and_narrow(edit, load, policy);
    }
    let end = edit.load_mem(load);
    for mem in path {
        sweep.ends.insert((mem, loc), end);
    }

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
        sweep.ends.clear();
    }
    // The store and the load's address are the forward's proof.
    let forwarded_node = edit.producer(forwarded);
    edit.absorb_node_fingerprint(forwarded_node, clobber_node);
    let addr = edit.load_addr(load);
    edit.absorb_fingerprint(forwarded, addr);
    edit.replace_value(load_value, forwarded);
    Ok(())
}

/// The earliest memory value `mem` equals through [`settle_one`] steps,
/// memoised in `settled`.
fn settle(settled: &mut FxHashMap<ValueId, ValueId>, f: &Function, mem: ValueId) -> ValueId {
    let mut path = Vec::new();
    let mut cur = mem;
    let end = loop {
        if let Some(&done) = settled.get(&cur) {
            break done;
        }
        match settle_one(f, cur) {
            Some(next) => {
                path.push(cur);
                cur = next;
            }
            None => {
                settled.insert(cur, cur);
                break cur;
            }
        }
    };
    for v in path {
        settled.insert(v, end);
    }
    end
}

/// The arm a `MemPhi` output equals: every arm on an edge some constant `If`
/// can take carries it, and every other arm is derived from it through
/// `Store` / `Call` / `CallOther`, so its def dominates the phi's region.
fn settle_one(f: &Function, mem: ValueId) -> Option<ValueId> {
    const ARM_STEPS: usize = 16;
    let phi = f.producer(mem);
    if !matches!(f.node_kind(phi), NodeKind::MemPhi) {
        return None;
    }
    let mut live = None;
    let mut dead = smallvec::SmallVec::<[ValueId; 2]>::new();
    for (edge, arm) in f.phi_arms(phi) {
        if never_taken(f, edge) {
            dead.push(arm);
        } else if *live.get_or_insert(arm) != arm {
            return None;
        }
    }
    let live = live.filter(|&m| m != mem)?;
    for mut arm in dead {
        let mut steps = 0;
        while arm != live {
            let def = f.producer(arm);
            if steps == ARM_STEPS
                || !matches!(
                    f.node_kind(def),
                    NodeKind::Store(_) | NodeKind::Call { .. } | NodeKind::CallOther { .. }
                )
            {
                return None;
            }
            arm = f.memory_input_of(def)?;
            steps += 1;
        }
    }
    Some(live)
}

/// Whether control edge `edge` leaves, through single-predecessor regions
/// and calls, the arm of an `If` its constant condition never takes.
fn never_taken(f: &Function, mut edge: ValueId) -> bool {
    const CTRL_STEPS: usize = 16;
    for _ in 0..CTRL_STEPS {
        let node = f.producer(edge);
        match f.node_kind(node) {
            NodeKind::If => {
                let budget = Cell::new(32);
                let Some(cond) = cone_const(f, f.if_cond(node), &budget) else {
                    return false;
                };
                let taken = f.node_outputs(node)[if cond == UBig::ZERO {
                    NodeKind::IF_FALSE_OUT_SLOT
                } else {
                    NodeKind::IF_TRUE_OUT_SLOT
                }];
                return edge != taken;
            }
            NodeKind::Region if f.node_inputs(node).len() == 1 => {
                edge = f.node_inputs(node)[0];
            }
            NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                edge = f.node_inputs(node)[NodeKind::CTRL_SLOT];
            }
            _ => return false,
        }
    }
    false
}

/// `value` evaluated over constant leaves, visiting at most `budget` nodes.
fn cone_const(f: &Function, value: ValueId, budget: &Cell<u32>) -> Option<UBig> {
    budget.set(budget.get().checked_sub(1)?);
    crate::const_eval::eval_node_const(f, value, &|v| cone_const(f, v, budget), None)
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
