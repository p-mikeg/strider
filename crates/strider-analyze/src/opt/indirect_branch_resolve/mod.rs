//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites and exposes the in-place IR edits for the
//! resolutions that don't require a CFG rebuild.  The strider
//! orchestrator drives the outer loop (CFG rebuild, cache invalidation,
//! iteration cap) and calls into the classifier + inplace functions
//! directly — there is no opt-pipeline pass for indirect-branch
//! resolution.
//!
//! ## Submodules
//!
//! - [`classify`] — producer-shape classifier returning
//!   [`ResolvedTargets`] (`classify_anchor*` family).
//! - [`inplace`] — in-place IR edits for `LinkRegister` returns and
//!   `Single` tail calls (`apply_link_register`, `apply_tail_call`).
//! - [`jump_table`] — rodata jump-table arm.
//! - [`stack_array`] — stack-array-of-labels arm.
//!
//! ## Where [`ResolvedTargets`] lives
//!
//! Defined here in `opt` and re-exported as `cfg::ResolvedTargets`.
//! `cfg` already depends on `opt` (cfg's `indirect_resolve` mini-graph
//! runs the opt pipeline), so opt is the upstream crate where the type
//! must live; the reverse direction would form a dep cycle.

#![allow(clippy::module_name_repetitions)]

use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

pub mod classify;
pub mod inplace;
pub mod jump_table;
pub mod stack_array;

/// Per-anchor enumeration cap, shared by both the rodata jump-table arm
/// (`jump_table::classify_jump_table`) and the stack-array-of-labels arm
/// (`stack_array::classify_stack_array`).
///
/// `u32::MAX + 1` if a known-bits mask were all-ones, so without this cap
/// a buggy KnownBits result could force iteration through 4 GiB of slots.
/// Real jump tables emitted by gcc/clang are bounded by the source-level
/// `switch` arm count, almost always well under 4096.  Tables larger than
/// this cap are unusual enough that we prefer `None` (defer to
/// `UnresolvedIndirectBranch`) over the pathological enumeration cost.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use classify::{
    classify_anchor, classify_anchor_with_rom, classify_anchor_with_rom_and_sp,
};
pub use inplace::{apply_link_register, apply_tail_call};
pub use jump_table::classify_jump_table;
pub use stack_array::classify_stack_array;

/// The set of statically-known targets of a single `BranchIndirect`.
///
/// The classifier's verdict on a placeholder anchor.  Re-exported from
/// `cfg` so callers that build `known_targets` maps for
/// `cfg::Builder::with_known_targets` use the same type the
/// classifier returns.
///
/// ## Variants
///
/// - [`Self::LinkRegister`] — the indirect branch is a return-via-LR
///   (typical on ARM/AArch64 with `bx lr`).  In-place edit: append the
///   ABI ret-val regs to the placeholder Return and we're done.
/// - [`Self::Single`] — the indirect branch resolves to exactly one
///   constant target.  In-place edit possible iff the target is a
///   tail call (out of function range); otherwise the orchestrator
///   does a CFG rebuild.
/// - [`Self::Multiple`] — the indirect branch resolves to a known set
///   of constant targets (jump table).  Always requires a CFG rebuild;
///   the orchestrator handles these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// The indirect branch dispatches to the link register's
    /// caller-provided value (i.e. a function return via LR).
    LinkRegister,
    /// The indirect branch resolves to exactly one constant target.
    Single(u64),
    /// The indirect branch resolves to a known set of constant
    /// targets.  Sorted-deduplicated by the classifier.
    ///
    /// **Invariant:** the inner `Vec` must be **non-empty**.  An
    /// empty `Multiple` would silently advertise zero runtime targets,
    /// making the dispatch site appear unreachable.  Use
    /// [`Self::multiple`] for the validating constructor that
    /// rejects empty input — the existing `Multiple(targets)`
    /// tuple-construct form is retained for pattern-matching and
    /// for callers that have already established non-emptiness.
    Multiple(Vec<u64>),
}

impl ResolvedTargets {
    /// Validating constructor for [`Self::Multiple`].  Returns `None`
    /// for an empty `targets` slice so a future arm cannot silently
    /// produce an unreachable dispatch site.  The classifier arms
    /// (jump-table, stack-array, ValuePhi) already check
    /// `targets.is_empty()` and return `None` instead of constructing
    /// an empty `Multiple`; this constructor codifies the contract for
    /// any future arm.  Emptiness is a programmer invariant violation,
    /// not a recoverable runtime condition, so `Option` is the
    /// idiomatic carrier.
    #[must_use]
    pub fn multiple(targets: Vec<u64>) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        Some(Self::Multiple(targets))
    }
}

/// Per-anchor calling-convention snapshot consumed by the in-place
/// editors ([`apply_link_register`] / [`apply_tail_call`]).  The
/// orchestrator populates this from the cache's `exit_vn_to_value` for
/// the dispatch region; the in-place editors thread it into the
/// resulting Call/Return nodes.
#[derive(Debug, Clone, Default)]
pub struct AnchorCallingContext {
    /// IR `NodeOutputId`s for the calling convention's
    /// `arg_passing_vars` at the dispatch site.  Threaded as
    /// `inputs[3..]` to the resulting Call node (slots after control,
    /// memory, target).
    pub arg_passing_outputs: Vec<NodeOutputId>,
    /// `NodeOutputKind`s for the calling convention's clobbered
    /// varnodes at the dispatch site.  Threaded as the Call node's
    /// value outputs after `[Control, Memory]`.
    pub clobbered_kinds: Vec<NodeOutputKind>,
    /// IR `NodeOutputId`s for the calling convention's `ret_val_regs`
    /// at the dispatch site.  Threaded as the resulting Return node's
    /// inputs after `[control, memory, target_value]`
    /// (link-register case) or `[call_ctrl, call_mem]` (tail-call
    /// case).
    pub ret_val_outputs: Vec<NodeOutputId>,
}

/// Walk the use-list of `anchor_output` and return the unique
/// 3-input `IndirectBranch` whose `target_value` input equals
/// `anchor_output` — the placeholder shape pinned at strider's lift
/// time.
///
/// Returns `None` when no such placeholder exists (e.g. an earlier
/// in-place edit already replaced it: `apply_tail_call` detaches the
/// node, and `apply_link_register` mutates the kind to
/// [`NodeKind::Return`]).  Public so strider's orchestrator can reuse
/// the same lookup for its own bookkeeping.
#[must_use]
pub fn find_placeholder_return_for_anchor(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer, _input_index) in graph.output_uses(anchor_output) {
        if !matches!(graph.node_kind(consumer), NodeKind::IndirectBranch) {
            continue;
        }
        // `IndirectBranch` has the signature `[control, memory,
        // target_value]` (see `node_signature::expected_signature`),
        // so `node_inputs_exact::<3>` is structurally guaranteed to
        // succeed; the `Ok(...)` arm is the only reachable branch.
        if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)
            && val == anchor_output
        {
            return Some(consumer);
        }
    }
    None
}
