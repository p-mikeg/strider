"""Quickstart: load an ELF, lift a function, query it, render it.

Run from the workspace root:
    python crates/strider-py/examples/python/01_quickstart.py

Writes /tmp/quickstart-cfg.html and /tmp/quickstart-graph.html.
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import add, load

# `load_elf` returns an `ElfLifter`, which is itself a `Lifter`. It picks the
# arch and calling convention off the ELF header, wires the code and ROM
# readers, and answers `symbol()` / `symbols()` / `entry_point()`.
WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
addr = prog.symbol("array_sum")
print(f"array_sum @ {addr:#x}")

# `analyze(name_or_addr)` does CFG build, IR lift, optimization and the
# indirect-branch fixed-point loop in one call. The returned `cfg` is the
# final resolved CFG that `function` was lifted from, so rendering it later
# needs no rebuild.
cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)
print(
    f"lifted array_sum: {function.node_count()} nodes, "
    f"{len(unresolved)} unresolved indirect branches"
)

# Bare `load()` matches every memory-load site. Narrow it by composing inside
# `addr=...` once you know the shape you are hunting for.
hits = function.find_all(load(), ignore_casts=True)
print(f"found {len(hits)} memory-load sites in array_sum")

# String captures are auto-interned per pattern, so "base" and "off" each
# become a Capture scoped to this one pattern.
narrow = function.find_all(
    load(addr=add("base", "off")),
    ignore_casts=True,
)
print(f"found {len(narrow)} loads of the form `base + offset`")
for hit in narrow:
    off_val = hit.const_uint("off")
    print(f"  offset = {off_val if off_val is not None else '<symbolic>'}")

# The HTML is self-contained; open it in any browser. `pretty=True` resolves
# register names, inlines constants and adds virtual nodes; omit it for the
# raw as-stored graph.
cfg.to_html("/tmp/quickstart-cfg.html", style="dark_cfg")
function.to_html("/tmp/quickstart-graph.html", pretty=True, style="dark")
print("wrote /tmp/quickstart-cfg.html and /tmp/quickstart-graph.html")
