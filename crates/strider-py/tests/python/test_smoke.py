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
    assert hasattr(strider, "StriderError")
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
        assert not hasattr(strider, collapsed), (
            f"strider.{collapsed} re-appeared — the API is meant to be a "
            "single `StriderError` class.  If a typed hierarchy is being "
            "reintroduced, update this test AND every test that uses "
            "`pytest.raises(strider.StriderError, match=...)` to use the new "
            "typed class."
        )


def test_strider_error_single_home():
    """`StriderError` has exactly one home: `strider.StriderError`.  The
    old `strider.errors` submodule path has been removed."""
    assert hasattr(strider, "StriderError")
    import importlib
    try:
        importlib.import_module("strider.errors")
        assert False, "strider.errors should not exist"
    except ModuleNotFoundError:
        pass
