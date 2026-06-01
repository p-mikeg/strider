// `dead_code` allow: the `PatGraph` `Pattern` impl that exercises this
// trait + engine lands in a subsequent task.  Until then, `Matcher`
// constructors, `find_all`, `match_at`, and the `PatternExt` blanket
// have no call sites in this crate and clippy --release runs with
// `-D warnings`.  Module-level allow keeps the build green; the items
// themselves are `pub` for the upcoming consumers.
#![allow(dead_code)]

//! Pattern matcher.

mod ctx;
mod try_match;

pub use ctx::{BuildCtx, MatchCtx};

use std::mem::Discriminant;

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::capture::{Bindings, Match};

/// LHS of a rewrite or query.  A `Pattern` can be matched against an
/// IR node-output to attempt to bind captures.
pub trait Pattern {
    /// Try the pattern against `root_out`.  On success returns `true`
    /// with any captures recorded in `bindings`; on failure returns
    /// `false` (caller is responsible for restoring bindings to a
    /// pre-attempt mark if needed).
    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool;

    /// Discriminant of the root node's `NodeKind`, used by `find_all`
    /// to pre-filter IR nodes by kind.  Returns `None` for kind-`Any`
    /// roots (then `find_all` scans everything).
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>>;

    /// Try the pattern against a node with **no** value outputs.  Used
    /// by zero-output kinds (`Return`, `If`, `IndirectBranch` — though
    /// `If` already has Control outputs that the matcher iterates).
    /// The default impl returns `false`; concrete `Pattern` impls
    /// (notably `PatGraph`) override this to dispatch into their
    /// recursive walker with `root_out = None`.
    fn try_match_node(
        &self,
        _ctx: &MatchCtx,
        _node: NodeId,
        _bindings: &mut Bindings,
    ) -> bool {
        false
    }
}

/// Default extension: match against a node directly (used for
/// zero-output kinds like `Return`).  Implemented for every `Pattern`
/// via a blanket impl.
pub trait PatternExt {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool;
}

impl<T: Pattern + ?Sized> PatternExt for T {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool {
        let outputs = ctx.function.node_outputs(node);
        if outputs.is_empty() {
            // Zero-output kinds (e.g. `Return`) — dispatch through the
            // `try_match_node` hook, which `Pattern` impls can override
            // to match without a `NodeOutputId`.
            return self.try_match_node(ctx, node, bindings);
        }
        for &out in outputs {
            let mark = bindings.mark();
            if self.try_match(ctx, out, bindings) {
                return true;
            }
            bindings.restore(mark);
        }
        false
    }
}

/// Top-level matcher.  Owns no per-match state; `try_new` validates
/// the function once up-front (matching the existing
/// `strider-analyze::pattern::Matcher` contract).
pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
}

impl<'f> Matcher<'f> {
    /// Validate the function then return a matcher bound to it.
    ///
    /// # Errors
    /// Returns an error if `function` has no entry node or if whole-graph
    /// validation (`strider_ir::validate::validate`) reports any
    /// invariant failure.
    pub fn try_new(function: &'f Function) -> anyhow::Result<Self> {
        let entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("Function has no entry"))?;
        strider_ir::validate::validate(function, entry)
            .map_err(|errs| anyhow::anyhow!("validation: {errs:?}"))?;
        Ok(Self { function })
    }

    /// Find every match for `pat` in the function.  Currently scans
    /// every reachable node and filters by the pattern's
    /// `root_kind_discriminant`; future revisions may add a kind index
    /// for speed.
    pub fn find_all<P: Pattern>(&self, pat: &P) -> Vec<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let target_disc = pat.root_kind_discriminant();
        let mut out = Vec::new();
        for node in self.function.walk() {
            if let Some(d) = target_disc
                && std::mem::discriminant(self.function.node_kind(node)) != d
            {
                continue;
            }
            self.try_at_node(node, pat, &ctx, &mut out);
        }
        out
    }

    /// Try `pat` at a specific IR node; returns the first match if any
    /// (iterating outputs for value-producing nodes; node-rooted for
    /// zero-output kinds).
    pub fn match_at<P: Pattern>(&self, node: NodeId, pat: &P) -> Option<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if pat.try_match_node_id(&ctx, node, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
            return None;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if pat.try_match(&ctx, out_id, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
        }
        None
    }

    fn try_at_node<P: Pattern>(
        &self,
        node: NodeId,
        pat: &P,
        ctx: &MatchCtx,
        out: &mut Vec<Match>,
    ) {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if pat.try_match_node_id(ctx, node, &mut bindings) {
                out.push(Match::from_root(node, bindings));
            }
            return;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if pat.try_match(ctx, out_id, &mut bindings) {
                out.push(Match::from_root(node, bindings));
                break;
            }
        }
    }
}
