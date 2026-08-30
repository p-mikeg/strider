"""ARM EABI records its float ABI in `e_flags`, and a soft/softfp image
passes floats in the core registers. Handing it `arm_aapcs` makes d0-d7
phantom float carriers and d0-d3 phantom float returns."""

import shutil
import struct

from strider._api import (
    _EF_ARM_ABI_FLOAT_HARD,
    _EF_ARM_ABI_FLOAT_SOFT,
    _arch_and_cc_for_elf,
    _ElfHeader,
)

SRC = "fixtures/out/arm/calling_convention.elf"


def _cc_for(flags, tmp_path):
    dst = tmp_path / "probe.elf"
    shutil.copy(SRC, dst)
    with open(dst, "r+b") as f:
        # ELF32 e_flags is at offset 36.
        f.seek(36)
        f.write(struct.pack("<I", flags))
    return _arch_and_cc_for_elf(_ElfHeader(str(dst)))[1].name()


def test_the_shipped_arm_fixtures_are_hard_float():
    header = _ElfHeader(SRC)
    assert header.e_flags & _EF_ARM_ABI_FLOAT_HARD
    arch, cc = _arch_and_cc_for_elf(header)
    assert arch.name() == "arm"
    assert cc.name() == "arm_aapcs"


def test_float_abi_bit_selects_the_convention(tmp_path):
    base = 0x0500_0000
    assert _cc_for(base | _EF_ARM_ABI_FLOAT_HARD, tmp_path) == "arm_aapcs"
    assert _cc_for(base | _EF_ARM_ABI_FLOAT_SOFT, tmp_path) == "arm_aapcs_soft"
    # Neither bit set (pre-EABI5 objects, hand-written asm) falls to hard.
    assert _cc_for(base, tmp_path) == "arm_aapcs"


def test_big_endian_arm_also_gets_the_float_abi(tmp_path):
    dst = tmp_path / "be.elf"
    shutil.copy("fixtures/out/arm_be/calling_convention.elf", dst)
    header = _ElfHeader(str(dst))
    fmt = "<I" if header.is_little_endian else ">I"
    with open(dst, "r+b") as f:
        f.seek(36)
        f.write(struct.pack(fmt, header.e_flags | _EF_ARM_ABI_FLOAT_SOFT))
    arch, cc = _arch_and_cc_for_elf(_ElfHeader(str(dst)))
    assert arch.name().startswith("arm_be")
    assert cc.name() == "arm_aapcs_soft"


def test_autodetected_soft_image_has_no_float_carrier(tmp_path):
    """End to end: with the soft bit set, `load_elf` picks a convention under
    which a float parameter has no VFP carrier at all."""
    import strider
    from strider import pattern as p

    def float_args(path):
        prog = strider.lift.load_elf(str(path))
        _cfg, fn, _u = prog.analyze("f64_arith")
        return len(fn.find_all(p.function_arg_float(0).capture(p.Capture())))

    src = "fixtures/out/arm/floats.elf"
    dst = tmp_path / "soft_floats.elf"
    shutil.copy(src, dst)
    with open(dst, "r+b") as f:
        f.seek(36)
        f.write(struct.pack("<I", 0x0500_0000 | _EF_ARM_ABI_FLOAT_SOFT))
    assert float_args(src) == 1
    assert float_args(dst) == 0
