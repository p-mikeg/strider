//! Address-width arithmetic: offsets wrap at the address's bit width.

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

/// Machine address arithmetic is mod 2^bitwidth, so reduce an accumulated byte
/// offset into `addr`'s signed range.  Unreduced, `(sp + 0x7FFFFFFF) +
/// 0x7FFFFFFF` on a 32-bit target reads as `sp + 4294967294` instead of
/// `sp - 2`, Disjoint from the slot it actually names.  An address wider than
/// the i128 carrier keeps the raw sum.
pub(crate) fn wrap_to_addr_width(function: &Function, addr: ValueId, offset: i128) -> i128 {
    function
        .value_type(addr)
        .ok()
        .and_then(|ty| ty.get_signed_int(offset as u128))
        .unwrap_or(offset)
}

/// `[start, end)` as at most two ranges inside the signed range `ty` reduces
/// offsets into: address arithmetic wraps, so a range running past the top
/// resumes at the bottom.  `start` is reduced, `end` need not be.  A width
/// the reduction does not reach keeps the range whole.
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
