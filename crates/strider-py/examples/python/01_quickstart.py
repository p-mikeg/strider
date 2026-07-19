"""01 — Quickstart: load ELF, lift, query, visualize.

The five-line minimum to get from a binary on disk to a queryable
sea-of-nodes IR graph. Every later example builds on this.

Run from the workspace root:
    python crates/strider-py/examples/python/01_quickstart.py

You should see a count of memory-load sites in `array_sum` and two
HTML files (`/tmp/quickstart-cfg.html` and `/tmp/quickstart-graph.html`)
you can open in a browser.
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import add, load

# 1. Load the ELF. `strider.lift.load_elf(path)` returns an `ElfLifter` —
#    one object that *is* the loaded binary (it IS a `Lifter`:
#    `isinstance(prog, strider.lift.Lifter)` is true). It auto-detects the
#    arch + calling convention from the ELF header, wires the code +
#    ROM readers internally, and answers `symbol()` / `symbols()` /
#    `entry_point()` queries — no pyelftools dance required.
WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
addr = prog.symbol("array_sum")
print(f"array_sum @ {addr:#x}")

# 2. Analyze a function. `ElfLifter.analyze(name_or_addr)` wraps:
#       Sleigh build → CFG build → IR lift → optimization
#       → indirect-branch fixed-point loop → final IR
#    in one call, returning `(Cfg, Function, unresolved_addrs)` — `cfg`
#    is the FINAL resolved CFG `function` was actually lifted from, so no
#    separate rebuild is needed to render it (see step 4).  Pattern
#    queries (`find_all`) live directly on the `Function` — no wrapper
#    needed.
cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)
print(
    f"lifted array_sum: {function.node_count()} nodes, "
    f"{len(unresolved)} unresolved indirect branches"
)

# 3. Query the optimized graph.
#    The pattern says "any load" — the simplest possible query, returns
#    every memory-load site in the function. Restrict it by composing
#    inside `addr=...` (e.g. `load(addr=add(var(base), var(off)))`) once
#    you know the shape you're hunting for.
hits = function.find_all(load(), ignore_casts=True)
print(f"found {len(hits)} memory-load sites in array_sum")

# A more specific pattern: loads whose address is a symbolic base plus
# a captured offset value. String captures are auto-interned per pattern.
narrow = function.find_all(
    load(addr=add("base", "off")),
    ignore_casts=True,
)
print(f"found {len(narrow)} loads of the form `base + offset`")
for hit in narrow:
    off_val = hit.const_uint("off")
    print(f"  offset = {off_val if off_val is not None else '<symbolic>'}")

# 4. Visualize. Open the HTMLs in any browser to see the rendered
#    graphviz output. `dark` and `dark_cfg` are the built-in styles.
#    `cfg` (from step 2's `analyze` call) already IS the final CFG —
#    render it directly, no `build_cfg` rebuild needed.  `pretty=True`
#    resolves register names, inlines constants and adds virtual nodes;
#    it needs a Sleigh, which the function reaches through its parent
#    Cfg's Lifter.  Omit it for the raw as-stored graph.
cfg.to_html("/tmp/quickstart-cfg.html", style="dark_cfg")
function.to_html("/tmp/quickstart-graph.html", pretty=True, style="dark")
print("wrote /tmp/quickstart-cfg.html and /tmp/quickstart-graph.html")
