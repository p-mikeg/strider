"""`is_complete` on a `build_cfg` CFG answers about indirect branches too.

`unresolved` rides on `AnalyzeResult`, so a `build_cfg` CFG had nothing to
consult and reported `True` for the very function `analyze(resolve=False)`
reports an unresolved branch in.
"""

from __future__ import annotations

import strider

_BASE = 0x1000
#: mov rax, [rdi] ; jmp rax
_INDIRECT = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])
_DISPATCH = 0x1003


def _lifter(code: bytes):
    mem = strider.reader.BufferReader(_BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def test_an_unresolved_indirect_branch_is_not_complete():
    cfg = _lifter(_INDIRECT).build_cfg(_BASE)
    assert not cfg.is_complete()
    # The channel `analyze` would have filled agrees.
    result = _lifter(_INDIRECT).analyze(
        _BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(resolve_indirect_branches=False),
    )
    assert result.unresolved == [_DISPATCH]


def test_a_straight_line_build_is_still_complete():
    cfg = _lifter(bytes([0x48, 0x31, 0xC0, 0xC3])).build_cfg(_BASE)  # xor rax,rax ; ret
    assert cfg.is_complete()


def test_a_seated_branch_clears_the_indirect_channel():
    """The seat is still `unverified_seeded`, but no site is left edgeless."""
    cfg = _lifter(_INDIRECT).build_cfg(
        _BASE, strider.cfg.CfgOptions(known_targets={_DISPATCH: "return"})
    )
    assert cfg.unverified_seeded_sites() == [_DISPATCH]
    assert not cfg.is_complete()
