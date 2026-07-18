"""Regression tests for the AArch64 zero-pcode-op gap bugs.

AArch64 instructions like ``nop``, ``paciasp``, ``autiasp``, and
several hint-class encodings lift to **zero** pcode ops on the Sleigh
spec strider uses.  Two distinct cfg-builder failures were caused by
this:

1. **"region at PcodeInsnAddr ... has no instructions"** — the cfg
   builder's outer loop walked across one or more zero-pcode-op
   machine instructions before reaching an already-explored
   region's start.  The fall-through path tried to finalise the
   current builder with empty ``insns`` and region finalisation
   rejected it.  Fix: when ``self.insns`` is empty at fall-through, hot-wire
   the parent edge straight into the existing region instead.

2. **"split address ... not found in region's instruction list"** —
   a branch target landed at the address of a zero-pcode-op
   instruction that wasn't recorded in the region's ``insns``, so
   ``contains_addr``'s lexicographic range test said yes but the
   exact-match ``position`` lookup said no.  Fix: round down to the
   largest insn whose address is ≤ the requested split address.

The two regression shapes live in the in-repo aarch64 fixture
``fixtures/cases/zero_pcode_holes.S`` — ``nop_fallthrough`` for bug 1
and ``autiasp_split`` for bug 2.  Each test skips cleanly when the
fixture wasn't built (no aarch64 toolchain in a contributor's env).
"""

from __future__ import annotations

import pathlib

import strider

from .conftest import fixture_path


def _lift_aarch64(elf_path: pathlib.Path, symbol: str):
    loaded = strider.lift.load_elf(str(elf_path))
    mem = loaded.reader()
    entry, size = loaded._elf.symbol_addr_and_size(symbol)
    sleigh_arch = strider.sleigh.SleighArch.aarch64()
    cc = strider.sleigh.CallingConvention.aarch64_aapcs64()
    lift = strider.lift.lifter(sleigh_arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        entry,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(function_max_size=size, allow_code_before_start_addr=True)
        ),
    )
    return function


# ── Bug 1: empty-insns fall-through across zero-pcode-op stretches ────────────


def test_aarch64_nop_fallthrough_lifts_cleanly():
    """``nop_fallthrough`` is a hand-written aarch64 stub whose
    fall-through path crosses a literal ``nop`` (zero pcode ops) into
    an already-explored region's start.  Pre-fix this tripped
    the region-finalisation non-empty invariant with
    ``"region at PcodeInsnAddr ... has no instructions"``."""
    elf = fixture_path("aarch64", "zero_pcode_holes")
    g = _lift_aarch64(elf, "nop_fallthrough")
    assert g.node_count() > 0


# ── Bug 2: split-into-zero-pcode-op-hole ──────────────────────────────────────


def test_aarch64_autiasp_split_lifts_cleanly():
    """``autiasp_split``'s ``cbz`` branches to the address of an
    ``autiasp`` (zero pcode ops) sitting inside an already-built
    region.  Pre-fix this raised ``"split address ... not found in
    region NodeIndex(N)'s instruction list"``."""
    elf = fixture_path("aarch64", "zero_pcode_holes")
    g = _lift_aarch64(elf, "autiasp_split")
    assert g.node_count() > 0
