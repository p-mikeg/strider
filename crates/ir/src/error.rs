//! Error type for the `ir` crate.
//!
//! Most fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! The exception is [`crate::validate::ValidationErrors`], which remains a
//! `thiserror`-derived aggregate so callers can `downcast_ref::<ValidationErrors>()`
//! the anyhow-wrapped error and inspect individual validation failures.

pub type Result<T> = anyhow::Result<T>;

/// Returned by [`crate::FunctionBuilder::build_call_other`] when the
/// supplied user-op `name` has no entry in
/// [`target::user_ops::classify`].  Strict-on-emission policy: any
/// new user-op surfaced by a real lift must be classified before the
/// lift can succeed.
///
/// Recover via `anyhow::Error::downcast_ref::<UnknownUserOpError>()`.
#[derive(Debug, thiserror::Error)]
#[error("unknown CallOther user-op name {name:?}; \
         add an entry to target::user_ops::classify")]
pub struct UnknownUserOpError {
    pub name: String,
}

#[cfg(test)]
mod unknown_user_op_tests {
    use super::*;

    #[test]
    fn display_contains_name() {
        let e = UnknownUserOpError {
            name: "mystery".to_string(),
        };
        let s = format!("{e}");
        assert!(s.contains("mystery"), "got: {s}");
        assert!(s.contains("user-op"), "got: {s}");
    }

    #[test]
    fn anyhow_downcast_recovers_type() {
        let e: anyhow::Error = UnknownUserOpError {
            name: "mystery".to_string(),
        }
        .into();
        assert!(e.downcast_ref::<UnknownUserOpError>().is_some());
    }
}
