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

# 1. Build a MemoryMap from the ELF and look up a symbol address.
#    `add_region_from_elf` pulls the SHF_ALLOC sections (code + .rodata)
#    into a fast Rust-side region table AND caches the parsed ELF for
#    later `symbol()` / `symbols()` / `entry_point()` queries — no
#    pyelftools dance required.
WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

mem = strider.MemoryMap()
mem.add_region_from_elf(str(FIXTURE))

addr = mem.symbol("array_sum")
print(f"array_sum @ {addr:#x}")

# 3. Run the full pipeline. This wraps:
#       Sleigh build → CFG build → IR lift → optimization
#       → indirect-branch fixed-point loop → final IR
#    in one call, returning the CFG, the lifted Function, and the Sleigh
#    handle that produced them.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem,
    rom=mem,                         # same MemoryMap serves both roles
    entry=addr,
    allow_code_before_start_addr=True,
)

print(f"lifted {result.function}, {result.cfg}")

# 4. Query the optimized graph.
#    The pattern says "any load" — the simplest possible query, returns
#    every memory-load site in the function. Restrict it by composing
#    inside `addr=...` (e.g. `load(addr=add(var(base), var(off)))`) once
#    you know the shape you're hunting for.
hits = result.function.find_all(load(), ignore_casts=True)
print(f"found {len(hits)} memory-load sites in array_sum")

# A more specific pattern: loads whose address is a symbolic base plus
# a captured offset value. String captures are auto-interned per pattern.
narrow = result.function.find_all(
    load(addr=add("base", "off")),
    ignore_casts=True,
)
print(f"found {len(narrow)} loads of the form `base + offset`")
for hit in narrow:
    off_val = hit.uint("off")
    print(f"  offset = {off_val if off_val is not None else '<symbolic>'}")

# 5. Visualize. Open the HTMLs in any browser to see the rendered
#    graphviz output. `dark` and `dark_cfg` are the built-in styles.
result.cfg.to_html("/tmp/quickstart-cfg.html", style="dark_cfg")
result.function.to_html("/tmp/quickstart-graph.html", style="dark")
print("wrote /tmp/quickstart-cfg.html and /tmp/quickstart-graph.html")
