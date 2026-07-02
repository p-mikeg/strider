//! Shared test helpers: graph construction, pre-built shapes, assertion DSL.
//!
//! Every test file should `use super::support::{graph, shapes, assertions};`
//! (or bring specific names into scope) rather than reaching into `strider_ir`
//! or `strider_pattern` internals directly.

#![allow(dead_code, unused_imports)]

pub(crate) mod assertions;
pub(crate) mod shapes;

pub(crate) use strider_ir_test_utils::{Tb, reg_vn, stack_vn_x86_64 as stack_vn};
