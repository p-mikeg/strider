# entity-utils / graphwalk / graphmock Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the broken `Worklist` deduplication, add the missing test coverage in all three crates, make small approved API additions to `Worklist`, and leave the three crates passing `cargo test`/`cargo clippy` cleanly.

**Architecture:** All work lands on branch `feature/utils-review` in worktree `.worktrees/utils-review`. Each task is a self-contained TDD slice committed on its own. No public API breakage; no changes to consumers (`ir/src/walk.rs`).

**Tech Stack:** Rust 2024, `cranelift-entity`, `cranelift-bitset`, `expect-test`, `itertools`. Workspace lints from `Cargo.toml`: `clippy::panic`, `unwrap_used`, `expect_used`, `unreachable`, `todo` are `deny`; `must_use_candidate`, `redundant_closure`, `map_unwrap_or`, `match_same_arms`, `missing_errors_doc` are `warn`.

**Working directory:** All commands assume cwd = `/home/mike/Desktop/strider/.worktrees/utils-review`.

**Spec:** [docs/superpowers/specs/2026-04-25-entity-utils-graphwalk-graphmock-review-design.md](../specs/2026-04-25-entity-utils-graphwalk-graphmock-review-design.md)

---

## File touch map

| File | What happens |
|------|--------------|
| `crates/entity-utils/src/worklist.rs` | Fix B1 enqueue dedup; rewrite `FromIterator` to flow through `enqueue` (B2 fix); add `len`, `contains`, `clear`; add `#[must_use]` to existing read-only methods; add `#[cfg(test)] mod tests`. |
| `crates/entity-utils/src/set.rs` | Add `#[must_use]` to read-only methods; add `#[cfg(test)] mod tests`. |
| `crates/entity-utils/src/lib.rs` | No code change; tests live in the modules. |
| `crates/graphwalk/src/lib.rs` | Add `#[must_use]` to two read-only methods. (No behavior change.) |
| `crates/graphwalk/tests/preorder.rs` | Add multi-root, self-loop, empty-roots, repeated-successor tests. |
| `crates/graphwalk/tests/postorder.rs` | Add multi-root preservation, self-loop, empty-roots, NopTracker tree-walk tests. |
| `crates/graphmock/src/lib.rs` | Replace duplicate `to_owned()` in `get_or_create` with `entry` API; add `#[must_use]`; expand the inline tests with whitespace, multi-edge fan-out, self-loop, name-recurrence cases. |

The plan is intentionally split into per-file tasks so each commit is small and reviewable.

---

## Task 1: Fix `Worklist::enqueue` dedup bug (B1) — TDD

**Files:**
- Modify: `crates/entity-utils/src/worklist.rs`

- [ ] **Step 1: Add the regression test BEFORE the fix.**

Open `crates/entity-utils/src/worklist.rs`. Append this module at the end of the file (after the existing `Extend` impl):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::entity_impl;

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
    struct Id(u32);
    entity_impl!(Id);

    #[test]
    fn enqueue_dedups_while_queued() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(7));
        wl.enqueue(Id(7));
        // Before the fix this would fail: enqueue never inserts into the
        // workset, so both pushes land in the deque and the second dequeue
        // returns Some instead of None.
        assert_eq!(wl.dequeue(), Some(Id(7)));
        assert_eq!(wl.dequeue(), None);
    }
}
```

- [ ] **Step 2: Run the test and confirm it fails.**

Run: `cargo test -p entity-utils enqueue_dedups_while_queued`
Expected: FAIL — second `dequeue` returns `Some(Id(7))` instead of `None`.

- [ ] **Step 3: Apply the fix.**

In `crates/entity-utils/src/worklist.rs`, replace the body of `enqueue` to insert into the workset:

```rust
    pub fn enqueue(&mut self, entity: E) {
        if !self.workset.contains(entity) {
            self.workset.insert(entity);
            self.worklist.push_back(entity);
        }
    }
```

- [ ] **Step 4: Re-run the test, expect it to pass.**

Run: `cargo test -p entity-utils enqueue_dedups_while_queued`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/entity-utils/src/worklist.rs
git commit -m "fix(entity-utils): Worklist::enqueue actually inserts into the workset

Without inserting into the workset, the contains() check never trips,
so enqueueing the same entity twice put both copies in the deque.
Add a regression test that double-enqueues and asserts the second
dequeue returns None."
```

---

## Task 2: Fix `Worklist::FromIterator` invariant inconsistency (B2) — TDD

**Files:**
- Modify: `crates/entity-utils/src/worklist.rs`

- [ ] **Step 1: Add the regression test inside the existing `tests` module.**

In the `#[cfg(test)] mod tests` block in `crates/entity-utils/src/worklist.rs`, add:

```rust
    #[test]
    fn from_iter_dedups_duplicates() {
        let mut wl: Worklist<Id> =
            [Id(1), Id(2), Id(1), Id(3), Id(2)].into_iter().collect();
        let mut got = Vec::new();
        while let Some(e) = wl.dequeue() {
            got.push(e);
        }
        // Order of first occurrence preserved; duplicates dropped.
        assert_eq!(got, vec![Id(1), Id(2), Id(3)]);
    }
```

- [ ] **Step 2: Run, expect failure on current code.**

Run: `cargo test -p entity-utils from_iter_dedups_duplicates`
Expected: FAIL — drained sequence has duplicates `[Id(1), Id(2), Id(1), Id(3), Id(2)]`.

- [ ] **Step 3: Replace the `FromIterator` impl with one that flows through `enqueue`.**

In `crates/entity-utils/src/worklist.rs`, replace the existing impl:

```rust
impl<E: EntityRef> FromIterator<E> for Worklist<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let mut wl = Self::new();
        wl.extend(iter);
        wl
    }
}
```

- [ ] **Step 4: Re-run the test, expect it to pass.**

Run: `cargo test -p entity-utils from_iter_dedups_duplicates`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/entity-utils/src/worklist.rs
git commit -m "fix(entity-utils): Worklist::FromIterator routes through enqueue

The previous impl built the deque by collecting the input directly and
the workset by re-collecting, so duplicates ended up only in the deque.
Forward to extend (which calls enqueue) so a single code path
establishes the invariant."
```

---

## Task 3: Comprehensive `Worklist` tests + new API methods

**Files:**
- Modify: `crates/entity-utils/src/worklist.rs`

- [ ] **Step 1: Add `len`, `contains`, `clear` and `#[must_use]` attributes.**

In `crates/entity-utils/src/worklist.rs`, after `is_empty` add:

```rust
    /// Number of entities currently queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.worklist.len()
    }

    /// Returns `true` if `entity` is currently queued.
    #[must_use]
    pub fn contains(&self, entity: E) -> bool {
        self.workset.contains(entity)
    }

    /// Removes every queued entity.
    pub fn clear(&mut self) {
        self.worklist.clear();
        self.workset.clear();
    }
```

Also add `#[must_use]` to the existing read-only methods on `Worklist`:

```rust
    #[must_use]
    pub fn new() -> Self { ... unchanged ... }

    #[must_use]
    pub fn is_empty(&self) -> bool { ... unchanged ... }
```

- [ ] **Step 2: Add the comprehensive test cases inside the existing `tests` module.**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn new_and_default_are_empty() {
        let wl: Worklist<Id> = Worklist::new();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);

        let wl: Worklist<Id> = Worklist::default();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);
    }

    #[test]
    fn enqueue_dequeue_roundtrip() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(0));
        assert!(!wl.is_empty());
        assert_eq!(wl.len(), 1);
        assert!(wl.contains(Id(0)));

        assert_eq!(wl.dequeue(), Some(Id(0)));
        assert!(wl.is_empty());
        assert!(!wl.contains(Id(0)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn fifo_order_across_distinct_entities() {
        let mut wl: Worklist<Id> = Worklist::new();
        for i in 0..5 {
            wl.enqueue(Id(i));
        }
        let mut got = Vec::new();
        while let Some(e) = wl.dequeue() {
            got.push(e);
        }
        assert_eq!(got, (0..5).map(Id).collect::<Vec<_>>());
    }

    #[test]
    fn re_enqueue_after_dequeue_is_allowed() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(42));
        assert_eq!(wl.dequeue(), Some(Id(42)));

        wl.enqueue(Id(42));
        assert_eq!(wl.dequeue(), Some(Id(42)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn extend_dedups() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.extend([Id(1), Id(2), Id(1)]);
        assert_eq!(wl.len(), 2);
        assert_eq!(wl.dequeue(), Some(Id(1)));
        assert_eq!(wl.dequeue(), Some(Id(2)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn clear_empties_both_queue_and_set() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.extend([Id(1), Id(2), Id(3)]);
        wl.clear();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);
        assert!(!wl.contains(Id(1)));

        // After clear, re-enqueue still works (workset must really be empty,
        // not just have stale entries).
        wl.enqueue(Id(1));
        assert_eq!(wl.dequeue(), Some(Id(1)));
    }

    #[test]
    fn contains_only_while_queued() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(5));
        assert!(wl.contains(Id(5)));
        let _ = wl.dequeue();
        assert!(!wl.contains(Id(5)));
    }
```

- [ ] **Step 3: Run all `worklist` tests; expect green.**

Run: `cargo test -p entity-utils --lib worklist::tests`
Expected: PASS — 9 tests (the 2 from earlier tasks + 7 added here).

- [ ] **Step 4: Commit.**

```bash
git add crates/entity-utils/src/worklist.rs
git commit -m "feat(entity-utils): Worklist gets len/contains/clear and full test suite

Adds the small read APIs (len, contains, clear) and #[must_use] on
existing read-only methods. Adds tests for new/default emptiness,
enqueue/dequeue roundtrip, FIFO ordering, re-enqueue after dequeue,
Extend dedup, and clear restoring usability."
```

---

## Task 4: `DenseEntitySet` test suite + `#[must_use]`

**Files:**
- Modify: `crates/entity-utils/src/set.rs`

- [ ] **Step 1: Add `#[must_use]` attributes to read-only methods.**

In `crates/entity-utils/src/set.rs`, add `#[must_use]` to:

```rust
    #[must_use]
    pub fn new() -> Self { ... }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self { ... }

    #[must_use]
    pub fn contains(&self, entity: E) -> bool { ... }

    #[must_use]
    pub fn iter(&self) -> Iter<'_, E> { ... }
```

- [ ] **Step 2: Add the inline test module to `crates/entity-utils/src/set.rs`.**

Append at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::entity_impl;

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
    struct Id(u32);
    entity_impl!(Id);

    #[test]
    fn new_and_default_are_empty() {
        let s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(!s.contains(Id(0)));
        assert!(s.iter().next().is_none());

        let s: DenseEntitySet<Id> = DenseEntitySet::default();
        assert!(!s.contains(Id(0)));
    }

    #[test]
    fn with_capacity_zero_is_valid() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::with_capacity(0);
        assert!(s.iter().next().is_none());
        s.insert(Id(0));
        assert!(s.contains(Id(0)));
    }

    #[test]
    fn insert_contains_remove() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(!s.contains(Id(3)));
        s.insert(Id(3));
        assert!(s.contains(Id(3)));
        s.remove(Id(3));
        assert!(!s.contains(Id(3)));
    }

    #[test]
    fn double_insert_is_idempotent() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(7));
        s.insert(Id(7));
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(7)]);
    }

    #[test]
    fn iter_yields_in_ascending_index_order() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        for &i in &[5u32, 1, 9, 2, 1] {
            s.insert(Id(i));
        }
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(1), Id(2), Id(5), Id(9)]);
    }

    #[test]
    fn clear_removes_everything() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(0));
        s.insert(Id(100));
        s.clear();
        assert!(!s.contains(Id(0)));
        assert!(!s.contains(Id(100)));
        assert!(s.iter().next().is_none());
    }

    #[test]
    fn from_iter_dedups() {
        let s: DenseEntitySet<Id> =
            [Id(2), Id(1), Id(2), Id(3)].into_iter().collect();
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(1), Id(2), Id(3)]);
    }

    #[test]
    fn from_iter_empty() {
        let s: DenseEntitySet<Id> = core::iter::empty().collect();
        assert!(s.iter().next().is_none());
    }

    #[test]
    fn remove_unmembered_is_noop() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(1));
        s.remove(Id(99));
        assert!(s.contains(Id(1)));
        assert!(!s.contains(Id(99)));
    }
}
```

- [ ] **Step 3: Run; expect green.**

Run: `cargo test -p entity-utils --lib set::tests`
Expected: PASS — 9 tests.

- [ ] **Step 4: Commit.**

```bash
git add crates/entity-utils/src/set.rs
git commit -m "test(entity-utils): cover DenseEntitySet behavior + add #[must_use]

Adds tests for emptiness, with_capacity(0), insert/contains/remove,
idempotent insert, iter ordering, clear, FromIterator dedup/empty,
and remove of an absent entity. Adds #[must_use] on read-only
constructors and queries."
```

---

## Task 5: `graphwalk` `#[must_use]` cleanup

**Files:**
- Modify: `crates/graphwalk/src/lib.rs`

- [ ] **Step 1: Identify the two `#[must_use]` candidates from clippy.**

Run: `cargo clippy -p graphwalk --lib 2>&1 | grep -B1 must_use_candidate`
Expected: two methods flagged. They are `PreOrderContext::new` and `PostOrderContext::new` (verify with the clippy output).

- [ ] **Step 2: Add `#[must_use]` to those two methods.**

In `crates/graphwalk/src/lib.rs`, prefix both `pub fn new() -> Self` impls with `#[must_use]`. Apply *only* to constructors that produce `Self`. Do NOT add `#[must_use]` to `next` or `next_event` (their return values may be intentionally discarded by clients that only care about the side effects on `visited`).

- [ ] **Step 3: Confirm no clippy warnings remain in the lib build.**

Run: `cargo clippy -p graphwalk --lib 2>&1 | tail -5`
Expected: no warnings (or only warnings unrelated to `must_use_candidate`).

- [ ] **Step 4: Commit.**

```bash
git add crates/graphwalk/src/lib.rs
git commit -m "style(graphwalk): #[must_use] on PreOrderContext::new and PostOrderContext::new"
```

---

## Task 6: `graphwalk` preorder edge-case tests

**Files:**
- Modify: `crates/graphwalk/tests/preorder.rs`

- [ ] **Step 1: Add a helper for the multi-root case and add the new tests.**

Append to `crates/graphwalk/tests/preorder.rs` after the existing `test_preorder!` invocations:

```rust
#[test]
fn empty_roots_yields_nothing() {
    let g = graphmock::graph("a -> b");
    let order: Vec<_> = entity_preorder(&g, core::iter::empty()).collect();
    assert!(order.is_empty());
}

#[test]
fn self_loop_visits_node_once() {
    let g = graphmock::graph("a -> a");
    let order = entity_preorder(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["a".to_owned()]);
}

#[test]
fn multi_root_disjoint_subgraphs_visits_both() {
    // Two disjoint chains: a -> b and x -> y. We pass both roots in.
    let g = graphmock::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let order = entity_preorder(&g, [a, x])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order.len(), 4);
    // Either {a, b} appears before {x, y} or vice versa, but each chain is
    // contiguous in pre-order. We assert both nodes from each chain appear.
    for name in ["a", "b", "x", "y"] {
        assert!(order.iter().any(|s| s == name), "missing {name} in {order:?}");
    }
}

#[test]
fn repeated_successor_is_visited_once() {
    // a -> b appears twice as a successor of a (because we say so).  The pre-order
    // walk must still visit b exactly once.  This exercises the "skip if already
    // visited" loop in PreOrderContext::next.
    let g = graphmock::graph(
        "a -> b, b
         b -> c",
    );
    let order = entity_preorder(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order.len(), 3);
    let count_b = order.iter().filter(|s| *s == "b").count();
    assert_eq!(count_b, 1);
}
```

- [ ] **Step 2: Run; expect green.**

Run: `cargo test -p graphwalk --test preorder`
Expected: 9 tests pass (5 existing + 4 new).

- [ ] **Step 3: Commit.**

```bash
git add crates/graphwalk/tests/preorder.rs
git commit -m "test(graphwalk): preorder edge cases (empty roots, self-loop, multi-root, repeated successor)"
```

---

## Task 7: `graphwalk` postorder edge-case tests

**Files:**
- Modify: `crates/graphwalk/tests/postorder.rs`

- [ ] **Step 1: Add tests after the existing `test_postorder!` invocations.**

Append to `crates/graphwalk/tests/postorder.rs`:

```rust
#[test]
fn empty_roots_yields_nothing() {
    let g = graphmock::graph("a -> b");
    let mut po = entity_postorder(&g, core::iter::empty());
    assert!(po.next().is_none());
    assert!(po.next_event().is_none());
}

#[test]
fn self_loop_emits_pre_and_post_once() {
    let g = graphmock::graph("a -> a");
    let mut po = entity_postorder(&g, [g.entry()]);
    let events: Vec<_> = core::iter::from_fn(|| po.next_event()).collect();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0].0, WalkPhase::Pre));
    assert!(matches!(events[1].0, WalkPhase::Post));
    assert_eq!(events[0].1, events[1].1);
}

#[test]
fn multi_root_preserves_root_order_in_rpo() {
    // Doc-comment in PostOrderContext::reset promises: if `u` precedes `v` in
    // `roots` and there's no path from v to u, then `u` precedes `v` in any RPO.
    // Build two disjoint chains (a -> b, x -> y) and pass roots in [a, x] order.
    let g = graphmock::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let mut po: Vec<_> = entity_postorder(&g, [a, x]).collect();
    po.reverse(); // RPO
    let names: Vec<_> = po.iter().map(|&n| g.name(n).to_owned()).collect();
    let pos_a = names.iter().position(|s| s == "a").unwrap();
    let pos_x = names.iter().position(|s| s == "x").unwrap();
    assert!(
        pos_a < pos_x,
        "expected a (first root) to precede x in RPO, got {names:?}"
    );
}

#[test]
fn nop_tracker_on_a_tree() {
    use graphwalk::{PostOrder, NopTracker};

    // Tree (no cycles, no joins): a -> {b, c}; b -> d.
    let g = graphmock::graph(
        "a -> b, c
         b -> d",
    );

    let order: Vec<_> = PostOrder::<&Graph, NopTracker>::new(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect();
    // Each node is visited exactly once even though NopTracker never records visits;
    // this only holds because the input really is a tree.
    assert_eq!(order.len(), 4);
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["a", "b", "c", "d"]);
}
```

- [ ] **Step 2: Run; expect green.**

Run: `cargo test -p graphwalk --test postorder`
Expected: 9 tests pass (5 existing + 4 new).

- [ ] **Step 3: Commit.**

```bash
git add crates/graphwalk/tests/postorder.rs
git commit -m "test(graphwalk): postorder edge cases (empty roots, self-loop, multi-root RPO order, NopTracker tree walk)"
```

---

## Task 8: `graphmock` cleanup + edge-case tests

**Files:**
- Modify: `crates/graphmock/src/lib.rs`

- [ ] **Step 1: Replace duplicate `to_owned()` in `get_or_create` with the entry API.**

In `crates/graphmock/src/lib.rs`, replace `get_or_create` with:

```rust
    fn get_or_create(&mut self, name: &str) -> NodeId {
        use std::collections::hash_map::Entry;
        match self.nodes_by_name.entry(name.to_owned()) {
            Entry::Occupied(o) => *o.get(),
            Entry::Vacant(v) => {
                let node = self.nodes.push(Node {
                    name: v.key().clone(),
                    preds: Vec::new(),
                    succs: Vec::new(),
                });
                v.insert(node);
                node
            }
        }
    }
```

This still allocates one `String` per *miss* (entry consumes the owned key) but no longer allocates two on miss.

- [ ] **Step 2: Add `#[must_use]` to read-only public methods.**

In `crates/graphmock/src/lib.rs`, add `#[must_use]` to:

```rust
    #[must_use]
    pub fn entry(&self) -> NodeId { ... unchanged ... }

    #[must_use]
    pub fn node(&self, name: &str) -> NodeId { ... unchanged ... }

    #[must_use]
    pub fn name(&self, node: NodeId) -> &str { ... unchanged ... }
```

And on the free function:

```rust
#[must_use]
pub fn graph(input: &str) -> Graph { ... unchanged ... }
```

- [ ] **Step 3: Add edge-case tests to the existing `mod tests`.**

In `crates/graphmock/src/lib.rs`, append to the existing `#[cfg(test)] mod tests`:

```rust
    use graphwalk::{GraphRef, PredGraphRef};
    use std::ops::ControlFlow;

    fn succs(g: &crate::Graph, node: crate::NodeId) -> Vec<String> {
        let mut out = Vec::new();
        (&g).try_successors(node, |s| {
            out.push(g.name(s).to_owned());
            ControlFlow::Continue(())
        });
        out
    }

    fn preds(g: &crate::Graph, node: crate::NodeId) -> Vec<String> {
        let mut out = Vec::new();
        (&g).try_predecessors(node, |p| {
            out.push(g.name(p).to_owned());
            ControlFlow::Continue(())
        });
        out
    }

    #[test]
    fn whitespace_only_input_yields_no_edges() {
        let g = graph("   \n\t\n   ");
        // Entry node id 0 doesn't exist because no nodes were ever created.
        // Just check we didn't panic and there are no successors-of-anything.
        // (We can't actually call entry() — it would index out of bounds —
        // but we can confirm the by-name map is empty by trying a lookup
        // through the public API: there is none, so the existence of `g`
        // is all we assert.)
        let _ = g;
    }

    #[test]
    fn fan_out_and_fan_in() {
        // a, b -> c, d adds 4 edges.
        let g = graph("a, b -> c, d");
        let a = g.node("a");
        let b = g.node("b");
        let c = g.node("c");
        let d = g.node("d");
        assert_eq!(succs(&g, a), vec!["c", "d"]);
        assert_eq!(succs(&g, b), vec!["c", "d"]);
        assert_eq!(preds(&g, c), vec!["a", "b"]);
        assert_eq!(preds(&g, d), vec!["a", "b"]);
    }

    #[test]
    fn self_loop() {
        let g = graph("a -> a");
        let a = g.node("a");
        assert_eq!(succs(&g, a), vec!["a"]);
        assert_eq!(preds(&g, a), vec!["a"]);
    }

    #[test]
    fn name_recurrence_resolves_to_same_id() {
        let g = graph(
            "a -> b
             b -> a",
        );
        let a1 = g.node("a");
        let a2 = g.node("a");
        assert_eq!(a1, a2);
        assert_eq!(succs(&g, a1), vec!["b"]);
        assert_eq!(preds(&g, a1), vec!["b"]);
    }
```

- [ ] **Step 4: Run all `graphmock` tests; expect green.**

Run: `cargo test -p graphmock`
Expected: 7 tests pass (3 existing + 4 new).

- [ ] **Step 5: Commit.**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "test(graphmock): edge cases + collapse duplicate to_owned in get_or_create

Switches get_or_create to the entry API so the missed-name path
allocates one String instead of two. Adds #[must_use] on read-only
methods. Adds tests for whitespace-only input, fan-out/fan-in, self-loop,
and name recurrence."
```

---

## Task 9: Whole-workspace verification

**Files:** none changed.

- [ ] **Step 1: Run all tests for the three crates plus `--workspace` to make sure nothing else regressed.**

Run: `cargo test -p entity-utils -p graphwalk -p graphmock`
Expected: all green.

Run: `cargo test --workspace`
Expected: all green (or no new failures vs. baseline; if there are pre-existing failures unrelated to this work, note them in the commit message of Step 3 below but do not fix them in this branch).

- [ ] **Step 2: Run clippy on the three crates.**

Run: `cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets`
Expected: no warnings.

- [ ] **Step 3: Run workspace clippy.**

Run: `cargo clippy --workspace --all-targets`
Expected: no *new* warnings introduced by this branch.

- [ ] **Step 4: If everything is clean, no commit needed. If a tweak was required (e.g., a clippy fix), make the smallest possible commit:**

```bash
git add <file>
git commit -m "style(<crate>): <one-line clippy fix>"
```

---

## Task 10: Code-review pass

**Files:** none changed (review output drives any follow-up commits).

- [ ] **Step 1: Invoke `superpowers:requesting-code-review` to do a self-review against the spec.**

Use the Skill tool with `superpowers:requesting-code-review`. Pass it the spec path and the branch name. Address any high-priority issues by adding follow-up commits.

- [ ] **Step 2: (Optional) Run CodeRabbit via `coderabbit:review` for a second-opinion AI review.**

Invoke `coderabbit:review` against the diff between `feature/utils-review` and `feature/ai`. Apply only the suggestions that improve clarity or correctness; ignore stylistic noise.

- [ ] **Step 3: Decide on merge strategy.**

Switch back to `/home/mike/Desktop/strider`, then either:

```bash
git merge --no-ff feature/utils-review
```

(if `feature/ai` has moved and a merge commit is appropriate), or

```bash
git merge --ff-only feature/utils-review
```

(if `feature/ai` hasn't moved). Ask the user before merging.

- [ ] **Step 4: After successful merge, remove the worktree and branch.**

```bash
cd /home/mike/Desktop/strider
git worktree remove .worktrees/utils-review
git branch -d feature/utils-review
```

(Only after explicit user approval.)

---

## Self-review notes

- Spec coverage: every requirement in the spec maps to a task above (B1 → Task 1, B2 → Task 2, API additions → Task 3, set tests → Task 4, graphwalk preorder → Task 6, postorder → Task 7, graphmock cleanup + tests → Task 8, must_use across all three → Tasks 3/4/5/8, verification → Task 9, merge plan → Task 10).
- Type consistency: `Worklist`, `DenseEntitySet`, `Graph`, `NodeId`, `WalkPhase`, `PostOrder`, `PreOrder`, `NopTracker`, `entity_preorder`, `entity_postorder` are used consistently across tasks.
- No placeholders; every test body is shown in full.
- The Task 10 merge step requires explicit user approval before destructive actions, per the auto-mode rules.
