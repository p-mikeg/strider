"""`array_sum` from x86/memory.elf is the default graph: it has a clean
`Load(addr = base + offset)` that any working matcher finds at least once.
"""

import pytest

import strider
from strider.pattern import Capture, var, int_add, load, int_const

from .conftest import built_function, built_lifter_and_function, fixture_path


def _build_graph(case="memory", symbol="array_sum"):
    return built_function("x86", case, symbol, optimize=False)


def test_find_all_load_in_array_sum():
    g = _build_graph()
    pat = load()
    hits = g.find_all(pat)
    # array_sum has at least one load (the array element fetch).
    assert len(hits) >= 1


def test_find_all_load_with_addr_pattern():
    g = _build_graph()
    base, off = Capture(), Capture()
    pat = load(addr=int_add(var(base), var(off)))
    hits = g.find_all(pat, ignore_casts=True)
    # Count depends on the optimization shape, so only pin "must not raise".
    assert isinstance(hits, list)


def test_bool_binary_preserves_i1_guard_against_wide_and():
    """`bool_binary(...)` keeps its `I1`-output guard: neither it nor its
    `.ordered()` terminal may match a wide integer `And`. The `bit_and`
    fixture lifts to a 32-bit `And` that `int_and` does match.
    """
    from strider.pattern import anything, bool_binary, int_and

    g = _build_graph("arithmetic", symbol="bit_and")

    wide_hits = g.find_all(int_and(anything(), anything()))
    assert len(wide_hits) >= 1, "expected the 32-bit And in bit_and()"

    assert g.find_all(bool_binary("And", anything(), anything())) == []
    assert g.find_all(bool_binary("And", anything(), anything()).ordered()) == []


def test_add_commutes_and_ordered_pins_operands():
    """`int_add(...)` matches commutatively, so its hit set is a superset of the
    `int_binary(...).ordered()` form (which must still find the canonical
    site rather than breaking the walk).
    """
    from strider.pattern import anything, int_binary

    g = _build_graph()
    commutative = g.find_all(int_add(anything(), anything()))
    ordered = g.find_all(int_binary("Add", anything(), anything()).ordered())
    assert isinstance(commutative, list)
    assert isinstance(ordered, list)
    assert len(commutative) >= len(ordered)


def test_match_get_uint_on_const():
    g = _build_graph()
    c = Capture()
    pat = int_const(c)
    hits = g.find_all(pat)
    if hits:
        v = hits[0].uint(c)
        assert v is None or isinstance(v, int)


def test_match_readers_align_with_node():
    """`Match.uint` / `sint` / `boolean` are the canonical const readers. The
    old `const_`-prefixed names are gone (clean break), and no builtin-
    shadowing `int` / `bool` name exists.
    """

    # `aarch64/builtins::expect_branch` carries a surviving I1 IntConst under
    # the default pipeline.
    elf = fixture_path("aarch64", "builtins")
    lift = strider.lift.load_elf(str(elf))
    _cfg, g, _u = lift.analyze("expect_branch")
    c = Capture()

    hits = g.find_all(int_const(c).bool_valued())
    assert hits, "expect_branch must carry a surviving I1 IntConst to test"
    m = hits[0]

    assert m.boolean(c) is True or m.boolean(c) is False
    assert isinstance(m.uint(c), int)
    assert isinstance(m.sint(c), int)

    for gone in ("const_uint", "const_int", "const_bool", "int", "bool"):
        assert not hasattr(m, gone), f"{gone} must not exist after the rename"


def test_bound_capture_view_delegates_and_guards():
    """`m[c]` mirrors the match readers; `_opt` returns None and the plain
    reader raises for an unbound capture."""
    import pytest
    g = _build_graph()
    c = Capture()
    hits = g.find_all(int_const(c))
    if not hits:
        return
    m = hits[0]
    assert m[c].has is True
    assert m[c].uint == m.uint(c)
    assert m[c].node is not None

    unbound = Capture()
    assert m[unbound].has is False
    assert m[unbound].uint_opt is None
    with pytest.raises(strider.StriderError):
        m[unbound].uint


def test_match_getitem_view_agrees_with_uint():
    """`m[c]` is a `BoundCapture`; its `uint` (and `int(m[c])`) agree
    with `m.uint(c)`, stay unsigned, and compare numerically."""
    g = _build_graph()
    c = Capture()
    hits = g.find_all(int_const(c))
    if not hits:
        return
    for m in hits:
        uint_val = m.uint(c)
        assert m[c].uint == uint_val
        assert int(m[c]) == uint_val
        assert m[c] == uint_val  # numeric __eq__
        assert m[c].uint >= 0


def test_partial_and_post_match_boolean_is_bool_type():
    """`m[c].boolean` for an I1 constant is a Python `bool` from both the
    post-match Match and the in-`.when()` one. `bool` subclasses `int`, hence
    the exact `type(...) is bool` assertions rather than `isinstance`.
    """

    # `aarch64/builtins::expect_branch` keeps a surviving I1 IntConst under
    # the default pipeline, so this exercises the contract for real rather
    # than skipping.
    elf = fixture_path("aarch64", "builtins")
    lift = strider.lift.load_elf(str(elf))
    _cfg, g, _u = lift.analyze("expect_branch")
    c = Capture()

    post = g.find_all(int_const(c).bool_valued())
    assert post, "expect_branch must carry a surviving I1 IntConst to test"
    for m in post:
        assert type(m[c].boolean) is bool, (
            f"m[c].boolean for an I1 const must be bool, got "
            f"{type(m[c].boolean).__name__}"
        )

    seen_types: list[type] = []

    def predicate(m):
        seen_types.append(type(m[c].boolean))
        return True

    hits = g.find_all(int_const(c).bool_valued().when(predicate))
    assert hits, "predicate-guarded match must still fire"
    assert seen_types, "predicate must have been invoked"
    assert all(t is bool for t in seen_types), (
        "in-.when() m[c].boolean for an I1 const must be bool (not int), "
        f"matching the post-match Match; got {[t.__name__ for t in seen_types]}"
    )


def test_when_receives_match():
    """`.when(f)` calls `f` with a genuine `strider.pattern.Match`, not a
    partial-match proxy type. `Match` already returns `None`/`False` for
    captures unbound at predicate-eval time, so no partial type is needed.
    """
    from strider.pattern import anything

    g = _build_graph()
    seen: dict[str, type] = {}
    c = Capture()

    def predicate(m):
        seen.setdefault("t", type(m))
        return True

    pat = int_add(int_const(c), anything()).when(predicate)
    g.find_all(pat)
    assert seen, "predicate must have been invoked at least once"
    assert seen["t"].__name__ == "Match"
    assert seen["t"] is strider.pattern.Match


def test_find_all_with_when_predicate_mutating_graph_is_safe():
    """A `.when()` predicate calling a mutating method on the same graph
    raises StriderError: the query holds the function borrowed for its walk.
    """
    lift, g = built_lifter_and_function("x86", "memory", "array_sum", optimize=False)
    errors_caught: list[str] = []

    def predicate(_m):
        try:
            lift.optimize(g)
        except strider.StriderError as e:
            errors_caught.append(str(e))
        return True

    c = Capture()
    pat = int_const(c).when(predicate)
    # A deadlock surfaces as a hung test; the assertion below catches the
    # "ran but didn't error" silent-failure case instead.
    hits = g.find_all(pat)
    if hits:
        assert errors_caught, (
            "Mutating call from inside .when() predicate must raise StriderError "
            "(not deadlock and not silently succeed)"
        )


def test_when_predicate_keyboard_interrupt_propagates():
    """`KeyboardInterrupt` from a predicate must escape `find_all`. When it
    was swallowed, Ctrl-C could not interrupt a slow walk stuck in a
    predicate.
    """
    import pytest


    g = _build_graph()
    counter = [0]

    def predicate(_m):
        counter[0] += 1
        if counter[0] >= 1:
            raise KeyboardInterrupt
        return True

    c = Capture()
    pat = int_const(c).when(predicate)
    with pytest.raises(KeyboardInterrupt):
        g.find_all(pat)


def test_when_predicate_system_exit_propagates():
    """`SystemExit` from a predicate must propagate, not be swallowed and
    treated as no-match.
    """
    import pytest


    g = _build_graph()

    def predicate(_m):
        raise SystemExit(0)

    c = Capture()
    pat = int_const(c).when(predicate)
    with pytest.raises(SystemExit):
        g.find_all(pat)


def test_when_predicate_exception_reaches_the_caller():
    """A buggy predicate must not abort the walk mid-flight, so its exception
    is stashed and re-raised once the query finishes."""

    g = _build_graph()

    def predicate(_m):
        raise ValueError("predicate is buggy")

    c = Capture()
    with pytest.raises(ValueError, match="predicate is buggy"):
        g.find_all(int_const(c).when(predicate))


def test_of_width_and_bool_output_constrain_find_count():
    """`.of_width(n)` / `.bool_valued()` constrain the matched node's
    value-output width. A node has exactly one such width, so the width-1
    and width-64 sets are disjoint and both are subsets of `anything()`.
    """
    from strider.pattern import anything

    g = _build_graph()

    total = len(g.find_all(anything()))
    assert total > 0

    bools = g.find_all(anything().of_width(1))
    wide = g.find_all(anything().of_width(64))
    # bool_valued is sugar for of_width(1).
    assert len(g.find_all(anything().bool_valued())) == len(bools)
    assert len(bools) <= total
    assert len(wide) <= total
    assert len(bools) + len(wide) <= total
    # No IR node is 7 bits wide.
    assert g.find_all(anything().of_width(7)) == []


def test_of_width_nested_under_op():
    """`.of_width` composes nested inside an op: an operand constrained to a
    width no IR node has kills the whole match.
    """
    from strider.pattern import anything, int_add, var

    g = _build_graph()
    base, off = Capture(), Capture()
    loose = g.find_all(int_add(var(base), var(off)))
    assert isinstance(loose, list)
    tight = g.find_all(int_add(var(base).of_width(7), var(off)))
    assert tight == []


def test_output_ty_exact_type():
    """`.value_ty("i1")` matches the same set as `.of_width(1)`; an
    unknown type name raises a StriderError."""
    import pytest

    from strider.pattern import anything

    g = _build_graph()
    by_width = g.find_all(anything().of_width(1))
    by_type = g.find_all(anything().value_ty("i1"))
    assert len(by_width) == len(by_type)
    # Case-insensitive.
    assert len(g.find_all(anything().value_ty("I1"))) == len(by_type)
    with pytest.raises(strider.StriderError):
        # Deliberate: no such value type.
        anything().value_ty("i7")  # type: ignore[arg-type]
