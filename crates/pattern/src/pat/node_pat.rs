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
///
/// The closure is only ever invoked after [`NodePat::root_kind`] has already
/// accepted the candidate node's discriminant — so it can focus on payload
/// checks (matching a specific op variant, space, offset, …) and optional
/// binding side-effects (`IntoAnyIntConst` capturing an IntVar value).
pub(crate) type NodeKindCheck =
    Arc<dyn Fn(&MatchCtx, NodeId, &mut Bindings) -> bool + Send + Sync>;

/// Fast-path root-kind hint on [`NodePat`].  [`crate::matcher::Matcher::find_all`]
/// uses it to skip candidate nodes whose `NodeKind` is incompatible with
/// the pattern, avoiding any per-candidate closure dispatch or allocation.
/// [`NodePat::try_match_common`] also short-circuits on it, so
/// [`crate::matcher::Matcher::match_at`] callers (e.g. rewrite rules) still
/// get the fast-fail path.
#[derive(Clone, Copy)]
pub enum KindFilter {
    /// Pattern can accept any `NodeKind` — the `kind_match` closure is
    /// the sole authority.  Used by wildcards ([`crate::pat::any`],
    /// [`crate::pat::var`]) and the match-only-false `*_const_with_fn`
    /// builders.
    Any,
    /// Pattern only accepts nodes whose discriminant equals the stored
    /// one.  Nearly every leaf `NodePat` falls here.
    Single(std::mem::Discriminant<NodeKind>),
}

impl KindFilter {
    /// Convenience: build a `Single` filter from an exemplar `NodeKind`.
    /// The payload of `exemplar` is ignored — only the discriminant matters.
    #[inline]
    pub(crate) fn exact(exemplar: &NodeKind) -> Self {
        Self::Single(std::mem::discriminant(exemplar))
    }

    /// Returns `true` if a node with the given `kind` is acceptable.
    #[inline]
    pub(crate) fn accepts(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Single(d) => *d == std::mem::discriminant(kind),
        }
    }
}

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
    /// Fast-path filter on the candidate node's `NodeKind` discriminant.
    /// Consulted before any clone or closure call — both by
    /// [`crate::matcher::Matcher::find_all`] (to skip incompatible
    /// candidate roots) and by [`Self::try_match_common`] (to fail fast
    /// at the top).  Every ctor sets this; patterns whose root kind
    /// genuinely varies use [`KindFilter::Any`] and rely on `kind_match`.
    pub(crate) root_kind: KindFilter,
    /// Checks the node's kind (and any kind-embedded data — e.g. constants,
    /// varnodes, spaces, offsets, phi offsets). May bind typed-constant
    /// values or operator variants.
    ///
    /// **Invariant** — when `inputs` is `InputsSpec::Fixed { pats, commutative }`
    /// with `pats.len() == 2` and `commutative(...)` can return `true`, this
    /// closure must NOT perform bindings that could vary between the forward
    /// and swapped attempt.  The commutative retry path restores bindings to
    /// the post-`kind_match` snapshot and re-runs input matching only — if
    /// `kind_match` could bind differently on re-entry the snapshot would not
    /// be restored.  Patterns that need to bind should do so in `post_match`
    /// instead (see `variant_agnostic.rs` for the canonical example).
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

    fn root_kind_filter(&self) -> KindFilter {
        self.root_kind
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
            let mark = b.mark();
            if self.try_match(ctx, out, b) {
                return true;
            }
            b.restore(mark);
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
    /// Minimal matcher-only `NodePat` with every build-side / capture-side
    /// field set to its default.  Chain `.with_*` setters to populate only
    /// the fields a particular ctor actually uses.
    ///
    /// `root_kind` advertises the node discriminant the pattern accepts;
    /// pass [`KindFilter::Any`] for patterns that genuinely match across
    /// kinds (match-only-false build ctors and wildcards).
    pub(crate) fn matcher(
        root_kind: KindFilter,
        kind_match: NodeKindCheck,
        inputs: InputsSpec,
    ) -> Self {
        Self {
            root_kind,
            kind_match,
            kind_build: None,
            build_result_ty: BuildTy::InheritRoot,
            inputs,
            outputs: OutputsSpec::None,
            consumers: ConsumersSpec::None,
            post_match: None,
            output_var: None,
            node_var: None,
        }
    }

    pub(crate) fn with_build(mut self, b: NodeKindBuilder) -> Self {
        self.kind_build = Some(b);
        self
    }

    pub(crate) fn with_build_ty(mut self, t: BuildTy) -> Self {
        self.build_result_ty = t;
        self
    }

    pub(crate) fn with_outputs(mut self, o: OutputsSpec) -> Self {
        self.outputs = o;
        self
    }

    pub(crate) fn with_consumers(mut self, c: ConsumersSpec) -> Self {
        self.consumers = c;
        self
    }

    pub(crate) fn with_post_match(mut self, pm: NodeKindCheck) -> Self {
        self.post_match = Some(pm);
        self
    }

    pub(crate) fn with_output_var(mut self, v: Option<crate::var::Var>) -> Self {
        self.output_var = v;
        self
    }

    pub(crate) fn with_node_var(mut self, nv: Option<crate::var::NodeVar>) -> Self {
        self.node_var = nv;
        self
    }

    pub(crate) fn into_pat(self) -> crate::pat::Pat {
        crate::pat::Pat::from_dyn(Arc::new(self))
    }

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
        // Structural fast-path: if the candidate node's discriminant is
        // outside this pattern's accepted kinds, there's no chance of
        // matching.  `find_all` already does this filter on the preorder
        // loop, but guarding here too protects `match_at` callers
        // (rewrite rules) and any direct `Matcher::match_node_id` recursion.
        if !self.root_kind.accepts(ctx.graph.graph.node_kind(node)) {
            return false;
        }

        // Fail-fast before any snapshot.  `kind_match` contract: must not
        // mutate `b` on a false return.  All current implementations satisfy
        // this — they either do a pure `matches!` check or call a `bind_*`
        // helper, and the binders are themselves no-ops on conflict.
        if !(self.kind_match)(ctx, node, b) {
            return false;
        }

        // Single journal mark used for both the commutative retry and the
        // total-failure rollback.  Taken after `kind_match` so any bindings
        // it performed (e.g. `IntVar` capture in `IntoAnyIntConst`) survive
        // a commutative retry of the input arm.  `BindingsMark` is `Copy`,
        // so reusing it across the retry restore and the final rollback
        // needs no clone.
        let after_kind = b.mark();

        if try_once(self, ctx, node, target, b, false) {
            return true;
        }

        if let InputsSpec::Fixed { pats, commutative } = &self.inputs
            && pats.len() == 2
            && commutative(ctx, node)
        {
            b.restore(after_kind);
            if try_once(self, ctx, node, target, b, true) {
                return true;
            }
        }

        b.restore(after_kind);
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
            // `swap` is only ever true when `pats.len() == 2` (enforced by
            // caller) — but re-assert defensively: the swapped arm would
            // otherwise read stale pat indices.
            if swap && pats.len() != 2 {
                return false;
            }
            for (pat_idx, sub_pat) in pats.iter().enumerate() {
                let inp_idx = if swap { 1 - pat_idx } else { pat_idx };
                let Some(&inp) = inputs.get(inp_idx) else {
                    return false;
                };
                if !match_one(ctx, inp, sub_pat, b) {
                    return false;
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

    // (b,c) outputs + consumers — both index into the same slice, fetch once.
    let needs_outputs = matches!(pat.outputs, OutputsSpec::Indexed(_))
        || matches!(pat.consumers, ConsumersSpec::Indexed(_));
    let outputs = needs_outputs.then(|| ctx.graph.graph.node_outputs(node));

    if let (OutputsSpec::Indexed(items), Some(outputs)) = (&pat.outputs, &outputs) {
        for (i, p) in items {
            let Some(&out) = outputs.get(*i) else {
                return false;
            };
            if !match_one(ctx, out, p, b) {
                return false;
            }
        }
    }

    if let (ConsumersSpec::Indexed(items), Some(outputs)) = (&pat.consumers, &outputs) {
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
