"""An RWX mapping is writable at runtime, so it must not appear in `.rom`.

`ReadOnlyMemory` backs constant folding: a byte exposed there is assumed
immutable, so a store-then-reload of an RWX global would fold to the
file-initial value.
"""

import struct

import pytest

import strider

RWX_ADDR = 0x1000
RWX_BYTES = b"\xde\xad\xbe\xef\x01\x02\x03\x04"


def _rwx_elf() -> bytes:
    """ELF64 x86-64 with a single RWX PT_LOAD."""
    ehsize, phentsize, data_off = 64, 56, 0x1000
    ehdr = (
        b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8
        + struct.pack(
            "<HHIQQQIHHHHHH",
            2,  # ET_EXEC
            62,  # EM_X86_64
            1,
            RWX_ADDR,  # e_entry
            ehsize,  # e_phoff
            0,  # e_shoff
            0,
            ehsize,
            phentsize,
            1,  # e_phnum
            64,
            0,  # e_shnum
            0,
        )
    )
    phdr = struct.pack(
        "<IIQQQQQQ",
        1,  # PT_LOAD
        7,  # PF_R | PF_W | PF_X
        data_off,
        RWX_ADDR,
        RWX_ADDR,
        len(RWX_BYTES),
        len(RWX_BYTES),
        0x1000,
    )
    body = ehdr + phdr
    return body + b"\x00" * (data_off - len(body)) + RWX_BYTES


@pytest.fixture
def rwx_elf(tmp_path):
    p = tmp_path / "rwx.elf"
    p.write_bytes(_rwx_elf())
    return str(p)


def test_rwx_bytes_are_fetchable(rwx_elf):
    """The fetch image keeps RWX so single-segment firmware still decodes."""
    loaded = strider.lift.load_elf(rwx_elf)
    assert loaded.reader().read(RWX_ADDR, len(RWX_BYTES)) == RWX_BYTES


@pytest.mark.parametrize("apply_relocations", [True, False])
@pytest.mark.parametrize("from_segments", [True, False])
def test_rwx_bytes_are_not_exposed_as_rom(rwx_elf, from_segments, apply_relocations):
    loaded = strider.lift.load_elf(
        rwx_elf, from_segments=from_segments, apply_relocations=apply_relocations
    )
    rom = loaded.rom()
    assert rom is None or rom.read(RWX_ADDR, len(RWX_BYTES)) is None
