"""``nop``, ``paciasp``, ``autiasp`` and several hint encodings produce no
pcode at all, leaving address holes in a region's instruction list.  Two
cfg-builder failures came out of that: a fall-through across such a hole
finalising a region with no instructions, and a branch target landing on
a hole address that ``contains_addr`` claimed but exact-match lookup
could not find.

The two shapes live in ``fixtures/cases/zero_pcode_holes.S``.
"""

from __future__ import annotations

import pathlib

import strider

from .conftest import fixture_path


def _lift_aarch64(elf_path: pathlib.Path, symbol: str):
    loaded = strider.lift.load_elf(str(elf_path))
    mem = loaded.reader()
    sym = loaded.symbol(symbol)
    sleigh_arch = strider.sleigh.SleighArch.aarch64()
    cc = strider.sleigh.CallingConvention.aarch64_aapcs64()
    lift = strider.lift.lifter(sleigh_arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        sym.address,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(
                function_max_size=sym.size, allow_code_before_start_addr=True
            )
        ),
    )
    return function


def test_aarch64_nop_fallthrough_lifts_cleanly():
    """``nop_fallthrough`` falls through a literal ``nop`` into an
    already-explored region's start.  Used to raise ``"region at
    PcodeInsnAddr ... has no instructions"``."""
    elf = fixture_path("aarch64", "zero_pcode_holes")
    g = _lift_aarch64(elf, "nop_fallthrough")
    assert g.node_count() > 0


def test_aarch64_autiasp_split_lifts_cleanly():
    """``autiasp_split``'s ``cbz`` branches to an ``autiasp`` address
    inside an already-built region.  Used to raise ``"split address ...
    not found in region NodeIndex(N)'s instruction list"``."""
    elf = fixture_path("aarch64", "zero_pcode_holes")
    g = _lift_aarch64(elf, "autiasp_split")
    assert g.node_count() > 0
