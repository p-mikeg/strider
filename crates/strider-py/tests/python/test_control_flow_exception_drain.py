"""A `KeyboardInterrupt` / `SystemExit` raised inside a Python
reader callback is stashed rather than raised (raising would be destroyed by
the next callback), so every entry point that can reach a callback has to
drain the stash before returning.  Otherwise the exception detonates on a
later, entirely valid call; for `SystemExit`, by exiting the process.
"""

import pytest
import strider

# x86_64: `xor eax, eax; ret`
_BYTES = bytes.fromhex("31c0c3")
_BASE = 0x1000


class _Reader(strider.reader.MemReader):
    """Raises `exc` while armed; serves `_BYTES` once `exc` is cleared."""

    def __init__(self, exc):
        super().__init__()
        self.exc = exc

    def read(self, addr, size):
        if self.exc is not None:
            raise self.exc
        off = addr - _BASE
        if off < 0 or off >= len(_BYTES):
            return None
        return _BYTES[off : off + size]


def _lifter(reader):
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), reader)


def _assert_later_call_is_clean(lift, reader):
    reader.exc = None
    result = lift.analyze(_BASE, strider.sleigh.CallingConvention.x86_64_systemv())
    assert result.function.node_count() > 0


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_build_cfg_surfaces_stashed_control_flow(exc_type):
    reader = _Reader(exc_type("during build_cfg"))
    lift = _lifter(reader)
    with pytest.raises(exc_type):
        lift.build_cfg(_BASE)
    _assert_later_call_is_clean(lift, reader)


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_pcode_at_surfaces_stashed_control_flow(exc_type):
    reader = _Reader(exc_type("during pcode_at"))
    lift = _lifter(reader)
    with pytest.raises(exc_type):
        lift.pcode_at(_BASE, _BASE)
    _assert_later_call_is_clean(lift, reader)


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_analyze_surfaces_stashed_control_flow(exc_type):
    reader = _Reader(exc_type("during analyze"))
    lift = _lifter(reader)
    with pytest.raises(exc_type):
        lift.analyze(_BASE, strider.sleigh.CallingConvention.x86_64_systemv())
    _assert_later_call_is_clean(lift, reader)


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_stash_does_not_leak_into_optimize_or_dot(exc_type):
    """The post-stash `Function` API stays usable on a separate handle."""
    raiser = _Reader(exc_type("during build_cfg"))
    with pytest.raises(exc_type):
        _lifter(raiser).build_cfg(_BASE)

    reader = _Reader(None)
    lift = _lifter(reader)
    result = lift.analyze(_BASE, strider.sleigh.CallingConvention.x86_64_systemv())
    lift.optimize(result.function)
    assert result.function.neighborhood_dot(
        result.function.entry_node(), pretty=True
    )
    assert result.function.find_all(strider.pattern.ret()) is not None


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_a_failed_query_does_not_leave_a_when_exception_behind(exc_type):
    """A `.when()` predicate stashes rather than raises, so the query boundary
    has to drain on its OWN error path too, not just on success."""
    reader = _Reader(None)
    lift = _lifter(reader)
    function = lift.analyze(
        _BASE, strider.sleigh.CallingConvention.x86_64_systemv()
    ).function

    def boom(m):
        raise exc_type("from when")

    x, y = strider.pattern.Capture("x"), strider.pattern.Capture("y")
    with pytest.raises(strider.StriderError):
        # The join rejects `y`: it shares no capture with the guarded pattern.
        function.find_all(
            [strider.pattern.anything().capture(x).when(boom),
             strider.pattern.anything().capture(y)]
        )
    assert function.find_all(strider.pattern.ret()) is not None
