//! Shared helpers for white-box tests inside `opt`.  Mirrors the slice of
//! `tests/common/mod.rs` that the per-pass `mod tests` modules need.

use crate::error::Result;
use ir::{BuiltFunctionGraph, FunctionBuilder, Value};

/// Builds a single-region function whose return value is what `f` produces.
pub(crate) fn make_fn<F>(f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
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
pub(crate) fn make_fn_with_var<F>(
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
