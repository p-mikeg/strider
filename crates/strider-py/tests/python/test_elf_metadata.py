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
