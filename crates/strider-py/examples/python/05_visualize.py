"""05 — Visualization: render the CFG and IR graph as standalone HTML.

The `dot` Rust crate produces self-contained HTML files (graphviz +
embedded JavaScript renderer) that work in any browser without a
running graphviz binary.

Both renders need a `Sleigh` (to resolve register names, inline
constants, add virtual nodes), which only the `Lifter` owns — a bare
`Function` carries none.  So `Cfg` (returned by `Lifter.analyze`
alongside the `Function` — the FINAL resolved CFG, no separate
`build_cfg` rebuild needed) is rendered via its own pair, and the IR
`Function` is rendered via the same pair on the `Function` itself:

    function.to_html(path=None, pretty=True, style=...)  # HTML file, or a str when path=None
    function.to_dot(path=None, pretty=True)              # .dot source, file or str

`pretty=True` selects the Sleigh-backed render (register names, inlined
constants, virtual nodes); the default `pretty=False` renders the graph
exactly as stored. The `style` argument applies to the pretty render and
is one of `"dark"`, `"dark_cfg"` (brighter palette tuned for CFGs), or
`"empty"`.

Run from the workspace root:
    python crates/strider-py/examples/python/05_visualize.py
    open /tmp/array_sum-cfg.html /tmp/array_sum-graph.html
"""

from __future__ import annotations

import pathlib

import strider

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"
OUT_DIR = pathlib.Path("/tmp")

prog = strider.lift.load_elf(str(FIXTURE))
cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)

# Write the CFG. `dark_cfg` is the recommended style for CFGs — higher
# contrast on basic-block boundaries.  `cfg` is the FINAL resolved CFG
# `analyze` returned above (the one `function` was actually lifted from)
# — no separate `build_cfg` rebuild needed.
cfg_html = OUT_DIR / "array_sum-cfg.html"
cfg.to_html(str(cfg_html), style="dark_cfg")
print(f"wrote {cfg_html} ({cfg_html.stat().st_size} bytes)")

# Write the lifted IR graph. Use `dark` for the IR.  `pretty=True` picks
# the Sleigh-backed renderer (the function reaches the Sleigh through its
# parent Cfg's Lifter); without it you get the raw as-stored graph.
graph_html = OUT_DIR / "array_sum-graph.html"
function.to_html(str(graph_html), pretty=True, style="dark")
print(f"wrote {graph_html} ({graph_html.stat().st_size} bytes)")

# Raw .dot for piping into a different renderer or for diffing across
# code changes.
graph_dot = OUT_DIR / "array_sum-graph.dot"
function.to_dot(str(graph_dot), pretty=True)
print(f"wrote {graph_dot} ({graph_dot.stat().st_size} bytes)")

# Omitting `path` returns the render as a Python string instead of
# writing to a file — useful when you want to embed the visualization
# in a Jupyter notebook or stream it over a socket without writing to
# disk.
html_blob = function.to_html(pretty=True, style="dark")
print(f"to_html(path=None) returned a {len(html_blob)}-byte string")
