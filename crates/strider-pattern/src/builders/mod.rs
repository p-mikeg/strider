//! Chained builder facade.  Free functions construct one-node
//! `Pat<R>` values; binary / unary / etc. builders (next commits)
//! compose them by merging child PatGraphs into a parent.

pub mod consts;
pub mod wildcards;

pub use consts::*;
pub use wildcards::*;

use strider_ir::node::NodeKind;

use crate::pat_graph::{PatGraph, Role};

/// Move-only newtype around an owned `PatGraph<R>`.  The role marker
/// `R` controls whether the pattern can be used as a `Template`
/// (`Concrete` only).
pub struct Pat<R: Role>(pub(crate) PatGraph<R>);

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
