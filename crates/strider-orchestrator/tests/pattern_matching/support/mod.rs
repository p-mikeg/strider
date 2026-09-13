#![allow(dead_code)] // a shape or assertion serves one of the sibling modules

pub(crate) mod assertions;
pub(crate) mod shapes;

pub(crate) use strider_ir_test_utils::{Tb, reg_vn, stack_vn_x86_64 as stack_vn};
