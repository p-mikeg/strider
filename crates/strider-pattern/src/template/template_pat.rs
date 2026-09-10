//! The build-side mirror of [`MatchPat`](crate::MatchPat), implemented only by
//! the buildable typed structs. That restriction is what makes a wildcard in a
//! rewrite RHS a compile error: `rewrite_rule<L, T: TemplatePat>` cannot accept
//! one.

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

/// A build-side `.capture(c)` reuses the LHS binding for `c` verbatim, so it
/// REPLACES what it wraps rather than wrapping it. Only a `var()` leaf may be wrapped:
/// over a composite the discarded operands would vanish from the RHS in
/// silence, so `template::int_add(..).capture(c)` is a compile error.
impl TemplatePat for crate::matcher::match_pat::Captured<crate::typed::wildcards::Var> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        b.capture(self.cap)
    }
}
