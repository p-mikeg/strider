strider_error::define_error! {
    pub struct Error wraps ErrorKind;

    /// Errors that can be produced by the pattern crate.
    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        /// A test assertion failed. Exists so tests can return `Result<(), Error>`
        /// instead of using `panic!`.
        #[error("assertion failed: {0}")]
        AssertionFailed(String),

        /// Propagated from the underlying IR layer — raised by `make_value_node`,
        /// `replace_all_uses`, and similar graph-mutation helpers used by the
        /// rewrite engine.
        #[error(transparent)]
        IrError(ir::ErrorKind),

        /// A user-supplied closure inside a [`crate::build::Build`] tree (e.g.
        /// the body passed to `int_const_fn`, `bool_const_fn`, or
        /// `float_const_fn`) returned an error.  Carries the original error as
        /// a boxed trait object so rule authors can surface arbitrary error
        /// types without having to shoehorn them into a dedicated variant.
        #[error("rewrite-rule closure failed: {0}")]
        RewriteClosure(Box<dyn std::error::Error + Send + Sync>),

        /// A capture variable referenced by a [`crate::build::FromCtx`] impl
        /// was not bound during the LHS match.  Indicates a pattern-authoring
        /// bug — every capture variable used in the RHS macro must appear in
        /// the LHS pattern and have a corresponding binding emitted by the
        /// matcher.  The payload names the capture **kind** (e.g. `"IntVar"`,
        /// `"IntBinaryOpVar"`) so the site of the bug is obvious from the
        /// error message.
        #[error("missing binding for capture of kind {0}")]
        MissingBinding(&'static str),

        /// Signal from a rewrite-rule RHS closure that the rule doesn't apply
        /// after all.  Used by partial-oracle helpers (e.g. `eval_int_binary`
        /// on divide-by-zero) that need to opt out without surfacing a hard
        /// error.  The [`crate::rewrite_rule`] interpreter converts this
        /// error back to "no change"; every other error variant propagates
        /// as a real failure.
        #[error("rewrite rule opted to skip")]
        RewriteSkip,

        /// A pattern was used on the RHS of a `rewrite_rule` but does not
        /// support construction (wildcards, guards, and control patterns
        /// like `call` / `ret` / `if_node` have no build semantics today).
        #[error("pattern {0} is not buildable (match-only)")]
        NotBuildable(&'static str),
    }
}

impl Error {
    /// Wraps an arbitrary closure error into a [`Error`] via
    /// [`ErrorKind::RewriteClosure`].  Use inside rewrite-rule RHS closures to
    /// forward custom error types (e.g. per-crate error enums) through the
    /// rewrite engine.
    #[track_caller]
    pub fn rewrite_closure<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        ErrorKind::RewriteClosure(Box::new(e)).into()
    }

    /// Returns an [`Error`] carrying [`ErrorKind::RewriteSkip`], the sentinel
    /// the `rewrite_rule` interpreter converts back to "no change" rather than
    /// treating as a hard failure.
    ///
    /// Typical use inside a `*_const_with!` closure body is to bail out when
    /// the operation has no meaningful constant value:
    ///
    /// ```rust
    /// use pattern::{IntVar, any_int_const, div, int_const_with, rewrite_rule};
    ///
    /// let a = IntVar::new();
    /// let b = IntVar::new();
    /// let _rule = rewrite_rule(
    ///     div(any_int_const(a), any_int_const(b)),
    ///     int_const_with!([a, b] => {
    ///         if b == 0 {
    ///             return Err(pattern::Error::skip());
    ///         }
    ///         a.wrapping_div(b)
    ///     }),
    /// );
    /// ```
    ///
    /// The sentinel round-trips via [`Error::is_skip`]:
    ///
    /// ```rust
    /// let err = pattern::Error::skip();
    /// assert!(err.is_skip());
    /// ```
    #[track_caller]
    #[must_use]
    pub fn skip() -> Self {
        ErrorKind::RewriteSkip.into()
    }

    /// Returns `true` if this error is the `RewriteSkip` sentinel used to
    /// opt out of a rewrite rule without surfacing a hard error.
    #[must_use]
    pub fn is_skip(&self) -> bool {
        matches!(self.kind(), ErrorKind::RewriteSkip)
    }

    /// Returns an [`Error`] carrying [`ErrorKind::NotBuildable`] for the
    /// pattern type named `pat_name` (typically `std::any::type_name::<Self>()`
    /// in a `Pattern::try_build` default impl).
    #[track_caller]
    #[must_use]
    pub fn not_buildable(pat_name: &'static str) -> Self {
        ErrorKind::NotBuildable(pat_name).into()
    }
}

/// Hand-rolled bridge so `?` across the `ir` → `pattern` boundary preserves the
/// origin backtrace + location chain captured by `ir`.  Decomposes the inner
/// wrapper, moves its `ErrorFields`, and appends the outer caller's site.
impl From<ir::Error> for Error {
    #[track_caller]
    fn from(e: ir::Error) -> Self {
        let (kind, fields) = e.decompose();
        Error {
            kind: Box::new(ErrorKind::IrError(*kind)),
            fields: fields.push_caller(),
        }
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
