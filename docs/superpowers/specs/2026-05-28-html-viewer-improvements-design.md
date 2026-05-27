# HTML graph-viewer improvements: edge labels, node names, kind picker, minimap fix

## Goal

Four usability improvements to the offline interactive IR graph viewer, all
confined to the single template `crates/dot/assets/graph_template_dot.html`
(HTML/CSS/JS). No Rust changes — the data each feature needs is already in
the rendered DOM. Offline (everything inlined, no network) and scalable to
thousands of nodes (no new O(N²) work; reuse the existing one-pass index,
CSS-driven dimming, and single-clone-per-render minimap).

## Background (how the viewer works today)

`graph_template_dot.html` is a self-contained viewer: the DOT source is
embedded as JSON and rendered to SVG **client-side** by the inlined
`viz-standalone.js` (Graphviz WASM); `svg-pan-zoom.min.js` (inlined) drives
pan/zoom. The Rust side (`crates/dot/src/lib.rs`) only substitutes the
inlined assets + DOT into the template placeholders.

Key facts established during exploration:

- Graphviz emits **no** `id`/`class`/`data-*`/`tooltip` per node/edge. The
  only per-element identity is the SVG `<title>` (the Graphviz node *name* —
  a render-local **serial integer** in the pretty renderer, e.g. `7`, *not*
  the IR `NodeId`) plus the `g.node` / `g.edge` class.
- Edges **do** carry a Graphviz `label=` (e.g. `lhs`, `rhs`, `cond`, `true`,
  `arg0`, `pred1`), which Graphviz renders as `<text>` inside the
  `g.edge` group. `labelOf(g)` (joins a group's `<text>`) already returns it;
  it currently lands in `#lblText` but not in a dedicated edge field.
- Precise `NodeKind` is **not** in the DOM. The JS derives a coarse category
  by regex over label text (`classify()` → `nk-arith`/`nk-mem`/…).
- Selection state is `sel={type,name,ekey,tail,head,el}`; sidebar fields
  `#sType`/`#sNode`/`#sEdge`/`#lblText` are filled by `updSelUI()`.
  Highlighting is CSS-driven off a single `gv-has-sel` root class; only the
  selected subset is tagged (constant work per selection).
- Search iterates nodes into `searchHits[]` with `searchIdx` and steps via
  `stepSearch(dir)` + `#sPrev`/`#sNext` + an `i/N` counter. This is the
  reusable "iterate" plumbing.
- The minimap is a **single clone** of the main SVG built once per render
  (`buildMM`), with a `<rect class="mini-vp">` indicator updated each frame
  by `mmTick` from `getVisibleSvgRect` (which inverts the live svg-pan-zoom
  viewport CTM). Click-to-pan maps the clicked clone point via
  `panToSvgPoint`.

## Feature designs

### 1. Edge label in a dedicated sidebar field

Add an "Edge label" row to the Selection panel (a `#sEdgeLabel` value next to
the existing `#sEdge` tail→head key). In `updSelUI()`, when the selection is
an edge, set it to the edge's label string via `labelOf(sel.el)`; blank it
(and/or hide the row) for node selections. The tail→head key stays in
`#sEdge`. No new data needed — the label `<text>` is already in the edge `<g>`.

### 2. Node name "nXX" in the sidebar

When a node is selected, show a name like `n14` (where `14` is the node's
serial — the `<title>`). Implementation: format the `#sNode` value as
`n${sel.name}` for node selections. This is a stable per-render identifier;
the pretty renderer does not preserve the IR `NodeId`, and the user explicitly
accepted "some indication of it, like `n14`". (The raw renderer already uses
the real `NodeId` as the serial, so there `nXX` *is* the IR id — a free bonus,
no special-casing.)

### 3. NodeKind picker + iterate

New collapsible "Kind" sidebar section containing:
- a `<select id="kindSel">` populated **once** after indexing, listing only
  the coarse categories present in this graph (derived from the same
  `classify()` buckets already computed by `applyKinds()`), each with a count,
  e.g. `arith (42)`, `mem (8)`.
- ◀ / ▶ buttons + an `i/N` counter (`#kindCount`), mirroring the search
  controls.

Picking a kind builds that kind's node list once (filter the existing
node index by category — O(N), user-triggered), resets the index to 0,
selects+zooms the first, and ◀/▶ cycle through them (reusing the
`stepSearch`-style modular stepping). Selecting a node from elsewhere does not
disturb the kind list. The category set is computed from `nLabel` via the
existing `classify()`, so no Rust change and no new DOM data.

### 4. Minimap viewport-rect + click-to-pan fix

**Symptom:** the viewport-indicator rectangle is offset / drifts and
click-to-pan lands in the wrong place. **Root cause:** a coordinate-frame
mismatch — the visible-region rect is computed by inverting the live
svg-pan-zoom viewport CTM (`getVisibleSvgRect`), but the clone renders the
content in the original-viewBox frame with the Graphviz `g#graph0`
translate/scale unaccounted-for, and the indicator rect is appended at the
clone root rather than in the content frame. When the Graphviz graph group
carries a non-identity transform (it normally does: `scale(s) translate(tx ty)`)
or the viewBox origin is non-zero, the rect and clicks misregister.

**Fix approach (to be pinned down via systematic-debugging against a real
rendered SVG):** compute the visible-region rectangle and the click-to-pan
inverse in the *same* coordinate frame the minimap clone actually paints —
i.e. account for the `g#graph0` content transform and the viewBox origin so
the `mini-vp` rect maps 1:1 onto the cloned graph, and a minimap click maps
back to the correct main-view pan target. Keep the single-clone-per-render
design (no re-clone on pan) and the rAF indicator update. Secondary hardening:
the `m.a===0` null path in `getVisibleSvgRect` currently freezes the rect —
make it degrade sanely; surface a parse failure instead of a silently-empty
minimap.

Because the exact transform chain only exists at runtime (Viz renders in the
browser; svg-pan-zoom's CTM is runtime-only), the fix will be derived from the
actual rendered SVG transforms (rendering the DOT→SVG via the vendored Viz in
Node where feasible to read the real `g#graph0` transform) and made
correct-by-construction, with final interactive confirmation by the user.

## Components touched

Only `crates/dot/assets/graph_template_dot.html`:
- HTML: new sidebar rows/section (`#sEdgeLabel`, the Kind section with
  `#kindSel`/`#kindPrev`/`#kindNext`/`#kindCount`).
- CSS: styling for the new rows/section (reusing existing classes).
- JS: `updSelUI` (edge label + `nXX` node name), a kind-iterate module
  (build list / step / counter), and the minimap coordinate fix in
  `buildMM` / `getVisibleSvgRect` / `panToSvgPoint` / `mmTick`.

Plus a `crates/dot` test asserting the generated HTML contains the new
controls and is well-formed.

## Testing / verification

- **Headless (CI-able):** a `dot`-crate test renders a small graph to HTML and
  asserts the new element IDs (`#sEdgeLabel`, `#kindSel`, …) are present and
  the document is structurally well-formed; regenerate a real example
  (`orchestrator_demo`) and check structure. This covers the *presence* of
  #1/#2/#3 wiring.
- **Pure-logic JS:** keep the kind-bucketing and `nXX`/edge-label formatting as
  small pure helpers so they're inspectable; assert their outputs where a
  string-level check is possible from the generated HTML.
- **Browser (user sign-off):** the minimap rect tracking, click-to-pan
  accuracy, and the interactive iterate/selection behavior require a real
  browser (svg-pan-zoom CTM is runtime-only). I generate the HTML; the user
  confirms the interactive behavior. This split was explicitly agreed.

## Constraints (must hold)

- **Offline:** no new network/CDN; everything stays inlined.
- **Scale:** no new per-node/per-edge sweeps beyond the existing O(N) index
  and user-triggered O(N) filters; dimming stays CSS-driven; minimap stays
  single-clone-per-render. The kind list is built once per pick.

## Out of scope

- Emitting real IR `NodeId` / precise `NodeKind` from Rust (the user accepted
  the serial-based `nXX` and coarse categories; a Rust-emitted-metadata
  upgrade is a possible future enhancement, not this change).
- Edge-label on-graph overlay (sidebar field only).
