//! Fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! [`crate::validate::ValidationErrors`] stays a `thiserror` aggregate so
//! callers can `downcast_ref` it out of the anyhow wrapper and inspect the
//! individual failures.

pub type Result<T> = anyhow::Result<T>;
