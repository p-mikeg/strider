import strider


def test_module_loads():
    assert hasattr(strider, "__version__")
