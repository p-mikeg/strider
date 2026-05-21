//! Trait + registry abstraction over the graph-invariant validator
//! passes.
//!
//! Each of the six graph-invariant checks (`uniqueness`, `control_state`,
//! `phis`, `function_arg_uniqueness`, `wide_consts`, `asm_fingerprints`)
//! shares the same `(graph, reachable, errs)` call shape — `uniqueness`
//! intentionally ignores `reachable` so it can flag detached zombies of
//! `Entry`/`InitialMemory`.  Pulling the shape into a trait + a
//! `&'static [&dyn ValidatorPass]` registry turns the dispatch into a
//! table-driven loop; adding a new invariant is a single-line registry
//! append.

use crate::graph::Graph;
use crate::walk::NodeIdSet;

use super::ValidationError;
use super::graph_invariants::{
    check_graph_invariants_asm_fingerprints, check_graph_invariants_control_state,
    check_graph_invariants_function_arg_uniqueness, check_graph_invariants_phis,
    check_graph_invariants_uniqueness, check_graph_invariants_wide_consts,
};

/// Shared shape for graph-invariant validator passes.  Each
/// implementation runs one whole-graph check and appends any
/// violations to `errs`.  Passes receive the precomputed `reachable`
/// set so the validator pays a single graph walk per `validate()` call;
/// passes that intentionally scan the whole arena (e.g. `UniquenessPass`)
/// simply ignore the parameter.
pub(super) trait ValidatorPass: Sync {
    /// Short identifier for the pass, used in diagnostics and the
    /// registry table.  Not user-visible; the validator emits typed
    /// `ValidationError`s, not pass names.  Currently consumed only by
    /// `#[cfg(test)]` debug helpers, so the dead-code lint is silenced.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Run the pass's invariant check against `graph`.  Implementations
    /// must only append to `errs` (never clear or reorder it) so the
    /// validator's overall error ordering stays stable.
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>);
}

pub(super) struct UniquenessPass;
impl ValidatorPass for UniquenessPass {
    fn name(&self) -> &'static str {
        "uniqueness"
    }
    fn check(&self, graph: &Graph, _reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_uniqueness(graph, errs);
    }
}

pub(super) struct ControlStatePass;
impl ValidatorPass for ControlStatePass {
    fn name(&self) -> &'static str {
        "control_state"
    }
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_control_state(graph, reachable, errs);
    }
}

pub(super) struct PhisPass;
impl ValidatorPass for PhisPass {
    fn name(&self) -> &'static str {
        "phis"
    }
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_phis(graph, reachable, errs);
    }
}

pub(super) struct FunctionArgUniquenessPass;
impl ValidatorPass for FunctionArgUniquenessPass {
    fn name(&self) -> &'static str {
        "function_arg_uniqueness"
    }
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_function_arg_uniqueness(graph, reachable, errs);
    }
}

pub(super) struct WideConstsPass;
impl ValidatorPass for WideConstsPass {
    fn name(&self) -> &'static str {
        "wide_consts"
    }
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_wide_consts(graph, reachable, errs);
    }
}

pub(super) struct AsmFingerprintsPass;
impl ValidatorPass for AsmFingerprintsPass {
    fn name(&self) -> &'static str {
        "asm_fingerprints"
    }
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>) {
        check_graph_invariants_asm_fingerprints(graph, reachable, errs);
    }
}

/// Registry of every graph-invariant pass, in the order the validator
/// dispatches them.  The order matches the historical hand-rolled
/// dispatch in `validate()` to keep error ordering byte-stable for the
/// v3 baseline snapshots.
pub(super) static GRAPH_INVARIANT_PASSES: &[&dyn ValidatorPass] = &[
    &UniquenessPass,
    &ControlStatePass,
    &PhisPass,
    &FunctionArgUniquenessPass,
    &WideConstsPass,
    &AsmFingerprintsPass,
];
