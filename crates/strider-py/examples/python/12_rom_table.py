from __future__ import annotations

import strider
from strider import reader, sleigh
from strider.pattern import int_const, load

CODE_BASE = 0x1000
TABLE_BASE = 0x2000

# Hand-assembled x86-64:
#     mov eax, dword [0x2000]    8b 04 25 00 20 00 00      load table[0]
#     ret                        c3
# Absolute-constant address, so LoadReadOnly can fold it (a register load would stay a Load).
CODE = bytes([0x8B, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00, 0xC3]) + bytes(16)

# Four LE uint32s; the code reads the first.
TABLE = b"".join(v.to_bytes(4, "little") for v in (0x11111111, 0x22222222, 0x33, 0x44))

mem = reader.BufferReader(CODE_BASE, CODE)
rom = reader.BufferReader(TABLE_BASE, TABLE)

# Third arg is the ROM; the Lifter wires it into LoadReadOnly.
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem, rom)
_cfg, fn, _unresolved = lft.analyze(
    CODE_BASE,
    sleigh.CallingConvention.x86_64_systemv(),
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    ),
)

remaining = fn.find_all(load())
print(f"Load nodes after optimize: {len(remaining)}  (0 == folded)")
assert not remaining, "the constant-address load should have folded"

# table[0] == 0x11111111. find_unique asserts exactly one such node.
fn.find_unique(int_const(0x11111111))
print("folded constant 0x11111111 present exactly once")
