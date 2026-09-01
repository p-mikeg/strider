import os

import pytest

import strider

from .conftest import fixture_path


def _switch_elf():
    return strider.lift.load_elf(str(fixture_path("x86", "switch")))


def test_endianness_reports_byte_order():
    lift = _switch_elf()
    assert lift.endianness == "little"
    # Also on the arch directly, and for a big-endian preset.
    assert strider.sleigh.SleighArch.x86_64().endianness() == "little"
    assert strider.sleigh.SleighArch.mipsbe32().endianness() == "big"


def test_is_arm_be8_distinguishes_be8_from_be32(tmp_path):
    """`EI_DATA` marks BE8 and BE32 images alike, so only `EF_ARM_BE8` tells
    them apart, and picking the wrong one decodes byte-swapped instructions."""
    be32 = fixture_path("arm_be", "arithmetic")
    assert strider.lift.load_elf(str(be32)).is_arm_be8 is False

    # The same image with EF_ARM_BE8 set: e_flags is a big-endian u32 at
    # offset 36 of a 32-bit ELF header.
    raw = bytearray(be32.read_bytes())
    flags = int.from_bytes(raw[36:40], "big")
    raw[36:40] = (flags | 0x0080_0000).to_bytes(4, "big")
    be8 = tmp_path / "be8.elf"
    be8.write_bytes(raw)
    assert strider.lift.load_elf(str(be8)).is_arm_be8 is True


def test_is_arm_be8_is_false_off_arm():
    assert _switch_elf().is_arm_be8 is False


def test_be8_flag_picks_the_be8_arch(tmp_path):
    """A BE8 image stores instructions little-endian, so the plain `arm_be`
    preset decodes every one of them byte-swapped; `EF_ARM_BE8` is the only
    thing in the header that separates the two."""
    be32 = fixture_path("arm_be", "arithmetic")
    assert strider.lift.load_elf(str(be32)).arch.name() == "arm_be"

    raw = bytearray(be32.read_bytes())
    flags = int.from_bytes(raw[36:40], "big")
    raw[36:40] = (flags | 0x0080_0000).to_bytes(4, "big")
    be8 = tmp_path / "be8.elf"
    be8.write_bytes(raw)
    assert strider.lift.load_elf(str(be8)).arch.name() == "arm_be_kernel"


def test_a_rewrite_the_guard_cannot_see_raises_rather_than_panics(tmp_path):
    """`check_unchanged` compares a size and an mtime truncated to whole
    seconds, so a same-length rewrite inside one second passes it and the
    re-parse then runs on bytes that are no longer ELF. That used to abort out
    of Rust as `PanicException`, which derives from `BaseException` and so
    escapes `except Exception`.

    The mtime is restored rather than raced, so the guard passes every run."""
    victim = tmp_path / "victim.elf"
    victim.write_bytes(fixture_path("x64", "arithmetic").read_bytes())
    before = os.stat(victim)
    lifter = strider.lift.load_elf(str(victim))

    victim.write_bytes(b"\x00" * before.st_size)  # same length, not ELF
    os.utime(victim, (before.st_atime, before.st_mtime))

    with pytest.raises(strider.StriderError):
        lifter.entry_point()


def test_a_fifo_is_rejected_rather_than_blocking(tmp_path):
    """Opening a FIFO read-only blocks until a writer appears, so `load_elf`
    on one never returned. The check stats first, which does not open."""
    if not hasattr(os, "mkfifo"):
        pytest.skip("no mkfifo on this platform")
    fifo = tmp_path / "pipe"
    os.mkfifo(fifo)
    with pytest.raises((ValueError, strider.StriderError)):
        strider.lift.load_elf(str(fifo))
