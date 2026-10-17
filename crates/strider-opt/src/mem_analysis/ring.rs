//! Address-width arithmetic: offsets wrap at the address's bit width.  Where
//! the width is unknown or past the `i128` carrier, a reduction leaves the
//! offset as it is and an overlap test answers `true`; at or past the carrier,
//! a split leaves the range whole.

use strider_ir::node::{ValueId, ValueType};
use strider_ir::{Function, IRViewer};

/// Bit width of an address expression, `None` when `addr` is not a typed value
/// edge.
pub(super) fn addr_bit_width(function: &Function, addr: ValueId) -> Option<u32> {
    function
        .value_type(addr)
        .ok()
        .and_then(|ty| u32::try_from(ty.bit_width()).ok())
}

/// `offset` reduced into `ty`'s signed range: machine address arithmetic is
/// mod 2^bitwidth.
pub(crate) fn reduce(ty: Option<ValueType>, offset: i128) -> i128 {
    ty.and_then(|ty| ty.get_signed_int(offset as u128))
        .unwrap_or(offset)
}

/// [`reduce`] at `addr`'s width.  Unreduced, `(sp + 0x7FFFFFFF) +
/// 0x7FFFFFFF` on a 32-bit target reads as `sp + 4294967294` instead of
/// `sp - 2`, Disjoint from the slot it actually names.
pub(crate) fn wrap_to_addr_width(function: &Function, addr: ValueId, offset: i128) -> i128 {
    reduce(function.value_type(addr).ok(), offset)
}

/// Byte `byte` of an access at `offset`, reduced: bytes past the top of the
/// signed range are the bytes at its bottom.  `None` past the `i128` carrier.
pub(crate) fn slot_key(ty: Option<ValueType>, offset: i128, byte: i128) -> Option<i128> {
    offset.checked_add(byte).map(|at| reduce(ty, at))
}

/// `[start, end)` as at most two ranges inside the signed range `ty` reduces
/// offsets into: address arithmetic wraps, so a range running past the top
/// resumes at the bottom.  `start` is reduced, `end` need not be.
pub(super) fn wrapped_ranges(ty: Option<ValueType>, start: i128, end: i128) -> [(i128, i128); 2] {
    let whole = [(start, end), (0, 0)];
    let Some(bits) = ty
        .filter(|ty| ty.is_integer())
        .map(ValueType::bit_width)
        .filter(|&bits| bits < 127)
    else {
        return whole;
    };
    let top = 1i128 << (bits - 1);
    if end <= top {
        return whole;
    }
    let modulus = top << 1;
    // A range wider than the circle covers all of it.
    let end = end.min(start + modulus);
    [(start, top), (-top, end - modulus)]
}

/// How far apart two offsets reduced at `bits` (below 128) may lie and still
/// compare in integer order, for accesses of up to `span` bytes: half the
/// modulus less the span.  A span past the half-modulus leaves no comparable
/// distance, so the bound floors at zero.  Not [`ring_overlap`]'s modular
/// distance: [`super::alias::offsets_comparable`] says why.
pub(super) fn comparable_distance(bits: u32, span: i128) -> u128 {
    (1i128 << (bits - 1))
        .saturating_sub(span)
        .max(0)
        .unsigned_abs()
}

/// Whether `[a, a + a_len)` and `[b, b + b_len)` share a byte mod 2^w, `w` the
/// width of `ty`.
pub(crate) fn ring_overlap(
    ty: Option<ValueType>,
    a: i128,
    a_len: i128,
    b: i128,
    b_len: i128,
) -> bool {
    let Some(mask) = ty.and_then(ValueType::int_mask_u128) else {
        return true;
    };
    let ahead = |from: i128, to: i128| (to.wrapping_sub(from) as u128) & mask;
    ahead(a, b) < a_len.max(0) as u128 || ahead(b, a) < b_len.max(0) as u128
}
