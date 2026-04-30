//! Shared mock-IR helpers used by tests across the workspace.
//!
//! Gated by `feature = "test-utils"` so production code can't reach it.
//! Consumers add `ir = { workspace = true, features = ["test-utils"] }`
//! to their `[dev-dependencies]`.

use crate::error::Result;
use crate::{BuiltFunctionGraph, FunctionBuilder, Value};

/// Builds a single-region function whose return value is what `f` produces.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_empty_fn<F>(f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    let mut b = FunctionBuilder::empty()?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    b.build()
}

/// Builds a single-region function with a tracked variable `vn`.  The closure
/// receives the read-back value (a `ControlPhi` over `InitialVar(vn)`) and
/// returns the value to wire into the function's `Return`.  Returns the built
/// graph and the read-back `Value` so the caller can refer to it later.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_fn_with_var<F>(
    vn: rsleigh::Vn,
    f: F,
) -> Result<(BuiltFunctionGraph, Value)>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build()?, x))
}

/// Fabricates a register varnode of the given size at offset `off`.
#[must_use]
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// Stack-pointer varnode at REGISTER:0x20 with x86 ESP width (4 bytes).
#[must_use]
pub fn sp_vn_x86() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

/// Stack-pointer varnode at REGISTER:0x20 with x86_64 RSP width (8 bytes).
#[must_use]
pub fn sp_vn_x86_64() -> rsleigh::Vn {
    reg_vn(0x20, 8)
}
