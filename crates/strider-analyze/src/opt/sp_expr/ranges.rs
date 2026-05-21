//! Range arithmetic shared by every SP-aware pass.

use strider_ir::node::NodeOutputId;
use strider_ir::Graph;

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` are
/// disjoint.
///
/// Endpoint computations use `saturating_add` so that callers passing
/// `size = i64::MAX` as a soundness-pessimistic fallback (e.g. when a Store's
/// `value_byte_size` is unknown) cannot panic in debug or wrap in release.
/// A saturated upper endpoint additionally short-circuits to "not disjoint"
/// — i.e. an unknown-extent range is treated as effectively infinite in both
/// directions, matching the conservative verdict callers expect.
#[inline]
#[must_use]
pub fn ranges_disjoint(a_off: i64, a_size: i64, b_off: i64, b_size: i64) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    // If either endpoint saturated, treat the corresponding range as
    // unbounded and report "not disjoint" — the conservative answer.
    if a_end == i64::MAX || b_end == i64::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Conservative byte size of a `Store`'s DATA slot, used as a range bound
/// for [`ranges_disjoint`].  Returns the value type's byte size when the
/// slot is value-typed (the IR signature guarantees this for any valid
/// `Store`); otherwise returns `i64::MAX` so callers' `ranges_disjoint`
/// checks fail closed (treat the unknown extent as effectively infinite,
/// the soundness-preserving verdict).
///
/// The fallback branch is unreachable in valid IR but exists as a
/// defensive guardrail — its rationale is duplicated across every caller
/// otherwise, so it lives here.
#[inline]
#[must_use]
pub(crate) fn store_value_byte_size(g: &Graph, store_data: NodeOutputId) -> i64 {
    g.output_kind(store_data)
        .as_value()
        .map_or(i64::MAX, |t| t.byte_size() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_disjoint_returns_true_for_non_overlapping() {
        // Adjacent ranges are disjoint (touching is fine).
        assert!(ranges_disjoint(0, 4, 4, 4));
        // Overlapping ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 2, 4));
        // Identical ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 0, 4));
        // Reverse order — equally disjoint.
        assert!(ranges_disjoint(4, 4, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_left_does_not_panic_and_is_conservative() {
        // The three memory-chain walkers (CallStackArgCollect,
        // stack_load_forward::probe, function_args::mem_chain_is_dirty)
        // pass `i64::MAX` as a soundness-pessimistic fallback when a Store's
        // `value_byte_size` is unknown. With plain `+`, `a_off + i64::MAX`
        // would panic in debug and wrap in release for any positive `a_off`.
        // ranges_disjoint must saturate cleanly and report "not disjoint"
        // (false) for any reachable load offset — the conservative verdict
        // callers depend on. SP-relative offsets in practice are small (kB
        // range), so we cover zero, modestly-negative, and modestly-positive
        // a_off values.
        assert!(!ranges_disjoint(0, i64::MAX, 100, 4));
        assert!(!ranges_disjoint(-1000, i64::MAX, 100, 4));
        assert!(!ranges_disjoint(1_000_000, i64::MAX, -1_000_000, 4));
        // Even very large positive a_off (where `a_off + i64::MAX` would
        // overflow without saturation) must not panic and must report
        // "not disjoint".
        assert!(!ranges_disjoint(1, i64::MAX, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_right_does_not_panic_and_is_conservative() {
        // Symmetric: i64::MAX on the b-side must also saturate and report
        // "not disjoint" without panicking.
        assert!(!ranges_disjoint(100, 4, 0, i64::MAX));
        assert!(!ranges_disjoint(100, 4, -1000, i64::MAX));
        assert!(!ranges_disjoint(-1_000_000, 4, 1_000_000, i64::MAX));
        assert!(!ranges_disjoint(0, 4, 1, i64::MAX));
    }
}
