"""When a subclassed `MemReader.read` raises, its message must survive
into the `StriderError` that bubbles back up.

A generic "callback failed" in place of the original `str(e)` leaves
users unable to debug their own reader.
"""

from __future__ import annotations

import pytest
import strider
from strider.reader import MemReader
from strider.sleigh import SleighArch, CallingConvention
from strider import StriderError


_SENTINEL = "T-12-canary-this-string-must-survive"


class _RaisingReader(MemReader):
    def read(self, addr: int, size: int) -> bytes:
        raise RuntimeError(_SENTINEL)


def test_mem_reader_python_exception_text_propagates_into_reader_error():
    reader = _RaisingReader()
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    # Any entry works; the first read attempt raises immediately.
    with pytest.raises(StriderError) as excinfo:
        strider.lift.lifter(arch, reader).analyze(0x1000, cc)

    msg = str(excinfo.value)
    assert _SENTINEL in msg, (
        f"Python exception text must survive in StriderError surface; "
        f"got: {msg!r}"
    )


def test_reader_exception_is_chained_as_cause():
    """The message survives, but only the original exception carries a
    traceback into the caller's own code."""

    class Raising(MemReader):
        def read(self, addr, size):
            raise LookupError("my-custom-failure")

    lift = strider.lift.lifter(SleighArch.x86_64(), Raising())
    with pytest.raises(StriderError) as excinfo:
        lift.analyze(0x1000, CallingConvention.x86_64_systemv())
    cause = excinfo.value.__cause__
    assert isinstance(cause, LookupError), f"cause was {cause!r}"
    assert cause.__traceback__ is not None


# 0x1000: test edi,edi; nop; ret. The fall-through 0x1002 is a later read.
_CODE = bytes([0x85, 0xFF, 0x90, 0xC3])


class _FailingAt(MemReader):
    def __init__(self, bad: int, result):
        super().__init__()
        self.bad = bad
        self.result = result

    def read(self, addr: int, size: int):
        if addr >= self.bad:
            return self.result(size)
        off = addr - 0x1000
        return _CODE[off:off + size] if 0 <= off < len(_CODE) else None


def _raise(size):
    raise ValueError("backend down")


@pytest.mark.parametrize("bad", [0x1000, 0x1002])
@pytest.mark.parametrize(
    "result, cause, text",
    [
        (_raise, ValueError, "backend down"),
        (lambda size: "abc", TypeError, "must return bytes"),
        (lambda size: b"\x90" * (size + 5), type(None), "more than requested"),
    ],
    ids=["raises", "wrong-type", "too-long"],
)
@pytest.mark.parametrize("call", ["analyze", "build_cfg"])
def test_reader_failure_fails_the_lift_at_any_address(bad, result, cause, text, call):
    lift = strider.lift.lifter(SleighArch.x86_64(), _FailingAt(bad, result))
    with pytest.raises(StriderError) as excinfo:
        if call == "analyze":
            lift.analyze(0x1000, CallingConvention.x86_64_systemv())
        else:
            lift.build_cfg(0x1000)
    msg = str(excinfo.value)
    assert text in msg
    assert "MemReader.read" in msg and "PyMemReader" not in msg
    assert "not mapped" not in msg
    assert isinstance(excinfo.value.__cause__, cause)


def test_reader_failure_does_not_leak_into_the_next_call():
    reader = _FailingAt(0x1002, _raise)
    lift = strider.lift.lifter(SleighArch.x86_64(), reader)
    with pytest.raises(StriderError):
        lift.analyze(0x1000, CallingConvention.x86_64_systemv())
    reader.bad = 1 << 63
    lift.analyze(0x1000, CallingConvention.x86_64_systemv())
