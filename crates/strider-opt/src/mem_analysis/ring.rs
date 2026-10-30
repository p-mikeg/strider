//! Address-width arithmetic: offsets wrap at the address's bit width.  Where
//! the width is unknown or past the `i128` carrier, a reduction leaves the
//! offset as it is and an overlap test answers `true`; at or past the carrier,
//! a split leaves the range whole.

use strider_ir::eval::mask_below;
use strider_ir::node::{ValueId, ValueType};
use strider_ir::{Function, IRViewer};

/// Bit width of an address expression.  Panics on a non-value edge, which the
/// node signature of every address operand already rules out.
pub(super) fn addr_bit_width(function: &Function, addr: ValueId) -> u32 {
    let ty = function
        .value_type(addr)
        .expect("an address operand is a value edge");
    u32::try_from(ty.bit_width()).expect("a ValueType width is a u16")
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

/// `[start, end)` as at most two ranges inside the signed range `ty` reduces
/// offsets into: address arithmetic wraps, so a range running past the top
/// resumes at the bottom.  `start` is reduced, `end` need not be.
pub(super) fn wrapped_ranges(ty: Option<ValueType>, start: i128, end: i128) -> [(i128, i128); 2] {
    let whole = [(start, end), (0, 0)];
    let Some(bits) = ty.map(ValueType::bit_width).filter(|&bits| bits < 127) else {
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
/// distance: [`super::alias::offsets_comparable`] says why, and which classes
/// take the ring instead.
pub(super) fn comparable_distance(bits: u32, span: i128) -> u128 {
    (1i128 << (bits - 1))
        .saturating_sub(span)
        .max(0)
        .unsigned_abs()
}

/// `v` sign-extended from bit `bits`, i.e. reduced into the signed range of
/// that width.  `bits` is below 128; at 128 the value already is its own
/// reduction.
fn sign_extend(bits: u32, v: i128) -> i128 {
    let shift = 128 - bits;
    (((v as u128) << shift) as i128) >> shift
}

/// Whether `[a, a + a_len)` and `[b, b + b_len)` share a byte mod `mask + 1`.
fn masked_overlap(mask: u128, a: i128, a_len: i128, b: i128, b_len: i128) -> bool {
    let ahead = |from: i128, to: i128| (to.wrapping_sub(from) as u128) & mask;
    ahead(a, b) < a_len.max(0) as u128 || ahead(b, a) < b_len.max(0) as u128
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
    // Offsets here are `i128`, so a modulus past that carrier cannot place
    // them apart; a float address has no modulus at all. Both answer "may
    // alias", the conservative direction.
    let Some(bits) = ty.map(ValueType::bit_width).filter(|&bits| bits <= 128) else {
        return true;
    };
    ring_overlap_at(bits as u32, a, a_len, b, b_len)
}

/// [`ring_overlap`] at an address width given as bits (at most 128), which is
/// what the alias verdict and the store index carry.
pub(super) fn ring_overlap_at(bits: u32, a: i128, a_len: i128, b: i128, b_len: i128) -> bool {
    masked_overlap(mask_below(bits), a, a_len, b, b_len)
}

/// The inclusive window `[lo, hi]` as the ranges of reduced offsets it covers,
/// at most two because it may wrap the top of the signed range.  `None` when
/// it wraps all the way round and names every offset.  `bits` is below 128,
/// where `1i128 << bits` still holds the modulus.
pub(super) fn ring_window(bits: u32, lo: i128, hi: i128) -> Option<[Option<(i128, i128)>; 2]> {
    debug_assert!(lo <= hi, "a window runs upwards");
    if (hi as u128).wrapping_sub(lo as u128) >= mask_below(bits) {
        return None;
    }
    let (lo, hi) = (sign_extend(bits, lo), sign_extend(bits, hi));
    Some(if lo <= hi {
        [Some((lo, hi)), None]
    } else {
        let top = 1i128 << (bits - 1);
        [Some((lo, top - 1)), Some((-top, hi))]
    })
}
