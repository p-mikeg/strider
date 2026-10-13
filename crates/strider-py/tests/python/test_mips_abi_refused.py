"""ELF32 MIPS carries its ABI in `e_flags`, and O32 is the only one strider
has a preset for. n32 wants a 64-bit decoder with 32-bit addresses, arguments
in $4-$11 and 64-bit returns; EABI and O64 want their own register sets. Lifting
any of them as O32 drops the arguments past the fourth."""

import shutil
import struct

import pytest

from strider._api import _arch_and_cc_for_elf, _ElfHeader

SRC = "fixtures/out/mips32le/arithmetic.elf"

#: `EF_MIPS_ABI2` plus the O32-shaped arch bits an n32 object carries.
_N32_FLAGS = 0x8000_0027


def _detect(flags, tmp_path):
    dst = tmp_path / "probe.elf"
    shutil.copy(SRC, dst)
    with open(dst, "r+b") as f:
        # ELF32 e_flags is at offset 36.
        f.seek(36)
        f.write(struct.pack("<I", flags))
    return _arch_and_cc_for_elf(_ElfHeader(str(dst)))


def test_the_shipped_mips32_fixtures_are_o32(tmp_path):
    arch, cc = _detect(_ElfHeader(SRC).e_flags, tmp_path)
    assert arch.name() == "mipsle32"
    assert cc.name() == "mips_o32"


@pytest.mark.parametrize(
    ("flags", "abi"),
    [
        (_N32_FLAGS, "n32"),
        (0x7000_0027, "n32"),
        (0x7000_2007, "O64"),
        (0x7000_3007, "EABI32"),
        (0x7000_4007, "EABI64"),
    ],
)
def test_an_elf32_mips_abi_with_no_preset_is_refused(flags, abi, tmp_path):
    with pytest.raises(ValueError, match=f"unsupported MIPS ABI {abi}"):
        _detect(flags, tmp_path)


def test_load_elf_refuses_an_n32_image(tmp_path):
    """The refusal reaches the loader, rather than an O32 lift of n32 code."""
    import strider

    dst = tmp_path / "n32.elf"
    shutil.copy(SRC, dst)
    with open(dst, "r+b") as f:
        f.seek(36)
        f.write(struct.pack("<I", _N32_FLAGS))
    with pytest.raises(ValueError, match="unsupported MIPS ABI n32"):
        strider.lift.load_elf(str(dst))
    # An explicit arch + cc is the documented way past the detector.
    lift = strider.lift.load_elf(
        str(dst),
        arch=strider.sleigh.SleighArch.mipsle32(),
        cc=strider.sleigh.CallingConvention.mips_o32(),
    )
    assert lift.cc.name() == "mips_o32"


def test_mips64_is_still_n64():
    header = _ElfHeader("fixtures/out/mips64le/arithmetic.elf")
    arch, cc = _arch_and_cc_for_elf(header)
    assert arch.name() == "mipsle64"
    assert cc.name() == "mips_n64"
