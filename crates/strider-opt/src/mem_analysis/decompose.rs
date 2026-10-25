//! `decompose` reduces an address to one `MemExpr { base, offset, kind }` by
//! walking `Add`-of-constant steps (and an alignment-mask anchor) down to a
//! memory terminal: `InitialVar(sp)` for a stack base, or a pure allocator's
//! return pointer for a heap base.
//!
//! A `Phi` is a terminal of its own: when every value it can hold is derived
//! from some allocation it names heap memory of unknown identity
//! ([`MemKind::HeapOpaque`]), and otherwise it decomposes to `None`, read
//! conservatively as "not a provable terminal".

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId, low_bits_mask_u128};
use strider_ir::{Function, IRViewer, IntBinaryOp, MemDecomp};

use super::ring::wrap_to_addr_width;
use super::{MemExpr, MemKind};

/// Memoized: a hit is O(1), a miss walks the address spine and caches the result.
/// [`super::stamp`] states when the memo is drained.
///
/// The memo is keyed by `ValueId` alone, so every walk reads the one
/// allocator set the function holds ([`Function::assumptions`]), which only
/// grows.  The memo is also the verdict `strider-pattern`'s region filters
/// read.
pub(crate) fn decompose(function: &Function, value: ValueId) -> Option<MemExpr> {
    match function.side_tables().memory_class(value) {
        MemDecomp::Stack(_) | MemDecomp::Heap(_) => return resolve_slot(function, value),
        MemDecomp::NotMemory => return None,
        MemDecomp::Unknown => {}
    }
    let result = spine_walk(function, value);
    match result {
        Some(e) => {
            let st = function.side_tables();
            match e.kind {
                MemKind::Stack => st.set_stack_slot(value, e.base, e.offset),
                MemKind::Heap | MemKind::HeapOpaque => st.set_heap_slot(value, e.base, e.offset),
            }
        }
        None => function.side_tables().set_not_memory(value),
    }
    result
}

#[cfg(test)]
thread_local! {
    /// Total [`spine_walk`] iterations, the unit the spine-complexity test counts.
    pub(super) static SPINE_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Where a spine walk bottomed out.
enum SpineEnd {
    /// A memory terminal, with the offset accrued above it.
    Rooted(MemExpr),
    /// A `Phi`, whose arms decide the class, plus the offset accrued above it.
    Phi { phi: ValueId, offset: i128 },
    /// No terminal.
    Opaque,
}

/// [`spine_walk_trailed`], resolving a `Phi` end through its arms, plus the
/// verdict for every node it passed through: they all share the root's base, at
/// the root's offset less what accrued getting there.  Committing only the root
/// would make a consumer-first sweep re-walk the whole spine per query,
/// O(depth^2) over the chain.
fn spine_walk(function: &Function, value: ValueId) -> Option<MemExpr> {
    let mut trail: Vec<(ValueId, i128)> = Vec::new();
    let result = match spine_walk_trailed(function, value, &mut trail) {
        SpineEnd::Rooted(root) => Some(root),
        SpineEnd::Phi { phi, offset } if phi_names_an_allocation(function, phi) => Some(MemExpr {
            base: phi,
            offset,
            kind: MemKind::HeapOpaque,
        }),
        SpineEnd::Phi { .. } | SpineEnd::Opaque => None,
    };
    commit_trail(function, &trail, result);
    result
}

/// Commits `root`'s verdict for every node of its trail.
fn commit_trail(function: &Function, trail: &[(ValueId, i128)], root: Option<MemExpr>) {
    let st = function.side_tables();
    match root {
        Some(root) => {
            for &(v, acc) in trail {
                // A suffix sum that no longer fits is left for its own walk.
                let Some(offset) = root.offset.checked_sub(acc) else {
                    continue;
                };
                let offset = wrap_to_addr_width(function, v, offset);
                match root.kind {
                    MemKind::Stack => st.set_stack_slot(v, root.base, offset),
                    MemKind::Heap | MemKind::HeapOpaque => {
                        st.set_heap_slot(v, root.base, offset);
                    }
                }
            }
        }
        // The walk from any trail node is a suffix of this one, so it bails the
        // same way.  The arithmetic bails clear the trail, since a suffix sum
        // can fit where the whole one does not.
        None => {
            for &(v, _) in trail {
                st.set_not_memory(v);
            }
        }
    }
}

/// Does every value `root` can hold derive from an allocation?  Then the
/// pointer names heap memory, though not which allocation.
///
/// The frontier is `root` plus every `Phi` its arms reach, expanded through an
/// index rather than by recursion, so an arm cycling back to a phi already on
/// it is a revisit and the walk terminates.  Each arm's own spine walk stops at
/// the next phi, so no walk re-enters this one.
///
/// A verdict is exact per frontier node (each decides on the terminals IT
/// reaches, propagated along the arm edges) and is committed for all of them,
/// so a nested chain costs one frontier walk in total, not one per query.
fn phi_names_an_allocation(function: &Function, root: ValueId) -> bool {
    let mut nodes = vec![root];
    let mut index: FxHashMap<ValueId, usize> = FxHashMap::default();
    index.insert(root, 0);
    // Arm to the phis holding it: the edge both verdicts propagate along.
    let mut holders: Vec<Vec<usize>> = vec![Vec::new()];
    // A non-heap arm: this phi can hold something that is not an allocation.
    let mut opaque = vec![false];
    // An allocation arm: there is something to name.  A frontier of nothing but
    // phis has no definition at all and names nothing.
    let mut allocated = vec![false];
    let mut i = 0;
    while i < nodes.len() {
        let arms: Vec<ValueId> = function
            .phi_data_inputs(function.producer(nodes[i]))
            .collect();
        for arm in arms {
            let mut trail: Vec<(ValueId, i128)> = Vec::new();
            match spine_walk_trailed(function, arm, &mut trail) {
                SpineEnd::Phi { phi, .. } => {
                    let j = match index.get(&phi) {
                        Some(&j) => j,
                        None => {
                            let j = nodes.len();
                            nodes.push(phi);
                            holders.push(Vec::new());
                            opaque.push(false);
                            allocated.push(false);
                            index.insert(phi, j);
                            j
                        }
                    };
                    holders[j].push(i);
                }
                SpineEnd::Rooted(e) => {
                    commit_trail(function, &trail, Some(e));
                    match e.kind {
                        MemKind::Heap | MemKind::HeapOpaque => allocated[i] = true,
                        MemKind::Stack => opaque[i] = true,
                    }
                }
                SpineEnd::Opaque => {
                    commit_trail(function, &trail, None);
                    opaque[i] = true;
                }
            }
        }
        i += 1;
    }
    propagate(&mut opaque, &holders);
    propagate(&mut allocated, &holders);
    for (i, &v) in nodes.iter().enumerate() {
        if allocated[i] && !opaque[i] {
            function.side_tables().set_heap_slot(v, v, 0);
        } else {
            function.side_tables().set_not_memory(v);
        }
    }
    allocated[0] && !opaque[0]
}

/// Reverse-reachability closure over `holders`: a set arm sets every phi
/// holding it.
fn propagate(flag: &mut [bool], holders: &[Vec<usize>]) {
    let mut work: Vec<usize> = (0..flag.len()).filter(|&i| flag[i]).collect();
    while let Some(i) = work.pop() {
        for &h in &holders[i] {
            if !flag[h] {
                flag[h] = true;
                work.push(h);
            }
        }
    }
}

/// Accumulates the constant offset down to the memory terminal.  O(spine depth),
/// not O(cone): only the single base-bearing operand is followed at each step.
///
/// `trail` collects `(node, offset accrued above it)` for every node down to
/// and including the alignment anchor.  Below the anchor offsets stop
/// accumulating, so those nodes do not share the returned base.
fn spine_walk_trailed(
    function: &Function,
    value: ValueId,
    trail: &mut Vec<(ValueId, i128)>,
) -> SpineEnd {
    let mut cur = value;
    let mut acc: i128 = 0;
    // Once an alignment-`And` is met the base is fixed to that And output (an
    // opaque, entry-alignment-dependent base) carrying the offset accrued above
    // it.  Below the anchor the walk only confirms SP-rooting, so those offsets
    // are ignored.  Iterative so a deep `And`-of-`And` chain cannot blow the
    // host stack.
    //
    // Termination: each step follows one input edge toward `InitialVar(sp)`, and
    // revisiting a node needs a data cycle, which in valid SSA passes through a
    // `Phi`, where the walk stops.  So the spine is acyclic.
    let mut anchor: Option<MemExpr> = None;
    loop {
        #[cfg(test)]
        SPINE_STEPS.with(|c| c.set(c.get() + 1));
        if anchor.is_none() {
            trail.push((cur, acc));
        }
        // A committed verdict short-circuits the walk.
        match function.side_tables().memory_class(cur) {
            MemDecomp::Stack(_) | MemDecomp::Heap(_) => {
                let Some(hit) = resolve_slot(function, cur) else {
                    trail.clear();
                    return SpineEnd::Opaque;
                };
                return match anchor {
                    // Same bail as the allocator terminal below, for a base the
                    // memo already committed.
                    Some(_) if hit.kind != MemKind::Stack => SpineEnd::Opaque,
                    // Above a stack base the anchor is the conservative result.
                    Some(a) => SpineEnd::Rooted(a),
                    None => {
                        let Some(sum) = hit.offset.checked_add(acc) else {
                            trail.clear();
                            return SpineEnd::Opaque;
                        };
                        SpineEnd::Rooted(MemExpr {
                            base: hit.base,
                            offset: wrap_to_addr_width(function, cur, sum),
                            kind: hit.kind,
                        })
                    }
                };
            }
            MemDecomp::NotMemory => return SpineEnd::Opaque,
            MemDecomp::Unknown => {}
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(_) if function.is_entry_sp(cur) => {
                return SpineEnd::Rooted(anchor.unwrap_or(MemExpr {
                    base: cur,
                    offset: wrap_to_addr_width(function, cur, acc),
                    kind: MemKind::Stack,
                }));
            }
            // A pure allocator's return pointer is a fresh heap base, bottomed
            // out exactly like the SP terminal.
            NodeKind::Call { .. } if is_allocator_return(function, cur, node) => {
                // An alignment mask above a heap base leaves the masked pointer's
                // offset to the raw base unknown, and heap disjointness is exact,
                // so returning the Stack-kinded anchor would call an aligned heap
                // pointer SP-rooted and Disjoint from its own object.
                if anchor.is_some() {
                    return SpineEnd::Opaque;
                }
                return SpineEnd::Rooted(MemExpr {
                    base: cur,
                    offset: wrap_to_addr_width(function, cur, acc),
                    kind: MemKind::Heap,
                });
            }
            // The arms decide, which is the caller's walk, not this one: it
            // would have to fan out, and an arm can lead back here.  The offset
            // rides along; below an alignment mask it is unknown, the same bail
            // as the allocator terminal above.
            NodeKind::Phi => {
                if anchor.is_some() {
                    return SpineEnd::Opaque;
                }
                return SpineEnd::Phi {
                    phi: cur,
                    offset: wrap_to_addr_width(function, cur, acc),
                };
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                // SP + const in either order.  The const shifts the accumulated
                // offset only while above the anchor.
                let Some((sp_operand, c)) = crate::const_eval::const_addend(function, node) else {
                    return SpineEnd::Opaque;
                };
                if anchor.is_none() {
                    // Fail-closed on overflow.
                    let Some(next) = acc.checked_add(c) else {
                        trail.clear();
                        return SpineEnd::Opaque;
                    };
                    acc = next;
                }
                cur = sp_operand;
            }
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                // Alignment-masked SP is a fresh opaque base, since its value
                // depends on entry-SP alignment.  The non-mask operand must be
                // SP-rooted; the base fixes to this And output the first time.
                let Some(sp_operand) = alignment_masked_operand(function, node) else {
                    return SpineEnd::Opaque;
                };
                anchor.get_or_insert(MemExpr {
                    base: cur,
                    offset: wrap_to_addr_width(function, cur, acc),
                    kind: MemKind::Stack,
                });
                cur = sp_operand;
            }
            _ => return SpineEnd::Opaque,
        }
    }
}

/// `value` is the primary return of a `Call` to a known allocator, i.e. a fresh
/// heap base. `build_call` emits ret-vals before clobbers, so the return
/// pointer is the first `Call` value output; only that output is a base, never
/// a clobbered register.
fn is_allocator_return(function: &Function, value: ValueId, call: NodeId) -> bool {
    let first_value = function.node_kind(call).output_head_len();
    function.node_outputs(call).get(first_value) == Some(&value)
        && call_returns_allocation(function, call)
}

/// `call` is to a known allocator and emits a return value, so its first value
/// output is a fresh heap base.
fn call_returns_allocation(function: &Function, call: NodeId) -> bool {
    let target = function.node_inputs(call)[NodeKind::CALL_TARGET_SLOT];
    let is_alloc = function
        .int_const_u128(target)
        .and_then(|a| u64::try_from(a).ok())
        .is_some_and(|a| function.assumptions().noalias_allocators.contains(&a));
    // A cheap gate before the tracked-varnode scan below: the allocator set is
    // empty by default.
    if !is_alloc {
        return false;
    }
    // The first value output is the return pointer only when the call emits
    // one.  A declared return register is not enough: `ret_and_clobber_vns`
    // drops one this function does not track, sliding the first clobber into
    // that slot, and a clobber holds garbage rather than a fresh pointer.
    let all = function.all_vns();
    let (ret_vals, _) = function
        .get_cc(call)
        .ret_and_clobber_vns(all, |v| vn_container::largest_container_in(all, v));
    !ret_vals.is_empty()
}

/// The memo tags the region, not the identity: a heap base that is a `Phi` is
/// the set of allocations its arms hold, which is what
/// [`phi_names_an_allocation`] commits.
fn resolve_slot(function: &Function, value: ValueId) -> Option<MemExpr> {
    let (class, slot) = function.side_tables().memory_decomp(value);
    let (base, offset) = slot?;
    let kind = match class {
        MemDecomp::Stack(_) => MemKind::Stack,
        MemDecomp::Heap(_) => match function.node_kind(function.producer(base)) {
            NodeKind::Phi => MemKind::HeapOpaque,
            _ => MemKind::Heap,
        },
        MemDecomp::Unknown | MemDecomp::NotMemory => return None,
    };
    Some(MemExpr { base, offset, kind })
}

/// A contiguous run of 1-bits over a low 0-bit run of a plausible alignment,
/// e.g. `0xFFFF_FFF0`.  A low-bit mask like `0xF` is a bit-extraction, not a
/// base, and zero / all-ones have no alignment effect, so all are rejected.
pub(super) fn is_alignment_mask(m: u128, width_bits: usize) -> bool {
    /// Largest alignment a frame demands, `alignas(4096)`.  Past it the mask
    /// clears more of the address than any ABI aligns: `sp & 0x8000_0000` at
    /// I32 is 0 or 0x8000_0000, which is not `sp` rounded down in any useful
    /// sense, the same reason `sp & 0x10` is rejected below.
    const MAX_ALIGN_BITS: u32 = 12;
    let tz = m.trailing_zeros();
    // A width past the `u128` carrier has no mask this can compare against.
    if tz == 0 || tz > MAX_ALIGN_BITS || (tz as usize) >= width_bits || width_bits > 128 {
        return false;
    }
    // Past the low zero run the rest must be a contiguous block of 1s that
    // reaches the TOP of the address width. `sp & 0x10` is 0 or 16 and
    // `sp & 0xFFFF_FFF0` at I64 truncates to the low 4 GiB; neither is `sp`
    // rounded down, and treating either as a stack base lets
    // `stack_global_disjoint` call it disjoint from a constant it may equal.
    let width_mask = low_bits_mask_u128(width_bits);
    m & width_mask == width_mask & !low_bits_mask_u128(tz as usize)
}

/// The operands of a node whose signature fixes its arity at two.
fn binary_operands(function: &Function, node: NodeId) -> [ValueId; 2] {
    function
        .node_inputs_exact::<2>(node)
        .expect("IntBinaryOp has 2 inputs per node signature")
}

/// The masked operand of an alignment-`And`, `None` when `node` is anything
/// else.  The one reading of the anchor shape [`decompose`] roots a base at.
pub(crate) fn alignment_masked_operand(function: &Function, node: NodeId) -> Option<ValueId> {
    if !matches!(
        function.node_kind(node),
        NodeKind::IntBinaryOp(IntBinaryOp::And)
    ) {
        return None;
    }
    let [l, r] = binary_operands(function, node);
    // The mask only names a stack base relative to the width it is applied at.
    let Ok(ty) = function.value_type(function.node_outputs(node)[0]) else {
        return None;
    };
    let width = ty.bit_width();
    if function
        .int_const_u128(r)
        .is_some_and(|m| is_alignment_mask(m, width))
    {
        Some(l)
    } else if function
        .int_const_u128(l)
        .is_some_and(|m| is_alignment_mask(m, width))
    {
        Some(r)
    } else {
        None
    }
}

/// The heap base a `Call` returns if it is a known pure allocator, else `None`.
pub(super) fn allocator_return_base(function: &Function, call: NodeId) -> Option<ValueId> {
    debug_assert!(
        matches!(function.node_kind(call), NodeKind::Call { .. }),
        "allocator_return_base on a non-Call"
    );
    let first_value = function.node_kind(call).output_head_len();
    let ret = *function.node_outputs(call).get(first_value)?;
    call_returns_allocation(function, call).then_some(ret)
}
