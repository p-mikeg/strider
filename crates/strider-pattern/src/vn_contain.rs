//! Pairwise varnode containment for explicit-varnode pattern predicates.
//!
//! The IR only ever stores the largest tracked container for an aliasing chain
//! (register aliasing is resolved at lift time). So a pattern that pins an
//! explicit varnode — `initial_var_for(vn)` / `phi(..).for_vn(vn)` — must match
//! when the node's stored container *encloses* the pinned varnode, not only when
//! they are byte-for-byte equal (pinning `eax` should match `InitialVar(rax)`).
//!
//! This is pure geometry on two varnodes: no tracked-set / universe is needed
//! because the node already carries its container. That is deliberately NOT the
//! lifter's `container_of` (which resolves an arbitrary varnode *into* a tracked
//! universe) — container resolution lives only in the lifter; this is a local
//! matching check the pattern crate owns.

/// True when `outer` fully encloses `inner` within the same aliasable space.
pub(crate) fn vn_contains(outer: &rsleigh::Vn, inner: &rsleigh::Vn) -> bool {
    outer.addr_space == inner.addr_space
        && outer.addr_off <= inner.addr_off
        && outer.addr_off.saturating_add(u64::from(outer.size))
            >= inner.addr_off.saturating_add(u64::from(inner.size))
}
