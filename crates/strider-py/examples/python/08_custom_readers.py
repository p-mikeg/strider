from __future__ import annotations

import strider
from strider import template as t
from strider.pattern import Capture, int_add, function_arg, int_const, load, var

CODE_ADDR = 0x0
DATA_ADDR = 0x1000
DATA_VALUE = 0x2A  # ROM value at 0x1000; the load folds to this

# Trailing NOPs so the disassembler's prefetch has bytes past the stream.
CODE = (
    bytes([0x48, 0x03, 0x3C, 0x25, 0x00, 0x10, 0x00, 0x00])  # add rdi, [0x1000]
    + bytes([0x48, 0x89, 0xF8])                              # mov rax, rdi
    + bytes([0xC3])                                          # ret
    + bytes([0x90] * 64)
)


class DictMem(strider.reader.MemReader):
    """Serve code bytes from a dict, counting calls to prove it fired."""

    def __init__(self, regions: dict[int, bytes]) -> None:
        super().__init__()
        self.regions = regions
        self.calls = 0

    def read(self, addr: int, size: int) -> bytes | None:
        self.calls += 1
        for base, blob in self.regions.items():
            if base <= addr and addr + size <= base + len(blob):
                off = addr - base
                return blob[off:off + size]
        return None


class DictRom(strider.reader.ReadOnlyMemory):
    """Serve a read-only word from Python for LoadReadOnly to fold against."""

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
        off = addr - self.base
        return self.blob[off:off + size]


mem = DictMem({CODE_ADDR: CODE})
rom = DictRom(base=DATA_ADDR, blob=DATA_VALUE.to_bytes(8, "little"))

lft = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom)
_cfg, fn, _unresolved = lft.analyze(
    CODE_ADDR,
    strider.sleigh.CallingConvention.x86_64_systemv(),
    opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
)

print(f"DictMem.read (code)   fired {mem.calls} time(s)")
print(f"DictRom.read (rodata) fired {rom.calls} time(s)")
print(f"Load nodes after fold: {len(fn.find_all(load()))}   (LoadReadOnly ate it)")
assert mem.calls > 0 and rom.calls > 0, "a reader never fired; wiring bug"
assert len(fn.find_all(load())) == 0, "load should have folded away"


# ignore_casts=True matches through Truncate / Extend nodes.
print("\n=== capture the folded constant ===")
k = Capture()
matches = fn.find_all(int_add(function_arg(0), var(k)), ignore_casts=True)
print(f"matched {len(matches)} `arg0 + <captured>` shape(s)")
for m in matches:
    print(f"  captured value k = {m.uint(k)}   (== {DATA_VALUE}? {m.uint(k) == DATA_VALUE})")
assert any(m.uint(k) == DATA_VALUE for m in matches), "expected to capture 42"


# Rewrite mutates in place, so clone first to keep fn pristine. The find side
# is a strider.pattern pattern, the replace side a strider.template template.
print("\n=== template rewrite (on a clone): arg0 + 42 -> arg0 + 0 ===")
edited = fn.clone()
x = Capture()
n = edited.rewrite(
    find=int_add(x, int_const(DATA_VALUE)),
    replace=t.int_add(t.var(x), t.int_const(0)),
)
print(f"rewrote {n} site(s)")

lft.optimize(edited)
orig_shapes = len(fn.find_all(int_add(function_arg(0), var(k)), ignore_casts=True))
edited_shapes = len(edited.find_all(int_add(function_arg(0), var(k)), ignore_casts=True))
print(f"`arg0 + <captured>` shapes, original: {orig_shapes}, clone after rewrite: {edited_shapes}")
assert orig_shapes >= 1, "original must be untouched by the clone's rewrite"
print("ok: custom readers -> lift -> capture -> template rewrite on a clone")
