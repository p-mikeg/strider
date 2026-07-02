"""05 — Visualization: render the CFG and IR graph as standalone HTML.

The `dot` Rust crate produces self-contained HTML files (graphviz +
embedded JavaScript renderer) that work in any browser without a
running graphviz binary. Both `Cfg` and `Function` expose the same trio:

    .to_html(path, style=...)    # write HTML file
    .to_dot(path)                # write raw .dot source
    .html_str(style=...)         # return HTML as a Python str

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

prog = strider.load_elf(str(FIXTURE))
function, unresolved = prog.analyze("array_sum", allow_code_before_start_addr=True)

# Write the CFG. `dark_cfg` is the recommended style for CFGs — higher
# contrast on basic-block boundaries.  `Function.cfg` is the snapshot
# CFG the function was lifted from — kept alive on the `Function` itself
# so it's self-describing without a separate result wrapper.
cfg_html = OUT_DIR / "array_sum-cfg.html"
function.cfg.to_html(str(cfg_html), style="dark_cfg")
print(f"wrote {cfg_html} ({cfg_html.stat().st_size} bytes)")

# Write the lifted IR graph. Use `dark` for the IR.
graph_html = OUT_DIR / "array_sum-graph.html"
function.to_html(str(graph_html), style="dark")
print(f"wrote {graph_html} ({graph_html.stat().st_size} bytes)")

# Raw .dot for piping into a different renderer or for diffing across
# code changes.
graph_dot = OUT_DIR / "array_sum-graph.dot"
function.to_dot(str(graph_dot))
print(f"wrote {graph_dot} ({graph_dot.stat().st_size} bytes)")

# `html_str` returns the HTML as a Python string — useful when you
# want to embed the visualization in a Jupyter notebook or stream it
# over a socket without writing to disk.
html_blob = function.html_str(style="dark")
print(f"html_str returned a {len(html_blob)}-byte string")
