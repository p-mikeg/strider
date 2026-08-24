"""``strider.StriderError`` must reach the caller with a stack trace even when
the caller never set ``RUST_BACKTRACE`` / ``RUST_LIB_BACKTRACE``.

Sweeping a large symbol corpus, the errors land in scripts started without those
env vars, and asking the user to re-run with them set doubles the feedback loop.
So capture is forced at import time and the trace is always on ``.backtrace``,
while ``str(e)`` stays the one line a caller can act on. ``STRIDER_BACKTRACE=1``
folds the trace back into the message.

The checks run a child Python with both vars cleared.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap


def _trigger(body: str) -> str:
    return textwrap.dedent(
        """\
        import strider

        mem = strider.reader.BufferReader(0x1000, b"\\x00" * 4)
        arch = strider.sleigh.SleighArch.aarch64()
        cc = strider.sleigh.CallingConvention.aarch64_aapcs64()
        try:
            strider.lift.lifter(arch, mem, rom=mem).analyze(0x10000, cc)
        except strider.StriderError as e:
        """
    ) + textwrap.indent(textwrap.dedent(body), "        ")


def _run(body: str, **env_overrides: str) -> str:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("RUST_BACKTRACE", "RUST_LIB_BACKTRACE", "STRIDER_BACKTRACE")
    }
    env.update(env_overrides)
    out = subprocess.run(
        [sys.executable, "-c", _trigger(body)],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    return out.stdout


def test_backtrace_is_reachable_without_any_env_var():
    """The trace is captured and kept, so a sweep logs it without re-running.

    ``cfg::`` ties the check to the lift path the trigger exercises. If that
    path changes, update the marker rather than dropping it: the point is that
    the trace names our code, not a stdlib frame.
    """
    msg = _run("print(e.backtrace)")
    assert "stack backtrace:" in msg.lower(), f"no backtrace header on .backtrace:\n{msg}"
    assert "cfg::" in msg, f"no Rust strider frame on .backtrace:\n{msg}"


def test_str_is_the_message_alone():
    """Without the env var, the message is what the caller can act on: no
    backtrace header, and short enough to read in a sweep's log."""
    msg = _run("print(len(str(e).splitlines())); print(str(e))")
    lines, rest = msg.split("\n", 1)
    assert int(lines) == 1, f"expected a one-line message, got {lines}:\n{rest}"
    assert "stack backtrace:" not in rest.lower(), f"backtrace leaked into str(e):\n{rest}"


def test_env_var_folds_the_trace_into_the_message():
    msg = _run("print(str(e))", STRIDER_BACKTRACE="1")
    assert "stack backtrace:" in msg.lower(), f"STRIDER_BACKTRACE=1 did not add the trace:\n{msg}"
