//! The compile-time-typed build-side builder trait.
//!
//! [`TemplatePat`] is the build-side mirror of
//! [`MatchPat`](crate::MatchPat): it is implemented **only** by the
//! buildable typed structs (`Var`, the const structs, the fixed-variant
//! value ops / casts / cmps / unary ops whose operands are themselves
//! `TemplatePat`, and the lowered-shape buildable structs), plus
//! [`Captured<P>`](crate::Captured) where `P: TemplatePat`.
//!
//! It is deliberately **not** implemented for the match-only structs
//! (`Any`, `IntBinaryAny` / the other `*Any` wildcards, `Predicate`,
//! `ValueOfWidth`, `InputsOfWidth`, `Guarded`, `Limited`, `Ordered`) —
//! those have no build form. This is the mechanism that makes a wildcard
//! in a rewrite RHS a **compile error**: `rewrite_rule<L, T:
//! TemplatePat>` cannot accept an RHS built from a match-only struct.

use crate::template::{Template, TemplateBuilder, TmplOutRef};

/// A compile-time-typed build-side pattern that lowers onto the
/// imperative [`TemplateBuilder`].
pub trait TemplatePat: Sized {
    /// Lower this template into `b`, returning the value-output handle of
    /// its root node.
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef;

    /// Seal this template into a finished [`Template`].
    #[must_use]
    fn into_template(self) -> Template {
        let mut b = TemplateBuilder::new();
        let root = self.compile(&mut b);
        b.finish(root)
    }
}

/// Captures the node producing the inner template's root output.
///
/// On the build side, a captured node resolves to its LHS binding at
/// instantiation time — see
/// [`TemplateBuilder::capture_node`](crate::template::TemplateBuilder::capture_node).
impl<P: TemplatePat> TemplatePat for crate::match_pat::Captured<P> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef {
        let o = self.inner.compile(b);
        b.capture_node(o, self.cap);
        o
    }
}
