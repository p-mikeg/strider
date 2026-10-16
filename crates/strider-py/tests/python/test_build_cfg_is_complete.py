"""`is_complete` on a `build_cfg` CFG answers about indirect branches too.

A build resolves nothing, so `Cfg.incomplete` names every dispatch it left
without an edge, the same site `analyze` without resolution reports.
"""

from __future__ import annotations

import pytest

import strider

_BASE = 0x1000
#: mov rax, [rdi] ; jmp rax
_INDIRECT = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])
_DISPATCH = 0x1003


def _lifter(code: bytes):
    mem = strider.reader.BufferReader(_BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def test_an_unresolved_indirect_branch_is_not_complete():
    cfg = _lifter(_INDIRECT).build_cfg(_BASE, on_incomplete=strider.lift.OnIncomplete.IGNORE)
    assert not cfg.is_complete()
    # `analyze` without resolution agrees.
    result = _lifter(_INDIRECT).analyze(
        _BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            on_incomplete=strider.lift.OnIncomplete.IGNORE,
        ),
    )
    assert result.unresolved == [_DISPATCH]


def test_a_straight_line_build_is_still_complete():
    cfg = _lifter(bytes([0x48, 0x31, 0xC0, 0xC3])).build_cfg(_BASE)  # xor rax,rax ; ret
    assert cfg.is_complete()


def test_a_seated_branch_leaves_only_its_seat_reported():
    """The seat is still `"unverified_seed"`, but no site is left edgeless."""
    cfg = _lifter(_INDIRECT).build_cfg(
        _BASE, strider.cfg.CfgOptions(known_targets={_DISPATCH: "return"}),
        on_incomplete=strider.lift.OnIncomplete.IGNORE,
    )
    assert cfg.incomplete == [(_DISPATCH, "unverified_seed")]
    assert not cfg.is_complete()


#: jmp qword [rdi*8 + 0x2000] ; ret ; ret. A memory-indirect jump's
#: `BranchIndirect` comes after its address arithmetic, not at p-code op 0.
_MEM_INDIRECT = bytes([0xFF, 0x24, 0xFD, 0x00, 0x20, 0x00, 0x00, 0xC3, 0xC3])


@pytest.mark.parametrize(
    "seed", [[0x1007, 0x1008], "return"], ids=["switch", "return"]
)
def test_a_seed_seated_past_p_code_op_0_is_unverified(seed):
    """A Python seed is keyed by machine address; the build records the site
    at its `BranchIndirect`."""
    cfg = _lifter(_MEM_INDIRECT).build_cfg(
        _BASE, strider.cfg.CfgOptions(known_targets={_BASE: seed}),
        on_incomplete=strider.lift.OnIncomplete.IGNORE,
    )
    pcode = cfg.pcode_at(_BASE)
    assert pcode is not None and not pcode.startswith("BranchIndirect"), (
        "precondition: the site must not be p-code op 0"
    )
    assert [a for a, r in cfg.incomplete if r == "unverified_seed"] == [_BASE]
    assert not cfg.is_complete()
