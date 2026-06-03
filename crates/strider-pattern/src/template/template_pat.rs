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

use crate::template::{Template, TemplateBuilder, TmplValueRef};

/// A compile-time-typed build-side pattern that lowers onto the
/// imperative [`TemplateBuilder`].
pub trait TemplatePat: Sized {
    /// Lower this template into `b`, returning the value-output handle of
    /// its root node.
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef;

    /// Seal this template into a finished [`Template`].
    fn into_template(self) -> Template {
        let mut b = TemplateBuilder::new();
        self.compile(&mut b);
        b.finish()
    }
}

/// A `.capture(c)` on the build side resolves to the LHS binding for `c`
/// (the captured value re-used verbatim). The capture *replaces* `inner` —
/// a capture is a fresh leaf — so `inner` is not built. See
/// [`TemplateBuilder::capture`](crate::template::TemplateBuilder::capture).
impl<P: TemplatePat> TemplatePat for crate::matcher::match_pat::Captured<P> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // `inner` is intentionally not compiled: the captured value stands
        // in for whatever it wrapped, and a capture is always a leaf.
        b.capture(self.cap)
    }
}
