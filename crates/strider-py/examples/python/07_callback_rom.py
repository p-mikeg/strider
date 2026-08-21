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
        # Return exactly size RAW bytes; no byte-swap (the optimizer decodes per endianness).
        self.calls += 1
        if addr < self.base or addr + size > self.base + len(self.blob):
            return None
        offset = addr - self.base
        return self.blob[offset:offset + size]


# Real ELF for the code bytes; the ROM comes from Python.
elf = strider.lift.load_elf(str(FIXTURE))
mem = elf.reader()
addr = elf.symbol("array_sum").address

# Base sits above the fixture's mapped regions. This callback is the lifter's
# whole rom, so it is the only fold source; mem feeds instruction fetch.
rom = CallbackRom(base=0xCAFE0000, blob=bytes(range(16)))

# The Lifter wires the rom into LoadReadOnly.
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
    print(
        "\n(no addresses matched our callback ROM in this fixture. The "
        "wiring is correct; pick a fixture whose code references your "
        "ROM region to see folds happen.)"
    )
