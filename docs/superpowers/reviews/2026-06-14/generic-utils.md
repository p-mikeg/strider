# Deep audit: generic utility crates (read-only)

Date: 2026-06-14
Scope: `dot`, `entity-utils`, `graphwalk`, `strider-ir-test-utils`
Method: code-vs-itself + real call-path verification across the workspace.
CLAUDE.md / doc-comments treated as suspect, not ground truth.

## Summary

These four crates are in strong shape: invariants hold, traversals are
O(V+E), the dedup/interner contracts are correct, and the DOT/HTML
escaping is layered and well-tested. No HIGH findings. The findings below
are LOW/MED: a small client-side `innerHTML` error sink, a couple of
unused generic-API surfaces, and a doc/heuristic imprecision. Edge-case
test coverage is unusually thorough; the few genuine gaps are named.

Severity counts: HIGH 0 · MED 1 · LOW 5

---

## MED-1 — `innerHTML` interpolation of render-error text in the HTML viewer

- Crate: `dot`
- Dimension: SOUNDNESS (escaping / injection)
- Severity: MED  · Confidence: HIGH
- Location: `crates/dot/assets/graph_template_dot.html:986`

What & why: every other DOM write in the viewer uses `textContent`
(verified: lines 318/321, 364–423, 769–825, etc.) and the DOT payload is
embedded as JSON and parsed (`JSON.parse(document.getElementById("dot-src").textContent)`,
line 289). The single exception is the render-failure path:

```js
gw.innerHTML=`<div ...>Render failed.\n\n${err}</div>`;
```

`err` is the exception thrown by `viz.renderSVGElement(dot, …)` (line
963). Its message is derived from the DOT source, which originates from
caller-supplied node labels. Labels are escaped for DOT (`escape_dot_label`)
and for JSON (`json_quote`), but neither escapes for HTML, and the Viz/
Graphviz error string can echo fragments of the offending input back. An
attacker who controls a node label and can induce a parse error could get
markup into the page via this `innerHTML`. Impact is bounded — these are
local, self-contained debug HTML files, not a served app — hence MED not
HIGH, but it is the one real escaping inconsistency in an otherwise
careful crate.

Proposed fix: build the error node with `textContent` instead of
`innerHTML` (create a `<div>`, set `style`, set `.textContent = "Render
failed.\n\n" + err`). Removes the only unescaped sink with no behaviour
change.

---

## LOW-1 — Unused generic API on `EntityInterner`

- Crate: `entity-utils`
- Dimension: simplify / dead surface
- Severity: LOW · Confidence: HIGH
- Location: `crates/entity-utils/src/interner.rs:54,59,69,74,79`

What & why: the only production consumers are `Function::wide_const_interner`
and `FunctionBuilder::var_table` (verified in `strider-ir/src/function/data.rs`
and `builder/vars.rs`). Across the whole workspace they call only
`intern`, `get`, `Index`, `len`, and `key_of`. The methods `contains`,
`is_empty`, `keys`, `values` are exercised only by the crate's own unit
tests — no production caller. (`len` *is* used, so `is_empty` is the lone
length-pair half that is dead.)

Proposed fix: this is a deliberately generic helper crate, so keeping a
rounded API is defensible; but if minimalism is preferred, drop
`contains` / `is_empty` / `keys` / `values` (and their tests) until a
caller needs them. At minimum, note that they exist for completeness, not
because anything calls them.

---

## LOW-2 — `Worklist::contains` and `NopTracker` are test-only

- Crate: `entity-utils`, `graphwalk`
- Dimension: simplify / dead surface
- Severity: LOW · Confidence: HIGH
- Location: `crates/entity-utils/src/worklist.rs:38`; `crates/graphwalk/src/lib.rs:99-107`

What & why: `Worklist::contains` is referenced only inside
`worklist.rs`'s own `#[cfg(test)]` block — no production or
cross-crate caller (verified by grep over `crates/**.rs`). `NopTracker`
is referenced only by `graphwalk/tests/postorder.rs::nop_tracker_on_a_tree`
— no production consumer anywhere. Both are small and generic; `NopTracker`
in particular is a legitimate building block for tree walks.

Proposed fix: keep `NopTracker` (cheap, correct, documents the
tree-only path). Consider dropping `Worklist::contains` unless retained
intentionally as a mirror of `HashSet::contains`; if kept, the test that
covers it is its only justification, which is worth a one-line note.

---

## LOW-3 — `DenseEntitySet::with_capacity` and `FromIterator` are unused outside the crate

- Crate: `entity-utils`
- Dimension: simplify / dead surface
- Severity: LOW · Confidence: MED
- Location: `crates/entity-utils/src/set.rs:24,127`

What & why: workspace-wide, sets are built via `DenseEntitySet::new()`
then `.insert`/`.collect()` through the `VisitTracker` path; no caller
uses `with_capacity` and no `.collect::<DenseEntitySet<_>>()` exists in
production (the `.collect()` sites in `strider-opt` collect into
`Worklist`/`Vec`, verified). The `FromIterator` impl is sound (it dedups,
covered by `from_iter_dedups`). Confidence MED because these are exactly
the kind of API a future caller reaches for, so removal could be churn.

Proposed fix: keep as generic-crate ergonomics; no action required beyond
awareness. Do not treat as a bug.

---

## LOW-4 — `dot_node_count` over-counts labels containing the literal `[label=`

- Crate: `dot`
- Dimension: runtime / correctness (heuristic)
- Severity: LOW · Confidence: HIGH
- Location: `crates/dot/src/lib.rs:332-334`

What & why: `dot.matches("[label=").count()` counts substrings, not node
statements. A node label that contains the text `[label=` (legal after
escaping — `escape_dot_label` does not alter `[`, `=`) inflates the count.
The doc comment already concedes this is "approximate" and only drives the
initial layout-engine choice, so the consequence is at worst an early
switch to `sfdp` — harmless. This is verified behaviour, not a regression;
flagged only so the imprecision is on record and not mistaken for an exact
node count by a future caller.

Proposed fix: none required. If exactness ever matters, count node
statements during emission in `DotEmitter::node` rather than re-scanning
the finished string.

---

## LOW-5 — `PreOrderContext`/`PostOrderContext` public but only the type-aliased forms are used

- Crate: `graphwalk`
- Dimension: simplify / API surface
- Severity: LOW · Confidence: MED
- Location: `crates/graphwalk/src/lib.rs:121,235` (and `reset`/`next`/`next_event`)

What & why: production code (`strider-ir/src/walk/mod.rs`) only goes
through `entity_preorder` / `entity_postorder` and the `PreOrder` /
`PostOrder` iterator type aliases. The raw `*Context` structs and their
`reset`/`next`/`next_event` methods are exercised only by graphwalk's own
tests. They are correct and arguably the right reusable primitive (the
`reset` root-order contract is carefully documented and pinned by tests),
so this is informational, not a removal recommendation.

Proposed fix: none; documented here so the surface is understood as
"primitive kept for reuse," not accidental.

---

## Soundness verifications that PASSED (no finding)

- `Worklist`: `worklist` (deque) and `workset` (set) only co-mutate in
  `enqueue`/`dequeue`/`clear`; cannot drift. `len()` reads the deque
  (O(1)), not the bitset. Enqueue dedup is single-pass
  `if workset.insert {push}` (O(1)); pinned at 10k scale by
  `enqueue_dedup_at_ten_thousand_scale`. Re-enqueue-after-dequeue allowed
  and tested.
- `EntityInterner`: forward `PrimaryMap` + reverse `FxHashMap` mutated
  only in `intern`, in lockstep; idempotent; one clone per genuinely-new
  value. `Index` panic path documented. Verified vs both real consumers.
- `DenseEntitySet`: `insert` returns the correct newly-inserted bool;
  `with_capacity(0)` valid; `clear`+reinsert valid; iter is fused and
  ascending. `len`/`is_empty` correctly documented as O(max_index/64).
- `graphwalk`: pre/post order correct on empty roots, single node,
  self-loop, cycles, disconnected multi-root, repeated successor, and
  duplicate root — all pinned by tests in `preorder.rs`/`postorder.rs`.
  The "skip already-visited before push" optimisation is correct (re-check
  on pop). `NopTracker` correctness is tree-only and the test says so.
  RPO root-order contract holds in both directions.
- `dot` escaping: `escape_dot_label` (DOT quotes) and `json_quote`
  (JSON + unconditional `<` → `<` to prevent `</script>` breakout)
  are both correct and exhaustively tested, including empty input, control
  chars, the 0x20 boundary, high Unicode, and script-breakout. The DOT
  digraph name and node/edge ids are all escaped. The `extra` verbatim
  attribute contract (caller quotes the value) is documented at both
  `node` and `edge`.
- `strider-ir-test-utils`: test-support, not dead. `Tb`/`RegisterSet`
  helpers all reach real `FunctionBuilder` APIs; no duplication of
  builder *logic* (they are thin `.expect()` wrappers, appropriate for
  test code). `MockRom` consolidates several former bespoke ROMs.
  `RegisterSet` uses struct-literal CC construction to deliberately skip
  ABI validation for degenerate fixtures — intentional and documented.

## Named edge-case test gaps (do NOT write — for follow-up)

1. `entity-utils`/interner: `intern_reverse_collision_distinct_keys` —
   intern two distinct values whose `V::hash` collides (a wrapper type
   with a degenerate `Hash`) and assert they get distinct keys and both
   resolve. Current tests never force a reverse-map collision; the impl is
   correct (FxHashMap handles it) but it is unpinned.
2. `graphwalk`: `nop_tracker_on_a_dag_double_visits` — document, via a
   test, that `NopTracker` on a non-tree DAG yields a node more than once
   (the contract is "tree-only"). The only NopTracker test uses a tree, so
   the misuse boundary is unpinned.
3. `dot`: `as_html_from_dot_label_with_literal_bracket_label_substring` —
   a node whose label contains `[label=` to pin the LOW-4 over-count
   behaviour (so a future "fix" doesn't silently change engine selection).
4. `dot`: a `GraphDotDumper` whose `dump_as_dot` returns `Err` to pin that
   `render_dot_string` wraps it as `anyhow!("dot dump error: {e}")` and
   propagates — the error path of `as_dot`/`dump_as_html` is currently
   untested.
