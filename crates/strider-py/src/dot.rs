//! Shared dot-rendering helpers used by `PyCfg` and `PyGraph`.
//!
//! Centralizes the style-name → `dot::DotStyle` mapping so the two
//! Python wrappers share one rendering code path.

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
