//! Range arithmetic shared by every SP-aware pass.

use strider_ir::Graph;
use strider_ir::node::{ValueId, ValueType};
use strider_target::Endianness;

/// Bit shift that aligns the load-width sub-word of a wider stored value into
/// the low end before truncation, given the store/load types and byte order.
///
/// The single SSoT for the "extract the `load_ty`-width slice from a wider
/// `store_ty` integer" rule, shared by the `LoadForward` node-building
/// narrowing and the jump-table evaluator's symbolic `reshape`:
///
/// * Little-endian: the load bytes are the LOW bytes — no shift (`0`).
/// * Big-endian: the load bytes are the HIGH bytes — shift right by
///   `(store_bytes - load_bytes) * 8` so they land in the low end.
///
/// Returns the shift in bits.  Callers only invoke it when the load is
/// narrower than the store, so the byte-size subtraction does not underflow.
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

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` are
/// disjoint.
///
/// Endpoint computations use `saturating_add` so that callers passing
/// `size = i128::MAX` as a soundness-pessimistic fallback (e.g. when a Store's
/// `value_byte_size` is unknown) cannot panic in debug or wrap in release.
/// A saturated upper endpoint additionally short-circuits to "not disjoint"
/// — i.e. an unknown-extent range is treated as effectively infinite in both
/// directions, matching the conservative verdict callers expect.
#[inline]
pub(super) fn ranges_disjoint(a_off: i128, a_size: i128, b_off: i128, b_size: i128) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    // If either endpoint saturated, treat the corresponding range as
    // unbounded and report "not disjoint" — the conservative answer.
    if a_end == i128::MAX || b_end == i128::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Byte size of a `Store`'s DATA slot, used as a range bound for
/// [`ranges_disjoint`].  The IR signature guarantees the slot is
/// value-typed for any valid `Store` (`DATA` is an `AnyInt` slot), so a
/// non-value here means malformed IR and panics rather than silently
/// degrading the alias verdict.
#[inline]
pub(crate) fn store_value_byte_size(g: &Graph, store_data: ValueId) -> i128 {
    g.value_kind(store_data)
        .as_value()
        .expect("Store data input is a value")
        .byte_size() as i128
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
        // load_forward::probe, function_args::mem_chain_is_dirty)
        // pass `i128::MAX` as a soundness-pessimistic fallback when a Store's
        // `value_byte_size` is unknown. With plain `+`, `a_off + i128::MAX`
        // would panic in debug and wrap in release for any positive `a_off`.
        // ranges_disjoint must saturate cleanly and report "not disjoint"
        // (false) for any reachable load offset — the conservative verdict
        // callers depend on. SP-relative offsets in practice are small (kB
        // range), so we cover zero, modestly-negative, and modestly-positive
        // a_off values.
        assert!(!ranges_disjoint(0, i128::MAX, 100, 4));
        assert!(!ranges_disjoint(-1000, i128::MAX, 100, 4));
        assert!(!ranges_disjoint(1_000_000, i128::MAX, -1_000_000, 4));
        // Even very large positive a_off (where `a_off + i128::MAX` would
        // overflow without saturation) must not panic and must report
        // "not disjoint".
        assert!(!ranges_disjoint(1, i128::MAX, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_right_does_not_panic_and_is_conservative() {
        // Symmetric: i128::MAX on the b-side must also saturate and report
        // "not disjoint" without panicking.
        assert!(!ranges_disjoint(100, 4, 0, i128::MAX));
        assert!(!ranges_disjoint(100, 4, -1000, i128::MAX));
        assert!(!ranges_disjoint(-1_000_000, 4, 1_000_000, i128::MAX));
        assert!(!ranges_disjoint(0, 4, 1, i128::MAX));
    }
}
