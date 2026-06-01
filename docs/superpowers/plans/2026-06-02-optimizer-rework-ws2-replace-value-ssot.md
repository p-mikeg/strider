# Optimizer Rework — Workstream 2: value-replacement SSoT Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make a single `Function::replace_value(old, new)` primitive the one place that pairs asm-fingerprint absorption with use-replacement, and route every value-rewrite pass through it (directly or via `after_replace`), deleting the scattered manual `extend_asm_fingerprint_from(new, old); replace_all_uses(old, new)` pairs.

**Architecture:** Today the "absorb fingerprint + replace uses" idiom is hand-written in ≥5 pass sites and also encapsulated by `OptimizationResult::after_replace` (which itself hand-writes it). WS2 introduces `Function::replace_value` as the single implementation; `after_replace` delegates to it, and the `&mut Function`/`RewriteCtx` pass sites call it directly. This delivers item 3's "no manual extend_asm SSoT" for value replacements. Structural edits (`remove_node_input`, `update_input`, branch-consumer swap) and fresh-subtree attribution (`create_node_attributed`) are out of scope — they belong to WS3's `RewriteOp` work and remain untouched here.

**Tech Stack:** Rust; `strider-ir` (`Function`, `Graph::replace_all_uses`, `extend_asm_fingerprint_from`), `strider-analyze` opt passes, `strider-pattern` `RewriteCtx` (derefs to `Function`).

**Spec:** `docs/superpowers/specs/2026-06-01-optimizer-rework-design.md` (D4, item 3 — value-rewrite slice).

**Scope boundary (explicit):** WS2 migrates ONLY value replacements (`extend_asm_fingerprint_from(X, n); replace_all_uses(out, X)` pairs). It does NOT touch: `detach_node_inputs` (Tier 2 orphan cleanup — kept), `remove_node_input`/`update_input` (structural — WS3), `create_node_attributed` (the fresh-node attribution SSoT — kept), or `if_cond_inversion`/`indirect_branch_resolve`/`worklist`/`mem_walk` fingerprint sites (WS3+). The global "no manual fingerprint in opt/" enforcement test is deferred to the end of WS3, once every site has a home.

---

## File structure

- `crates/strider-ir/src/function.rs` — add `Function::replace_value` next to `extend_asm_fingerprint_from` (~line 460).
- `crates/strider-analyze/src/opt/pipeline.rs:46-57` — `after_replace` delegates to `replace_value`.
- `crates/strider-analyze/src/opt/load_readonly/mod.rs:164-167` — use `replace_value`.
- `crates/strider-analyze/src/opt/redundant_phis/mod.rs:51-52,164-165,175-176` — use `replace_value`.
- `crates/strider-analyze/src/opt/dead_branch/mod.rs:178-179` — use `replace_value`.
- `crates/strider-analyze/src/opt/load_forward/mod.rs:150-151` — use `replace_value`.

---

### Task 1: `Function::replace_value` SSoT primitive

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (add after `extend_asm_fingerprint_from`, ~line 467)
- Test: `crates/strider-ir/src/function.rs` (existing `#[cfg(test)]` module, or wherever Function unit tests live — search the file for `mod tests`)

- [ ] **Step 1: Write the failing test**

Find the Function unit-test module (`grep -n "mod tests" crates/strider-ir/src/function.rs`). If one exists, add this test; if the crate keeps Function tests elsewhere, add it adjacent to existing `extend_asm_fingerprint_from` / `replace_all_uses` tests (search the crate: `grep -rn "fn.*replace_all_uses" crates/strider-ir/src/ | grep test`). Adapt node construction to the helpers used by neighbouring tests.

```rust
#[test]
fn replace_value_absorbs_fingerprint_and_redirects_uses() -> crate::error::Result<()> {
    // Build: a producer `old` whose value is consumed by a sink, plus a
    // replacement `new`. replace_value(old, new) must (a) redirect the sink
    // to `new`, and (b) union old's asm-fingerprint into new's.
    let mut f = /* build a Function with: old (IntConst, fp={0xAA}), new (IntConst, fp={0xBB}), sink consuming old */;
    // ... see neighbouring tests for the exact builder calls ...
    let changed = f.replace_value(old_out, new_out)?;
    assert!(changed, "a live use existed, so replace must report changed");
    // new's fingerprint now includes old's address.
    let new_node = f.node_for_output(new_out);
    assert!(f.asm_fingerprint(new_node).contains(&0xAA));
    assert!(f.asm_fingerprint(new_node).contains(&0xBB));
    // sink now consumes new, not old.
    // ... assert the sink's input now points at new_out ...
    Ok(())
}
```

The implementer should write the concrete builder calls by copying the construction idiom from the nearest existing test that exercises `replace_all_uses` + `extend_asm_fingerprint_from` (e.g. in `crates/strider-ir/src/ops/rewrite.rs` tests or `function.rs` tests). The assertions that MUST hold: changed==true, new's fingerprint ⊇ {old's addr, new's own addr}, sink redirected.

- [ ] **Step 2: Run the test — verify it FAILS**

`cargo test -p strider-ir replace_value -- --nocapture`
Expected: compile error `no method named 'replace_value'`.

- [ ] **Step 3: Implement `replace_value`**

In `crates/strider-ir/src/function.rs`, immediately after `extend_asm_fingerprint_from` (~line 467), add:

```rust
/// The single value-replacement primitive: redirect every use of `old`
/// to `new`, after **absorbing** `old`'s producer asm-fingerprint into
/// `new`'s producer (superset-only union).
///
/// This is the one place that pairs fingerprint absorption with
/// use-replacement — optimization passes call this instead of hand-writing
/// `extend_asm_fingerprint_from(new, old)` followed by
/// `replace_all_uses(old, new)`, so the superset-only fingerprint contract
/// has exactly one implementation for value rewrites.
///
/// Returns `true` iff at least one use was redirected (mirrors
/// [`Graph::replace_all_uses`]).
///
/// # Errors
/// Propagates [`Graph::replace_all_uses`]'s error arm unchanged.
pub fn replace_value(
    &mut self,
    old: crate::node::NodeOutputId,
    new: crate::node::NodeOutputId,
) -> crate::error::Result<bool> {
    let old_node = self.node_for_output(old);
    let new_node = self.node_for_output(new);
    self.extend_asm_fingerprint_from(new_node, old_node);
    self.replace_all_uses(old, new)
}
```

Confirm the return type matches `replace_all_uses` (it returns `crate::error::Result<bool>` per `crates/strider-ir/src/ops/rewrite.rs:18`). If `replace_all_uses` returns a different `Result` alias, match it exactly. `node_for_output`, `extend_asm_fingerprint_from`, `replace_all_uses` are all reachable on `Function` (the latter via `Deref` to `Graph` — confirm it resolves; if not, call `self.graph_mut().replace_all_uses(...)` or the equivalent mutable-graph accessor used elsewhere in `function.rs`).

- [ ] **Step 4: Run the test — verify it PASSES**

`cargo test -p strider-ir replace_value -- --nocapture` → PASS.

- [ ] **Step 5: Whole IR crate green + clippy**

`cargo test -p strider-ir` then `cargo clippy -p strider-ir --no-deps` → no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-ir/src/function.rs
git commit -m "feat(strider-ir): add Function::replace_value SSoT (absorb fingerprint + replace uses)"
```

---

### Task 2: `after_replace` delegates to `replace_value`; migrate `LoadReadOnly`

**Files:**
- Modify: `crates/strider-analyze/src/opt/pipeline.rs:46-57`
- Modify: `crates/strider-analyze/src/opt/load_readonly/mod.rs:159-167`

- [ ] **Step 1: Delegate `after_replace`**

Replace the body of `OptimizationResult::after_replace` (pipeline.rs:52-56) so it calls the new primitive instead of hand-writing the pair:

```rust
pub fn after_replace(
    self,
    function: &mut strider_pattern::RewriteCtx<'_>,
    old: strider_ir::node::NodeOutputId,
    new: strider_ir::node::NodeOutputId,
) -> crate::opt::Result<Self> {
    // `RewriteCtx` derefs to `Function`; `replace_value` is the SSoT that
    // absorbs `old`'s fingerprint into `new` and redirects all uses.
    let changed = function.replace_value(old, new)?;
    Ok(self | OptimizationResult::from_changed(changed))
}
```

Update the doc comment above it to say it delegates to `Function::replace_value`. Confirm `function.replace_value(...)` resolves through `RewriteCtx`'s `DerefMut<Target = Function>` (it does — see `rewrite/mod.rs:542`). The `?` maps `strider_ir`'s error into `crate::opt::Result` via the existing `anyhow` conversion (same as today's `replace_all_uses(old, new)?`).

- [ ] **Step 2: Migrate `try_fold_const_load_at`**

In `load_readonly/mod.rs`, replace lines 159-167 (the manual absorb + replace tail) with:

```rust
    let new_out = function.make_int_const(masked, ty)?;
    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // new IntConst (so the always-on fingerprint check sees a non-empty
    // fingerprint even on the cache-hit dedup path) and redirects all uses.
    function.replace_value(data_out, new_out)
```

(Delete the `new_node`/`load_node`/`extend_asm_fingerprint_from`/`replace_all_uses` lines; `replace_value` returns the `Result<bool>` the function already returns. `function` here is `&mut Function`, so `replace_value` resolves directly.)

- [ ] **Step 3: Run the affected tests**

```
cargo test -p strider-analyze --lib load_readonly
cargo test -p strider-analyze --lib known_bits     # uses after_replace
cargo test -p strider-analyze --lib peephole       # uses after_replace
```
Expected: all green (behaviour identical — `replace_value` is the same two operations in the same order).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-analyze/src/opt/pipeline.rs crates/strider-analyze/src/opt/load_readonly/mod.rs
git commit -m "refactor(strider-analyze): route after_replace + LoadReadOnly through replace_value"
```

---

### Task 3: Migrate `RedundantPhis`, `DeadBranchElimination` redirect, `LoadForward` forward

**Files:**
- Modify: `crates/strider-analyze/src/opt/redundant_phis/mod.rs:51-52,164-165,175-176`
- Modify: `crates/strider-analyze/src/opt/dead_branch/mod.rs:178-179`
- Modify: `crates/strider-analyze/src/opt/load_forward/mod.rs:150-151`

Each site is the same shape: `ctx.extend_asm_fingerprint_from(SRC_NODE, OLD_NODE); ctx.replace_all_uses(OLD_OUT, SRC_OUT)?`. `ctx` is a `RewriteCtx` (derefs to `Function`), so each pair collapses to `ctx.replace_value(OLD_OUT, SRC_OUT)?`. The `SRC_NODE` is always `function.node_for_output(SRC_OUT)` — `replace_value` recomputes it internally, so the explicit `*_node` locals at each site become dead and should be removed if they're used ONLY for the fingerprint call.

- [ ] **Step 1: redundant_phis — three arms**

For each of the three sites (51-52, 164-165, 175-176), replace the `extend_asm_fingerprint_from(X_node, node_id); replace_all_uses(out, X)?` pair with `ctx.replace_value(out, X)?`. Read each site first: the variable names differ (`input`/`input_node`, `value`/`value_node`). Keep the surrounding `detach_node_inputs(node_id)` calls (lines 21, 183) — those are Tier-2 orphan cleanup, NOT in scope. Remove any now-unused `*_node` locals (clippy will flag them).

- [ ] **Step 2: dead_branch — redirect**

Replace lines 178-179 (`extend_asm_fingerprint_from(ctrl_in_node, if_node); replace_all_uses(live_ctrl, ctrl_in)?`) with `ctx.replace_value(live_ctrl, ctrl_in)?`. Leave the `remove_node_input` (64, 70) and `detach_node_inputs` (74) calls untouched — they are structural CFG edits scoped to WS3. Remove the now-unused `ctrl_in_node`/`if_node` locals if they were only used for the fingerprint call (check: `if_node` may still be needed elsewhere; only remove truly-dead locals).

- [ ] **Step 3: load_forward — forward**

Replace lines 150-151 (`extend_asm_fingerprint_from(forwarded_node, load); replace_all_uses(load_out, forwarded)?`) with `let changed = ctx.replace_value(load_out, forwarded)?;` (the result is consumed by the `if changed { detach... }` on 152-153 — keep that). Leave the `create_node_attributed(..., &[load])` narrow-synthesis calls (512-573) untouched — fresh-node attribution is the separate SSoT, in scope nowhere in WS2.

- [ ] **Step 4: Run the affected pass tests**

```
cargo test -p strider-analyze --lib redundant_phis
cargo test -p strider-analyze --lib dead_branch
cargo test -p strider-analyze --lib load_forward
```
Expected: all green (behaviour-preserving).

- [ ] **Step 5: Clippy (catch dead locals)**

`cargo clippy -p strider-analyze --no-deps` → no `unused variable` / `unused let` warnings from the removed fingerprint locals.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-analyze/src/opt/redundant_phis/mod.rs crates/strider-analyze/src/opt/dead_branch/mod.rs crates/strider-analyze/src/opt/load_forward/mod.rs
git commit -m "refactor(strider-analyze): route phi/branch/load-forward value replacements through replace_value"
```

---

### Task 4: Regression backstop

**Files:** none (gate only)

- [ ] **Step 1: Full workspace tests**

`cargo test --workspace` → 0 failures (fixtures already built in this worktree).

- [ ] **Step 2: Orchestrator demo smoke test**

`cargo run -p strider-analyze --example orchestrator_demo` → exit 0, three HTML dumps regenerate.

- [ ] **Step 3: Confirm the manual value-pairs are gone from the migrated passes**

```
grep -n "extend_asm_fingerprint_from" crates/strider-analyze/src/opt/load_readonly/mod.rs \
  crates/strider-analyze/src/opt/redundant_phis/mod.rs \
  crates/strider-analyze/src/opt/dead_branch/mod.rs \
  crates/strider-analyze/src/opt/load_forward/mod.rs
```
Expected: NO matches in these four files (they now route through `replace_value`). `if_cond_inversion`, `indirect_branch_resolve`, `worklist`, `mem_walk` still have sites — that's expected (WS3+ scope).

- [ ] **Step 4: Commit (if any incidental fix)**

```bash
git add -A && git commit -m "test: WS2 regression backstop for replace_value SSoT" || echo "nothing to commit"
```

---

## Self-review

**Spec coverage (D4 / item 3, value-rewrite slice):**
- SSoT primitive — Task 1 (`Function::replace_value`).
- `after_replace` unified onto it — Task 2.
- All four value-rewrite pass sites migrated — Tasks 2-3.
- Structural/fresh-node/other sites explicitly deferred to WS3 (documented in Scope boundary) — not a gap, a sequencing decision.

**Placeholder scan:** Task 1's test has a `/* build ... */` sketch because the exact builder idiom must match neighbouring Function tests — the implementer fills it by copying the nearest existing `replace_all_uses` test's construction. Every other step has concrete code. The Task 1 test's REQUIRED assertions are spelled out explicitly.

**Type consistency:** `replace_value(old: NodeOutputId, new: NodeOutputId) -> Result<bool>` used identically in Task 1 (def), Task 2 (`after_replace`, load_readonly), Task 3 (three passes). Matches `replace_all_uses`'s `Result<bool>`.

**Risk note:** behaviour is preserved by construction — `replace_value` performs the identical two operations in the identical order as every site it replaces. The existing per-pass test suites (load_readonly, redundant_phis, dead_branch, load_forward, known_bits, peephole) are the gate; any divergence means a typo in which output/node was passed.
