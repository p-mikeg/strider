//! Error type for the `ir` crate.
//!
//! Most fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! The exception is [`crate::validate::ValidationErrors`], which remains a
//! `thiserror`-derived aggregate so callers can `downcast_ref::<ValidationErrors>()`
//! the anyhow-wrapped error and inspect individual validation failures.

pub type Result<T> = anyhow::Result<T>;

/// Constructed by the strider lifter (in `crates/strider/src/strider/insn/`)
/// when a `pcode::CallOther` opcode's user-op `name` has no entry in
/// [`target::call_other_abi::classify`].  Strict-on-emission policy: any
/// new user-op surfaced by a real lift must be classified before the
/// lift can succeed.  Builders [`crate::FunctionBuilder::build_call_other_modeled`]
/// and [`crate::FunctionBuilder::build_call_other_terminal`] consume an
/// already-classified `CallOtherClass` and never raise this error.
///
/// Recover via `anyhow::Error::downcast_ref::<UnknownCallOtherError>()`.
#[derive(Debug, thiserror::Error)]
#[error("unknown CallOther user-op name {name:?}; \
         add an entry to target::call_other_abi::classify")]
pub struct UnknownCallOtherError {
    pub name: String,
}

#[cfg(test)]
mod unknown_user_op_tests {
    use super::*;

    #[test]
    fn display_contains_name() {
        let e = UnknownCallOtherError {
            name: "mystery".to_string(),
        };
        let s = format!("{e}");
        assert!(s.contains("mystery"), "got: {s}");
        assert!(s.contains("user-op"), "got: {s}");
    }

    #[test]
    fn anyhow_downcast_recovers_type() {
        let e: anyhow::Error = UnknownCallOtherError {
            name: "mystery".to_string(),
        }
        .into();
        assert!(e.downcast_ref::<UnknownCallOtherError>().is_some());
    }
}
