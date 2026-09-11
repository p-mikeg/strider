from __future__ import annotations

import pathlib
import sys

import strider

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"
OUT_DIR = pathlib.Path("/tmp")

prog = strider.lift.load_elf(str(FIXTURE))
result = prog.analyze(
    "array_sum",
    opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
)
cfg = result.cfg
function = result.function

cfg_dot = OUT_DIR / "array_sum-cfg-neighborhood.dot"
cfg_dot.write_text(cfg.neighborhood_dot(cfg.entry(), depth=2))
print(f"wrote {cfg_dot}")

# pretty=True resolves register names and inlines constants; the default
# draws the same nodes exactly as stored.
ir_dot = OUT_DIR / "array_sum-ir-neighborhood.dot"
ir_dot.write_text(function.neighborhood_dot(function.entry_node(), depth=2, pretty=True))
print(f"wrote {ir_dot}")

print("render a .dot with:  dot -Tsvg <file>.dot -o out.svg")

# The interactive explorer re-centers as you click; it blocks until Ctrl-C.
if "--serve" in sys.argv:
    prog.visualize(function)
