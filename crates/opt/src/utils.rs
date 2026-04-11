use ir::{BuiltFunctionGraph};
use ir::node::{NodeOutputId, NodeKind, NodeOutputKind, NodeOutputType};
use crate::opt::OptimizationResult;
use crate::error::Result;

/// Returns the float constant bit-pattern of `output` (raw `u64`), or `None`
/// if the output does not hold a `FloatConst`.
pub(crate) fn float_const_val(fg: &BuiltFunctionGraph, output: NodeOutputId) -> Option<u64> {
    let ty = fg.graph.output_kind(output).as_value()?;
    if !ty.is_float() {
        return None;
    }
    let node = fg.graph.get_node_from_output(output);
    match *fg.graph.node_kind(node) {
        NodeKind::FloatConst(bits) => Some(bits),
        _ => None,
    }
}

/// Creates (or retrieves) a `FloatConst(bits)` node with type `ty` and returns
/// its output id.
pub(crate) fn make_float_const(fg: &mut BuiltFunctionGraph, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
    let node = fg.graph.create_node(
        NodeKind::FloatConst(bits),
        [],
        [NodeOutputKind::OutputType(ty)],
    );
    Ok(fg.graph.node_outputs_exact::<1>(node)?[0])
}

/// Returns the integer constant value of `output` (masked to its declared type),
/// or `None` if the output does not hold an integer constant.
pub(crate) fn int_const_val(fg: &BuiltFunctionGraph, output: NodeOutputId) -> Option<u64> {
    let ty = fg.graph.output_kind(output).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    let node = fg.graph.get_node_from_output(output);
    match *fg.graph.node_kind(node) {
        NodeKind::IntConst(v) => ty.get_unsigned_int(v),
        _ => None,
    }
}

/// Returns the boolean constant value of `output`, or `None` if it is not a
/// `BoolConst` node.
pub(crate) fn bool_const_val(fg: &BuiltFunctionGraph, output: NodeOutputId) -> Option<bool> {
    if !fg.graph.output_kind(output).is_bool() {
        return None;
    }
    let node = fg.graph.get_node_from_output(output);
    match *fg.graph.node_kind(node) {
        NodeKind::BoolConst(v) => Some(v),
        _ => None,
    }
}

/// Creates (or retrieves from the deduplication cache) an `IntConst(val)` node
/// with the given type and returns its output id.
pub(crate) fn make_int_const(fg: &mut BuiltFunctionGraph, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
    let node = fg.graph.create_node(
        NodeKind::IntConst(val),
        [],
        [NodeOutputKind::OutputType(ty)],
    );
    Ok(fg.graph.node_outputs_exact::<1>(node)?[0])
}

/// Creates (or retrieves) a `BoolConst(val)` node and returns its output id.
pub(crate) fn make_bool_const(fg: &mut BuiltFunctionGraph, val: bool) -> Result<NodeOutputId> {
    let node = fg.graph.create_node(
        NodeKind::BoolConst(val),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    Ok(fg.graph.node_outputs_exact::<1>(node)?[0])
}

/// Creates an `IntBitsToFloat` node directly on the graph and returns its output.
pub(crate) fn make_int_bits_to_float_node(fg: &mut BuiltFunctionGraph, input: NodeOutputId, ty: NodeOutputType) -> Result<NodeOutputId> {
    let node = fg.graph.create_node(
        NodeKind::IntBitsToFloat,
        [input],
        [NodeOutputKind::OutputType(ty)],
    );
    Ok(fg.graph.node_outputs_exact::<1>(node)?[0])
}

/// Creates a `FloatToFloat` node directly on the graph and returns its output.
pub(crate) fn make_float_to_float_node(fg: &mut BuiltFunctionGraph, input: NodeOutputId, ty: NodeOutputType) -> Result<NodeOutputId> {
    let node = fg.graph.create_node(
        NodeKind::FloatToFloat,
        [input],
        [NodeOutputKind::OutputType(ty)],
    );
    Ok(fg.graph.node_outputs_exact::<1>(node)?[0])
}

/// Redirects every consumer of `old` to `new_val` instead.
///
/// Returns [`OptimizationResult::Changed`] if at least one use was replaced,
/// [`OptimizationResult::NoChange`] if `old` had no uses.
pub(crate) fn replace_all_uses(
    fg: &mut BuiltFunctionGraph,
    old: NodeOutputId,
    new_val: NodeOutputId,
) -> Result<OptimizationResult> {
    let mut cursor = fg.graph.output_use_cursor(old);
    if cursor.current().is_none() {
        return Ok(OptimizationResult::NoChange);
    }
    while cursor.current().is_some() {
        cursor.replace_current_with(new_val)?;
    }
    Ok(OptimizationResult::Changed)
}
