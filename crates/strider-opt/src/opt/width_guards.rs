use strider_ir::Function;
use strider_ir::node::ValueType;
use strider_pattern::{Bindings, Capture};

/// The bit width every capture in `caps` carries, or `None` when one is
/// unbound, untyped or of another width.
pub(crate) fn same_width(f: &Function, b: &Bindings, caps: &[Capture]) -> Option<usize> {
    let (&first, rest) = caps.split_first()?;
    let width = b.get_type(first, f)?.bit_width();
    rest.iter()
        .all(|&c| b.get_type(c, f).is_some_and(|t| t.bit_width() == width))
        .then_some(width)
}

/// Whether every capture in `caps` carries the root type `ty`'s width.
pub(crate) fn at_root_width(f: &Function, b: &Bindings, ty: ValueType, caps: &[Capture]) -> bool {
    same_width(f, b, caps) == Some(ty.bit_width())
}

/// Whether `cap` binds a shift count below integer `ty`'s width, so
/// `1 << count` can be built at `ty`.
pub(crate) fn shift_count_in_range(
    f: &Function,
    b: &Bindings,
    ty: ValueType,
    cap: Capture,
) -> bool {
    ty.is_integer()
        && b.get_uint(cap, f)
            .is_some_and(|count| count < strider_ir::UBig::from(ty.bit_width()))
}

/// `cap`'s type when that is an integer one, `None` for a float or an
/// unbound / untyped capture.
pub(crate) fn int_type(f: &Function, b: &Bindings, cap: Capture) -> Option<ValueType> {
    b.get_type(cap, f).filter(|t| t.is_integer())
}
