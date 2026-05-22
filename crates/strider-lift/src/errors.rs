//! Typed error markers for the lift pipeline.
//!
//! Lift-stage failures (sleigh decode failure, pcode-lift failure, cfg
//! builder failure) all bubble through the orchestrator as
//! [`anyhow::Error`].  The Python boundary in `strider-py` needs to
//! decide whether to surface such a failure as `LiftError` (a typed
//! Python subclass of `StriderError`) or as the generic catch-all.
//!
//! Before this module existed, that decision was made by scanning the
//! formatted anyhow chain for substrings like `"lift"`, `"sleigh"`,
//! `"decode"`, or `"pcode"`.  The heuristic misclassified unrelated
//! errors whose message happened to contain one of those tokens (e.g.
//! a `ReaderError` reporting `"failed to decode section .got.plt"`).
//!
//! [`LiftError`] is a thin marker that wraps an existing
//! [`anyhow::Error`].  Lift-stage boundaries wrap their result in this
//! type before returning; the Python boundary uses
//! [`anyhow::Error::downcast_ref`] to recognise the marker.  No
//! substring scan, no misclassification.
//!
//! The wrapped error keeps its full anyhow context chain — the
//! [`std::fmt::Display`] impl forwards to the inner error and
//! [`std::error::Error::source`] exposes it for downstream chain
//! inspection.

/// Marker wrapping an [`anyhow::Error`] that originated from the
/// lift pipeline (CFG construction, pcode lifting, sleigh decode).
///
/// Construct via [`LiftError::wrap`] at lift-boundary call sites:
///
/// ```ignore
/// let cfg = Builder::for_arch(arch, sleigh, addr, opts)
///     .build()
///     .map_err(strider_lift::LiftError::wrap)?;
/// ```
///
/// Recover at the Python boundary via `anyhow::Error::downcast_ref`:
///
/// ```ignore
/// if e.downcast_ref::<strider_lift::LiftError>().is_some() {
///     return LiftError::new_err(format!("{e:?}"));
/// }
/// ```
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LiftError(#[source] pub anyhow::Error);

impl LiftError {
    /// Wrap an existing `anyhow::Error` as a lift-stage failure.  The
    /// wrapped error keeps its full context chain — the
    /// [`std::fmt::Display`] impl forwards to the inner error.
    ///
    /// Use at the lift-pipeline boundary inside the orchestrator
    /// (cfg `Builder::build`, `Strider::analyze_cfg_with`) so the
    /// Python converter can route to `LiftError` via a typed downcast
    /// instead of a substring scan over the formatted chain.
    #[must_use]
    pub fn wrap(e: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(LiftError(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_is_downcastable() {
        let inner = anyhow::anyhow!("sleigh decode failed at 0x1000");
        let wrapped = LiftError::wrap(inner);
        assert!(
            wrapped.downcast_ref::<LiftError>().is_some(),
            "wrapped lift error must be recoverable via downcast"
        );
    }

    #[test]
    fn plain_anyhow_with_decode_substring_is_not_lift_error() {
        // Regression guard: a plain anyhow error whose message
        // happens to contain "decode" (e.g. a reader error
        // reporting `"failed to decode section .got.plt"`) must
        // NOT downcast to `LiftError`.  Pre-fix, the substring
        // heuristic in strider-py would have misclassified this.
        let err: anyhow::Error = anyhow::anyhow!("failed to decode section .got.plt");
        assert!(
            err.downcast_ref::<LiftError>().is_none(),
            "an arbitrary anyhow error with 'decode' in its message \
             must not be classified as LiftError just because of the substring"
        );
    }

    #[test]
    fn display_forwards_to_inner_message() {
        let wrapped = LiftError::wrap(anyhow::anyhow!("specific lift detail"));
        // The Display impl of LiftError forwards to the wrapped
        // anyhow Error's Display (which formats the head message),
        // so users see the original message intact at the Python
        // boundary when we format `{e}`.
        let s = format!("{wrapped}");
        assert!(s.contains("specific lift detail"), "got: {s}");
    }

    #[test]
    fn anyhow_context_chain_survives_wrap() {
        // anyhow chain through a wrapped LiftError must still
        // surface in the formatted Debug rendering — the Python
        // converter formats with `{e:?}` so users get the full
        // Caused-by chain.
        let inner = anyhow::anyhow!("root cause").context("intermediate context");
        let wrapped = LiftError::wrap(inner);
        let s = format!("{wrapped:?}");
        assert!(s.contains("intermediate context"), "got: {s}");
        assert!(s.contains("root cause"), "got: {s}");
    }
}
