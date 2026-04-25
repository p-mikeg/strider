---
title: entity-utils / graphwalk / graphmock — correctness & test review
date: 2026-04-25
status: draft
---

# entity-utils / graphwalk / graphmock — correctness & test review

## Goal

Pass over three small foundation crates to (a) fix correctness bugs, (b) add
the test coverage that is currently missing, and (c) make minor API additions
where they are immediately useful for the tests we are writing. No public
API breakage. No consumer changes.

The work happens on its own branch in a worktree so it can run in parallel
with the other crate-review streams that are currently active on the main
checkout.

## Scope

### In scope

1. **`entity-utils::worklist::Worklist`** — fix the deduplication bug and the
   `FromIterator` / `Extend` invariant inconsistency.
2. **`entity-utils::worklist::Worklist`** — add `len`, `contains`, `clear`,
   `is_empty` (already chosen as approved set B).
3. **Tests** for `entity-utils` (currently zero), with explicit regression
   tests for the bugs fixed in (1).
4. **Tests** for `graphwalk` — extend existing pre/post-order suites to
   cover multi-root, self-loop, disjoint roots, empty roots, isolated
   subgraphs, the `NopTracker` tree path, and the post-pop "already
   visited" skip loop in `PreOrderContext::next`.
5. **Tests** for `graphmock` — parser edge cases that exercise behavior the
   downstream tests rely on.
6. **Minor cleanups** in `graphmock`: collapse the duplicate `to_owned()` in
   `get_or_create` via the `entry` API.
7. `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`
   pass on the worktree branch.

### Out of scope

- Consumer migration (`ir/src/walk.rs` and the others stay untouched).
- Public API breakage. Method signatures of existing items are preserved.
- Micro-optimizations beyond what is already listed.
- Adding new traversal algorithms, peek APIs, or parallel walks.
- Changing how `cranelift_bitset::CompoundBitSet::with_capacity` is wrapped.
  We will document the existing behavior, not change it.

## Bugs to fix

### B1 — `Worklist::enqueue` never inserts into the workset

`crates/entity-utils/src/worklist.rs:35-39`

```rust
pub fn enqueue(&mut self, entity: E) {
    if !self.workset.contains(entity) {
        self.worklist.push_back(entity);
    }
}
```

The check exists but the matching `self.workset.insert(entity);` is missing.
Result: the workset stays empty for the entire life of the worklist as long
as nothing else inserts into it, so:

- Duplicate enqueues all land in the deque.
- `dequeue` then calls `workset.remove` on an empty set, a no-op.

Fix: insert into the workset inside the `if`, then remove on dequeue (which
is already correct).

### B2 — `Worklist::FromIterator` builds an inconsistent state

`crates/entity-utils/src/worklist.rs:56-62`

```rust
fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
    let worklist: VecDeque<_> = iter.into_iter().collect();
    let workset: DenseEntitySet<_> = worklist.iter().copied().collect();
    Self { worklist, workset }
}
```

If the input iterator contains duplicates, the deque holds them all but the
workset has only the unique values. After `dequeue`, the workset removes
the value, but its remaining duplicates in the deque can be dequeued again
(behavior #1 leaks back in even after #1 is fixed).

Fix: deduplicate while building. Forward to `enqueue` so the invariant is
established by a single code path.

### B3 — `Worklist::Extend` calls broken `enqueue`

Falls out automatically once B1 is fixed. The fix for B1 also fixes B3.

## API additions on `Worklist` (approved set B)

All return-only / read-only / clear; no behavior change to existing methods.

```rust
impl<E: EntityRef> Worklist<E> {
    /// Number of entities currently queued.
    pub fn len(&self) -> usize { self.worklist.len() }

    /// `true` if `entity` is currently queued.
    pub fn contains(&self, entity: E) -> bool { self.workset.contains(entity) }

    /// Removes every queued entity.
    pub fn clear(&mut self) {
        self.worklist.clear();
        self.workset.clear();
    }
}
```

`is_empty` already exists.

## Tests to add

### `entity-utils` — `crates/entity-utils/src/set.rs` (inline `#[cfg(test)] mod tests`)

- `new`/`default` produce an empty set.
- `insert` then `contains` returns `true`; `remove` then `contains` returns `false`.
- Inserting the same entity twice still reports membership once and `iter`
  yields it once.
- `iter` yields entities in ascending index order (this is what
  `CompoundBitSet::iter` documents; pinning it here protects callers).
- `with_capacity` of `0` is valid and behaves like `new`.
- `clear` removes everything.
- `FromIterator` with duplicates produces a deduplicated set.
- `FromIterator` from an empty iterator produces an empty set.

### `entity-utils` — `crates/entity-utils/src/worklist.rs` (inline `#[cfg(test)] mod tests`)

- `new`/`default` produce an empty worklist; `is_empty` true; `len` zero.
- `enqueue` then `dequeue` returns the entity; `is_empty` true after.
- **Regression for B1:** enqueue same entity twice; `len` is `1`; first
  `dequeue` returns it, second returns `None`.
- After dequeue, re-enqueueing the same entity is allowed and dequeued
  successfully (the "FIFO with dedup *while queued*" semantics in the
  doc-comment).
- FIFO order across distinct entities.
- **Regression for B2:** `FromIterator` with duplicates ends up with `len`
  equal to the unique count, and dequeueing drains exactly those uniques.
- `Extend` calls dedupe through `enqueue`.
- `contains`, `clear`, `len` exercised against live state.

### `graphwalk` — extend `tests/preorder.rs`

- **Multi-root:** two roots, no path between them, both visited; relative
  order matches the documented stack-push order.
- **Self-loop:** node with edge to itself yields once.
- **Empty roots:** `entity_preorder(&g, [])` yields nothing.
- **Already-visited skip:** the post-pop loop in `PreOrderContext::next`
  handles a stack head whose top is a visited node by skipping past it
  (construct a graph that pushes the same successor twice).

### `graphwalk` — extend `tests/postorder.rs`

- **Multi-root preservation:** the doc-comment in `PostOrderContext::reset`
  promises a specific RPO root order. Pin it with a test that uses two
  unrelated roots and asserts the RPO begins with the first root.
- **Self-loop:** node with edge to itself; both pre and post events fire
  once.
- **Empty roots.**
- **`NopTracker` tree walk:** a tree (no joins, no cycles) traversed via
  `PostOrder<_, NopTracker>`; verify each node visited exactly once. (We
  rely on the user contract that the graph really is a tree.)

### `graphmock` — extend the existing inline tests

- Whitespace-only and trailing-blank-line input produces an empty graph.
- Multi-pred / multi-succ on the same line: `a, b -> c, d` adds 4 edges.
- Self-loop: `a -> a`.
- Repeating a name later still resolves to the same `NodeId`.

## Minor cleanups

- `graphmock::Graph::get_or_create` uses `nodes_by_name.entry(name)` to
  avoid the duplicate `to_owned()` allocation on miss. No public API
  change.

## Verification

- `cargo test --workspace` — green.
- `cargo clippy --workspace -- -D warnings` — green.
- `cargo build --workspace` — green.

## Branch / merge plan

- New worktree at `.worktrees/utils-review` on a new branch
  `feature/utils-review` cut from `feature/ai`.
- All changes land as small, self-contained commits scoped per crate per
  concern (one commit for each Worklist bug fix, one per test file, etc.).
- After verification, merge `feature/utils-review` back into `feature/ai`
  with a fast-forward or merge commit (decision deferred to merge time
  based on whether `feature/ai` has moved).
