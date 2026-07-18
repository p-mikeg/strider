"""Regression: `strider.lift.load_elf` must not leak its backing bytes.

The loader used to `Box::leak` the whole file to fabricate a `'static`
`object::File`, so every `load_elf` call retained one file-sized buffer that
`del` + `gc.collect()` could never reclaim — RSS grew without bound (visibly
so on a vmlinux: ~file-size per call).  This loops the loader far more times
than its working set and asserts resident memory stays bounded.
"""

from __future__ import annotations

import gc
import sys

import pytest

import strider

from .conftest import fixture_path


def _rss_bytes() -> int:
    # statm field 1 (resident pages) * page size.
    with open("/proc/self/statm") as f:
        return int(f.read().split()[1]) * 4096


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="needs /proc")
def test_load_elf_does_not_leak_backing_bytes():
    path = str(fixture_path("x64", "arithmetic"))
    file_size = __import__("os").path.getsize(path)

    # Warm up so the baseline is steady (allocator arenas, one-time tables).
    for _ in range(10):
        del_target = strider.lift.load_elf(path)
        del del_target
    gc.collect()
    base = _rss_bytes()

    iters = 300
    for _ in range(iters):
        elf = strider.lift.load_elf(path)
        del elf
        gc.collect()
    growth = _rss_bytes() - base

    # Pre-fix, growth ≈ iters * file_size (one leaked copy per call).
    # Post-fix the bytes free on drop, so growth is allocator noise.
    budget = max(file_size * 8, 4 * 1024 * 1024)
    assert growth < budget, (
        f"RSS grew {growth} B over {iters} load_elf calls "
        f"(file {file_size} B, budget {budget} B) — load_elf is leaking"
    )
