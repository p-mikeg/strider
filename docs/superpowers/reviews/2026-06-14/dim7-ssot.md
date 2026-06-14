# Deep Audit — Dimension 7: SSoT (Single Source of Truth) boundaries

Date: 2026-06-14
Scope: entire `crates/` workspace (15 crates), READ-ONLY.
Method: read the state-owning code directly (`strider-ir` Function/Graph/builder/edit,
`strider-opt` pipeline/OptCtx, `strider-orchestrator` Strider, `strider-lift` Lifter),
plus targeted greps for the three categories, with each candidate verified against
the real signature + at least one call site (or both storage sites for duplication).
Comments / CLAUDE.md / memory were treated as hints, not ground truth.

## Executive summary

The workspace is **unusually SSoT-disciplined by design**. The calling-convention
SSoT (`default_cc` + `all_vns`), the container map, every side-table, the live-set /
roots bookkeeping, the dedup cache, and the `OptCtx` are each owned in exactly one
place with explicit derivation accessors and `compact`-time remapping. The deliberate
"single public constructor taking the full SSoT" pattern is pervasive and correct —
not flagged.

Genuine findings are modest: a small number of redundant/derivable function arguments
(all LOW–MED, no live drift), and several *documented, invariant-guarded* caches that
are correct today but are the only structural drift surfaces if future edits bypass
their maintenance verbs. No HIGH correctness-risk SSoT violation was found.

Counts: **Category 1 (duplication): 2** · **Category 2 (boundary): 0** ·
**Category 3 (redundant args): 3**. Severity: HIGH 0 · MED 2 · LOW 3.

---

## Findings

### D7-1 — `Function::retain_reachable(entry)` takes a param the receiver owns
- **Category:** 3 (derivable argument) · **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-ir/src/function/data.rs:873`
  `pub fn retain_reachable(&mut self, entry: NodeId) -> Result<NodeIdRemap>`
- **Call sites:** the *only* caller is `Function::compact` at
  `crates/strider-ir/src/function/data.rs:891-895`, which computes
  `let entry = self.entry.ok_or(...)?;` then calls `self.retain_reachable(entry)`.
  No other caller in the workspace (grep: single hit besides the definition).
- **What & why:** `entry` is `self.entry()` at the one call site. The method is a
  `&mut self` method on the very `Function` that owns `entry`, so the argument is a
  pure duplicate of state the receiver already holds. (It is `pub`, so an external
  caller *could* pass a non-entry root, but none does, and that would be a misuse —
  "retain reachable" semantically means "from the function's entry".)
- **Proposed fix:** drop the param: `pub fn retain_reachable(&mut self) -> Result<...>`,
  reading `self.entry.ok_or(...)?` internally; `compact` then calls
  `self.retain_reachable()`. If a from-arbitrary-root variant is ever needed, add it
  as a distinct `retain_reachable_from(root)` so the common path can't pass the wrong id.

### D7-2 — `FunctionLifter::new` receives `cc` separately while it also holds `lifter`
- **Category:** 3 (redundant/avoidable argument) · **Severity:** LOW · **Confidence:** MED
- **Location:** `crates/strider-lift/src/lift/function_lifter.rs:40-49`
  `fn new(lifter: &Lifter<R>, cc: &BuiltCallingConvention, cfg, all_vns, per_address_ccs)`
- **Call site:** `crates/strider-lift/src/lift/mod.rs:198`
  `FunctionLifter::new(self, cc, cfg, all_vns, Some(&opts.per_address_ccs))`.
- **What & why:** `endianness` is *not* a separate param — it is correctly derived as
  `lifter.arch.endianness()` (line 49), good. The `cc`, however, is a genuine
  per-function input (not a `Lifter` field — `Lifter` deliberately does NOT store the
  CC; it's per-call). So `cc` is **not** redundant with `lifter`. This is a
  *near-miss*: the only thing worth noting is that `cc` is immediately consumed into
  `FunctionBuilder::new` and never stored on `FunctionLifter`, so threading it through
  the struct ctor (vs. constructing the builder at the call site and passing the
  builder) is a minor shape choice, not a drift risk.
- **Proposed fix:** none required — documented here to record that the "duplicate
  endianness" smell a shallow scan suggests does NOT exist (endianness is derived, not
  passed). Leave as-is.

### D7-3 — Matcher walk threads `matcher` (holds `&Function`) alongside derivable lookups
- **Category:** 3 (derivable-from-one-arg) · **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-pattern/src/matcher/walk.rs:142` `try_match_at(matcher,
  pat, pat_node, ir_node, root_value, out_vertex, bindings)` (`#[allow(clippy::too_many_arguments)]`).
- **What & why:** `matcher.function().node_kind(ir_node)` is looked up *inside* the fn
  (line 152), so the node's kind is **not** a redundant parameter (a shallow scan may
  mis-flag this — verified it is derived, not passed). The real (cosmetic) observation
  is the 7-arg shape: `matcher`, `pat`, and `bindings` are stable context for an entire
  match attempt and could live behind a small per-attempt context struct, leaving the
  recursive call to vary only `(pat_node, ir_node, root_value, out_vertex)`. No drift,
  no correctness issue — purely an argument-count cleanup.
- **Proposed fix:** optional: bundle `(matcher, pat, &mut bindings)` into a
  `MatchCtx<'_>` so `try_match_at` recurses on the 4 varying fields. Low value.

### D7-4 — `value_vn` map holds two disjoint populations under one key space
- **Category:** 1 (blurred ownership / two facts in one table) · **Severity:** MED
  · **Confidence:** HIGH
- **Location:** `crates/strider-ir/src/function/data.rs:173-188` (field) + accessors
  `get_vn_for_value` / `set_vn_for_value`.
- **What & why:** the single `FxHashMap<ValueId, Vn>` stores BOTH (a) a lift-time
  `Phi`'s source varnode tag AND (b) a `Call`/`CallOther` clobber output's clobbered
  register. These are semantically different facts sharing one table; "absent entry"
  is overloaded to mean *three* things (anonymous phi, non-phi value, no-tag). This is
  not a *drift* risk (one writer per value, keyed by the producing value's `ValueId`,
  remapped wholesale by `compact`), but it IS blurred ownership: a future reader that
  assumes "every `value_vn` entry is a phi tag" (the indirect-branch classifier's
  Phi-of-IntConst arm comes close) would mis-read a clobber tag. The two populations
  never collide *only because* phi outputs and clobber outputs are distinct `ValueId`s.
- **Proposed fix:** acceptable to keep one map for memory/remap economy, but the
  invariant ("disjoint by producing-node kind") should be the documented contract of
  the accessors, and any new consumer must filter by `producer(value)`'s kind before
  interpreting the tag. No code change strictly required; flagged so the dual meaning
  isn't silently relied upon. Alternatively split into `phi_source_vn` /
  `clobber_vn` for unambiguous ownership.

### D7-5 — `EditFunction` caches `live_nodes`/`roots`; correctness rests on every mutation routing through the verbs
- **Category:** 1 (state derivable from the graph but cached) · **Severity:** MED
  · **Confidence:** HIGH
- **Location:** `crates/strider-ir/src/function/state.rs:46-57` (`FunctionState`) +
  `crates/strider-ir/src/function/edit.rs` (the mutation verbs + `function_mut` escape hatch).
- **What & why:** `live_nodes` + `roots` are a derivable view of the entry-reachable
  graph, cached on `FunctionState` and maintained incrementally by the curated edit
  verbs. This is a deliberate performance cache (avoids per-edit `compute_full`), and
  `populate` reseeds it as a pure read. The single drift surface is
  `EditFunction::function_mut()` (`edit.rs:96`), which hands out `&mut Function`
  **bypassing** the bookkeeping — its own doc-comment says the caller "is responsible
  for any state the curated verbs would otherwise maintain." Any pass that mutates
  graph structure through `function_mut` and then relies on `postorder()`/`roots`
  without reseeding would read a stale live-set.
- **Verification:** the cache is invariant-guarded in tests
  (`rewrite_rule.rs` `assert_live_matches_reachable`), so drift is *detected* in the
  test suite; no live caller was found mutating structure via `function_mut` and then
  reusing cached order. Correct today.
- **Proposed fix:** none functionally; recommend auditing every `function_mut()` caller
  for "structural mutation followed by cached-order read" and, if any exists, route it
  through the verbs or reseed. The escape hatch is the only thing standing between this
  cache and a true SSoT guarantee.

---

## Verified NON-findings (checked and cleared)

These were plausible SSoT violations that were investigated and found correct — recorded
so they are not re-flagged in future audits:

- **`validate(function)`** (`validate/mod.rs:71`) already takes only `&Function` and
  derives entry internally — CLAUDE.md still documents the old `(function, entry)`
  signature; **CLAUDE.md is stale here**, the code is right.
- **`Strider` vs `Lifter` state** (`orchestrator/src/lib.rs:108-120`): `Strider` owns
  exactly one `Lifter` (arch + owned Sleigh + cached SleighRegs) and a `rom`; no
  arch/cc/sleigh duplication. CC is per-call, not stored.
- **`SleighRegs` cache** (`lift/mod.rs:84-95`): built once from the `Lifter`'s own owned
  `Sleigh` at construction; same owner, cannot drift.
- **`known_targets` / `ResolvedTargets`** (`orchestrator/src/lib.rs:208-235`): single
  authoritative `FxHashMap` owned by the `analyze` loop; `ResolvedTargets` is a value
  type owned by `strider-cfg` and fed back by value. The agent-suggested "HIGH drift
  risk" is hypothetical (requires future external mutation of a loop-local) — downgraded
  to non-finding.
- **`OptCtx`** (`pipeline.rs:102-126`): no duplicated state; `sp_memo` is a documented
  per-drain cache cleared by the pipeline at every graph change; `endianness` is
  deliberately NOT carried (read from `Function::endianness` at decode time — explicit
  SSoT choice).
- **`default_cc` clone + `set_stack_args`** (`builder/mod.rs:353`): mutates the
  Function's *own* resolved CC copy; the Function is the authoritative owner of its CC,
  so post-clone mutation is intended, not drift.
- **`CallingConvention` vs `BuiltCallingConvention`** (spec → resolved) and
  `vn_to_container` / `initial_var_index` (derivable accelerators, both remapped by
  `compact`): standard derive-once / accelerator patterns with single owners.
- **`call_clobbered_for` / `call_ret_vals_for`**: derived on demand from
  `(default_cc, all_vns)` — no cached projected lists, so per-address-CC overrides
  cannot drift against a stale cache. Exemplary SSoT.

## Bottom line

No HIGH-severity SSoT drift exists. The two MED items (`value_vn` dual population,
`EditFunction` live-set cache via the `function_mut` escape hatch) are correct today
but are the workspace's only structural drift surfaces; both are guarded by
invariants/tests. The three LOW items are argument-shape cleanups with no behavioural
impact. The dominant finding is positive: the SSoT discipline here is strong, and one
piece of CLAUDE.md (the `validate` signature) has drifted from the code.
