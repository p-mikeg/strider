# Round 13 — Simplifications Sweep (Emphasis B)

Honest scan after Round 12 W4.  Surface is largely cleaned up; small set
of genuine remaining wins.

**Per-category LOC delta:**

| Cat | Name                                | Findings | Est. LOC delta |
| --- | ----------------------------------- | -------- | -------------- |
| 1   | Delete dead code                    | 7        | ~ −55          |
| 2   | Merge similar code                  | 1        | ~ −30          |
| 3   | Inline single-callsite helpers      | 0        |     0          |
| 4   | Stdlib idioms                       | 0        |     0          |
| 5   | Tighten visibility                  | 4        |  ±0 (textual)  |
| 6   | Drop redundant wrappers             | 0        |     0          |
| 7   | Collapse partial-state types        | 0        |     0          |

Net LOC: ≈ −85.

---

## Cat 1 — Delete dead code

### 1.1  `Capture::from_id` — unused public ctor

- `crates/pattern/src/var.rs:67-70` (fn + ~10 lines doc).
- Doc claims "used by PyO3 bindings to revive a Capture across FFI" —
  zero callers anywhere.  The binding uses `Capture::new()` + interning,
  never reconstructs.
- Net LOC: ≈ −14.

### 1.2  `BuiltFunctionGraph::set_call_clobbered_for_test` — unused

- `crates/ir/src/function.rs:169-171`.  No callers.  Only
  `set_call_other_clobbered_for_test` is exercised (pattern tests).
- Net LOC: ≈ −5.

### 1.3  `BuiltFunctionGraph::set_ret_val_regs_for_test` — unused

- `crates/ir/src/function.rs:175-177`.  No callers.
- Net LOC: ≈ −5.

### 1.4  `BuiltFunctionGraph::ret_val_regs_as_slice` — unused accessor

- `crates/ir/src/function.rs:135-138`.  Documented at line 78 as the
  official read path for the `pub(crate)` field; zero call sites
  workspace-wide.
- Net LOC: ≈ −5.

### 1.5  `BuiltFunctionGraph::no_memory_clobber()` accessor — unused

- `crates/ir/src/function.rs:145-152`.  The only readers go through the
  field directly inside the IR crate; the public accessor has zero
  callers.
- Net LOC: ≈ −8.

### 1.6  `RewriteCtxView::new` — unused public ctor

- `crates/pattern/src/rewrite.rs:248-251`.  Documented at line 239-240
  as one of three ctor paths.  Zero call sites — every caller uses
  `From<&BuiltFunctionGraph>` or `RewriteCtx::as_view()`.
- Net LOC: ≈ −4.

### 1.7  Stale `#[allow(dead_code)]` attributes + clippy items

Three `#[allow(dead_code)]` claim items are unused but they have many
callers:

- `crates/strider-py/src/graph.rs:55` — `read_inner` (5 callers).
- `crates/strider-py/src/graph.rs:64` — `write_inner` (1 caller).
- `crates/strider-py/src/errors.rs:38` — `into_strider_err` (~32 callers).
- `crates/strider-py/src/errors.rs:72` — `into_lift_err` (used).

Plus clippy-confirmed dead items:

- `crates/ir/src/node_signature.rs:283-288` — macro rule #2 of `sig!`
  is never used (the `out_tail:` without `in_tail:` form).  Clippy
  `-W unused` flags it.  Net: −6.
- `crates/opt/src/lib.rs:48` — `extern crate self as opt;` is unused.
  Clippy `-W unused` flags it.  Net: −1.

Net for 1.7: ≈ −11 (4 stale attrs + 7 LOC clippy items).

---

## Cat 2 — Merge similar code

### 2.1  `inner.read() … "Graph lock poisoned"` boilerplate

- `crates/strider-py/src/graph.rs` — the literal "Graph lock poisoned"
  string appears 14 times.  The pattern

      let graph = self
          .inner
          .read()
          .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;

  repeats verbatim in `to_html`, `to_dot`, `html_str`, `node_count`,
  `count_loop_headers`, `node_ids`, `node_kind`, `asm_fingerprint`,
  `wide_const_bytes`, `call_other_name`, `validate`, and 3 more bodies.
  A helper `read_inner()` already exists at line 56 — sites could route
  through `read_inner().map_err(into_strider_err)?`.
- Not load-bearing-different: every site is the same guard with the
  same error message.
- Net LOC: ≈ −30 (10+ sites × ~3 lines).
- Status: **NEW** — Round 12 W4's `read_inner` work only touched the
  `try_write_inner` path.

---

## Cat 3 — Inline single-callsite helpers

No findings.  Round 12 W12/W14 swept these.

## Cat 4 — Replace bespoke patterns with stdlib idioms

No findings.  Searched for `.iter().filter().filter()`, manual
`None => return None`, `.collect::<Vec<_>>().len()`, etc.  None
present in touched surface.

---

## Cat 5 — Tighten visibility

### 5.1  `ir::walk::{cfg_outputs, cfg_succs, graph_walk_succs, GraphWalkSuccs}`

- `crates/ir/src/walk.rs:52, 78, 87, 95`.
- Zero external (`ir::walk::cfg_*` or `walk::cfg_*`) callers.  All four
  consumed only by `cfg_reachable` and the impl of
  `GraphWalkSuccs::try_successors`.  All could be `pub(crate)`.

### 5.2  `opt::sp_expr` module + `ranges_disjoint` / `decompose_sp`

- `crates/opt/src/lib.rs:52` (`pub mod sp_expr;`) and `sp_expr.rs:64, 263`.
- Zero external callers outside the opt crate.  Both fns consumed only
  by sibling opt passes.  Drop to `pub(crate)` + private `mod`.

### 5.3  `opt::stack_load_forward` module

- `crates/opt/src/lib.rs:64`.  Only the `pub use ... StackLoadForward`
  needs visibility; no external `opt::stack_load_forward::*` paths.
  `find_stack_stored_value_at_offset` is already `pub(crate)`.

### 5.4  `opt::indirect_branch_resolve` module

- `crates/opt/src/lib.rs:60`.  Grepped for `opt::indirect_branch_resolve::`
  — only doc-comment references, no live `use ...` paths.  The seven
  needed symbols already re-exported at crate root.

5.1–5.4 apply together as one tightening commit; textual only.

---

## Cat 6 — Drop redundant wrappers

No findings.  Newtypes (`NodeId`, `NodeOutputId`, `RegionId`, `VarId`,
`WideConstId`, `Capture`) all carry type-safety invariants.
`GraphWalkSuccs<'_>(&Graph)` required by `graphwalk::GraphRef` impl.

## Cat 7 — Collapse partial-state types

No findings.  Round 11 W14 already collapsed `cfg::FunctionBoundary`.
`FunctionGraph::new_invalid()` is `pub(crate)`, builder-only, not
externally observable.

---

## Recommendation

Highest leverage as a single commit: **Cat 2.1** (strider-py read-guard
merge, ~−30 LOC, large readability win on a frequently-edited file).
Everything else is dead-code/visibility hygiene — batch as one tidy
commit.

Round 13 confirms the Round 12 W4 pattern: small real tail, no big
surprises.
