import pytest
import strider
from strider.pattern import (
    Capture, Pat, var, anything, int_const, bool_const,
    add, sub, mul, load, store, call, ret, if_else, phi, initial_var,
)


def test_capture_creates():
    c = Capture()
    assert "Capture" in repr(c)


def test_capture_distinct():
    c1 = Capture()
    c2 = Capture()
    assert repr(c1) != repr(c2) or c1 is not c2  # at minimum: not aliased


def test_int_const_returns_pat():
    p = int_const(42)
    assert isinstance(p, Pat)


def test_add_with_capture_objects():
    a, b = Capture(), Capture()
    p = add(var(a), var(b))
    assert isinstance(p, Pat)


def test_add_with_strings():
    p = add("x", "y")
    assert isinstance(p, Pat)


def test_load_with_addr():
    # `load()` returns a typed builder, not a Pat; `find_all` accepts either.
    p = load(addr=add("base", "off"))
    assert isinstance(p.into_pat(), Pat)


def test_store_with_addr_and_data():
    p = store(addr="ptr", data=int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_underscore_string_means_wildcard():
    # "_" and "any_" are reserved wildcards: they convert to a wildcard
    # silently.  Using them as capture NAMES (`.cap("_")`) is an error.
    p = add("_", "x")
    assert isinstance(p, Pat)
    p = add("any_", "x")
    assert isinstance(p, Pat)


def test_reserved_name_via_cap_raises():
    with pytest.raises(strider.StriderError):
        add("x", "y").cap("_")
    with pytest.raises(strider.StriderError):
        add("x", "y").cap("any_")


def test_capture_method_on_pat():
    c = Capture()
    p = add("x", "y").capture(c)
    assert isinstance(p, Pat)


def test_cap_method_on_pat():
    p = add("x", "y").cap("sum")
    assert isinstance(p, Pat)


def test_call_constructor():
    assert isinstance(call().into_pat(), Pat)
    assert isinstance(call(at=0x1000).into_pat(), Pat)


def test_ret_constructor():
    assert isinstance(ret().into_pat(), Pat)


def test_control_node_patterns_exist():
    import strider
    assert strider.pattern.indirect_branch() is not None
    assert strider.pattern.unreachable() is not None
    assert strider.pattern.switch() is not None


def test_if_constructor():
    assert isinstance(if_else().into_pat(), Pat)
    assert isinstance(if_else(cond="cnd").into_pat(), Pat)


def test_phi_constructor():
    assert isinstance(phi().into_pat(), Pat)


def test_initial_var_constructor():
    assert isinstance(initial_var(), Pat)


def test_pattern_submodule_dir():
    import strider.pattern as p
    for name in ["add", "sub", "mul", "load", "store", "call", "ret",
                 "if_else", "phi", "var", "anything", "int_const",
                 "bool_const", "Capture", "Pat"]:
        assert hasattr(p, name), name


def test_capture_hash_distinct_for_first_100_ids():
    """Regression: `hash(Capture())` used to be the repr's length, collapsing
    every id with the same digit count into one bucket and breaking any
    dict/set keyed on Capture."""
    captures = [Capture() for _ in range(100)]
    hashes = {hash(c) for c in captures}
    assert len(hashes) == 100, (
        f"hash collision: only {len(hashes)} distinct hashes for 100 captures"
    )


def test_capture_usable_as_dict_key():
    captures = [Capture() for _ in range(50)]
    d = {c: i for i, c in enumerate(captures)}
    assert len(d) == 50, f"dict key collision: only {len(d)} entries for 50 captures"


def test_float_is_nan_constructs_pattern():
    """Regression: `float_is_nan(x)` used to raise NotImplementedError. It
    now builds the IEEE-754 self-inequality (x != x), the same IR shape
    Sleigh's FLOAT_NAN lowering produces at lift time."""
    from strider.pattern import float_is_nan, anything
    p = float_is_nan(anything())
    assert isinstance(p, Pat)


def test_pyat_ordered_on_finalized_pat_raises():
    """Regression: `Pat.ordered()` on a finalized Pat used to silently return
    self.  It now raises, pointing at `int_binary(...).ordered()`."""
    with pytest.raises(strider.StriderError):
        add(var(Capture()), var(Capture())).ordered()


def test_renamed_constructors():
    """Keyword-colliding and underscore-suffixed constructors were renamed to
    descriptive ones (`and_` to `int_and`, `not_`/`bit_not` to `int_not`,
    `if_` to `if_else`, `any_` to `anything`, ...). The old names must not
    come back as aliases."""
    from strider import pattern as pat

    assert hasattr(pat, "int_and")
    assert hasattr(pat, "int_or")
    assert hasattr(pat, "int_xor")
    assert hasattr(pat, "int_not")
    assert hasattr(pat, "if_else")
    assert hasattr(pat, "anything")
    for gone in ("and_", "or_", "xor", "not_", "bit_not", "if_", "any_"):
        assert not hasattr(pat, gone), gone
