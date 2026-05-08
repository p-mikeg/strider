# `dot` — Graphviz `.dot` and `.html` rendering

Converts any "graph that can iterate its nodes" into Graphviz DOT source,
optionally rendered as standalone interactive HTML. Used by [`cfg`](../cfg),
[`ir`](../ir), and the strider CLI example to dump the CFG, the raw IR, and the
optimised IR side-by-side.

## Public surface

- `GraphDotDumper` — implement on a graph type to emit DOT statements per node.
  Has associated types `Node`, `Error`, `State` and methods
  `create_initial_state`, `iter_nodes`, `dump_as_dot`.
- `GraphDot<G: GraphDotDumper>` — wraps a dumper plus a `DotStyle`.
  `as_dot()` returns the raw DOT string; `as_svg()` shells out to `dot -Tsvg`;
  `as_html_from_svg()` and `as_html_from_dot()` produce standalone HTML pages;
  `dump_as_dot(path)` and `dump_as_html(path)` write to disk.
- `DotStyle` — pre-built visual themes. `DotStyle::dark()`, `DotStyle::dark_cfg()`,
  `DotStyle::empty()`.
- `DotEmitter` — low-level string builder. `new(name, style)`, `node(id, label,
  shape, extra)`, `edge(from, to, extra)`, `finish()`. Handles DOT label
  escaping internally.

## Architecture

The crate is a single-file library (`src/lib.rs`). Two HTML templates live in
`assets/`: `graph_template_svg.html` (used by `as_html_from_svg`, embeds an SVG
pre-rendered by the system `dot` binary) and `graph_template_dot.html` (used by
`as_html_from_dot`, embeds the raw DOT source and renders it client-side via
Graphviz WASM, so no local `dot` install is needed).

`escape_dot_label` and `json_quote` are private helpers handling DOT label
escaping and JSON-string-in-HTML escaping, respectively. The latter
unconditionally escapes `<` to `<` so a label containing `</script>` cannot
break out of the embedding `<script type="application/json">` element.

## Key invariants

- DOT identifiers and labels emitted by `DotEmitter::node` and `DotEmitter::edge`
  are always wrapped in double quotes and escaped. Literal `\n` characters in a
  label are converted to the DOT centre-justify escape; pre-existing `\n`/`\l`/`\r`
  two-char escapes pass through unchanged.
- `DotEmitter` `extra` attributes are inserted verbatim — caller must quote
  values that need quoting (e.g. `("fillcolor", "\"#3a2a10\"")`).
- `as_html_from_dot` is the recommended entry point for interactive output: it
  has no system dependency on Graphviz.
- The `GraphDotDumper::Error` bound is `Debug + Display + Send + Sync + 'static`
  rather than `std::error::Error`, so impls can use `anyhow::Error` directly.

## Tests

Inline unit tests in `src/lib.rs` (mod `label_tests`) cover `escape_dot_label`
and `json_quote` edge cases. Integration tests in `crates/dot/tests/`.

```
cargo test --package dot
```

## Gotchas

- `as_svg()` and `as_html_from_svg()` shell out to the system `dot` binary; if
  it is not installed they return an `anyhow::Error`. Use `as_html_from_dot()`
  for offline use.
- The dark CFG theme (`DotStyle::dark_cfg()`) explicitly switches the node font
  from `monospace` to `Courier` because `viz.js` (the WASM Graphviz) only ships
  width metrics for a fixed font set; without the swap, multiline labels
  overflow their boxes in browser-rendered HTML.
- Zero workspace dependencies beyond `anyhow`. This crate intentionally does
  not know about `rsleigh`, `ir`, or any other strider crate — implementors of
  `GraphDotDumper` provide the domain-specific node-formatting logic.
