"""02 — Custom Python `MemReader`: lift bytes the binary reader can't reach.

The fast path is `BufferReader` (data lives entirely in Rust). The flexible
path is subclassing `strider.MemReader` so your `read(addr, size)` runs
in Python — useful for lazy formats, paged-from-disk firmware, decrypted
ROM dumps, or any source the standard ELF reader doesn't cover.

This example serves a single hand-assembled x86 instruction stream
(two NOPs and a RET) from a Python dict, lifts it, and confirms the
resulting IR contains a `Return` node — and that Python's `read` was
actually called from Rust.

Run from the workspace root:
    python crates/strider-py/examples/python/02_python_reader.py

The performance contract: every byte fetched during sleigh disassembly
takes one GIL acquire + one Python method call. Fine for tiny snippets
or unusual sources; use `BufferReader` when you have the bytes already.
"""

from __future__ import annotations

import strider
from strider.pattern import ret


class DictMem(strider.MemReader):
    """Serve bytes from a Python dict mapping address → bytes object.

    Tracks how often Rust called us, so we can prove the callback
    really fired (and not just satisfied a Rust-side cache hit).
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


# Two NOPs (0x90) followed by a RET (0xc3) at virtual address 0x1000.
# Pad with extra NOPs so sleigh's prefetch window has bytes to read
# past the actual instruction stream — readers must serve any address
# the disassembler asks for, even speculatively.
INSTR = bytes([0x90, 0x90, 0xc3]) + bytes([0x90] * 64)
mem = DictMem({0x1000: INSTR})

lft = strider.lifter(strider.SleighArch.x86(), mem)
_cfg, function, _unresolved = lft.analyze(0x1000, strider.CallingConvention.x86_cdecl())

# Confirm the IR has at least one return.
hits = function.find_all(ret())
print(f"lifted graph contains {len(hits)} Return node(s)")
assert len(hits) >= 1, "expected at least one Return"

# Confirm the Python callback actually fired.
print(f"DictMem.read was called {mem.calls} time(s) by Rust")
assert mem.calls > 0, "Rust never invoked the Python reader — wiring bug"

print("ok — Python-implemented MemReader drove a real lift end-to-end")
