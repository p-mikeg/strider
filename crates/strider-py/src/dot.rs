//! Shared dot-rendering helpers used by `PyCfg` and `PyFunction`.
//!
//! Centralizes the style-name → `dot::DotStyle` mapping so the two
//! Python wrappers share one rendering code path.

use pyo3::PyResult;

use crate::errors::into_strider_err;

/// Maps a style name to a `dot::DotStyle`, rejecting an unrecognised one.
///
/// Rejecting rather than defaulting is deliberate: a typo'd theme used to
/// fall through to `dark` and render successfully, so the caller got a
/// perfectly good picture in the wrong theme and no signal that the
/// argument had been ignored.
pub fn dot_style_for(name: Option<&str>) -> PyResult<dot::DotStyle> {
    let style = match name.unwrap_or("dark") {
        "dark" => dot::DotStyle::dark(),
        "dark_cfg" => dot::DotStyle::dark_cfg(),
        "empty" => dot::DotStyle::empty(),
        other => {
            return Err(into_strider_err(anyhow::anyhow!(
                "unknown dot style {other:?} — expected \"dark\", \
                 \"dark_cfg\" or \"empty\""
            )));
        }
    };
    Ok(style)
}

/// Guard for the render entry points that accept `style` only alongside
/// `pretty=True`: silently ignoring a style on the raw render would be the
/// same defect [`dot_style_for`] exists to prevent.
pub fn reject_style_without_pretty(style: Option<&str>) -> PyResult<()> {
    match style {
        None => Ok(()),
        Some(_) => Err(into_strider_err(anyhow::anyhow!(
            "`style` applies to the pretty render only — pass pretty=True, \
             or drop `style` for the raw as-stored render"
        ))),
    }
}
