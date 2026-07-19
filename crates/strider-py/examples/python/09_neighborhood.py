"""Explore a function's neighborhood: the CFG regions or IR nodes within a
few hops of a chosen center, instead of the whole graph at once.

`neighborhood_dot(center, depth, ...)` returns Graphviz DOT for just that
local view. `Cfg.entry()` and `Function.entry_node()` give a natural center.
For an interactive version that re-centers on click, `Lifter.visualize`
serves a browser explorer (it blocks, so this example runs it only with
`--serve`).

Run from the workspace root:
    python crates/strider-py/examples/python/09_neighborhood.py
    python crates/strider-py/examples/python/09_neighborhood.py --serve
"""

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

# CFG neighborhood: the regions within 2 hops of the entry region.
cfg_dot = OUT_DIR / "array_sum-cfg-neighborhood.dot"
cfg_dot.write_text(cfg.neighborhood_dot(cfg.entry(), depth=2))
print(f"wrote {cfg_dot}")

# IR neighborhood: the nodes within 2 hops of the entry node. Rendered
# through the lifter so register names resolve; `Function.neighborhood_dot`
# is the same view without a Sleigh.
ir_dot = OUT_DIR / "array_sum-ir-neighborhood.dot"
ir_dot.write_text(prog.neighborhood_dot(function, function.entry_node(), depth=2))
print(f"wrote {ir_dot}")

print("render a .dot with:  dot -Tsvg <file>.dot -o out.svg")

# The interactive explorer serves the same neighborhood view in a browser
# and re-centers as you click. It blocks until Ctrl-C.
if "--serve" in sys.argv:
    prog.visualize(function)
