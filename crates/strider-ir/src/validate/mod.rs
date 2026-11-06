//! Whole-graph IR validator. Every check aggregates into a
//! [`ValidationErrors`] bundle rather than failing fast.

use crate::IRViewer;
use crate::function::Function;
use crate::node::signature::ExpectedValueKind;
use crate::node::{NodeId, ValueId, ValueKind, ValueType};
use crate::schedule::ScheduleContext;
use crate::walk::NodeIdSet;

mod graph_invariants;
mod local_typing;
#[cfg(test)]
mod tests;

use graph_invariants::{
    UniqueKinds, check_arith_widths, check_asm_fingerprint, check_call_cc, check_consts,
    check_control_single_use, check_extend_truncate, check_function_invariants_availability,
    check_function_invariants_data_cycles, check_function_invariants_memory_linearity,
    check_function_invariants_side_indices, check_function_invariants_terminator_reachable,
    check_initial_vars, check_memory_chain, check_phis, check_region, check_switch,
    check_unique_kinds, note_unique_kinds,
};
use local_typing::check_local_typing;

/// Validates the entry-reachable part of `function`.
///
/// # Errors
///
/// Returns a [`ValidationErrors`] bundle aggregating every violation found.
pub fn validate(function: &Function) -> Result<(), ValidationErrors> {
    let entry = function.entry();
    // Only reachable nodes are judged; detached ones keep stale shapes.
    let mut walk = crate::walk::walk_graph(function.graph(), entry);
    walk.by_ref().for_each(|_| {});
    let reachable: NodeIdSet = walk.into_visited();
    let mut errs: Vec<ValidationError> = Vec::new();

    let mut seen = UniqueKinds::default();
    for node in &reachable {
        let kind = function.node_kind(node);
        check_local_typing(function.graph(), node, &mut errs);
        note_unique_kinds(&mut seen, node, kind);
        check_region(function, node, kind, &mut errs);
        check_control_single_use(function, node, &mut errs);
        check_phis(function, node, kind, &mut errs);
        check_consts(function, node, kind, &mut errs);
        check_initial_vars(function, node, kind, &mut errs);
        check_extend_truncate(function, node, kind, &mut errs);
        check_arith_widths(function, node, kind, &mut errs);
        check_switch(function, node, kind, &mut errs);
        check_call_cc(function, node, kind, &mut errs);
        check_asm_fingerprint(function, node, kind, &mut errs);
        check_memory_chain(function, &reachable, node, kind, &mut errs);
    }
    check_unique_kinds(&seen, &mut errs);

    check_function_invariants_data_cycles(function, &reachable, &mut errs);
    check_function_invariants_side_indices(function, &reachable, &mut errs);
    check_function_invariants_terminator_reachable(function, &mut errs);

    // Scheduling needs a structurally valid graph; a missing asm fingerprint
    // cannot make it unsafe.
    if errs
        .iter()
        .all(|e| matches!(e, ValidationError::MissingAsmFingerprint { .. }))
    {
        let ctx = ScheduleContext::new(function, &reachable);
        check_function_invariants_memory_linearity(&ctx, &mut errs);
        check_function_invariants_availability(&ctx, &mut errs);
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

#[derive(Debug)]
pub struct ValidationErrors(pub Vec<ValidationError>);

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("node {node:?} has {actual} inputs, expected {expected}")]
    NodeInputCountMismatch {
        node: NodeId,
        expected: usize,
        actual: usize,
    },

    #[error("node {node:?} input[{input_idx}] has kind {actual:?}, expected {expected:?}")]
    NodeInputKindMismatch {
        node: NodeId,
        input_idx: usize,
        expected: ExpectedValueKind,
        actual: ValueKind,
    },

    #[error("node {node:?} has {actual} outputs, expected {expected}")]
    NodeOutputCountMismatch {
        node: NodeId,
        expected: usize,
        actual: usize,
    },

    #[error("node {node:?} output[{output_idx}] has kind {actual:?}, expected {expected:?}")]
    NodeOutputKindMismatch {
        node: NodeId,
        output_idx: usize,
        expected: ExpectedValueKind,
        actual: ValueKind,
    },

    #[error("node {node:?} output[{output_idx}] is {ty:?}, a width outside ValueType::SUPPORTED")]
    UnsupportedValueType {
        node: NodeId,
        output_idx: usize,
        ty: ValueType,
    },

    #[error("multiple Entry nodes: {first:?} and {second:?}")]
    MultipleEntryNodes { first: NodeId, second: NodeId },

    #[error("multiple InitialMemory nodes: {first:?} and {second:?}")]
    MultipleInitialMemoryNodes { first: NodeId, second: NodeId },

    #[error("Region {region:?} has zero predecessors")]
    EmptyRegionPredecessors { region: NodeId },

    #[error(
        "node {node:?} produces a Control output {value:?} that no reachable node \
         consumes"
    )]
    UnusedControlOutput { node: NodeId, value: ValueId },

    #[error(
        "node {node:?} produces a Control output {value:?} consumed by more than \
         one node"
    )]
    ReusedControlOutput { node: NodeId, value: ValueId },

    #[error(
        "{count} node(s) control-reachable from Entry, the first being {node:?}, \
         reach no terminator (`Return` / `IndirectBranch` / `Unreachable`)"
    )]
    NoTerminatorReachable { node: NodeId, count: usize },

    #[error("node {node:?} input[{input_idx}] is not available where it is used")]
    InputNotAvailable { node: NodeId, input_idx: usize },

    #[error(
        "phi node {phi:?} input[0] token producer {producer:?} has kind \
         {producer_kind:?}; expected PhiToken from a Region"
    )]
    PhiTokenNotFromRegion {
        phi: NodeId,
        producer: NodeId,
        producer_kind: ValueKind,
    },

    #[error(
        "phi {phi:?} has {actual_values} value inputs but its Region \
         owner {owner_region:?} has {expected_predecessors} predecessors"
    )]
    PhiValueArityMismatch {
        phi: NodeId,
        owner_region: NodeId,
        expected_predecessors: usize,
        actual_values: usize,
    },

    #[error(
        "value phi {phi:?} declares output type {output_ty:?} but value input \
         at position {input_index} has type {input_ty:?}; a phi must merge \
         values of a single type"
    )]
    PhiInputTypeMismatch {
        phi: NodeId,
        input_index: usize,
        output_ty: ValueType,
        input_ty: ValueType,
    },

    #[error(
        "node {node:?} (kind {kind:?}) is reachable but has an empty \
         asm-fingerprint"
    )]
    MissingAsmFingerprint {
        node: NodeId,
        kind: crate::node::NodeKind,
    },

    #[error(
        "node {node:?} is `IntConst({id:?})` but the const \
         interner has no entry for that id"
    )]
    DanglingConstId {
        node: NodeId,
        id: crate::node::IntConstId,
    },

    #[error(
        "node {node:?} (`IntConst(...)`) has an interned value that exceeds its \
         declared output width (bits set above the type's bit width)"
    )]
    ConstWidthMismatch {
        node: NodeId,
        id: crate::node::IntConstId,
    },

    #[error(
        "node {node:?} is `FloatConst({bits:#x})`, which has bits set above its \
         declared output width"
    )]
    FloatConstWidthMismatch { node: NodeId, bits: u64 },

    #[error(
        "node {node:?} is a `FloatConst` declared {ty:?}, wider than its 64-bit \
         payload"
    )]
    FloatConstUnrepresentableType { node: NodeId, ty: ValueType },

    #[error(
        "node {node:?} (kind {kind:?}) is its own transitive data producer; only a \
         Phi / MemPhi may close a data cycle"
    )]
    DataCycle {
        node: NodeId,
        kind: crate::node::NodeKind,
    },

    #[error(
        "reachable Store {node:?} (kind {kind:?}): its Memory output lists no \
         reachable consumer, so its use-list is severed"
    )]
    OrphanedMemoryOutput {
        node: NodeId,
        kind: crate::node::NodeKind,
    },

    #[error(
        "reachable Store {node:?} is off the memory chain: no Return, \
         IndirectBranch, Unreachable, Call, CallOther or live MemPhi arm takes \
         the token it produces, directly or through later Stores, so the write \
         is lost"
    )]
    LostStore { node: NodeId },

    #[error(
        "{node:?} takes the memory state from before {effect:?}, a memory effect \
         on a path into it, so that effect is lost"
    )]
    MemoryRewind { node: NodeId, effect: NodeId },

    #[error(
        "{region:?} merges a path carrying {effect:?}, a memory effect, but has no \
         `MemPhi` on the memory chain to merge it into, so that effect is lost"
    )]
    UnmergedMemoryEffect { region: NodeId, effect: NodeId },

    #[error("node {node:?} names InitialVnId {id:?}, which the function never minted")]
    DanglingInitialVnId {
        node: NodeId,
        id: crate::node::InitialVnId,
    },

    #[error("InitialVar {node:?} reads varnode {vn:?} as {ty:?}, not as the integer of its size")]
    InitialVarTypeMismatch {
        node: NodeId,
        vn: rsleigh::Vn,
        ty: ValueType,
    },

    #[error(
        "initial_var_index entry for varnode {vn:?} points at reachable node \
         {node:?} (kind {actual_kind:?}); expected an InitialVar({vn:?}) node"
    )]
    StaleInitialVarIndex {
        node: NodeId,
        vn: rsleigh::Vn,
        actual_kind: crate::node::NodeKind,
    },

    #[error(
        "value_vn entry tags value {value:?} (varnode {vn:?}) whose reachable \
         producer {producer:?} has kind {producer_kind:?}; only Phi / Call / \
         CallOther outputs carry a value_vn tag"
    )]
    StaleValueVn {
        value: ValueId,
        vn: rsleigh::Vn,
        producer: NodeId,
        producer_kind: crate::node::NodeKind,
    },

    #[error(
        "node {node:?} (kind {kind:?}) has input width {in_width} and output \
         width {out_width}; `Extend` must strictly widen and `Truncate` must \
         strictly narrow"
    )]
    ExtendTruncateWidthDirection {
        node: NodeId,
        kind: crate::node::NodeKind,
        in_width: usize,
        out_width: usize,
    },

    #[error(
        "node {node:?} (kind {kind:?}) mixes widths: operand {operand_idx} is \
         {operand_width} bits against {expected_width}"
    )]
    ArithmeticWidthMismatch {
        node: NodeId,
        kind: crate::node::NodeKind,
        operand_idx: usize,
        operand_width: usize,
        expected_width: usize,
    },

    #[error("Switch {node:?} has no control outputs")]
    EmptySwitchTargets { node: NodeId },

    #[error("node {node:?} is `Switch({id:?})` but the function has no table for that id")]
    DanglingSwitchTableId {
        node: NodeId,
        id: crate::node::SwitchTableId,
    },

    #[error(
        "node {node:?} is `Call {{ cc: Some({id:?}) }}` but the function has no \
         convention for that id"
    )]
    DanglingCcId { node: NodeId, id: crate::node::CcId },

    #[error(
        "Switch {node:?} has {outputs} control outputs but {targets} recorded target addresses"
    )]
    SwitchTargetArityMismatch {
        node: NodeId,
        outputs: usize,
        targets: usize,
    },
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for err in &self.0 {
            writeln!(f, "{err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

impl ValidationErrors {
    /// Renders each error followed by the kind and lowest asm-fingerprint
    /// address of every node it names.
    #[must_use]
    pub fn report(self, function: &Function) -> ValidationReport {
        let mut rendered = String::new();
        for err in &self.0 {
            rendered.push_str(&err.to_string());
            let described: Vec<String> = err
                .nodes()
                .into_iter()
                .map(|node| describe_node(function, node))
                .collect();
            if !described.is_empty() {
                rendered.push_str(&format!(" [{}]", described.join("; ")));
            }
            rendered.push('\n');
        }
        ValidationReport {
            errors: self,
            rendered,
        }
    }
}

/// `node12 Load(..) @ 0x401a2c`; the address is left off an unattributed node.
fn describe_node(function: &Function, node: NodeId) -> String {
    if !function.graph().has_node(node) {
        return format!("{node:?}");
    }
    let kind = function.node_kind(node);
    match function
        .side_tables()
        .asm_fingerprint(node)
        .into_iter()
        .min()
    {
        Some(addr) => format!("{node:?} {kind:?} @ {addr:#x}"),
        None => format!("{node:?} {kind:?}"),
    }
}

/// [`ValidationErrors`] rendered against the function they were found in.
#[derive(Debug)]
pub struct ValidationReport {
    pub errors: ValidationErrors,
    rendered: String,
}

impl std::fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl std::error::Error for ValidationReport {}

impl ValidationError {
    /// Every node the error names, in field order.
    #[must_use]
    pub fn nodes(&self) -> smallvec::SmallVec<[NodeId; 2]> {
        use ValidationError as E;
        match *self {
            E::MultipleEntryNodes { first, second }
            | E::MultipleInitialMemoryNodes { first, second } => {
                smallvec::smallvec![first, second]
            }
            E::PhiTokenNotFromRegion { phi, producer, .. } => smallvec::smallvec![phi, producer],
            E::PhiValueArityMismatch {
                phi, owner_region, ..
            } => smallvec::smallvec![phi, owner_region],
            E::PhiInputTypeMismatch { phi, .. } => smallvec::smallvec![phi],
            E::MemoryRewind { node, effect } => smallvec::smallvec![node, effect],
            E::UnmergedMemoryEffect { region, effect } => smallvec::smallvec![region, effect],
            E::EmptyRegionPredecessors { region } => smallvec::smallvec![region],
            E::StaleValueVn { producer, .. } => smallvec::smallvec![producer],
            E::NodeInputCountMismatch { node, .. }
            | E::NodeInputKindMismatch { node, .. }
            | E::NodeOutputCountMismatch { node, .. }
            | E::NodeOutputKindMismatch { node, .. }
            | E::UnsupportedValueType { node, .. }
            | E::UnusedControlOutput { node, .. }
            | E::ReusedControlOutput { node, .. }
            | E::NoTerminatorReachable { node, .. }
            | E::InputNotAvailable { node, .. }
            | E::MissingAsmFingerprint { node, .. }
            | E::DanglingConstId { node, .. }
            | E::ConstWidthMismatch { node, .. }
            | E::FloatConstWidthMismatch { node, .. }
            | E::FloatConstUnrepresentableType { node, .. }
            | E::DataCycle { node, .. }
            | E::OrphanedMemoryOutput { node, .. }
            | E::LostStore { node }
            | E::DanglingInitialVnId { node, .. }
            | E::InitialVarTypeMismatch { node, .. }
            | E::StaleInitialVarIndex { node, .. }
            | E::ExtendTruncateWidthDirection { node, .. }
            | E::ArithmeticWidthMismatch { node, .. }
            | E::EmptySwitchTargets { node }
            | E::DanglingSwitchTableId { node, .. }
            | E::DanglingCcId { node, .. }
            | E::SwitchTargetArityMismatch { node, .. } => smallvec::smallvec![node],
        }
    }
}
