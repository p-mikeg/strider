//! Shared mock-IR helpers used by tests across the workspace.
//!
//! Gated by `feature = "test-utils"` so production code can't reach it.
//! Consumers add `ir = { workspace = true, features = ["test-utils"] }`
//! to their `[dev-dependencies]`.
//!
//! Every helper here sets a **sentinel lift address** on the
//! `FunctionBuilder` for the duration of the closure so every node
//! created through `build_*` API inherits a non-empty asm-fingerprint.
//! This makes mock-graph tests satisfy the always-on Layer-C
//! asm-fingerprint check (Phase 1 Task 1.4b / G3) without needing to
//! stamp each node by hand.  The sentinel value is the magic constant
//! [`SENTINEL_LIFT_ADDR`] (`0xDEAD_BEEF_0000_0001`) so debugging is
//! unambiguous when a sentinel leaks into production output.

use crate::error::Result;
use crate::{BuiltFunctionGraph, FunctionBuilder, Value};

/// Sentinel asm-fingerprint address used by every helper in this
/// module.  Distinct from any real machine address so debug output
/// (graph dumps, IR snapshots) is obvious when a sentinel-stamped
/// node leaks into a production code path.
pub const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// Builds a single-region function whose return value is what `f` produces.
///
/// Sets [`SENTINEL_LIFT_ADDR`] as the active lift address for the
/// duration of `f` and the trailing `build_return` so every emitted
/// node carries a non-empty asm-fingerprint (Layer-C contract).
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
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let val = f(&mut b)?;
    // Re-stamp the sentinel after the closure so that the trailing
    // `build_return` is attributed even if `f` cleared the lift_addr
    // (e.g. asm-fingerprint-propagation tests that set their own
    // per-insn addresses and reset to `None` before returning).
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Builds a single-region function with a tracked variable `vn`.  The closure
/// receives the read-back value (a `VarPhi` over `InitialVar(vn)`) and
/// returns the value to wire into the function's `Return`.  Returns the built
/// graph and the read-back `Value` so the caller can refer to it later.
///
/// Sets [`SENTINEL_LIFT_ADDR`] for the duration of `f` and the trailing
/// `build_return` so every emitted node carries a non-empty
/// asm-fingerprint (Layer-C contract).
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
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    // Re-stamp the sentinel after the closure (see `make_empty_fn`).
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    Ok((b.build()?, x))
}

/// Fabricates a register varnode of the given size at offset `off`.
#[must_use]
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
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

/// Builds a single-region function with `sp_vn` tracked as a stack-pointer
/// variable.  The closure receives the builder and the read-back SP value
/// (`InitialVar(sp_vn)`) and is responsible for emitting the function body
/// — including the `Return`.  This matches `FunctionBuilder::new_raw(vec![sp],
/// &[], &[sp], &[], None, 0)?` + region setup, which appears verbatim in
/// dozens of opt tests.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_sp_fn<F>(sp_vn: rsleigh::Vn, f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<()>,
{
    let mut b = FunctionBuilder::new_raw(vec![sp_vn], &[], &[sp_vn], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_val = b.read_variable(&sp_vn)?;
    f(&mut b, sp_val)?;
    b.set_lift_addr(None);
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeKind;

    #[test]
    fn make_sp_fn_emits_initial_var_for_sp() -> Result<()> {
        let sp = sp_vn_x86_64();
        let fg = make_sp_fn(sp, |b, sp_val| {
            b.build_return(Some(sp_val), &[])?;
            Ok(())
        })?;
        let has_initial_var_sp = fg
            .all_node_ids()
            .any(|n| matches!(fg.graph.node_kind(n), NodeKind::InitialVar(v) if *v == sp));
        assert!(has_initial_var_sp);
        Ok(())
    }
}
