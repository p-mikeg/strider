"""Render the CFG and the IR graph as standalone HTML or .dot.

The HTML embeds its own renderer, so it opens in any browser with no
graphviz binary installed.

`to_html` / `to_dot` take `path=None` to return the render as a string
instead of writing a file. `pretty=True` resolves register names, inlines
constants and adds virtual nodes; the default renders the graph exactly as
stored. `style` is one of "dark", "dark_cfg" (tuned for CFGs), or "empty".

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

# `analyze` already returned the final resolved CFG, so no rebuild is needed.
cfg_html = OUT_DIR / "array_sum-cfg.html"
cfg.to_html(str(cfg_html), style="dark_cfg")
print(f"wrote {cfg_html} ({cfg_html.stat().st_size} bytes)")

graph_html = OUT_DIR / "array_sum-graph.html"
function.to_html(str(graph_html), pretty=True, style="dark")
print(f"wrote {graph_html} ({graph_html.stat().st_size} bytes)")

# Raw .dot pipes into another renderer, or diffs across code changes.
graph_dot = OUT_DIR / "array_sum-graph.dot"
function.to_dot(str(graph_dot), pretty=True)
print(f"wrote {graph_dot} ({graph_dot.stat().st_size} bytes)")

# Omit `path` to get the render back as a string, for embedding in a notebook
# or streaming over a socket without touching disk.
html_blob = function.to_html(pretty=True, style="dark")
print(f"to_html(path=None) returned a {len(html_blob)}-byte string")
