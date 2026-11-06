//! Operand checks a builder runs before creating a node, as free functions
//! over `&Function` rather than `IRViewer` methods:
//!
//! ```compile_fail,E0599
//! use strider_ir::node::ValueId;
//! use strider_ir::{Function, IRViewer};
//!
//! fn check(f: &Function, v: ValueId) -> strider_ir::Result<()> {
//!     f.require_value_kind(v)
//! }
//! ```

use anyhow::anyhow;

use crate::IRViewer;
use crate::function::Function;
use crate::node::{NodeKind, ValueId, ValueKind, ValueType};

/// Each errors unless `value_id`'s kind satisfies the predicate.
macro_rules! value_kind_requirements {
    ($($name:ident => $pred:ident, $noun:literal;)+) => { $(
        pub(crate) fn $name(f: &Function, value_id: ValueId) -> crate::Result<()> {
            let kind = f.graph().value_kind(value_id);
            if kind.$pred() {
                Ok(())
            } else {
                Err(anyhow!("output {value_id:?} is not {} (got {kind:?})", $noun))
            }
        }
    )+ };
}

value_kind_requirements! {
    require_value_kind => is_value, "a value edge";
    require_bool_value => is_bool, "a bool value";
    require_memory_kind => is_memory, "a memory edge";
}

/// Never coerces.
pub(crate) fn require_value_type(
    f: &Function,
    value_id: ValueId,
    expected: ValueType,
) -> crate::Result<ValueId> {
    let actual = f.value_type(value_id)?;
    if actual != expected {
        return Err(anyhow!(
            "operand {value_id:?} has type {actual} but the operation \
             requires {expected}; the caller must insert the truncate / \
             extend / bitcast"
        ));
    }
    Ok(value_id)
}

pub(crate) fn require_integer_value(f: &Function, value_id: ValueId) -> crate::Result<()> {
    ensure_value_type(
        value_id,
        f.value_type(value_id)?.is_integer(),
        "an integer value",
    )
}

pub(crate) fn require_float_value(f: &Function, value_id: ValueId) -> crate::Result<()> {
    ensure_value_type(
        value_id,
        f.value_type(value_id)?.is_float(),
        "a float value",
    )
}

pub(crate) fn require_integer_type(ty: ValueType) -> crate::Result<()> {
    if !ty.is_integer() {
        return Err(anyhow!("type {ty:?} is not an integer type"));
    }
    Ok(())
}

pub(crate) fn require_float_type(ty: ValueType) -> crate::Result<()> {
    if !ty.is_float() {
        return Err(anyhow!("type {ty:?} is not a float type"));
    }
    Ok(())
}

pub(crate) fn validate_value_inputs(f: &Function, inputs: &[ValueId]) -> crate::Result<()> {
    for &v in inputs {
        require_value_kind(f, v)?;
    }
    Ok(())
}

/// Errors unless `value` may fill input `slot` of a `kind` node.
pub(crate) fn require_slot(
    f: &Function,
    kind: &NodeKind,
    slot: usize,
    value: ValueId,
) -> crate::Result<()> {
    let actual = f.graph().value_kind(value);
    let want = kind
        .expected_input_kind(slot)
        .ok_or_else(|| anyhow!("{kind:?} has no input slot {slot}"))?;
    if !want.admits(actual) {
        return Err(anyhow!(
            "operand {value:?} at {kind:?} input slot {slot} is {actual}, \
             where its node signature expects {want:?}"
        ));
    }
    Ok(())
}

/// Errors unless a `kind` node may output `ty` at slot 0.
pub(crate) fn require_output_type(kind: &NodeKind, ty: ValueType) -> crate::Result<()> {
    let want = kind
        .expected_output_kind(0)
        .ok_or_else(|| anyhow!("{kind:?} has no output slot 0"))?;
    if !want.admits(ValueKind::Typed(ty)) {
        return Err(anyhow!(
            "a {kind:?} cannot output {ty}: its node signature expects {want:?}"
        ));
    }
    Ok(())
}

fn ensure_value_type(value_id: ValueId, ok: bool, noun: &str) -> crate::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(anyhow!("output {value_id:?} is not {noun}"))
    }
}
