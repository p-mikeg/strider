//! The build-side mirror of [`MatchPat`](crate::MatchPat), implemented only by
//! the buildable typed patterns. That restriction is what makes a wildcard in
//! a rewrite RHS a compile error: `rewrite_rule<L, T: TemplatePat>` cannot
//! accept one.

use crate::expr::IntoExpr;
use crate::template::Template;

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
pub trait TemplatePat: IntoExpr + Send {
    fn into_template(self) -> Template {
        self.into_expr().into_template()
    }
}
