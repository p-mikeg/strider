use pyo3::prelude::*;

use crate::errors::into_strider_err;

const DEFAULT_STYLE: &str = "dark";

/// `dark` with Courier metrics.
pub const DEFAULT_CFG_STYLE: &str = "dark_cfg";

type StyleCtor = fn() -> dot::DotStyle;

/// `strider.cfg.DOT_STYLES`.
pub(crate) const DOT_STYLES: [(&str, StyleCtor); 3] = [
    ("dark", dot::DotStyle::dark),
    ("dark_cfg", dot::DotStyle::dark_cfg),
    ("empty", dot::DotStyle::default),
];

/// The [`dot::DotStyle`] named `name`.
pub fn dot_style_for(name: &str) -> PyResult<dot::DotStyle> {
    if let Some((_, style)) = DOT_STYLES.iter().find(|(n, _)| *n == name) {
        return Ok(style());
    }
    let quoted: Vec<String> = DOT_STYLES.iter().map(|(n, _)| format!("{n:?}")).collect();
    let (last, rest) = quoted.split_last().expect("a non-empty roster");
    Err(into_strider_err(anyhow::anyhow!(
        "unknown dot style {name:?}; expected {} or {last}",
        rest.join(", ")
    )))
}

/// Writes `d` (HTML when `html`, else DOT) to `path`, or returns it without
/// one. A failed write raises `OSError` naming `path`.
pub fn render<G: dot::GraphDotDumper>(
    d: &dot::GraphDot<G>,
    html: bool,
    path: Option<std::path::PathBuf>,
) -> PyResult<Option<String>> {
    let text = if html {
        d.as_html_from_dot()
    } else {
        d.as_dot()
    }
    .map_err(into_strider_err)?;
    let Some(p) = path else { return Ok(Some(text)) };
    // `(errno, strerror, filename)` lets CPython pick the `OSError` subclass.
    std::fs::write(&p, text).map_err(|e| {
        let msg = e.to_string();
        let strerror = msg.split(" (os error").next().unwrap_or(&msg).to_owned();
        pyo3::exceptions::PyOSError::new_err((e.raw_os_error().unwrap_or(0), strerror, p))
    })?;
    Ok(None)
}

/// `pretty=`: `False` is the raw render, `True` the pretty one in the default
/// theme, a style name the pretty one in that theme.
#[derive(FromPyObject)]
pub enum Pretty {
    Flag(bool),
    Style(String),
}

impl Pretty {
    /// The pretty render's theme; `None` for the raw render.
    pub fn theme(&self) -> Option<&str> {
        match self {
            Pretty::Flag(false) => None,
            Pretty::Flag(true) => Some(DEFAULT_STYLE),
            Pretty::Style(s) => Some(s),
        }
    }
}
