from __future__ import annotations

import strider
from strider.pattern import ret


class DictMem(strider.reader.MemReader):
    """Serve bytes from a dict of base address -> blob, counting calls."""

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


# nop ; nop ; ret at 0x1000. Trailing NOP padding: the disassembler prefetches
# past the instruction stream, so the reader must serve those addresses.
INSTR = bytes([0x90, 0x90, 0xc3]) + bytes([0x90] * 64)
mem = DictMem({0x1000: INSTR})

lft = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem)
_cfg, function, _unresolved = lft.analyze(0x1000, strider.sleigh.CallingConvention.x86_cdecl())

hits = function.find_all(ret())
print(f"lifted graph contains {len(hits)} Return node(s)")
assert len(hits) >= 1, "expected at least one Return"

print(f"DictMem.read was called {mem.calls} time(s) by Rust")
assert mem.calls > 0, "Rust never invoked the Python reader; wiring bug"

print("ok: Python-implemented MemReader drove a real lift end-to-end")
