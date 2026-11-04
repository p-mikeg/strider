//! An address as `base + offset`, down `Add`-of-constant steps and alignment
//! masks to `InitialVar(sp)` (stack) or a pure allocator's return (heap).  A
//! `Phi` all of whose values derive from allocations is
//! [`MemKind::HeapOpaque`]; any other `Phi` is `None`.

use rustc_hash::FxHashMap;
use strider_ir::eval::mask_below;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IntBinaryOp, MemDecomp};

use super::ring::wrap_to_addr_width;
use super::{MemExpr, MemKind};

/// Memoised per `ValueId` on the function, for every node the walk passes;
/// [`super::stamp`] says when the memo is drained.
pub(crate) fn decompose(function: &Function, value: ValueId) -> Option<MemExpr> {
    if let Some(verdict) = memoised(function, value) {
        return verdict;
    }
    let (spine, end) = descend(function, value);
    let below = match end {
        End::Known(verdict) => Some(verdict),
        // Below an alignment mask the offset to the phi is unknown, and the
        // mask over it is opaque whatever the phi holds.
        End::Phi(_) if aligned(&spine) => None,
        End::Phi(phi) => Some(phi_names_an_allocation(function, phi).then_some(MemExpr {
            base: phi,
            offset: 0,
            kind: MemKind::HeapOpaque,
        })),
    };
    unwind(function, &spine, below).expect("a spine whose bottom is unknown holds a mask")
}

#[cfg(test)]
thread_local! {
    /// Total [`descend`] iterations, the unit the spine-complexity test counts.
    pub(super) static SPINE_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One step of an address spine, from a node to the one operand that carries
/// its base.
#[derive(Clone, Copy)]
enum Step {
    /// `+ c`.
    Add(i128),
    /// An alignment mask: a fresh stack base of its own.
    Align,
}

/// Where a spine bottoms out.
enum End {
    /// A verdict already committed: memoised, or a terminal its node decides.
    Known(Option<MemExpr>),
    /// A `Phi`, whose arms decide.
    Phi(ValueId),
}

/// The spine below `value`, each node with its step, down to the first node
/// whose verdict needs no operand.  Ends because every data cycle passes a
/// `Phi`, where it stops.
fn descend(function: &Function, value: ValueId) -> (Vec<(ValueId, Step)>, End) {
    let mut spine = Vec::new();
    let mut cur = value;
    loop {
        #[cfg(test)]
        SPINE_STEPS.with(|c| c.set(c.get() + 1));
        if let Some(verdict) = memoised(function, cur) {
            return (spine, End::Known(verdict));
        }
        let node = function.producer(cur);
        let root = |kind| {
            Some(MemExpr {
                base: cur,
                offset: 0,
                kind,
            })
        };
        let (next, step) = match *function.node_kind(node) {
            NodeKind::InitialVar(_) if function.is_entry_sp(cur) => {
                return (
                    spine,
                    End::Known(commit(function, cur, root(MemKind::Stack))),
                );
            }
            NodeKind::Call { .. } if allocator_return_base(function, node) == Some(cur) => {
                return (
                    spine,
                    End::Known(commit(function, cur, root(MemKind::Heap))),
                );
            }
            NodeKind::Phi => return (spine, End::Phi(cur)),
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                match crate::const_eval::const_addend(function, node) {
                    Some((operand, c)) => (operand, Step::Add(c)),
                    None => return (spine, End::Known(commit(function, cur, None))),
                }
            }
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                match alignment_masked_operand(function, node) {
                    Some((operand, _)) => (operand, Step::Align),
                    None => return (spine, End::Known(commit(function, cur, None))),
                }
            }
            _ => return (spine, End::Known(commit(function, cur, None))),
        };
        spine.push((cur, step));
        cur = next;
    }
}

/// Whether `spine` holds an alignment mask.
fn aligned(spine: &[(ValueId, Step)]) -> bool {
    spine.iter().any(|(_, step)| matches!(step, Step::Align))
}

/// Commits each node's verdict of `spine` bottom up from `below` (`None`
/// while unknown, which a mask ends), returning the top node's.
fn unwind(
    function: &Function,
    spine: &[(ValueId, Step)],
    below: Option<Option<MemExpr>>,
) -> Option<Option<MemExpr>> {
    spine.iter().rev().fold(below, |below, &(v, step)| {
        let verdict =
            match step {
                Step::Add(c) => below.map(|e| {
                    e.map(|e| MemExpr {
                        offset: wrap_to_addr_width(function, v, e.offset.wrapping_add(c)),
                        ..e
                    })
                }),
                // A mask over a heap base is opaque: heap disjointness is
                // exact, so a stack-kinded mask would call an aligned heap
                // pointer disjoint from its own object.
                Step::Align => Some(below.flatten().filter(|e| e.kind == MemKind::Stack).map(
                    |_| MemExpr {
                        base: v,
                        offset: 0,
                        kind: MemKind::Stack,
                    },
                )),
            };
        verdict.map(|verdict| commit(function, v, verdict))
    })
}

/// Memoises `verdict` for `v` and returns it.
fn commit(function: &Function, v: ValueId, verdict: Option<MemExpr>) -> Option<MemExpr> {
    match verdict {
        Some(e) if e.kind == MemKind::Stack => function.set_stack_slot(v, e.base, e.offset),
        Some(e) => function.set_heap_slot(v, e.base, e.offset),
        None => function.set_not_memory(v),
    }
    verdict
}

/// Whether every value `root` can hold derives from an allocation.  Walks
/// `root` and every `Phi` its arms reach once, and commits the verdict of
/// each.
fn phi_names_an_allocation(function: &Function, root: ValueId) -> bool {
    let mut frontier = vec![Frontier::new(root)];
    let mut index: FxHashMap<ValueId, usize> = FxHashMap::default();
    index.insert(root, 0);
    let mut i = 0;
    while i < frontier.len() {
        let arms: Vec<ValueId> = function
            .phi_data_inputs(function.producer(frontier[i].phi))
            .collect();
        for arm in arms {
            let (spine, end) = descend(function, arm);
            let below = match end {
                End::Phi(phi) if !aligned(&spine) => {
                    let j = *index.entry(phi).or_insert_with(|| {
                        frontier.push(Frontier::new(phi));
                        frontier.len() - 1
                    });
                    frontier[j].holders.push(i);
                    continue;
                }
                End::Phi(_) => None,
                End::Known(verdict) => Some(verdict),
            };
            frontier[i].holds |= match unwind(function, &spine, below).flatten() {
                Some(e) if e.kind != MemKind::Stack => ALLOCATED,
                _ => OPAQUE,
            };
        }
        i += 1;
    }
    propagate(&mut frontier);
    for f in &frontier {
        if f.holds == ALLOCATED {
            function.set_heap_slot(f.phi, f.phi, 0);
        } else {
            function.set_not_memory(f.phi);
        }
    }
    frontier[0].holds == ALLOCATED
}

/// A non-heap arm: the phi can hold something that is not an allocation.
const OPAQUE: u8 = 1;
/// An allocation arm.  A frontier of nothing but phis names nothing.
const ALLOCATED: u8 = 2;

/// One phi of [`phi_names_an_allocation`]'s frontier.
struct Frontier {
    phi: ValueId,
    /// The frontier phis holding this one as an arm.
    holders: Vec<usize>,
    /// What its arms hold, [`OPAQUE`] and [`ALLOCATED`] bits.
    holds: u8,
}

impl Frontier {
    fn new(phi: ValueId) -> Self {
        Self {
            phi,
            holders: Vec::new(),
            holds: 0,
        }
    }
}

/// Sets an arm's bits on every phi holding it, transitively.
fn propagate(frontier: &mut [Frontier]) {
    let mut work: Vec<usize> = (0..frontier.len())
        .filter(|&i| frontier[i].holds != 0)
        .collect();
    while let Some(i) = work.pop() {
        let holds = frontier[i].holds;
        for k in 0..frontier[i].holders.len() {
            let h = frontier[i].holders[k];
            if frontier[h].holds | holds != frontier[h].holds {
                frontier[h].holds |= holds;
                work.push(h);
            }
        }
    }
}

/// A `Call`'s SP as `(stack base, offset)`.
pub(crate) fn call_sp(function: &Function, call: NodeId) -> Option<(ValueId, i128)> {
    match decompose(function, function.node_inputs(call)[NodeKind::CALL_SP_SLOT])? {
        MemExpr {
            base,
            offset,
            kind: MemKind::Stack,
        } => Some((base, offset)),
        _ => None,
    }
}

/// The heap base a `Call` returns if it is a listed pure allocator: its first
/// value output.
pub(super) fn allocator_return_base(function: &Function, call: NodeId) -> Option<ValueId> {
    debug_assert!(
        matches!(function.node_kind(call), NodeKind::Call { .. }),
        "allocator_return_base on a non-Call"
    );
    let first_value = function.node_kind(call).output_head_len();
    let ret = *function.node_outputs(call).get(first_value)?;
    let target = function.node_inputs(call)[NodeKind::CALL_TARGET_SLOT];
    let is_alloc = function
        .int_const_u128(target)
        .and_then(|a| u64::try_from(a).ok())
        .is_some_and(|a| function.assumptions().noalias_allocators.contains(&a));
    if !is_alloc {
        return None;
    }
    // Only if it is the first declared return register: an untracked one is
    // dropped, sliding another register into the slot.
    let all = function.all_vns();
    let container = |v: &rsleigh::Vn| vn_container::largest_container_in(all, v);
    let cc = function.get_cc(call);
    let (ret_vals, _) = cc.ret_and_clobber_vns(all, container);
    let pointer = cc.ret_val_regs.first().map(container)?;
    (ret_vals.first() == Some(&pointer)).then_some(ret)
}

/// The memoised verdict for `value`, `None` while not yet computed.
fn memoised(function: &Function, value: ValueId) -> Option<Option<MemExpr>> {
    let (class, slot) = function.memory_decomp(value);
    let resolved = || slot.expect("a region class is set with the slot it resolves to");
    Some(match class {
        MemDecomp::Unknown => return None,
        MemDecomp::NotMemory => None,
        MemDecomp::Stack(_) => {
            let (base, offset) = resolved();
            Some(MemExpr {
                base,
                offset,
                kind: MemKind::Stack,
            })
        }
        MemDecomp::Heap(_) => {
            let (base, offset) = resolved();
            let kind = match function.kind_of_value(base) {
                NodeKind::Phi => MemKind::HeapOpaque,
                _ => MemKind::Heap,
            };
            Some(MemExpr { base, offset, kind })
        }
    })
}

/// Whether `m` is 1s from the top of the width down to a low 0-run of at
/// most `MAX_ALIGN_BITS`, e.g. `0xFFFF_FFF0`.
pub(super) fn is_alignment_mask(m: u128, width_bits: usize) -> bool {
    /// Largest alignment a frame demands, `alignas(4096)`.
    const MAX_ALIGN_BITS: u32 = 12;
    let tz = m.trailing_zeros();
    if tz == 0 || tz > MAX_ALIGN_BITS || (tz as usize) >= width_bits || width_bits > 128 {
        return false;
    }
    // Short of the top, `sp & 0xFFFF_FFF0` at I64 is not `sp` rounded down,
    // and as a stack base `stack_global_disjoint` would misjudge it.
    let width_mask = mask_below(width_bits as u32);
    m & width_mask == width_mask & !mask_below(tz)
}

/// The masked operand of an alignment-`And` and the alignment it rounds down
/// to.
pub(crate) fn alignment_masked_operand(
    function: &Function,
    node: NodeId,
) -> Option<(ValueId, i128)> {
    let (operand, mask) = crate::const_eval::var_and_const_of(
        function,
        node,
        NodeKind::IntBinaryOp(IntBinaryOp::And),
    )?;
    let width = function
        .value_type(function.node_outputs(node)[0])
        .ok()?
        .bit_width();
    is_alignment_mask(mask, width).then(|| (operand, 1i128 << mask.trailing_zeros()))
}
