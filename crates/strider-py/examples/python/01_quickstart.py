from __future__ import annotations

import pathlib

import strider
from strider.pattern import Capture, int_add, load

# load_elf returns an ElfLifter (a Lifter); arch and CC come from the ELF header.
WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
addr = prog.symbol("array_sum").address
print(f"array_sum @ {addr:#x}")

# analyze does CFG build, lift, optimize, and the indirect-branch fixed-point in
# one call. The returned cfg is the final resolved one.
cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)
print(
    f"lifted array_sum: {function.node_count()} nodes, "
    f"{len(unresolved)} unresolved indirect branches"
)

# Bare load() matches every load site; narrow with addr=...
hits = function.find_all(load(), ignore_casts=True)
print(f"found {len(hits)} memory-load sites in array_sum")

# int_add is commutative: a base+offset load matches under both operand orders,
# so find_all counts it twice (example 06 shows .ordered() to opt out).
base, off = Capture("base"), Capture("off")
narrow = function.find_all(
    load(addr=int_add(base, off)),
    ignore_casts=True,
)
print(f"found {len(narrow)} `base + offset` binding rows (both operand orders)")
for hit in narrow:
    # off may be non-constant; uint_opt returns None rather than raising.
    off_val = hit.uint_opt(off)
    print(f"  offset = {off_val if off_val is not None else '<symbolic>'}")

# pretty=True resolves register names, inlines constants, adds virtual nodes;
# omit for the raw as-stored graph.
cfg.to_html("/tmp/quickstart-cfg.html", style="dark_cfg")
function.to_html("/tmp/quickstart-graph.html", pretty="dark")
print("wrote /tmp/quickstart-cfg.html and /tmp/quickstart-graph.html")
