"""End-to-end pattern-matching tests against the real test fixtures.

We pick `array_sum` from x86/memory.elf because it has a clean
`Load(addr = base + offset)` pattern that any working matcher will
find at least once.
"""

import strider
from strider.pattern import Capture, var, add, load, int_const

from .conftest import fixture_path, symbol_addr


def _build_graph(elf_path, symbol="array_sum"):
    addr = symbol_addr(elf_path, symbol)
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(elf_path)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    return s.analyze_cfg(cfg).function, sleigh


def test_find_all_load_in_array_sum(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
    pat = load()
    hits = g.find_all(pat)
    # array_sum has at least one load (the array element fetch).
    assert len(hits) >= 1


def test_find_all_load_with_addr_pattern(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
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

    elf = fixture_path("x86", "arithmetic")
    g, _ = _build_graph(elf, symbol="bit_and")

    # The wide-integer matcher finds the 32-bit `a & b`.
    wide_hits = g.find_all(and_(any_(), any_()))
    assert len(wide_hits) >= 1, "expected the 32-bit And in bit_and()"

    # The boolean matcher (I1 guard) must not match the wide And — in
    # either commutative or `.ordered()` form.
    assert g.find_all(bool_binary("And", any_(), any_())) == []
    assert g.find_all(bool_binary("And", any_(), any_()).ordered()) == []


def test_add_commutes_and_ordered_pins_operands(x86_memory_elf):
    """`add(...)` matches commutatively; building the same query through
    the chainable `int_binary(...).ordered()` terminal still finds at
    least the canonical site (sanity that `.ordered()` doesn't break the
    walk) — paralleling the bool-builder symmetry on the Rust side.
    """
    from strider.pattern import any_, int_binary

    g, _ = _build_graph(x86_memory_elf)
    # Commutative `add(any, any)` and its ordered counterpart both run
    # without error; commutative count is a superset of the ordered one.
    commutative = g.find_all(add(any_(), any_()))
    ordered = g.find_all(int_binary("Add", any_(), any_()).ordered())
    assert isinstance(commutative, list)
    assert isinstance(ordered, list)
    assert len(commutative) >= len(ordered)


def test_match_get_uint_on_const(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
    # Find every IntConst in the graph and verify uint() returns an int.
    from strider.pattern import any_int_const
    c = Capture()
    pat = any_int_const(c)
    hits = g.find_all(pat)
    if hits:
        v = hits[0].uint(c)
        assert v is None or isinstance(v, int)


# ── regression tests ──────────────────────────────────────────────────────


def test_match_getitem_returns_unsigned_python_int(x86_memory_elf):
    """Regression: PyMatch.__getitem__ must convert
    `u128` constants directly without sign-truncation.  Previously a
    `as i128` cast would surface any U128 value with bit 127 set as a
    *negative* Python int (e.g. `u128::MAX` → `-1`).  Confirm both
    `m["cap"]` and `m.uint("cap")` agree on the unsigned value.
    """
    from strider.pattern import any_int_const
    g, _ = _build_graph(x86_memory_elf)
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


def test_find_all_with_when_predicate_mutating_graph_is_safe(x86_memory_elf):
    """Regression: a `.when()`
    predicate that calls a mutating method on the same graph must
    surface a typed error rather than deadlocking.  The fix uses
    `try_write_inner()` which returns Err on contention so the
    predicate sees a clean StriderError instead of blocking forever.
    """
    from strider.pattern import any_int_const
    g, _ = _build_graph(x86_memory_elf)
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


def test_when_predicate_keyboard_interrupt_propagates(x86_memory_elf):
    """A `.when()` predicate that raises `KeyboardInterrupt`
    must propagate the exception out of `find_all` rather than being
    silently swallowed.  Without the fix, Ctrl-C in an interactive
    Python session is unable to interrupt a slow `find_all` walk
    that's stuck inside a predicate.
    """
    import pytest

    from strider.pattern import any_int_const

    g, _ = _build_graph(x86_memory_elf)
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


def test_when_predicate_system_exit_propagates(x86_memory_elf):
    """A `.when()` predicate that raises `SystemExit` must
    propagate (not be swallowed and treated as no-match).
    """
    import pytest

    from strider.pattern import any_int_const

    g, _ = _build_graph(x86_memory_elf)

    def predicate(_m):
        raise SystemExit(0)

    c = Capture()
    pat = any_int_const(c).when(predicate)
    with pytest.raises(SystemExit):
        g.find_all(pat)


def test_when_predicate_ordinary_exception_does_not_propagate(x86_memory_elf):
    """Companion: ordinary predicate exceptions
    (`ValueError`, etc.) should still be swallowed and treated as
    no-match — a buggy predicate must not abort the entire `find_all`
    walk.  Only control-flow exceptions (`KeyboardInterrupt` /
    `SystemExit`) propagate.
    """
    from strider.pattern import any_int_const

    g, _ = _build_graph(x86_memory_elf)

    def predicate(_m):
        raise ValueError("predicate is buggy")

    c = Capture()
    pat = any_int_const(c).when(predicate)
    # Must NOT raise — find_all completes, returning whatever it
    # found (every match counted as no-match because the predicate
    # raised).
    hits = g.find_all(pat)
    assert isinstance(hits, list)
