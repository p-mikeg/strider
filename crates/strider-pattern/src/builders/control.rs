//! Control-flow chained builders: `CallPat`, `CallOtherPat`, `RetPat`,
//! `IfPat`.
//!
//! All four builders accumulate sparse positional sub-pattern
//! constraints on the IR's input slots and emit incoming edges with the
//! right `consumer_slot`.  Sub-patterns are widened to `Pat<Wildcard>`
//! at insertion time; the finalised pattern is `Pat<Wildcard>` (per the
//! plan's note that rewrite rules rarely build a Call / Return / If on
//! the RHS).
//!
//! ## Slot conventions (matches the proven `strider-analyze` semantics)
//!
//! * `Call` inputs: `[ctrl(0), mem(1), target(2), arg0(3), arg1(4), …]`.
//!   `CallPat::arg(i, p)` shifts by +3 so callers address positional
//!   arguments directly.
//! * `CallOther` inputs: `[ctrl(0), mem(1), pcode-arg0(2), …,
//!   implicit-read0(N+2), …]`.  `CallOtherPat::arg(idx, p)` writes the
//!   raw input slot (no shift) so callers can match on control / memory
//!   / pcode-args / implicit-reads uniformly; the convenience aliases
//!   `.ctrl(p)` and `.mem(p)` write slots 0 and 1.
//! * `Return` inputs: `[ctrl(0), mem(1), retval0(2), retval1(3), …]`.
//!   `RetPat::ret_val(i, p)` shifts by +2.  `RetPat::preceded_by(p)`
//!   writes slot 0 (the ctrl input).
//! * `If` inputs: `[ctrl(0), cond(1)]`.  `IfPat::cond(p)` writes slot 1.
//!
//! ## Deferred features (need post_match closure signature widening)
//!
//! * `CallOtherPat::name(s)` — needs `Function::call_other_name(node)`
//!   at match time.
//! * `IfPat::true_branch(p)` / `false_branch(p)` — need a forward-walk
//!   from the matched If's `Control` outputs to their single consumer,
//!   which the current backward-edge matcher doesn't support.

use strider_ir::node::NodeKind;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, Role, Wildcard,
    merge_subgraph,
};

use super::consts::{int_const, int_const_any_of};
use super::Pat;

// ── CallPat ───────────────────────────────────────────────────────────────────

/// Builder for `Call` node patterns.  Created by [`call`].
///
/// `Call` is the lifter's representation of a function call; clobbers
/// caller-saved registers and the memory token.
pub struct CallPat {
    target: Option<Pat<Wildcard>>,
    ctrl: Option<Pat<Wildcard>>,
    mem: Option<Pat<Wildcard>>,
    args: Vec<(usize, Pat<Wildcard>)>,
}

impl CallPat {
    fn new() -> Self {
        Self {
            target: None,
            ctrl: None,
            mem: None,
            args: Vec::new(),
        }
    }

    /// Constrain the call target (`inputs[2]`).
    #[must_use]
    pub fn target(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.target = Some(p.into());
        self
    }

    /// Constrain the call target to the literal address `addr`.
    /// Equivalent to `.target(int_const(addr))`.
    #[must_use]
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// Constrain the call target to any address in `addrs`.  An empty
    /// iterator vacuously fails — matches nothing.  Equivalent to
    /// `.target(int_const_any_of(addrs))`.
    #[must_use]
    pub fn at_any<I>(self, addrs: I) -> Self
    where
        I: IntoIterator<Item = u64>,
    {
        self.target(int_const_any_of(addrs))
    }

    /// Constrain positional argument `idx` (0-based, after `ctrl` /
    /// `mem` / `target`).  Mapped to raw input slot `idx + 3`.
    #[must_use]
    pub fn arg(mut self, idx: usize, p: impl Into<Pat<Wildcard>>) -> Self {
        self.args.push((3 + idx, p.into()));
        self
    }

    /// Constrain the call's control predecessor (`inputs[0]`).
    #[must_use]
    pub fn ctrl(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.ctrl = Some(p.into());
        self
    }

    /// Constrain the call's memory predecessor (`inputs[1]`).
    #[must_use]
    pub fn mem(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.mem = Some(p.into());
        self
    }

    /// Finalise the builder and bind the resulting `Call` node to `c`.
    #[must_use]
    pub fn capture(self, c: crate::Capture) -> Pat<Wildcard> {
        Pat::<Wildcard>::from(self).capture(c)
    }
}

impl From<CallPat> for Pat<Wildcard> {
    fn from(b: CallPat) -> Pat<Wildcard> {
        let CallPat {
            target,
            ctrl,
            mem,
            args,
        } = b;
        let mut indexed: Vec<(usize, Pat<Wildcard>)> = Vec::new();
        if let Some(p) = ctrl {
            indexed.push((0, p));
        }
        if let Some(p) = mem {
            indexed.push((1, p));
        }
        if let Some(p) = target {
            indexed.push((2, p));
        }
        indexed.extend(args);
        finalise_kind(KindSpec::Exact(NodeKind::Call), NodeKind::Call, indexed)
    }
}

/// Construct a fresh [`CallPat`].
#[must_use]
pub fn call() -> CallPat {
    CallPat::new()
}

// ── CallOtherPat ─────────────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns.  Created by [`call_other`].
///
/// `CallOther` represents a user-op (Sleigh `CALLOTHER`) — opaque
/// architecture-specific instructions modelled outside the pcode core.
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    inputs: Vec<(usize, Pat<Wildcard>)>,
    name_filter: Option<String>,
}

impl CallOtherPat {
    fn new() -> Self {
        Self {
            user_op_id: None,
            inputs: Vec::new(),
            name_filter: None,
        }
    }

    /// Constrain the matched node's user-op id (the `CallOther` payload).
    #[must_use]
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.user_op_id = Some(v);
        self
    }

    /// Restrict the match to `CallOther` nodes whose
    /// `Function::call_other_name` equals `name`.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name_filter = Some(name.into());
        self
    }

    /// Constrain `inputs[idx]` of the matched `CallOther`.  Unlike
    /// [`CallPat::arg`], this is the raw input slot — callers address
    /// control / memory / pcode-args / implicit-reads uniformly.  See
    /// module-level docs for the slot layout.
    #[must_use]
    pub fn arg(mut self, idx: usize, p: impl Into<Pat<Wildcard>>) -> Self {
        self.inputs.push((idx, p.into()));
        self
    }

    /// Convenience: match the control input (`inputs[0]`).
    #[must_use]
    pub fn ctrl(self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.arg(0, p)
    }

    /// Convenience: match the memory input (`inputs[1]`).
    #[must_use]
    pub fn mem(self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.arg(1, p)
    }

    /// Finalise the builder and bind the resulting `CallOther` node to `c`.
    #[must_use]
    pub fn capture(self, c: crate::Capture) -> Pat<Wildcard> {
        Pat::<Wildcard>::from(self).capture(c)
    }
}

impl From<CallOtherPat> for Pat<Wildcard> {
    fn from(b: CallOtherPat) -> Pat<Wildcard> {
        let CallOtherPat { user_op_id, inputs, name_filter } = b;
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = match user_op_id {
            None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
            Some(expected) => KindSpec::VariantWith {
                discriminant: std::mem::discriminant(&exemplar),
                check: Box::new(move |k| {
                    matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected)
                }),
            },
        };
        let post_match = name_filter.map(|want| -> crate::pat_graph::PostMatchFn {
            Box::new(move |ctx, node, _ty, _b| {
                ctx.function.call_other_name(node) == Some(want.as_str())
            })
        });
        finalise_kind_with_post(kind, exemplar, inputs, post_match)
    }
}

/// Construct a fresh [`CallOtherPat`].
#[must_use]
pub fn call_other() -> CallOtherPat {
    CallOtherPat::new()
}

// ── RetPat ────────────────────────────────────────────────────────────────────

/// Builder for `Return` node patterns.  Created by [`ret`].
pub struct RetPat {
    preceded_by: Option<Pat<Wildcard>>,
    ret_vals: Vec<(usize, Pat<Wildcard>)>,
}

impl RetPat {
    fn new() -> Self {
        Self {
            preceded_by: None,
            ret_vals: Vec::new(),
        }
    }

    /// Match `p` against the Return's direct ctrl predecessor (`inputs[0]`).
    #[must_use]
    pub fn preceded_by(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.preceded_by = Some(p.into());
        self
    }

    /// Constrain return value at position `idx` (0-based after ctrl and
    /// mem).  Mapped to raw input slot `idx + 2`.
    #[must_use]
    pub fn ret_val(mut self, idx: usize, p: impl Into<Pat<Wildcard>>) -> Self {
        self.ret_vals.push((2 + idx, p.into()));
        self
    }

    /// Finalise the builder and bind the resulting `Return` node to `c`.
    #[must_use]
    pub fn capture(self, c: crate::Capture) -> Pat<Wildcard> {
        Pat::<Wildcard>::from(self).capture(c)
    }
}

impl From<RetPat> for Pat<Wildcard> {
    fn from(b: RetPat) -> Pat<Wildcard> {
        let RetPat { preceded_by, ret_vals } = b;
        let mut indexed: Vec<(usize, Pat<Wildcard>)> = Vec::new();
        if let Some(p) = preceded_by {
            indexed.push((0, p));
        }
        indexed.extend(ret_vals);
        finalise_kind(KindSpec::Exact(NodeKind::Return), NodeKind::Return, indexed)
    }
}

/// Construct a fresh [`RetPat`].
#[must_use]
pub fn ret() -> RetPat {
    RetPat::new()
}

// ── IfPat ─────────────────────────────────────────────────────────────────────

/// Builder for `If` node patterns.  Created by [`if_node`].
///
/// `.cond(p)` constrains the branch condition (input 1).  `.true_branch(p)`
/// / `.false_branch(p)` walk forward to the single consumer of the
/// If's true / false Control output and match `p` against that
/// consumer; both fail the match if the output has zero or multiple
/// consumers (we refuse to pick arbitrarily when a control output
/// forks).
pub struct IfPat {
    cond: Option<Pat<Wildcard>>,
    true_branch: Option<Pat<Wildcard>>,
    false_branch: Option<Pat<Wildcard>>,
}

impl IfPat {
    fn new() -> Self {
        Self {
            cond: None,
            true_branch: None,
            false_branch: None,
        }
    }

    /// Constrain the branch condition (`inputs[1]`).  `inputs[0]` is
    /// the ctrl predecessor.
    #[must_use]
    pub fn cond(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.cond = Some(p.into());
        self
    }

    /// Match `p` against the single consumer of the If's true-branch
    /// `Control` output.  Refuses to match (no fan-in / fan-out
    /// ambiguity) when the output has zero or multiple consumers.
    #[must_use]
    pub fn true_branch(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.true_branch = Some(p.into());
        self
    }

    /// Match `p` against the single consumer of the If's false-branch
    /// `Control` output.
    #[must_use]
    pub fn false_branch(mut self, p: impl Into<Pat<Wildcard>>) -> Self {
        self.false_branch = Some(p.into());
        self
    }

    /// Finalise the builder and bind the resulting `If` node to `c`.
    #[must_use]
    pub fn capture(self, c: crate::Capture) -> Pat<Wildcard> {
        Pat::<Wildcard>::from(self).capture(c)
    }
}

impl From<IfPat> for Pat<Wildcard> {
    fn from(b: IfPat) -> Pat<Wildcard> {
        let IfPat { cond, true_branch, false_branch } = b;
        let mut indexed: Vec<(usize, Pat<Wildcard>)> = Vec::new();
        if let Some(p) = cond {
            indexed.push((1, p));
        }
        // If both branch arms are None, finalise as a plain kind match.
        if true_branch.is_none() && false_branch.is_none() {
            return finalise_kind(KindSpec::Exact(NodeKind::If), NodeKind::If, indexed);
        }
        // Wrap the branch sub-patterns in a post_match closure that walks
        // forward to the single consumer of the chosen Control output.
        //
        // Caveat: the post_match closure receives `b: &Bindings`
        // (immutable), so captures inside the branch sub-patterns
        // cannot be propagated back into the outer match's bindings —
        // they are evaluated against a throwaway `Bindings` and
        // discarded.  Cross-capture sharing across the branch
        // boundary isn't supported from this path.  Patterns that
        // need it should use the parent-level `Pat::when_match`
        // combinator instead.
        let post_match: crate::pat_graph::PostMatchFn =
            Box::new(move |ctx, node, _ty, _b| {
                if let Some(tb) = &true_branch
                    && !match_branch_consumer(ctx, node, 0, tb)
                {
                    return false;
                }
                if let Some(fb) = &false_branch
                    && !match_branch_consumer(ctx, node, 1, fb)
                {
                    return false;
                }
                true
            });
        finalise_kind_with_post(
            KindSpec::Exact(NodeKind::If),
            NodeKind::If,
            indexed,
            Some(post_match),
        )
    }
}

/// Walk forward to the single consumer of the If's Control output at
/// `output_index` and match `pat` against it.  Returns `false` when the
/// output has zero or multiple consumers, or when `pat` doesn't match.
fn match_branch_consumer(
    ctx: &crate::MatchCtx,
    if_node: strider_ir::node::NodeId,
    output_index: usize,
    pat: &Pat<Wildcard>,
) -> bool {
    let outputs = ctx.function.node_outputs(if_node);
    let Some(&out) = outputs.get(output_index) else {
        return false;
    };
    let mut uses = ctx.function.output_uses(out);
    let Some((first, _)) = uses.next() else {
        return false;
    };
    if uses.next().is_some() {
        return false;
    }
    let mut throwaway = crate::Bindings::default();
    crate::PatternExt::try_match_node_id(pat, ctx, first, &mut throwaway)
}

/// Construct a fresh [`IfPat`].
#[must_use]
pub fn if_node() -> IfPat {
    IfPat::new()
}

// ── Shared finaliser ─────────────────────────────────────────────────────────

/// Build a control-family pattern with the given kind spec, build
/// exemplar, and indexed sub-patterns.
fn finalise_kind(
    kind: KindSpec,
    build_exemplar: NodeKind,
    indexed: Vec<(usize, Pat<Wildcard>)>,
) -> Pat<Wildcard> {
    finalise_kind_with_post(kind, build_exemplar, indexed, None)
}

/// Same as [`finalise_kind`] but with an optional `post_match` closure
/// installed on the root pat node.  Used by `CallOtherPat::name` and
/// `IfPat::true_branch / false_branch`.
fn finalise_kind_with_post(
    kind: KindSpec,
    build_exemplar: NodeKind,
    indexed: Vec<(usize, Pat<Wildcard>)>,
    post_match: Option<crate::pat_graph::PostMatchFn>,
) -> Pat<Wildcard> {
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let root = parent.add_node(NodeData {
        kind,
        output_ty: None,
        capture: None,
        post_match,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(build_exemplar),
            ty: BuildTy::InheritRoot,
        }),
        force_ordered: false,
    });
    for (slot, sub) in indexed {
        let sub_root = merge_subgraph(&mut parent, sub.0);
        parent.add_edge(
            sub_root,
            root,
            EdgeData {
                consumer_slot: slot,
                producer_output_slot: 0,
            },
        );
    }
    parent.set_root(root);
    Pat::from_graph(parent)
}
