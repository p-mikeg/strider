"""ppc64 ELFv1 function descriptors.

On that ABI an `STT_FUNC` `st_value` addresses an 8-byte-aligned
{entry, TOC, env} triple in `.opd`, not code, and `st_size` measures that
triple. Following it is what lets `analyze(<symbol name>)` work at all: without
it every ELFv1 function lifts the descriptor bytes, while `_arch_and_cc_for_elf`
was already selecting `powerpc64_elf_v1` off `e_flags` for exactly these images.

The shipped ppc64 fixtures are ELFv2 and carry no `.opd`, so the image is
synthesised here around real ppc64be `add` code lifted out of one of them.
"""

from __future__ import annotations

import struct

import pytest

import strider

#: `add` from `fixtures/out/ppc64be/arithmetic.elf`, ending in `blr`.
ADD_CODE = bytes.fromhex(
    "f881ffe87c641b78e861ffe89081fff49061fff08061fff48081fff07c6322147c6307b44e800020"
)

_BASE = 0x10000000
_EM_PPC64 = 21


def _ppc64_elfv1(path, e_flags):
    """A linked big-endian ppc64 executable whose one function symbol names an
    `.opd` descriptor pointing at `ADD_CODE`.

    One PT_LOAD maps the whole file at `_BASE`, so every virtual address is its
    file offset plus the base.
    """
    text_off, opd_off = 0x100, 0x200
    sym_off, str_off, shstr_off, sh_off = 0x300, 0x400, 0x440, 0x500
    text_addr, opd_addr = _BASE + text_off, _BASE + opd_off

    buf = bytearray(0x700)
    buf[text_off : text_off + len(ADD_CODE)] = ADD_CODE
    # {entry, TOC, env}; only the first doubleword is a code address.
    buf[opd_off : opd_off + 24] = struct.pack(">QQQ", text_addr, 0, 0)

    strtab = b"\0add\0"
    shstrtab = b"\0.text\0.opd\0.symtab\0.strtab\0.shstrtab\0"
    buf[str_off : str_off + len(strtab)] = strtab
    buf[shstr_off : shstr_off + len(shstrtab)] = shstrtab
    # Elf64_Sym: st_name, st_info, st_other, st_shndx, st_value, st_size.
    # STB_GLOBAL | STT_FUNC in section 2 (`.opd`), sized as the descriptor.
    buf[sym_off : sym_off + 48] = bytes(24) + struct.pack(
        ">IBBHQQ", strtab.index(b"add"), 0x12, 0, 2, opd_addr, 24
    )

    def shdr(name, sh_type, flags, addr, off, size, link=0, info=0, align=1, ent=0):
        return struct.pack(
            ">IIQQQQIIQQ",
            shstrtab.index(name) if name else 0,
            sh_type,
            flags,
            addr,
            off,
            size,
            link,
            info,
            align,
            ent,
        )

    shdrs = b"".join(
        [
            shdr(b"", 0, 0, 0, 0, 0),
            shdr(b".text\0", 1, 0x6, text_addr, text_off, len(ADD_CODE), align=4),
            shdr(b".opd\0", 1, 0x3, opd_addr, opd_off, 24, align=8),
            shdr(b".symtab\0", 2, 0, 0, sym_off, 48, link=4, info=1, align=8, ent=24),
            shdr(b".strtab\0", 3, 0, 0, str_off, len(strtab)),
            shdr(b".shstrtab\0", 3, 0, 0, shstr_off, len(shstrtab)),
        ]
    )
    buf[sh_off : sh_off + len(shdrs)] = shdrs

    # PT_LOAD, R+X, mapping the whole file.
    buf[0x40:0x78] = struct.pack(
        ">IIQQQQQQ", 1, 0x5, 0, _BASE, _BASE, len(buf), len(buf), 0x1000
    )
    buf[0:64] = struct.pack(
        ">4sBBBBB7xHHIQQQIHHHHHH",
        b"\x7fELF",
        2,  # ELFCLASS64
        2,  # ELFDATA2MSB
        1,
        0,
        0,
        2,  # ET_EXEC
        _EM_PPC64,
        1,
        opd_addr,  # e_entry: a descriptor on this ABI, exactly as `st_value` is
        0x40,
        sh_off,
        e_flags,
        64,
        56,
        1,
        64,
        6,
        5,
    )
    path.write_bytes(bytes(buf))
    return str(path), text_addr, opd_addr


@pytest.fixture
def elfv1(tmp_path):
    return _ppc64_elfv1(tmp_path / "opd.elf", e_flags=1)


def test_the_image_is_read_as_elfv1(elfv1):
    path, _text, _opd = elfv1
    from strider._api import _ElfHeader, _arch_and_cc_for_elf

    _arch, cc = _arch_and_cc_for_elf(_ElfHeader(path))
    assert cc.name() == "powerpc64_elf_v1"


def test_a_function_symbol_resolves_to_code_not_to_its_descriptor(elfv1):
    path, text_addr, opd_addr = elfv1
    sym = strider.lift.load_elf(path).symbol("add")
    assert sym.address == text_addr, f"{sym.address:#x} is the descriptor {opd_addr:#x}"
    # `st_size` measured the 24-byte triple, so it says nothing about the code
    # and must not go on to bound the lift.
    assert sym.size is None


def test_analyze_by_symbol_name_lifts_the_function(elfv1):
    path, text_addr, _opd = elfv1
    lift = strider.lift.load_elf(path)
    by_name = lift.analyze("add")
    by_address = lift.analyze(text_addr)
    # The descriptor bytes are eight zero doublewords, which do not decode as
    # this function; lifting them is what the fix stops.
    assert by_name.cfg.region_at(text_addr) == by_name.cfg.entry()
    assert by_name.function.node_count() == by_address.function.node_count()


def test_the_elf_entry_point_is_a_descriptor_too(elfv1):
    """`e_entry` carries a descriptor on this ABI just as `st_value` does, so
    `entry_point()` has to go through the same table."""
    path, text_addr, _opd = elfv1
    assert strider.lift.load_elf(path).entry_point() == text_addr


def test_elfv2_leaves_addresses_alone(tmp_path):
    """`OpdTable` is `None` on ELFv2, so nothing is followed even though the
    synthesised image still carries a section named `.opd`."""
    path, _text, opd_addr = _ppc64_elfv1(tmp_path / "v2.elf", e_flags=2)
    lift = strider.lift.load_elf(path)
    sym = lift.symbol("add")
    assert (sym.address, sym.size) == (opd_addr, 24)
    assert lift.entry_point() == opd_addr
