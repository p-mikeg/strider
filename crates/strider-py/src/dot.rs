//! Dot-rendering helpers shared by `PyCfg` and `PyFunction`.

use pyo3::PyResult;

use crate::errors::into_strider_err;

/// Rejecting an unknown style rather than defaulting is deliberate: a typo'd
/// theme used to fall through to `dark` and render fine, handing the caller a
/// good picture in the wrong theme with no signal the argument was ignored.
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

/// Silently ignoring `style` on a raw render would be the same defect
/// [`dot_style_for`] exists to prevent.
pub fn reject_style_without_pretty(style: Option<&str>) -> PyResult<()> {
    match style {
        None => Ok(()),
        Some(_) => Err(into_strider_err(anyhow::anyhow!(
            "`style` applies to the pretty render only — pass pretty=True, \
             or drop `style` for the raw as-stored render"
        ))),
    }
}
