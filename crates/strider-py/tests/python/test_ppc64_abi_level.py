"""PowerPC64 carries its ABI level in `e_flags`, and the two ABIs differ in
linkage-area size. Picking the wrong one reads stack arguments 16 bytes off,
which collects none at all."""

import shutil
import struct

import strider
from strider._api import _arch_and_cc_for_elf, _ElfHeader

SRC = "fixtures/out/ppc64be/calling_convention.elf"


def _cc_for(flags, tmp_path):
    dst = tmp_path / "probe.elf"
    shutil.copy(SRC, dst)
    with open(dst, "r+b") as f:
        f.seek(48)
        f.write(struct.pack(">I", flags))
    return _arch_and_cc_for_elf(_ElfHeader(str(dst))).__getitem__(1).name()


def test_the_shipped_ppc64_fixtures_are_elfv2():
    for path in (SRC, "fixtures/out/ppc64le/calling_convention.elf"):
        header = _ElfHeader(path)
        assert header.e_flags & 0x3 == 2, path
        assert _arch_and_cc_for_elf(header)[1].name() == "powerpc64_elf_v2"


def test_abi_level_selects_the_convention(tmp_path):
    assert _cc_for(2, tmp_path) == "powerpc64_elf_v2"
    assert _cc_for(1, tmp_path) == "powerpc64_elf_v1"
    # Big-endian images predating the flag are ELFv1.
    assert _cc_for(0, tmp_path) == "powerpc64_elf_v1"
