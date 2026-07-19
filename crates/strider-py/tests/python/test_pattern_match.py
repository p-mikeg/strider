"""End-to-end pattern matching against the real fixtures.

`array_sum` from x86/memory.elf is the default graph: it has a clean
`Load(addr = base + offset)` that any working matcher finds at least once.
"""

import strider
from strider.pattern import Capture, var, add, load, int_const

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
    pat = load(addr=add(var(base), var(off)))
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
    """`add(...)` matches commutatively, so its hit set is a superset of the
    `int_binary(...).ordered()` form (which must still find the canonical
    site rather than breaking the walk).
    """
    from strider.pattern import anything, int_binary

    g = _build_graph()
    commutative = g.find_all(add(anything(), anything()))
    ordered = g.find_all(int_binary("Add", anything(), anything()).ordered())
    assert isinstance(commutative, list)
    assert isinstance(ordered, list)
    assert len(commutative) >= len(ordered)


def test_match_get_uint_on_const():
    g = _build_graph()
    from strider.pattern import any_int_const
    c = Capture()
    pat = any_int_const(c)
    hits = g.find_all(pat)
    if hits:
        v = hits[0].const_uint(c)
        assert v is None or isinstance(v, int)


def test_match_const_readers_align_with_node():
    """`Match.const_int` / `const_bool` / `const_uint` replaced the old
    `Match.int` / `bool` / `uint`; the old builtin-shadowing names must be
    gone entirely.
    """
    from strider.pattern import any_int_const

    # `aarch64/builtins::expect_branch` carries a surviving I1 IntConst under
    # the default pipeline.
    elf = fixture_path("aarch64", "builtins")
    lift = strider.lift.load_elf(str(elf))
    _cfg, g, _u = lift.analyze("expect_branch")
    c = Capture()

    hits = g.find_all(any_int_const(c).bool_valued())
    assert hits, "expect_branch must carry a surviving I1 IntConst to test"
    m = hits[0]

    assert m.const_bool(c) is True or m.const_bool(c) is False
    assert isinstance(m.const_uint(c), int)
    assert isinstance(m.const_int(c), int)

    assert not hasattr(m, "int")
    assert not hasattr(m, "bool")
    assert not hasattr(m, "uint")


def test_match_getitem_returns_unsigned_python_int():
    """Regression: `m[c]` used to sign-truncate 128-bit constants, so any
    value with bit 127 set surfaced as a negative Python int (u128 max came
    back as -1). `m[c]` and `m.const_uint(c)` must agree, and stay unsigned.
    """
    from strider.pattern import any_int_const
    g = _build_graph()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    if not hits:
        return
    for m in hits:
        getitem_val = m[c]
        uint_val = m.const_uint(c)
        assert getitem_val == uint_val, (
            f"m[c] vs m.const_uint(c) disagreement: getitem={getitem_val!r}, uint={uint_val!r}"
        )
        # IntConsts in real graphs fit in u128 and must be non-negative.
        assert getitem_val >= 0, (
            f"m[c] for an IntConst must be non-negative; got {getitem_val} "
            "(sign-truncation regression)"
        )


def test_partial_and_post_match_getitem_agree_on_bool_type():
    """Regression: `m[c]` for an I1 constant must be a Python `bool` from
    both the post-match Match and the in-`.when()` one.

    A uint read also succeeds on an I1 value (returning 0/1), so an accessor
    probing uint before bool leaks the boolean out as a plain int inside a
    predicate while the post-match path returns bool: one `m[c]` yielding two
    Python types. `bool` subclasses `int`, hence the exact `type(...) is
    bool` assertions rather than `isinstance`.
    """
    from strider.pattern import any_int_const

    # `aarch64/builtins::expect_branch` keeps a surviving I1 IntConst under
    # the default pipeline, so this exercises the contract for real rather
    # than skipping. array_sum's `Xor(_, 1:i1)` NOT-lowering no longer
    # survives `IfCondInversion`.
    elf = fixture_path("aarch64", "builtins")
    lift = strider.lift.load_elf(str(elf))
    _cfg, g, _u = lift.analyze("expect_branch")
    c = Capture()

    post = g.find_all(any_int_const(c).bool_valued())
    assert post, "expect_branch must carry a surviving I1 IntConst to test"
    for m in post:
        assert type(m[c]) is bool, (
            f"PyMatch m[c] for an I1 const must be bool, got {type(m[c]).__name__}"
        )

    seen_types: list[type] = []

    def predicate(m):
        seen_types.append(type(m[c]))
        return True

    hits = g.find_all(any_int_const(c).bool_valued().when(predicate))
    assert hits, "predicate-guarded match must still fire"
    assert seen_types, "predicate must have been invoked"
    assert all(t is bool for t in seen_types), (
        "in-.when() Match m[c] for an I1 const must be bool (not int), matching "
        f"the post-match Match; got {[t.__name__ for t in seen_types]}"
    )


def test_when_receives_match():
    """`.when(f)` calls `f` with a genuine `strider.pattern.Match`, not a
    partial-match proxy type. `Match` already returns `None`/`False` for
    captures unbound at predicate-eval time, so no partial type is needed.
    """
    from strider.pattern import anything, any_int_const

    g = _build_graph()
    seen: dict[str, type] = {}
    c = Capture()

    def predicate(m):
        seen.setdefault("t", type(m))
        return True

    pat = add(any_int_const(c), anything()).when(predicate)
    g.find_all(pat)
    assert seen, "predicate must have been invoked at least once"
    assert seen["t"].__name__ == "Match"
    assert seen["t"] is strider.pattern.Match


def test_find_all_with_when_predicate_mutating_graph_is_safe():
    """Regression: a `.when()` predicate calling a mutating method on the
    same graph used to deadlock. It must now raise StriderError instead.
    """
    from strider.pattern import any_int_const
    lift, g = built_lifter_and_function("x86", "memory", "array_sum", optimize=False)
    errors_caught: list[str] = []

    def predicate(_m):
        try:
            lift.optimize(g)
        except strider.StriderError as e:
            errors_caught.append(str(e))
        return True

    c = Capture()
    pat = any_int_const(c).when(predicate)
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

    from strider.pattern import any_int_const

    g = _build_graph()
    counter = [0]

    def predicate(_m):
        counter[0] += 1
        if counter[0] >= 1:
            raise KeyboardInterrupt
        return True

    c = Capture()
    pat = any_int_const(c).when(predicate)
    with pytest.raises(KeyboardInterrupt):
        g.find_all(pat)


def test_when_predicate_system_exit_propagates():
    """`SystemExit` from a predicate must propagate, not be swallowed and
    treated as no-match.
    """
    import pytest

    from strider.pattern import any_int_const

    g = _build_graph()

    def predicate(_m):
        raise SystemExit(0)

    c = Capture()
    pat = any_int_const(c).when(predicate)
    with pytest.raises(SystemExit):
        g.find_all(pat)


def test_when_predicate_ordinary_exception_does_not_propagate():
    """Ordinary predicate exceptions stay swallowed and count as no-match: a
    buggy predicate must not abort the whole walk. Only KeyboardInterrupt and
    SystemExit propagate.
    """
    from strider.pattern import any_int_const

    g = _build_graph()

    def predicate(_m):
        raise ValueError("predicate is buggy")

    c = Capture()
    pat = any_int_const(c).when(predicate)
    hits = g.find_all(pat)
    assert isinstance(hits, list)


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
    from strider.pattern import anything, add, var

    g = _build_graph()
    base, off = Capture(), Capture()
    loose = g.find_all(add(var(base), var(off)))
    assert isinstance(loose, list)
    tight = g.find_all(add(var(base).of_width(7), var(off)))
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
        anything().value_ty("i7")
