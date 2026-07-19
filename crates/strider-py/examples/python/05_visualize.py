"""05 — Visualization: render the CFG and IR graph as standalone HTML.

The `dot` Rust crate produces self-contained HTML files (graphviz +
embedded JavaScript renderer) that work in any browser without a
running graphviz binary.

Both renders need a `Sleigh` (to resolve register names, inline
constants, add virtual nodes), which only the `Lifter` owns — a bare
`Function` carries none.  So `Cfg` (returned by `Lifter.analyze`
alongside the `Function` — the FINAL resolved CFG, no separate
`build_cfg` rebuild needed) is rendered via its own pair, and the IR
`Function` is rendered via the same pair on the `Lifter` that produced
it:

    lift.to_html(function, path=None, style=...)  # HTML file, or a str when path=None
    lift.to_dot(function, path=None)                # raw .dot source, file or str

The `style` argument is one of `"dark"`, `"light"`, or `"dark_cfg"`
(brighter palette tuned for CFGs).

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

# Write the lifted IR graph. Use `dark` for the IR.  `to_html` / `to_dot`
# live on the `Lifter` (not `Function`) because the pretty renderer needs
# the Sleigh to resolve register names.
graph_html = OUT_DIR / "array_sum-graph.html"
prog.to_html(function, str(graph_html), style="dark")
print(f"wrote {graph_html} ({graph_html.stat().st_size} bytes)")

# Raw .dot for piping into a different renderer or for diffing across
# code changes.
graph_dot = OUT_DIR / "array_sum-graph.dot"
prog.to_dot(function, str(graph_dot))
print(f"wrote {graph_dot} ({graph_dot.stat().st_size} bytes)")

# Omitting `path` returns the render as a Python string instead of
# writing to a file — useful when you want to embed the visualization
# in a Jupyter notebook or stream it over a socket without writing to
# disk.
html_blob = prog.to_html(function, style="dark")
print(f"to_html(path=None) returned a {len(html_blob)}-byte string")
