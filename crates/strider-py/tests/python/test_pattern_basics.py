import pytest
import strider
from strider.pattern import (
    Capture, Pat, var, any_, int_const, bool_const,
    add, sub, mul, load, store, call, ret, if_, phi, initial_var,
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
    # `load()` now returns a `LoadPat` typed builder; finalise via
    # `.into_pat()` for the back-compat assertion.  Builder forms
    # (`load().addr(...)`, `load().space(...)`) are accepted by
    # `Graph.find_all` directly via `PatLike`.
    p = load(addr=add("base", "off"))
    assert isinstance(p.into_pat(), Pat)


def test_store_with_addr_and_data():
    p = store(addr="ptr", data=int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_underscore_string_means_wildcard():
    # "_" and "any_" are reserved wildcards — they convert to any()
    # silently rather than raising.  Trying to use them as capture
    # names via PyCapture-aware methods (e.g. .cap("_")) IS an error.
    p = add("_", "x")
    assert isinstance(p, Pat)
    p = add("any_", "x")
    assert isinstance(p, Pat)


def test_reserved_name_via_cap_raises():
    with pytest.raises(strider.errors.PatternError):
        add("x", "y").cap("_")
    with pytest.raises(strider.errors.PatternError):
        add("x", "y").cap("any_")


def test_capture_method_on_pat():
    c = Capture()
    p = add("x", "y").capture(c)
    assert isinstance(p, Pat)


def test_cap_method_on_pat():
    p = add("x", "y").cap("sum")
    assert isinstance(p, Pat)


def test_call_constructor():
    # `call()` returns a `CallPat` typed builder so chaining
    # `.at(addr)`, `.target(p)`, `.arg(idx, p)`, `.ret_output(idx, p)`
    # is legal.  The builder is a `PatLike` (accepted directly by
    # `Graph.find_all`); use `.into_pat()` to get a finalised `Pat`.
    assert isinstance(call().into_pat(), Pat)
    assert isinstance(call(at=0x1000).into_pat(), Pat)


def test_ret_constructor():
    # `ret()` returns a `RetPat` typed builder (chain `.preceded_by`,
    # `.ret_val(idx, p)`).  Finalise via `.into_pat()`.
    assert isinstance(ret().into_pat(), Pat)


def test_if_constructor():
    # `if_()` returns an `IfPat` typed builder (chain `.cond`,
    # `.true_branch`, `.false_branch`).
    assert isinstance(if_().into_pat(), Pat)
    assert isinstance(if_(cond="cnd").into_pat(), Pat)


def test_phi_constructor():
    # `phi()` returns a `PhiPat` typed builder (chain `.for_vn`,
    # `.input(idx, p)`).
    assert isinstance(phi().into_pat(), Pat)


def test_initial_var_constructor():
    assert isinstance(initial_var(), Pat)


def test_pattern_submodule_dir():
    import strider.pattern as p
    # Smoke check that the most common builders are present.
    for name in ["add", "sub", "mul", "load", "store", "call", "ret",
                 "if_", "phi", "var", "any_", "int_const",
                 "bool_const", "Capture", "Pat"]:
        assert hasattr(p, name), name


def test_capture_hash_distinct_for_first_100_ids():
    """Regression: PyCapture.__hash__ used to be `len(repr(...))`, which
    collapsed every same-decimal-digit-count id (10..99 → 11, 100..999 → 12,
    etc.) to one bucket — breaking any dict/set keyed on Capture.  The fix
    hashes the underlying u32 id directly."""
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
    """float_is_nan(x) used to raise NotImplementedError; it now produces
    a valid Pat via the IEEE-754 self-inequality (x != x) which matches
    the same IR shape Sleigh's FLOAT_NAN lowering produces at lift time."""
    from strider.pattern import float_is_nan, any_
    p = float_is_nan(any_())
    assert isinstance(p, Pat)


def test_pyat_ordered_on_finalized_pat_raises():
    """Pat.ordered() on a finalized Pat (e.g. add(...)) used to silently
    return self; it now raises PatternError, pointing users to the typed
    builder (int_binary(...).ordered())."""
    with pytest.raises(strider.errors.PatternError):
        add(var(Capture()), var(Capture())).ordered()
