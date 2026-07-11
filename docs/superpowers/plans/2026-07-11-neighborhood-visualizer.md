# Unified Neighborhood Visualizer (IR + CFG) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `lift.visualize(target)` — one Lifter method that starts the interactive neighborhood explorer for either a `Function` (IR) or a `Cfg`, sharing the server/frontend via an internal `_Visualizer` protocol.

**Architecture:** A new `Cfg::neighborhood_dot` / `raw_neighborhood_dot` (petgraph BFS over predecessor/successor blocks, reusing the existing `CfgDotDumper` styling) mirrors the IR neighborhood renderer. `explore.py`'s server is refactored to consume a duck-typed `_Visualizer` (`entry`/`dot`/`search`/`completions`); `_IrVisualizer` wraps today's behaviour and `_CfgVisualizer` is new. `Lifter.visualize` dispatches on the target type and prints the URL.

**Tech Stack:** Rust (`strider-cfg`, `strider-py` PyO3), Python (`http.server`, vendored viz.js), petgraph.

## Global Constraints

- Rust-only workspace + PyO3 bindings; follow clippy + workspace lints.
- The server is single-threaded — `Function`/`Cfg` are PyO3 `unsendable`; it must run on the caller's thread (existing constraint).
- Never open a browser — print the URL only. No `webbrowser` import/use in `explore.py`.
- Real DOT node id = the graph's native id (IR `NodeId.as_u32()` / CFG region `NodeIndex.index()`) so navigation is 1:1.
- After building `strider-py`, copy the `.so` per the wheel-shadow rule: `cp target/release/libstrider_py.so <venv>/.../strider/strider.abi3.so` AND `crates/strider-py/strider/strider.abi3.so`.
- Commit messages end with the `Co-Authored-By: Claude Opus 4.8 (1M context)` and `Claude-Session` trailers.

---

### Task 1: CFG neighborhood BFS (pure, unit-testable)

**Files:**
- Create: `crates/strider-cfg/src/neighborhood.rs`
- Modify: `crates/strider-cfg/src/lib.rs` (add `mod neighborhood;`)
- Test: inline `#[cfg(test)]` in `neighborhood.rs`

**Interfaces:**
- Produces: `pub(crate) fn neighborhood_regions(cfg: &Cfg, center: NodeIndex, depth: usize, max_nodes: usize) -> rustc_hash::FxHashSet<NodeIndex>`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::neighborhood_regions;
    use crate::Builder;
    use strider_target::SleighArch;

    // Two x86_64 basic blocks: `jz` splits into a taken/fallthrough pair.
    // 7500 (jz +2), 90 (nop), C3 (ret) → entry block + two successors.
    fn two_way_cfg() -> crate::Cfg {
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let arch = SleighArch::x86_64();
        let mut sleigh = arch.build_sleigh(strider_reader::BufferReader::new(0x1000, bytes.clone()))
            .expect("sleigh");
        Builder::for_arch(&arch, &mut sleigh, 0x1000, &crate::CfgOptions::default())
            .build()
            .expect("cfg")
    }

    #[test]
    fn depth_bounds_and_walks_both_directions() {
        let cfg = two_way_cfg();
        let entry = cfg.entry();
        // depth 0 = just the center
        assert_eq!(neighborhood_regions(&cfg, entry, 0, 999).len(), 1);
        // depth 1 from entry reaches its successor block(s)
        let d1 = neighborhood_regions(&cfg, entry, 1, 999);
        assert!(d1.len() >= 2, "depth 1 must reach a successor: {}", d1.len());
        assert!(d1.contains(&entry));
        // budget caps the set
        assert!(neighborhood_regions(&cfg, entry, 5, 1).len() <= 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-cfg --lib neighborhood`
Expected: FAIL — `neighborhood_regions` not found (module doesn't exist).

> Note: confirm the exact `Builder::for_arch` / `SleighArch::build_sleigh` / `BufferReader` constructor names against `crates/strider-cfg/src/dot.rs` tests (`fn dot_string`) which already build a `Cfg` from bytes; copy that harness verbatim if the names differ.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Neighborhood BFS over the CFG region graph, for the interactive explorer.

use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

use crate::Cfg;

/// BFS the depth-`depth` neighborhood around `center` over **both** edge
/// directions (predecessor + successor blocks), capped at `max_nodes`. BFS
/// visits in level order, so the budget keeps the nearest `max_nodes` regions.
pub(crate) fn neighborhood_regions(
    cfg: &Cfg,
    center: NodeIndex,
    depth: usize,
    max_nodes: usize,
) -> FxHashSet<NodeIndex> {
    let g = cfg.region_graph();
    let mut seen = FxHashSet::default();
    seen.insert(center);
    let mut queue = VecDeque::from([(center, 0usize)]);
    'bfs: while let Some((node, dist)) = queue.pop_front() {
        if dist >= depth {
            continue;
        }
        let neighbors = g
            .neighbors_directed(node, Direction::Incoming)
            .chain(g.neighbors_directed(node, Direction::Outgoing));
        for nb in neighbors {
            if seen.len() >= max_nodes {
                break 'bfs;
            }
            if seen.insert(nb) {
                queue.push_back((nb, dist + 1));
            }
        }
    }
    seen
}
```

Add to `lib.rs` near the other `mod` declarations:
```rust
mod neighborhood;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-cfg --lib neighborhood`
Expected: PASS (both assertions).

- [ ] **Step 5: Commit**

```bash
git add crates/strider-cfg/src/neighborhood.rs crates/strider-cfg/src/lib.rs
git commit -m "feat(cfg): neighborhood BFS over the region graph"
```

---

### Task 2: CFG pretty + raw neighborhood renderers

**Files:**
- Modify: `crates/strider-cfg/src/neighborhood.rs` (add the two render methods + render test)
- Reference: `crates/strider-cfg/src/dot.rs` (reuse the per-block label logic — extract a shared `fn region_label(cfg, sleigh, node) -> Result<String>` if the existing `dump_as_dot` body can be factored; otherwise inline the same label building)

**Interfaces:**
- Consumes: `neighborhood_regions` (Task 1)
- Produces:
  - `pub fn Cfg::neighborhood_dot<R: rsleigh::MemReader>(&self, sleigh: &rsleigh::Sleigh<R>, center: NodeIndex, depth: usize, max_nodes: usize) -> crate::Result<String>`
  - `pub fn Cfg::raw_neighborhood_dot(&self, center: NodeIndex, depth: usize, max_nodes: usize) -> crate::Result<String>`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn neighborhood_dot_ids_are_region_indices_and_center_highlighted() {
        let cfg = two_way_cfg();
        let entry = cfg.entry();
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let arch = SleighArch::x86_64();
        let mut sleigh = arch.build_sleigh(strider_reader::BufferReader::new(0x1000, bytes))
            .expect("sleigh");
        let dot = cfg.neighborhood_dot(&sleigh, entry, 1, 999).expect("dot");
        // real dot node id == region index of the center
        assert!(dot.contains(&format!("\"{}\"", entry.index())));
        // center carries the gold highlight border
        assert!(dot.contains("#ffcc00"));

        // raw: one n<idx> box per region, no Sleigh, edges as stored
        let raw = cfg.raw_neighborhood_dot(entry, 1, 999).expect("raw");
        assert!(raw.contains(&format!("n{}", entry.index())));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-cfg --lib neighborhood`
Expected: FAIL — `neighborhood_dot` / `raw_neighborhood_dot` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `neighborhood.rs` (uses `::dot::DotEmitter` + `DotStyle::dark_cfg()`; the pretty per-block label mirrors `CfgDotDumper::dump_as_dot` — `Instruction(addr=…)` + `\l<addr>: <insn.ctx_fmt(sleigh,&regs)>` lines; the raw label is `n<idx>: <start_addr:#x> (<insns.len()> insns)`):

```rust
use crate::types::RegionInstruction;

impl Cfg {
    /// Pretty render of the depth-`depth` neighborhood around region `center`
    /// (BFS over predecessor+successor blocks, `max_nodes` budget), reusing the
    /// full-CFG block styling. DOT node ids are region indices; `center` gets a
    /// gold border.
    pub fn neighborhood_dot<R: rsleigh::MemReader>(
        &self,
        sleigh: &rsleigh::Sleigh<R>,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        let set = neighborhood_regions(self, center, depth, max_nodes);
        let regs = sleigh.regs()?;
        let g = self.region_graph();
        let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::dark_cfg());
        for &node in &set {
            let region = g.node_weight(node).ok_or_else(|| anyhow::anyhow!("bad region"))?;
            let start = region.start_addr.machine_addr.addr;
            let mut label = format!("Instruction(addr={start:#x})");
            for insn in &region.insns {
                let a = insn.addr.machine_addr.addr;
                let pretty = insn.insn.ctx_fmt(sleigh, &regs);
                label.push_str(&format!("\\l{a:#x}: {pretty}"));
            }
            label.push_str("\\l");
            let id = node.index().to_string();
            let extra: &[(&str, &str)] = if node == center {
                &[("color", "\"#ffcc00\""), ("penwidth", "2.5")]
            } else {
                &[]
            };
            out.node(&id, &label, "box", extra);
        }
        // Control edges within the set (topology; unweighted).
        for &node in &set {
            for succ in g.neighbors_directed(node, Direction::Outgoing) {
                if set.contains(&succ) {
                    out.edge(&node.index().to_string(), &succ.index().to_string(), &[]);
                }
            }
        }
        Ok(out.finish())
    }

    /// Structure-faithful render of the neighborhood: one `n<idx>` box per
    /// region (start addr + instruction count), edges as stored, no Sleigh.
    pub fn raw_neighborhood_dot(
        &self,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        let set = neighborhood_regions(self, center, depth, max_nodes);
        let g = self.region_graph();
        let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::dark_cfg());
        for &node in &set {
            let region = g.node_weight(node).ok_or_else(|| anyhow::anyhow!("bad region"))?;
            let start = region.start_addr.machine_addr.addr;
            let label = format!("n{}  {start:#x}\\l{} insns", node.index(), region.insns.len());
            let id = format!("n{}", node.index());
            let extra: &[(&str, &str)] = if node == center {
                &[("color", "\"#ffcc00\""), ("penwidth", "2.5")]
            } else {
                &[]
            };
            out.node(&id, &label, "box", extra);
        }
        for &node in &set {
            for succ in g.neighbors_directed(node, Direction::Outgoing) {
                if set.contains(&succ) {
                    out.edge(&format!("n{}", node.index()), &format!("n{}", succ.index()), &[]);
                }
            }
        }
        Ok(out.finish())
    }
}
```

> If `DotStyle::dark_cfg()` / `RegionInstruction` imports differ, copy the exact paths from `crates/strider-cfg/src/dot.rs`. Do NOT re-derive if-true/false edge labels for the neighborhood — plain control edges are sufficient for v1 (note this simplification with a `// ponytail:` comment; edge labels can be a follow-up).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-cfg --lib neighborhood`
Expected: PASS. Also run `cargo clippy -p strider-cfg` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-cfg/src/neighborhood.rs
git commit -m "feat(cfg): pretty + raw neighborhood DOT renderers"
```

---

### Task 3: PyCfg bindings (neighborhood_dot / raw / entry / block_at)

**Files:**
- Modify: `crates/strider-py/src/cfg.rs`
- Test: exercised via Task 6's Python smoke test (no standalone Rust test — needs a live Sleigh from the Lifter)

**Interfaces:**
- Consumes: `Cfg::neighborhood_dot`, `Cfg::raw_neighborhood_dot`, `Cfg::entry` (Task 2)
- Produces (on `PyCfg`):
  - `fn entry(&self) -> u32` — region index of the CFG entry
  - `fn neighborhood_dot(&self, center: u32, depth: usize, max_nodes: usize) -> PyResult<String>`
  - `fn raw_neighborhood_dot(&self, center: u32, depth: usize, max_nodes: usize) -> PyResult<String>`
  - `fn block_at(&self, addr: u64) -> Option<u32>`

- [ ] **Step 1: Add the bindings**

In `crates/strider-py/src/cfg.rs`, inside the `#[pymethods] impl PyCfg` block (use `with_sleigh` — the existing helper that borrows the Lifter's Sleigh — for the pretty path; `NodeIndex::new(center as usize)` to rebuild a region id; `into_strider_err` for errors):

```rust
    /// The region index of the CFG entry — the default explorer center.
    fn entry(&self) -> u32 {
        self.inner.entry().index() as u32
    }

    /// Pretty neighborhood DOT around region `center` (needs the Lifter's Sleigh).
    #[pyo3(signature = (center, depth=5, max_nodes=60))]
    fn neighborhood_dot(&self, py: Python<'_>, center: u32, depth: usize, max_nodes: usize)
        -> PyResult<String>
    {
        let node = petgraph::graph::NodeIndex::new(center as usize);
        self.with_sleigh(py, |cfg, sleigh| cfg.neighborhood_dot(sleigh, node, depth, max_nodes))
            .map_err(crate::errors::into_strider_err)
    }

    /// Structure-faithful neighborhood DOT (no Sleigh).
    #[pyo3(signature = (center, depth=5, max_nodes=60))]
    fn raw_neighborhood_dot(&self, center: u32, depth: usize, max_nodes: usize) -> PyResult<String> {
        let node = petgraph::graph::NodeIndex::new(center as usize);
        self.inner.raw_neighborhood_dot(node, depth, max_nodes)
            .map_err(crate::errors::into_strider_err)
    }

    /// The region index whose instruction range contains `addr`, if any.
    fn block_at(&self, addr: u64) -> Option<u32> {
        for (idx, region) in self.inner.region_graph().node_references_or_regions() {
            // range = [start machine addr, last insn machine addr]
            let start = region.start_addr.machine_addr.addr;
            let last = region.insns.last().map_or(start, |i| i.addr.machine_addr.addr);
            if start <= addr && addr <= last {
                return Some(idx.index() as u32);
            }
        }
        None
    }
```

> Verify `with_sleigh`'s exact closure signature in `cfg.rs` (it wraps `dispatch_dot`). For `block_at`, iterate with whatever region-iteration the crate exposes — prefer `self.inner.regions()` paired with `region_graph().node_indices()` if there is no `node_references`. If `region_graph()`/`entry()` are not `pub` on `Cfg`, add thin `pub` accessors in `strider-cfg` (`entry` already exists; add `regions_with_ids()` returning `(NodeIndex, &Region)` if needed).

- [ ] **Step 2: Build and verify it compiles**

Run: `cargo build -p strider-py --release`
Expected: builds clean. Then copy the `.so` per the wheel-shadow rule (Global Constraints).

- [ ] **Step 3: Smoke-check the bindings from Python**

Run:
```bash
python - <<'PY'
import strider
lift = strider.lifter(strider.SleighArch.x86_64(), strider.BufferReader(0x1000, b"\x75\x01\x90\xc3"))
cfg = lift.build_cfg(0x1000)
e = cfg.entry()
print("entry", e, "dot ok", "#ffcc00" in cfg.neighborhood_dot(e, depth=1))
print("raw ok", ("n%d"%e) in cfg.raw_neighborhood_dot(e, depth=1))
print("block_at", cfg.block_at(0x1000))
PY
```
Expected: prints `entry <n> dot ok True`, `raw ok True`, `block_at <n>`.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/src/cfg.rs crates/strider-cfg/src/  # if accessors added
git commit -m "feat(py): PyCfg neighborhood_dot / raw / entry / block_at"
```

---

### Task 4: `_Visualizer` protocol — refactor `serve` + `_IrVisualizer`; drop browser

**Files:**
- Modify: `crates/strider-py/strider/explore.py`
- Test: Task 6

**Interfaces:**
- Produces:
  - `serve(visualizer, *, host="127.0.0.1", port=0, depth=5)` — generic server over a visualizer
  - `class _IrVisualizer` with `entry()`, `dot(center, depth, raw)`, `search(query)`, `completions()`

- [ ] **Step 1: Extract the generic server**

Refactor `explore.py` so the `do_GET` endpoints delegate to a `visualizer` object rather than `lifter`/`function`:
- `/entry` → `json.dumps(visualizer.entry())`
- `/patterns` → `json.dumps(visualizer.completions())`
- `/dot` → `visualizer.dot(center, depth, raw)`
- `/pattern` → `json.dumps(visualizer.search(q))` where `search` returns a dict `{"highlight":[...]}` or `{"center": id}` (frontend already highlights a set; add a 2-line branch in the frontend `search()` JS: if the response has `center`, call `recenter(center)`, else highlight as today).

Add the IR visualizer wrapping current behaviour:
```python
class _IrVisualizer:
    def __init__(self, lifter, function):
        self._lifter, self._fn = lifter, function
    def entry(self):
        return self._fn.entry_node()
    def dot(self, center, depth, raw):
        return (self._fn.raw_neighborhood_dot(center, depth=depth) if raw
                else self._lifter.neighborhood_dot(self._fn, center, depth=depth))
    def search(self, query):
        return {"highlight": _run_pattern(self._fn, query)}
    def completions(self):
        return _pattern_names()
```

Delete every `webbrowser` import and call — the server only `print`s the URL.

- [ ] **Step 2: Keep `serve` back-compat alias**

`serve(lifter, function, ...)` may still be imported by tests/users. Make the OLD signature delegate:
```python
def serve(lifter, function, host="127.0.0.1", port=0, depth=5):
    return _serve(_IrVisualizer(lifter, function), host=host, port=port, depth=depth)
```
where `_serve(visualizer, ...)` is the generic shell.

- [ ] **Step 3: Manual smoke (deferred to Task 6)**

No standalone test here; covered by Task 6.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/strider/explore.py
git commit -m "refactor(explore): serve over a _Visualizer protocol; drop browser launch"
```

---

### Task 5: `_CfgVisualizer` + `Lifter.visualize` dispatch

**Files:**
- Modify: `crates/strider-py/strider/explore.py` (add `_CfgVisualizer`)
- Modify: `crates/strider-py/src/strider_cls.rs` (add `Lifter.visualize`)
- Modify: `crates/strider-py/strider/__init__.py` / `crates/strider-py/strider/*.pyi`

**Interfaces:**
- Consumes: Task 3 bindings, Task 4 `_serve`
- Produces:
  - `class _CfgVisualizer` with the same four methods (address/text search)
  - `Lifter.visualize(target, *, host, port, depth)` dispatching Function vs Cfg

- [ ] **Step 1: Add `_CfgVisualizer`**

```python
class _CfgVisualizer:
    def __init__(self, cfg):
        self._cfg = cfg
    def entry(self):
        return self._cfg.entry()
    def dot(self, center, depth, raw):
        return (self._cfg.raw_neighborhood_dot(center, depth=depth) if raw
                else self._cfg.neighborhood_dot(center, depth=depth))
    def search(self, query):
        q = query.strip()
        try:
            addr = int(q, 0)          # bare address → center the containing block
            blk = self._cfg.block_at(addr)
            return {"center": blk} if blk is not None else {"highlight": []}
        except ValueError:
            hits = [rid for rid in self._all_region_ids()
                    if q.lower() in self._region_text(rid).lower()]
            return {"highlight": hits}
    def completions(self):
        return []   # (block-start addresses can be added later)
```

For `_region_text` / `_all_region_ids`: use `self._cfg.raw_neighborhood_dot` is not enough — add a small `PyCfg.region_text(rid) -> str` (disassembly of one block) and `PyCfg.region_ids() -> list[int]` in Task 3's file if not already present, OR reuse `pcode_at`. Simplest: add `PyCfg.region_texts() -> dict[int, str]` (index → joined disassembly) and cache it in `_CfgVisualizer.__init__`.

> Adjust Task 3 to also expose `region_texts()` if you take this route; keep the search substring match over that dict.

- [ ] **Step 2: Add `Lifter.visualize` (Rust)**

In `crates/strider-py/src/strider_cls.rs`, add a method that imports the Python `explore` module and calls the right visualizer. Since the visualizer + server are Python, the cleanest is a thin Python-level dispatch: add `visualize` in `explore.py` and call it. In `strider_cls.rs` expose nothing new if `visualize` can be a pure-Python `Lifter` method — but `Lifter` is a pyclass, so add the dispatch in Python by attaching it, OR add a Rust `#[pymethod] fn visualize` that calls into `explore`.

Preferred (pure Python, least Rust): add to `explore.py`:
```python
def visualize(lifter, target, *, host="127.0.0.1", port=0, depth=5):
    # dispatch on type name to avoid importing the pyclass types
    tn = type(target).__name__
    if tn == "Function":
        vis = _IrVisualizer(lifter, target)
    elif tn == "Cfg":
        vis = _CfgVisualizer(target)
    else:
        raise TypeError(f"visualize expects a Function or Cfg, got {tn}")
    return _serve(vis, host=host, port=port, depth=depth)
```
and bind it as a `Lifter` method in `strider_cls.rs`:
```rust
#[pyo3(signature = (target, host="127.0.0.1".to_string(), port=0, depth=5))]
fn visualize(slf: Py<Self>, py: Python<'_>, target: Py<PyAny>,
             host: String, port: u16, depth: usize) -> PyResult<()> {
    let explore = py.import("strider.explore")?;
    explore.call_method1("visualize", (slf, target, /* kwargs host/port/depth */))?;
    Ok(())
}
```
(Use `PyDict` kwargs for host/port/depth; `slf` is the `Py<PyLifter>` passed as the `lifter` arg.)

- [ ] **Step 3: Surface + build**

Ensure `visualize` is importable and `serve` still exported from `__init__.py`. Add `.pyi` stubs for `Lifter.visualize`, `Cfg.entry/neighborhood_dot/raw_neighborhood_dot/block_at`. Build:
```bash
cargo build -p strider-py --release   # + copy .so per wheel-shadow rule
```

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/strider/explore.py crates/strider-py/src/strider_cls.rs crates/strider-py/strider/__init__.py crates/strider-py/strider/*.pyi
git commit -m "feat(py): lift.visualize(target) — CFG visualizer + type dispatch"
```

---

### Task 6: End-to-end smoke test

**Files:**
- Create: `crates/strider-py/tests/python/test_visualize_cfg.py`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the smoke test**

```python
import threading, time, urllib.request, json
import strider


def _serve_bg(target_kind, port):
    def run():
        lift = strider.lifter(strider.SleighArch.x86_64(),
                              strider.BufferReader(0x1000, b"\x75\x01\x90\xc3"))
        cfg = lift.build_cfg(0x1000)
        target = cfg if target_kind == "cfg" else lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())[1]
        lift.visualize(target, port=port)
    threading.Thread(target=run, daemon=True).start()


def _get(port, path):
    for _ in range(40):
        try:
            return urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=8).read().decode()
        except Exception:
            time.sleep(0.3)
    raise RuntimeError("server never came up")


def test_visualize_cfg_serves_neighborhood_and_search():
    _serve_bg("cfg", 8931)
    entry = int(_get(8931, "/entry"))
    pretty = _get(8931, f"/dot?center={entry}&depth=1&raw=0")
    raw = _get(8931, f"/dot?center={entry}&depth=1&raw=1")
    assert "#ffcc00" in pretty              # center highlighted
    assert f"n{entry}" in raw               # raw uses region-index ids
    # address search centers the containing block
    res = json.loads(_get(8931, "/pattern?q=0x1000"))
    assert res.get("center") is not None or res.get("highlight") is not None
    # frontend loads
    assert "viz" in _get(8931, "/").lower()


def test_visualize_ir_still_works():
    _serve_bg("ir", 8932)
    entry = int(_get(8932, "/entry"))
    assert _get(8932, f"/dot?center={entry}&depth=2&raw=0")
```

- [ ] **Step 2: Run it**

Run: `uv run --no-sync python -m pytest crates/strider-py/tests/python/test_visualize_cfg.py -q`
Expected: 2 passed.

- [ ] **Step 3: Regression gate**

Run: `cargo test -p strider-cfg` and the existing explorer-adjacent pytest; `cargo clippy -p strider-cfg -p strider-py --release` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/tests/python/test_visualize_cfg.py
git commit -m "test(py): lift.visualize(cfg) end-to-end smoke"
```

---

## Self-Review

- **Spec coverage:** API (`lift.visualize`) → Task 5. `_Visualizer` protocol + IR/CFG visualizers → Tasks 4–5. `Cfg::neighborhood_dot`/`raw` → Tasks 1–2. PyCfg `entry`/`block_at` → Task 3. Dual-mode search → Task 5. No browser → Task 4. Testing → Tasks 1,2,6. Covered.
- **Open verification points (flagged inline, resolve during execution, do not guess):** exact `Builder::for_arch`/`build_sleigh` test-harness names (Task 1), `with_sleigh` closure shape + region iteration for `block_at`/`region_texts` (Task 3), whether `region_graph()` is `pub` (add accessor if not), and PyO3 kwargs plumbing for `Lifter.visualize` (Task 5).
- **Simplifications (intentional, v1):** neighborhood control edges are unlabelled (no if-true/false) — mark with `// ponytail:`; CFG completions empty. Both are follow-ups, not gaps.
