"""``strider.StriderError`` must carry a stack trace even when the
caller never set ``RUST_BACKTRACE`` / ``RUST_LIB_BACKTRACE``.

Sweeping a large symbol corpus, the errors land in scripts started
without those env vars, and asking the user to re-run with them set
doubles the feedback loop.  Backtrace capture is forced at import time
unless the user set the env var to a falsy value first.

The check runs a child Python with both vars cleared.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap


_TRIGGER = textwrap.dedent(
    """\
    import strider

    mem = strider.reader.BufferReader(0x1000, b"\\x00" * 4)
    arch = strider.sleigh.SleighArch.aarch64()
    cc = strider.sleigh.CallingConvention.aarch64_aapcs64()
    try:
        strider.lift.lifter(arch, mem, rom=mem).analyze(0x10000, cc)
    except strider.StriderError as e:
        # Print to stdout so the parent test can read it back without
        # reassembling captured stderr/traceback frames.
        print(str(e))
    """
)


def _run_without_backtrace_env() -> str:
    env = {k: v for k, v in os.environ.items() if k not in ("RUST_BACKTRACE", "RUST_LIB_BACKTRACE")}
    out = subprocess.run(
        [sys.executable, "-c", _TRIGGER],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    return out.stdout


def test_strider_error_includes_backtrace_without_env_var():
    """With both env vars unset, the error's ``str()`` must still hold a
    backtrace header and at least one strider frame."""
    msg = _run_without_backtrace_env()
    # Case-insensitive: anyhow releases differ on capitalisation
    # (`stack backtrace:` vs `Stack backtrace:`).
    assert "stack backtrace:" in msg.lower(), (
        f"no 'Stack backtrace:' header in StriderError message:\n{msg}"
    )
    # ``cfg::`` ties the check to the lift path the trigger exercises. If
    # that path changes, update the marker rather than dropping it: the
    # point is that the trace names our code, not a stdlib frame.
    assert "cfg::" in msg, (
        f"no Rust strider frame in StriderError message:\n{msg}"
    )
