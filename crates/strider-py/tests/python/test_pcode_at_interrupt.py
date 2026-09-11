"""`Lifter.pcode_at` sweeps with the GIL held, so Ctrl-C reaches the caller
only where the sweep asks for pending signals.

Without that ask the interrupt is queued and delivered when the call returns,
which on a large region is minutes of an uninterruptible interpreter.
"""

from __future__ import annotations

import signal
import time

import pytest

import strider

#: Long enough that an unchecked sweep runs for seconds past the interrupt
#: (~2.5 MB/s), short enough to bound the test.
_REGION = 8 * 1024 * 1024

_BASE = 0x1000

_AFTER = 0.5

#: Generous against a loaded machine, still far under the whole sweep.
_DEADLINE = 2.0


@pytest.mark.skipif(not hasattr(signal, "setitimer"), reason="POSIX timers only")
def test_an_interrupt_lands_while_the_sweep_is_still_running():
    mem = strider.reader.BufferReader(_BASE, b"\x90" * _REGION)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)

    previous = signal.signal(signal.SIGALRM, signal.default_int_handler)
    signal.setitimer(signal.ITIMER_REAL, _AFTER)
    started = time.monotonic()
    try:
        with pytest.raises(KeyboardInterrupt):
            lift.pcode_at(_BASE, _BASE + _REGION - 1)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)
    waited = time.monotonic() - started - _AFTER
    assert waited < _DEADLINE, f"interrupt surfaced {waited:.1f}s after it was raised"
