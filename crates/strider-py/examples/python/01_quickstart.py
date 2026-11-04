from __future__ import annotations

import pathlib
import tempfile

import strider
from strider.pattern import Capture, int_add, load

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

# load_elf returns an ElfLifter (a Lifter); arch and CC come from the ELF header.
prog = strider.lift.load_elf(str(FIXTURE))
addr = prog.symbol("array_sum").address
print(f"array_sum @ {addr:#x}")

# analyze builds the CFG, lifts, optimizes and resolves indirect branches.
# `cfg.incomplete` lists every address it cannot vouch for (none here).
cfg, function, _unresolved = prog.analyze("array_sum")
print(f"lifted array_sum: {function.node_count()} nodes, incomplete={cfg.incomplete}")

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

# pretty resolves register names, inlines constants and adds virtual nodes; a
# theme name in place of True picks the colours, False is the raw as-stored graph.
out = pathlib.Path(tempfile.gettempdir())
cfg.to_html(str(out / "quickstart-cfg.html"), style="dark_cfg")
function.to_html(str(out / "quickstart-graph.html"), pretty="dark")
print(f"wrote {out / 'quickstart-cfg.html'} and {out / 'quickstart-graph.html'}")
