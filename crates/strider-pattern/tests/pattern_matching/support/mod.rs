//! Shared test helpers: graph construction, pre-built shapes, assertion DSL.
//! Test files go through these rather than reaching into `strider_ir` /
//! `strider_pattern` internals directly.

#![allow(dead_code, unused_imports)]

pub(crate) mod assertions;
pub(crate) mod shapes;

pub(crate) use strider_ir_test_utils::{Tb, reg_vn, stack_vn_x86_64 as stack_vn};
