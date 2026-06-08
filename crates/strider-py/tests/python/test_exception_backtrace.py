"""The Rust stack trace must always be present in
``strider.errors.StriderError`` messages, regardless of whether the
caller set ``RUST_LIB_BACKTRACE`` or ``RUST_BACKTRACE`` themselves.

When chasing failures across a large kernel-symbol corpus the lift
errors land in scripts that were started without the env var.  Asking
the user to re-run with ``RUST_LIB_BACKTRACE=1`` doubles their feedback
loop.  ``strider-py`` forces backtrace capture at module import time
(unless the user explicitly opted out by setting the env var to a
falsy value before importing).

The check spawns a child Python with both env vars cleared; if the
forcing in ``crates/strider-py/src/lib.rs`` is correct, the child's
``StriderError.__str__`` must contain a Rust source location.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap


_TRIGGER = textwrap.dedent(
    """\
    import strider

    mem = strider.BufferReader(0x1000, b"\\x00" * 4)
    arch = strider.SleighArch.aarch64()
    cc = strider.CallingConvention.aarch64_aapcs64()
    try:
        strider.run(arch=arch, cc=cc, mem=mem, rom=mem, entry=0x10000)
    except strider.errors.StriderError as e:
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
    """Even with ``RUST_BACKTRACE`` and ``RUST_LIB_BACKTRACE`` unset,
    the lift error's ``str()`` must contain a Rust stack backtrace.

    ``std::backtrace::Backtrace::Display`` always emits a
    ``"Stack backtrace:"`` (case-insensitive) header and at least one
    frame line containing a Rust path-qualified function name (e.g.
    ``cfg::cfg::builder::``).  Both must be present.
    """
    msg = _run_without_backtrace_env()
    # Header — case-insensitive because anyhow versions differ on
    # capitalisation across releases (`stack backtrace:` vs `Stack
    # backtrace:`).
    assert "stack backtrace:" in msg.lower(), (
        f"no 'Stack backtrace:' header in StriderError message:\n{msg}"
    )
    # At least one strider-stack frame.  Pinning to ``cfg::`` ties the
    # check to the lift path the trigger above exercises; if that
    # changes, update the marker rather than removing it (the goal
    # is to assert the trace points at our code, not a generic
    # stdlib frame).
    assert "cfg::" in msg, (
        f"no Rust strider frame in StriderError message:\n{msg}"
    )
