from __future__ import annotations

import strider
from strider import reader, sleigh
from strider.pattern import Capture, int_add, ret

# Hand-assembled x86-64:
#     lea eax, [rdi + rsi]     8d 04 37      eax = edi + esi
#     ret                      c3
# Trailing padding for the disassembler's prefetch past ret. Optional here: a
# BufferReader answers the region edge with a short read.
BASE = 0x400000
CODE = bytes([0x8D, 0x04, 0x37, 0xC3]) + bytes(16)

mem = reader.BufferReader(BASE, CODE)
# read gives None for an unmapped address.
head = mem.read(BASE, 3)
assert head is not None and head.hex() == "8d0437"
assert mem.read(BASE - 1, 4) is None

# analyze(addr, cc) does CFG + lift + optimize; allow_code_before_start_addr
# lets the CFG builder walk code below the entry.
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)
_cfg, function, unresolved = lft.analyze(
    BASE,
    sleigh.CallingConvention.x86_64_systemv(),
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    ),
)

print(f"lifted {function.node_count()} nodes from {len(CODE)} raw bytes")
print(f"unresolved indirect branches: {len(unresolved)}")

# The lea is address arithmetic, so it lifts to an int_add of the two arg registers.
a, b = Capture("a"), Capture("b")
adds = function.find_all(int_add(a, b))
print(f"int_add sites (the edi + esi from the lea): {len(adds)}")
assert adds

rets = function.find_all(ret())
print(f"returns: {len(rets)}")
assert rets
