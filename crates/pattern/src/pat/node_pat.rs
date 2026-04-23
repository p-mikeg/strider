//! Generic node-level data pattern. Covers the vast majority of data-level
//! patterns with a single struct parameterised by a kind-match closure, an
//! `InputsSpec`, and optional post-match / capture hooks.
//!
//! `InputsSpec::None` handles zero-input patterns (constants, `InitialVar`,
//! `FunctionArg`); `InputsSpec::Fixed` covers unary / binary / cmp ops with
//! optional commutative retry; `InputsSpec::Indexed` covers sparse
//! positional matching for memory ops (`Load` / `Store` / `StackStore` /
//! `StackStorePhi`) and `Phi`.

use std::sync::Arc;

use ir::node::{NodeId, NodeOutputId};

use crate::matcher::Bindings;
use crate::pat::traits::{DataPattern, MatchCtx};
use crate::var::{NodeVar, Var};

/// Closure type used by [`NodePat::kind_match`] and [`NodePat::post_match`].
pub(crate) type NodeKindCheck =
    Arc<dyn Fn(&MatchCtx, NodeId, &mut Bindings) -> bool + Send + Sync>;

/// Runtime decider for whether an arity-2 [`InputsSpec::Fixed`] match should
/// retry with swapped operands.  Concrete ops fix the answer at construction
/// (`|_, _| true` or `|_, _| false`); `*Any` patterns inspect the matched op
/// variant to decide per-match.
pub(crate) type CommutativeDecider =
    Arc<dyn Fn(&MatchCtx, NodeId) -> bool + Send + Sync>;

/// A generic node-level data pattern. Covers every data pattern with the
/// shape "check node kind, match inputs in some arrangement, optionally
/// bind output/node captures".
pub struct NodePat {
    /// Checks the node's kind (and any kind-embedded data — e.g. constants,
    /// varnodes, spaces, offsets, phi offsets). May bind typed-constant
    /// values or operator variants. Has access to graph side-tables via
    /// `MatchCtx` (needed for `StackStorePhi::offsets`).
    pub(crate) kind_match: NodeKindCheck,
    /// How to match the node's inputs.
    pub(crate) inputs: InputsSpec,
    /// Runs AFTER inputs match (and after each commutative retry) but BEFORE
    /// output/node captures. This is where `*Any` patterns bind the
    /// op-variant — the bind must retry alongside input rebinding.
    pub(crate) post_match: Option<NodeKindCheck>,
    pub(crate) output_var: Option<Var>,
    pub(crate) node_var: Option<NodeVar>,
}

pub enum InputsSpec {
    /// Arity 0: constants, `InitialVar`, `FunctionArg`.
    None,
    /// Arity N with ordered operand matching. When `commutative(ctx, node)`
    /// returns true and the node has exactly 2 inputs, both orderings are
    /// tried.
    ///
    /// Sub-patterns are held as [`crate::pat::Pat`] (not `DynDataPat`) so
    /// the fluent builder API (`impl Into<Pat>`) can compose them uniformly;
    /// dispatch goes through [`crate::matcher::Matcher::match_output`].
    Fixed {
        pats: Vec<crate::pat::Pat>,
        commutative: CommutativeDecider,
    },
    /// Sparse positional constraints: only the listed input indices are
    /// matched against their sub-patterns; unlisted slots are unconstrained.
    /// Used by memory patterns (addr / data at specific indices) and phi
    /// patterns (per-predecessor constraints).
    Indexed(Vec<(usize, crate::pat::Pat)>),
}

impl InputsSpec {
    /// Fixed arity, never commutative.
    pub(crate) fn fixed_ordered(pats: Vec<crate::pat::Pat>) -> Self {
        Self::Fixed {
            pats,
            commutative: Arc::new(|_ctx, _node| false),
        }
    }

    /// Fixed arity-2, always commutative (concrete op is known to be so).
    pub(crate) fn fixed_commutative(lhs: crate::pat::Pat, rhs: crate::pat::Pat) -> Self {
        Self::Fixed {
            pats: vec![lhs, rhs],
            commutative: Arc::new(|_ctx, _node| true),
        }
    }

    /// Fixed arity-2, commutative decided at match time by `f(ctx, node)`.
    pub(crate) fn fixed_maybe_commutative<F>(
        lhs: crate::pat::Pat,
        rhs: crate::pat::Pat,
        f: F,
    ) -> Self
    where
        F: Fn(&MatchCtx, NodeId) -> bool + Send + Sync + 'static,
    {
        Self::Fixed {
            pats: vec![lhs, rhs],
            commutative: Arc::new(f),
        }
    }
}

impl DataPattern for NodePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        let snap = b.clone();

        // 1) Kind check (may bind typed-constant values).
        if !(self.kind_match)(ctx, node, b) {
            *b = snap;
            return false;
        }

        // 2) Match inputs, with snapshot/restore around the whole thing so
        //    the post_match + captures also participate in the commutative
        //    retry.
        let after_kind = b.clone();

        if try_once(self, ctx, node, target, b, false) {
            return true;
        }

        // Commutative retry (arity-2 Fixed only, commutative decider true).
        if let InputsSpec::Fixed { pats, commutative } = &self.inputs
            && pats.len() == 2
            && commutative(ctx, node)
        {
            *b = after_kind;
            if try_once(self, ctx, node, target, b, true) {
                return true;
            }
        }

        *b = snap;
        false
    }
}

fn try_once(
    pat: &NodePat,
    ctx: &MatchCtx,
    node: NodeId,
    target: NodeOutputId,
    b: &mut Bindings,
    swap: bool,
) -> bool {
    // (a) match inputs
    match &pat.inputs {
        InputsSpec::None => {}
        InputsSpec::Fixed { pats, commutative: _ } => {
            let inputs = ctx.graph.graph.node_inputs(node);
            if inputs.len() != pats.len() {
                return false;
            }
            if swap {
                // only valid for arity 2
                let (Some(&i0), Some(&i1)) = (inputs.get(0), inputs.get(1)) else {
                    return false;
                };
                if pats.len() != 2 {
                    return false;
                }
                if !match_one(ctx, i1, &pats[0], b) {
                    return false;
                }
                if !match_one(ctx, i0, &pats[1], b) {
                    return false;
                }
            } else {
                for (i, sub_pat) in pats.iter().enumerate() {
                    let Some(&inp) = inputs.get(i) else {
                        return false;
                    };
                    if !match_one(ctx, inp, sub_pat, b) {
                        return false;
                    }
                }
            }
        }
        InputsSpec::Indexed(items) => {
            let inputs = ctx.graph.graph.node_inputs(node);
            for (i, p) in items {
                let Some(&inp) = inputs.get(*i) else {
                    return false;
                };
                if !match_one(ctx, inp, p, b) {
                    return false;
                }
            }
        }
    }

    // (b) post_match (op-var binding for *Any patterns)
    if let Some(pm) = &pat.post_match
        && !pm(ctx, node, b)
    {
        return false;
    }

    // (c) output/node captures — output_var first, then node_var
    if let Some(v) = pat.output_var
        && !b.bind_var(v, target)
    {
        return false;
    }
    if let Some(nv) = pat.node_var
        && !b.bind_node_var(nv, node)
    {
        return false;
    }

    true
}

fn match_one(ctx: &MatchCtx, out: NodeOutputId, pat: &crate::pat::Pat, b: &mut Bindings) -> bool {
    ctx.matcher.match_output(out, pat, b)
}
