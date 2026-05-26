//! Pattern builders for `MemProject` and `MemUnion` nodes.
//!
//! `MemProjectPat` matches a `MemProject` node (which splits a unified
//! memory edge into per-`AliasClass` partition outputs).  `MemUnionPat`
//! matches a `MemUnion` node (which merges partition outputs back into a
//! unified memory edge).
//!
//! Both builders accept an optional `.class(AliasClass)` filter that
//! narrows the match:
//!
//! * On `MemProject`: requires that at least one of the node's output
//!   slots carries `NodeOutputKind::Memory(Some(class))`.
//! * On `MemUnion`: requires that at least one of the node's input
//!   edges originates from a `NodeOutputKind::Memory(Some(class))` output.

use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputKind};
use strider_target::AliasClass;

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat};

// ── MemProjectPat ─────────────────────────────────────────────────────────────

/// Builder for `MemProject` node patterns.  Created by
/// [`crate::pattern::pat::mem_project`].
///
/// Without a `.class()` filter, matches any `MemProject` node
/// regardless of which partitions it exposes.  With `.class(c)`, the
/// match requires that at least one output slot carries
/// `NodeOutputKind::Memory(Some(c))`.
///
/// `MemProject` splits a unified memory chain into per-`AliasClass`
/// partitions; it has one memory input (the unified chain) and N memory
/// outputs (one per active partition).
pub struct MemProjectPat {
    class: Option<AliasClass>,
}

impl MemProjectPat {
    pub(crate) fn new() -> Self {
        Self { class: None }
    }

    /// Restrict the match to `MemProject` nodes that expose an output
    /// slot for partition `c` (i.e. one of the node's output slots
    /// carries `NodeOutputKind::Memory(Some(c))`).
    #[must_use]
    pub fn class(mut self, c: AliasClass) -> Self {
        self.class = Some(c);
        self
    }
}

impl From<MemProjectPat> for Pat {
    fn from(b: MemProjectPat) -> Pat {
        let MemProjectPat { class } = b;
        let kind = KindSpec::Exact(NodeKind::MemProject);
        let mut pat = NodePat::matcher(kind, InputsSpec::None);

        if let Some(want_class) = class {
            pat = pat.with_post_match(Arc::new(move |ctx, node, _b| {
                // Accept only if at least one output slot carries the
                // requested partition class.
                ctx.graph.node_outputs(node).iter().any(|&out| {
                    matches!(ctx.graph.output_kind(out),
                             NodeOutputKind::Memory(Some(c)) if c == want_class)
                })
            }));
        }

        pat.into_pat()
    }
}

// ── MemUnionPat ───────────────────────────────────────────────────────────────

/// Builder for `MemUnion` node patterns.  Created by
/// [`crate::pattern::pat::mem_union`].
///
/// Without a `.class()` filter, matches any `MemUnion` node regardless
/// of which partitions it merges.  With `.class(c)`, the match requires
/// that at least one input edge originates from a
/// `NodeOutputKind::Memory(Some(c))` output slot.
///
/// `MemUnion` merges per-`AliasClass` partition chains back into a
/// single unified memory output; it has variadic memory inputs and one
/// `Memory(None)` output.
pub struct MemUnionPat {
    class: Option<AliasClass>,
}

impl MemUnionPat {
    pub(crate) fn new() -> Self {
        Self { class: None }
    }

    /// Restrict the match to `MemUnion` nodes that accept an input
    /// from partition `c` (i.e. at least one input edge is a
    /// `NodeOutputKind::Memory(Some(c))` output).
    #[must_use]
    pub fn class(mut self, c: AliasClass) -> Self {
        self.class = Some(c);
        self
    }
}

impl From<MemUnionPat> for Pat {
    fn from(b: MemUnionPat) -> Pat {
        let MemUnionPat { class } = b;
        let kind = KindSpec::Exact(NodeKind::MemUnion);
        let mut pat = NodePat::matcher(kind, InputsSpec::None);

        if let Some(want_class) = class {
            pat = pat.with_post_match(Arc::new(move |ctx, node, _b| {
                // Accept only if at least one input edge originates from
                // the requested partition class.
                ctx.graph.node_inputs(node).into_iter().any(|input_out| {
                    matches!(ctx.graph.output_kind(input_out),
                             NodeOutputKind::Memory(Some(c)) if c == want_class)
                })
            }));
        }

        pat.into_pat()
    }
}
