//! Whole-graph IR validator.  Every check aggregates into a
//! [`ValidationErrors`] bundle rather than failing fast.

use crate::IRViewer;
use crate::function::Function;
use crate::node::{NodeId, ValueId, ValueKind, ValueType};
use crate::node_signature::ExpectedValueKind;
use crate::walk::NodeIdSet;

mod graph_invariants;
mod local_typing;
#[cfg(test)]
mod tests;

use graph_invariants::{
    check_function_invariants_asm_fingerprints, check_function_invariants_consts,
    check_function_invariants_control_single_use, check_function_invariants_extend_truncate,
    check_function_invariants_memory_chain, check_function_invariants_phis,
    check_function_invariants_region, check_function_invariants_side_indices,
    check_function_invariants_switch, check_function_invariants_uniqueness,
};
use local_typing::check_local_typing;

/// Validates the entry-reachable part of `function`.
///
/// # Errors
///
/// Returns a [`ValidationErrors`] bundle aggregating every violation found.
pub fn validate(function: &Function) -> Result<(), ValidationErrors> {
    let entry = function.entry();
    // Detached zombies (dead-branch residue, collapsed phis) carry stale
    // shapes, so only the entry-reachable set is checked.
    let mut walk = crate::walk::walk_graph(function.graph(), entry);
    walk.by_ref().for_each(|_| {});
    let reachable: NodeIdSet = walk.into_visited();
    let mut errs: Vec<ValidationError> = Vec::new();

    for (node, _kind) in function.reachable_kind_iter(&reachable) {
        check_local_typing(function.graph(), node, &mut errs);
    }

    check_function_invariants_uniqueness(function, &reachable, &mut errs);
    check_function_invariants_region(function, &reachable, &mut errs);
    check_function_invariants_control_single_use(function, &reachable, &mut errs);
    check_function_invariants_phis(function, &reachable, &mut errs);
    check_function_invariants_consts(function, &reachable, &mut errs);
    check_function_invariants_extend_truncate(function, &reachable, &mut errs);
    check_function_invariants_switch(function, &reachable, &mut errs);
    check_function_invariants_asm_fingerprints(function, &reachable, &mut errs);
    check_function_invariants_memory_chain(function, &reachable, &mut errs);
    check_function_invariants_side_indices(function, &reachable, &mut errs);

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

fn kind_matches(expected: ExpectedValueKind, actual: ValueKind) -> bool {
    match expected {
        ExpectedValueKind::Control => matches!(actual, ValueKind::Control),
        ExpectedValueKind::Memory => matches!(actual, ValueKind::Memory),
        ExpectedValueKind::PhiToken => matches!(actual, ValueKind::PhiToken),
        ExpectedValueKind::Bool => {
            matches!(actual, ValueKind::Typed(ValueType::I1))
        }
        ExpectedValueKind::AnyInt => {
            matches!(actual, ValueKind::Typed(t) if t.is_integer())
        }
        ExpectedValueKind::AnyFloat => {
            matches!(actual, ValueKind::Typed(t) if t.is_float())
        }
        ExpectedValueKind::AnyValue => matches!(actual, ValueKind::Typed(_)),
    }
}

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

    #[error("multiple Entry nodes: {first:?} and {second:?}")]
    MultipleEntryNodes { first: NodeId, second: NodeId },

    #[error("multiple InitialMemory nodes: {first:?} and {second:?}")]
    MultipleInitialMemoryNodes { first: NodeId, second: NodeId },

    #[error("Region {region:?} has zero predecessors")]
    EmptyRegionPredecessors { region: NodeId },

    #[error(
        "node {node:?} produces a Control output {value:?} that no reachable node \
         consumes; every control edge must reach a terminator (`Return` / \
         `IndirectBranch` / `Unreachable`)"
    )]
    UnusedControlOutput { node: NodeId, value: ValueId },

    #[error(
        "node {node:?} produces a Control output {value:?} consumed by more than \
         one node; a control edge has exactly one successor (a split must be an \
         `If`, a merge must go through a `Region`)"
    )]
    ReusedControlOutput { node: NodeId, value: ValueId },

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
         asm-fingerprint; non-exempt nodes must record at least one \
         contributing machine-instruction address"
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
        id: crate::node::const_value::ConstId,
    },

    #[error(
        "node {node:?} (`IntConst(...)`) has an interned value that exceeds its \
         declared output width (bits set above the type's bit width)"
    )]
    ConstWidthMismatch {
        node: NodeId,
        id: crate::node::const_value::ConstId,
    },

    #[error(
        "reachable Store {node:?} (kind {kind:?}) produces a Memory output that no \
         reachable node consumes; a Store must stay anchored in the live memory \
         chain (back to a Return / IndirectBranch terminator) or it is silently \
         dropped by compaction"
    )]
    OrphanedMemoryOutput {
        node: NodeId,
        kind: crate::node::NodeKind,
    },

    #[error(
        "initial_var_index entry for varnode {vn:?} points at reachable node \
         {node:?} (kind {actual_kind:?}); expected an InitialVar({vn:?}) node — \
         the index has drifted from the live graph"
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
         strictly narrow (same value, opposite-direction op is a redundant \
         spelling)"
    )]
    ExtendTruncateWidthDirection {
        node: NodeId,
        kind: crate::node::NodeKind,
        in_width: usize,
        out_width: usize,
    },

    #[error("Switch {node:?} has no control outputs")]
    EmptySwitchTargets { node: NodeId },

    #[error(
        "Switch {node:?} has {outputs} control outputs but {targets} recorded target addresses"
    )]
    SwitchTargetArityMismatch {
        node: NodeId,
        outputs: usize,
        targets: usize,
    },
}

impl std::fmt::Debug for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ValidationErrors").field(&self.0).finish()
    }
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
