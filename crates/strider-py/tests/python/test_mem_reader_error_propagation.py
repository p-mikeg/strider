"""When a subclassed `MemReader.read` raises, its message must survive
into the `StriderError` that bubbles back up.

Regression: a generic "callback failed" message used to replace the
original `str(e)`, leaving users unable to debug their own reader.
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
