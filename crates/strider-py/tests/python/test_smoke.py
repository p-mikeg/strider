import strider


def test_module_loads():
    assert hasattr(strider, "__version__")


def test_error_surface_is_single_class():
    """The Python error surface is a single `StriderError` class — all
    Rust-side `anyhow` chains land here, formatted with the caused-by
    chain in the message (and the Rust backtrace under
    `RUST_BACKTRACE=1`).  No subclass hierarchy; assertions on specific
    failure kinds should match on the message text (use
    `pytest.raises(StriderError, match="...")`)."""
    from strider import errors
    assert hasattr(errors, "StriderError")
    # The subclasses that earlier drafts exposed (LiftError, ReaderError,
    # PatternError, RewriteError, UnresolvedIndirectBranchError,
    # UnknownCallOtherError) have been collapsed.  Confirm they're gone
    # so future drift back to a typed hierarchy is caught here.
    for collapsed in (
        "LiftError",
        "ReaderError",
        "PatternError",
        "RewriteError",
        "UnresolvedIndirectBranchError",
        "UnknownCallOtherError",
    ):
        assert not hasattr(errors, collapsed), (
            f"errors.{collapsed} re-appeared — the API is meant to be a "
            "single `StriderError` class.  If a typed hierarchy is being "
            "reintroduced, update this test AND every test that uses "
            "`pytest.raises(errors.StriderError, match=...)` to use the new "
            "typed class."
        )
