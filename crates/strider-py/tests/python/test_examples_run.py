"""The shipped example scripts are executed, not just type-checked.

`pyproject.toml` puts `examples/python` under pyright, which catches a stale
identifier but not a script that imports cleanly and then raises. The README
points readers at these as the main learning path, so a broken one is a broken
tutorial.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

# python/ -> tests/ -> strider-py/ -> crates/ -> workspace root.
WORKSPACE_ROOT = pathlib.Path(__file__).resolve().parents[4]
EXAMPLES_DIR = WORKSPACE_ROOT / "crates" / "strider-py" / "examples" / "python"

# Each runs in about a second against the committed fixtures; 17_custom_abis
# builds several conventions and takes ~4s. The bound is for a hang, not a
# budget.
TIMEOUT_SECONDS = 120


def _scripts() -> list[pathlib.Path]:
    return sorted(EXAMPLES_DIR.glob("[0-9]*.py"))


def test_every_example_is_collected():
    """A renamed or unnumbered script would silently drop out of the sweep."""
    found = {p.name for p in _scripts()}
    on_disk = {p.name for p in EXAMPLES_DIR.glob("*.py")}
    assert found == on_disk, f"not collected: {sorted(on_disk - found)}"
    assert len(found) >= 17


@pytest.mark.parametrize("script", _scripts(), ids=lambda p: p.stem)
def test_example_runs(script: pathlib.Path):
    """Run from the workspace root: the scripts name fixtures relative to it."""
    proc = subprocess.run(
        [sys.executable, str(script)],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )
    assert proc.returncode == 0, (
        f"{script.name} exited {proc.returncode}\n"
        f"--- stdout ---\n{proc.stdout[-4000:]}\n"
        f"--- stderr ---\n{proc.stderr[-4000:]}"
    )
