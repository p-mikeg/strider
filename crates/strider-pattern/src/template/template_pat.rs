//! The build-side mirror of [`MatchPat`](crate::MatchPat), implemented only by
//! the buildable typed structs. The match-only structs deliberately do NOT
//! implement it, which is what makes a wildcard in a rewrite RHS a compile
//! error: `rewrite_rule<L, T: TemplatePat>` cannot accept one.

use crate::template::{Template, TemplateBuilder, TmplValueRef};

pub trait TemplatePat: Sized {
    /// Returns the value-output handle of the lowered root node.
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef;

    fn into_template(self) -> Template {
        let mut b = TemplateBuilder::new();
        self.compile(&mut b);
        b.finish()
    }
}

/// A build-side `.capture(c)` reuses the LHS binding for `c` verbatim. Since
/// a capture is always a leaf, it *replaces* `inner` rather than wrapping it.
impl<P: TemplatePat> TemplatePat for crate::matcher::match_pat::Captured<P> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // `inner` is deliberately not compiled: the captured value stands in
        // for whatever it wrapped.
        b.capture(self.cap)
    }
}
