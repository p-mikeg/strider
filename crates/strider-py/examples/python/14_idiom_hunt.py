from __future__ import annotations

import pathlib

import strider
from strider.pattern import (
    Capture,
    PatLike,
    int_add,
    int_and,
    int_const,
    int_mul,
    int_shl,
    int_sub,
    load,
    store,
    var,
)

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
_cfg, fn, _unresolved = prog.analyze(
    "array_sum",
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    ),
)


def idioms() -> dict[str, PatLike]:
    """Fresh captures per call, so each returned pattern is independent."""
    x, k = Capture("x"), Capture("k")
    base, off = Capture("base"), Capture("off")
    return {
        # The pipeline folds xor(r, r) to a constant, so the idiom is spotted
        # by its result rather than by the xor.
        "zero via xor r,r (folded)": int_const(0),
        "shift-left (x << k)": int_shl(var(x), var(k)),   # often mul-by-2^k
        "bitmask (x & k)": int_and(var(x), var(k)),
        "indexed load": load(addr=int_add(base, off)),
        "integer multiply": int_mul(var(x), var(k)),
        "subtract": int_sub(var(x), var(k)),
        "store": store(),
    }


print(f"scanning array_sum ({fn.node_count()} nodes) for idioms:\n")
found: list[tuple[str, int]] = []
for name, pat in idioms().items():
    n = len(fn.find_all(pat, ignore_casts=True))
    flag = "*" if n else " "
    print(f"  [{flag}] {name:22} {n:3d}")
    if n:
        found.append((name, n))

print(f"\n{len(found)} of {len(idioms())} idioms present in this function")

# Swap the function and the table reports a different fingerprint.
top = max(found, key=lambda kv: kv[1]) if found else None
if top:
    print(f"most common idiom here: {top[0]!r} ({top[1]} sites)")
