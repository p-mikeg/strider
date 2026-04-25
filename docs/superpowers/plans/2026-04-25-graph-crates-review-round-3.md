# entity-utils / graphmock / graphwalk Review (Round 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply the small correctness, simplification, and readability fixes that survived round 2: a parser footgun in `graphmock`, a few missing docs and `Debug`/`#[must_use]` annotations, a `FusedIterator` marker on `entity-utils::set::Iter`, a tighter preallocation hint, and one test pinning duplicate-root post-order behaviour.

**Architecture:** No structural changes. The three crates are small (1396 LOC total), already pass `cargo clippy -- -D warnings` and `-W clippy::pedantic -W clippy::nursery` clean, and round-1/round-2 already polished most rough edges. Round 3 closes a real silent footgun in `graphmock`'s parser and fills in a handful of missed annotations and docs that round 2 didn't cover.

**Tech Stack:** Rust 2024, `cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`.

**Working directory:** All commands assume `cwd = /home/mike/Desktop/strider/.worktrees/graph-crates-review-2`.

---

## Review Findings — Executive Summary

After reading every line of [crates/entity-utils/src/](crates/entity-utils/src/), [crates/graphmock/src/](crates/graphmock/src/), [crates/graphwalk/src/](crates/graphwalk/src/), and the test directories, the data-structure invariants (worklist dedup, bitset iteration order, pre/post-order DFS visit-once, root-order RPO) are correct and round-1/2 tests pin them. The crates pass `cargo clippy --all-targets -- -W clippy::pedantic -W clippy::nursery` with **zero** code warnings (only `clippy::cargo` package-metadata noise from the workspace Cargo.toml — out of scope).

**Correctness (A):**

- **A1 — `graphmock::graph` silently creates phantom empty-name nodes.** [crates/graphmock/src/lib.rs:96-108](crates/graphmock/src/lib.rs#L96-L108) — A line like `"a -> "` produces a successor whose interned name is the empty string, and `"a, -> b"` produces a phantom predecessor. The split-and-trim never rejects empty tokens. The existing `# Panics` rule says malformed input is a programmer error; treat empty tokens as malformed too. Same panic shape, regression-test pinned.
- **A2 — `Graph::node` / `Graph::name` panic without `# Panics` docs; `Graph::entry` returns a stale id on empty graphs.** [crates/graphmock/src/lib.rs:32-45](crates/graphmock/src/lib.rs#L32-L45) — `node` panics on missing key (HashMap index), `name` panics on out-of-bounds (PrimaryMap index), `entry` returns `NodeId(0)` even when no nodes were ever created (any subsequent `name(entry())` panics deep inside `PrimaryMap`). Document each — and remove the only test that constructs the empty case (`whitespace_only_input_yields_no_edges` at [lib.rs:194-203](crates/graphmock/src/lib.rs#L194-L203)) since it pins nothing observable.

**Simplification / consistency (B):**

- **B1 — `PostOrderContext` lacks `#[derive(Debug)]`.** [crates/graphwalk/src/lib.rs:227-230](crates/graphwalk/src/lib.rs#L227-L230) — `PreOrderContext` derives Debug at line 128; the post-order twin doesn't. Trivial parity fix.
- **B2 — `Worklist` has no `#[derive(Debug)]`.** [crates/entity-utils/src/worklist.rs:12-16](crates/entity-utils/src/worklist.rs#L12-L16) — derive `Debug` (the auto-generated impl picks up `E: Debug` as a bound).
- **B3 — Public `Iter<'a, E>` in entity-utils has no doc comment.** [crates/entity-utils/src/set.rs:76-79](crates/entity-utils/src/set.rs#L76-L79).
- **B4 — Public `Graph` struct in graphmock has no doc comment.** [crates/graphmock/src/lib.rs:26-29](crates/graphmock/src/lib.rs#L26-L29).
- **B5 — `PreOrder::new` / `PostOrder::new` lack `#[must_use]`** while `*Context::new` have it. [crates/graphwalk/src/lib.rs:191-202](crates/graphwalk/src/lib.rs#L191-L202), [crates/graphwalk/src/lib.rs:322-339](crates/graphwalk/src/lib.rs#L322-L339).

**Robustness / micro-perf (C):**

- **C1 — `entity-utils::set::Iter` doesn't impl `FusedIterator`.** [crates/entity-utils/src/set.rs:81-87](crates/entity-utils/src/set.rs#L81-L87) — the underlying `cranelift_bitset::compound::Iter` is naturally fused (once it returns `None`, it keeps returning `None`); the marker is free and lets callers using `core::iter::Fuse` skip the wrapper.
- **C2 — `DenseEntitySet::from_iter` preallocates from `min_size` only.** [crates/entity-utils/src/set.rs:107-117](crates/entity-utils/src/set.rs#L107-L117) — switch to `upper.unwrap_or(lower)` so size-known iterators (e.g. collecting from a `Vec` or `Range`) preallocate the full capacity.

**Test coverage gaps (D):**

- **D1 — No test pins "duplicate root in `roots` is visited only once" for `PostOrder`.** Implicit in the diamond test, but the duplicate-root case isn't exercised. Add an explicit test.

**Out of scope:** No new public API beyond what's needed to fix the above. No reorganisation of `graphwalk` into multiple files. No `iter()`/`peek()` on `Worklist` (YAGNI per project convention). The `clippy::cargo` package-metadata warnings are workspace-wide and not specific to these crates.

**Clippy gate (E):** `cargo clippy --workspace --all-targets -- -D warnings` must remain clean after all changes. Run as the final task.

---

## File touch map

| File | What happens |
|------|--------------|
| [crates/graphmock/src/lib.rs](crates/graphmock/src/lib.rs) | A1 (parser empty-token panic + 3 should_panic tests), A2 (doc panics on `node`/`name`/`entry`, drop weak test), B4 (Graph doc). |
| [crates/graphwalk/src/lib.rs](crates/graphwalk/src/lib.rs) | B1 (Debug on PostOrderContext), B5 (must_use on Pre/PostOrder::new). |
| [crates/graphwalk/tests/postorder.rs](crates/graphwalk/tests/postorder.rs) | D1 (duplicate-root test). |
| [crates/entity-utils/src/set.rs](crates/entity-utils/src/set.rs) | B3 (Iter doc), C1 (FusedIterator + tiny test), C2 (size-hint upper bound). |
| [crates/entity-utils/src/worklist.rs](crates/entity-utils/src/worklist.rs) | B2 (Debug derive + smoke test). |

No `Cargo.toml` changes. No workspace lint changes.

---

## Verification gate (run after every task)

```bash
cargo test  -p entity-utils -p graphwalk -p graphmock
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -D warnings
```

Both must pass. The pedantic/nursery sweep is verified once at the end (Task 9):

```bash
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -W clippy::pedantic -W clippy::nursery
```

Expected: zero code warnings (cargo-metadata warnings are workspace-wide and ignored).

---

## Tasks

### Task 1: A1 — Reject empty-name tokens in `graphmock::graph`

**Files:**
- Modify: [crates/graphmock/src/lib.rs:81-112](crates/graphmock/src/lib.rs#L81-L112)
- Test: extend the `#[cfg(test)] mod tests` block in the same file

- [ ] **Step 1: Write the failing regression tests**

Append inside the `mod tests { … }` block (after `name_recurrence_resolves_to_same_id` at the end) of [crates/graphmock/src/lib.rs](crates/graphmock/src/lib.rs):

```rust
    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn empty_succ_token_panics() {
        // "a -> " trims to ("a", ""): the empty successor used to silently
        // create a phantom node.  Reject as malformed.
        let _ = graph("a -> ");
    }

    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn empty_pred_token_panics() {
        let _ = graph(" -> b");
    }

    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn trailing_comma_panics() {
        let _ = graph("a, -> b");
    }
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p graphmock empty_ trailing_comma`
Expected: FAIL — `graph()` currently does not panic for empty tokens.

- [ ] **Step 3: Reject empty tokens in the parser**

Replace the body of the `for line in input.lines() { … }` loop in `graph()` at [crates/graphmock/src/lib.rs:87-109](crates/graphmock/src/lib.rs#L87-L109) with:

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

        #[allow(clippy::panic)]
        let check = |name: &str| {
            if name.is_empty() {
                panic!("graphmock: empty node name in line: {line:?}");
            }
            name
        };

        let preds: Vec<_> = preds.split(',').map(str::trim).map(check).collect();
        let succs: Vec<_> = succs.split(',').map(str::trim).map(check).collect();

        for pred in &preds {
            let pred = graph.get_or_create(pred);
            for succ in &succs {
                let succ = graph.get_or_create(succ);
                graph.add_succ(pred, succ);
            }
        }
    }
```

(`preds` becomes a `Vec` so we can iterate it once for validation+collection and again for edge insertion; `check` collapses the empty-name guard into the existing iterator pipeline.)

- [ ] **Step 4: Run all graphmock tests + clippy**

Run: `cargo test -p graphmock && cargo clippy -p graphmock --all-targets -- -D warnings`
Expected: all tests pass (including the three new `#[should_panic]` cases); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "$(cat <<'EOF'
fix(graphmock): reject empty node names in DSL parser

A line like "a -> " or " -> b" used to silently create a phantom
empty-name node.  Treat empty tokens as malformed input, same shape
as the existing "missing ->" panic.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: A2 — Document panic conditions on `Graph::{node,name,entry}`; drop the weak test

**Files:**
- Modify: [crates/graphmock/src/lib.rs:31-45](crates/graphmock/src/lib.rs#L31-L45)
- Modify: [crates/graphmock/src/lib.rs:194-203](crates/graphmock/src/lib.rs#L194-L203) (delete `whitespace_only_input_yields_no_edges`)

- [ ] **Step 1: Add `# Panics` docs to the three accessors**

Replace the public-method block in `impl Graph` at [crates/graphmock/src/lib.rs:32-45](crates/graphmock/src/lib.rs#L32-L45):

```rust
    #[must_use]
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }

    #[must_use]
    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

    #[must_use]
    pub fn name(&self, node: NodeId) -> &str {
        &self.nodes[node].name
    }
```

with:

```rust
    /// Returns the conventional entry node id (`NodeId(0)`).
    ///
    /// **Precondition:** the input passed to [`graph`] declared at least one
    /// edge — i.e. the graph contains at least one node.  Calling this on an
    /// empty graph returns a stale id that will panic when used as a key into
    /// [`Graph::name`] or any traversal.
    #[must_use]
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }

    /// Looks up a node by the name it was given in the DSL.
    ///
    /// # Panics
    ///
    /// Panics if `name` was never declared in the input passed to [`graph`].
    #[must_use]
    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

    /// Returns the DSL name of `node`.
    ///
    /// # Panics
    ///
    /// Panics if `node` did not originate from this graph.
    #[must_use]
    pub fn name(&self, node: NodeId) -> &str {
        &self.nodes[node].name
    }
```

- [ ] **Step 2: Delete the weak `whitespace_only_input_yields_no_edges` test**

The test at [lib.rs:194-203](crates/graphmock/src/lib.rs#L194-L203) creates a graph and asserts only `let _ = g;`. After Task 1, whitespace-only input is still legal (lines that trim to empty are skipped before the `->` check), but the test pins nothing observable. Delete the entire `#[test] fn whitespace_only_input_yields_no_edges()` block and any blank line attached to it.

- [ ] **Step 3: Verify**

Run: `cargo test -p graphmock && cargo doc -p graphmock --no-deps`
Expected: all tests pass; rustdoc renders the new `# Panics` sections without warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "$(cat <<'EOF'
docs(graphmock): document Graph accessor panic conditions

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: B4 — Doc-comment the public `Graph` struct

**Files:**
- Modify: [crates/graphmock/src/lib.rs:26-29](crates/graphmock/src/lib.rs#L26-L29)

- [ ] **Step 1: Add the doc comment**

Replace [crates/graphmock/src/lib.rs:26-29](crates/graphmock/src/lib.rs#L26-L29):

```rust
pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}
```

with:

```rust
/// A small directed graph built from the [`graph`] DSL, used as a fixture
/// for `graphwalk` traversal tests.
///
/// `&Graph` implements [`graphwalk::GraphRef`] and [`graphwalk::PredGraphRef`],
/// so it plugs straight into [`graphwalk::PreOrder`] / [`graphwalk::PostOrder`].
pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}
```

- [ ] **Step 2: Verify rustdoc renders**

Run: `cargo doc -p graphmock --no-deps`
Expected: clean build; the rendered struct page shows the new doc.

- [ ] **Step 3: Commit**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "$(cat <<'EOF'
docs(graphmock): doc-comment the public Graph struct

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: B1 + B5 — `Debug` on `PostOrderContext`, `#[must_use]` on `PreOrder/PostOrder::new`

**Files:**
- Modify: [crates/graphwalk/src/lib.rs:227-230](crates/graphwalk/src/lib.rs#L227-L230)
- Modify: [crates/graphwalk/src/lib.rs:191-202](crates/graphwalk/src/lib.rs#L191-L202)
- Modify: [crates/graphwalk/src/lib.rs:322-339](crates/graphwalk/src/lib.rs#L322-L339)

- [ ] **Step 1: Derive `Debug` on `PostOrderContext`**

Replace [crates/graphwalk/src/lib.rs:227-230](crates/graphwalk/src/lib.rs#L227-L230):

```rust
/// Internal stack-based state for a post-order DFS traversal.
pub struct PostOrderContext<N> {
    stack: Vec<(WalkPhase, N)>,
}
```

with:

```rust
/// Internal stack-based state for a post-order DFS traversal.
#[derive(Debug)]
pub struct PostOrderContext<N> {
    stack: Vec<(WalkPhase, N)>,
}
```

- [ ] **Step 2: Add `#[must_use]` to `PreOrder::new`**

In the `impl<G: GraphRef, V: VisitTracker<G::NodeId>> PreOrder<G, V>` block, replace the method signature at [crates/graphwalk/src/lib.rs:192-201](crates/graphwalk/src/lib.rs#L192-L201):

```rust
    /// Creates a pre-order traversal starting from `roots`.
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
```

with:

```rust
    /// Creates a pre-order traversal starting from `roots`.
    #[must_use]
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
```

- [ ] **Step 3: Add `#[must_use]` to `PostOrder::new`**

Same change in the `impl … PostOrder<G, V>` block at [crates/graphwalk/src/lib.rs:323-332](crates/graphwalk/src/lib.rs#L323-L332):

```rust
    /// Creates a post-order traversal starting from `roots`.
    #[must_use]
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
```

- [ ] **Step 4: Verify**

Run: `cargo test -p graphwalk && cargo clippy -p graphwalk --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/graphwalk/src/lib.rs
git commit -m "$(cat <<'EOF'
style(graphwalk): derive Debug on PostOrderContext, #[must_use] on Pre/PostOrder::new

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: D1 — Test that a duplicate root is visited only once

**Files:**
- Modify: [crates/graphwalk/tests/postorder.rs](crates/graphwalk/tests/postorder.rs)

- [ ] **Step 1: Append the test**

After the `nop_tracker_on_a_tree` test in [crates/graphwalk/tests/postorder.rs](crates/graphwalk/tests/postorder.rs), append:

```rust
#[test]
fn duplicate_root_visited_once() {
    // PostOrderContext::next_event drops a second Pre for an already-visited
    // node.  Passing the same root twice must yield exactly one (Pre, Post)
    // pair, not two — this is what makes idempotent root lists safe.
    let g = graphmock::graph("a -> b");
    let a = g.node("a");
    let mut po = entity_postorder(&g, [a, a]);
    let events: Vec<_> = core::iter::from_fn(|| po.next_event()).collect();
    let pre_a = events
        .iter()
        .filter(|(p, n)| matches!(p, WalkPhase::Pre) && *n == a)
        .count();
    let post_a = events
        .iter()
        .filter(|(p, n)| matches!(p, WalkPhase::Post) && *n == a)
        .count();
    assert_eq!(pre_a, 1, "expected one Pre event for `a`, got {events:?}");
    assert_eq!(post_a, 1, "expected one Post event for `a`, got {events:?}");
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p graphwalk --test postorder duplicate_root_visited_once`
Expected: PASS — the invariant already holds; we are pinning it.

- [ ] **Step 3: Commit**

```bash
git add crates/graphwalk/tests/postorder.rs
git commit -m "$(cat <<'EOF'
test(graphwalk): pin duplicate-root visit-once invariant

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: B3 + C1 — Doc-comment `Iter` and impl `FusedIterator`

**Files:**
- Modify: [crates/entity-utils/src/set.rs:76-87](crates/entity-utils/src/set.rs#L76-L87)
- Modify: the `mod tests` block in the same file

- [ ] **Step 1: Doc-comment `Iter` and impl `FusedIterator`**

Replace [crates/entity-utils/src/set.rs:76-87](crates/entity-utils/src/set.rs#L76-L87):

```rust
pub struct Iter<'a, E> {
    inner: cranelift_bitset::compound::Iter<'a>,
    _marker: PhantomData<E>,
}

impl<E: EntityRef> Iterator for Iter<'_, E> {
    type Item = E;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(E::new)
    }
}
```

with:

```rust
/// Iterator over a [`DenseEntitySet`] in ascending entity-index order.
///
/// Returned by [`DenseEntitySet::iter`] and `<&DenseEntitySet>::into_iter`.
pub struct Iter<'a, E> {
    inner: cranelift_bitset::compound::Iter<'a>,
    _marker: PhantomData<E>,
}

impl<E: EntityRef> Iterator for Iter<'_, E> {
    type Item = E;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(E::new)
    }
}

// `cranelift_bitset::compound::Iter` keeps yielding `None` once exhausted,
// so the wrapper is naturally fused.
impl<E: EntityRef> core::iter::FusedIterator for Iter<'_, E> {}
```

- [ ] **Step 2: Pin the marker in a test**

Append inside the existing `mod tests` block in [crates/entity-utils/src/set.rs](crates/entity-utils/src/set.rs) (after `into_iter_for_ref_yields_same_as_iter`):

```rust
    #[test]
    fn iter_is_fused() {
        fn assert_fused<I: core::iter::FusedIterator>(_: &I) {}
        let s: DenseEntitySet<Id> = [Id(1)].into_iter().collect();
        let it = s.iter();
        assert_fused(&it);
    }
```

- [ ] **Step 3: Verify**

Run: `cargo test -p entity-utils && cargo clippy -p entity-utils --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/entity-utils/src/set.rs
git commit -m "$(cat <<'EOF'
feat(entity-utils): mark DenseEntitySet::Iter as FusedIterator + doc

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: C2 — Use upper-bound size hint in `DenseEntitySet::from_iter`

**Files:**
- Modify: [crates/entity-utils/src/set.rs:107-117](crates/entity-utils/src/set.rs#L107-L117)

- [ ] **Step 1: Replace the size-hint pick**

Replace [crates/entity-utils/src/set.rs:107-117](crates/entity-utils/src/set.rs#L107-L117):

```rust
impl<E: EntityRef> FromIterator<E> for DenseEntitySet<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let (min_size, _) = iter.size_hint();
        let mut set = Self::with_capacity(min_size);
        for entity in iter {
            set.insert(entity);
        }
        set
    }
}
```

with:

```rust
impl<E: EntityRef> FromIterator<E> for DenseEntitySet<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let mut set = Self::with_capacity(upper.unwrap_or(lower));
        for entity in iter {
            set.insert(entity);
        }
        set
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p entity-utils`
Expected: all existing tests pass — observable behaviour unchanged, only initial bitset capacity differs.

- [ ] **Step 3: Commit**

```bash
git add crates/entity-utils/src/set.rs
git commit -m "$(cat <<'EOF'
perf(entity-utils): preallocate DenseEntitySet::from_iter from upper bound

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: B2 — Derive `Debug` on `Worklist`

**Files:**
- Modify: [crates/entity-utils/src/worklist.rs:12-16](crates/entity-utils/src/worklist.rs#L12-L16)
- Modify: the `mod tests` block in the same file

- [ ] **Step 1: Add `Debug` to the derive list**

Replace [crates/entity-utils/src/worklist.rs:12-16](crates/entity-utils/src/worklist.rs#L12-L16):

```rust
#[derive(Clone)]
pub struct Worklist<E> {
    worklist: VecDeque<E>,
    workset: DenseEntitySet<E>,
}
```

with:

```rust
#[derive(Clone, Debug)]
pub struct Worklist<E> {
    worklist: VecDeque<E>,
    workset: DenseEntitySet<E>,
}
```

(`#[derive(Debug)]` synthesises `impl<E: Debug> Debug for Worklist<E>`. `DenseEntitySet<E>` already implements `Debug` for any `E` because its derive uses `PhantomData<E>` and `CompoundBitSet: Debug` — so the derive succeeds whenever `E: Debug`.)

- [ ] **Step 2: Pin the derive with a smoke test**

Append inside the existing `mod tests` block in [crates/entity-utils/src/worklist.rs](crates/entity-utils/src/worklist.rs):

```rust
    #[test]
    fn debug_format_smoke() {
        // `Worklist` derives Debug; this is a regression pin so the derive
        // can't be silently removed.  We don't assert a specific format string.
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(1));
        let _ = format!("{wl:?}");
    }
```

- [ ] **Step 3: Verify**

Run: `cargo test -p entity-utils && cargo clippy -p entity-utils --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/entity-utils/src/worklist.rs
git commit -m "$(cat <<'EOF'
feat(entity-utils): derive Debug on Worklist

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Final clippy + workspace gate

**Files:** (none — verification only)

- [ ] **Step 1: Verify the three crates pass strict clippy**

Run from the worktree root:

```bash
cargo clippy -p entity-utils -p graphmock -p graphwalk --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 2: Verify the full workspace still builds and tests**

```bash
cargo build --workspace && cargo test --workspace
```

Expected: clean.

- [ ] **Step 3: Verify pedantic + nursery still pass clean**

```bash
cargo clippy -p entity-utils -p graphmock -p graphwalk --all-targets -- -W clippy::pedantic -W clippy::nursery
```

Expected: zero code warnings (the round-2 baseline). Cargo-metadata warnings from `clippy::cargo` are workspace-wide and out of scope.

If any new code warnings appear from the changes above, address them inline. Likely candidates: `must_use_candidate` on the new `pub fn`s (already covered), `missing_panics_doc` on something I overlooked.

---

## Self-Review

- **Spec coverage:** every finding listed in the executive summary (A1, A2, B1–B5, C1, C2, D1) maps to exactly one task. The clippy gate (E) is Task 9.
- **Placeholder scan:** none. Every step has the literal new code or command.
- **Type consistency:** no new types or trait signatures introduced beyond `FusedIterator` impl and `Debug` derives — both fully spelled out.
- **Order independence:** Tasks 1–3 touch only `graphmock`; Tasks 4–5 only `graphwalk`; Tasks 6–8 only `entity-utils`. Task 9 must run last. Within a crate, tasks are commit-independent.
