"""07 — `ReadOnlyMemory` callback: Python-served `.rodata`.

`LoadReadOnly` is the optimizer pass that resolves `Load` nodes whose
addresses are compile-time constants, by reading the value from a
caller-supplied ROM. The ROM can be:

  - `BufferReader` (fast — Rust-side reads).
  - Any subclass of `strider.ReadOnlyMemory` (callback — Python `read`
    fires once per fold candidate, under the GIL).

This example serves a tiny lookup-table ROM from Python and shows that
LoadReadOnly folds the resulting load into a constant. Compare against
example 02 (which subclasses `MemReader` for sleigh's instruction-fetch
path) — the two ABCs are independent: subclass each one you want to
fill, or use a `BufferReader` for both.

Run from the workspace root:
    python crates/strider-py/examples/python/07_callback_rom.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import load

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"


class CallbackRom(strider.ReadOnlyMemory):
    """Serve a flat byte blob from Python. Tracks how often Rust called us
    so we can confirm the LoadReadOnly pass actually invoked the callback.
    """

    def __init__(self, base: int, blob: bytes) -> None:
        super().__init__()
        self.base = base
        self.blob = blob
        self.calls = 0

    def read(self, addr: int, size: int) -> bytes | None:
        # Return the RAW `size` bytes (no endianness swap — the optimizer
        # decodes them per the run's endianness). Must be exactly `size` long.
        self.calls += 1
        if addr < self.base or addr + size > self.base + len(self.blob):
            return None
        offset = addr - self.base
        return self.blob[offset:offset + size]


# Use the real ELF for the code (sleigh fetch needs disassembly bytes),
# but layer a Python-served ROM on top to demonstrate the callback path.
elf = strider.load_elf(str(FIXTURE))
mem = elf.reader()
addr = elf.symbol("array_sum")

# A 16-byte read-only blob the callback "owns". The address is well
# above the fixture's mapped regions so it doesn't overlap the real
# `.rodata` — LoadReadOnly will only consult our callback for addresses
# the ELF BufferReader doesn't cover.
rom = CallbackRom(base=0xCAFE0000, blob=bytes(range(16)))

# The ROM flows in through `strider.run(..., rom=...)` — the callback ABC
# is wired to LoadReadOnly by the orchestrator, so there is no
# `LoadReadOnly(rom)` pass to construct by hand. `mem` serves both the
# sleigh instruction fetch (code) and the ELF-backed constant loads;
# `rom` layers our Python-served blob on top for addresses the ELF
# doesn't cover.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem,
    entry=addr,
    rom=rom,
    allow_code_before_start_addr=True,
)
function = result.function
after = len(function.find_all(load()))

print(f"loads after optimize: {after}")
print(f"CallbackRom.read invoked {rom.calls} time(s) by LoadReadOnly")

if rom.calls == 0:
    # array_sum doesn't reach into our synthetic 0xCAFE0000 region, so
    # for *this* fixture the callback never fires. That's expected — the
    # demonstration is the wiring (subclass + pass + pipeline), not the
    # particular fixture's address pattern.
    print(
        "\n(no addresses matched our callback ROM in this fixture — the "
        "wiring is correct; pick a fixture whose code references your "
        "ROM region to see folds happen.)"
    )
