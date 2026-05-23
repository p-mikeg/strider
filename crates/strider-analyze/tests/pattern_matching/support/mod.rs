//! Shared test helpers: graph construction, pre-built shapes, assertion DSL.
//!
//! Every test file should `use super::support::{graph, shapes, assertions};`
//! (or bring specific names into scope) rather than reaching into `strider_ir`
//! or `strider_analyze::pattern` internals directly.

#![allow(dead_code, unused_imports)]

pub mod assertions;
pub mod graph;
pub mod shapes;

pub use graph::{Tb, reg_vn, sp_vn};
