# Round 10-3B — Stale Comment Sweep

Scope: every comment block that names a deleted symbol, references a closed
task, describes behaviour the surrounding code doesn't implement, cites stale
line numbers, half-rewrites a comment, or contains a broken cross-reference.
Source verified against current `feature/ai` tree at `c7a2903`.

Findings are grouped by category. Severity grading:
- **HIGH** — actively misleads a reader (claims a non-existent symbol or the
  opposite of current behaviour).
- **MED**  — partly accurate; a careful reader will be confused for a few
  minutes before realising what's stale.
- **LOW**  — cosmetic / clutter (Round-9-wave-N prefix, etc.).

────────────────────────────────────────────────────────────────────────

## A. Deleted symbols (`opt::with_built`)

`opt::with_built` was renamed to `opt::with_rewrite_ctx` (round 9 wave 28).
Four comment sites still name the old function. None of these will fail a
build — the names appear inside `///` doc strings, not as code references —
but each one points a reader at a non-existent symbol.

### A1. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` doc names `opt::with_built` twice
- **Severity:** HIGH
- **Where:** `crates/ir/src/function.rs:153` and `crates/ir/src/function.rs:173`
- **Comment text (excerpt):**
  > "Used by `opt::with_built` and `strider::rewrite::GraphRewriter` to bridge
  > `(&mut Graph, NodeId)` callers to `&mut BuiltFunctionGraph`-typed helpers
  > (the `pattern` crate's rewrite machinery is typed against
  > `BuiltFunctionGraph`)."
  > … "the two production callers (`opt::with_built` and `compact`'s test
  > fixture) consume `&mut BuiltFunctionGraph` because their downstream traits
  > (`OptimizerOnBuilt::optimize_built`, `BuiltFunctionGraph::compact`) take
  > that type."
- **Issue:** `opt::with_built` no longer exists — `crates/opt/src/pipeline.rs:85`
  defines `with_rewrite_ctx` instead. Worse, the claim "the `pattern` crate's
  rewrite machinery is typed against `BuiltFunctionGraph`" is **false**:
  `pattern::rewrite_rule` returns a closure typed
  `Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>`
  (`crates/pattern/src/rewrite.rs:46`), and
  `OptimizerOnBuilt::optimize_built` takes `&mut pattern::RewriteCtx<'_>`
  (`crates/opt/src/pipeline.rs:156-159`). The doc invites a reader to grep
  for symbols that do not exist and contradicts the actual signatures.
- **Verified against:**
  - `grep -rn "opt::with_built" crates` → only the four stale doc sites.
  - `grep -n "fn with_rewrite_ctx" crates/opt/src/pipeline.rs` → `:85`.
  - `crates/opt/src/pipeline.rs:156-159` (`optimize_built` takes
    `&mut pattern::RewriteCtx<'_>`).
- **Fix:** Replace `opt::with_built` with `opt::with_rewrite_ctx`. Reword the
  bridge claim to say the partial-state ctor is **kept for tests and the
  short-lived `RewriteCtx::for_built` constructor in `pattern`**, since
  `OptimizerOnBuilt::optimize_built` now takes `&mut RewriteCtx`. Drop the
  "production callers" wording — the only live callers are tests and a
  `BuiltFunctionGraph::compact` test fixture.

### A2. `OptimizerPipeline::with_rewrite_ctx` adapter doc names `with_built`
- **Severity:** LOW
- **Where:** `crates/opt/src/pipeline.rs:75-84`
- **Comment text (excerpt):**
  > "Bridge `(&mut Graph, NodeId)` callers to a `&mut RewriteCtx`-typed
  > closure. Round 9 wave 28 (H-9/D2): replaces the previous `with_built`
  > adapter that constructed a partial-state `BuiltFunctionGraph` via
  > `from_graph_and_entry_for_rewrite`."
- **Issue:** Historical reference. `with_built` is named correctly as a past
  symbol — the comment is doing its job here. Borderline cosmetic but kept
  for clarity since the rest of the wording is accurate.
- **Fix:** No action required.

### A3. `pattern::RewriteCtx::new` doc claims `opt::with_built` is a caller
- **Severity:** HIGH
- **Where:** `crates/pattern/src/rewrite.rs:160-162`
- **Comment text:**
  > "Constructs a `RewriteCtx` from a raw `(graph, entry)` pair —
  > the rewrite-only path used by `opt::with_built`,
  > `strider::rewrite::GraphRewriter::apply_rule`, and similar."
- **Issue:** Names a deleted symbol as a current caller.
- **Verified against:** `grep -rn "opt::with_built"` → only stale docs.
  Actual caller is `opt::with_rewrite_ctx` at `crates/opt/src/pipeline.rs:90`.
- **Fix:** Rename `opt::with_built` to `opt::with_rewrite_ctx`.

### A4. `GraphRewriter::graph` field doc names `opt::with_built`
- **Severity:** HIGH
- **Where:** `crates/strider/src/rewrite.rs:60-65`
- **Comment text:**
  > "The graph to rewrite. Held as `&mut Graph` rather than
  > `&mut BuiltFunctionGraph` to align with the optimizer pass contract
  > `(&mut Graph, NodeId)`. `pattern::rewrite_rule`'s closure expects
  > `&mut BuiltFunctionGraph`, so `Self::apply_rule` swaps the graph into
  > a short-lived `BuiltFunctionGraph` per call (via `mem::take` — same
  > trick as `opt::with_built`)."
- **Issue:** Two compounding lies in a single sentence:
  1. `pattern::rewrite_rule`'s closure does **not** expect
     `&mut BuiltFunctionGraph`. It expects `&mut RewriteCtx<'g>`
     (`crates/pattern/src/rewrite.rs:46`).
  2. `Self::apply_rule` does **not** swap the graph via `mem::take`.
     Verified — `grep -n "mem::take" crates/strider/src/rewrite.rs` returns
     hits **only inside doc comments**, never in code. The actual body
     (`crates/strider/src/rewrite.rs:117-138`) builds a `RewriteCtx::new`
     directly. Plus `opt::with_built` doesn't exist.
- **Verified against:** `crates/strider/src/rewrite.rs:117-138` body uses
  `pattern::RewriteCtx::new(&mut *self.graph, self.entry)` — no `mem::take`,
  no `BuiltFunctionGraph`.
- **Fix:** Rewrite the field doc to say the field is `&mut Graph` because
  `pattern::rewrite_rule` returns a closure typed against
  `&mut RewriteCtx<'g>`, and `apply_rule` constructs that `RewriteCtx`
  per node from the wrapped `(graph, entry)` pair. Delete the `mem::take`
  / `opt::with_built` clauses entirely.

### A5. `GraphRewriter::apply_rule` doc repeats the same `mem::take` claim
- **Severity:** HIGH
- **Where:** `crates/strider/src/rewrite.rs:86-94`
- **Comment text:**
  > "The closure shape matches what `pattern::rewrite_rule` hands back —
  > `Fn(&mut BuiltFunctionGraph, NodeId) -> pattern::Result<bool>`.
  > Wraps the wrapped graph into a short-lived `BuiltFunctionGraph` per
  > call (via `mem::take`) so the closure has the input shape the
  > `pattern` crate's rewrite engine was designed for. The dummy
  > `BuiltFunctionGraph` carries empty `variables` / `call_clobbered` /
  > `ret_val_regs` — `pattern::rewrite_rule` only touches `graph` and
  > `entry`, verified by inspection of `pattern::rewrite_rule`'s
  > implementation."
- **Issue:** Same as A4 — the closure type is `Fn(&mut RewriteCtx<'g>, NodeId)`
  per `crates/pattern/src/rewrite.rs:46`. The body builds a `RewriteCtx`
  directly; there is no dummy `BuiltFunctionGraph` and no `mem::take`.
- **Verified against:** `crates/strider/src/rewrite.rs:117-138` and
  `crates/strider/src/rewrite.rs:119` `where F: for<'g> Fn(&mut pattern::RewriteCtx<'g>, NodeId) -> pattern::Result<bool>`.
- **Fix:** Replace the entire paragraph with: "The closure shape matches what
  `pattern::rewrite_rule` hands back — `for<'g> Fn(&mut RewriteCtx<'g>, NodeId)
  -> pattern::Result<bool>`. `apply_rule` constructs a fresh
  `RewriteCtx::new(graph, entry)` per candidate root."

────────────────────────────────────────────────────────────────────────

## B. Half-rewritten comments

### B1. "migrated from `&mut RewriteCtx` to `&mut RewriteCtx`" (typo / no-op)
- **Severity:** HIGH
- **Where:** `crates/opt/src/pipeline.rs:136-137`
- **Comment text:**
  > "Round 9 wave 28 (H-9/D2): parameter type was migrated from
  > `&mut pattern::RewriteCtx<'_>` to `&mut pattern::RewriteCtx<'_>`."
- **Issue:** "From X to X" — both ends of the migration name the same type.
  Almost certainly the original draft was "from `&mut ir::BuiltFunctionGraph`
  to `&mut pattern::RewriteCtx<'_>`" and one half was over-replaced. The
  surrounding paragraph confirms the intended meaning ("…the rewrite-only
  context is sufficient. `RewriteCtx` provides `Deref<Target=Graph>`…
  so existing pass bodies that say `function.node_kind(_)` … work
  unchanged"), which only makes sense if the prior type was the BFG.
- **Verified against:** Same paragraph context + the matching adapter doc at
  `crates/opt/src/pipeline.rs:75-84` which says it "replaces the previous
  `with_built` adapter that constructed a partial-state `BuiltFunctionGraph`."
- **Fix:** Change the second `&mut pattern::RewriteCtx<'_>` to
  `&mut ir::BuiltFunctionGraph` (or, better, just delete the migration note
  wholesale — the adapter at line 75 already explains the change).

### B2. `RewriteCtx::preorder` doc describes a future migration that already happened
- **Severity:** MED
- **Where:** `crates/pattern/src/rewrite.rs:178-183`
- **Comment text:**
  > "Round 9 wave 26 (H-9/D2 groundwork): pre-order graph walk starting at
  > `Self::entry`. Mirrors `BuiltFunctionGraph::preorder` so a future
  > migration of `OptimizerOnBuilt::optimize_built` from
  > `&mut BuiltFunctionGraph` to `&mut RewriteCtx` can switch the parameter
  > type without rewriting every pass body that calls `function.preorder()`."
- **Issue:** "a future migration … from `&mut BuiltFunctionGraph` to
  `&mut RewriteCtx`" — but the migration is **already done** at
  `crates/opt/src/pipeline.rs:158` (`fn optimize_built(&self,
  function: &mut pattern::RewriteCtx<'_>) -> …`). The "future" framing is
  stale; it should now read "Provides the same `preorder()` shape that
  `BuiltFunctionGraph` exposes, so pass bodies migrated by wave 28 keep
  the same call-site syntax."
- **Verified against:** `grep -A3 "fn optimize_built" crates/opt/src/pipeline.rs`.
- **Fix:** Reword to past tense; drop "a future migration … can switch".

────────────────────────────────────────────────────────────────────────

## C. Behaviour drift

### C1. `RegionTerminator::Switch` doc: "reserved for the future jump-table resolver"
- **Severity:** HIGH
- **Where:** `crates/cfg/src/cfg/types.rs:119-121`
- **Comment text:**
  > "`Switch` is **reserved** for the future jump-table resolver and is not
  > constructed by the cfg builder today — it is part of the API so that
  > adding jump-table support is a purely additive change."
- **Issue:** Directly contradicts the variant's own doc 30 lines below (line
  153-156: "Constructed by the cfg builder from a `ResolvedTargets::Multiple`
  resolution"), and the cfg builder calls
  `self.finish_current_region(RegionTerminator::Switch { … })` at
  `crates/cfg/src/cfg/builder/region_builder.rs:508`. Jump-table support
  has landed; the variant is no longer "reserved".
- **Verified against:**
  - `grep -rn "RegionTerminator::Switch" crates/cfg/src` →
    `crates/cfg/src/cfg/builder/region_builder.rs:508` constructs the variant.
  - `crates/cfg/src/cfg/types.rs:153-160` describes the cfg builder
    construction path.
- **Fix:** Replace the "reserved" sentence with: "`Switch` is constructed by
  the cfg builder when the strider orchestrator's IR-level indirect-branch
  resolver feeds back a `ResolvedTargets::Multiple` resolution via
  `known_targets` — see the variant doc below for details." Or just delete
  the paragraph; the per-variant doc already explains the construction.

### C2. `pattern::rewrite_rule` doc claims closure takes `&mut BuiltFunctionGraph`
- **Severity:** HIGH
- **Where:** `crates/pattern/src/rewrite.rs:13-19`
- **Comment text:**
  > "Build a rewrite-rule closure from an LHS and RHS `Pat`.
  > The returned closure takes `&mut BuiltFunctionGraph` and a candidate root
  > `NodeId`, attempts the match, and on success materializes the RHS template
  > via `Pattern::try_build` and redirects the root's value output to the
  > built output via `BuiltFunctionGraph::replace_all_uses`."
- **Issue:** The function signature on line 43-46 is
  `-> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync + 'static`.
  The closure takes `&mut RewriteCtx<'g>`, not `&mut BuiltFunctionGraph`.
  Also `replace_all_uses` is a method on `Graph`, not `BuiltFunctionGraph`
  (`crates/ir/src/ops/rewrite.rs:18`); the cross-reference
  `[BuiltFunctionGraph::replace_all_uses]` does not resolve.
- **Verified against:** `crates/pattern/src/rewrite.rs:43-46` (signature),
  `crates/ir/src/ops/rewrite.rs:7-22` (`impl Graph { fn replace_all_uses }`).
- **Fix:** "The returned closure takes `&mut RewriteCtx<'g>` and a candidate
  root `NodeId` … redirects the root's value output to the built output via
  `Graph::replace_all_uses`."

### C3. `from_graph_and_entry_for_rewrite` doc claims `OptimizerOnBuilt::optimize_built` takes `&mut BuiltFunctionGraph`
- **Severity:** HIGH
- **Where:** `crates/ir/src/function.rs:175-176`
- **Comment text (excerpt):**
  > "…their downstream traits (`OptimizerOnBuilt::optimize_built`,
  > `BuiltFunctionGraph::compact`) take that type."
- **Issue:** `OptimizerOnBuilt::optimize_built` takes
  `&mut pattern::RewriteCtx<'_>` since round 9 wave 28
  (`crates/opt/src/pipeline.rs:156-159`). Only `BuiltFunctionGraph::compact`
  takes `&mut BuiltFunctionGraph` now. The "Migrating would require changing
  those trait signatures across every opt pass — multi-day refactor deferred"
  paragraph was true before wave 28; today only `compact` is left, and that
  is one method, not a multi-pass refactor.
- **Verified against:** `crates/opt/src/pipeline.rs:156-159` and
  `crates/ir/src/function.rs:230` (`fn compact(&mut self) -> NodeIdRemap`).
- **Fix:** Drop `OptimizerOnBuilt::optimize_built` from the list of
  downstream traits, and rephrase the "multi-day refactor" line: only
  `compact` and a few tests still take `&mut BuiltFunctionGraph`, and the
  ctor remains as a small test/helper convenience.

### C4. Test comment names a non-existent error variant `ValidationFailed`
- **Severity:** MED
- **Where:** `crates/opt/src/pipeline.rs:362-364`
- **Comment text:**
  > "`run(graph, entry)` validates the final graph just like the historical
  > `run(&mut pattern::RewriteCtx<'_>)` did — i.e. an invalid graph in the
  > post-pass output surfaces as `ValidationFailed`."
- **Issue:** Two issues:
  1. The error variant `ValidationFailed` doesn't exist. Validation failures
     surface as a `ValidationErrors` bundle (`crates/ir/src/validate/mod.rs:71`)
     wrapped in `anyhow::Error` (recoverable via
     `err.downcast_ref::<ValidationErrors>()`).
  2. "Historical `run(&mut pattern::RewriteCtx<'_>)`" — the previous
     signature was almost certainly `run(&mut BuiltFunctionGraph)`, not
     `run(&mut RewriteCtx)`. (Same flavour of half-rewrite as B1.)
- **Verified against:**
  - `grep -rn "ValidationFailed" crates` → no hits.
  - `grep -rn "ValidationErrors" crates/ir/src` → 8 hits, all the actual type.
  - `crates/opt/src/pipeline.rs:263-267` shows the current `run` signature
    is `run(&mut ir::Graph, ir::node::NodeId) -> crate::Result<()>`.
- **Fix:** "…surfaces as a `ValidationErrors` bundle wrapped in
  `anyhow::Error`." Drop the "historical `run(&mut pattern::RewriteCtx<'_>)`"
  parenthesis or replace with `run(&mut BuiltFunctionGraph)`.

### C5. `FunctionBuilder::build` body comment names a non-existent helper
- **Severity:** MED
- **Where:** `crates/ir/src/builder/mod.rs:570-572`
- **Comment text:**
  > "The order here matches the iteration order used by `build_call_other`
  > so the i-th clobber output of a CallOther node corresponds to
  > `call_other_clobbered[i]`."
- **Issue:** `build_call_other` is not a function in the workspace.
  The CallOther builders are `build_call_other_modeled` and
  `build_call_other_terminal` (`crates/ir/src/builder/call.rs:192`,
  `:279`). A reader who greps for `fn build_call_other` finds nothing.
- **Verified against:**
  `grep -n "fn build_call_other" crates/ir/src/builder/call.rs` →
  `:192 build_call_other_terminal`, `:279 build_call_other_modeled`.
- **Fix:** Replace `build_call_other` with `build_call_other_modeled`
  (the modeled path is the one with multiple clobber outputs; the terminal
  variant has no value output and one combined ctrl edge).

────────────────────────────────────────────────────────────────────────

## D. Cross-reference breakage

### D1. Two doc cross-refs use a relative file-path that no longer resolves
- **Severity:** MED
- **Where:** `crates/ir/src/node/kind.rs:38` and `crates/ir/src/node/kind.rs:68`
- **Comment text:**
  > line 38: "Introduced by
  > `[opt::FunctionArgDetect](../../../opt/src/function_args.rs)` which …"
  > line 68: "Synthesized by
  > `[opt::StackLoadForward](../../../opt/src/stack_load_forward.rs)` when …"
- **Issue:** Both pass modules were converted from `*.rs` files to `*/mod.rs`
  modules. The relative paths point to files that don't exist:
  - `crates/opt/src/function_args.rs` → now `crates/opt/src/function_args/mod.rs`
  - `crates/opt/src/stack_load_forward.rs` → now
    `crates/opt/src/stack_load_forward/mod.rs`
  Also, this style of link (`[name](relative-path-to-source)`) does **not**
  resolve as a rustdoc intra-doc link — `rustdoc` treats it as an external
  URL and the target file is not on the rendered docs site, so the link
  silently 404s.
- **Verified against:**
  - `ls crates/opt/src/function_args.rs` → does not exist.
  - `ls crates/opt/src/function_args/` → `mod.rs tests.rs`.
  - Same shape for `stack_load_forward`.
- **Fix:** Change to plain rustdoc intra-doc links:
  `` [`opt::FunctionArgDetect`] `` and `` [`opt::StackLoadForward`] ``
  (rustdoc resolves these via the `pub use` re-exports in
  `crates/opt/src/lib.rs`).

────────────────────────────────────────────────────────────────────────

## E. Closed / opaque task references

### E1. Inline-code reference "(Task 15)" is opaque
- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:191`
- **Comment text:**
  > "…the second sweep would converge in zero rewrites (Task 15)."
- **Issue:** "Task 15" is referenced bare. There are at least four "Task 15"
  entries across `docs/superpowers/plans/*` (cfg-crate-tests,
  analyzer-crate-review, ir-crate-review-fresh, callother-classification,
  wide-const-and-deferred-items). Without a plan path the reader can't
  identify which task. Either it's closed (the comment is from the closed
  task and the reference is the equivalent of a ticket id no longer in
  the tracker) or it's open but unidentifiable.
- **Verified against:** `grep -rn "Task 15" docs/superpowers/plans/`.
- **Fix:** Either add an explicit plan reference (e.g.
  `// (see docs/superpowers/plans/2026-04-25-…-task-15)`) or delete the
  parenthetical — the surrounding text already explains the optimisation.

### E2. `Task17` TODOs are still open — keep
- **Severity:** none (positive finding)
- **Where:** `crates/cfg/src/cfg/decode_cache.rs:35`,
  `crates/strider/src/orchestrator.rs:287`,
  `crates/strider/src/strider/pipeline.rs:43`
- **Verified against:**
  `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md` exists,
  and `docs/superpowers/plans/2026-05-08-review-ai-followup.md:189` keeps
  `TODO(Task17)` as live tracking. No action required.

────────────────────────────────────────────────────────────────────────

## F. Round-9-wave clutter (low-priority)

64 `Round 9 …` / `R9-…` prefixes appear across the workspace. Most are
useful at-the-time historical attribution for audit-trail purposes; a few
specifically name a previous-but-already-replaced state and should be
condensed once the surrounding behaviour stabilises further.

Representative samples:
- `crates/cfg/src/cfg/types.rs:41` "Read the raw u64 address. Round 9 V3
  (R9-2D H2): canonical accessor for the migration path that …"
- `crates/ir/src/function.rs:115` "// Round 9 V2 (R9-2D H4): canonical
  read-only accessors for the CC fields. …"
- `crates/target/src/arch.rs:133-150` four consecutive Round-9-V7 prefixes
  on trivial getters.

These are not stale per se — they accurately label a round-9 addition — but
they make the docs noisy. None of them mislead a reader; flagged for cleanup
on a future condensation pass, not for round-10 action.

**Severity:** LOW. **Suggested action:** Defer to a single follow-up pass
once the CC-field-visibility migration completes. No correctness issue.

────────────────────────────────────────────────────────────────────────

## G. Items that **were** flagged but verified accurate (false alarms)

For audit-trail completeness, items the round-10 prompt suggested might be
stale but which currently match the code:

- **`crates/ir/src/error.rs` `UnknownCallOtherError` doc** — accurately names
  `FunctionBuilder::build_call_other_modeled` and
  `FunctionBuilder::build_call_other_terminal`; both exist
  (`crates/ir/src/builder/call.rs:192,279`). No fix needed.
- **`crates/ir/src/builder/mod.rs:558-567` `FunctionBuilder::build` `# Errors`** —
  correctly names `ValidationErrors` (the bundle type, plural) and the
  `downcast_ref::<crate::validate::ValidationErrors>()` recovery pattern.
  No fix needed.
- **`crates/cfg/src/cfg/builder/indirect_resolve.rs:40-44` `Multiple` reservation
  prose** — already updated; correctly says the cfg-time mini-graph resolver
  never returns `Multiple` and the IR-level resolver routes through the
  orchestrator. No fix needed.
- **`crates/opt/src/lib.rs:13-36` pass table** — matches the current
  pipeline composition (verified against `default_pipeline`,
  `stable_default_pipeline`, `destructive_default_pipeline` bodies on lines
  123-202). No fix needed.
- **`crates/strider/tests/indirect_branch.rs` 7 `#[ignore]` tests** — each
  reason names a real lifter shape. The aarch64-be reason ("lifter emits
  `Or(SP,K)` instead of `Add(SP,K)` and wraps stored labels in `Truncate`")
  matches the resolver's stack-array classifier (see
  `crates/opt/src/indirect_branch_resolve/stack_array.rs:117,141,426`,
  the `Round 9 IMPORTANT (R9-EA3 IMP-1 / arch wave) peel` comments which
  document the same lifter-shape gap). The mips64 PIC GOT-indirect reason
  matches the resolver's lack of a GOT-indirect arm (no
  `Add(Load[gp+off], const)` shape match in the classifier). The PPC
  reasons are honest "needs a one-shot pcode trace" placeholders. No fix
  needed.
- **`crates/cfg/src/cfg/decode_cache.rs:35` and the two `TODO(Task17)`
  siblings** — see E2; tracked, not stale.
- **`crates/strider-py/src/pattern.rs:1048` line-range `pcode-lift/src/value/float.rs:78-90`** —
  verified: `handle_float_nan` is at lines 78-90 of `float.rs`. No fix needed.

────────────────────────────────────────────────────────────────────────

## Counts

| Category                                         | HIGH | MED | LOW |
|--------------------------------------------------|------|-----|-----|
| A. Deleted symbols (`opt::with_built`)           |   3  |  0  |  1  |
| B. Half-rewritten comments                       |   1  |  1  |  0  |
| C. Behaviour drift                               |   3  |  2  |  0  |
| D. Cross-reference breakage                      |   0  |  1  |  0  |
| E. Closed / opaque task references               |   0  |  0  |  1  |
| F. Round-9-wave clutter                          |   0  |  0  |  1  |
| **Total findings**                               | **7** | **4** | **3** |

False-alarm items audited and cleared: 7 (see section G).

Files touched by HIGH-severity findings (in order, for quick triage):
1. `crates/ir/src/function.rs` — A1, C3
2. `crates/pattern/src/rewrite.rs` — A3, B2, C2
3. `crates/strider/src/rewrite.rs` — A4, A5
4. `crates/opt/src/pipeline.rs` — B1, C4
5. `crates/cfg/src/cfg/types.rs` — C1
6. `crates/ir/src/builder/mod.rs` — C5
7. `crates/ir/src/node/kind.rs` — D1
