from __future__ import annotations

import strider
from strider import reader, sleigh
from strider.pattern import Capture, int_add, int_const, int_sub, ret

IMAGE_BASE = 0x8000_0000

# Three routines concatenated, each padded so the next starts at a round offset
# and the prefetch past each ret has bytes. x86-64 encodings:
#
#   add:      lea eax,[rdi+rsi] ; ret        8d 04 37             c3
#   sub:      mov eax,edi ; sub eax,esi ; ret 89 f8  29 f0        c3
#   zero_xor: xor eax,eax ; ret              31 c0                c3
def _pad(b: bytes, to: int) -> bytes:
    return b + bytes([0x90] * (to - len(b)))

ADD = _pad(bytes([0x8D, 0x04, 0x37, 0xC3]), 0x20)
SUB = _pad(bytes([0x89, 0xF8, 0x29, 0xF0, 0xC3]), 0x20)
XOR = _pad(bytes([0x31, 0xC0, 0xC3]), 0x20)
IMAGE = ADD + SUB + XOR

mem = reader.BufferReader(IMAGE_BASE, IMAGE)
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)
cc = sleigh.CallingConvention.x86_64_systemv()
opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
)

# A signature per routine. The third's xor eax,eax folds to int_const(0), so
# match the folded result, not the raw xor.
ENTRIES = {
    "add": (0x00, int_add(Capture("a"), Capture("b"))),
    "sub": (0x20, int_sub(Capture("a"), Capture("b"))),
    "zero (xor folded)": (0x40, int_const(0)),
}

print(f"image: {len(IMAGE)} bytes mapped at {IMAGE_BASE:#x}, one BufferReader\n")
for name, (offset, signature) in ENTRIES.items():
    addr = IMAGE_BASE + offset
    _cfg, fn, _unresolved = lft.analyze(addr, cc, opts=opts)
    sig_hits = len(fn.find_all(signature, ignore_casts=True))
    ret_hits = len(fn.find_all(ret()))
    print(
        f"  {name:9} @ {addr:#011x}  {fn.node_count():2d} nodes  "
        f"signature x{sig_hits}  ret x{ret_hits}"
    )
    assert sig_hits >= 1, f"{name}: expected its signature in the IR"
    assert ret_hits >= 1, f"{name}: expected a return"
