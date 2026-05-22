//! Shared mock-IR helpers used by tests across the workspace.
//!
//! Every helper here sets a **sentinel lift address** on the
//! `FunctionBuilder` for the duration of the closure so every node
//! created through the `build_*` API inherits a non-empty
//! asm-fingerprint.  This makes mock-graph tests satisfy the always-on
//! Layer-C asm-fingerprint check without needing to stamp each node by
//! hand.  The sentinel value is the magic constant [`SENTINEL_LIFT_ADDR`]
//! (`0xDEAD_BEEF_0000_0001`) so debugging is unambiguous when a sentinel
//! leaks into production output.
//!
//! This is a dedicated test-utility crate so consumers can dev-depend on
//! it without forcing strider-ir to carry a feature flag.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use strider_ir::{BuiltFunctionGraph, FunctionBuilder, Result, Value};

/// Sentinel asm-fingerprint address used by every helper in this
/// module.  Distinct from any real machine address so debug output
/// (graph dumps, IR snapshots) is obvious when a sentinel-stamped
/// node leaks into a production code path.
pub const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// Fluent builder for the 7-positional-arg `FunctionBuilder::new_raw`
/// signature used by mock-IR tests across the workspace.
///
/// The builder defers to `FunctionBuilder::new_raw` and then stamps
/// [`SENTINEL_LIFT_ADDR`] as the active lift address so every node
/// the test subsequently creates carries a non-empty asm-fingerprint
/// (Layer-C contract).  Test sites no longer repeat the
/// `new_raw + set_lift_addr` dance.
///
/// The constructed `FunctionBuilder` has the sentinel lift_addr set
/// but no region created yet — callers that want a single entry
/// region can use [`RegisterSet::build_fn_single_region`] instead.
#[derive(Default, Clone)]
pub struct RegisterSet {
    tracked: Vec<rsleigh::Vn>,
    arg_passing: Vec<rsleigh::Vn>,
    callee_saved: Vec<rsleigh::Vn>,
    ret_val: Vec<rsleigh::Vn>,
    sp: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
}

impl RegisterSet {
    /// Construct an empty register set.  All vectors start empty and
    /// `sp` / `ret_stack_pop` default to `None` / `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `vn` to the tracked-variables list.  Equivalent to the
    /// first positional argument of `FunctionBuilder::new_raw`.
    #[must_use]
    pub fn tracked(mut self, vn: rsleigh::Vn) -> Self {
        self.tracked.push(vn);
        self
    }

    /// Append `vn` to the arg-passing list (second positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn arg(mut self, vn: rsleigh::Vn) -> Self {
        self.arg_passing.push(vn);
        self
    }

    /// Append `vn` to the callee-saved list (third positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn callee_saved(mut self, vn: rsleigh::Vn) -> Self {
        self.callee_saved.push(vn);
        self
    }

    /// Append `vn` to the ret-val list (fourth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn ret(mut self, vn: rsleigh::Vn) -> Self {
        self.ret_val.push(vn);
        self
    }

    /// Set the stack-pointer varnode (fifth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn sp(mut self, vn: rsleigh::Vn) -> Self {
        self.sp = Some(vn);
        self
    }

    /// Set the `ret_stack_pop` value (sixth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn ret_stack_pop(mut self, n: i64) -> Self {
        self.ret_stack_pop = n;
        self
    }

    /// Construct a `FunctionBuilder` with this register set and stamp
    /// [`SENTINEL_LIFT_ADDR`] as the active lift address.  No region
    /// is created — callers that need multiple regions can drive
    /// `create_region` / `set_entry_region` / `set_region` themselves.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new_raw`.
    pub fn build_fn(self) -> Result<FunctionBuilder> {
        let mut b = FunctionBuilder::new_raw(
            self.tracked,
            &self.arg_passing,
            &self.callee_saved,
            &self.ret_val,
            self.sp,
            self.ret_stack_pop,
        )?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        Ok(b)
    }

    /// Construct a `FunctionBuilder` with this register set and a
    /// single entry region.  Equivalent to `build_fn` followed by
    /// `create_region` + `set_entry_region` + `set_region`.
    /// [`SENTINEL_LIFT_ADDR`] is stamped as the active lift address.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new_raw`,
    /// `create_region`, or `set_entry_region`.
    pub fn build_fn_single_region(self) -> Result<FunctionBuilder> {
        let mut b = FunctionBuilder::new_raw(
            self.tracked,
            &self.arg_passing,
            &self.callee_saved,
            &self.ret_val,
            self.sp,
            self.ret_stack_pop,
        )?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        Ok(b)
    }
}

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
    let mut b = RegisterSet::new()
        .tracked(vn)
        .arg(vn)
        .build_fn_single_region()?;
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
    let mut b = RegisterSet::new()
        .tracked(sp_vn)
        .callee_saved(sp_vn)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp_vn)?;
    f(&mut b, sp_val)?;
    b.set_lift_addr(None);
    b.build()
}
