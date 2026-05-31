//! Generic node-level pattern.  One [`NodePat`] struct covers every pattern
//! shape (data and control), parameterised by:
//!
//! * [`KindSpec`] — kind-level constraint on the candidate node's
//!   `NodeKind`.  Data-driven for `Any` / `Variant` / `Exact`; carries a
//!   payload-only closure in `VariantWith`.  Kind-phase is pure — it can
//!   never touch [`Bindings`], which makes the commutative-retry rollback
//!   trivially safe.
//! * [`InputsSpec`] — how to match the node's inputs.  `None` handles
//!   zero-input patterns (constants, `InitialVar`, `FunctionArg`); `Fixed`
//!   covers unary / binary / cmp ops with optional commutative retry;
//!   `Indexed` covers sparse positional matching for memory ops (`Load` /
//!   `Store`), `Phi`, and the control
//!   patterns (`Call`, `CallOther`, `Return`, `If`).
//! * [`NodePat::outputs`] — sparse positional sub-pattern constraints on
//!   specific output slots (used by `Call` for return-value captures and
//!   `CallOther` for clobber-output captures).  Empty means "no output
//!   constraints" — the matcher's iteration is a no-op.
//! * [`NodePat::post_match`] — the one place bindings can be installed
//!   during the match pipeline (op-variant captures, typed-const captures,
//!   side-table lookups).
//! * [`BuildSpec`] (optional) — build-side spec for use as a rewrite-rule
//!   RHS.  `None` means "match-only".

use std::sync::Arc;

use entity_utils::DenseEntitySet;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};

use crate::pattern::error::Result;
use crate::pattern::matcher::Bindings;
use crate::pattern::pat::traits::{BuildCtx, BuildOutcome, MatchCtx, Pattern};

/// Post-match hook closure used by [`NodePat::post_match`].
///
/// Post-match runs after the kind spec has already accepted the candidate
/// and all input / output constraints have passed — it is the
/// place for bindings that depend on payload data (e.g. the `*_any`
/// op-variant capture binding the matched node's `NodeId`).
pub(crate) type PostMatchFn =
    Arc<dyn Fn(&MatchCtx, NodeId, &mut Bindings) -> bool>;

/// Kind-level constraint carried by every [`NodePat`].
///
/// Dispatch has two phases:
/// * [`discriminant`](Self::discriminant) — O(1), closure-free.
///   Used by [`crate::pattern::matcher::Matcher::find_all`] to look up the
///   matcher's lazy kind-index bucket for the spec's root kind,
///   skipping every node with a different `NodeKind` discriminant.
/// * [`matches`](Self::matches) — full check (discriminant + payload).
///   Used by [`NodePat::try_match_common`] to gate the whole match.
///
/// The `VariantWith` closure is payload-only (`&NodeKind -> bool`) — it
/// cannot read graph side tables.  Patterns that need side-table access
/// use a [`NodePat::post_match`] hook on top of a `VariantWith` kind spec.
#[derive(Clone)]
pub(crate) enum KindSpec {
    /// Accepts any `NodeKind`.  Used by wildcards and by the match-only-false
    /// `*_const_with_fn` builders (whose `try_match` never succeeds anyway).
    Any,
    /// Matches a `NodeKind` variant by discriminant, ignoring the payload.
    /// (e.g. `load()` with no space constraint accepts any `Load(_)`.)
    Variant(std::mem::Discriminant<NodeKind>),
    /// Matches a `NodeKind` value exactly (discriminant + payload equality).
    /// (e.g. `int_const(5)`, `add`, `int_binary(Add, …)`.)
    Exact(NodeKind),
    /// Variant match plus a payload-only predicate.  Used when a pattern
    /// constrains some but not all payload fields (e.g. `load().space(S)`).
    VariantWith {
        discriminant: std::mem::Discriminant<NodeKind>,
        check: Arc<dyn Fn(&NodeKind) -> bool>,
    },
}

impl KindSpec {
    /// Build a `Variant` spec from an exemplar.  The payload is ignored —
    /// only the discriminant is retained.
    #[inline]
    pub(crate) fn variant(exemplar: &NodeKind) -> Self {
        Self::Variant(std::mem::discriminant(exemplar))
    }

    /// Build a `VariantWith` spec with a payload-only predicate.
    pub(crate) fn variant_with<F>(exemplar: &NodeKind, check: F) -> Self
    where
        F: Fn(&NodeKind) -> bool + 'static,
    {
        Self::VariantWith {
            discriminant: std::mem::discriminant(exemplar),
            check: Arc::new(check),
        }
    }

    /// Returns the unique `NodeKind` discriminant this spec accepts,
    /// or `None` for the wildcard [`Self::Any`].  Used by
    /// [`crate::pattern::matcher::Matcher::find_all`] to look up the kind
    /// index's bucket for concrete-rooted patterns rather than scanning
    /// every node.
    #[inline]
    pub(crate) fn discriminant(&self) -> Option<std::mem::Discriminant<NodeKind>> {
        match self {
            Self::Any => None,
            Self::Variant(d) | Self::VariantWith { discriminant: d, .. } => Some(*d),
            Self::Exact(k) => Some(std::mem::discriminant(k)),
        }
    }

    /// Full check: discriminant + payload.
    #[inline]
    pub(crate) fn matches(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Variant(d) => *d == std::mem::discriminant(kind),
            Self::Exact(k) => k == kind,
            Self::VariantWith { discriminant, check } => {
                *discriminant == std::mem::discriminant(kind) && check(kind)
            }
        }
    }
}

/// Arbitrary `rsleigh::Vn` used as a payload-don't-care exemplar when
/// constructing `NodeKind` variants for discriminant-only purposes
/// (e.g. `KindSpec::variant(&NodeKind::Load(VnSpace::RAM))`).
#[inline]
pub(crate) fn exemplar_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        size: 0,
        addr_off: 0,
        addr_space: rsleigh::VnSpace::CONST,
    }
}

/// Runtime decider for whether an arity-2 [`InputsSpec::Fixed`] match should
/// retry with swapped operands.  Concrete ops fix the answer at construction
/// (`|_, _| true` or `|_, _| false`); `*Any` patterns inspect the matched op
/// variant to decide per-match.
pub(crate) type CommutativeDecider =
    Arc<dyn Fn(&MatchCtx, NodeId) -> bool>;

/// Closure that produces a concrete [`NodeKind`] at build time, reading
/// any needed captures / root-type info out of the [`BuildCtx`].  Used
/// in [`BuildKind::Fn`].
pub(crate) type NodeKindBuilder =
    Arc<dyn Fn(&BuildCtx<'_>) -> Result<NodeKind>>;

/// How to pick the output type of a node built by [`NodePat::try_build`].
pub(crate) enum BuildTy {
    /// Use the root-type parameter threaded through `try_build`.
    InheritRoot,
    /// Use a specific type regardless of root (cmps, bool ops, bool const).
    Fixed(NodeOutputType),
}

/// How to obtain the `NodeKind` at build time.
pub(crate) enum BuildKind {
    /// Emit a fixed literal — no captures needed (constants with known
    /// value, concrete-op binary/unary/cmp patterns, unit-variant casts).
    Exact(NodeKind),
    /// Compute the `NodeKind` at build time from the [`BuildCtx`]
    /// (variant-agnostic `*_any` ops, `*_const_with_fn` builders).
    Fn(NodeKindBuilder),
}

/// Build-side specification for a [`NodePat`].  Present iff the pattern is
/// buildable (usable on the RHS of a rewrite rule).
pub(crate) struct BuildSpec {
    pub(crate) kind: BuildKind,
    pub(crate) ty: BuildTy,
}

/// A generic node-level pattern. Covers every pattern shape: "check node
/// kind, match inputs in some arrangement, optionally constrain outputs,
/// optionally bind output/node captures".  Buildable patterns
/// additionally populate [`NodePat::build`].
///
/// Kind-phase purity: the [`KindSpec::VariantWith`] closure is
/// payload-only (`&NodeKind -> bool`) and can therefore never touch
/// [`Bindings`].  All data-dependent binding happens strictly in
/// [`Self::post_match`], which runs after the inputs/outputs
/// pass — so the commutative retry path can safely snapshot-restore
/// without worrying about bindings made during the kind check.
pub(crate) struct NodePat {
    /// Kind-level constraint consulted by both [`crate::pattern::matcher::Matcher::find_all`]
    /// (prefilter, discriminant-only) and [`Self::try_match_common`] (full
    /// check, discriminant + payload).  Every ctor sets this; wildcards
    /// and the match-only-false `*_const_with_fn` builders use [`KindSpec::Any`].
    pub(crate) kind: KindSpec,
    /// Build-side specification.  `None` means "match-only" (default for
    /// wildcards, control patterns, memory ops, phis — none of which
    /// current rules need to build).
    pub(crate) build: Option<BuildSpec>,
    /// How to match the node's inputs.
    pub(crate) inputs: InputsSpec,
    /// Sparse positional constraints on the node's outputs.  Empty means
    /// "no output constraints"; non-empty entries match each listed
    /// `(slot_idx, sub_pattern)` pair against the node's output at that
    /// slot.  Used by `CallPat::ret_output` and `CallOtherPat::ret` to
    /// capture / constrain specific return-value / clobber outputs.
    pub(crate) outputs: Vec<(usize, crate::pattern::pat::Pat)>,
    /// Runs AFTER inputs/outputs match (and after each commutative
    /// retry).  This is the designated binding site for payload-dependent
    /// captures (op-variant Vars, typed-constant Vars), since it executes
    /// once bindings from sub-matches are already in place.
    pub(crate) post_match: Option<PostMatchFn>,
}

pub(crate) enum InputsSpec {
    /// Arity 0: constants, `InitialVar`, `FunctionArg`.
    None,
    /// Arity N with ordered operand matching. When `commutative(ctx, node)`
    /// returns true and the node has exactly 2 inputs, both orderings are
    /// tried.
    Fixed {
        pats: Vec<crate::pattern::pat::Pat>,
        commutative: CommutativeDecider,
    },
    /// Sparse positional constraints: only the listed input indices are
    /// matched against their sub-patterns; unlisted slots are unconstrained.
    Indexed(Vec<(usize, crate::pattern::pat::Pat)>),
}

impl InputsSpec {
    /// Fixed arity, never commutative.
    pub(crate) fn fixed_ordered(pats: Vec<crate::pattern::pat::Pat>) -> Self {
        Self::Fixed {
            pats,
            commutative: Arc::new(|_ctx, _node| false),
        }
    }

    /// Fixed arity-2, always commutative (concrete op is known to be so).
    pub(crate) fn fixed_commutative(lhs: crate::pattern::pat::Pat, rhs: crate::pattern::pat::Pat) -> Self {
        Self::Fixed {
            pats: vec![lhs, rhs],
            commutative: Arc::new(|_ctx, _node| true),
        }
    }

    /// Fixed arity-2, commutative decided at match time by `f(ctx, node)`.
    pub(crate) fn fixed_maybe_commutative<F>(
        lhs: crate::pattern::pat::Pat,
        rhs: crate::pattern::pat::Pat,
        f: F,
    ) -> Self
    where
        F: Fn(&MatchCtx, NodeId) -> bool + 'static,
    {
        Self::Fixed {
            pats: vec![lhs, rhs],
            commutative: Arc::new(f),
        }
    }
}

impl Pattern for NodePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.function.node_for_output(target);
        self.try_match_common(ctx, node, b)
    }

    fn kind_spec(&self) -> KindSpec {
        self.kind.clone()
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let outputs = ctx.function.node_outputs(node);
        if outputs.is_empty() {
            // Zero-output nodes (e.g. `Return`) can't be reached via the
            // default "iterate outputs" loop; match directly against the
            // node.
            return self.try_match_common(ctx, node, b);
        }
        for &out in outputs {
            let mark = b.mark();
            if self.try_match(ctx, out, b) {
                return true;
            }
            b.restore(mark);
        }
        false
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        let Some(build) = &self.build else {
            return Err(crate::pattern::error::not_buildable(std::any::type_name::<Self>()));
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
                    let BuildOutcome::Out(o) = p.as_dyn().try_build(ctx)?;
                    out.push(o);
                }
                out
            }
            InputsSpec::Indexed(_) => {
                return Err(crate::pattern::error::not_buildable(std::any::type_name::<Self>()));
            }
        };

        let kind = match &build.kind {
            BuildKind::Exact(k) => *k,
            BuildKind::Fn(f) => f(ctx)?,
        };
        let ty = match build.ty {
            BuildTy::InheritRoot => ctx.root_ty,
            BuildTy::Fixed(t) => t,
        };
        let out = ctx.function.make_value_node(kind, input_outs, ty)?;
        Ok(BuildOutcome::Out(out))
    }
}

impl NodePat {
    /// Minimal matcher-only `NodePat` with every build-side / capture-side
    /// field set to its default.  Chain `.with_*` setters to populate only
    /// the fields a particular ctor actually uses.
    ///
    /// `kind` advertises the node kind the pattern accepts; use
    /// [`KindSpec::Any`] for patterns that genuinely match across kinds
    /// (match-only-false build ctors and wildcards).
    pub(crate) fn matcher(kind: KindSpec, inputs: InputsSpec) -> Self {
        Self {
            kind,
            build: None,
            inputs,
            outputs: Vec::new(),
            post_match: None,
        }
    }

    /// Install a build-side spec producing a literal `NodeKind`.  Used by
    /// every fixed-op / fixed-constant ctor.  `ty` is baked in at the
    /// same call — the kind and its output type always travel together.
    pub(crate) fn with_build_exact(mut self, k: NodeKind, ty: BuildTy) -> Self {
        self.build = Some(BuildSpec { kind: BuildKind::Exact(k), ty });
        self
    }

    /// Install a build-side spec that computes the `NodeKind` at build time
    /// (variant-agnostic and `*_const_with_fn` cases).  `ty` is baked in
    /// at the same call.
    pub(crate) fn with_build_fn(mut self, f: NodeKindBuilder, ty: BuildTy) -> Self {
        self.build = Some(BuildSpec { kind: BuildKind::Fn(f), ty });
        self
    }

    pub(crate) fn with_outputs(mut self, outputs: Vec<(usize, crate::pattern::pat::Pat)>) -> Self {
        self.outputs = outputs;
        self
    }

    pub(crate) fn with_post_match(mut self, pm: PostMatchFn) -> Self {
        self.post_match = Some(pm);
        self
    }

    pub(crate) fn into_pat(self) -> crate::pattern::pat::Pat {
        crate::pattern::pat::Pat::from_dyn(Arc::new(self))
    }

    /// Core match pipeline shared by output-rooted (`try_match`) and
    /// node-rooted (`try_match_node` for zero-output nodes) entry points.
    fn try_match_common(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        b: &mut Bindings,
    ) -> bool {
        // Kind gate: discriminant + payload check in one go.  The kind spec
        // is closure-free for `Any`/`Variant`/`Exact` and runs a payload-only
        // predicate for `VariantWith`.  `find_all` already prefilters by
        // discriminant for speed; guarding here too covers `match_at`
        // callers (rewrite rules) and direct `Matcher::match_node_id`
        // recursion.  Because the kind check can never mutate `b`, we don't
        // need a snapshot before it.
        if !self.kind.matches(ctx.function.node_kind(node)) {
            return false;
        }

        // Single journal mark used for both the commutative retry and the
        // total-failure rollback.  `BindingsMark` is `Copy`, so reusing it
        // across the retry restore and the final rollback needs no clone.
        let before_inputs = b.mark();

        if try_once(self, ctx, node, b, false) {
            return true;
        }

        if let InputsSpec::Fixed { pats, commutative } = &self.inputs
            && pats.len() == 2
            && commutative(ctx, node)
        {
            b.restore(before_inputs);
            if try_once(self, ctx, node, b, true) {
                return true;
            }
        }

        b.restore(before_inputs);
        false
    }
}

fn try_once(
    pat: &NodePat,
    ctx: &MatchCtx,
    node: NodeId,
    b: &mut Bindings,
    swap: bool,
) -> bool {
    // (a) match inputs
    match &pat.inputs {
        InputsSpec::None => {}
        InputsSpec::Fixed { pats, commutative: _ } => {
            let inputs = ctx.function.node_inputs(node);
            if inputs.len() != pats.len() {
                return false;
            }
            // Caller (`try_match_common`) enforces `pats.len() == 2`
            // before setting `swap = true`; the `inputs.get(inp_idx)?`
            // bound check below catches any contract regression.
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
            let inputs = ctx.function.node_inputs(node);
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

    // (b) outputs — sparse positional constraints, lazily fetched.
    if !pat.outputs.is_empty() {
        let outputs = ctx.function.node_outputs(node);
        for (i, p) in &pat.outputs {
            let Some(&out) = outputs.get(*i) else {
                return false;
            };
            if !match_one(ctx, out, p, b) {
                return false;
            }
        }
    }

    // (c) post_match (op-var binding for *Any patterns)
    if let Some(pm) = &pat.post_match
        && !pm(ctx, node, b)
    {
        return false;
    }

    true
}

/// Match `pat` against the value produced by `out`.  Delegates to the
/// matcher's `match_output_with_walk_through` so the walk-through
/// behavior (gated on `MatcherOptions`) is consistent across every
/// recursion path — direct match first, then cast walk-through, then
/// Region walk-through.
fn match_one(ctx: &MatchCtx, out: NodeOutputId, pat: &crate::pattern::pat::Pat, b: &mut Bindings) -> bool {
    ctx.matcher.match_output_with_walk_through(out, pat, b)
}

/// Match `pat` against a forward-step consumer node (used by
/// `IfPattern` for `IfPat::true_branch` / `false_branch`).  Honors
/// [`crate::pattern::matcher::MatcherOptions::ignore_regions`]: if the
/// direct match fails and the consumer is a `Region`, retry
/// against the single consumer of the Region's control output.
pub(crate) fn match_consumer_node(
    ctx: &MatchCtx,
    node: NodeId,
    pat: &crate::pattern::pat::Pat,
    b: &mut Bindings,
) -> bool {
    // Walk forward through single-consumer `Region` headers iteratively.
    // A `DenseEntitySet` visited-set bounds the walk to distinct nodes and
    // bails cleanly the instant a control back-edge would revisit one, so a
    // cyclic CFG can neither loop forever nor grow the stack.
    let mark = b.mark();
    let mut visited: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut cur = node;
    loop {
        if !visited.insert(cur) {
            // Already tried this node on this walk — a control cycle; the
            // pattern doesn't match along it.  Bindings are at `mark`.
            return false;
        }
        if ctx.matcher.match_node_id(cur, pat, b) {
            return true;
        }
        b.restore(mark);
        if !ctx.matcher.options.ignore_regions {
            return false;
        }
        // Region's outputs are [Control, PhiToken]; the Control
        // output is the one consumed by the next region's body.
        if !matches!(ctx.function.node_kind(cur), NodeKind::Region) {
            return false;
        }
        let outputs = ctx.function.node_outputs(cur);
        let Some(&ctrl_out) = outputs.iter().find(|out| {
            matches!(
                ctx.function.output_kind(**out),
                strider_ir::node::NodeOutputKind::Control
            )
        }) else {
            return false;
        };
        // Inline single-consumer lookup: walk forward to the unique
        // consumer of `ctrl_out`.  Returns `None` (and so bails the walk)
        // when the output has zero or multiple consumers — we refuse to
        // pick arbitrarily when a control output forks.
        let mut uses = ctx.function.output_uses(ctrl_out);
        let Some(first) = uses.next() else {
            return false;
        };
        if uses.next().is_some() {
            return false;
        }
        cur = first.0;
    }
}
