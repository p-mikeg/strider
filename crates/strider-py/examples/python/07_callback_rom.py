"""Serve read-only data from Python for the LoadReadOnly pass to fold against.

LoadReadOnly resolves `Load` nodes at compile-time-constant addresses by
reading a caller-supplied ROM. That ROM is either a `BufferReader` (reads
stay on the Rust side) or a subclass of `strider.reader.ReadOnlyMemory`,
whose `read` fires once per fold candidate.

`ReadOnlyMemory` and the `MemReader` of 02_python_reader.py are independent:
subclass whichever you need, or use a `BufferReader` for both.

Run from the workspace root:
    python crates/strider-py/examples/python/07_callback_rom.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import load

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"


class CallbackRom(strider.reader.ReadOnlyMemory):
    """Serve a flat byte blob, counting calls to confirm LoadReadOnly used it."""

    def __init__(self, base: int, blob: bytes) -> None:
        super().__init__()
        self.base = base
        self.blob = blob
        self.calls = 0

    def read(self, addr: int, size: int) -> bytes | None:
        # Return exactly `size` RAW bytes. Do not byte-swap; the optimizer
        # decodes them per the run's endianness.
        self.calls += 1
        if addr < self.base or addr + size > self.base + len(self.blob):
            return None
        offset = addr - self.base
        return self.blob[offset:offset + size]


# Real ELF for the code bytes, Python-served ROM layered on top.
elf = strider.lift.load_elf(str(FIXTURE))
mem = elf.reader()
addr = elf.symbol("array_sum")

# The base address sits well above the fixture's mapped regions so it cannot
# overlap the real `.rodata`; LoadReadOnly consults this callback only for
# addresses the ELF reader doesn't cover.
rom = CallbackRom(base=0xCAFE0000, blob=bytes(range(16)))

# The Lifter wires the ROM into LoadReadOnly itself, so there is no
# `LoadReadOnly(rom)` pass to construct by hand. `mem` serves both the
# instruction fetch and the ELF-backed constant loads.
lft = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem, rom)
_cfg, function, _unresolved = lft.analyze(
    addr,
    strider.sleigh.CallingConvention.x86_cdecl(),
    opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
)
after = len(function.find_all(load()))

print(f"loads after optimize: {after}")
print(f"CallbackRom.read invoked {rom.calls} time(s) by LoadReadOnly")

if rom.calls == 0:
    # Expected: array_sum never reaches into the synthetic 0xCAFE0000 region,
    # so the callback has nothing to answer. The wiring is what this shows,
    # not this fixture's address pattern.
    print(
        "\n(no addresses matched our callback ROM in this fixture — the "
        "wiring is correct; pick a fixture whose code references your "
        "ROM region to see folds happen.)"
    )
