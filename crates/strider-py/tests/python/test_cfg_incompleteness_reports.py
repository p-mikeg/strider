"""`Cfg`'s incompleteness channels report what `analyze` accumulated, not what
the last round happens to still carry.

The resolver re-lifts until the induced edge set converges, and a round that
raised a report can be rebuilt away: seating a clashing arm costs the site, so
the next round has neither the arm nor the clash. Reading the final CFG alone
launders that round, and Python would then claim a completeness Rust does not.
"""

from __future__ import annotations

import struct
from typing import List

import strider

BASE = 0x1000
#: The ARM arm of the table below. It lands inside the region the Thumb arm
#: already decoded, so the two arms disagree about the mode those bytes are in.
CLASH = 0x1042


def _arm_program() -> bytes:
    """ARM at 0x1000, dispatching through a two-entry interworking table.

    ```text
    1000  cmp r0, #2
    1004  bhs 0x1014
    1008  add r2, pc, #8          ; the table at 0x1018
    100c  ldr r1, [r2, r0, lsl #2]
    1010  bx r1                   ; the dispatch
    1014  bx lr
    1018  .word 0x1041            ; Thumb 0x1040
    101c  .word 0x1042            ; ARM 0x1042, interior to the Thumb region
    1040  nop ; ldr r0, [pc, #4] ; bx r0 ; nop ; .word 0x1050   (Thumb)
    ```

    Seating the table decodes 0x1040 as Thumb and then reaches 0x1042 as ARM,
    which is the clash. That costs the dispatch, so the round after it rebuilds
    with no arm at 0x1042 at all -- but the Thumb-only `bx r0` it discovered
    keeps the loop going for one more round, so the returned CFG is the clean
    one.
    """
    words = {BASE + i * 4: 0xE12FFF1E for i in range(0x40)}  # bx lr
    words[0x1000] = 0xE3500002
    words[0x1004] = 0x2A000002
    words[0x1008] = 0xE28F2008
    words[0x100C] = 0xE7921100
    words[0x1010] = 0xE12FFF11
    words[0x1018] = 0x1041
    words[0x101C] = CLASH
    words[0x1040] = 0x480146C0  # nop ; ldr r0, [pc, #4]
    words[0x1044] = 0x46C04700  # bx r0 ; nop
    words[0x1048] = 0x1050
    return b"".join(struct.pack("<I", words[a]) for a in sorted(words))


def _analyze_arm() -> strider.lift.AnalyzeResult:
    mem = strider.reader.BufferReader(BASE, _arm_program())
    lift = strider.lift.lifter(strider.sleigh.SleighArch.arm(), mem, rom=mem)
    return lift.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())


def test_a_mode_clash_is_reported_after_its_round_is_rebuilt_away() -> None:
    result = _analyze_arm()
    assert result.cfg.region_at(CLASH) is None, (
        f"precondition: the returned CFG must no longer reach {CLASH:#x}, "
        "or the final-CFG read would answer and the test proves nothing"
    )
    assert result.cfg.isa_mode_conflicts() == [CLASH]
    # The clash costs the dispatch, which comes back as a live placeholder.
    assert result.unresolved == [0x1010]


def _x86_interior_program() -> bytes:
    """x86-64 at 0x1000: `movabs rax, 0` (ten bytes), `je 0x1005` -- five bytes
    into that `movabs` -- then `ret`."""
    return bytes([0x48, 0xB8]) + bytes(8) + bytes([0x74, 0xF9, 0xC3])


def _x86_lifter() -> strider.lift.Lifter:
    mem = strider.reader.BufferReader(BASE, _x86_interior_program())
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def test_an_interior_branch_target_reaches_python() -> None:
    result = _x86_lifter().analyze(
        BASE, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    assert result.cfg.interior_branch_targets() == [0x1005]


def test_build_cfg_reports_the_one_build_it_ran() -> None:
    """No resolver runs, so that build's own reports are the whole answer;
    only `unverified_seeded_sites` has nothing to say."""
    cfg = _x86_lifter().build_cfg(BASE)
    assert cfg.interior_branch_targets() == [0x1005]
    assert cfg.isa_mode_conflicts() == []
    assert cfg.unverified_seeded_sites() == []


def test_a_clean_analysis_reports_nothing() -> None:
    code = bytes([0x48, 0x31, 0xC0, 0xC3])  # xor rax, rax ; ret
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    result = lift.analyze(BASE, strider.sleigh.CallingConvention.x86_64_systemv())
    empty: List[int] = []
    assert result.cfg.isa_mode_conflicts() == empty
    assert result.cfg.interior_branch_targets() == empty
