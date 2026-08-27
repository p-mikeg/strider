from __future__ import annotations

import pathlib

import strider
from strider.pattern import int_const, load

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

ARCH = strider.sleigh.SleighArch.x86()
CC = strider.sleigh.CallingConvention.x86_cdecl()
OPTS = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
)

# Real ELF for the code bytes; the ROM comes from Python. `main` reads four
# 32-bit globals from absolute addresses, which is the shape LoadReadOnly folds.
elf = strider.lift.load_elf(str(FIXTURE))
mem = elf.reader()
addr = elf.symbol("main").address


class ProbeRom(strider.reader.ReadOnlyMemory):
    """Answers nothing, records what it was asked for."""

    def __init__(self) -> None:
        super().__init__()
        self.seen: list[tuple[int, int]] = []

    def read(self, addr: int, size: int) -> bytes | None:
        self.seen.append((addr, size))
        return None


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


# A rom that answers None leaves every constant-address load standing, and its
# call log is the address window the next rom has to cover.
probe = ProbeRom()
lft = strider.lift.lifter(ARCH, mem, probe)
_cfg, unfolded, _unresolved = lft.analyze(addr, CC, opts=OPTS)
print(f"probed addresses: {sorted({hex(a) for a, _ in probe.seen})}")
print(f"loads with a rom that answers nothing: {len(unfolded.find_all(load()))}")

base = min(a for a, _ in probe.seen)
end = max(a + n for a, n in probe.seen)
VALUES = (0x11111111, 0x22222222, 0x33333333, 0x44444444)
blob = b"".join(v.to_bytes(4, "little") for v in VALUES)[: end - base]

# This callback is the lifter's whole rom, so it is the only fold source; mem
# feeds instruction fetch. The constants below come from Python, not the ELF.
rom = CallbackRom(base=base, blob=blob)
lft = strider.lift.lifter(ARCH, mem, rom)
_cfg, function, _unresolved = lft.analyze(addr, CC, opts=OPTS)

print(f"\nloads after optimize: {len(function.find_all(load()))}")
print(f"CallbackRom.read invoked {rom.calls} time(s) by LoadReadOnly")
assert rom.calls > 0, "the rom never fired; wiring bug"
assert not function.find_all(load()), "every constant-address load should have folded"

for value in VALUES[: len(blob) // 4]:
    function.find_unique(int_const(value))
    print(f"  folded 0x{value:08x} from the callback, present exactly once")
print("\nok: a Python ReadOnlyMemory is what LoadReadOnly folds against")
