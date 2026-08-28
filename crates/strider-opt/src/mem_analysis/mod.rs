//! `decompose` reduces an address to one `MemExpr { base, offset, kind }` by
//! walking `Add`-of-constant steps (and an alignment-mask anchor) down to a
//! memory terminal: `InitialVar(sp)` for a stack base, or a pure allocator's
//! return pointer for a heap base.
//!
//! A `Phi` is a terminal of its own: when every value it can hold is derived
//! from some allocation it names heap memory of unknown identity
//! ([`MemKind::HeapOpaque`]), and otherwise it decomposes to `None`, read
//! conservatively as "not a provable terminal".

use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, IntBinaryOp, MemDecomp};
use strider_target::Endianness;

use crate::OptOptions;
use crate::mem_ssa::MemorySSAWalker;
use AddrClass::*;

mod frame_escape;

/// Which memory region a decomposed base roots in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemKind {
    /// `InitialVar(sp)` or an alignment-masked SP `And` output.
    Stack,
    /// A pure allocator's fresh return pointer.
    Heap,
    /// Some allocation, which one unknown: a `Phi` whose every arm is heap.
    /// Carries no usable identity, so it is may-alias against every heap base
    /// including itself.
    HeapOpaque,
}

/// `base + offset`, where `base` is a memory terminal of the given `kind`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemExpr {
    pub base: ValueId,
    pub offset: i128,
    pub kind: MemKind,
}

/// Coarse Load / Store address class; [`alias_verdict`] is keyed on the
/// `(load_class, store_class)` pair.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddrClass {
    /// Offsets are only comparable within one `base`.  Distinct bases (say
    /// `InitialVar(sp)` vs `sp & -16`) differ by the caller-dependent
    /// `sp mod align`, so their offsets live in different coordinate systems
    /// and must be treated as may-alias.
    StackRooted { base: ValueId, offset: i128 },
    /// Rooted at a pure allocator's return pointer. Two distinct allocation
    /// bases never overlap (noalias), unlike SP bases, and no allocation ever
    /// coincides with the stack or a global.
    HeapRooted { base: ValueId, offset: i128 },
    /// Rooted at a `Phi` of heap bases: provably an allocation, but the base
    /// is one of several, so it may be any of them and offsets from it are not
    /// comparable with anything.
    HeapOpaque,
    /// Literal `.data` / `.rodata` / `.bss` / MMIO pointer.
    Constant { addr: i128 },
    /// Anything else.  Only `ValueId` equality proves two of these equal;
    /// distinct ids can still compute the same runtime address.
    Anchor { value: ValueId },
}

/// Verdict for one (load, store) address pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasVerdict {
    /// Same start address; the sizes are the caller's to check.
    Match,
    /// Provably non-overlapping.
    Disjoint,
    /// Neither provable.
    MayAlias,
}

/// Memoized: a hit is O(1), a miss walks the address spine and caches the result.
/// Sound because the fixed-point loop drains the memo after every changing
/// pass; the post-passes that skip the drain only mutate the graph in ways that
/// leave every address value's decomposition unchanged.
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

/// Bit width of an address expression, `None` when `addr` is not a typed value
/// edge.
fn addr_bit_width(function: &Function, addr: ValueId) -> Option<u32> {
    function
        .value_type(addr)
        .ok()
        .and_then(|ty| u32::try_from(ty.bit_width()).ok())
}

/// Machine address arithmetic is mod 2^bitwidth, so reduce an accumulated byte
/// offset into `addr`'s signed range.  Unreduced, `(sp + 0x7FFFFFFF) +
/// 0x7FFFFFFF` on a 32-bit target reads as `sp + 4294967294` instead of
/// `sp - 2`, Disjoint from the slot it actually names.  An address wider than
/// the i128 carrier keeps the raw sum.
fn wrap_to_addr_width(function: &Function, addr: ValueId, offset: i128) -> i128 {
    function
        .value_type(addr)
        .ok()
        .and_then(|ty| ty.get_signed_int(offset as u128))
        .unwrap_or(offset)
}

#[cfg(test)]
thread_local! {
    /// Total [`spine_walk`] iterations, the unit the spine-complexity test counts.
    pub(crate) static SPINE_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
            NodeKind::InitialVar(id)
                if function.initial_vn(id) == function.default_cc().stack_vn =>
            {
                return SpineEnd::Rooted(anchor.unwrap_or(MemExpr {
                    base: cur,
                    offset: wrap_to_addr_width(function, cur, acc),
                    kind: MemKind::Stack,
                }));
            }
            // A pure allocator's return pointer is a fresh heap base, bottomed
            // out exactly like the SP terminal.
            NodeKind::Call if is_allocator_return(function, cur, node) => {
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
                let [lhs, rhs] = binary_operands(function, node);
                // SP + const in either order.  The const shifts the accumulated
                // offset only while above the anchor.
                let (sp_operand, c) =
                    match (function.int_const_i128(rhs), function.int_const_i128(lhs)) {
                        (Some(c), _) => (lhs, c),
                        (None, Some(c)) => (rhs, c),
                        _ => return SpineEnd::Opaque,
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
/// pointer is the first `Call` value output (`outputs[2]`, after control and
/// memory); only that output is a base, never a clobbered register.
fn is_allocator_return(function: &Function, value: ValueId, call: NodeId) -> bool {
    if function.node_outputs(call).get(2) != Some(&value) {
        return false;
    }
    // Call inputs are [ctrl, mem, target, sp, ...args]; the target is slot 2.
    let target = function.node_inputs(call)[2];
    let is_alloc = function
        .int_const_u128(target)
        .and_then(|a| u64::try_from(a).ok())
        .is_some_and(|a| function.side_tables().is_noalias_allocator(a));
    // A cheap gate before the tracked-varnode scan below: the allocator set is
    // empty by default.
    if !is_alloc {
        return false;
    }
    // `outputs[2]` is the return pointer only when the call actually emits
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

fn classify_addr(function: &Function, addr: ValueId) -> AddrClass {
    match decompose(function, addr) {
        Some(MemExpr { base, offset, kind }) => match kind {
            MemKind::Heap => AddrClass::HeapRooted { base, offset },
            MemKind::HeapOpaque => AddrClass::HeapOpaque,
            MemKind::Stack => AddrClass::StackRooted { base, offset },
        },
        None => {
            if let Some(c) = function.int_const_u128(addr) {
                AddrClass::Constant { addr: c as i128 }
            } else {
                AddrClass::Anchor { value: addr }
            }
        }
    }
}

/// The one classification of a `Store`'s address, so every consumer agrees.
fn classify_store_addr(function: &Function, store_node: NodeId) -> AddrClass {
    classify_addr(function, function.store_addr(store_node))
}

/// A contiguous run of high 1-bits over at least one low 0-bit, e.g.
/// `0xFFFF_FFF0`.  A low-bit mask like `0xF` is a bit-extraction, not a base,
/// and zero / all-ones have no alignment effect, so all are rejected.
fn is_alignment_mask(m: u128) -> bool {
    let tz = m.trailing_zeros();
    if tz == 0 || tz == 128 {
        return false;
    }
    // Past the low zero run the rest must be a contiguous block of 1s, i.e.
    // `shifted + 1` is a power of two.
    let shifted = m >> tz;
    shifted != 0 && shifted & shifted.wrapping_add(1) == 0
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
    if function.int_const_u128(r).is_some_and(is_alignment_mask) {
        Some(l)
    } else if function.int_const_u128(l).is_some_and(is_alignment_mask) {
        Some(r)
    } else {
        None
    }
}

impl AddrClass {
    fn offset(self) -> Option<i128> {
        match self {
            StackRooted { offset, .. } | HeapRooted { offset, .. } => Some(offset),
            Constant { addr } => Some(addr),
            HeapOpaque | Anchor { .. } => None,
        }
    }
}

/// Endpoints saturate, so an oversized `size` cannot panic or wrap; a saturated
/// endpoint reports "not disjoint", treating the range as unbounded.
#[inline]
fn ranges_disjoint(a_off: i128, a_size: i128, b_off: i128, b_size: i128) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    if a_end == i128::MAX || b_end == i128::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Only the same-base `StackRooted`, same-base `HeapRooted`, and
/// `Constant`/`Constant` arms of [`alias_verdict`] reach here; each carries an
/// offset, so the unwraps hold.
fn offset_range_verdict(load: SizedAddr, store: SizedAddr) -> AliasVerdict {
    let load_off = load.class.offset().expect("offset-bearing load class");
    let store_off = store.class.offset().expect("offset-bearing store class");
    if !offsets_comparable(load, store, load_off, store_off) {
        AliasVerdict::MayAlias
    } else if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load.size, store_off, store.size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// Whether the two reduced offsets can be compared in integer order.
///
/// `wrap_to_addr_width` reduces each offset into the address type's signed
/// range, which is correct mod 2^w but not order-preserving across the wrap:
/// two addresses two bytes apart mod 2^32 reduce to `+2147483646` and
/// `-2147483648`, which any interval test reads as nearly 2^32 apart. Whenever
/// the reduced distance exceeds half the modulus the true distance is the
/// short way round, and only `MayAlias` is sound.
fn offsets_comparable(load: SizedAddr, store: SizedAddr, load_off: i128, store_off: i128) -> bool {
    let bits = match (load.addr_bits, store.addr_bits) {
        // A missing width is "not known", not "not reduced": the offsets
        // cannot be shown to share a modulus, so only `MayAlias` is sound.
        (None, _) | (_, None) => return false,
        (Some(load_bits), Some(store_bits)) => {
            // Two reductions at different widths land in different moduli, so
            // their difference names no distance at all and only `MayAlias` is
            // sound. Taking one side's width would compute the bound from the
            // wrong modulus and read a wrapped range as far away.
            if load_bits != store_bits {
                return false;
            }
            load_bits
        }
    };
    // Past the i128 carrier `wrap_to_addr_width` cannot reduce, so the offsets
    // are the exact sums and compare in integer order. At exactly 128 it DOES
    // reduce, and `1i128 << 127` would overflow, so there is no bound to test.
    if bits > 128 {
        return true;
    }
    if bits == 128 {
        return false;
    }
    let half = 1i128 << (bits - 1);
    let span = load.size.max(store.size);
    // A span past the half-modulus leaves no comparable distance at all, so
    // the bound floors at zero rather than turning negative.
    let bound = half.saturating_sub(span).max(0);
    load_off.abs_diff(store_off) <= bound.unsigned_abs()
}

/// One operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
    /// Bit width of the address expression, for the modular-comparability
    /// check in [`offsets_comparable`]. `None` disables it.
    pub(crate) addr_bits: Option<u32>,
}

pub(crate) fn alias_verdict(
    load: SizedAddr,
    store: SizedAddr,
    options: MemOptions,
) -> AliasVerdict {
    let distinct_sp_bases_disjoint = options.distinct_sp_bases_disjoint;
    match (load.class, store.class) {
        // Different SP bases differ by an unknown amount, so their offsets are
        // unrelated and normally may-alias.  `distinct_sp_bases_disjoint` opts
        // into assuming they address disjoint regions, which stack-arg
        // detection relies on: incoming-arg slots above the entry SP do not
        // overlap frame locals rooted at an alignment-masked SP.
        (StackRooted { base: lb, .. }, StackRooted { base: sb, .. }) => {
            if lb == sb {
                offset_range_verdict(load, store)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Distinct bases from a listed noalias allocator are taken as disjoint,
        // and no such allocation as coinciding with the stack or a global.  The
        // claim rests on `AssumptionOptions::noalias_allocators` being right,
        // not on the IR.  Same base falls through to offsets;
        // a heap base against an opaque `Anchor` stays may-alias (the `_` arm),
        // since the opaque pointer could be this allocation laundered.
        (HeapRooted { base: lb, .. }, HeapRooted { base: sb, .. }) => {
            if lb == sb {
                offset_range_verdict(load, store)
            } else {
                AliasVerdict::Disjoint
            }
        }
        // An allocation of unknown identity is still an allocation: no
        // allocation overlaps the stack or a global.
        (HeapRooted { .. } | HeapOpaque, StackRooted { .. } | Constant { .. })
        | (StackRooted { .. } | Constant { .. }, HeapRooted { .. } | HeapOpaque) => {
            AliasVerdict::Disjoint
        }
        // SOUNDNESS: an unknown identity may be ANY identity, including the
        // very base it is paired with.  Only the region is proven, so nothing
        // here is disjoint and no offset is comparable.
        (HeapOpaque, HeapRooted { .. } | HeapOpaque) | (HeapRooted { .. }, HeapOpaque) => {
            AliasVerdict::MayAlias
        }
        (Constant { .. }, Constant { .. }) => offset_range_verdict(load, store),
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Distinct ids can still compute the same runtime address.
                AliasVerdict::MayAlias
            }
        }
        (StackRooted { .. }, Constant { .. }) | (Constant { .. }, StackRooted { .. }) => {
            if options.stack_global_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Anchor against anything bails either way.
        _ => AliasVerdict::MayAlias,
    }
}

/// The SP-aware [`MemorySSAWalker`].
struct MemWalker<'a> {
    analyzer: &'a MemAnalyzer,
    /// The probed location.  Precomputed rather than a `Load` `NodeId`, since
    /// a probe need not have a backing `Load` node.
    load: SizedAddr,
    /// Distinct spaces never alias, even at the same numeric address.
    load_space: rsleigh::VnSpace,
    /// [`Self::private_frame_forward`] reads only `load` and the options, both
    /// fixed for the whole walk, and falls through to a bounded SP-spine climb.
    /// Asked repeatedly along one walk, so recomputing multiplies the walk by
    /// the spine depth.
    private_frame: std::cell::Cell<Option<bool>>,
}

#[cfg(test)]
thread_local! {
    /// Total [`MemWalker::def_clobbers`] calls, the unit the walk-complexity
    /// tests count.
    pub(crate) static WALK_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl MemorySSAWalker for MemWalker<'_> {
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        #[cfg(test)]
        WALK_STEPS.with(|c| c.set(c.get() + 1));
        match *function.node_kind(def) {
            // A cross-space store cannot clobber this load, so the walk
            // continues to a same-space def.  Stopping here would let
            // `load_forward` forward a different-space store's value into the
            // load, a miscompile.
            NodeKind::Store(store_space) => {
                if store_space != self.load_space {
                    return false;
                }
                let store = self.analyzer.store_sized(function, def);
                // A private-frame SP slot cannot be aliased by an opaque
                // (`Anchor`) store: for that pointer to name this slot a frame
                // address would have had to escape, which the private-frame gate
                // rules out.  Same gate as the `Call` relaxation.
                if self.private_frame_forward(function)
                    && matches!(store.class, AddrClass::Anchor { .. })
                {
                    return false;
                }
                // Anything but `Disjoint` clobbers; `load_forward` re-checks
                // for an exact `Match` afterward.  Same store derivation as
                // `verdict`, so the walk stops at exactly the stores that
                // re-check sees.
                self.analyzer.alias(self.load, store) != AliasVerdict::Disjoint
            }
            NodeKind::Call => {
                // A callee declared transparent to memory writes none, so the
                // load steps through whatever the frame analysis proves.
                if function.get_cc(def).preserves_memory {
                    return false;
                }
                // A private frame has no escaping stack address, so the callee
                // cannot name this slot and the load steps through, unless the
                // slot is one the ABI hands it, which privacy says nothing
                // about.
                if (self.private_frame_forward(function)
                    && !self.in_outgoing_arg_area(function, def))
                    || self.allocator_transparent(function, def)
                {
                    false
                } else {
                    self.analyzer.options.calls_block
                }
            }
            // `preserves_memory` is the per-user-op ABI attribute, read the
            // same way as a `Call`'s.  Nothing else lets the walk through: an
            // opaque user-op may write the stack without taking a frame
            // address, and `calls_block` speaks for a conforming callee's frame
            // discipline, which a syscall handed a pointer into the
            // incoming-argument area does not have.
            NodeKind::CallOther { .. } => !function.get_cc(def).preserves_memory,
            // No opaque memory producer can be proven disjoint.
            _ => true,
        }
    }
}

impl MemWalker<'_> {
    /// The shared gate for the private-frame forwarding relaxations: the knob is
    /// set, this probe is a slot of THIS function's frame, and no frame address
    /// escapes.  Under it no callee and no opaque pointer can name the slot, so
    /// both a `Call` and an `Anchor` store step through.  Assumes valid input (a
    /// fabricated pointer numerically equal to the slot is excluded).
    ///
    /// Says nothing about the outgoing-argument window, which the ABI hands to
    /// the callee: pair it with [`Self::in_outgoing_arg_area`] at a `Call`.
    fn private_frame_forward(&self, function: &Function) -> bool {
        if let Some(cached) = self.private_frame.get() {
            return cached;
        }
        let verdict = match self.load.class {
            AddrClass::StackRooted { base, offset } => {
                self.analyzer.options.call_relaxations
                    && self.analyzer.options.escape_analysis
                    && in_own_frame(function, base, offset, self.load.size)
                    && !frame_escape::frame_address_escapes_cached(function)
            }
            _ => false,
        };
        self.private_frame.set(Some(verdict));
        verdict
    }

    /// Does the probed slot fall in the region `call` hands its callee?  A
    /// private frame does NOT cover it: the caller writes arguments there and
    /// the callee owns them, so it may write them back.
    ///
    /// The region is everything below `call_sp + base_offset` (the callee's
    /// own frame under the call's SP plus the ABI-reserved area above it: x86's
    /// return-address slot, PPC64's linkage area, MIPS o32's 16 bytes, Windows
    /// x64's home space), and above that the stack-argument window
    /// `[call_sp + base_offset, call_sp + offset_of(k))`, `k` being the
    /// contiguous prefix of arg slots with a store reaching the call, the same
    /// prefix [`crate::post_opt::call_stack_args`] collects.
    ///
    /// `k` proxies the callee's declared argument count, which needs a
    /// signature this crate does not have.  Once lowered to memory an argument
    /// push is indistinguishable from an incidental in-window write, so `k`
    /// errs WIDE: a spill just above the arguments blocks a forward that would
    /// have been fine.
    ///
    /// [`crate::AssumptionOptions::callee_preserves_stack_args`] empties the
    /// window instead, dropping `k` and everything above `base_offset`.
    ///
    /// Erring NARROW would be unsound: a gap ends the window, and the inner
    /// probe stops at the first `Call`, so an argument store hidden behind an
    /// EARLIER call would end the prefix at that call and let a load from a
    /// slot the NEXT callee owns forward across that callee.  A def the probe
    /// cannot see through therefore CONTINUES the window
    /// ([`SlotReach::Blinded`]).
    ///
    /// Runs from inside a memory-SSA walk and starts one of its own, so the
    /// inner walk drops every `Call` relaxation: keeping them would re-enter
    /// this method at every earlier `Call`, one fresh walk each, for
    /// `T(k) = sum(T(j), j < k)`, exponential, and recurse without bound
    /// through a loop-header back-edge.  Relaxation-free, the inner walk stops
    /// at the first `Call`, so the argument prefix ends there.
    ///
    /// The window itself is a property of the call, so the prefix walk is
    /// memoised on the analyzer ([`ArgWindow`]) and every later probe of the
    /// same call is an interval test.
    fn in_outgoing_arg_area(&self, function: &Function, call: NodeId) -> bool {
        let AddrClass::StackRooted {
            base: load_base,
            offset: load_off,
        } = self.load.class
        else {
            return false;
        };
        let stack_args = function.get_cc(call).stack_args;
        // Call inputs are [control, memory, target, sp, ...args].
        let sp_value = function.node_inputs(call)[3];
        let Some(MemExpr {
            base: call_base,
            offset: call_sp_off,
            kind: MemKind::Stack,
        }) = self.analyzer.decompose(function, sp_value)
        else {
            // The call's SP is unknown, so the window cannot be placed and any
            // slot might be in it.  A non-stack SP is not a coordinate system
            // the probed slot shares.
            return true;
        };
        // Offsets are only comparable within one base.
        if call_base != load_base {
            return true;
        }
        // Below the first argument slot is the callee's frame and the reserved
        // area, both of which it may scratch.
        let window_lo = call_sp_off + stack_args.map_or(0, |s| s.base_offset);
        if load_off < window_lo {
            return true;
        }
        // The relaxation empties the argument window, leaving only the
        // reserved area below it callee-writable.
        if self.analyzer.options.callee_preserves_stack_args {
            return false;
        }
        // Nothing declared above the reserved area, so nothing left to overlap.
        let Some(stack_args) = stack_args else {
            return false;
        };
        let Some(mem_value) = function.memory_input_of(call) else {
            return true;
        };
        let window_hi = load_off + self.load.size;
        let geometry = ArgWindowGeometry {
            mem_start: mem_value,
            base: call_base,
            lo: window_lo,
            sp_offset: call_sp_off,
            args: stack_args,
        };
        self.analyzer
            .arg_window_covers(function, call, &geometry, load_off, window_hi)
    }

    /// A pure allocator writes only its own fresh allocation (plus internal
    /// bookkeeping the caller cannot name), so it is transparent to a probe with
    /// a known base other than this call's return.  A stack slot and a *different*
    /// heap object step through; the call's own allocation, a global (the
    /// allocator's private state may live there), or an opaque pointer do not.
    fn allocator_transparent(&self, function: &Function, call: NodeId) -> bool {
        if !self.analyzer.options.call_relaxations {
            return false;
        }
        let Some(ret_base) = allocator_return_base(function, call) else {
            return false;
        };
        match self.load.class {
            // An allocator is still a callee, and the ABI hands it the
            // outgoing-argument area, which it may scratch.
            AddrClass::StackRooted { .. } => !self.in_outgoing_arg_area(function, call),
            AddrClass::HeapRooted { base, .. } => base != ret_base,
            // The unknown allocation may be this call's own.
            AddrClass::HeapOpaque | AddrClass::Constant { .. } | AddrClass::Anchor { .. } => false,
        }
    }
}

/// The callee-owned region of one call's outgoing-argument window, in the
/// call's SP base coordinates.  Read off the call alone, so one walk of the
/// argument prefix answers every probe against that call.
pub(crate) struct ArgWindow {
    /// `[start, end)` of each anchored argument slot, ascending and
    /// non-overlapping.
    ranges: Vec<(i128, i128)>,
    /// The slot the prefix could not be shown to end at, from which the callee
    /// keeps everything above ([`SlotReach::Blinded`]).
    blind_at: Option<i128>,
    /// How far the scan looked.  A probe reaching above this is outside what
    /// the walk decided and needs a wider one.
    known_to: i128,
}

impl ArgWindow {
    /// Does `[offset, hi)` meet the owned region?  Mirrors the prefix walk:
    /// the first slot ending above `offset` decides, and a slot at or above
    /// `hi` is one the probe never reaches.
    fn covers(&self, offset: i128, hi: i128) -> bool {
        match self
            .ranges
            .get(self.ranges.partition_point(|&(_, end)| end <= offset))
        {
            Some(&(start, _)) => start < hi,
            None => self.blind_at.is_some_and(|slot| slot < hi),
        }
    }
}

/// Where a call's argument window sits, all of it read off the call.
struct ArgWindowGeometry {
    /// The call's memory input, the chain the prefix is scanned along.
    mem_start: ValueId,
    base: ValueId,
    /// First argument slot.
    lo: i128,
    /// SP at the call, the origin [`strider_target::StackArgs`] measures from.
    sp_offset: i128,
    args: strider_target::StackArgs,
}

/// Walks the contiguous prefix of argument slots up to `hi`, the reach the
/// probing load needs.  A wider `hi` only ever ends the prefix sooner (a store
/// above the narrow window is one more def the scan can stop at), so a window
/// scanned for one probe answers a shallower one conservatively.
fn scan_arg_window(
    function: &Function,
    options: MemOptions,
    geometry: &ArgWindowGeometry,
    hi: i128,
) -> ArgWindow {
    let mut scan = ArgStoreScan::new(
        options.without_call_relaxations(),
        geometry.mem_start,
        geometry.base,
        geometry.lo,
        hi,
    );
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = geometry.sp_offset + geometry.args.offset_of(cursor);
        if slot_off >= hi {
            return ArgWindow {
                ranges,
                blind_at: None,
                known_to: hi,
            };
        }
        let store = match scan.reach_at(function, slot_off) {
            SlotReach::Anchored(hit) => hit,
            // Nothing the caller wrote as an argument reaches the slot, so the
            // argument list ends below it.
            //
            // KNOWN LIMIT: an argument whose alignment exceeds the slot stride
            // leaves a hole the psABI still hands the callee, and this reads
            // that hole as the end. Continuing past it would blind the window
            // from the hole up, which is indistinguishable from a spill sitting
            // above the arguments and would stop forwarding for every one of
            // those. Only the callee's arity separates the two, and the IR does
            // not have it. Reachable only under `escape_analysis` or a
            // non-empty `noalias_allocators`, both off by default.
            SlotReach::NotAnArgument => {
                return ArgWindow {
                    ranges,
                    blind_at: None,
                    known_to: i128::MAX,
                };
            }
            // The window cannot be shown to end here, so the callee keeps
            // ownership all the way up.
            SlotReach::Blinded => {
                return ArgWindow {
                    ranges,
                    blind_at: Some(slot_off),
                    known_to: i128::MAX,
                };
            }
        };
        let size = store.size(function);
        ranges.push((slot_off, slot_off.saturating_add(size)));
        cursor += geometry.args.slots_spanned(size);
    }
}

/// One lazy reverse scan of a call's memory chain for the stores anchoring its
/// outgoing-argument slots, replacing a rescan per slot.
///
/// The scan probes the whole window at once and resumes past each store it
/// finds, so the scanned segments partition the chain: every slot together
/// costs one traversal, where a probe per slot costs one each.
///
/// A per-slot probe reaches the same defs.  Every def kind but `Store` and
/// `MemPhi` clobbers regardless of the probed range, and a `Store` disjoint
/// from the whole window is disjoint from every slot in it, so a def the
/// window probe skips no slot probe would stop at.  `MemPhi` transparency does
/// depend on the probed slot, so a slot left uncovered when the scan stops at
/// one falls back to its own probe.
pub(crate) struct ArgStoreScan {
    probe: MemAnalyzer,
    /// Chain start, for the `MemPhi` fallback probe.
    mem_start: ValueId,
    base: ValueId,
    /// The window `[lo, lo + size)` as one probe.
    lo: i128,
    size: i128,
    /// Where the next resume starts, `None` once the scan has stopped.
    cursor: Option<ValueId>,
    /// Merged byte ranges of the stores scanned so far.
    covered: BTreeMap<i128, i128>,
    /// Anchor offset -> the store anchored there, for the anchors no nearer
    /// store already covers.
    anchored: FxHashMap<i128, ReachingSpStore>,
    /// A loop back-edge can put an already-scanned store back on the chain
    /// below itself, which would make the resume loop forever.
    seen: FxHashSet<NodeId>,
    /// The def that ended the scan: the one reaching every slot the scan left
    /// uncovered, and so the reason the prefix ends where it does.
    stopped_at: Option<NodeId>,
}

impl ArgStoreScan {
    pub(crate) fn new(
        options: MemOptions,
        mem_start: ValueId,
        base: ValueId,
        lo: i128,
        hi: i128,
    ) -> Self {
        Self {
            probe: MemAnalyzer::new(options),
            mem_start,
            base,
            lo,
            size: hi.saturating_sub(lo),
            cursor: Some(mem_start),
            covered: BTreeMap::new(),
            anchored: FxHashMap::default(),
            seen: FxHashSet::default(),
            stopped_at: None,
        }
    }

    /// What reaches `slot_off` on the call's memory chain.  Scans further along
    /// it only while what is scanned leaves `slot_off` uncovered.
    pub(crate) fn reach_at(&mut self, function: &Function, slot_off: i128) -> SlotReach {
        while !range_covers(&self.covered, slot_off) {
            if self.advance(function) {
                continue;
            }
            let Some(def) = self.stopped_at else {
                return SlotReach::Blinded;
            };
            if !matches!(function.node_kind(def), NodeKind::MemPhi) {
                return classify_reaching_def(function, def);
            }
            // A `MemPhi` clobbers the probe that met it, not necessarily this
            // slot: its arms can still agree on this one, which only the slot's
            // own probe can say.
            let def =
                self.probe
                    .nearest_sp_clobber(function, self.mem_start, self.base, slot_off, 1);
            return match self.probe.anchored_store(function, def, self.base) {
                Some(hit) if hit.store_offset == slot_off => SlotReach::Anchored(hit),
                // Anchored below the slot: it covers the slot without the
                // caller ever having written the slot itself.
                Some(_) => SlotReach::NotAnArgument,
                None => classify_reaching_def(function, def),
            };
        }
        match self.anchored.get(&slot_off) {
            Some(hit) => SlotReach::Anchored(*hit),
            // A store anchored below covers the slot, so the slot was not
            // written as a slot.
            None => SlotReach::NotAnArgument,
        }
    }

    /// Resumes the scan at the next store, `false` once it stops.
    fn advance(&mut self, function: &Function) -> bool {
        let Some(cur) = self.cursor.take() else {
            return false;
        };
        let def = self
            .probe
            .nearest_sp_clobber(function, cur, self.base, self.lo, self.size);
        if !self.seen.insert(def) {
            self.stopped_at = Some(def);
            return false;
        }
        let Some(hit) = self.probe.anchored_store(function, def, self.base) else {
            self.stopped_at = Some(def);
            return false;
        };
        // A store an already-scanned one covers is not the nearest reaching
        // def of its own anchor, so it must not claim that anchor.
        if !range_covers(&self.covered, hit.store_offset) {
            self.anchored.insert(hit.store_offset, hit);
        }
        add_range(
            &mut self.covered,
            hit.store_offset,
            hit.store_offset.saturating_add(hit.size(function)),
        );
        self.cursor = function.memory_input_of(def);
        true
    }
}

/// What reaches one argument slot on a call's memory chain.
pub(crate) enum SlotReach {
    /// A store anchored exactly at the slot.
    Anchored(ReachingSpStore),
    /// A visible def that is not a store anchored at the slot, so the caller
    /// did not write it as an argument.
    NotAnArgument,
    /// A def the argument probe cannot see through, leaving it unknown whether
    /// the caller wrote the slot.  Hidden is not absent.
    Blinded,
}

fn classify_reaching_def(function: &Function, def: NodeId) -> SlotReach {
    match *function.node_kind(def) {
        // The clean bottom of the chain.
        NodeKind::InitialMemory => SlotReach::NotAnArgument,
        // A `Call` / `CallOther` hides the caller's own argument stores from a
        // relaxation-free probe, an opaque producer hides everything, a
        // `MemPhi` reaching here has arms that disagree about the slot, and a
        // `Store` reaching here is one the scan could not place (its address
        // does not decompose to this base, or a back-edge put it back on the
        // chain), so it hides whatever lies below it.
        _ => SlotReach::Blinded,
    }
}

/// Adds `[start, end)` to a set of disjoint, non-touching `start -> end`
/// intervals, absorbing every interval it meets.
fn add_range(set: &mut BTreeMap<i128, i128>, start: i128, end: i128) {
    if end <= start {
        return;
    }
    let (mut start, mut end) = (start, end);
    if let Some((&s, &e)) = set.range(..=start).next_back()
        && e >= start
    {
        start = s;
        end = end.max(e);
    }
    while let Some((&s, &e)) = set.range(start..=end).next() {
        set.remove(&s);
        end = end.max(e);
    }
    set.insert(start, end);
}

fn range_covers(set: &BTreeMap<i128, i128>, point: i128) -> bool {
    set.range(..=point)
        .next_back()
        .is_some_and(|(_, &e)| e > point)
}

/// Does the access `[base + offset, base + offset + size)` lie wholly in THIS
/// function's own frame?
///
/// [`frame_escape`] proves only that no address of this frame escaped from this
/// function's BODY.  At the entry SP plus the ABI's stack-argument base and
/// above lies the CALLER's outgoing-argument block, which the caller may hold or
/// have leaked a pointer into; nothing here can reveal that, so a callee must be
/// assumed able to write it.  Below that bound is this frame plus the reserved
/// area the ABI gives the callee to scratch.
///
/// `base_offset` is the first incoming slot on every supported ABI and
/// `validate` rejects a negative one, so the stack always grows away from it:
/// x86-64 8, i386 4, ARM / AArch64 / MIPS n64 0, MIPS o32 16, PPC32 8,
/// PPC64 ELFv1 112 / ELFv2 96 (linkage area plus the 64-byte parameter
/// save area).  An ABI declaring none bounds at 0, since the
/// caller's frame still starts at the entry SP.
///
/// The comparison is in the entry SP's coordinates, so the base has to be AT OR
/// BELOW it. `InitialVar(sp)` is exactly it, and masking only clears bits, but
/// the alignment anchor is a mask of whatever SP-rooted expression the spine was
/// on, so `(sp + K) & !0xF` sits K bytes ABOVE the entry SP, inside the caller's
/// frame. The operand is therefore walked down to `InitialVar(sp)` and its
/// accumulated displacement required to be non-positive.
fn in_own_frame(function: &Function, base: ValueId, offset: i128, size: i128) -> bool {
    let base_node = function.producer(base);
    let rooted_at_entry_sp = match *function.node_kind(base_node) {
        NodeKind::InitialVar(id) => function.initial_vn(id) == function.default_cc().stack_vn,
        _ => alignment_masked_operand(function, base_node)
            .is_some_and(|operand| at_or_below_entry_sp(function, operand, 0)),
    };
    // The END of the access, not its start: one that begins below the bound and
    // reaches over it touches the caller's block, which a callee may write.
    rooted_at_entry_sp
        && offset.saturating_add(size)
            <= function
                .default_cc()
                .stack_args
                .map_or(0, |args| args.base_offset)
}

/// Whether `value` is the entry SP displaced by a non-positive amount.
///
/// Follows the spine shapes [`decompose`] does (`Add(x, const)` and an
/// alignment `And`), accumulating the constants an anchor would otherwise
/// discard, and through a `Phi`, where EVERY arm must qualify because the join
/// could take any of them. The accumulated displacement is compared at the
/// address width, so a chain that wraps is judged on the address it really
/// forms. Anything else, a positive displacement, a revisited value (a
/// loop-carried base) or an exhausted budget answers `false`, which only ever
/// costs precision.
fn at_or_below_entry_sp(function: &Function, value: ValueId, acc: i128) -> bool {
    /// A real SP spine is a handful of links; this only bounds a pathological one.
    const MAX_STEPS: usize = 256;
    let sp = function.default_cc().stack_vn;
    let mut work = vec![(value, acc)];
    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut steps = 0usize;
    while let Some((cur, acc)) = work.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return false;
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(id) => {
                // Compare at the ADDRESS width, as `decompose` does: a raw
                // `i128` sum can be non-positive while the address it stands
                // for wrapped back above the entry SP, into the caller's frame.
                if function.initial_vn(id) != sp || wrap_to_addr_width(function, cur, acc) > 0 {
                    return false;
                }
            }
            // Masking only clears bits, so it can only move the base DOWN.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let Some(operand) = alignment_masked_operand(function, node) else {
                    return false;
                };
                work.push((operand, acc));
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let [l, r] = binary_operands(function, node);
                let (next, konst) = match (function.int_const_i128(l), function.int_const_i128(r)) {
                    (_, Some(k)) => (l, k),
                    (Some(k), _) => (r, k),
                    _ => return false,
                };
                let Some(sum) = acc.checked_add(konst) else {
                    return false;
                };
                work.push((next, sum));
            }
            NodeKind::Phi => {
                // A revisited value is a loop-carried base: its displacement is
                // not a constant, so it cannot be shown non-positive.
                if !seen.insert(cur) {
                    return false;
                }
                for arm in function.int_inputs(cur) {
                    work.push((arm, acc));
                }
            }
            _ => return false,
        }
    }
    true
}

/// The heap base a `Call` returns if it is a known pure allocator, else `None`.
fn allocator_return_base(function: &Function, call: NodeId) -> Option<ValueId> {
    if !matches!(function.node_kind(call), NodeKind::Call) {
        return None;
    }
    let ret = *function.node_outputs(call).get(2)?;
    is_allocator_return(function, ret, call).then_some(ret)
}

#[derive(Clone, Copy)]
pub(crate) struct MemOptions {
    stack_global_disjoint: bool,
    /// Whether a `Call` / `CallOther` on the probed location's memory chain
    /// shadows it.
    calls_block: bool,
    distinct_sp_bases_disjoint: bool,
    /// Read only where the outgoing-argument window is consulted, so it is
    /// inert unless [`Self::escape_analysis`] or a non-empty allocator set
    /// opens that path.
    callee_preserves_stack_args: bool,
    /// Let an SP-rooted `Load` step through a `Call` when the frame is provably
    /// private ([`frame_escape`]).
    escape_analysis: bool,
    /// Whether any `Call` relaxation applies at all.  Cleared for the inner
    /// walk that computes a call's outgoing-argument window
    /// ([`MemWalker::in_outgoing_arg_area`]), which the relaxations would
    /// otherwise re-enter.
    call_relaxations: bool,
}

impl MemOptions {
    /// A `Call` on the memory chain clobbers the probed location, and
    /// distinct SP bases stay conservatively non-disjoint.
    pub(crate) fn call_blocking(stack_global_disjoint: bool) -> Self {
        Self {
            stack_global_disjoint,
            calls_block: true,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            escape_analysis: false,
            call_relaxations: true,
        }
    }

    /// The incoming-stack-argument probe: the knobs scoped to argument
    /// detection, and no private-frame relaxation.
    pub(crate) fn incoming_args(stack_global_disjoint: bool, options: &OptOptions) -> Self {
        Self {
            calls_block: !options.assumptions.assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint: options.assumptions.distinct_sp_bases_disjoint,
            callee_preserves_stack_args: options.assumptions.callee_preserves_stack_args,
            ..Self::call_blocking(stack_global_disjoint)
        }
    }

    /// Opt into forwarding an SP-rooted load across a private-frame `Call`.
    pub(crate) fn with_escape_analysis(mut self, enabled: bool) -> Self {
        self.escape_analysis = enabled;
        self
    }

    /// Opt into an empty outgoing-argument window
    /// ([`crate::AssumptionOptions::callee_preserves_stack_args`]).
    pub(crate) fn with_callee_preserves_stack_args(mut self, enabled: bool) -> Self {
        self.callee_preserves_stack_args = enabled;
        self
    }

    fn without_call_relaxations(mut self) -> Self {
        self.call_relaxations = false;
        self
    }
}

/// The SP-aware query surface.  Takes the `&Function` per call rather than
/// binding it, so a query may be interleaved with `&mut` edits.
pub(crate) struct MemAnalyzer {
    options: MemOptions,
    /// One [`ArgWindow`] per `Call`, scanned on first use.  Scoped to the
    /// analyzer, so its owner decides how long the memo may outlive the graph
    /// state it was read from.
    arg_windows: std::cell::RefCell<FxHashMap<NodeId, ArgWindow>>,
}

impl MemAnalyzer {
    pub(crate) fn new(options: MemOptions) -> Self {
        Self {
            options,
            arg_windows: std::cell::RefCell::default(),
        }
    }

    /// [`ArgWindow::covers`] against the memo, scanning `call`'s window on a
    /// miss.  A memoised window that did not reach `hi` is rescanned.
    fn arg_window_covers(
        &self,
        function: &Function,
        call: NodeId,
        geometry: &ArgWindowGeometry,
        offset: i128,
        hi: i128,
    ) -> bool {
        let cached = self
            .arg_windows
            .borrow()
            .get(&call)
            .filter(|window| hi <= window.known_to)
            .map(|window| window.covers(offset, hi));
        if let Some(covered) = cached {
            return covered;
        }
        // Scan PAST the request when a narrower window is already cached, so an
        // ascending run of probes stops missing on every step: the width
        // scanned above `lo` doubles, so one call rescans O(log range) times
        // instead of once per probe. The growth is measured from `lo` because
        // `known_to` is an absolute stack offset, normally negative. A window
        // scanned further is a superset, the same equivalence the
        // `hi <= known_to` reuse above already relies on.
        let scan_hi = match self.arg_windows.borrow().get(&call).map(|w| w.known_to) {
            Some(prev) if prev > geometry.lo => hi.max(
                geometry
                    .lo
                    .saturating_add(prev.saturating_sub(geometry.lo).saturating_mul(2)),
            ),
            _ => hi,
        };
        let window = scan_arg_window(function, self.options, geometry, scan_hi);
        let covered = window.covers(offset, hi);
        // Keep whichever entry reaches FURTHER. Overwriting with a narrower
        // `known_to` makes a later, higher probe miss and rescan the whole
        // prefix.
        //
        // Descending probes reuse the first window outright; ascending ones
        // rescan, but against the doubled `scan_hi` above rather than their own
        // `hi`, so the misses are logarithmic in the range rather than one per
        // probe.
        let mut memo = self.arg_windows.borrow_mut();
        match memo.get(&call) {
            Some(prev) if prev.known_to >= window.known_to => {}
            _ => {
                memo.insert(call, window);
            }
        }
        covered
    }

    pub(crate) fn options(&self) -> MemOptions {
        self.options
    }

    fn walker(&self, load: SizedAddr, load_space: rsleigh::VnSpace) -> MemWalker<'_> {
        MemWalker {
            analyzer: self,
            load,
            load_space,
            private_frame: std::cell::Cell::new(None),
        }
    }

    /// The one place the knobs meet [`alias_verdict`].
    fn alias(&self, load: SizedAddr, store: SizedAddr) -> AliasVerdict {
        alias_verdict(load, store, self.options)
    }

    fn load_sized(&self, function: &Function, load: NodeId) -> SizedAddr {
        let class = classify_addr(function, function.load_addr(load));
        let (_, ty) = function
            .single_value_output(load)
            .expect("Load has 1 typed output per node signature");
        SizedAddr {
            class,
            size: ty.byte_size() as i128,
            addr_bits: addr_bit_width(function, function.load_addr(load)),
        }
    }

    /// The store's address class paired with its stored width.
    fn store_sized(&self, function: &Function, store: NodeId) -> SizedAddr {
        SizedAddr {
            class: classify_store_addr(function, store),
            size: store_value_byte_size(function, function.store_data(store)),
            addr_bits: addr_bit_width(function, function.store_addr(store)),
        }
    }

    /// See the free [`decompose`].
    pub(crate) fn decompose(&self, function: &Function, value: ValueId) -> Option<MemExpr> {
        decompose(function, value)
    }

    /// The node-based counterpart of the class-based [`alias_verdict`].
    ///
    /// Distinct address spaces never alias, even at the same numeric offset,
    /// so a cross-space pair is `Disjoint` before any offset is compared.
    /// [`def_clobbers`](MemWalker::def_clobbers) applies the same rule while
    /// walking; a caller re-checking one pair has to apply it too.
    pub(crate) fn verdict(
        &self,
        function: &Function,
        load_node: NodeId,
        store_node: NodeId,
    ) -> AliasVerdict {
        if let (NodeKind::Load(load_space), NodeKind::Store(store_space)) = (
            function.node_kind(load_node),
            function.node_kind(store_node),
        ) && load_space != store_space
        {
            return AliasVerdict::Disjoint;
        }
        self.alias(
            self.load_sized(function, load_node),
            self.store_sized(function, store_node),
        )
    }

    /// Read-only; [`crate::mem_ssa::narrow_load_to`] does the rewire.
    pub(crate) fn nearest_clobber(
        &self,
        function: &Function,
        load: NodeId,
        mem: ValueId,
    ) -> NodeId {
        let NodeKind::Load(load_space) = *function.node_kind(load) else {
            unreachable!("nearest_clobber is only called on Load nodes");
        };
        let load = self.load_sized(function, load);
        let mem_node = function.producer(mem);
        let mut walker = self.walker(load, load_space);
        walker.find_nearest_clobber(function, mem_node)
    }

    /// The nearest `Store` covering `[offset, offset + probe_size)` relative to
    /// SP terminal `base`, or `None` when the nearest covering def is anything
    /// else (a `Call`, a disagreeing `MemPhi`, `InitialMemory`, an opaque
    /// producer, or a store rooted at a different SP base).
    ///
    /// `probe_size` is the caller's call: pass the access width to catch a
    /// partial tail-overlap as a clobber, or `1` to merely discover a store and
    /// read its natural width back off `size`.
    pub(crate) fn reaching_store(
        &self,
        function: &Function,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
    ) -> Option<ReachingSpStore> {
        let clobber = self.nearest_sp_clobber(function, mem_start, base, offset, probe_size);
        self.anchored_store(function, clobber, base)
    }

    /// The nearest def of `[offset, offset + probe_size)` relative to SP
    /// terminal `base` on the chain from `mem_start`.
    fn nearest_sp_clobber(
        &self,
        function: &Function,
        mem_start: ValueId,
        base: ValueId,
        offset: i128,
        probe_size: i128,
    ) -> NodeId {
        // Stack memory lives in RAM, so a same-address store in another space
        // counts as disjoint and is skipped.
        let mut walker = self.walker(
            SizedAddr {
                class: AddrClass::StackRooted { base, offset },
                size: probe_size,
                // `offset` came from `decompose`, so it is already reduced at
                // the base's width. Passing `None` would make
                // `offsets_comparable` reject every pair.
                addr_bits: addr_bit_width(function, base),
            },
            rsleigh::VnSpace::RAM,
        );
        walker.find_nearest_clobber(function, function.producer(mem_start))
    }

    /// `clobber` read as a `Store` whose own SP offset shares `base`, the only
    /// shape comparable to a probed location.
    fn anchored_store(
        &self,
        function: &Function,
        clobber: NodeId,
        base: ValueId,
    ) -> Option<ReachingSpStore> {
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }
        let MemExpr {
            base: store_base,
            offset: store_offset,
            ..
        } = decompose(function, function.store_addr(clobber))?;
        (store_base == base).then_some(ReachingSpStore {
            node: clobber,
            store_offset,
        })
    }
}

/// Result of [`MemAnalyzer::reaching_store`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReachingSpStore {
    node: NodeId,
    /// Byte offset from `base`.  Equals the probed `offset` only when the
    /// store is anchored at the probed location.
    pub store_offset: i128,
}

impl ReachingSpStore {
    pub(crate) fn data(&self, function: &Function) -> ValueId {
        function.store_data(self.node)
    }

    /// Byte width of the stored value.
    pub(crate) fn size(&self, function: &Function) -> i128 {
        store_value_byte_size(function, self.data(function))
    }
}

/// Right-shift in bits that brings the `load_ty`-width slice of a wider
/// `store_ty` integer into the low end.  Little-endian keeps the load bytes
/// low already, so the shift is 0.
///
/// `load_ty` must be no wider than `store_ty`, else the subtraction underflows.
#[inline]
pub(crate) fn high_low_shift_bits(
    store_ty: ValueType,
    load_ty: ValueType,
    endianness: Endianness,
) -> u32 {
    match endianness {
        Endianness::Little => 0,
        Endianness::Big => (store_ty.byte_size() - load_ty.byte_size()) as u32 * 8,
    }
}

/// Byte width of a `Store`'s DATA operand.  Panics on a non-value operand,
/// which the IR signature already rules out.
#[inline]
pub(crate) fn store_value_byte_size(function: &Function, store_data: ValueId) -> i128 {
    function
        .value_type(store_data)
        .expect("Store data input is a value")
        .byte_size() as i128
}

/// The 4-byte x86 SP varnode the tests in this module tree share. `heap_tests`
/// keeps its own 8-byte one, which is a different fixture, not a duplicate.
#[cfg(test)]
pub(crate) fn test_sp() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    }
}

#[cfg(test)]
mod tests;
