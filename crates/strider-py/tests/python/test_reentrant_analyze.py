"""A `MemReader.read` callback that re-enters `analyze` on the same
`Lifter` must raise `StriderError`, not panic.

The callback runs inside rsleigh's nounwind `extern "C"` instruction-fetch
frame, so a `PyBorrowError` panic escaping `read` cannot unwind: Rust calls
`abort()` and the interpreter dies with SIGABRT (exit 134). An abort cannot
be caught in-process, so both cases run in a subprocess and assert the real
exit code.
"""

from __future__ import annotations

import subprocess
import sys

_PRELUDE = r"""
import strider

BASE = 0x1000
CODE = b"\x48\x83\xc0\x01\xc3"  # add rax, 1 ; ret

arch = strider.sleigh.SleighArch.x86_64()
cc = strider.sleigh.CallingConvention.x86_64_systemv
inner = strider.reader.BufferReader(BASE, CODE)


def opts():
    return strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    )


holder = {}
nested = []
"""

# The nested error is caught inside `read`, so it never crosses the FFI
# frame: this pins the exception TYPE the re-entrant call raises.
_CHILD_CAUGHT = _PRELUDE + r"""
class SelfReentrant(strider.reader.MemReader):
    n = 0

    def read(self, addr, size):
        if SelfReentrant.n == 0:
            SelfReentrant.n = 1
            try:
                holder["lift"].analyze(BASE, cc(), opts=opts())
            except BaseException as e:
                nested.append(type(e).__name__)
        return inner.read(addr, size)


holder["lift"] = strider.lift.lifter(arch, SelfReentrant())
try:
    holder["lift"].analyze(BASE, cc(), opts=opts())
    outer = "ok"
except BaseException as e:
    outer = type(e).__name__
print("NESTED:", nested[:1])
print("OUTER:", outer)
"""

# The nested error escapes `read` into rsleigh's nounwind callback frame:
# this pins that the process survives it.
_CHILD_PROPAGATED = _PRELUDE + r"""
class SelfReentrant(strider.reader.MemReader):
    n = 0

    def read(self, addr, size):
        if SelfReentrant.n == 0:
            SelfReentrant.n = 1
            holder["lift"].analyze(BASE, cc(), opts=opts())
        return inner.read(addr, size)


holder["lift"] = strider.lift.lifter(arch, SelfReentrant())
try:
    holder["lift"].analyze(BASE, cc(), opts=opts())
    outer = "ok"
except BaseException as e:
    outer = type(e).__name__
print("OUTER:", outer)
"""


def _run_child(script):
    return subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        timeout=300,
    )


def test_reentrant_analyze_raises_strider_error():
    p = _run_child(_CHILD_CAUGHT)
    assert p.returncode == 0, f"exit {p.returncode}\n{p.stdout}\n{p.stderr}"
    assert "NESTED: ['StriderError']" in p.stdout, p.stdout
    assert "OUTER: ok" in p.stdout, p.stdout


def test_reentrant_analyze_error_does_not_abort_the_process():
    p = _run_child(_CHILD_PROPAGATED)
    assert p.returncode == 0, (
        f"child exited {p.returncode} (134 = SIGABRT: a non-unwinding panic "
        f"crossed rsleigh's FFI frame)\nstdout: {p.stdout}\nstderr: {p.stderr}"
    )
    # The failed read is reported as a lift failure, not a panic.
    assert "OUTER: StriderError" in p.stdout, p.stdout
