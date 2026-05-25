//! Errors raised by [`crate::run`].
//!
//! All strider errors propagate as [`anyhow::Error`].  Errors carry an
//! informative message and (with `RUST_BACKTRACE=1`) a backtrace.  No
//! typed selectivity — callers treat errors as opaque.
