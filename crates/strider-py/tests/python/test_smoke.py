import strider


def test_module_loads():
    assert hasattr(strider, "__version__")


def test_error_surface_is_single_class():
    """One `StriderError` class carries every failure, with the caused-by
    chain in the message, so assertions on a specific failure kind match on
    message text."""
    assert hasattr(strider, "StriderError")
    # Catches drift toward a typed hierarchy.
    for collapsed in (
        "LiftError",
        "ReaderError",
        "PatternError",
        "RewriteError",
        "UnresolvedIndirectBranchError",
        "UnknownCallOtherError",
    ):
        assert not hasattr(strider, collapsed), (
            f"strider.{collapsed} exists; the API is meant to be a "
            "single `StriderError` class.  If a typed hierarchy is being "
            "reintroduced, update this test AND every test that uses "
            "`pytest.raises(strider.StriderError, match=...)` to use the new "
            "typed class."
        )


def test_strider_error_single_home():
    """`StriderError` has exactly one home; the old `strider.errors`
    submodule path was removed."""
    assert hasattr(strider, "StriderError")
    import importlib
    try:
        importlib.import_module("strider.errors")
        assert False, "strider.errors should not exist"
    except ModuleNotFoundError:
        pass


def test_cdylib_is_private():
    """The native extension loads as `strider._strider`.  The old
    `strider.strider` submodule shadowed its own package name."""
    import importlib

    importlib.import_module("strider._strider")
    assert not hasattr(strider, "strider")
