"""Subclass `strider.reader.MemReader` to serve code bytes from Python.

Useful for lazy formats, paged-from-disk firmware, decrypted ROM dumps, or
anything the ELF reader doesn't cover. Every byte fetched during disassembly
costs one Python call, so prefer `BufferReader` when you already hold the
bytes.

Run from the workspace root:
    python crates/strider-py/examples/python/02_python_reader.py
"""

from __future__ import annotations

import strider
from strider.pattern import ret


class DictMem(strider.reader.MemReader):
    """Serve bytes from a dict of base address to blob.

    Counts calls so the example can prove the callback really fired.
    """

    def __init__(self, regions: dict[int, bytes]) -> None:
        super().__init__()
        self.regions = regions
        self.calls = 0

    def read(self, addr: int, size: int) -> bytes | None:
        self.calls += 1
        for base, blob in self.regions.items():
            if base <= addr < base + len(blob):
                offset = addr - base
                end = offset + size
                if end <= len(blob):
                    return blob[offset:end]
        return None


# nop ; nop ; ret, at virtual address 0x1000. The trailing NOP padding matters:
# the disassembler prefetches past the real instruction stream, and a reader
# must serve whatever address it asks for, even speculatively.
INSTR = bytes([0x90, 0x90, 0xc3]) + bytes([0x90] * 64)
mem = DictMem({0x1000: INSTR})

lft = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem)
_cfg, function, _unresolved = lft.analyze(0x1000, strider.sleigh.CallingConvention.x86_cdecl())

hits = function.find_all(ret())
print(f"lifted graph contains {len(hits)} Return node(s)")
assert len(hits) >= 1, "expected at least one Return"

print(f"DictMem.read was called {mem.calls} time(s) by Rust")
assert mem.calls > 0, "Rust never invoked the Python reader — wiring bug"

print("ok — Python-implemented MemReader drove a real lift end-to-end")
