//! Chained builder facade.  Free functions construct one-node
//! `Pat<R>` values; binary / unary / etc. builders (next commits)
//! compose them by merging child PatGraphs into a parent.

pub mod bool_ops;
pub mod casts;
pub mod cmps;
pub mod consts;
pub mod control;
pub mod float_ops;
pub mod function_arg;
pub mod int_ops;
pub mod memory;
pub mod phi;
pub mod unary_ops;
pub mod wildcards;

pub use bool_ops::*;
pub use casts::*;
pub use cmps::*;
pub use consts::*;
pub use control::*;
pub use float_ops::*;
pub use function_arg::*;
pub use int_ops::*;
pub use memory::*;
pub use phi::*;
pub use unary_ops::*;
pub use wildcards::*;

use strider_ir::node::NodeKind;

use crate::pat_graph::{PatGraph, Role};

/// Newtype around an owned `PatGraph<R>`.  The role marker `R`
/// controls whether the pattern can be used as a `Template`
/// (`Concrete` only).
///
/// `Clone`: `PatGraph` is now cheaply cloneable (closures live behind
/// `Rc<dyn Fn>`), so a `Pat` is too.  Cloning is the strider-py
/// wrapper's path for reusing one `Pat` across multiple matcher /
/// rewrite invocations without rebuilding the graph each time.
pub struct Pat<R: Role>(pub(crate) PatGraph<R>);

impl<R: Role> Clone for Pat<R> {
    fn clone(&self) -> Self {
        Pat(self.0.clone())
    }
}

impl<R: Role> Pat<R> {
    /// Finalise a constructed `PatGraph` into a `Pat`, asserting DAG.
    ///
    /// Builders construct a `PatGraph<R>` then funnel it through here
    /// so the cycle check fires at builder-output time rather than at
    /// match time.  The current leaf-only builders produce single-node
    /// graphs (vacuously DAG), but the same finalisation path will be
    /// reused by binary / unary builders that wire edges.
    ///
    /// # Panics
    ///
    /// Panics if the constructed graph contains a cycle.  This is a
    /// builder-bug guard — chained builders only add forward edges, so
    /// reaching this case means the storage layer was misused.
    #[allow(clippy::expect_used)]
    pub(crate) fn from_graph(g: PatGraph<R>) -> Self {
        if let Some(root) = g.root() {
            crate::pat_graph::assert_dag(&g.inner, root)
                .expect("PatGraph passed to Pat::from_graph contains a cycle");
        }
        Pat(g)
    }

    /// Always-safe role widening (`Concrete` → `Wildcard`).
    #[must_use]
    pub fn into_wildcard(self) -> Pat<crate::pat_graph::Wildcard> {
        Pat(self.0.into_wildcard())
    }

    /// After this pattern matches successfully, additionally run `f` with
    /// access to the per-match context, the matched root's output type,
    /// and the full capture bindings.  The match fails if `f` returns
    /// `false`.  For commutative pat nodes that failure triggers the
    /// other-ordering retry automatically.
    ///
    /// Coerces the role to [`Wildcard`] because a custom predicate has no
    /// template counterpart — a guarded pattern cannot be used as the
    /// RHS of a rewrite.
    ///
    /// # Panics
    ///
    /// Panics if the pattern has no root (cannot happen for builders that
    /// funnel through [`Pat::from_graph`], every of which sets a root).
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn when_match<F>(self, f: F) -> Pat<crate::pat_graph::Wildcard>
    where
        F: Fn(
                &crate::MatchCtx,
                strider_ir::node::NodeOutputType,
                &crate::Bindings,
            ) -> bool
            + 'static,
    {
        let mut g = self.0.into_wildcard();
        let root = g.root().expect("Pat has no root");
        let nd = g.inner
            .node_weight_mut(root)
            .expect("root index invalid");
        // Compose with any existing post_match: both must accept the
        // match.  The user-facing `when_match` signature omits the
        // `NodeId` (the upstream `Pat::when_match` is bindings-aware,
        // not node-id-aware); we ignore the `node` parameter at the
        // adapter layer.
        let new_fn: crate::pat_graph::PostMatchFn = if let Some(prev) = nd.post_match.take() {
            std::rc::Rc::new(move |ctx, node, ty, b| prev(ctx, node, ty, b) && f(ctx, ty, b))
        } else {
            std::rc::Rc::new(move |ctx, _node, ty, b| f(ctx, ty, b))
        };
        nd.post_match = Some(new_fn);
        Pat(g)
    }

    /// Bind the matched root to `c`.  Preserves the pattern's role.
    /// For control-flow patterns (`Call`, `If`, `Return`, `CallOther`)
    /// only the [`NodeId`](strider_ir::node::NodeId) is bound; for
    /// value-producing patterns the value
    /// [`NodeOutputId`](strider_ir::node::NodeOutputId) is bound.
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn capture(mut self, c: crate::Capture) -> Self {
        let root = self.0.root().expect("Pat has no root");
        let nd = self
            .0
            .inner
            .node_weight_mut(root)
            .expect("root index invalid");
        nd.capture = Some(c.as_ref());
        self
    }

    /// Convenience: intern (or look up) a [`Capture`] keyed by `name`
    /// and bind the matched root to it.  Repeated calls with the same
    /// name in the same pattern enforce capture-equality across pat
    /// positions.
    #[must_use]
    pub fn cap(self, name: impl AsRef<str>) -> Self {
        let c = crate::Capture::named(name.as_ref());
        self.capture(c)
    }

    /// Mark the root pat node as non-commutative even if its
    /// `NodeKind` would normally be.  The matcher will NOT trigger
    /// operand-order retry on the matched IR node.
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn ordered(mut self) -> Self {
        let root = self.0.root().expect("Pat has no root");
        let nd = self
            .0
            .inner
            .node_weight_mut(root)
            .expect("root index invalid");
        nd.force_ordered = true;
        self
    }
}

// Concrete → Wildcard role widening via `From`.  Mirrors
// [`Pat::into_wildcard`] but lets callers write `.into()` in
// `impl Into<Pat<Wildcard>>` positions (e.g. test helpers,
// assertion fns).  No reverse impl — the role widening is one-way.
impl From<Pat<crate::pat_graph::Concrete>> for Pat<crate::pat_graph::Wildcard> {
    fn from(p: Pat<crate::pat_graph::Concrete>) -> Self {
        p.into_wildcard()
    }
}

// Pattern impl for Pat<R> — delegates to PatGraph<R>.
impl<R: Role> crate::Pattern for Pat<R> {
    fn try_match(
        &self,
        ctx: &crate::MatchCtx,
        root_out: strider_ir::node::NodeOutputId,
        b: &mut crate::Bindings,
    ) -> bool {
        self.0.try_match(ctx, root_out, b)
    }
    fn root_kind_discriminant(&self) -> Option<std::mem::Discriminant<NodeKind>> {
        self.0.root_kind_discriminant()
    }
    fn try_match_node(
        &self,
        ctx: &crate::MatchCtx,
        node: strider_ir::node::NodeId,
        b: &mut crate::Bindings,
    ) -> bool {
        self.0.try_match_node(ctx, node, b)
    }
}

/// Conversion to `Pat<R>`.  Today the chained builders return `Pat<R>`
/// directly so `IntoPat` is just an identity-flavoured helper for
/// builder-arg ergonomics in subsequent commits (binary / unary /
/// call builders accept `impl IntoPat<_>` so callers may pass either
/// a `Pat<R>` directly or a future wrapper type).
pub trait IntoPat<R: Role> {
    fn into_pat(self) -> Pat<R>;
}
impl<R: Role> IntoPat<R> for Pat<R> {
    fn into_pat(self) -> Pat<R> {
        self
    }
}
