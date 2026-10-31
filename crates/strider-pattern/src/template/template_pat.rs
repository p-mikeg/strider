//! The build-side mirror of [`MatchPat`](crate::MatchPat), implemented only by
//! the buildable typed patterns. That restriction is what makes a wildcard in
//! a rewrite RHS a compile error: `rewrite_rule<L, T: TemplatePat>` cannot
//! accept one.

use crate::expr::{Expr, TemplateExpr};
use crate::template::{Template, TemplateBuilder, TmplValueRef};

/// A build-side `var()` and its `.capture()` are templates:
///
/// ```
/// use strider_pattern::{Capture, CaptureExt, TemplatePat, template, var};
/// let (a, b) = (Capture::new(), Capture::new());
/// TemplatePat::into_template(template::int_add(var(a).capture(b), var(a)));
/// ```
///
/// A wildcard is not:
///
/// ```compile_fail,E0277
/// use strider_pattern::{Capture, TemplatePat, anything, template, var};
/// TemplatePat::into_template(template::int_add(anything(), var(Capture::new())));
/// ```
///
/// Nor is a composite's `.capture()`, which would drop its operands:
///
/// ```compile_fail,E0277
/// use strider_pattern::{Capture, CaptureExt, TemplatePat, template, var};
/// let c = Capture::new();
/// TemplatePat::into_template(template::int_add(var(c), var(c)).capture(c));
/// ```
pub trait TemplatePat: Sized + Send {
    /// Returns the value-output handle of the lowered root node.
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef;

    fn into_template(self) -> Template {
        let mut b = TemplateBuilder::new();
        self.compile(&mut b);
        b.finish()
    }

    /// The pattern as an operand of another. Defaults to lowering through
    /// [`compile`](Self::compile).
    #[doc(hidden)]
    fn expr(self) -> Expr
    where
        Self: 'static,
    {
        Expr::lowered_template(self)
    }
}

impl<T: TemplateExpr> TemplatePat for T {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        self.into_expr().lower_template(b)
    }

    fn expr(self) -> Expr {
        self.into_expr()
    }
}
