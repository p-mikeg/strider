use crate::expr::IntoExpr;
use crate::template::Template;

/// Seals [`TemplatePat`].
pub(crate) mod sealed {
    pub trait Sealed {}
}

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
/// Nor is a composite's `.capture()`:
///
/// ```compile_fail,E0277
/// use strider_pattern::{Capture, CaptureExt, TemplatePat, template, var};
/// let c = Capture::new();
/// TemplatePat::into_template(template::int_add(var(c), var(c)).capture(c));
/// ```
pub trait TemplatePat: IntoExpr + Send + sealed::Sealed {
    fn into_template(self) -> Template {
        self.into_expr().into_template()
    }
}
