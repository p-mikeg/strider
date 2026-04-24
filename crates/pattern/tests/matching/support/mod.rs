//! Shared test helpers: graph construction, pre-built shapes, assertion DSL.
//!
//! Every test file should `use super::support::{graph, shapes, assertions};`
//! (or bring specific names into scope) rather than reaching into `ir` or
//! `pattern` internals directly.  Keeping the helper surface narrow is what
//! makes tests read as data rather than plumbing.

// These modules offer a menu of helpers; a given test file uses only part of
// it, so unused-dead-code warnings are expected across the suite as a whole.
#![allow(dead_code, unused_imports)]

pub mod assertions;
pub mod graph;
pub mod shapes;

pub use graph::{Tb, reg_vn, sp_vn};
