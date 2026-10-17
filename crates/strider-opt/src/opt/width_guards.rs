//! Width preconditions of the rewrite rules that compute a constant.

use strider_ir::Function;
use strider_ir::node::ValueType;
use strider_pattern::{Bindings, Capture};

/// The bit width every capture in `caps` carries, or `None` when one is
/// unbound, untyped or of another width.
///
/// Moving a constant across an operator is an identity only when the whole
/// shape evaluates at one width: reassociating `Add(Add(x:I8, C1:I8):I8,
/// C2:I16):I16` into one `I16` add loses the inner add's 8-bit wraparound.
/// `validate` rejects such a node, but a rewrite can mint one mid-pipeline.
pub(crate) fn same_width(f: &Function, b: &Bindings, caps: &[Capture]) -> Option<usize> {
    let (&first, rest) = caps.split_first()?;
    let width = b.get_type(first, f)?.bit_width();
    rest.iter()
        .all(|&c| b.get_type(c, f).is_some_and(|t| t.bit_width() == width))
        .then_some(width)
}

/// Whether `1 << count` is buildable at `ty`: `ty` has a `u128` modulus and
/// `count` is below its width.
///
/// A count is an ordinary constant, so an `I64` shift can name 200, and
/// `1u128 << 200` panics.
pub(crate) fn shift_in_range(ty: ValueType, count: u128) -> bool {
    ty.int_mask_u128().is_some() && count < ty.bit_width() as u128
}

/// `cap`'s all-ones mask, or `None` for a float or past the `u128` the
/// constant arithmetic computes in.
pub(crate) fn carrier_mask(f: &Function, b: &Bindings, cap: Capture) -> Option<u128> {
    b.get_type(cap, f)?.int_mask_u128()
}
