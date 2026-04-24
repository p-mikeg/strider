//! Generic node-level data pattern. Covers the vast majority of patterns
//! (data and control) with a single struct parameterised by a kind-match
//! closure, an `InputsSpec`, an `OutputsSpec`, a `ConsumersSpec`, and
//! optional post-match / capture hooks.
//!
//! `InputsSpec::None` handles zero-input patterns (constants, `InitialVar`,
//! `FunctionArg`); `InputsSpec::Fixed` covers unary / binary / cmp ops with
//! optional commutative retry; `InputsSpec::Indexed` covers sparse
//! positional matching for memory ops (`Load` / `Store` / `StackStore` /
//! `StackStorePhi`), `Phi`, and the control patterns (`Call`, `CallOther`,
//! `Return`, `If`).
//!
//! `OutputsSpec::Indexed` constrains the `NodeOutputId` at specific output
//! positions — used by `Call` for return-value captures.
//!
//! `ConsumersSpec::Indexed` constrains the single consumer node of a given
//! output — used by `If` for branch successors (direct-step forward walk).

use std::sync::Arc;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};

use crate::error::{Error, Result};
use crate::matcher::Bindings;
use crate::matcher::walk;
use crate::pat::traits::{BuildCtx, BuildOutcome, MatchCtx, Pattern};
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

/// Closure that produces a concrete [`NodeKind`] at build time, reading
/// any needed captures / root-type info out of the [`BuildCtx`].  Used by
/// [`NodePat::kind_build`].
pub(crate) type NodeKindBuilder =
    Arc<dyn Fn(&BuildCtx<'_>) -> Result<NodeKind> + Send + Sync>;

/// How to pick the output type of a node built by [`NodePat::try_build`].
pub enum BuildTy {
    /// Use the root-type parameter threaded through `try_build`.
    InheritRoot,
    /// Use a specific type regardless of root (cmps, bool ops, bool const).
    Fixed(NodeOutputType),
}

/// A generic node-level pattern. Covers every pattern shape: "check node
/// kind, match inputs in some arrangement, optionally constrain outputs
/// and consumers, optionally bind output/node captures".  Buildable
/// patterns additionally populate [`NodePat::kind_build`].
pub struct NodePat {
    /// Checks the node's kind (and any kind-embedded data — e.g. constants,
    /// varnodes, spaces, offsets, phi offsets). May bind typed-constant
    /// values or operator variants.
    pub(crate) kind_match: NodeKindCheck,
    /// Build-side counterpart to `kind_match`: produces the [`NodeKind`] to
    /// materialize.  `None` means "match-only" (default for wildcards,
    /// control patterns, memory ops, phis — none of which current rules
    /// need to build).
    pub(crate) kind_build: Option<NodeKindBuilder>,
    /// Output type picker for the built node.
    pub(crate) build_result_ty: BuildTy,
    /// How to match the node's inputs.
    pub(crate) inputs: InputsSpec,
    /// Optional constraints on the node's outputs (by position).
    pub(crate) outputs: OutputsSpec,
    /// Optional constraints on the single consumer of outputs (by position).
    pub(crate) consumers: ConsumersSpec,
    /// Runs AFTER inputs/outputs/consumers match (and after each commutative
    /// retry) but BEFORE output/node captures.
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
    Fixed {
        pats: Vec<crate::pat::Pat>,
        commutative: CommutativeDecider,
    },
    /// Sparse positional constraints: only the listed input indices are
    /// matched against their sub-patterns; unlisted slots are unconstrained.
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

/// Constraints on output slots by position.  Each entry's sub-pattern is
/// matched against the `NodeOutputId` at that output index.
pub enum OutputsSpec {
    None,
    Indexed(Vec<(usize, crate::pat::Pat)>),
}

/// Constraints on the consumer of an output slot by position.  For each
/// entry, the helper finds the single consumer of `outputs[i]` (via
/// [`walk::next_control_node`]) and matches the sub-pattern as a node.  If
/// the output has zero or multiple consumers, the match fails.
pub enum ConsumersSpec {
    None,
    Indexed(Vec<(usize, crate::pat::Pat)>),
}

impl Pattern for NodePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_common(ctx, node, Some(target), b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let outputs = ctx.graph.graph.node_outputs(node);
        if outputs.is_empty() {
            // Zero-output nodes (e.g. `Return`) can't be reached via the
            // default "iterate outputs" loop; match directly against the
            // node with no target output.
            return self.try_match_common(ctx, node, None, b);
        }
        for out in outputs.into_iter() {
            let snap = b.clone();
            if self.try_match(ctx, out, b) {
                return true;
            }
            *b = snap;
        }
        false
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        let Some(kind_build) = &self.kind_build else {
            return Err(Error::not_buildable(std::any::type_name::<Self>()));
        };

        // Recurse into inputs.  Only `Fixed` inputs are buildable — ordered
        // positional materialization.  `Indexed` is used by memory/phi/
        // control patterns that no rule needs to construct; if we reach one
        // here it's a usage error, surface it as NotBuildable.
        let input_outs: Vec<NodeOutputId> = match &self.inputs {
            InputsSpec::None => Vec::new(),
            InputsSpec::Fixed { pats, .. } => {
                let mut out = Vec::with_capacity(pats.len());
                for p in pats {
                    match p.as_dyn().try_build(ctx)? {
                        BuildOutcome::Out(o) => out.push(o),
                        BuildOutcome::Skip => return Ok(BuildOutcome::Skip),
                    }
                }
                out
            }
            InputsSpec::Indexed(_) => {
                return Err(Error::not_buildable(std::any::type_name::<Self>()));
            }
        };

        let kind = kind_build(ctx)?;
        let ty = match self.build_result_ty {
            BuildTy::InheritRoot => ctx.root_ty,
            BuildTy::Fixed(t) => t,
        };
        let out = ctx.graph.make_value_node(kind, input_outs, ty)?;
        Ok(BuildOutcome::Out(out))
    }
}

impl NodePat {
    /// Core match pipeline shared by output-rooted (`try_match`) and
    /// node-rooted (`try_match_node` for zero-output nodes) entry points.
    /// `target` is the `NodeOutputId` the match started from, if any — it
    /// drives `output_var` binding and is otherwise unused.
    fn try_match_common(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        target: Option<NodeOutputId>,
        b: &mut Bindings,
    ) -> bool {
        let snap = b.clone();

        if !(self.kind_match)(ctx, node, b) {
            *b = snap;
            return false;
        }

        let after_kind = b.clone();

        if try_once(self, ctx, node, target, b, false) {
            return true;
        }

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
    target: Option<NodeOutputId>,
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

    // (b) match outputs by index (NodeOutputId sub-patterns)
    if let OutputsSpec::Indexed(items) = &pat.outputs {
        let outputs = ctx.graph.graph.node_outputs(node);
        for (i, p) in items {
            let Some(&out) = outputs.get(*i) else {
                return false;
            };
            if !match_one(ctx, out, p, b) {
                return false;
            }
        }
    }

    // (c) match single consumer of outputs by index
    if let ConsumersSpec::Indexed(items) = &pat.consumers {
        let outputs = ctx.graph.graph.node_outputs(node);
        for (i, p) in items {
            let Some(&out) = outputs.get(*i) else {
                return false;
            };
            let Some(consumer) = walk::next_control_node(ctx.matcher, out) else {
                return false;
            };
            if !ctx.matcher.match_node_id(consumer, p, b) {
                return false;
            }
        }
    }

    // (d) post_match (op-var binding for *Any patterns)
    if let Some(pm) = &pat.post_match
        && !pm(ctx, node, b)
    {
        return false;
    }

    // (e) output/node captures — output_var only meaningful when matched
    // from an output (target is Some); skipped for zero-output nodes.
    if let (Some(v), Some(tgt)) = (pat.output_var, target)
        && !b.bind_var(v, tgt)
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
