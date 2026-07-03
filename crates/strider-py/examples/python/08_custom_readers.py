"""08 — Custom `MemReader` + `ReadOnlyMemory`, then capture + rewrite.

Combines the two callback ABCs (see examples 02 and 07) into one lift,
then shows two things you do with the resulting IR:

  - `DictMem(strider.MemReader)` serves the *code* bytes sleigh
    disassembles (the instruction-fetch path).
  - `DictRom(strider.ReadOnlyMemory)` serves the *data* the optimizer's
    `LoadReadOnly` pass folds constant-address loads against.

The code at 0x0 computes `arg0 + *(uint64*)0x1000` and returns it:

    48 03 3c 25 00 10 00 00    add rdi, qword [0x1000]
    48 89 f8                   mov rax, rdi
    c3                         ret

`Lifter.analyze(...)` (built via `strider.lifter(arch, mem, rom=rom)`)
runs the full default optimizer pipeline. `LoadReadOnly` folds the load
into `IntConst(42)`, leaving `Add(arg0, 42)` in the graph. We then:

  1. Query it with a capturing pattern and read the captured constant
     back as a Python int (`Match.uint`).
  2. Template-rewrite `arg0 + 42 → arg0 + 0` and re-optimize via
     `Lifter.optimize`, so ConstantFold collapses the add away entirely.

Run from the workspace root:
    python crates/strider-py/examples/python/08_custom_readers.py
"""

from __future__ import annotations

import strider
from strider.pattern import Capture, add, function_arg, int_const, load, var

CODE_ADDR = 0x0
DATA_ADDR = 0x1000
DATA_VALUE = 0x2A  # what the ROM serves at 0x1000 → what the load folds to

# add rdi, [0x1000] ; mov rax, rdi ; ret  — padded with NOPs so sleigh's
# prefetch window always has bytes to read past the real instruction stream.
CODE = (
    bytes([0x48, 0x03, 0x3C, 0x25, 0x00, 0x10, 0x00, 0x00])  # add rdi, [0x1000]
    + bytes([0x48, 0x89, 0xF8])                              # mov rax, rdi
    + bytes([0xC3])                                          # ret
    + bytes([0x90] * 64)
)


class DictMem(strider.MemReader):
    """Serve code bytes from a dict; count callbacks to prove it fired."""

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


class DictRom(strider.ReadOnlyMemory):
    """Serve a read-only word from Python for LoadReadOnly to fold against."""

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
        off = addr - self.base
        return self.blob[off:off + size]


mem = DictMem({CODE_ADDR: CODE})
rom = DictRom(base=DATA_ADDR, blob=DATA_VALUE.to_bytes(8, "little"))

lft = strider.lifter(strider.SleighArch.x86_64(), mem, rom)
_cfg, fn, _unresolved = lft.analyze(
    CODE_ADDR,
    strider.CallingConvention.x86_64_systemv(),
    opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True)),
)

print(f"DictMem.read (code)   fired {mem.calls} time(s)")
print(f"DictRom.read (rodata) fired {rom.calls} time(s)")
print(f"Load nodes after fold: {len(fn.find_all(load()))}   (LoadReadOnly ate it)")
assert mem.calls > 0 and rom.calls > 0, "a reader never fired — wiring bug"
assert len(fn.find_all(load())) == 0, "load should have folded away"


# --- 1. Query a capture and read the matched value back ------------------
# A bare `Capture` in the operand slot binds whatever value sits there;
# `Match.uint` reads it back as a Python int (None if it's not a constant).
# `ignore_casts=True` sees through the register-width truncate/extend nodes
# the lifter inserts around rdi.
print("\n=== capture the folded constant ===")
k = Capture()
matches = fn.find_all(add(function_arg(0), var(k)), ignore_casts=True)
print(f"matched {len(matches)} `arg0 + <captured>` shape(s)")
for m in matches:
    print(f"  captured value k = {m.uint(k)}   (== {DATA_VALUE}? {m.uint(k) == DATA_VALUE})")
assert any(m.uint(k) == DATA_VALUE for m in matches), "expected to capture 42"


# --- 2. Template rewrite on a CLONE: arg0 + 42 → arg0 + 0 -----------------
# Rewrite mutates in place, so clone first to keep `fn` pristine. The clone
# owns a fresh graph + side-tables. Share a Capture between find and replace
# so both sides agree on `x`; the replace side is a template DAG built from
# the same pattern builders.
print("\n=== template rewrite (on a clone): arg0 + 42 → arg0 + 0 ===")
edited = fn.clone()
x = Capture()
n = edited.rewrite(find=add(x, int_const(DATA_VALUE)), replace=add(x, int_const(0)))
print(f"rewrote {n} site(s)")

# Re-optimize the clone so ConstantFold collapses `x + 0 → x`.
lft.optimize(edited)
orig_shapes = len(fn.find_all(add(function_arg(0), var(k)), ignore_casts=True))
edited_shapes = len(edited.find_all(add(function_arg(0), var(k)), ignore_casts=True))
print(f"`arg0 + <captured>` shapes — original: {orig_shapes}, clone after rewrite: {edited_shapes}")
assert orig_shapes >= 1, "original must be untouched by the clone's rewrite"
print("ok — custom readers → lift → capture → template rewrite on a clone")
