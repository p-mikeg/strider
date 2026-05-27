# HTML Graph-Viewer Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an edge-label sidebar field, `nXX` node names, a coarse-NodeKind picker+iterate control, and fix the minimap viewport-rect/click-to-pan coordinate bug — all in the single offline viewer template.

**Architecture:** Pure frontend. Everything lives in `crates/dot/assets/graph_template_dot.html` (HTML/CSS/JS); no Rust logic changes. The data each feature needs is already in the rendered DOM (edge label `<text>`, node serial `<title>`, coarse kind via `classify()`). A `dot`-crate test guards that the template still contains the new controls. Interactive behavior (minimap drag, kind iterate) is verified by the user in a browser.

**Tech Stack:** Static HTML + vanilla JS, client-side Graphviz via inlined `viz-standalone.js`, `svg-pan-zoom.min.js`. Rust `dot` crate (`include_str!` + string substitution).

**Verification commands:**
- `cargo test -p dot` (structural template test + existing tests)
- `cargo run -p strider-analyze --example orchestrator_demo` (regenerates `graph.html` / `graph-opt.html` / `cfg.html` in the workspace root — open in a browser to verify)
- `cargo clippy -p dot --all-targets -- -D warnings`

**Branch:** `rewrite/html-viewer`. Commit + push after each task: `git push origin rewrite/html-viewer`.

**Line numbers below are pre-edit references into `graph_template_dot.html`; re-grep before editing since earlier tasks shift them.**

---

## Task 1: Edge label in a dedicated sidebar field

**Files:**
- Modify: `crates/dot/assets/graph_template_dot.html` (Selection-panel HTML ~187; `updSelUI` ~358-366)

- [ ] **Step 1: Add the sidebar row.** In the Selection `kv` block (currently `Type`/`Node`/`Edge` rows ~184-188), add an Edge-label row after the `Edge` row:

```html
            <div class="k">Edge</div><div class="v" id="sEdge">—</div>
            <div class="k">Edge label</div><div class="v" id="sEdgeLabel">—</div>
```

- [ ] **Step 2: Populate it in `updSelUI`.** Replace the body of `updSelUI` (~358-366) so the edge label is shown only for edge selections (the label text is the edge `<g>`'s `<text>`, via the existing `labelOf`):

```js
function updSelUI(){
  document.getElementById("sType").textContent=sel.type??"—";
  document.getElementById("sNode").textContent=sel.type==="node"?("n"+sel.name):"—";
  document.getElementById("sEdge").textContent=sel.ekey??"—";
  // Edge label: the <text> rendered inside the selected g.edge (e.g. "lhs",
  // "true", "arg0"). Only meaningful for an edge selection.
  const elbl=(sel.type==="edge"&&sel.el)?labelOf(sel.el):"";
  document.getElementById("sEdgeLabel").textContent=elbl||"—";
  // #lblText still mirrors the selected element's full text (node or edge).
  const lbl=sel.el?labelOf(sel.el):"";
  const lw=document.getElementById("lblWrap");
  document.getElementById("lblText").textContent=lbl;
  lw.style.display=lbl?"":"none";
}
```

(Note: this same edit implements Task 2's `nXX` node name via the `sel.type==="node"?("n"+sel.name)` line — keep both in this one function.)

- [ ] **Step 3: Verify structurally.** Run: `grep -c 'id="sEdgeLabel"' crates/dot/assets/graph_template_dot.html` → Expected: `1`. Then `cargo build -p dot` → Expected: success.

- [ ] **Step 4: Commit.**

```bash
git add crates/dot/assets/graph_template_dot.html
git commit -m "feat(viewer): show selected edge's label in a dedicated sidebar field"
git push origin rewrite/html-viewer
```

---

## Task 2: Node name "nXX" in the sidebar

This is folded into the `updSelUI` rewrite in Task 1, Step 2 (the
`sel.type==="node"?("n"+sel.name)` expression turns the bare serial `14`
into `n14`). If Task 1 is already committed, no separate edit is needed —
but verify and add an explicit check.

**Files:**
- Verify: `crates/dot/assets/graph_template_dot.html` (`updSelUI`)

- [ ] **Step 1: Confirm the formatting is present.** Run: `grep -n '"n"+sel.name' crates/dot/assets/graph_template_dot.html` → Expected: one hit inside `updSelUI`.

- [ ] **Step 2: Guard the empty case.** Ensure a non-node selection shows `—` (already handled by the ternary's `:"—"`). No further code.

- [ ] **Step 3:** (No commit if folded into Task 1.) If implemented separately, commit:

```bash
git add crates/dot/assets/graph_template_dot.html
git commit -m "feat(viewer): show node name as nXX in the selection panel"
git push origin rewrite/html-viewer
```

---

## Task 3: Coarse NodeKind picker + iterate

**Files:**
- Modify: `crates/dot/assets/graph_template_dot.html` — the "Node kinds" sidebar section (~242-245); add JS near the search/iterate block (~744-778); wire buttons near the existing button wiring (~927-930); call the list builder where `applyKinds()` is invoked after indexing (search for `applyKinds()` call site).

- [ ] **Step 1: Add the picker UI to the "Node kinds" section.** Replace the section body (~243-244) so the picker sits above the legend chips:

```html
      <div class="sec">
        <div class="sec-hd open" data-s="legend"><span class="chev">▶</span><span class="sec-lbl">Node kinds</span></div>
        <div class="sec-bd open" id="s-legend">
          <div class="row">
            <select id="kindSel" style="flex:1;padding:6px 8px;border-radius:var(--r2);border:1px solid var(--border2);background:#111115;color:var(--text);font-size:12px;outline:none;cursor:pointer">
              <option value="">Kind: (all) — pick to iterate</option>
            </select>
          </div>
          <div class="row"><button class="f" id="kindPrev" disabled>◀</button><button class="f" id="kindNext" disabled>▶</button><span class="hint" style="margin-left:auto"><span id="kindCount">—</span></span></div>
          <div class="legend" id="legend"></div>
        </div>
      </div>
```

- [ ] **Step 2: Add the kind-iterate JS.** Add after `stepSearch` (~778). The `KINDS` table already maps a CSS class (`nk-arith`) to a human label (`Arith`); build a class→label map and bucket the indexed nodes by `classify(nLabel)`:

```js
// ── kind picker + iterate ──────────────────────────────────────────────────
// Buckets nodes by the coarse category `classify()` already computes, and
// lets the user step through every node of a chosen category. Built once
// after each index (O(N), same pass cost as applyKinds).
const KIND_LABEL=new Map(KINDS.map(([,cls,lbl])=>[cls,lbl]));
let kindHits=[],kindIdx=-1;
function buildKindList(){
  const sel=document.getElementById("kindSel");
  // Count nodes per category.
  const counts=new Map();
  for(const[n] of nIdx.entries()){
    const c=classify(nLabel.get(n)||"");
    if(c)counts.set(c,(counts.get(c)||0)+1);
  }
  // Reset the <select> to just the "(all)" option, then add present kinds.
  sel.length=1;
  for(const[cls,cnt] of[...counts.entries()].sort((a,b)=>b[1]-a[1])){
    const o=document.createElement("option");
    o.value=cls;o.textContent=`${KIND_LABEL.get(cls)||cls} (${cnt})`;
    sel.appendChild(o);
  }
  kindHits=[];kindIdx=-1;updKindCounter();
}
function updKindCounter(){
  const el=document.getElementById("kindCount");
  if(el)el.textContent=kindHits.length?`${kindIdx+1}/${kindHits.length}`:"—";
  const dis=kindHits.length<2;
  document.getElementById("kindPrev").disabled=dis;
  document.getElementById("kindNext").disabled=dis;
}
function pickKind(cls){
  kindHits=[];kindIdx=-1;
  if(cls){
    for(const[n,g] of nIdx.entries())
      if(classify(nLabel.get(n)||"")===cls)kindHits.push({n,g});
  }
  updKindCounter();
  if(kindHits.length){kindIdx=0;selN(kindHits[0].n,kindHits[0].g,true);}
}
function stepKind(dir){
  if(!kindHits.length)return;
  kindIdx=(kindIdx+dir+kindHits.length)%kindHits.length;
  updKindCounter();
  const h=kindHits[kindIdx];selN(h.n,h.g,true);
}
```

- [ ] **Step 3: Build the list after indexing.** Find the call to `applyKinds()` (it runs once per render after `indexSvg`) and add `buildKindList();` immediately after it.

- [ ] **Step 4: Wire the controls.** Near the search button wiring (`document.getElementById("sfind")...` ~927-930), add:

```js
document.getElementById("kindSel").addEventListener("change",e=>pickKind(e.target.value));
document.getElementById("kindPrev").addEventListener("click",()=>stepKind(-1));
document.getElementById("kindNext").addEventListener("click",()=>stepKind(1));
```

- [ ] **Step 5: Verify structurally.** Run: `grep -c 'id="kindSel"' crates/dot/assets/graph_template_dot.html` → `1`; `grep -c 'function buildKindList' crates/dot/assets/graph_template_dot.html` → `1`. Then `cargo build -p dot` → success.

- [ ] **Step 6: Commit.**

```bash
git add crates/dot/assets/graph_template_dot.html
git commit -m "feat(viewer): coarse NodeKind picker with prev/next iteration"
git push origin rewrite/html-viewer
```

---

## Task 4: Fix minimap viewport-rect + click-to-pan

**Files:**
- Modify: `crates/dot/assets/graph_template_dot.html` — `getVisibleSvgRect` (~652-665), and verify `panToSvgPoint` (~563-571) / `buildMM` (~682-712) consistency.

**Root-cause hypothesis (to confirm via the browser):** `getVisibleSvgRect` maps the container's **screen-space** corners through the **non-screen** matrix `vp.getCTM()` (vp-local→svg-user). When svg-pan-zoom sizes the SVG to the container (so svg-user ≠ screen pixels), this scales/offsets the rect — the indicator drifts. The clone (`buildMM`) renders content in the original-viewBox frame (= vp-local space, viewport transform removed) and the `mini-vp` rect is in those same viewBox units, so the rect must be computed in vp-local space — which is exactly `vp.getScreenCTM().inverse()` applied to screen corners.

- [ ] **Step 1: Fix the matrix in `getVisibleSvgRect`.** Replace `vp.getCTM()` with `vp.getScreenCTM()` so screen corners map correctly into vp-local (viewBox) space, and degrade sanely when the matrix is missing:

```js
function getVisibleSvgRect(svg){
  if(!svg)return null;
  const vp=svg.querySelector(".svg-pan-zoom_viewport");
  if(!vp)return null;
  // vp.getScreenCTM(): vp-local (== the clone/viewBox frame, since the clone
  // removed the viewport transform) → screen. Invert it to map the container's
  // screen-space corners into vp-local space — the frame the mini-vp rect and
  // the cloned graph share.
  const m=vp.getScreenCTM();
  if(!m||m.a===0)return null;
  const r=gw.getBoundingClientRect();
  const inv=m.inverse();
  const p=(sx,sy)=>new DOMPoint(sx,sy).matrixTransform(inv);
  const tl=p(r.left,r.top);
  const br=p(r.left+r.width,r.top+r.height);
  return{x:Math.min(tl.x,br.x),y:Math.min(tl.y,br.y),w:Math.abs(br.x-tl.x),h:Math.abs(br.y-tl.y)};
}
```

- [ ] **Step 2: Confirm click-to-pan shares the frame.** `mm` click maps `clientX/Y` → clone-user via `clone.getScreenCTM().inverse()` (clone-user == viewBox == vp-local), then calls `panToSvgPoint(mainSvgRef, x, y)`, which maps `(x,y)` via `vp.getScreenCTM()` (vp-local→screen) and pans that screen point to centre. With Step 1, both rect and click use `vp.getScreenCTM()` ⇒ one consistent frame. No code change expected here; if the browser shows click-to-pan still off, instrument `panToSvgPoint` (log the screen point vs container centre) and reconcile against Step 1's frame.

- [ ] **Step 3: Generate a real graph and verify in a browser (user step).** Run:

```bash
cargo run -p strider-analyze --example orchestrator_demo
```

Open the produced `graph.html` (workspace root) in a browser. Confirm: (a) the minimap rectangle exactly frames the currently-visible region and tracks it while panning/zooming; (b) clicking a spot in the minimap recenters the main view on that spot. If either is off, capture the observed vs expected (screenshot or description) and iterate on the coordinate math (systematic-debugging: instrument the CTMs, compare frames).

- [ ] **Step 4: Commit.**

```bash
git add crates/dot/assets/graph_template_dot.html
git commit -m "fix(viewer): minimap viewport rect + click-to-pan use the screen CTM frame"
git push origin rewrite/html-viewer
```

---

## Task 5: Structural regression test + final verification

**Files:**
- Modify: `crates/dot/src/lib.rs` (test module — add a structural test)

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)]` module in `crates/dot/src/lib.rs` (the template is the private `HTML_DOT_TEMPLATE` const, accessible from the in-file test module):

```rust
#[test]
fn template_contains_new_viewer_controls() {
    // Guards that the viewer template keeps the controls the JS wires up:
    // edge-label field, node-name field, and the kind picker.
    let t = super::HTML_DOT_TEMPLATE;
    for id in ["sEdgeLabel", "kindSel", "kindPrev", "kindNext", "kindCount"] {
        assert!(t.contains(&format!("id=\"{id}\"")), "template missing id={id}");
    }
    assert!(t.contains("function buildKindList"), "template missing buildKindList");
}
```

- [ ] **Step 2: Run it.** Run: `cargo test -p dot template_contains_new_viewer_controls` → Expected: PASS (Tasks 1 & 3 added these). If it fails, the missing id names the gap — fix the template.

- [ ] **Step 3: Full gate.** Run:

```bash
cargo test -p dot
cargo clippy -p dot --all-targets -- -D warnings
```

Expected: all pass, clippy clean.

- [ ] **Step 4: Regenerate examples and confirm no breakage.** Run `cargo run -p strider-analyze --example orchestrator_demo`; confirm it writes `graph.html` / `graph-opt.html` / `cfg.html` without error. Open one and smoke-test all four features (select an edge → label shows; select a node → `nXX` shows; pick a kind → ◀/▶ iterate; minimap tracks + click-to-pan).

- [ ] **Step 5: Commit.**

```bash
git add crates/dot/src/lib.rs
git commit -m "test(dot): assert the viewer template keeps the new controls"
git push origin rewrite/html-viewer
```

---

## Self-review notes

- **Spec coverage:** #1 edge-label field → Task 1; #2 nXX node name → Task 1/2; #3 kind picker+iterate → Task 3; #4 minimap fix → Task 4; structural test + verification → Task 5. All covered.
- **No-Rust-change** holds except the Task 5 *test* (allowed — it's a guard, not viewer logic).
- **Scale/offline:** kind list built once per render (O(N), same as `applyKinds`); no new sweeps; minimap stays single-clone-per-render; nothing new fetched.
- **Type/name consistency:** IDs (`sEdgeLabel`, `kindSel`, `kindPrev`, `kindNext`, `kindCount`) and JS names (`buildKindList`, `pickKind`, `stepKind`, `updKindCounter`, `KIND_LABEL`, `kindHits`, `kindIdx`) are consistent across Tasks 3 and 5.
- **Known limitation:** Task 4's exact fix is hypothesis-driven; the browser step (4.3) is the real validator and may require one debugging iteration with the user's observations.
```
