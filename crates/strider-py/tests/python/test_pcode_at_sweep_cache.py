"""`pcode_at` reuses its sweep engine, and must still answer as a fresh one.

Cloning the Sleigh is a whole `Sleigh::new` plus a context replay, tens of
milliseconds against the microseconds the one instruction it decodes costs, so
walking an instruction stream was one clone per address. The engine is kept
between calls instead, keyed by the entry it swept from and retired by anything
that commits context; decoding writes flow context back into it, which is why
the key is needed at all.
"""

from __future__ import annotations

import re
import struct

import strider

#: The p-code text renders a space by its host pointer, which differs between
#: any two Sleigh instances and says nothing about the decode.
_SPACE_PTR = re.compile(r"0x[0-9a-f]{10,}")


def _norm(text: str) -> str:
    return _SPACE_PTR.sub("<space>", text)


BASE = 0x1000


def _arm_interworking() -> bytes:
    """ARM at 0x1000 that branches into Thumb at 0x1020, so the same bytes
    decode differently depending on the mode the sweep carries."""
    words = {BASE + i * 4: 0xE1A00000 for i in range(0x10)}  # nop (mov r0,r0)
    words[0x1000] = 0xE59F0004  # ldr r0, [pc, #4]
    words[0x1004] = 0xE12FFF10  # bx r0
    words[0x100C] = 0x1021  # -> Thumb 0x1020
    words[0x1020] = 0x46C046C0  # nop ; nop (Thumb)
    words[0x1024] = 0x4770E7FE  # b . ; bx lr
    return b"".join(struct.pack("<I", words[a]) for a in sorted(words))


def _fresh(code: bytes):
    mem = strider.reader.BufferReader(BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.arm(), mem, rom=mem)


def _addrs(code: bytes) -> list[int]:
    lift = _fresh(code)
    lift.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())
    out = []
    for a in range(BASE, BASE + 0x18, 4):
        try:
            lift.pcode_at(BASE, a)
        except strider.StriderError:
            continue
        out.append(a)
    return out


def test_a_repeated_sweep_answers_as_a_one_shot_one():
    code = _arm_interworking()
    addrs = _addrs(code)
    assert addrs, "precondition: the sweep must reach something"

    reused = _fresh(code)
    reused.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())
    for a in addrs:
        one_shot = _fresh(code)
        one_shot.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())
        assert _norm(reused.pcode_at(BASE, a)) == _norm(
            one_shot.pcode_at(BASE, a)
        ), hex(a)


def test_interleaving_entries_does_not_carry_state_between_them():
    """A different entry gets its own engine; the first entry must still read
    as it did before the second one swept."""
    code = _arm_interworking()
    lift = _fresh(code)
    lift.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())
    first = lift.pcode_at(BASE, BASE + 4)
    lift.pcode_at(BASE + 4, BASE + 4)
    assert lift.pcode_at(BASE, BASE + 4) == first


def test_an_analysis_between_sweeps_does_not_stale_the_answer():
    """`analyze` can commit context, which the cached engine predates."""
    code = _arm_interworking()
    lift = _fresh(code)
    before = lift.pcode_at(BASE, BASE)
    lift.analyze(BASE, strider.sleigh.CallingConvention.arm_aapcs())
    after = lift.pcode_at(BASE, BASE)
    assert _norm(after) == _norm(_fresh(code).pcode_at(BASE, BASE))


def _interworking_with_spare_entries(count: int) -> bytes:
    """`_arm_interworking` followed by `count` ARM `bx lr` words, each of which
    is also a legal Thumb entry, so a sweep of entries at both ISA modes pins a
    fresh decode mode per address."""
    return _arm_interworking() + struct.pack("<I", 0xE12FFF1E) * count


#: Enough alternating entries to pass the engine's context-commit log limit.
_ENTRIES = 1000


def test_a_sweep_past_the_context_commit_limit_refuses_rather_than_guesses():
    """The sweep engine is a clone, and a clone carries the pinned decode modes
    only while the engine still holds its commit log. Past that the clone
    starts from the pspec defaults, which for an interworking binary decodes
    the Thumb address as ARM: that answer must be refused, never returned.
    """
    code = _interworking_with_spare_entries(_ENTRIES)
    lift = _fresh(code)
    cc = strider.sleigh.CallingConvention.arm_aapcs()
    lift.analyze(BASE, cc)
    thumb = _norm(lift.pcode_at(BASE, 0x1020))

    spare = BASE + 0x40
    for i in range(_ENTRIES):
        for entry in (spare + 4 * i, spare + 4 * i + 1):
            try:
                lift.build_cfg(entry)
            except strider.StriderError:
                pass  # a mode this address does not decode in still pins one
        if (i + 1) % 200:
            continue
        try:
            after = _norm(lift.pcode_at(BASE, 0x1020))
        except strider.StriderError:
            return
        assert after == thumb, f"sweep changed ISA mode after {i + 1} entries"
