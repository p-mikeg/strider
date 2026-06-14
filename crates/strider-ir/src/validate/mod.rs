//! Whole-graph validator for the IR.
//!
//! The validator walks a built [`crate::graph::Graph`] starting from the
//! function's own entry node and checks structural invariants across three
//! groups:
//!   - **Local typing** (`local_typing`): per-node input/output kind checks
//!     against `node_signature::expected_signature` (reachability-scoped).
//!   - **Use-list consistency** (`use_list_consistency`): bidirectional
//!     consistency between inputs and the outputs' use-lists
//!     (reachability-scoped on the source side).
//!   - **Graph invariants** (`graph_invariants`): whole-graph rules —
//!     Entry/InitialMemory uniqueness, Region predecessor kinds,
//!     phi-token ownership, phi per-predecessor arity,
//!     Call/Return calling-convention arity (output / input slot counts
//!     against the calling convention, honouring per-`Call` clobber
//!     overrides), wide-const consistency (including that an
//!     `IntConst(Wide(..))` declares an `I80`/`I128`/`I256`/`I512`
//!     output type matching its interned byte size), non-empty
//!     asm-fingerprints on every reachable non-exempt node, and that every
//!     reachable `Store`'s Memory output stays consumed (anchored in the
//!     live memory chain).
//!
//! On failure the validator returns a [`ValidationErrors`] bundle that
//! aggregates every [`ValidationError`] it found during a single pass, so
//! callers can see all problems at once rather than only the first.

use crate::IRViewer;
use crate::function::Function;
use crate::node::{NodeId, UseId, ValueId, ValueKind, ValueType};
use crate::node_signature::ExpectedValueKind;
use crate::walk::NodeIdSet;

mod graph_invariants;
mod local_typing;
#[cfg(test)]
mod tests;
mod use_list_consistency;

use graph_invariants::{
    check_graph_invariants_asm_fingerprints, check_graph_invariants_cc_arity,
    check_graph_invariants_memory_chain, check_graph_invariants_phis,
    check_graph_invariants_region, check_graph_invariants_side_indices,
    check_graph_invariants_uniqueness, check_graph_invariants_wide_consts,
};
use local_typing::check_local_typing;
use use_list_consistency::check_use_list_consistency;

/// Validates the structural invariants of `function`, starting the walk from
/// the function's own entry node.
///
/// Returns `Ok(())` if every checked invariant holds, or a
/// [`ValidationErrors`] bundle describing every violation otherwise. If the
/// function has not been built (no entry node), returns a bundle containing a
/// single [`ValidationError::NoEntry`].
///
/// Local per-node checks (`check_local_typing`) are scoped to nodes
/// reachable from the entry so that detached zombie nodes left behind by
/// optimization passes (e.g. orphaned dead-branch residue) do not trigger
/// false positives.  Use-list consistency and graph-invariants
/// checks iterate all nodes but are naturally tolerant of detached nodes:
/// `detach_node_inputs` scrubs the use-lists of the producers it disconnects,
/// so a detached node contributes no inputs and no live use-list entries
/// anywhere.
///
/// # Errors
///
/// Returns a [`ValidationErrors`] bundle aggregating every local-typing,
/// use-list, and graph-invariants violation found in `function`. Validation
/// does not fail fast — every check runs to completion so the caller sees
/// the full set of problems at once.
pub fn validate(function: &Function) -> Result<(), ValidationErrors> {
    let Some(entry) = function.entry() else {
        return Err(ValidationErrors(vec![ValidationError::NoEntry]));
    };
    // Drive the walk to completion and reuse its internal DenseEntitySet
    // tracker rather than re-collecting yielded NodeIds.  Saves N inserts
    // and one extra allocation per validate call.
    let mut walk = crate::walk::walk_graph(function.graph(), entry);
    walk.by_ref().for_each(|_| {});
    let reachable: NodeIdSet = walk.into_visited();
    let mut errs: Vec<ValidationError> = Vec::new();

    for (node, _kind) in function.reachable_kind_iter(&reachable) {
        check_local_typing(function.graph(), node, &mut errs);
    }

    check_use_list_consistency(function.graph(), &reachable, &mut errs);

    check_graph_invariants_uniqueness(function.graph(), &mut errs);
    check_graph_invariants_region(function.graph(), &reachable, &mut errs);
    check_graph_invariants_phis(function.graph(), &reachable, &mut errs);
    check_graph_invariants_wide_consts(function, &reachable, &mut errs);
    check_graph_invariants_cc_arity(function, &reachable, &mut errs);
    check_graph_invariants_asm_fingerprints(function, &reachable, &mut errs);
    check_graph_invariants_memory_chain(function, &reachable, &mut errs);
    check_graph_invariants_side_indices(function, &reachable, &mut errs);

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

/// Returns whether an actual [`ValueKind`] satisfies the
/// [`ExpectedValueKind`] declared by a [`NodeKind`]'s signature.
///
/// `AnyInt` matches any integer-typed output (I1, I8, I16, I32, I64, I80,
/// I128, I256, I512); `AnyFloat` matches F32, F64, or F80; `Bool`
/// matches only the 1-bit `Typed(I1)`.  `Control`, `Memory`, and
/// `PhiToken` match their identically-named [`ValueKind`]
/// variants.
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

/// A bundle of [`ValidationError`]s produced by a single [`validate`] call.
pub struct ValidationErrors(pub Vec<ValidationError>);

/// An individual IR validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("function has no entry node (not built)")]
    NoEntry,

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

    #[error(
        "node {node:?} input[{input_idx}] references output {value:?} \
         but is not in that output's use-list"
    )]
    InputMissingFromUseList {
        node: NodeId,
        input_idx: usize,
        value: ValueId,
    },

    #[error(
        "output {value:?}'s use-list contains input {listed_use:?} \
         that no longer references this output"
    )]
    UseListContainsStaleInput { value: ValueId, listed_use: UseId },

    #[error("multiple Entry nodes: {first:?} and {second:?}")]
    MultipleEntryNodes { first: NodeId, second: NodeId },

    #[error("multiple InitialMemory nodes: {first:?} and {second:?}")]
    MultipleInitialMemoryNodes { first: NodeId, second: NodeId },

    #[error("missing Entry node")]
    MissingEntryNode,

    #[error("missing InitialMemory node")]
    MissingInitialMemoryNode,

    #[error(
        "Region {region:?} input[{input_idx}] producer {producer:?} \
         has kind {producer_kind:?}, expected Control"
    )]
    RegionNonControlPredecessor {
        region: NodeId,
        input_idx: usize,
        producer: NodeId,
        producer_kind: ValueKind,
    },

    #[error("Region {region:?} has zero predecessors")]
    EmptyRegionPredecessors { region: NodeId },

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
        "node {node:?} is `IntConst(Wide({id:?}))` but the wide-const \
         side-table has no entry for that id"
    )]
    DanglingWideConstId {
        node: NodeId,
        id: crate::wide_const::WideConstId,
    },

    #[error(
        "node {node:?} (`IntConst(Wide(...))`) stores {actual_bytes}-byte value \
         but its output type is {output_type:?} ({expected_bytes}-byte)"
    )]
    WideConstWidthMismatch {
        node: NodeId,
        output_type: ValueType,
        expected_bytes: usize,
        actual_bytes: usize,
    },

    #[error(
        "node {node:?} (`IntConst(Wide(...))`) declares non-wide output type \
         {output_type:?}; only I80 / I128 / I256 / I512 are valid wide-const output types"
    )]
    WideConstInvalidOutputType {
        node: NodeId,
        output_type: ValueType,
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
