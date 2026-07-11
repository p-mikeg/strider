# Unified neighborhood visualizer (IR + CFG)

## Problem

The interactive neighborhood explorer built for the IR sea-of-nodes
(`crates/strider-py/strider/explore.py` + `Function::neighborhood_dot` /
`raw_neighborhood_dot`) is well-liked: a local server renders a small,
graphviz-laid-out neighborhood around a center node, with pan/zoom, history,
a Neighbors panel, a raw toggle, and a strider-pattern search bar. We want the
same experience for the **CFG** (basic-block graph), and we want the shared
machinery factored out so the two graphs reuse one implementation.

## Goals

- A CFG neighborhood explorer with the same UX as the IR one.
- A single, clean Python entry point that works for both graphs.
- The server + frontend shell shared verbatim; only the graph-specific hooks
  differ.
- No new browser launch — print the URL only (per user preference).

## Non-goals

- No graph types beyond IR (`Function`) and CFG (`Cfg`).
- No publicly-exposed custom visualizers — the abstraction is internal.
- No CFG-specific frontend redesign — identical shell.

## API

A single method on the `Lifter` (which owns the Sleigh the pretty render
needs), dispatching on the target type:

```python
lift.visualize(target, *, host="127.0.0.1", port=0, depth=5)
```

- `target` is a `Function` (IR neighborhood) or a `Cfg` (CFG neighborhood).
- Starts the single-threaded server (the `Function`/`Cfg` are `unsendable`,
  so the server must run on the caller's thread — same as today), and
  **prints the URL**. Never opens a browser.

The current free function `strider.serve(lifter, function)` is kept as a thin
alias delegating to `lift.visualize(function)` so existing callers/tests keep
working.

## Architecture: the `_Visualizer` protocol

The server and frontend are graph-agnostic. Only three things differ between
the IR and the CFG, so we extract a small duck-typed protocol (not a real base
class — Python duck typing) and make the server consume it:

```python
class _Visualizer:               # protocol (informal)
    def entry(self) -> int                  # id of the starting center
    def dot(self, center: int, depth: int, raw: bool) -> str   # neighborhood DOT
    def search(self, query: str) -> _SearchResult              # highlight / center
    def completions(self) -> list[str]                         # autocomplete entries
```

`_SearchResult` is `{"highlight": list[int]}` (highlight matches, IR pattern
behaviour) or `{"center": int}` (recenter on a single id, CFG address jump).
Both are already expressible in the existing frontend (it highlights a set and
can recenter).

- `serve(visualizer, *, host, port, depth)` — the generic shell. Holds the
  current server (endpoints `/`, `/viz.js`, `/entry`, `/patterns`, `/dot`,
  `/pattern`) and the current `_FRONTEND` HTML verbatim, but every endpoint
  now delegates to the `visualizer` instead of calling IR-specific methods.
  `close_connection = True`, guarded writes, and the keep-alive fix are
  unchanged.
- `_IrVisualizer(lifter, function)` — wraps today's behaviour:
  `entry` → `function.entry_node()`; `dot` → `lifter.neighborhood_dot(...)` or
  `function.raw_neighborhood_dot(...)`; `search`/`completions` → the strider
  pattern eval + `strider.pattern` names.
- `_CfgVisualizer(lifter, cfg)` — new (see below).

`Lifter.visualize` builds the right visualizer and calls `serve`.

## CFG neighborhood (Rust)

Add to `strider-cfg` a neighborhood renderer mirroring the IR one:

```rust
impl Cfg {
    pub fn neighborhood_dot<R: MemReader>(
        &self, sleigh: &Sleigh<R>, center: RegionId,
        depth: usize, max_nodes: usize,
    ) -> Result<String>;

    pub fn raw_neighborhood_dot(
        &self, center: RegionId, depth: usize, max_nodes: usize,
    ) -> Result<String>;   // no Sleigh: block id + start addr + edge topology
}
```

- **BFS** over the `petgraph` region graph following **both** directions
  (`neighbors_directed(Incoming)` = predecessor blocks, `Outgoing` = successor
  blocks) up to `depth`, capped at `max_nodes` (level-order, nearest-win —
  same rule as the IR `neighborhood_nodes`). Extract the BFS as a small helper
  so it is unit-testable independent of rendering.
- **Pretty render** reuses the existing `CfgDotDumper` per-block label (addr +
  disassembly via Sleigh) and if-true/false edge styling, but only over the
  neighborhood set. Real DOT node id = region `NodeIndex` (`.index()`), so the
  explorer navigates by region id 1:1. `center` gets the gold border.
- **Raw render** = one box per region (`n<index>`: start addr + insn count) and
  edges as stored, no Sleigh — the "when I don't trust the pretty output"
  view, scale-safe via the same BFS.

Exposed on `PyCfg`: `cfg.neighborhood_dot(center, depth, max_nodes)` routes
through the Lifter's Sleigh (like the IR pretty path); `cfg.raw_neighborhood_dot(...)`
needs no Sleigh. `PyCfg` also exposes `entry()` (region index) and, for the
address search, a `block_at(addr) -> int | None` lookup returning the region
whose instruction range **contains** `addr` (any block insn addr, not only the
block start), or `None` if no block covers it.

## Search

- **IR** — unchanged: the search box evaluates a strider pattern and highlights
  the match roots; completions are `strider.pattern` names.
- **CFG** — dual mode in `_CfgVisualizer.search`:
  - a bare address (`0x401a30` / decimal) → `block_at(addr)` → `{"center": id}`
    (recenter on the block whose instruction range contains it; miss → no-op);
  - any other text → substring match over each block's disassembly →
    `{"highlight": [ids]}`.
  - completions = block start addresses.

## Testing

- **Rust (TDD).** CFG neighborhood BFS: depth 0 = center only; depth 1 reaches
  immediate predecessors and successors; `max_nodes` caps the set; a render
  test asserting the center carries the highlight border and node ids equal
  region indices. Build fixtures with the existing `strider-cfg` test helpers
  (bytes → `Builder::for_arch` → `Cfg`).
- **Python (smoke).** `lift.visualize(cfg)` on a lifted fixture: the server
  answers `/entry`, `/dot?center=…&depth=…` (pretty and `raw=1`), and
  `/pattern?q=0x…` (address jump) / text; the frontend HTML loads and contains
  the shared panels. Mirror the existing IR explorer smoke tests. Confirm no
  browser is opened (no `webbrowser` import/use in `explore.py`).

## Files touched

- `crates/strider-cfg/src/` — new `neighborhood.rs` (BFS + pretty/raw render),
  wired into `dot.rs`/`lib.rs`.
- `crates/strider-py/src/cfg.rs` — `neighborhood_dot` / `raw_neighborhood_dot`
  / `entry` / `block_at` bindings.
- `crates/strider-py/src/strider_cls.rs` — `Lifter.visualize` dispatch.
- `crates/strider-py/strider/explore.py` — generalize `serve` onto the
  `_Visualizer` protocol; add `_IrVisualizer` / `_CfgVisualizer`; drop the
  `webbrowser` launch.
- `crates/strider-py/strider/__init__.py` / `.pyi` — surface updates.
- Tests as above.
