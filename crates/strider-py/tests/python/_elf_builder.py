"""A minimal little-endian x86-64 ELF writer for tests that need a shape no
toolchain emits on purpose: overlapping PT_LOADs, a symbol at address 0, an
undefined symbol carrying a PLT address."""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Optional

PF_X, PF_W, PF_R = 1, 2, 4
SHF_WRITE, SHF_ALLOC, SHF_EXECINSTR = 1, 2, 4
STT_NOTYPE, STT_OBJECT, STT_FUNC = 0, 1, 2
STB_LOCAL, STB_GLOBAL = 0, 1
ET_REL, ET_EXEC = 1, 2


@dataclass
class Section:
    name: str
    addr: int
    data: bytes
    flags: int = SHF_ALLOC | SHF_EXECINSTR


@dataclass
class Sym:
    name: str
    value: int
    #: Index into the section list, or `None` for `SHN_UNDEF`.
    section: Optional[int]
    size: int = 0
    kind: int = STT_FUNC


@dataclass
class Elf:
    e_type: int = ET_EXEC
    #: `(p_flags, p_vaddr, section index whose bytes it maps)`.
    loads: list[tuple[int, int, int]] = field(default_factory=list)
    sections: list[Section] = field(default_factory=list)
    symbols: list[Sym] = field(default_factory=list)

    def write(self, path) -> None:
        ehsize, phentsize, shentsize = 64, 56, 64
        offset = ehsize + phentsize * len(self.loads)
        blobs = b""
        sec_offsets = []
        for sec in self.sections:
            pad = (-(offset + len(blobs))) % 16
            blobs += b"\0" * pad
            sec_offsets.append(offset + len(blobs))
            blobs += sec.data

        strtab = b"\0"
        symtab = b"\0" * 24
        for sym in self.symbols:
            name_off = len(strtab)
            strtab += sym.name.encode() + b"\0"
            shndx = 0 if sym.section is None else sym.section + 1
            info = (STB_GLOBAL << 4) | sym.kind
            symtab += struct.pack("<IBBHQQ", name_off, info, 0, shndx, sym.value, sym.size)

        names = [s.name for s in self.sections] + [".symtab", ".strtab", ".shstrtab"]
        shstrtab = b"\0"
        name_offs = []
        for n in names:
            name_offs.append(len(shstrtab))
            shstrtab += n.encode() + b"\0"

        tail_start = offset + len(blobs)
        symtab_off = tail_start
        strtab_off = symtab_off + len(symtab)
        shstrtab_off = strtab_off + len(strtab)
        shoff = shstrtab_off + len(shstrtab)
        shoff += (-shoff) % 8
        nsec = len(self.sections)
        shnum = nsec + 4

        out = b"\x7fELF" + bytes([2, 1, 1, 0]) + bytes(8)
        out += struct.pack(
            "<HHIQQQIHHHHHH",
            self.e_type, 62, 1, 0, ehsize if self.loads else 0, shoff, 0,
            ehsize, phentsize, len(self.loads), shentsize, shnum, shnum - 1,
        )
        for flags, vaddr, sec_index in self.loads:
            size = len(self.sections[sec_index].data)
            out += struct.pack(
                "<IIQQQQQQ", 1, flags, sec_offsets[sec_index], vaddr, vaddr, size, size, 1
            )
        out += blobs + symtab + strtab + shstrtab
        out += b"\0" * (shoff - len(out))
        out += bytes(shentsize)
        for i, sec in enumerate(self.sections):
            out += struct.pack(
                "<IIQQQQIIQQ", name_offs[i], 1, sec.flags, sec.addr, sec_offsets[i],
                len(sec.data), 0, 0, 1, 0,
            )
        out += struct.pack(
            "<IIQQQQIIQQ", name_offs[nsec], 2, 0, 0, symtab_off, len(symtab),
            nsec + 2, 1, 8, 24,
        )
        out += struct.pack(
            "<IIQQQQIIQQ", name_offs[nsec + 1], 3, 0, 0, strtab_off, len(strtab), 0, 0, 1, 0
        )
        out += struct.pack(
            "<IIQQQQIIQQ", name_offs[nsec + 2], 3, 0, 0, shstrtab_off, len(shstrtab),
            0, 0, 1, 0,
        )
        with open(path, "wb") as f:
            f.write(out)
