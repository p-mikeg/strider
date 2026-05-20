//! Shared dot-rendering helpers used by `PyCfg` and `PyGraph`.
//!
//! Centralizes the style-name → `strider_ir::dot::DotStyle` mapping so the two
//! Python wrappers share one rendering code path.

/// Maps a style name to a `strider_ir::dot::DotStyle`.  Unknown names fall back
/// to `dark`.
pub fn dot_style_for(name: Option<&str>) -> strider_ir::dot::DotStyle {
    match name.unwrap_or("dark") {
        "dark_cfg" => strider_ir::dot::DotStyle::dark_cfg(),
        "empty" => strider_ir::dot::DotStyle::empty(),
        // "dark" and any unknown name fall through to dark.
        _ => strider_ir::dot::DotStyle::dark(),
    }
}
