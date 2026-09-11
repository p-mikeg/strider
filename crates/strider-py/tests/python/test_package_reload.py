"""`importlib.reload(strider)` must re-run the package body cleanly.

The extension submodules survive a reload, so every mutation the package makes
to them has to be idempotent, and the two attributes the import system binds
through the module dict are already gone the second time round.
"""

from __future__ import annotations

import importlib

import strider


def test_reload_keeps_dunder_all_free_of_duplicates():
    before = {
        name: list(getattr(strider, name).__all__)
        for name in ("lift", "opt", "cfg", "reader", "template", "pattern")
    }
    importlib.reload(strider)
    for name, want in before.items():
        got = list(getattr(strider, name).__all__)
        assert got == want, f"strider.{name}.__all__ drifted on reload"
        assert len(got) == len(set(got)), f"strider.{name}.__all__ has duplicates"


def test_reload_leaves_the_package_usable():
    importlib.reload(strider)
    mem = strider.reader.BufferReader(0x1000, b"\x89\xf8\xc3")
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    assert fn.node_count() > 0
    assert not hasattr(strider, "_strider")
    assert not hasattr(strider, "_api")
