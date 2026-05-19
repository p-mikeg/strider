"""Phase 4 Task 4.0 — verify the V2 reference type type-checks under
mypy --strict.

The consumer at `_consumer_reference.py` exercises every method on
`strider.pattern.StackStorePatV2`.  This test runs `mypy --strict`
on it and asserts a zero exit.  When the Task 4.1 proc-macro lands,
the same consumer must continue to type-check unchanged — the macro
emits the same `.pyi` shape the hand-written stub already exposes.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

CONSUMER = Path(__file__).parent / "_consumer_reference.py"


def _mypy_available() -> bool:
    """Detect whether `mypy` is importable in the current Python env.
    Prefer this over `shutil.which("mypy")` since the dev workflow
    (`uv run --with mypy pytest …`) installs mypy as a Python package
    rather than dropping a shell wrapper on PATH."""
    return importlib.util.find_spec("mypy") is not None


@pytest.mark.skipif(not _mypy_available(), reason="mypy not installed")
def test_reference_consumer_passes_mypy_strict() -> None:
    """Run `mypy --strict` on the V2 reference consumer."""
    # Run mypy as a subprocess so the test exit codes pass through
    # cleanly even when mypy's API surface changes between versions.
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "mypy",
            "--strict",
            str(CONSUMER),
        ],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, (
        f"mypy --strict failed:\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}\n"
    )


def test_reference_consumer_runs() -> None:
    """Smoke-test the consumer at runtime — every method call must
    succeed against the actual extension module, not just the .pyi.
    Catches the case where the stubs lie about the runtime surface."""
    # Import the consumer module by path.  `_consumer_reference` is
    # under `tests/python/` so it's importable when pytest is invoked
    # from the strider-py root.
    sys.path.insert(0, str(CONSUMER.parent))
    try:
        import _consumer_reference  # type: ignore[import-not-found]

        _consumer_reference.build_with_v2()
        _consumer_reference.build_via_free_function()
    finally:
        sys.path.pop(0)
