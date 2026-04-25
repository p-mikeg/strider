# entity-utils / graphmock / graphwalk Review (Round 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the few real correctness/readability gaps that survived rounds 1–3 in [crates/entity-utils](../../../crates/entity-utils), [crates/graphmock](../../../crates/graphmock), and [crates/graphwalk](../../../crates/graphwalk): document the asymmetric root-visit-order semantics of `PreOrder` (which currently silently visits roots in *reverse* iteration order), pin that behaviour with a regression test, and tighten the `graphmock::graph` parser by dropping a redundant `Vec` allocation.

**Architecture:** No structural changes. The three crates pass `cargo clippy --all-targets -- -D warnings` and `-W clippy::pedantic -W clippy::nursery` clean as of round 3 (only workspace-wide `clippy::cargo` package-metadata warnings, out of scope). Round 4 is doc-only on `graphwalk`, one new test on `graphwalk`, and one localised parser cleanup on `graphmock`. **No public-API changes**, **no behaviour changes** (the PreOrder root-order is already-existing; we only document and pin it).

**Tech Stack:** Rust 2024, `cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`. Workspace lints: `unwrap_used / expect_used / panic / unreachable / todo` denied; pedantic/nursery checked at the gate but not enforced workspace-wide.

**Working directory:** All commands assume `cwd = /home/mike/Desktop/strider/.worktrees/graph-crates-review-4`. Branch: `review/graph-crates-review-4`.

---

## Review Findings — Executive Summary

After re-reading every line of [crates/entity-utils/src/](../../../crates/entity-utils/src/), [crates/graphmock/src/](../../../crates/graphmock/src/), [crates/graphwalk/src/](../../../crates/graphwalk/src/), the test directories, and the only in-tree consumer ([crates/ir/src/walk.rs](../../../crates/ir/src/walk.rs)) — and after running `cargo test`, `cargo clippy -- -D warnings`, and `cargo clippy -- -W clippy::pedantic -W clippy::nursery` on the three crates:

- All data-structure invariants (worklist dedup, bitset iteration order, post-order RPO root-order, self-loop handling, `NopTracker` tree-only contract, `Iter` fused-ness) are correct and have round 1/2/3 regression tests pinning them.
- The crates compile and pass tests on this worktree's baseline (9 entity-utils + 9 graphmock + 12 graphwalk tests, all green).
- `cargo clippy -p entity-utils -p graphmock -p graphwalk --all-targets -- -D warnings -W clippy::pedantic -W clippy::nursery` is **clean** (zero code warnings on the three crates; the cargo-metadata warnings about `opt`/`pattern` are workspace-wide and out of scope for this review).

The remaining items are narrow:

### A. Correctness / contract-clarity

- **A1 — `PreOrderContext::reset` silently visits roots in REVERSE iteration order.** [crates/graphwalk/src/lib.rs:140-144](../../../crates/graphwalk/src/lib.rs#L140-L144) extends the stack with the roots verbatim and pops LIFO, so passing `[a, x]` yields `x` first. Compare [crates/graphwalk/src/lib.rs:241-256](../../../crates/graphwalk/src/lib.rs#L241-L256), where `PostOrderContext::reset` carries an explicit doc-comment guaranteeing source-order preservation in any RPO. The asymmetry is real and surprising. **Decision: document, do not change behaviour.**
  - `ir/src/walk.rs` (the only in-tree consumer of `graphwalk::PreOrder`) calls it with a single root via `iter::once(entry)` ([crates/ir/src/walk.rs:115-117](../../../crates/ir/src/walk.rs#L115-L117)), so root order is moot for it; but a future caller passing multiple roots would hit this footgun. A doc note plus a regression test is the right level of fix — a behaviour change risks subtly altering `walk_graph`'s yield order (the `ir` validator and several optimisation passes consume it) and round-3 already left the door open by adding the post-order `multi_root_preserves_root_order_in_rpo` test without a pre-order twin.
  - Out of scope here: changing `PreOrderContext::reset` to push roots in reverse so it matches the post-order semantic. That is a real refactor (downstream visit order changes), best handled in its own focused round if/when a multi-root pre-order consumer materialises.

### B. Simplification / readability

- **B1 — `graphmock::graph` parser collects `preds` into a `Vec` it doesn't need.** [crates/graphmock/src/lib.rs:128-141](../../../crates/graphmock/src/lib.rs#L128-L141) collects both `preds` and `succs` into `Vec<&str>` so a single `chain()` validation loop can poke each name. Only `succs` actually needs to be a `Vec` (it is iterated `preds.len()` times in the inner loop). `preds` can stream — validate inline, then iterate once. One allocation removed; one `for x in y.iter().chain(z.iter())` validation pass folded into the natural iteration; no behaviour change.

### C. No-op for round 4 (call out for the user)

- **C1 — `Worklist` (entity-utils) is unused outside its own crate.** Round 2 already noted this and chose to keep it as a documented general-purpose primitive; round 3 agreed. Re-flagging because nothing has changed. **Recommendation: still leave as-is.**
- **C2 — `PredGraphRef` (graphwalk) has no callers outside `graphmock`'s own impl.** Same shape as C1: documented public API, no consumer in-tree. **Recommendation: leave as-is.**
- **C3 — Pedantic / nursery sweep is already clean** for these three crates. Restriction-group warnings (e.g., `clippy::missing_inline_in_public_items`, `clippy::implicit_return`) are explicitly opt-in lints not required by the project and produce hundreds of false positives across all dependencies. **Out of scope.**

### D. Out of scope (explicit rejections)

- No new public API beyond what's in the existing crates (no `Worklist::with_capacity`, no `DenseEntitySet::extend`, etc.).
- No re-organisation of `graphwalk` (already small enough).
- No change to `entity-utils::set::Iter` (already fused, doc'd, IntoIterator-supporting).
- No removal of unused-but-documented public items (`Worklist`, `PredGraphRef`).

---

## File touch map

| File | What happens |
|------|--------------|
| [crates/graphwalk/src/lib.rs](../../../crates/graphwalk/src/lib.rs) | A1 — extend `PreOrderContext::reset`'s doc-comment to spell out the LIFO root-visit-order semantics, mirror the `PostOrderContext::reset` style. |
| [crates/graphwalk/tests/preorder.rs](../../../crates/graphwalk/tests/preorder.rs) | A1 — add `multi_root_visited_in_reverse_iteration_order` regression test pinning the documented behaviour. |
| [crates/graphmock/src/lib.rs](../../../crates/graphmock/src/lib.rs) | B1 — drop the `preds: Vec<&str>` allocation; inline empty-name validation. |

No `Cargo.toml` changes. No workspace-lint changes.

---

## Verification gate (run after every task)

```bash
cargo test  -p entity-utils -p graphwalk -p graphmock
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -D warnings
```

Both must pass. The pedantic/nursery sweep is verified once at the end (Task 4):

```bash
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -W clippy::pedantic -W clippy::nursery
```

Expected: zero code warnings (cargo-metadata warnings on `opt`/`pattern` are workspace-wide and out of scope).

The full-workspace gate is also Task 4.

---

## Tasks

### Task 1: A1 (doc) — Document `PreOrderContext::reset`'s LIFO root-visit order

**Files:**
- Modify: [crates/graphwalk/src/lib.rs:140-144](../../../crates/graphwalk/src/lib.rs#L140-L144)

- [ ] **Step 1: Replace the doc comment + body of `PreOrderContext::reset`**

In [crates/graphwalk/src/lib.rs](../../../crates/graphwalk/src/lib.rs), find the existing implementation:

```rust
    /// Resets the traversal, replacing the current stack with `roots`.
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();
        self.stack.extend(roots);
    }
```

(inside `impl<N: Copy> PreOrderContext<N>` at lines 140–144).

Replace it with:

```rust
    /// Resets the traversal, replacing the current stack with `roots`.
    ///
    /// **Root visit order:** because the internal stack is popped LIFO, roots are
    /// visited in **reverse** of their iteration order. That is, if `roots`
    /// yields `[u, v]` (with no path from `v` to `u` in the graph), the pre-order
    /// walk yields `v` (and its subtree) before `u`. This is the *opposite* of
    /// [`PostOrderContext::reset`], which is carefully shaped so source order is
    /// preserved in any RPO derived from a post-order walk.
    ///
    /// Callers that want forward source-order over multiple roots should
    /// reverse the iterator themselves (e.g. `roots.into_iter().rev()`).
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();
        self.stack.extend(roots);
    }
```

The body is unchanged; only the doc-comment is extended.

- [ ] **Step 2: Verify**

Run:

```bash
cargo test  -p graphwalk
cargo clippy -p graphwalk --all-targets -- -D warnings
cargo doc   -p graphwalk --no-deps
```

Expected: all tests pass; clippy clean; rustdoc renders the new section without warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/graphwalk/src/lib.rs
git commit -m "$(cat <<'EOF'
docs(graphwalk): document PreOrderContext::reset LIFO root-visit order

PostOrderContext::reset documents that source order is preserved in any
RPO derived from a post-order walk.  PreOrderContext::reset has the
opposite property — roots are visited in REVERSE iteration order because
the internal stack is popped LIFO — but until now this was implicit.

Spell out the asymmetry and tell callers how to recover forward order.
No behaviour change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: A1 (test) — Pin the reverse root-order semantics for `PreOrder`

**Files:**
- Modify: [crates/graphwalk/tests/preorder.rs](../../../crates/graphwalk/tests/preorder.rs) (append a single new `#[test]` fn at the end of the file)

- [ ] **Step 1: Write the failing regression test**

Append to [crates/graphwalk/tests/preorder.rs](../../../crates/graphwalk/tests/preorder.rs), after `repeated_successor_is_visited_once` (currently the last test):

```rust
#[test]
fn multi_root_visited_in_reverse_iteration_order() {
    // Doc-comment in PreOrderContext::reset promises: if `u` precedes `v` in
    // `roots` and there's no path from v to u, then `v` is visited before `u`
    // in pre-order (LIFO stack semantics — the OPPOSITE of post-order).
    // Build two disjoint chains (a -> b, x -> y) and pass roots in [a, x] order.
    let g = graphmock::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let order: Vec<_> = entity_preorder(&g, [a, x])
        .map(|n| g.name(n).to_owned())
        .collect();
    let pos_a = order.iter().position(|s| s == "a").unwrap();
    let pos_x = order.iter().position(|s| s == "x").unwrap();
    assert!(
        pos_x < pos_a,
        "expected x (second root) to precede a in pre-order \
         (LIFO root visit order), got {order:?}"
    );
}
```

- [ ] **Step 2: Run it to confirm it passes**

Run:

```bash
cargo test -p graphwalk --test preorder multi_root_visited_in_reverse_iteration_order
```

Expected: PASS — the invariant already holds; we are pinning it.

- [ ] **Step 3: Sanity-check the whole graphwalk test suite**

Run:

```bash
cargo test -p graphwalk
cargo clippy -p graphwalk --all-targets -- -D warnings
```

Expected: all tests pass; clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/graphwalk/tests/preorder.rs
git commit -m "$(cat <<'EOF'
test(graphwalk): pin PreOrder reverse-root-order invariant

Mirror multi_root_preserves_root_order_in_rpo (post-order) with a
pre-order test that pins the OPPOSITE invariant: roots are visited in
reverse iteration order due to LIFO stack pop.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: B1 — Drop the redundant `preds: Vec` allocation in `graphmock::graph`

**Files:**
- Modify: [crates/graphmock/src/lib.rs:113-141](../../../crates/graphmock/src/lib.rs#L113-L141) — the body of the `for line in input.lines()` loop in `pub fn graph(input: &str) -> Graph`.

This is a self-contained simplification: drop one `Vec` allocation and one validation pass. **No behaviour change**: the existing `#[should_panic(expected = "graphmock: empty node name")]` tests at lines 263-280 (`empty_succ_token_panics`, `empty_pred_token_panics`, `trailing_comma_panics`) keep passing, because we still panic on the same conditions with the same message prefix.

- [ ] **Step 1: Replace the loop body**

In [crates/graphmock/src/lib.rs](../../../crates/graphmock/src/lib.rs), find the existing `for line in input.lines()` block (currently lines 108–141):

```rust
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // graphmock is a test-only DSL helper; input is a hard-coded string in
        // downstream tests, so a malformed line is a programmer error, not a
        // runtime condition that deserves error plumbing.
        #[allow(clippy::panic)]
        let (preds, succs) = line
            .split_once("->")
            .unwrap_or_else(|| panic!("graphmock: line missing `->`: {line:?}"));

        let check_name = |name: &str| {
            assert!(
                !name.is_empty(),
                "graphmock: empty node name in line: {line:?}"
            );
        };

        let preds: Vec<&str> = preds.split(',').map(str::trim).collect();
        let succs: Vec<&str> = succs.split(',').map(str::trim).collect();
        for name in preds.iter().chain(succs.iter()) {
            check_name(name);
        }

        for pred in &preds {
            let pred = graph.get_or_create(pred);
            for succ in &succs {
                let succ = graph.get_or_create(succ);
                graph.add_succ(pred, succ);
            }
        }
    }
```

Replace it with:

```rust
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // graphmock is a test-only DSL helper; input is a hard-coded string in
        // downstream tests, so a malformed line is a programmer error, not a
        // runtime condition that deserves error plumbing.
        #[allow(clippy::panic)]
        let (preds, succs) = line
            .split_once("->")
            .unwrap_or_else(|| panic!("graphmock: line missing `->`: {line:?}"));

        let check_nonempty = |name: &str| {
            assert!(
                !name.is_empty(),
                "graphmock: empty node name in line: {line:?}"
            );
        };

        // `succs` is iterated once per pred, so it must be collected.  `preds`
        // is iterated only once, so we stream it and validate each name inline.
        let succs: Vec<&str> = succs.split(',').map(str::trim).collect();
        for &succ in &succs {
            check_nonempty(succ);
        }

        for pred in preds.split(',').map(str::trim) {
            check_nonempty(pred);
            let pred = graph.get_or_create(pred);
            for &succ in &succs {
                let succ = graph.get_or_create(succ);
                graph.add_succ(pred, succ);
            }
        }
    }
```

Notes:
- The closure is renamed `check_nonempty` to make the call sites self-describing (`check_nonempty(pred)` reads as a guard).
- The inner `for &succ in &succs` matches the outer `for &succ in &succs` so `succ` is `&str` consistently. (Matches the existing inner-loop pattern; no auto-deref surprise.)
- The `#[allow(clippy::panic)]` only gates the `unwrap_or_else(|| panic!(...))` line; `assert!` inside `check_nonempty` is not flagged by `clippy::panic` (it's a separate macro).

- [ ] **Step 2: Run all graphmock tests + clippy**

Run:

```bash
cargo test  -p graphmock
cargo clippy -p graphmock --all-targets -- -D warnings
```

Expected: all 12 graphmock tests pass (including the three `#[should_panic(expected = "graphmock: empty node name")]` cases); clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(graphmock): stream preds, drop redundant Vec allocation

graph() collected both preds and succs into Vec<&str> just so a single
chain() pass could validate empty names.  preds is iterated only once,
so stream it and inline the empty-name check.  Same panic shape, same
tests pass, one less allocation and one less validation pass per line.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Final clippy + workspace gate

**Files:** (none — verification only)

- [ ] **Step 1: Verify the three crates pass strict clippy**

Run from the worktree root:

```bash
cargo clippy -p entity-utils -p graphmock -p graphwalk --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 2: Verify the full workspace still builds and tests**

```bash
cargo build --workspace
cargo test  --workspace
```

Expected: clean build; all workspace tests pass. (`ir` is the only non-test consumer of `entity-utils` and `graphwalk`; this catches accidental API breakage.)

- [ ] **Step 3: Verify pedantic + nursery still pass clean on the three reviewed crates**

```bash
cargo clippy -p entity-utils -p graphmock -p graphwalk --all-targets -- \
    -W clippy::pedantic -W clippy::nursery
```

Expected: zero code warnings (matches the round-3 baseline). Cargo-metadata warnings from `clippy::cargo` are workspace-wide and out of scope.

- [ ] **Step 4: Verify rustdoc renders cleanly**

```bash
cargo doc -p entity-utils -p graphmock -p graphwalk --no-deps
```

Expected: clean build, no warnings. The new doc-comment on `PreOrderContext::reset` (Task 1) is the only new rustdoc surface.

If any new code warnings appear from the changes above, address them inline before landing the round.

---

## Self-Review

- **Spec coverage:** every finding listed in the executive summary maps to exactly one task.
  - A1 → Tasks 1 (doc) + 2 (test)
  - B1 → Task 3
  - C1, C2, C3 — explicit no-ops with rationale; no task needed.
  - D — explicit out-of-scope; no task needed.
  - Verification gate (E) → Task 4.
- **Placeholder scan:** none. Every step has the literal new code or command.
- **Type consistency:** no new types or trait signatures introduced. The only new closure (`check_nonempty` in Task 3) takes `&str` and returns `()` — same shape as the existing `check_name` it replaces.
- **Order independence:** Tasks 1–3 each touch a single, distinct file (Task 1: graphwalk lib.rs, Task 2: graphwalk preorder.rs test, Task 3: graphmock lib.rs). They commute and may be executed in any order. Task 4 must run last.
- **Risk assessment:** Tasks 1 and 2 are doc + test — zero behaviour-change risk. Task 3 is a refactor whose preserved behaviour is pinned by the existing `#[should_panic]` tests in graphmock (`empty_succ_token_panics`, `empty_pred_token_panics`, `trailing_comma_panics`) plus the integration tests in `crates/graphwalk/tests/{preorder,postorder}.rs` that use the parser. If Task 3 broke parsing, those break first.
