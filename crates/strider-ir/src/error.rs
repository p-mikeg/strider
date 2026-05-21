//! Error type for the `ir` crate.
//!
//! Most fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! The exception is [`crate::validate::ValidationErrors`], which remains a
//! `thiserror`-derived aggregate so callers can `downcast_ref::<ValidationErrors>()`
//! the anyhow-wrapped error and inspect individual validation failures.

pub type Result<T> = anyhow::Result<T>;
