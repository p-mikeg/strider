use pyo3::PyResult;

use crate::errors::into_strider_err;

/// Map a style name onto a [`dot::DotStyle`].  An unknown name is an error,
/// never a silent default.
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

/// Error if `style` is set on a render path that has no styling.
pub fn reject_style_without_pretty(style: Option<&str>) -> PyResult<()> {
    match style {
        None => Ok(()),
        Some(_) => Err(into_strider_err(anyhow::anyhow!(
            "`style` applies to the pretty render only — pass pretty=True, \
             or drop `style` for the raw as-stored render"
        ))),
    }
}
