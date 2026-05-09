"""End-to-end pattern-matching tests against the real test fixtures.

We pick `array_sum` from x86/memory.elf because it has a clean
`Load(addr = base + offset)` pattern that any working matcher will
find at least once.
"""

import strider
from strider.pattern import Capture, var, add, load, int_const

from .conftest import symbol_addr


def _build_graph(elf_path, symbol="array_sum"):
    addr = symbol_addr(elf_path, symbol)
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf_path))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    return s.analyze_cfg(cfg).graph, sleigh


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


# ── Round 8 regression tests ───────────────────────────────────────────────


def test_match_getitem_returns_unsigned_python_int(x86_memory_elf):
    """Regression for round8-1F MED: PyMatch.__getitem__ must convert
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
    """Regression for round8-correctness-borrowing HIGH: a `.when()`
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
