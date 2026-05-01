//! Shared dot-rendering adapters used by `PyCfg` and `PyGraph`.
//!
//! Centralizes style-name → `dot::DotStyle` mapping and the
//! `dump_html` / `dump_dot` / `html_str` thin wrappers so the two
//! Python wrappers share one rendering code path.

use std::path::Path;

use pyo3::prelude::*;

use crate::errors::into_strider_err;

/// Maps a style name to a `dot::DotStyle`.  Unknown names fall back
/// to `dark`.
pub fn dot_style_for(name: Option<&str>) -> dot::DotStyle {
    match name.unwrap_or("dark") {
        "dark_cfg" => dot::DotStyle::dark_cfg(),
        "empty" => dot::DotStyle::empty(),
        // "dark" and any unknown name fall through to dark.
        _ => dot::DotStyle::dark(),
    }
}

pub fn dump_html<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>, path: &str) -> PyResult<()> {
    d.dump_as_html(Path::new(path)).map_err(into_strider_err)
}

pub fn dump_dot<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>, path: &str) -> PyResult<()> {
    d.dump_as_dot(Path::new(path)).map_err(into_strider_err)
}

pub fn html_str<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>) -> PyResult<String> {
    d.as_html_from_dot().map_err(into_strider_err)
}
