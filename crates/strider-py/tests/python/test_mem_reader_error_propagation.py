"""T-12: when a subclassed `MemReader.read` raises, the exception's
text must surface in the `StriderError` that bubbles back up.

Pre-fix the adapter used a generic "callback failed" message that
lost the original Python exception's `str(e)`.  Post-fix the adapter
preserves the original message so users can debug their Python
reader without attaching a debugger.

Pinned here so a future rewrite of the adapter's PyErr-handling
doesn't silently regress message visibility.
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
    # Any entry will do — the first read attempt triggers the
    # Python exception immediately.
    with pytest.raises(StriderError) as excinfo:
        strider.lift.lifter(arch, reader).analyze(0x1000, cc)

    msg = str(excinfo.value)
    assert _SENTINEL in msg, (
        f"Python exception text must survive in StriderError surface; "
        f"got: {msg!r}"
    )
