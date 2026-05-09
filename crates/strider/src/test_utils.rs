//! Test-only helpers for constructing a [`Strider`] without re-typing
//! the `let arch = …; let regs = arch.probe_regs()…; Strider::new(arch,
//! regs, cc)?` boilerplate at every test site.
//!
//! Exported under `#[cfg(any(feature = "test-utils", test))]` so it's
//! available to:
//! 1. Unit tests inside `crates/strider/src/`.
//! 2. Integration tests under `crates/strider/tests/` (test profile
//!    auto-enables the `cfg(test)` half of the gate).
//! 3. Other crates that opt in via
//!    `strider = { workspace = true, features = ["test-utils"] }` in
//!    `[dev-dependencies]`.
//!
//! Promoted from the per-test-file inline pattern flagged by
//! `reviews/round8-repetition-sweep.md` (#5) — 18 sites duplicated the
//! same 3-line setup.

// Test-fixture helpers: an `.expect()` here is the right failure mode
// (a missing `.sla` file or unresolved CC register name is a setup
// error, not a runtime condition).  The crate-wide clippy lint level
// forbids `expect`, so allow it explicitly for this test-only module.
#![allow(clippy::expect_used, clippy::panic)]

use crate::{CallingConvention, SleighArch, Strider};

/// Construct a `Strider` for the given `arch` + `cc` pair, probing
/// Sleigh against an empty memory reader for the register table.
///
/// # Panics
///
/// Panics if `probe_regs` fails (Sleigh setup failure — almost always a
/// missing `.sla` file at compile time) or if `Strider::new` fails
/// (calling-convention register-name resolution failure).  Both are
/// programming errors at the test-fixture level, so panicking gives
/// the most actionable failure mode.
#[must_use]
pub fn strider_for_arch(arch: SleighArch, cc: CallingConvention) -> Strider {
    let regs = arch.probe_regs().expect("strider test-utils: probe_regs");
    Strider::new(arch, regs, cc).expect("strider test-utils: Strider::new")
}

/// Construct a `Strider` for x86_64 SystemV.  The most common
/// configuration in synthetic-fixture tests.
#[must_use]
pub fn strider_x86_64() -> Strider {
    strider_for_arch(SleighArch::x86_64(), CallingConvention::x86_64_systemv())
}

/// Construct a `Strider` for x86 cdecl.
#[must_use]
pub fn strider_x86() -> Strider {
    strider_for_arch(SleighArch::x86(), CallingConvention::x86_cdecl())
}

/// Construct a `Strider` for AArch64 AAPCS64.
#[must_use]
pub fn strider_aarch64() -> Strider {
    strider_for_arch(SleighArch::aarch64(), CallingConvention::aarch64_aapcs64())
}

/// Construct a `Strider` for ARM AAPCS.
#[must_use]
pub fn strider_arm() -> Strider {
    strider_for_arch(SleighArch::arm(), CallingConvention::arm_aapcs())
}
