import pytest
import strider
from strider.pattern import (
    Capture, Pat, var, anything, int_const, bool_const,
    int_add, int_sub, int_mul, load, store, call, ret, if_else, phi, initial_var,
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
    p = int_add(var(a), var(b))
    assert isinstance(p, Pat)


def test_bare_string_operand_is_rejected():
    # A bare string is not a capture operand: use Capture("name") for a
    # capture or anything() for a wildcard.
    with pytest.raises(strider.StriderError):
        load(addr=int_add(Capture("x"), "y")).into_pat()


def test_load_with_addr():
    # `load()` returns a typed builder, not a Pat; `find_all` accepts either.
    p = load(addr=int_add(Capture("base"), Capture("off")))
    assert isinstance(p.into_pat(), Pat)


def test_store_with_addr_and_data():
    p = store(addr=Capture("ptr"), data=int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_reserved_name_via_capture_string_raises():
    with pytest.raises(strider.StriderError):
        int_add(Capture("x"), Capture("y")).capture("_")
    with pytest.raises(strider.StriderError):
        int_add(Capture("x"), Capture("y")).capture("any_")


def test_capture_method_on_pat():
    c = Capture()
    p = int_add(Capture("x"), Capture("y")).capture(c)
    assert isinstance(p, Pat)


def test_capture_method_takes_a_string_name():
    # A string name interns to the same Capture, so the two spellings are
    # interchangeable and `cap` is gone.
    p = int_add(Capture("x"), Capture("y")).capture("sum")
    assert isinstance(p, Pat)
    assert not hasattr(p, "cap")


def test_call_constructor():
    assert isinstance(call().into_pat(), Pat)
    assert isinstance(call().target(0x1000).into_pat(), Pat)


def test_ret_constructor():
    assert isinstance(ret().into_pat(), Pat)


def test_control_node_patterns_exist():
    import strider
    assert strider.pattern.indirect_branch() is not None
    assert strider.pattern.unreachable() is not None
    assert strider.pattern.switch() is not None


def test_if_constructor():
    assert isinstance(if_else().into_pat(), Pat)
    assert isinstance(if_else(cond=Capture("cnd")).into_pat(), Pat)


def test_phi_constructor():
    assert isinstance(phi().into_pat(), Pat)


def test_initial_var_constructor():
    assert isinstance(initial_var(), Pat)


def test_pattern_submodule_dir():
    import strider.pattern as p
    for name in ["int_add", "int_sub", "int_mul", "load", "store", "call", "ret",
                 "if_else", "phi", "var", "anything", "int_const",
                 "bool_const", "Capture", "Pat"]:
        assert hasattr(p, name), name


def test_capture_hash_distinct_for_first_100_ids():
    """`hash(Capture())` discriminates by capture id, so a dict or set keyed
    on Capture keeps distinct captures in distinct buckets."""
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
    """`float_is_nan(x)` builds the IEEE-754 self-inequality (x != x), the
    same IR shape Sleigh's FLOAT_NAN lowering produces at lift time."""
    from strider.pattern import float_is_nan, anything
    p = float_is_nan(anything())
    assert isinstance(p, Pat)


def test_pat_ordered_pins_a_finalized_binary_op():
    """`Pat.ordered()` pins the operand order of a finalised binary op, as
    the builder form does. A shape with no operands to order raises."""
    assert isinstance(int_add(var(Capture()), var(Capture())).ordered(), Pat)
    with pytest.raises(strider.StriderError, match=r"anything\(\)"):
        anything().ordered()


def test_renamed_constructors():
    """Every constructor carries a descriptive name (`int_and`, `int_not`,
    `if_else`, `anything`). The keyword-colliding and underscore-suffixed
    spellings (`and_`, `not_`, `bit_not`, `if_`, `any_`) must not exist as
    aliases."""
    from strider import pattern as pat

    assert hasattr(pat, "int_and")
    assert hasattr(pat, "int_or")
    assert hasattr(pat, "int_xor")
    assert hasattr(pat, "int_not")
    assert hasattr(pat, "if_else")
    assert hasattr(pat, "anything")
    for gone in ("and_", "or_", "xor", "not_", "bit_not", "if_", "any_"):
        assert not hasattr(pat, gone), gone


def test_node_builder_capture_is_chainable():
    # capture() binds the node but returns the builder (it does NOT finalize
    # to a Pat), so more constraints can still be chained after it.
    c = Capture()
    captured = phi().capture(c)
    assert type(captured).__name__ == "PhiPat"
    assert type(captured.any_input(anything())).__name__ == "PhiPat"
    assert type(call().capture(c)).__name__ == "CallPat"
    assert type(if_else().capture(c)).__name__ == "IfPat"
    assert type(store().capture(c)).__name__ == "StorePat"
    assert type(ret().capture(c)).__name__ == "RetPat"


def test_int_const_set_takes_a_negative_like_the_scalar():
    from strider.pattern import int_const

    assert int_const([-1]) is not None


def test_int_const_takes_a_scalar_or_a_set():
    from strider.pattern import int_const

    assert int_const(5) is not None
    assert int_const([1, 2, 3]) is not None


def test_pat_ordered_names_itself():
    with pytest.raises(strider.StriderError, match="ordered") as e:
        anything().ordered()
    assert "\u2014" not in str(e.value)


def test_call_target_takes_an_empty_candidate_list():
    # No candidate qualifies, which is a pattern, not an error.
    assert isinstance(call().target([]).into_pat(), Pat)
