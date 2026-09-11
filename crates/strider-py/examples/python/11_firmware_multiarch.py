from __future__ import annotations

import strider
from strider import reader, sleigh
from strider.pattern import Capture, int_add

# label -> (arch, calling convention, base address, code bytes)
#   x86-64   endbr64 ; lea eax,[rdi+rsi] ; ret
#   aarch64  add w0, w0, w1 ; ret
#   mips32le jr ra ; addu v0, a0, a1   (add in the branch delay slot)
CASES = {
    "x86-64": (
        sleigh.SleighArch.x86_64(),
        sleigh.CallingConvention.x86_64_systemv(),
        0x401290,
        bytes.fromhex("f30f1efa8d0437c3"),
    ),
    "aarch64": (
        sleigh.SleighArch.aarch64(),
        sleigh.CallingConvention.aarch64_aapcs64(),
        0x8E0,
        bytes.fromhex("0000010bc0035fd6"),
    ),
    "mips32le": (
        sleigh.SleighArch.mipsle32(),
        sleigh.CallingConvention.mips_o32(),
        0x400840,
        bytes.fromhex("0800e00321108500"),
    ),
}

opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
)

# int_add is an IR node kind, not an ISA mnemonic, so one pattern serves every arch.
a, b = Capture("a"), Capture("b")
pat = int_add(a, b)

for label, (arch, cc, base, code) in CASES.items():
    mem = reader.BufferReader(base, code + bytes(64))  # pad past the stream
    lft = strider.lift.lifter(arch, mem)
    _cfg, fn, _unresolved = lft.analyze(base, cc, opts=opts)

    hits = fn.find_all(pat, ignore_casts=True)
    print(
        f"{label:9} {len(code):2d} bytes -> {fn.node_count():2d} IR nodes, "
        f"{len(hits)} int_add match(es)"
    )
    assert hits, f"{label}: the add should survive as an int_add"
