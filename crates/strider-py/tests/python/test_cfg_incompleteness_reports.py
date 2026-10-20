"""`Cfg.incomplete` reports what `analyze` accumulated, not what the last round
happens to still carry.

The resolver re-lifts until the induced edge set converges, and a round that
raised a report can be rebuilt away: seating a clashing arm costs the site, so
the next round has neither the arm nor the clash. Reading the final CFG alone
launders that round, and Python would then claim a completeness Rust does not.
"""

from __future__ import annotations

import struct

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
    which is the clash. The clashing arm is dropped and the Thumb arm re-seated,
    so the round after it rebuilds reaching 0x1042 only as interior bytes of the
    Thumb region, and the `bx r0` it discovered keeps the loop going for one
    more round. Nothing in the returned CFG says two modes ever arrived there;
    the report is what carries it.
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
    return lift.analyze(
        BASE,
        strider.sleigh.CallingConvention.arm_aapcs(),
    )


def test_a_mode_clash_is_reported_after_its_round_is_rebuilt_away() -> None:
    result = _analyze_arm()
    # Precondition: the returned CFG reaches CLASH only through the surviving
    # Thumb arm, so no ARM decode of it is left to read the clash off. A
    # final-CFG answer would prove nothing about the sticky report.
    assert result.cfg.region_at(CLASH) == result.cfg.region_at(0x1041), (
        f"precondition: {CLASH:#x} must be interior to the Thumb arm's region"
    )
    assert [a for a, r in result.cfg.incomplete if r == "isa_mode_conflict"] == [CLASH]
    # The clash costs its own arm; the dispatch stays a live placeholder.
    assert result.unresolved == [0x1010]


def _x86_interior_program() -> bytes:
    """x86-64 at 0x1000: `movabs rax, 0` (ten bytes), `je 0x1005` (five bytes
    into that `movabs`), then `ret`."""
    return bytes([0x48, 0xB8]) + bytes(8) + bytes([0x74, 0xF9, 0xC3])


def _x86_lifter() -> strider.lift.Lifter:
    mem = strider.reader.BufferReader(BASE, _x86_interior_program())
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def test_an_interior_branch_target_reaches_python() -> None:
    result = _x86_lifter().analyze(
        BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
    )
    assert result.cfg.incomplete == [(0x1005, "interior_target")]


def test_build_cfg_reports_the_one_build_it_ran() -> None:
    """No resolver runs, so that build's own reports are the whole answer;
    with no dispatch, no site reason has anything to say."""
    cfg = _x86_lifter().build_cfg(BASE)
    assert cfg.incomplete == [(0x1005, "interior_target")]


def test_a_clean_analysis_reports_nothing() -> None:
    code = bytes([0x48, 0x31, 0xC0, 0xC3])  # xor rax, rax ; ret
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    result = lift.analyze(BASE, strider.sleigh.CallingConvention.x86_64_systemv())
    assert result.cfg.incomplete == []


def test_a_return_seed_answers_the_dispatch_unverified() -> None:
    """Seating a `Return` here answers the dispatch outright, so `unresolved`
    stays empty and the seat alone is reported."""
    code = bytes([0xB8, 0x08, 0x10, 0x00, 0x00, 0xFF, 0xE0, 0x90, 0xC3])
    dispatch = 0x1005
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    result = lift.analyze(
        BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(known_targets={dispatch: "return"}),
        ),
    )
    assert result.unresolved == []
    assert result.cfg.incomplete == [(dispatch, "unverified_seed")]


def test_a_return_seed_vouches_for_an_unproven_ret() -> None:
    """A `"return"` seed over a `ret` whose target is not the caller's moves
    it from `"unresolved_branch"` to `"unverified_seed"`."""
    code = bytes([0x50, 0xC3])  # push rax ; ret
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    assert lift.analyze(BASE, cc).cfg.incomplete == [(0x1001, "unresolved_branch")]
    seeded = lift.analyze(
        BASE,
        cc,
        strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(known_targets={0x1001: "return"}),
        ),
    )
    assert seeded.unresolved == []
    assert seeded.cfg.incomplete == [(0x1001, "unverified_seed")]


_REASONS = (
    "unresolved_branch",
    "unverified_seed",
    "isa_mode_conflict",
    "interior_target",
    "unmapped_target",
    "undecodable_target",
    "past_function_end",
)


def test_incomplete_reasons_are_the_seven_in_order() -> None:
    assert strider.cfg.INCOMPLETE_REASONS == _REASONS


def test_incomplete_lists_each_site_with_its_reason() -> None:
    code = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])  # mov rax, [rdi] ; jmp rax
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    result = lift.analyze(
        BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
    )
    assert result.cfg.incomplete == [(0x1003, "unresolved_branch")]
    assert result.unresolved == [0x1003]


def test_build_cfg_incomplete_names_the_dispatch_it_left() -> None:
    code = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    cfg = lift.build_cfg(BASE)
    assert cfg.incomplete == [(0x1003, "unresolved_branch")]


def test_a_fall_through_past_the_size_is_a_reported_tail_call() -> None:
    # xor eax, eax; then a lock cmpxchg past the 2-byte size, never decoded
    code = bytes([0x31, 0xC0, 0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58])
    mem = strider.reader.BufferReader(BASE, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    opts = strider.cfg.CfgOptions(function_max_size=2)
    result = lift.analyze(
        BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(cfg=opts),
    )
    assert result.cfg.incomplete == [(BASE + 2, "past_function_end")]
    assert lift.build_cfg(BASE, opts).incomplete == [(BASE + 2, "past_function_end")]


def test_incomplete_sorts_by_address_across_reasons() -> None:
    reasons = [r for _, r in _analyze_arm().cfg.incomplete]
    sites = [a for a, _ in _analyze_arm().cfg.incomplete]
    assert sites == sorted(sites)
    assert {"isa_mode_conflict", "unresolved_branch"} <= set(reasons)
