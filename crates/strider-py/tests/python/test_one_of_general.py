"""`one_of` (OR) and `first_of` (OR, first-match cut) are general combinators:
an arm is anything a top-level pattern is, and the alternation nests in any
operand slot (value, memory, control), mirroring how `find_all([...])` (AND)
accepts anything in its list."""

import strider
from strider import pattern as p
from .conftest import fixture_path


def _lift(path, sym):
    prog = strider.lift.load_elf(path)
    _c, fn, _u = prog.analyze(sym)
    return fn


def _calls_fn():
    prog = strider.lift.load_elf(str(fixture_path("x86", "calls")))
    for sym in prog.functions():
        _c, fn, _u = prog.analyze(sym.address)
        if fn.find_all(p.ret().ctrl(p.call())):
            return fn
    raise AssertionError("no function with a call before a ret")


def test_one_of_in_control_slot_matches_like_the_single_arm():
    # ctrl(call()) matched 1; one_of([load, call]) must match it too,
    # not silently return 0.
    fn = _calls_fn()
    single = len(fn.find_all(p.ret().ctrl(p.call())))
    assert single >= 1
    union = len(fn.find_all(p.ret().ctrl(p.one_of([p.load(), p.call()]))))
    assert union == single


def test_one_of_in_memory_slot():
    # A memory-input slot accepts an OR of memory producers.
    fn = _lift(str(fixture_path("x86", "memory")), "array_sum")
    direct = len(fn.find_all(p.load().mem(p.store())))
    union = len(fn.find_all(p.load().mem(p.one_of([p.store(), p.mem_phi()]))))
    assert union >= direct


def _load_after_call_fn():
    prog = strider.lift.load_elf(str(fixture_path("x86", "calls")))
    for sym in prog.functions():
        _c, fn, _u = prog.analyze(sym.address)
        if fn.find_all(p.load().mem(p.call())):
            return fn
    raise AssertionError("no function loading through a call's memory token")


def test_one_of_in_memory_slot_takes_a_call_arm():
    # A call produces a memory token as well as values, and the slot decides
    # which edge an arm anchors on, so the arm matches what `mem(call())`
    # matches on its own.
    fn = _load_after_call_fn()
    direct = len(fn.find_all(p.load().mem(p.call())))
    assert direct >= 1
    union = len(fn.find_all(p.load().mem(p.one_of([p.store(), p.call()]))))
    assert union == direct


def test_one_of_union_semantics_at_value_slot():
    # Both overlapping arms fire (union), one match each.
    fn = _lift(str(fixture_path("x86", "memory")), "array_sum")
    x = p.Capture("x")
    hits = fn.find_all(p.load(addr=p.one_of([p.anything().capture(x), p.int_add(p.anything(), p.anything()).capture(x)])))
    assert hits


def test_node_rooted_control_arms():
    # ret / if_else / switch / indirect_branch / unreachable are valid arms:
    # the core synthesizes an `Any` output for the alternation to wire.
    fn = _calls_fn()
    rets = len(fn.find_all(p.ret()))
    calls = len(fn.find_all(p.call()))
    assert rets >= 1 and calls >= 1
    # Return and Call are disjoint node kinds, so the OR is their sum.
    assert len(fn.find_all(p.one_of([p.ret(), p.call()]))) == rets + calls
    # Every node-rooted kind at least compiles as an arm without raising.
    for arm in (p.if_else(), p.switch(), p.indirect_branch(), p.unreachable()):
        fn.find_all(p.one_of([arm, p.call()]))


def test_first_of_cuts_where_one_of_unions():
    # A wildcard arm and a narrower arm both match an add; one_of yields both
    # bindings, first_of commits to the first arm only.
    fn = _lift(str(fixture_path("x86", "memory")), "array_sum")
    x = p.Capture("x")
    add_addr = p.load(addr=p.int_add(p.anything(), p.anything()))
    n_adds = len(fn.find_all(add_addr))
    assert n_adds >= 1

    wild_then_add = [p.anything().capture(x), p.int_add(p.anything(), p.anything()).capture(x)]
    union = fn.find_all(p.load(addr=p.one_of(wild_then_add)))
    first = fn.find_all(p.load(addr=p.first_of(wild_then_add)))
    # first_of commits to the wildcard arm, so never more matches than the union.
    assert len(first) <= len(union)
    assert len(first) >= 1


def test_captured_one_of_in_memory_slot():
    # `.cap` on the alternation must not change which slots accept it.
    fn = _lift(str(fixture_path("x86", "memory")), "array_sum")
    plain = len(fn.find_all(p.load().mem(p.one_of([p.store(), p.mem_phi()]))))
    capped = fn.find_all(p.load().mem(p.one_of([p.store(), p.mem_phi()]).capture("m")))
    assert len(capped) == plain
    assert all("m" in m for m in capped)
