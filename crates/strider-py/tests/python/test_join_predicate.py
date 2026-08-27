import pytest

import strider
from strider import pattern as p
from strider.pattern import constraints as cons


def _diamond_with_calls():
    # test edi,edi; je; call 0x2000; jmp; call 0x3000; call 0x4000; ret.
    # The call targets survive as IntConst nodes.
    code = bytes([
        0x85, 0xff,
        0x74, 0x07,
        0xe8, 0xf7, 0x0f, 0x00, 0x00,
        0xeb, 0x05,
        0xe8, 0xf0, 0x1f, 0x00, 0x00,
        0xe8, 0xeb, 0x2f, 0x00, 0x00,
        0xc3,
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _u = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def _consts(fn):
    x = p.Capture()
    return sorted({m.uint(x) for m in fn.find_all(p.int_const(x))})


class ConstEquals(cons.JoinPredicate):
    def __init__(self, cap: p.Capture, value: int):
        super().__init__()
        self.cap, self.value = cap, value

    def captures(self):
        return [self.cap]

    def constraint(self, m):
        return m.uint(self.cap) == self.value


def test_join_predicate_filters():
    fn = _diamond_with_calls()
    consts = _consts(fn)
    assert len(consts) >= 2, "need at least two constants to have something to exclude"
    target = consts[0]

    x = p.Capture()
    hits = fn.find_all([p.int_const(x)], constraints=[ConstEquals(x, target)])
    assert hits, "the target constant survives"
    assert all(h.uint(x) == target for h in hits), "only the target survives"


def test_join_predicate_default_captures_is_filter_only():
    fn = _diamond_with_calls()
    target = _consts(fn)[0]
    x = p.Capture()

    class FilterOnly(cons.JoinPredicate):
        # no captures() override -> defaults to [], a pure filter
        def constraint(self, m):
            return m.uint(x) == target

    hits = fn.find_all([p.int_const(x)], constraints=[FilterOnly()])
    assert hits and all(h.uint(x) == target for h in hits)


def test_join_predicate_connects_disjoint_patterns():
    fn = _diamond_with_calls()
    a, b = p.Capture(), p.Capture()

    # Two capture-bearing patterns that share NO capture: a bare join is rejected.
    with pytest.raises(strider.StriderError):
        fn.find_all([p.int_const(a), p.int_const(b)])

    class Ordered(cons.JoinPredicate):
        def captures(self):
            return [a, b]

        def constraint(self, m):
            return m.uint(a) < m.uint(b)

    # Declaring [a, b] correlates the two patterns, so the join runs.
    hits = fn.find_all([p.int_const(a), p.int_const(b)], constraints=[Ordered()])
    assert hits and all(h.uint(a) < h.uint(b) for h in hits)


def test_join_predicate_composes_under_negate():
    fn = _diamond_with_calls()
    target = _consts(fn)[0]
    x = p.Capture()

    kept = {
        h.uint(x)
        for h in fn.find_all(
            [p.int_const(x)], constraints=[cons.negate(ConstEquals(x, target))]
        )
    }
    assert target not in kept, "negate drops the target"
    assert kept, "every other constant survives"


def test_join_predicate_composes_in_any_of():
    fn = _diamond_with_calls()
    a, b = _consts(fn)[:2]
    x = p.Capture()

    kept = {
        h.uint(x)
        for h in fn.find_all(
            [p.int_const(x)],
            constraints=[cons.any_of([ConstEquals(x, a), ConstEquals(x, b)])],
        )
    }
    assert kept == {a, b}


def test_join_predicate_filter_exception_surfaces():
    fn = _diamond_with_calls()
    x = p.Capture()

    class Boom(cons.JoinPredicate):
        def captures(self):
            return [x]

        def constraint(self, m):
            raise ValueError("boom")

    with pytest.raises(ValueError, match="boom"):
        fn.find_all([p.int_const(x)], constraints=[Boom()])


def test_join_predicate_missing_filter_override_raises():
    fn = _diamond_with_calls()
    x = p.Capture()

    class NoFilter(cons.JoinPredicate):
        def captures(self):
            return [x]

    with pytest.raises(NotImplementedError):
        fn.find_all([p.int_const(x)], constraints=[NoFilter()])


def test_join_predicate_over_declared_capture_raises():
    fn = _diamond_with_calls()
    x, ghost = p.Capture(), p.Capture()

    class Ghost(cons.JoinPredicate):
        def captures(self):
            return [ghost]  # bound by no pattern

        def constraint(self, m):
            return True

    with pytest.raises(strider.StriderError):
        fn.find_all([p.int_const(x)], constraints=[Ghost()])


def test_non_constraint_object_rejected():
    fn = _diamond_with_calls()
    x = p.Capture()
    with pytest.raises(TypeError):
        # Deliberate: not a constraint.
        fn.find_all([p.int_const(x)], constraints=[object()])  # type: ignore[list-item]
