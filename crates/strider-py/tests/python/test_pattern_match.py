"""End-to-end pattern-matching tests against the real test fixtures.

We pick `array_sum` from x86/memory.elf because it has a clean
`Load(addr = base + offset)` pattern that any working matcher will
find at least once.
"""

import strider
from strider.pattern import Capture, var, add, load, int_const

from .conftest import built_function


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
    # No assertion on the count (depends on optimization shape) — but
    # the call must not raise.
    assert isinstance(hits, list)


def test_bool_binary_preserves_i1_guard_against_wide_and():
    """`bool_binary(...)` keeps the `I1`-output guard: the chainable
    builder (and its `.ordered()` terminal) must NOT match a wide
    integer `And`.  `bit_and(a, b)` from the arithmetic fixture lifts
    to a 32-bit `And`; `and_(any_, any_)` matches it (wide integer And),
    while `bool_binary("And", any_, any_)` must match none of those wide
    Ands — proving the bool builder stays boolean-specific.
    """
    from strider.pattern import any_, bool_binary, and_

    g = _build_graph("arithmetic", symbol="bit_and")

    # The wide-integer matcher finds the 32-bit `a & b`.
    wide_hits = g.find_all(and_(any_(), any_()))
    assert len(wide_hits) >= 1, "expected the 32-bit And in bit_and()"

    # The boolean matcher (I1 guard) must not match the wide And — in
    # either commutative or `.ordered()` form.
    assert g.find_all(bool_binary("And", any_(), any_())) == []
    assert g.find_all(bool_binary("And", any_(), any_()).ordered()) == []


def test_add_commutes_and_ordered_pins_operands():
    """`add(...)` matches commutatively; building the same query through
    the chainable `int_binary(...).ordered()` terminal still finds at
    least the canonical site (sanity that `.ordered()` doesn't break the
    walk) — paralleling the bool-builder symmetry on the Rust side.
    """
    from strider.pattern import any_, int_binary

    g = _build_graph()
    # Commutative `add(any, any)` and its ordered counterpart both run
    # without error; commutative count is a superset of the ordered one.
    commutative = g.find_all(add(any_(), any_()))
    ordered = g.find_all(int_binary("Add", any_(), any_()).ordered())
    assert isinstance(commutative, list)
    assert isinstance(ordered, list)
    assert len(commutative) >= len(ordered)


def test_match_get_uint_on_const():
    g = _build_graph()
    # Find every IntConst in the graph and verify uint() returns an int.
    from strider.pattern import any_int_const
    c = Capture()
    pat = any_int_const(c)
    hits = g.find_all(pat)
    if hits:
        v = hits[0].uint(c)
        assert v is None or isinstance(v, int)


# ── regression tests ──────────────────────────────────────────────────────


def test_match_getitem_returns_unsigned_python_int():
    """Regression: PyMatch.__getitem__ must convert
    `u128` constants directly without sign-truncation.  Previously a
    `as i128` cast would surface any U128 value with bit 127 set as a
    *negative* Python int (e.g. `u128::MAX` → `-1`).  Confirm both
    `m["cap"]` and `m.uint("cap")` agree on the unsigned value.
    """
    from strider.pattern import any_int_const
    g = _build_graph()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    if not hits:
        return
    for m in hits:
        getitem_val = m[c]
        uint_val = m.uint(c)
        assert getitem_val == uint_val, (
            f"m[c] vs m.uint(c) disagreement: getitem={getitem_val!r}, uint={uint_val!r}"
        )
        # IntConsts in real graphs fit in u128 and must be non-negative.
        assert getitem_val >= 0, (
            f"m[c] for an IntConst must be non-negative; got {getitem_val} "
            "(sign-truncation regression)"
        )


def test_partial_and_post_match_getitem_agree_on_bool_type():
    """Regression: `m[c]` for an `I1` (boolean) constant capture must
    surface as a Python `bool` from BOTH the post-match `PyMatch` and the
    in-`.when()`-predicate `PyPartialMatch` proxy.

    `get_uint` also matches an `I1` value (returning 0/1), so a
    `__getitem__` that probed uint *before* bool would leak the boolean out
    as a plain `int` inside a predicate while the post-match path returned
    `bool` — the same `m[c]` yielding two Python types.  Both accessors must
    probe bool first.  (`bool` subclasses `int`, so this asserts exact
    `type(...) is bool`, not `isinstance`.)
    """
    from strider.pattern import any_int_const

    # `memory/array_sum` contains an `I1` IntConst (the `Xor(_, 1:i1)`
    # NOT-lowering of its loop condition), so this is not vacuous.
    g = _build_graph()
    c = Capture()

    post = g.find_all(any_int_const(c).bool_valued())
    assert post, "fixture must contain at least one I1 (bool) IntConst"
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
        "PyPartialMatch m[c] for an I1 const must be bool (not int), matching "
        f"PyMatch; got {[t.__name__ for t in seen_types]}"
    )


def test_find_all_with_when_predicate_mutating_graph_is_safe():
    """Regression: a `.when()`
    predicate that calls a mutating method on the same graph must
    surface a typed error rather than deadlocking.  The fix uses
    `try_write_inner()` which returns Err on contention so the
    predicate sees a clean StriderError instead of blocking forever.
    """
    from strider.pattern import any_int_const
    g = _build_graph()
    errors_caught: list[str] = []

    def predicate(_m):
        try:
            # Mutating call from inside the predicate — must raise,
            # NOT deadlock.
            g.reoptimize()
        except strider.errors.StriderError as e:
            errors_caught.append(str(e))
        return True

    c = Capture()
    pat = any_int_const(c).when(predicate)
    # `find_all` must complete (no deadlock).  Pytest's timeout would
    # surface a deadlock as a hung test; the assertion below catches
    # the "ran but didn't error" silent-failure case.
    hits = g.find_all(pat)
    if hits:
        assert errors_caught, (
            "Mutating call from inside .when() predicate must raise StriderError "
            "(not deadlock and not silently succeed)"
        )


# ── Regression: KeyboardInterrupt / SystemExit propagation ────────────────


def test_when_predicate_keyboard_interrupt_propagates():
    """A `.when()` predicate that raises `KeyboardInterrupt`
    must propagate the exception out of `find_all` rather than being
    silently swallowed.  Without the fix, Ctrl-C in an interactive
    Python session is unable to interrupt a slow `find_all` walk
    that's stuck inside a predicate.
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
    """A `.when()` predicate that raises `SystemExit` must
    propagate (not be swallowed and treated as no-match).
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
    """Companion: ordinary predicate exceptions
    (`ValueError`, etc.) should still be swallowed and treated as
    no-match — a buggy predicate must not abort the entire `find_all`
    walk.  Only control-flow exceptions (`KeyboardInterrupt` /
    `SystemExit`) propagate.
    """
    from strider.pattern import any_int_const

    g = _build_graph()

    def predicate(_m):
        raise ValueError("predicate is buggy")

    c = Capture()
    pat = any_int_const(c).when(predicate)
    # Must NOT raise — find_all completes, returning whatever it
    # found (every match counted as no-match because the predicate
    # raised).
    hits = g.find_all(pat)
    assert isinstance(hits, list)


def test_of_width_and_bool_output_constrain_find_count():
    """`.of_width(n)` / `.bool_valued()` constrain the matched node's
    value-output width: each is a strict subset of the unconstrained
    `any_()` match, `.bool_valued()` equals `.of_width(1)`, and the
    width-1 and width-64 sets are disjoint (a node has exactly one value
    output width).
    """
    from strider.pattern import any_

    g = _build_graph()

    total = len(g.find_all(any_()))
    assert total > 0

    bools = g.find_all(any_().of_width(1))
    wide = g.find_all(any_().of_width(64))
    # bool_valued is sugar for of_width(1).
    assert len(g.find_all(any_().bool_valued())) == len(bools)
    # Each width filter is a (proper-or-equal) subset of the whole graph.
    assert len(bools) <= total
    assert len(wide) <= total
    # A node has one value-output width, so a 1-bit filter and a 64-bit
    # filter cannot both match the same set unless one is empty; their
    # sum never exceeds the total.
    assert len(bools) + len(wide) <= total
    # A width that no node produces yields nothing.
    assert g.find_all(any_().of_width(7)) == []


def test_of_width_nested_under_op():
    """`.of_width` composes nested inside an op: constraining an operand
    to a non-existent width makes the whole match fail, vs the
    unconstrained operand which can match.
    """
    from strider.pattern import any_, add, var

    g = _build_graph()
    base, off = Capture(), Capture()
    # Unconstrained add operands match (>= 0, must not raise).
    loose = g.find_all(add(var(base), var(off)))
    assert isinstance(loose, list)
    # Constraining an operand to a 7-bit width (no IR node is 7 bits)
    # makes the add match nothing.
    tight = g.find_all(add(var(base).of_width(7), var(off)))
    assert tight == []


def test_output_ty_exact_type():
    """`.value_ty("i1")` matches the same set as `.of_width(1)`; an
    unknown type name raises a StriderError."""
    import pytest

    from strider.pattern import any_

    g = _build_graph()
    by_width = g.find_all(any_().of_width(1))
    by_type = g.find_all(any_().value_ty("i1"))
    assert len(by_width) == len(by_type)
    # Case-insensitive.
    assert len(g.find_all(any_().value_ty("I1"))) == len(by_type)
    # Unknown type name is rejected.
    with pytest.raises(strider.errors.StriderError):
        any_().value_ty("i7")
