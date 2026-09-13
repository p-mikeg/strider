use pyo3::prelude::*;

use crate::errors::into_strider_err;

/// The theme a render falls back to when the caller names none.
const DEFAULT_STYLE: &str = "dark";

/// The CFG renderers' fallback theme: `dark` with Courier metrics.
pub const DEFAULT_CFG_STYLE: &str = "dark_cfg";

/// Map a style name onto a [`dot::DotStyle`].  An unknown name is an error,
/// never a silent default.
pub fn dot_style_for(name: Option<&str>) -> PyResult<dot::DotStyle> {
    let style = match name.unwrap_or(DEFAULT_STYLE) {
        "dark" => dot::DotStyle::dark(),
        "dark_cfg" => dot::DotStyle::dark_cfg(),
        "empty" => dot::DotStyle::empty(),
        other => {
            return Err(into_strider_err(anyhow::anyhow!(
                "unknown dot style {other:?}; expected \"dark\", \
                 \"dark_cfg\" or \"empty\""
            )));
        }
    };
    Ok(style)
}

/// Writes `d` to `path` and returns `None`, or returns the text when there is
/// no `path`; HTML when `html`, else DOT.
pub fn render<G: dot::GraphDotDumper>(
    d: &dot::GraphDot<G>,
    html: bool,
    path: Option<&str>,
) -> PyResult<Option<String>> {
    match (html, path) {
        (true, Some(p)) => d.dump_as_html(p).map(|()| None),
        (false, Some(p)) => d.dump_as_dot(p).map(|()| None),
        (true, None) => d.as_html_from_dot().map(Some),
        (false, None) => d.as_dot().map(Some),
    }
    .map_err(into_strider_err)
}

/// The `pretty=` render selector: `False` renders the graph as stored, `True`
/// renders it prettily in the default theme, and a style name renders it
/// prettily in that theme.
#[derive(FromPyObject)]
pub enum Pretty {
    Flag(bool),
    Style(String),
}

impl Pretty {
    /// `None` for the raw render, else the pretty render's theme.
    pub fn theme(&self) -> Option<&str> {
        match self {
            Pretty::Flag(false) => None,
            Pretty::Flag(true) => Some(DEFAULT_STYLE),
            Pretty::Style(s) => Some(s),
        }
    }
}
