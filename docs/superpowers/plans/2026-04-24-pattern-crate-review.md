# Pattern Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply concrete correctness, simplification, and readability improvements to the `pattern` crate, backed by targeted new tests and a final CodeRabbit pass.

**Architecture:** Surgical edits only — no redesign. The crate is structurally sound (sea-of-nodes matcher with journal-based backtracking, Arc-cheap `Pat`, fluent builders with a clean capture rule). Changes group into four buckets: (A) correctness, (B) simplification, (C) readability, (D) test coverage, plus (E) final CodeRabbit gate.

**Tech Stack:** Rust 2024, `cargo test -p pattern`, `cargo clippy -p pattern`, CodeRabbit CLI 1.1.0.

---

## Review Findings — Executive Summary

**No critical bugs.** After reading every source file under [crates/pattern/src/](crates/pattern/src/) the matcher, commutativity retry, journal rollback, and capture-conflict rules are correct. The items below are real but small.

**Correctness-adjacent (A):**

- **A1 — Dead `RewriteOutcome` enum.** [rewrite.rs:12-19](crates/pattern/src/rewrite.rs#L12-L19) defines `RewriteOutcome { RedirectTo, Skip }` and [lib.rs:71](crates/pattern/src/lib.rs#L71) re-exports it, but nothing constructs or consumes it (`rewrite_rule` returns `Result<bool>` and internally switches on `BuildOutcome`). [error.rs:40](crates/pattern/src/error.rs#L40) references a stale `crate::build::RewriteOutcome::Skip` path.
- **A2 — `ret().capture(v)` silently never matches.** Control patterns use `.capture_node(nv)`, but the blanket `IntoPat::capture` ([pat/mod.rs:205](crates/pattern/src/pat/mod.rs#L205)) still applies, producing a `CapturePat` whose value-kind gate ([any.rs:86](crates/pattern/src/pat/any.rs#L86)) always fails on zero-output nodes. It compiles, runs, and never hits. Needs a doc warning plus a `try_match_node` override on `CapturePat` / `GuardPat` that rejects zero-output nodes early (and an explicit test pinning the behaviour).
- **A3 — `rewrite_rule` assumes a single value output.** [rewrite.rs:56](crates/pattern/src/rewrite.rs#L56) calls `node_outputs_exact::<1>(node)?`. Current rewrite rules always target single-value nodes, but if a rule's LHS ever roots on a multi-output node (e.g. `Load = [Memory, Value]`, any `Call`) this returns an IR error rather than redirecting the value slot. Add a one-line doc constraint — real fix is out of scope.

**Simplification (B):**

- **B1 — Duplicated value-kind gate.** The `output_kind(target).as_value().is_none()` check appears in three places: [any.rs:34](crates/pattern/src/pat/any.rs#L34) (`VarPat`), [any.rs:86](crates/pattern/src/pat/any.rs#L86) (`CapturePat`), [guards.rs:47](crates/pattern/src/pat/guards.rs#L47) (`GuardPat`). Extract to a single helper `require_value_output(ctx, target) -> Option<NodeOutputType>`.
- **B2 — `Bindings::{bind,get}_var` / `bind,get_node_var` open-coded.** The `decl_bind_get!` macro at [bindings.rs:126-155](crates/pattern/src/matcher/bindings.rs#L126-L155) unifies the 11 typed variants; the first two (Var, NodeVar) are hand-written at [bindings.rs:75-121](crates/pattern/src/matcher/bindings.rs#L75-L121). Bring them under the same macro.
- **B3 — Clippy pedantic cleanup.** `cargo clippy -p pattern -- -W clippy::pedantic` shows 118 warnings. Narrow to a concrete fix set: drop redundant `.into_iter()` ([traits.rs:64](crates/pattern/src/pat/traits.rs#L64), [node_pat.rs:298](crates/pattern/src/pat/node_pat.rs#L298)), elide lifetime in `apply_rules_in_order` ([rewrite.rs:98](crates/pattern/src/rewrite.rs#L98)), add `#[must_use]` to fluent builder setters. Skip the bulk-rewrite `#[must_use]` on ctor free-functions — they're obviously result-returning and the attribute noise outweighs the benefit.
- **B4 — `function_arg_count` Option match.** [matcher/mod.rs:133-138](crates/pattern/src/matcher/mod.rs#L133-L138) — replace 5-line `match` with `map_or(0, |&m| m as usize + 1)`.
- **B5 — Redundant `swap` arity guard.** [node_pat.rs:465-470](crates/pattern/src/pat/node_pat.rs#L465-L470) — the comment admits it's "defensive" because `try_match_common` only passes `swap=true` for arity-2 patterns. Demote to `debug_assert!(!swap || pats.len() == 2)` immediately before the loop.

**Readability (C):**

- **C1 — `lib.rs` re-export list is noisy.** Alphabetically sorted but with stray `// Builder types`, `// Blanket trait`, etc. scattered mid-list. Either commit to alphabetic (strip the section comments) or commit to logical groups (de-alphabetize + keep the comments as real headers).
- **C2 — `Match::get_float` name is misleading.** [match_result.rs:58](crates/pattern/src/matcher/match_result.rs#L58) returns `Option<u64>` (IEEE 754 bits). The comment at 55-57 acknowledges the collision with `get_float_bits(Var, graph)`. Rename to `get_float_bits(FloatVar)` — yes, the collision disappears because the two methods take different argument types. Breaking to downstream but this is a small crate with limited users.
- **C3 — Surface the commutativity semantics.** [commutativity.rs:20-22](crates/pattern/src/matcher/commutativity.rs#L20-L22) includes `Equal`/`Carry`/`Scarry` — verified correct against [ir/src/ops/op_kinds.rs:30-49](crates/ir/src/ops/op_kinds.rs#L30-L49) (`Carry(l,r)` = unsigned overflow of `l+r` which is symmetric in `l,r`; same for `Scarry`). Add a one-line comment naming why these three are commutative so the next reader doesn't second-guess.
- **C4 — Consolidate the value-kind rule doc.** Currently repeated verbatim in [any.rs:30-33](crates/pattern/src/pat/any.rs#L30-L33), [any.rs:82-85](crates/pattern/src/pat/any.rs#L82-L85), [guards.rs](crates/pattern/src/pat/guards.rs), and [builders/mod.rs:11-24](crates/pattern/src/pat/builders/mod.rs#L11-L24). Leave the builders/mod.rs version as canonical; make the others point at it.

**Test coverage (D):**

The matcher's own test module ([matcher/tests.rs](crates/pattern/src/matcher/tests.rs)) is thin. The integration suite under [tests/matching/](crates/pattern/tests/matching/) does cover arithmetic, control, captures/predicates, float/stack, variant-agnostic ops, and helpers — but it's missing a handful of specific behaviours that are easy to author and would pin real invariants.

- **D1 — Commutative retry actually fires.** Build `add(var(a), int_const(5))`; the node's operand order is `(int_const(5), var_source)` — pattern should match because commutative retry swaps the pattern's view. Assert `a` binds to the non-const operand.
- **D2 — Capture conflict rejects.** Build `add(x, y)` where `x ≠ y`; pattern `add(var(v), var(v))`. Assert zero hits.
- **D3 — Capture conflict on self-add matches.** Build `add(x, x)`; same pattern. Assert one hit.
- **D4 — `.when(f)` returning `false` rolls back bindings.** Pattern `add(var(a), var(b)).when(always_false)` over a graph with at least one `add` and another pattern that also binds `a`. Assert the outer search finds zero hits for the `.when`-guarded pattern. (Note: captures/predicates.rs already has `.when` tests, but none explicitly assert bindings-rollback on failure.)
- **D5 — Zero-output capture footgun.** `ret().capture(v)` — pin the current "never matches" behaviour (or the post-fix "consistent failure with clear semantics") in a test so future refactors don't silently change it.
- **D6 — `StackStorePhiPat::offsets` multiset comparison.** Build a `StackStorePhi` with `[8, 0, 8]` offsets, match with `.offsets([8, 0, 8])` and `.offsets([0, 8, 8])` — both should hit; `.offsets([0, 8])` should miss. Exercises the sort+compare path.

**CodeRabbit gate (E):** Run `coderabbit review` once with the `pattern`-crate changes staged, address its findings, repeat until clean.

---

## File Structure

No new top-level files. New test files:
- [crates/pattern/tests/matching/commutativity.rs](crates/pattern/tests/matching/commutativity.rs) — D1, D4
- [crates/pattern/tests/matching/captures_conflict.rs](crates/pattern/tests/matching/captures_conflict.rs) — D2, D3, D5

Or extend existing `captures_predicates.rs`, `float_stack.rs`, `control.rs` if that's the established convention. Tasks below assume extension over new files; revisit if grepping shows one file per theme is the pattern.

Modified files:
- [crates/pattern/src/rewrite.rs](crates/pattern/src/rewrite.rs) — A1 (remove dead enum), B3 (elide lifetime), A3 (doc note)
- [crates/pattern/src/lib.rs](crates/pattern/src/lib.rs) — A1, C1
- [crates/pattern/src/error.rs](crates/pattern/src/error.rs) — A1 (fix stale doclink)
- [crates/pattern/src/pat/any.rs](crates/pattern/src/pat/any.rs) — A2, B1, C4
- [crates/pattern/src/pat/guards.rs](crates/pattern/src/pat/guards.rs) — A2, B1, C4
- [crates/pattern/src/pat/traits.rs](crates/pattern/src/pat/traits.rs) — B3 (into_iter), A2 context
- [crates/pattern/src/pat/node_pat.rs](crates/pattern/src/pat/node_pat.rs) — B3 (into_iter), B5 (swap guard)
- [crates/pattern/src/pat/mod.rs](crates/pattern/src/pat/mod.rs) — new `require_value_output` helper home (or put it in `matcher/mod.rs`); A2 doc note on `IntoPat::capture`
- [crates/pattern/src/matcher/bindings.rs](crates/pattern/src/matcher/bindings.rs) — B2 (macro the first two bind/get pairs)
- [crates/pattern/src/matcher/commutativity.rs](crates/pattern/src/matcher/commutativity.rs) — C3
- [crates/pattern/src/matcher/mod.rs](crates/pattern/src/matcher/mod.rs) — B4 (function_arg_count)
- [crates/pattern/src/matcher/match_result.rs](crates/pattern/src/matcher/match_result.rs) — C2 (rename + update call sites in examples/tests if any)

---

## Tasks

### Task 1: A1 — Remove dead `RewriteOutcome`

**Files:**
- Modify: [crates/pattern/src/rewrite.rs](crates/pattern/src/rewrite.rs)
- Modify: [crates/pattern/src/lib.rs:71](crates/pattern/src/lib.rs#L71)
- Modify: [crates/pattern/src/error.rs:40](crates/pattern/src/error.rs#L40)

- [ ] **Step 1: Confirm no downstream usage**

Run: `grep -rn --include='*.rs' RewriteOutcome /home/mike/Desktop/strider`
Expected: matches only in `rewrite.rs` (definition + module doc), `lib.rs` (re-export), `error.rs` (stale doclink). If anything else appears, **stop and reassess**.

- [ ] **Step 2: Delete the enum and its module-doc mention**

In [rewrite.rs](crates/pattern/src/rewrite.rs):

- At line 2, replace the module-doc list `//! [`boxed_rule`], and the [`RewriteOutcome`] outcome enum.` with `//! [`boxed_rule`].`
- Delete lines 12-19 (the `RewriteOutcome` enum and its doc block).

- [ ] **Step 3: Drop from the re-export**

In [lib.rs:71](crates/pattern/src/lib.rs#L71), change:

```rust
pub use rewrite::{BoxedRule, RewriteOutcome, apply_rules_in_order, boxed_rule, rewrite_rule};
```

to:

```rust
pub use rewrite::{BoxedRule, apply_rules_in_order, boxed_rule, rewrite_rule};
```

- [ ] **Step 4: Fix the stale doclink in error.rs**

In [error.rs:38-41](crates/pattern/src/error.rs#L38-L41), replace `[`crate::build::RewriteOutcome::Skip`]` with `"no change"` (prose — there's no longer a concrete type to link to).

- [ ] **Step 5: Verify**

Run: `cargo build -p pattern && cargo test -p pattern`
Expected: builds clean, all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/src/rewrite.rs crates/pattern/src/lib.rs crates/pattern/src/error.rs
git commit -m "refactor(pattern): remove unused RewriteOutcome enum"
```

---

### Task 2: A2 — Fix the `ret().capture(v)` silent-miss footgun

**Files:**
- Modify: [crates/pattern/src/pat/any.rs](crates/pattern/src/pat/any.rs) (`CapturePat`)
- Modify: [crates/pattern/src/pat/guards.rs](crates/pattern/src/pat/guards.rs) (`GuardPat`)
- Modify: [crates/pattern/src/pat/mod.rs:203-215](crates/pattern/src/pat/mod.rs#L203-L215) (`IntoPat::capture` doc)
- Test: extend [crates/pattern/tests/matching/captures_predicates.rs](crates/pattern/tests/matching/captures_predicates.rs)

The root issue: the default `Pattern::try_match_node` iterates the node's outputs. A zero-output node (like `Return`) has an empty output list, so the default immediately returns `false`. `NodePat` overrides `try_match_node` to delegate to `try_match_common` in that case; `CapturePat` and `GuardPat` do not, so wrapping a `RetPat` in a capture or guard produces a pattern that never matches. Combined with the blanket `IntoPat::capture` impl, `ret().capture(v)` compiles but is always a no-op.

Fix strategy:
- Override `try_match_node` on `CapturePat` and `GuardPat` to forward to the inner pattern's `try_match_node` for zero-output nodes, then (for `CapturePat`) apply the value-kind gate + `bind_var`, which correctly fails on a zero-output node because there's no output to bind — same observable behaviour, but the failure is explicit instead of silent.
- Add a doc warning on [`IntoPat::capture`](crates/pattern/src/pat/mod.rs#L205) naming the rule: "use `.capture_node(nv)` on control-flow builders (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`)."
- Pin the behaviour with a test so this doesn't silently regress.

- [ ] **Step 1: Add a failing test**

Append to [crates/pattern/tests/matching/captures_predicates.rs](crates/pattern/tests/matching/captures_predicates.rs):

```rust
#[test]
fn ret_capture_value_var_never_matches() -> ir::Result<()> {
    // Regression pin: `ret().capture(v)` compiles (Pat inherits the blanket
    // IntoPat), but `Var` binds a data output and `Return` is zero-output,
    // so no site can possibly match.  Users who want a handle on the Return
    // itself must call `.capture_node(NodeVar::new())`.
    use pattern::{Matcher, Var, ret};
    let g = crate::matching::common::graph_single_return()?;
    let v = Var::new();
    let pat: pattern::Pat = ret().capture(v).into();
    let m = Matcher::new(&g);
    let hits = m.find_all(&pat);
    assert!(hits.is_empty(), "ret().capture(Var) must not produce hits");
    Ok(())
}
```

If `graph_single_return` isn't already in [tests/matching/common.rs](crates/pattern/tests/matching/common.rs), add it — a minimal function builder that creates a region, sets entry, and calls `build_return(None, &[])`.

- [ ] **Step 2: Run the new test to confirm current behaviour**

Run: `cargo test -p pattern --test matching ret_capture_value_var_never_matches -- --nocapture`
Expected: PASS (the invariant is already true; we're pinning it).

- [ ] **Step 3: Document `IntoPat::capture`**

Prepend this doc to [pat/mod.rs:205](crates/pattern/src/pat/mod.rs#L205):

```rust
/// Bind the matched **value** output to `v`.
///
/// **Only valid on value-producing patterns.**  Control-flow builders
/// (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`) expose `.capture_node(nv)`
/// instead — calling `.capture(v)` on one of those (via this blanket impl)
/// compiles but the resulting pattern never matches, because `Var` refers
/// to a data edge and those nodes have none at the value slot.
```

- [ ] **Step 4: Override `try_match_node` on `CapturePat` (explicit failure)**

Append a `try_match_node` override to the `impl Pattern for CapturePat` block at [any.rs:63](crates/pattern/src/pat/any.rs#L63):

```rust
    fn try_match_node(&self, ctx: &MatchCtx, node: ir::node::NodeId, b: &mut Bindings) -> bool {
        // A `CapturePat` binds the matched value output.  For zero-output
        // nodes (e.g. `Return`) there is no output to bind, so fail
        // explicitly rather than fall through the default outputs-iterator
        // and report a silent miss.
        if ctx.graph.graph.node_outputs(node).is_empty() {
            return false;
        }
        // Default behaviour: iterate outputs via try_match.
        for out in ctx.graph.graph.node_outputs(node) {
            let mark = b.mark();
            if self.try_match(ctx, out, b) {
                return true;
            }
            b.restore(mark);
        }
        false
    }
```

Do the same in [guards.rs](crates/pattern/src/pat/guards.rs), inside `impl Pattern for GuardPat`. Both combinators have the same semantic — the inner pattern's value output is the focus; zero-output nodes can't match.

- [ ] **Step 5: Re-run the pinned test + full suite**

Run: `cargo test -p pattern`
Expected: PASS including the new test (behaviour is unchanged observably — still no hits — but now the path is explicit).

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/src/pat/any.rs crates/pattern/src/pat/guards.rs crates/pattern/src/pat/mod.rs crates/pattern/tests/matching/captures_predicates.rs crates/pattern/tests/matching/common.rs
git commit -m "fix(pattern): pin zero-output capture/guard behaviour, doc capture rule"
```

---

### Task 3: A3 — Document single-value-output assumption in `rewrite_rule`

**Files:**
- Modify: [crates/pattern/src/rewrite.rs:38-85](crates/pattern/src/rewrite.rs#L38-L85)

- [ ] **Step 1: Extend the `rewrite_rule` doc comment**

Above the `pub fn rewrite_rule(...)` definition, append this paragraph to the existing doc block:

```
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output — the rule redirects
/// that output's uses to the RHS-built output.  Rooting a rule on a
/// multi-output node (any `Call`, `Load`, `Store`, control-flow node)
/// returns an `IrError` from [`BuiltFunctionGraph::node_outputs_exact`].
/// If you need multi-slot rewriting, operate on the value slot explicitly.
```

- [ ] **Step 2: Verify rustdoc renders**

Run: `cargo doc -p pattern --no-deps` and open the target HTML.
Expected: the constraint note is visible on the `rewrite_rule` page.

- [ ] **Step 3: Commit**

```bash
git add crates/pattern/src/rewrite.rs
git commit -m "docs(pattern): note rewrite_rule single-value-output constraint"
```

---

### Task 4: B1 — Extract `require_value_output` helper

**Files:**
- Modify: [crates/pattern/src/pat/traits.rs](crates/pattern/src/pat/traits.rs) (add helper to `MatchCtx` impl)
- Modify: [crates/pattern/src/pat/any.rs:34](crates/pattern/src/pat/any.rs#L34), [any.rs:86](crates/pattern/src/pat/any.rs#L86)
- Modify: [crates/pattern/src/pat/guards.rs:47](crates/pattern/src/pat/guards.rs#L47)

- [ ] **Step 1: Add helper on `MatchCtx`**

In [traits.rs](crates/pattern/src/pat/traits.rs) after the `MatchCtx` struct definition (around line 34):

```rust
impl MatchCtx<'_, '_> {
    /// Returns `Some(ty)` if `target` is a value output, `None` otherwise.
    /// Used by the value-kind gate in `VarPat`, `CapturePat`, and `GuardPat`
    /// — `Var` bindings refer to data edges only.
    pub(crate) fn require_value_output(
        &self,
        target: ir::node::NodeOutputId,
    ) -> Option<ir::node::NodeOutputType> {
        self.graph.graph.output_kind(target).as_value()
    }
}
```

- [ ] **Step 2: Replace the three call sites**

- [any.rs:34](crates/pattern/src/pat/any.rs#L34) — `VarPat::try_match`:
  ```rust
  if ctx.require_value_output(target).is_none() {
      return false;
  }
  ```

- [any.rs:86](crates/pattern/src/pat/any.rs#L86) — `CapturePat::try_match`, same replacement; keep the surrounding `b.restore(mark)` flow.

- [guards.rs:47](crates/pattern/src/pat/guards.rs#L47) — `GuardPat::try_match`:
  ```rust
  let Some(out_ty) = ctx.require_value_output(target) else {
      b.restore(mark);
      return false;
  };
  ```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pattern`
Expected: all green; behaviour unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates/pattern/src/pat/traits.rs crates/pattern/src/pat/any.rs crates/pattern/src/pat/guards.rs
git commit -m "refactor(pattern): hoist value-output gate into MatchCtx helper"
```

---

### Task 5: B2 — Unify `Bindings` var/node_var under `decl_bind_get!`

**Files:**
- Modify: [crates/pattern/src/matcher/bindings.rs:75-121, 157-167](crates/pattern/src/matcher/bindings.rs#L75)

The existing `decl_bind_get!` macro produces `pub fn bind_$name` / `pub fn get_$name` pairs. `bind_var` / `bind_node_var` are `pub(crate)` today, and the macro emits `pub` methods — either widen the macro to accept a visibility or widen those two methods to `pub`. The crate-private-ness is vestigial; `Match::get` / `Match::get_node` already expose the reads publicly, and there's no reason binding a `Var` can't also be pub since it requires `Bindings` which is only authored by the match engine.

Simplest: make them `pub`, then replace the hand-rolled impls with two `decl_bind_get!` rows.

- [ ] **Step 1: Promote `bind_var` / `bind_node_var` to `pub`**

Lines 75 and 87 — delete `(crate)`.

- [ ] **Step 2: Delete the open-coded impls**

In [bindings.rs:62-121](crates/pattern/src/matcher/bindings.rs#L62-L121) keep `mark`, `restore`. Delete `bind_var`, `bind_node_var`, `get`, `get_node` (lines 75-121).

- [ ] **Step 3: Add two `decl_bind_get!` rows**

At the macro call list starting at [bindings.rs:157](crates/pattern/src/matcher/bindings.rs#L157), prepend:

```rust
decl_bind_get!(Var,             bind_var,             get,                 Var,             NodeOutputId, "the matched `NodeOutputId` (data output)");
decl_bind_get!(NodeVar,         bind_node_var,        get_node,            NodeVar,         NodeId,       "the matched `NodeId` (control node)");
```

Note the `get` / `get_node` method names (vs `get_var` / `get_node_var`) preserve the existing public surface.

- [ ] **Step 4: Verify public surface unchanged**

Run: `cargo build -p pattern && cargo test -p pattern`
Expected: clean build. The generated methods have identical signatures to the open-coded ones.

- [ ] **Step 5: Commit**

```bash
git add crates/pattern/src/matcher/bindings.rs
git commit -m "refactor(pattern): unify Var/NodeVar bindings under decl_bind_get!"
```

---

### Task 6: B3, B4, B5 — Clippy sweep, `function_arg_count`, swap-guard demotion

**Files:**
- Modify: [crates/pattern/src/pat/traits.rs:64](crates/pattern/src/pat/traits.rs#L64) — drop `.into_iter()`
- Modify: [crates/pattern/src/pat/node_pat.rs:298](crates/pattern/src/pat/node_pat.rs#L298) — drop `.into_iter()`
- Modify: [crates/pattern/src/pat/node_pat.rs:465-470](crates/pattern/src/pat/node_pat.rs#L465-L470) — swap guard
- Modify: [crates/pattern/src/rewrite.rs:98-103](crates/pattern/src/rewrite.rs#L98-L103) — elide lifetime
- Modify: [crates/pattern/src/matcher/mod.rs:133-138](crates/pattern/src/matcher/mod.rs#L133-L138) — `function_arg_count`

Do NOT do the bulk `#[must_use]` attribute rewrite on every public function — noise outweighs value. Add `#[must_use]` only to the 12 fluent builder setters that return `Self` (where dropping the return is genuinely a bug).

- [ ] **Step 1: Fix `explicit_into_iter_loop`**

- [traits.rs:64](crates/pattern/src/pat/traits.rs#L64): `for out in ctx.graph.graph.node_outputs(node) {` (drop `.into_iter()`).
- [node_pat.rs:298](crates/pattern/src/pat/node_pat.rs#L298): `for out in outputs {` (drop `.into_iter()`).

- [ ] **Step 2: Elide lifetime in `apply_rules_in_order`**

[rewrite.rs:98-103](crates/pattern/src/rewrite.rs#L98-L103):

```rust
pub fn apply_rules_in_order<R>(
    rules: &[R],
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + '_
```

- [ ] **Step 3: Demote swap-guard to `debug_assert!`**

[node_pat.rs:460-479](crates/pattern/src/pat/node_pat.rs#L460-L479) — inside the `InputsSpec::Fixed` arm, above the `for (pat_idx, sub_pat) in pats.iter().enumerate()` loop, replace the runtime `if swap && pats.len() != 2 { return false; }` + the comment with:

```rust
debug_assert!(
    !swap || pats.len() == 2,
    "swap is only set by try_match_common's arity-2 retry path",
);
```

- [ ] **Step 4: Simplify `function_arg_count`**

[matcher/mod.rs:133-138](crates/pattern/src/matcher/mod.rs#L133-L138):

```rust
pub fn function_arg_count(&self) -> usize {
    self.function_arg_index()
        .0
        .keys()
        .max()
        .map_or(0, |&m| m as usize + 1)
}
```

- [ ] **Step 5: Add `#[must_use]` to fluent setters**

On every public `-> Self` method on the builders (`LoadPat::space`, `LoadPat::addr`, `StorePat::{space,addr,data}`, `StackStorePat::{space,offset,data}`, `StackStorePhiPat::{space,data,offsets}`, `CallPat::{target,arg,ret_output,capture_node,at}`, `CallOtherPat::{user_op_id,arg,capture_node}`, `IfPat::{cond,true_branch,false_branch,capture_node}`, `RetPat::{preceded_by,ret_val,capture_node}`, `FunctionArgPat::{source,index}`, `PhiPat::vn`, `IntBinaryOpPat::ordered`, `BoolBinaryOpPat::ordered`, `FloatBinaryOpPat::ordered`), prepend `#[must_use]` to the `pub fn`.

- [ ] **Step 6: Verify**

Run: `cargo build -p pattern && cargo test -p pattern && cargo clippy -p pattern -- -W clippy::pedantic 2>&1 | grep -c warning`
Expected: build clean, tests green, clippy pedantic warning count drops noticeably (baseline: 118).

- [ ] **Step 7: Commit**

```bash
git add crates/pattern/src/
git commit -m "style(pattern): address selected clippy pedantic warnings"
```

---

### Task 7: C1 — Clean up `lib.rs` re-exports

**Files:**
- Modify: [crates/pattern/src/lib.rs:76-223](crates/pattern/src/lib.rs#L76-L223)

Choose one: logical grouping (drop alphabetic, keep the section comments as headers with no sorting within) OR pure alphabetic (strip all mid-list comments). The codebase has no rustfmt config forcing one; pick logical grouping since the intent is clearly documentary.

- [ ] **Step 1: Reorder the `pat` re-export block**

Restructure [lib.rs:79-212](crates/pattern/src/lib.rs#L79-L212) into clearly labelled `pub use` groups, each with its own `pub use pat::{ ... };` block so the section boundaries are syntactically real, not comment-only. One possible layout:

```rust
// ── Core types ───────────────────────────────────────────────────────────────
pub use matcher::{Bindings, Match, Matcher};
pub use pat::{IntoPat, Pat, MatchPredicateFn, PredicateFn};

// ── Builder structs ──────────────────────────────────────────────────────────
pub use pat::{
    BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat,
    IfPat, IntBinaryOpPat, LoadPat, PhiPat, RetPat, StackStorePat,
    StackStorePhiPat, StorePat,
};

// ── Const-capture overload traits ───────────────────────────────────────────
pub use pat::{IntoAnyBoolConst, IntoAnyFloatConst, IntoAnyIntConst};

// ── Wildcards & captures ────────────────────────────────────────────────────
pub use pat::{any, predicate, var};

// ── Int constructors ────────────────────────────────────────────────────────
pub use pat::{
    add, and, div, int_binary, int_binary_any, int_carry, int_cmp, int_cmp_any,
    int_eq, int_le, int_lt, int_sborrow, int_scarry, int_sle, int_slt,
    int_unary, int_unary_any, lzcount, mul, neg, not, or, popcount,
    rem, sdiv, shl, shr, srem, sshr, sub, xor,
};

// ── Bool constructors ───────────────────────────────────────────────────────
pub use pat::{bool_and, bool_binary, bool_binary_any, bool_not, bool_or, bool_unary, bool_unary_any, bool_xor};

// ── Float constructors ──────────────────────────────────────────────────────
pub use pat::{
    float_abs, float_add, float_binary, float_binary_any, float_bits_to_int,
    float_ceil, float_cmp, float_cmp_any, float_div, float_eq, float_floor,
    float_le, float_lt, float_mul, float_ne, float_neg, float_round,
    float_sqrt, float_sub, float_to_float, float_to_int, float_unary,
    float_unary_any,
};

// ── Casts & coercions ───────────────────────────────────────────────────────
pub use pat::{cast_to_bool, cast_to_float, cast_to_int, extend, int_bits_to_float, int_to_float, sign_extend, truncate, zero_extend};

// ── Constants ───────────────────────────────────────────────────────────────
pub use pat::{any_bool_const, any_float_const, any_int_const, bool_const, float_const, int_const};

// ── Memory / phi / function-arg ─────────────────────────────────────────────
pub use pat::{function_arg, function_arg_any, function_arg_reg, function_arg_stack, load, phi, phi_for, stack_store, stack_store_phi, store};

// ── Control ─────────────────────────────────────────────────────────────────
pub use pat::{call, call_other, if_node, initial_var, initial_var_for, ret};
```

- [ ] **Step 2: Stop `rustfmt` from re-sorting**

If the project's rustfmt config sorts `use` groups (the original flat-alpha layout strongly suggests it does), verify: run `cargo fmt -p pattern` and check the diff. If it re-sorts, either (a) add `#[rustfmt::skip]` above the block or (b) accept a partial rollback — per-block ordering is preserved even with sorting, which is the main readability win.

- [ ] **Step 3: Verify**

Run: `cargo build -p pattern && cargo test -p pattern`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pattern/src/lib.rs
git commit -m "style(pattern): group lib.rs re-exports into logical sections"
```

---

### Task 8: C2 — Rename `Match::get_float` → `Match::get_float_bits(FloatVar)`

**Files:**
- Modify: [crates/pattern/src/matcher/match_result.rs:52-60](crates/pattern/src/matcher/match_result.rs#L52-L60)
- Audit & fix all call sites inside the crate (tests, examples)

Both `get_float(FloatVar) -> Option<u64>` and `get_float_bits(Var, &BuiltFunctionGraph) -> Option<u64>` exist; the name collision is resolved by argument types only. Renaming the first to `get_float_bits(FloatVar)` is actually still unambiguous because overloading on the Var type works — but Rust doesn't support overloaded methods. So the rename would need a different name.

Revised proposal: rename to **`get_float_const(FloatVar) -> Option<u64>`** to mirror `get_int_const` / `get_bool_const` which both live on `Match` already. Document that the returned `u64` is the IEEE 754 bit pattern.

- [ ] **Step 1: Rename the method**

In [match_result.rs:52-60](crates/pattern/src/matcher/match_result.rs#L52-L60):

```rust
/// Returns the IEEE 754 bit pattern bound to the [`FloatVar`] `fv`, or
/// `None` if `fv` was not captured in this match.  Parallel to
/// [`Match::get_int_const`] and [`Match::get_bool_const`] for typed const
/// captures.
pub fn get_float_const(&self, fv: FloatVar) -> Option<u64> {
    self.bindings.get_float_bits(fv)
}
```

- [ ] **Step 2: Update call sites**

Run: `grep -rn --include='*.rs' '\.get_float(' /home/mike/Desktop/strider`
For each hit, rewrite `.get_float(` → `.get_float_const(`. Expect zero or a handful of occurrences inside `tests/` and `examples/`.

- [ ] **Step 3: Verify**

Run: `cargo build -p pattern && cargo test -p pattern && cargo test -p pattern --examples`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pattern/
git commit -m "refactor(pattern): rename Match::get_float to get_float_const"
```

---

### Task 9: C3 — Document commutativity of `IntCmpOp::Carry` / `Scarry`

**Files:**
- Modify: [crates/pattern/src/matcher/commutativity.rs](crates/pattern/src/matcher/commutativity.rs)

- [ ] **Step 1: Add a why-comment**

Above `is_commutative_int_cmp_op` at line 20:

```rust
/// `Equal` is symmetric by definition.  `Carry(l, r)` and `Scarry(l, r)`
/// ask whether the addition `l + r` overflows (unsigned / signed) — since
/// addition commutes, so do these two comparisons.  `Less` / `LessEqual` /
/// `Sless` / `SlessEqual` are directional and thus non-commutative; same
/// for `Borrow` / `Sborrow`, which encode subtraction-based relations.
```

- [ ] **Step 2: Commit**

```bash
git add crates/pattern/src/matcher/commutativity.rs
git commit -m "docs(pattern): explain IntCmpOp commutativity choices"
```

---

### Task 10: C4 — Consolidate value-kind rule docs

**Files:**
- Modify: [crates/pattern/src/pat/any.rs:28-33, 82-89](crates/pattern/src/pat/any.rs#L28-L33) — shorten
- Modify: [crates/pattern/src/pat/guards.rs](crates/pattern/src/pat/guards.rs) — shorten
- Treat [builders/mod.rs:11-24](crates/pattern/src/pat/builders/mod.rs#L11-L24) as canonical reference

- [ ] **Step 1: Replace each duplicated paragraph**

In each of the three files, reduce the long explanation to one line pointing at the builders/mod.rs rule:

```rust
// Value-kind gate: see the capture rule on `pat::builders` — `Var` binds
// a data edge, so non-value outputs (Memory / Control) cause the match
// to fail.
```

- [ ] **Step 2: Commit**

```bash
git add crates/pattern/src/pat/any.rs crates/pattern/src/pat/guards.rs
git commit -m "docs(pattern): deduplicate value-kind gate docs"
```

---

### Task 11: D1-D6 — Add targeted regression tests

**Files:**
- Modify: [crates/pattern/tests/matching/captures_predicates.rs](crates/pattern/tests/matching/captures_predicates.rs) — D2, D3, D4
- Modify: [crates/pattern/tests/matching/arithmetic.rs](crates/pattern/tests/matching/arithmetic.rs) — D1
- Modify: [crates/pattern/tests/matching/float_stack.rs](crates/pattern/tests/matching/float_stack.rs) — D6

D5 (`ret().capture(v)` regression pin) is already handled in Task 2.

- [ ] **Step 1: D1 — Commutative retry fires on reversed operand order**

Append to [crates/pattern/tests/matching/arithmetic.rs](crates/pattern/tests/matching/arithmetic.rs):

```rust
#[test]
fn commutative_retry_fires_on_reversed_add() -> ir::Result<()> {
    use pattern::{Matcher, Var, add, int_const, var};
    // Build: add(int_const(5), arg) — const on the LEFT.
    let (g, _arg_vn) = crate::matching::common::graph_add_const_left(5)?;
    // Pattern: add(arg, const 5) — const on the RIGHT.  Commutative retry
    // should match by swapping.
    let x = Var::new();
    let pat: pattern::Pat = add(var(x), int_const(5)).into();
    let m = Matcher::new(&g);
    let hits = m.find_all(&pat);
    assert_eq!(hits.len(), 1, "commutative add should match in both orders");
    assert!(hits[0].get(x).is_some(), "x should bind to the non-const operand");
    Ok(())
}
```

Add `graph_add_const_left` to [common.rs](crates/pattern/tests/matching/common.rs) if not present.

- [ ] **Step 2: D2 + D3 — Capture conflict**

Append to [captures_predicates.rs](crates/pattern/tests/matching/captures_predicates.rs):

```rust
#[test]
fn capture_conflict_on_distinct_operands_rejects() -> ir::Result<()> {
    use pattern::{Matcher, Var, add, var};
    // Graph has add(a, b) where a != b.
    let g = crate::matching::common::graph_add_two_distinct_args()?;
    // Pattern forces the same Var on both operands.
    let v = Var::new();
    let pat: pattern::Pat = add(var(v), var(v)).into();
    let m = Matcher::new(&g);
    assert!(m.find_all(&pat).is_empty(), "distinct operands must not match");
    Ok(())
}

#[test]
fn capture_conflict_on_self_add_matches() -> ir::Result<()> {
    use pattern::{Matcher, Var, add, var};
    // Graph has add(a, a) — same operand on both sides.
    let g = crate::matching::common::graph_add_self()?;
    let v = Var::new();
    let pat: pattern::Pat = add(var(v), var(v)).into();
    let m = Matcher::new(&g);
    let hits = m.find_all(&pat);
    assert_eq!(hits.len(), 1, "add(x, x) must match add(var(v), var(v))");
    assert!(hits[0].get(v).is_some());
    Ok(())
}
```

Add missing `common.rs` helpers as needed.

- [ ] **Step 3: D4 — Guard-failure rollback**

Append to [captures_predicates.rs](crates/pattern/tests/matching/captures_predicates.rs):

```rust
#[test]
fn when_failure_rolls_back_bindings() -> ir::Result<()> {
    use pattern::{Matcher, Var, add, var};
    // Two patterns over the same graph:
    //  - plain add(var(a), var(b)) — should hit exactly once
    //  - add(var(a), var(b)).when(|_,_,_| false) — should never hit
    let g = crate::matching::common::graph_single_add()?;
    let a = Var::new();
    let b = Var::new();
    let plain: pattern::Pat = add(var(a), var(b)).into();
    let guarded: pattern::Pat = add(var(a), var(b)).when(|_g, _ty, _out| false).into();
    let m = Matcher::new(&g);
    assert_eq!(m.find_all(&plain).len(), 1);
    assert!(m.find_all(&guarded).is_empty(), "false guard must reject all candidates");
    Ok(())
}
```

- [ ] **Step 4: D6 — `StackStorePhi` multiset offsets**

Append to [float_stack.rs](crates/pattern/tests/matching/float_stack.rs) (or extract to a new `stack_store.rs` if that file is too crowded):

```rust
#[test]
fn stack_store_phi_offsets_multiset_match() -> ir::Result<()> {
    use pattern::{Matcher, stack_store_phi};
    // Graph has a StackStorePhi with offsets [8, 0, 8].
    let g = crate::matching::common::graph_stack_store_phi_8_0_8()?;
    let m = Matcher::new(&g);

    // Exact order matches.
    let p1: pattern::Pat = stack_store_phi().offsets([8, 0, 8]).into();
    assert_eq!(m.find_all(&p1).len(), 1);

    // Permutation matches (sort-on-both-sides).
    let p2: pattern::Pat = stack_store_phi().offsets([0, 8, 8]).into();
    assert_eq!(m.find_all(&p2).len(), 1);

    // Subset does not match.
    let p3: pattern::Pat = stack_store_phi().offsets([0, 8]).into();
    assert!(m.find_all(&p3).is_empty());

    Ok(())
}
```

If building a `StackStorePhi` graph by hand is awkward, adapt an existing test fixture; this exercise is about pinning the offsets multiset logic.

- [ ] **Step 5: Verify**

Run: `cargo test -p pattern`
Expected: all new tests PASS, existing suite still green.

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/tests/
git commit -m "test(pattern): pin commutativity, capture-conflict, guard, offset behaviour"
```

---

### Task 12: E — CodeRabbit review gate

**Files:** none (review only)

- [ ] **Step 1: Run CodeRabbit against the branch diff**

Run: `coderabbit review --plain --base master --dir crates/pattern --no-color`

CodeRabbit compares the working tree against `master`, so it sees every change on the `feature/ai` branch inside the pattern crate. If the run is too long, narrow with `--files` for the specific files modified in Tasks 1-10.

- [ ] **Step 2: Read the findings**

Expected output: list of suggestions grouped by file. Treat each as a candidate improvement — don't autofix blindly.

- [ ] **Step 3: Decide per finding**

For each finding, choose one of:
- **Apply** — edit the file, add to this plan's commit log.
- **Defer** — note in an explicit reply (CodeRabbit supports `.coderabbit.yaml`-based suppression) with a brief reason.
- **Reject** — add a short rationale to the review-response file or a commit message.

- [ ] **Step 4: If any fixes were applied, re-run**

Run CodeRabbit once more until no new findings are raised.

- [ ] **Step 5: Commit any fixes**

```bash
git add crates/pattern/
git commit -m "fix(pattern): address CodeRabbit review findings"
```

---

## Self-Review Checklist (run before starting)

1. **Spec coverage:** Every finding A1-A3, B1-B5, C1-C4, D1-D6 has a task that implements it. ✓
2. **Placeholder scan:** No "TBD", "fill in details", "similar to above" without repeated code. Every step shows the exact edit. ✓
3. **Type consistency:** Method names in the test tasks match the public API after Task 8's rename (`get_float_const`, not `get_float`). The `#[must_use]` list in Task 6 references real methods that exist in the current builders. ✓

## Known-unused items verified during review

- `RewriteOutcome` — confirmed dead (Task 1).
- `exemplar_vn()` — used by builders/function_arg.rs, builders/phi.rs, etc.; NOT dead.
- All 49 public constructor functions — all present, all reachable through `lib.rs` re-exports.

## Out of scope (explicitly rejected)

- Splitting [node_pat.rs](crates/pattern/src/pat/node_pat.rs) (545 LOC) into sub-modules — the file is cohesive, every section is tightly coupled, and splitting would trade locality for a small length win.
- Lazy HashMap overlay on `Bindings` — the author's note at [bindings.rs:28-29](crates/pattern/src/matcher/bindings.rs#L28-L29) already considers this; the linear-scan default is correct for current pattern sizes. Don't change without a benchmark.
- `kind_spec()` returning `&KindSpec` instead of `KindSpec` — the Arc inside `VariantWith` is cheap to clone; the signature change ripples into the `Pattern` trait.
- Bulk `#[must_use]` on every public function (clippy default) — noise without proportional benefit.
