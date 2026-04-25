# entity-utils / graphmock / graphwalk Review (Round 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve all `clippy::pedantic`/`clippy::nursery` findings in the three crates that survived round 1, plus a handful of minor readability / ergonomics fixes — without any public-API breakage and with all tests still green.

**Architecture:** Work lands on branch `review/graph-crates` in worktree `.worktrees/review-graph-crates`. Each task is a self-contained slice with its own commit. No behavior changes; the only "API additions" are an `IntoIterator for &DenseEntitySet<E>` impl (purely additive) and `len` / `is_empty` accessors on `DenseEntitySet`. No changes to downstream consumers (`ir/src/walk.rs` keeps compiling unchanged).

**Tech Stack:** Rust 2024, `cranelift-entity`, `cranelift-bitset`, `expect-test`, `itertools`. Workspace lints in `Cargo.toml`: `unwrap_used / expect_used / panic / unreachable / todo` denied; `redundant_closure / map_unwrap_or / match_same_arms / must_use_candidate / missing_errors_doc` warned. The plan also resolves the additional `pedantic` / `nursery` warnings observed below — without enabling those lint groups workspace-wide.

**Working directory:** All commands assume `cwd = /home/mike/Desktop/strider/.worktrees/review-graph-crates`.

---

## Findings

### Correctness

After reading the three crates plus `crates/ir/src/walk.rs` (the only external consumer of `entity-utils::set::DenseEntitySet` and `graphwalk::PreOrder`), and re-running the unit tests in this worktree, **no correctness defects were found**. Round 1 (`utils-review`) already fixed the `Worklist::enqueue` dedup bug and `FromIterator` arity; Round 1.x added the `Debug` derives and edge-case tests. The post-order traversal semantics, root-order RPO invariant, self-loop handling, and `NopTracker` tree-only contract are all exercised by tests.

One observation worth noting (not a defect):

- **`Worklist` is unused outside `entity-utils`** (`grep -rn 'Worklist' crates/` shows only the file that defines it and its own tests). It is public API and library-shaped, but the workspace currently has no consumer. The plan does **not** propose removing it — it is a documented general-purpose primitive — but flag it explicitly so the user can decide. If the user wants it deleted, that becomes a separate (very small) task; otherwise we leave it.

### Style / pedantic warnings (verified via `cargo clippy -- -W clippy::pedantic -W clippy::nursery`)

| # | File | Lint | Note |
|---|------|------|------|
| 1 | `entity-utils/src/set.rs:56` | `iter_without_into_iter` | `DenseEntitySet::iter` exists but no `IntoIterator for &DenseEntitySet<E>`. |
| 2 | `entity-utils/src/set.rs:90` | `use_self` | `DenseEntitySet::with_capacity(...)` inside an `impl DenseEntitySet`. |
| 3 | `graphwalk/src/lib.rs:136` | `missing_const_for_fn` | `PreOrderContext::new` is purely `Vec::new()` — `const`-able. |
| 4 | `graphwalk/src/lib.rs:235` | `missing_const_for_fn` | Same for `PostOrderContext::new`. |
| 5 | `graphmock/src/lib.rs:33` | `missing_const_for_fn` | `Graph::entry`. |
| 6 | `graphmock/src/lib.rs:70` | `missing_panics_doc` | `pub fn graph(...)` panics on malformed lines, undocumented. |
| 7 | `graphmock/src/lib.rs:86,87` | `redundant_closure` | `map(|p| p.trim())` → `map(str::trim)`. |
| 8 | `graphmock/src/lib.rs:195` (test) | `many_single_char_names` | Test-helper bindings — silence with a localized `#[allow]` rather than rename. |
| 9 | `graphwalk/tests/postorder.rs` (5 sites) | `needless_raw_string_hashes` | `expect![[r#"..."#]]` blocks contain no `"` and no `#`. |
| 10 | `graphwalk/tests/postorder.rs:207` | `redundant_clone` | `let mut sorted = order.clone();` — `order` is unused after. |
| 11 | `graphwalk/tests/preorder.rs:72` | `needless_collect` | `let order: Vec<_> = ....collect(); assert!(order.is_empty())`. |

### Minor cleanups (not lint-driven)

| # | File | Note |
|---|------|------|
| 12 | `graphmock/src/lib.rs` (tests) | Test name typo `loop_grpah` → `loop_graph`. |
| 13 | `graphmock/src/lib.rs` `graph()` | Replace `line.split("->").collect::<Vec<_>>().try_into().unwrap()` with `line.split_once("->")` + `Result::ok().expect(...)` (still test-only `unwrap`-equivalent under the existing `#[allow]`, but expresses "exactly two halves" intent better). |
| 14 | `entity-utils/src/set.rs` | Add `#[must_use]` `len()` and `is_empty()` for parity with `Vec`/`HashSet` and to make state queries cheap (`bitset.len()` exists). |

### Performance

No hot-path issues found. `PreOrderContext` / `PostOrderContext` already do the expected "skip-already-visited successor before pushing" optimization that round 1 noted. Worklist's `enqueue` is O(1) via the bitset. No allocator-per-node hotspots in graphmock that aren't already collapsed. **No performance tasks proposed.**

---

## File touch map

| File | What happens |
|------|--------------|
| `crates/entity-utils/src/set.rs` | Fix `use_self`. Add `IntoIterator for &DenseEntitySet<E>` forwarding to `iter()`. Add `#[must_use] len()` / `is_empty()`. Add a tiny test for each. |
| `crates/graphwalk/src/lib.rs` | Make `PreOrderContext::new` and `PostOrderContext::new` `const fn`. |
| `crates/graphmock/src/lib.rs` | Make `Graph::entry` `const fn`. Add `# Panics` doc on `graph()`. Replace `|p| p.trim()` closures with `str::trim`. Replace the `try_into().unwrap()` two-half split with `split_once`. Localized `#[allow(clippy::many_single_char_names)]` on the affected test fns. Rename `loop_grpah` → `loop_graph`. |
| `crates/graphwalk/tests/postorder.rs` | Drop the now-unnecessary `r#"..."#` hashes (5 places). Drop the redundant `order.clone()`. |
| `crates/graphwalk/tests/preorder.rs` | Replace the `Vec<_> + is_empty` pattern with `.next().is_none()`. |

No `Cargo.toml` or workspace-lint changes — pedantic/nursery stay opt-in for the developer rather than enforced.

---

## Verification gate (run after every task)

```bash
cargo test  -p entity-utils -p graphwalk -p graphmock
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -D warnings
```

Both must pass. The pedantic/nursery sweep is verified once at the very end with:

```bash
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- -W clippy::pedantic -W clippy::nursery 2>&1 | grep -c '^warning:'
```

Expected: `0` (after the final task).

A whole-workspace check is the final gate:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: pass.

---

## Task 1: `entity-utils::set` — `use_self`, `len`/`is_empty`, `IntoIterator` impl

**Files:**
- Modify: `crates/entity-utils/src/set.rs`

- [ ] **Step 1: Fix `use_self` in `from_iter`.**

In `crates/entity-utils/src/set.rs`, change

```rust
let mut set = DenseEntitySet::with_capacity(min_size);
```

to

```rust
let mut set = Self::with_capacity(min_size);
```

- [ ] **Step 2: Add `len` and `is_empty`.**

Insert these methods inside `impl<E: EntityRef> DenseEntitySet<E>`, right after `clear`:

```rust
    /// Returns the number of entities currently in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bitset.len()
    }

    /// Returns `true` if the set contains no entities.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bitset.is_empty()
    }
```

- [ ] **Step 3: Add `IntoIterator for &DenseEntitySet<E>`.**

Append this impl after the existing `Iterator for Iter<'_, E>` impl:

```rust
impl<'a, E: EntityRef> IntoIterator for &'a DenseEntitySet<E> {
    type Item = E;
    type IntoIter = Iter<'a, E>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
```

- [ ] **Step 4: Add tests for `len` / `is_empty` / `for &set` loop.**

Append inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn len_and_is_empty_track_membership() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        s.insert(Id(1));
        s.insert(Id(2));
        assert_eq!(s.len(), 2);
        assert!(!s.is_empty());
        s.insert(Id(1)); // idempotent
        assert_eq!(s.len(), 2);
        s.remove(Id(1));
        assert_eq!(s.len(), 1);
        s.clear();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn into_iter_for_ref_yields_same_as_iter() {
        let s: DenseEntitySet<Id> = [Id(3), Id(1), Id(4)].into_iter().collect();
        let by_iter: Vec<_> = s.iter().collect();
        let by_for: Vec<_> = (&s).into_iter().collect();
        assert_eq!(by_iter, by_for);
        // Also exercise the for-loop sugar that the new impl unlocks.
        let mut by_for_sugar = Vec::new();
        for id in &s {
            by_for_sugar.push(id);
        }
        assert_eq!(by_iter, by_for_sugar);
    }
```

- [ ] **Step 5: Verify.**

```bash
cargo test  -p entity-utils
cargo clippy -p entity-utils --all-targets -- -D warnings
```

Expected: all tests pass; clippy clean.

- [ ] **Step 6: Commit.**

```bash
git add crates/entity-utils/src/set.rs
git commit -m "$(cat <<'EOF'
refactor(entity-utils): add len/is_empty and IntoIterator for &DenseEntitySet

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `graphwalk` — `const fn` for context constructors

**Files:**
- Modify: `crates/graphwalk/src/lib.rs`

- [ ] **Step 1: Make `PreOrderContext::new` const.**

In `crates/graphwalk/src/lib.rs`, change

```rust
    /// Creates an empty context.
    #[must_use]
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }
```

(inside `impl<N: Copy> PreOrderContext<N>`) to:

```rust
    /// Creates an empty context.
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }
```

- [ ] **Step 2: Make `PostOrderContext::new` const.**

Apply the same change in `impl<N: Copy> PostOrderContext<N>`:

```rust
    /// Creates an empty context.
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }
```

- [ ] **Step 3: Verify.**

```bash
cargo test  -p graphwalk
cargo clippy -p graphwalk --all-targets -- -D warnings
```

- [ ] **Step 4: Commit.**

```bash
git add crates/graphwalk/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(graphwalk): make PreOrderContext/PostOrderContext::new const fn

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `graphmock` — `const fn entry`, doc panics, closure cleanups, parser tweak, test typo

**Files:**
- Modify: `crates/graphmock/src/lib.rs`

- [ ] **Step 1: Make `Graph::entry` const.**

In `crates/graphmock/src/lib.rs`, change

```rust
    #[must_use]
    pub fn entry(&self) -> NodeId {
        NodeId(0)
    }
```

to

```rust
    #[must_use]
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }
```

- [ ] **Step 2: Document the panic on malformed input and replace the `try_into` split.**

Replace the existing `pub fn graph(...)` body so the doc-comment names the panic and the parser uses `split_once`:

```rust
/// Build a [`Graph`] from a tiny edge-list DSL.
///
/// Each non-blank line has the form `pred[, pred…] -> succ[, succ…]`. Whitespace
/// around names is trimmed. Names are interned: a name's first appearance creates
/// a node, later appearances reuse the same id.
///
/// # Panics
///
/// Panics if a non-blank line does not contain exactly one `->` separator. This
/// helper is test-only; the input is a hard-coded literal in callers, so a
/// malformed line is a programmer error rather than a runtime condition.
#[must_use]
pub fn graph(input: &str) -> Graph {
    let mut graph = Graph {
        nodes: PrimaryMap::new(),
        nodes_by_name: std::collections::HashMap::default(),
    };

    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // graphmock is a test-only DSL helper; input is a hard-coded string in
        // downstream tests, so a malformed line is a programmer error, not a
        // runtime condition that deserves error plumbing.
        #[allow(clippy::unwrap_used)]
        let (preds, succs) = line
            .split_once("->")
            .unwrap_or_else(|| panic!("graphmock: line missing `->`: {line:?}"));
        let preds = preds.split(',').map(str::trim);
        let succs: Vec<_> = succs.split(',').map(str::trim).collect();

        for pred in preds {
            let pred = graph.get_or_create(pred);
            for succ in &succs {
                let succ = graph.get_or_create(succ);
                graph.add_succ(pred, succ);
            }
        }
    }

    graph
}
```

(Note: the workspace forbids `clippy::panic`, so the `panic!` here also needs the localized allow. Wrap the `unwrap_or_else` line — and only that line — in `#[allow(clippy::unwrap_used, clippy::panic)]`. Keep the existing test-only helper rationale comment above it.)

- [ ] **Step 3: Fix the test typo.**

Rename the test fn `loop_grpah` to `loop_graph`. (Single edit; nothing references it externally.)

- [ ] **Step 4: Silence `many_single_char_names` in the affected test functions.**

Add `#[allow(clippy::many_single_char_names)]` immediately above each test fn that uses `a/b/c/d` (or similar) as bindings — currently `fan_out_and_fan_in` (`a, b, c, d`). Do not rename — the names mirror the DSL graph and renaming would hurt readability.

- [ ] **Step 5: Verify.**

```bash
cargo test  -p graphmock
cargo clippy -p graphmock --all-targets -- -D warnings
```

- [ ] **Step 6: Commit.**

```bash
git add crates/graphmock/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(graphmock): const fn entry, document graph() panic, tidy parser

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `graphwalk` tests — needless raw-string hashes, redundant clone, needless collect

**Files:**
- Modify: `crates/graphwalk/tests/postorder.rs`
- Modify: `crates/graphwalk/tests/preorder.rs`

- [ ] **Step 1: Drop `r#"..."#` hashes from all five `expect![[...]]` blocks in `postorder.rs`.**

For each of the five `expect_postorder!` invocations whose expected_full body uses `r#"..."#`, change the opening `r#"` to `r"` and the closing `"#` to `"`. The bodies contain no `"` characters, so no escaping is needed.

The five sites: lines 58, 75, 94, 114, 133 (one per `test_postorder!` macro call: `straight_line`, `diamond`, `straight_line_skips`, `simple_loop`, `loop_diamond`).

- [ ] **Step 2: Drop the redundant `order.clone()` in `nop_tracker_on_a_tree`.**

In `crates/graphwalk/tests/postorder.rs`, change

```rust
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["a", "b", "c", "d"]);
```

to

```rust
    let mut sorted = order;
    sorted.sort();
    assert_eq!(sorted, vec!["a", "b", "c", "d"]);
```

(Removing `.clone()` is the documented fix; `order` is not used after this point.)

- [ ] **Step 3: Replace the needless `Vec` collect in `preorder.rs`.**

In `crates/graphwalk/tests/preorder.rs`, change

```rust
#[test]
fn empty_roots_yields_nothing() {
    let g = graphmock::graph("a -> b");
    let order: Vec<_> = entity_preorder(&g, core::iter::empty()).collect();
    assert!(order.is_empty());
}
```

to

```rust
#[test]
fn empty_roots_yields_nothing() {
    let g = graphmock::graph("a -> b");
    assert!(entity_preorder(&g, core::iter::empty()).next().is_none());
}
```

- [ ] **Step 4: Verify.**

```bash
cargo test  -p graphwalk
cargo clippy -p graphwalk --all-targets -- -D warnings
```

- [ ] **Step 5: Commit.**

```bash
git add crates/graphwalk/tests/postorder.rs crates/graphwalk/tests/preorder.rs
git commit -m "$(cat <<'EOF'
test(graphwalk): drop needless raw-string hashes, clone, and collect

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Final whole-workspace verification

**Files:** none.

- [ ] **Step 1: Workspace test suite.**

```bash
cargo test --workspace
```

Expected: all tests pass. (Round 1 left the workspace green; this round only adds tests / clippy fixes.)

- [ ] **Step 2: Workspace clippy under the project's enforced lints.**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: no errors, no warnings.

- [ ] **Step 3: Pedantic / nursery sweep on the three reviewed crates.**

```bash
cargo clippy -p entity-utils -p graphwalk -p graphmock --all-targets -- \
    -W clippy::pedantic -W clippy::nursery 2>&1 \
  | tee /tmp/pedantic.log \
  | grep -c '^warning:'
```

Expected output: `0`.

If the count is non-zero, inspect `/tmp/pedantic.log`, fix the remaining lint, and re-run before finishing.

- [ ] **Step 4: Final check that no consumer broke.**

```bash
cargo build -p ir
cargo test  -p ir
```

Expected: pass. (`ir` is the only external consumer of `entity-utils` / `graphwalk`; this catches accidental API breaks.)
