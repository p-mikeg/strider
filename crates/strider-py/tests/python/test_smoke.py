import strider


def test_module_loads():
    assert hasattr(strider, "__version__")


def test_error_hierarchy():
    from strider import errors
    assert issubclass(errors.LiftError, errors.StriderError)
    assert issubclass(errors.ReaderError, errors.StriderError)
    assert issubclass(errors.PatternError, errors.StriderError)
    assert issubclass(errors.RewriteError, errors.StriderError)
