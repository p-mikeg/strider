# Incremental Re-canonicalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `DedupNodes` optimizer pass and have the IR edit machinery re-canonicalize mutated nodes incrementally at the existing `clean()` drain.

**Architecture:** Finish the half-ported spidir `state.rs` state machine. Add a `NEEDS_RECANON` flag bit (spidir's `CANONICAL`, polarity-inverted to fit a function that starts fully canonical — so no O(n) init). Input-mutating edit verbs flag + enqueue the changed node; `clean()`'s drain canonicalizes it via a new `NodeCache::canonicalize` primitive, merging structural twins with the same `replace_value` `DedupNodes` used (so the cascade and fingerprint contract come for free).

**Tech Stack:** Rust, `cranelift-entity`, `hashbrown::HashTable`, the workspace `entity-utils` (`Worklist`, `DenseEntitySet`), `bitflags`.

---

## File structure

- `crates/strider-graph/src/node_cache.rs` — add `NodeCache::canonicalize` (dual of `get_or_alloc` for an existing node).
- `crates/strider-graph/src/graph.rs` — add `Graph::canonicalize_node` (wrapper) and `Graph::node_of_use` (accessor).
- `crates/strider-graph/tests/proptest_invariants.rs` — cache-primitive tests.
- `crates/strider-ir/src/function/state.rs` — add `NodeFlags::NEEDS_RECANON`.
- `crates/strider-ir/src/function/edit.rs` — `enqueue_for_recanon`, `canonicalize_node` helper, `clean()` drain arm, hooks in `update_input` + `replace_value`.
- `crates/strider-opt/src/lib.rs`, `crates/strider-opt/src/opt/mod.rs` — delete `DedupNodes` registration + module.
- `crates/strider-opt/src/opt/dedup_nodes/` — delete (repurpose tests into edit.rs / an integration test).

---

### Task 1: `NodeCache::canonicalize` primitive + `Graph` wrappers

**Files:**
- Modify: `crates/strider-graph/src/node_cache.rs` (after `get_or_alloc`, ~line 117)
- Modify: `crates/strider-graph/src/graph.rs` (near `value_of_use` ~242 and the cache-delegating methods)
- Test: `crates/strider-graph/tests/proptest_invariants.rs`

- [ ] **Step 1: Write the failing test** (append to `proptest_invariants.rs`)

```rust
#[test]
fn canonicalize_merges_a_mutated_twin() {
    // A = Add(x, y) (cached). C = Add(x, z); rewire z->y so C becomes a
    // structural twin of A (and is invalidated). canonicalize_node(C) must
    // return A; canonicalize_node of a unique node returns None.
    let mut g = TestGraph::new();
    let x = g.create_node(TestKind::Const(1), [], [TestVal::Int]);
    let y = g.create_node(TestKind::Const(2), [], [TestVal::Int]);
    let z = g.create_node(TestKind::Const(3), [], [TestVal::Int]);
    let xv = g.node_outputs(x)[0];
    let yv = g.node_outputs(y)[0];
    let zv = g.node_outputs(z)[0];
    let a = g.create_node(TestKind::Add, [xv, yv], [TestVal::Int]);
    let c = g.create_node(TestKind::Add, [xv, zv], [TestVal::Int]);
    assert_ne!(a, c, "different inputs => not deduped at creation");
    // Rewire C's second input z -> y (invalidates C's cache entry).
    let c_use1 = g.node_input_id_at(c, 1).unwrap();
    g.update_input(c_use1, yv);
    // Now C is structurally Add(x, y) == A, but un-canonical.
    assert_eq!(g.canonicalize_node(c), Some(a), "mutated twin canonicalizes to A");
    // A unique node re-inserts and returns None.
    let d = g.create_node(TestKind::Add, [yv, xv], [TestVal::Int]); // (y,x) order differs
    assert_eq!(g.canonicalize_node(d), None, "unique node has no twin");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p strider-graph canonicalize_merges_a_mutated_twin`
Expected: FAIL — `no method named canonicalize_node`.

- [ ] **Step 3: Add `NodeCache::canonicalize`** in `node_cache.rs` (immediately after `get_or_alloc`)

```rust
    /// Re-canonicalize an EXISTING node whose inputs may have changed (the dual
    /// of [`get_or_alloc`](Self::get_or_alloc) for a node already in the store).
    ///
    /// Returns `Some(twin)` if a structurally-equal OTHER cacheable node is
    /// already cached — the caller merges `node` into `twin`. Returns `None` if
    /// the node is not cacheable, or if no twin exists (in which case `node` is
    /// (re-)inserted as its own canonical representative). Touches no edges.
    pub(crate) fn canonicalize<N, V, C: NodeCacheable<N, V>>(
        &mut self,
        store: &RawStore<N, V>,
        node: NodeId,
    ) -> Option<NodeId>
    where
        V: Clone,
    {
        let kind = store.kind_of(node);
        if !C::should_cache(kind) {
            return None;
        }
        let inputs = store.input_values(node);
        let outputs = store.output_kinds(node);
        let h = Self::avoid_sentinel(C::hash(kind, &inputs, &outputs));
        // Probe for a structurally-equal OTHER node (exclude `node` itself).
        if let Some(&twin) = self
            .table
            .find(h, |&cand| cand != node && C::eq(store, cand, kind, &inputs, &outputs))
        {
            return Some(twin);
        }
        // No twin: ensure `node` is its own canonical entry. It was invalidated
        // when its inputs changed (hash == HASH_NONE), so insert it now.
        if self.node_hashes[node] == HASH_NONE {
            self.table
                .insert_unique(h, node, |&existing| self.node_hashes[existing]);
            self.node_hashes[node] = h;
        }
        None
    }
```

- [ ] **Step 4: Add `Graph::canonicalize_node` and `Graph::node_of_use`** in `graph.rs`

```rust
    /// Re-canonicalize `node` against the dedup cache after its inputs changed.
    /// `Some(twin)` => an existing structurally-equal node the caller should
    /// merge `node` into; `None` => `node` is now the canonical representative
    /// (or is non-cacheable). See [`NodeCache::canonicalize`].
    pub fn canonicalize_node(&mut self, node: NodeId) -> Option<NodeId> {
        self.cache.canonicalize::<N, V, C>(&self.store, node)
    }

    /// The node that owns input slot `use_id` (the consumer of that edge).
    pub fn node_of_use(&self, use_id: UseId) -> NodeId {
        self.store.inputs[use_id].node_id
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p strider-graph canonicalize_merges_a_mutated_twin`
Expected: PASS.

- [ ] **Step 6: Run the whole crate + commit**

Run: `cargo test -p strider-graph` (expect all green), `cargo clippy -p strider-graph` (no warnings).

```bash
git add crates/strider-graph/
git commit -m "feat(graph): NodeCache::canonicalize — re-dedup a mutated existing node"
```

---

### Task 2: `NEEDS_RECANON` flag + `clean()` canonicalization arm + edit-verb hooks

**Files:**
- Modify: `crates/strider-ir/src/function/state.rs:24-31` (the `NodeFlags` bitflags)
- Modify: `crates/strider-ir/src/function/edit.rs` (`update_input` ~496, `replace_value` ~598, `clean` ~401, new helpers)
- Test: `crates/strider-ir/src/function/edit.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** (append to edit.rs's test module; mirror the existing `single_region_builder` fixture used by other edit tests)

```rust
    #[test]
    fn clean_merges_a_mutated_twin_and_absorbs_fingerprint() {
        // Build A = Add(x, y) and C = Add(x, z) under DISTINCT fingerprints,
        // both used by a Return. Rewire C's z -> y so C becomes A's twin, then
        // clean(): C must merge into A (C dead, A live, the Return now uses A),
        // and A's fingerprint must absorb C's address (superset contract).
        let (mut f, x, y, z) = /* helper building three int consts + entry */;
        // ... build A under addr 0xA, C under addr 0xC, a Return consuming both.
        // (Concrete fixture: use IRBuilderExt::build_int_binary_operation with
        //  set_lift_addr brackets, as the flag_cmp tests do.)
        let mut ctx = EditFunction::new(&mut f).unwrap();
        let c_use_z = ctx.function().node_input_id_at(c_node, 1).unwrap();
        ctx.update_input(c_use_z, y_val); // C becomes Add(x, y) == A
        ctx.clean();
        assert!(!ctx.is_live(c_node), "the duplicate C is culled");
        assert!(ctx.is_live(a_node), "the survivor A stays live");
        assert!(ctx.function().asm_fingerprint(a_node).contains(&0xC),
            "A absorbs C's asm address (superset contract)");
    }
```

> NOTE for the implementer: write the fixture concretely using the existing
> `test_fixtures::single_region_builder` + `IRBuilderExt` + `set_lift_addr`
> bracketing (see `flag_cmp_canonicalize/tests.rs` for the address-stamping
> pattern). Bind `a_node`/`c_node`/`y_val` from the builder.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p strider-ir clean_merges_a_mutated_twin`
Expected: FAIL — after `clean()`, `c_node` is still live (no canonicalization yet).

- [ ] **Step 3: Add `NEEDS_RECANON`** to `NodeFlags` in `state.rs`

```rust
    pub(crate) struct NodeFlags: u8 {
        const ENQUEUED = 0b01;
        const OUTPUT_KILLED = 0b10;
        /// The node's inputs changed, so it may no longer be the canonical
        /// representative of its `(kind, inputs, output-kinds)` key — `clean`
        /// must re-canonicalize it. (spidir's `CANONICAL`, inverted: this graph
        /// starts fully canonical via the construction cache, so the natural
        /// dual is a set-on-dirty bit needing no O(n) initialization.)
        const NEEDS_RECANON = 0b100;
    }
```

- [ ] **Step 4: Add the `enqueue_for_recanon` + `canonicalize_node` helpers** in edit.rs (near `enqueue`, ~318)

```rust
    /// Flag a node whose inputs just changed as maybe-non-canonical and enqueue
    /// it for the next `clean` drain.
    fn enqueue_for_recanon(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node) {
            self.state.flags[node].insert(NodeFlags::NEEDS_RECANON);
            self.enqueue(node);
        }
    }

    /// Re-canonicalize a live node flagged `NEEDS_RECANON`: merge it into an
    /// existing structural twin if one exists, else (inside `graph.canonicalize_node`)
    /// it becomes the canonical representative.
    fn canonicalize_node(&mut self, node: NodeId) {
        self.state.flags[node].remove(NodeFlags::NEEDS_RECANON);
        if let Some(twin) = self.function.graph_mut().canonicalize_node(node) {
            // A cacheable node is always single-value-output; the Err arm is defensive.
            let Ok([node_out]) = self.function.node_outputs_exact::<1>(node) else {
                return;
            };
            let [twin_out] = self
                .function
                .node_outputs_exact::<1>(twin)
                .expect("a cacheable twin is single-value-output");
            // replace_value absorbs node's fingerprint into twin, redirects every
            // use, enqueues node for the dead-cull, AND (via its own consumer hook)
            // re-flags the redirected consumers — that is the cascade, for free.
            let _ = self.replace_value(node_out, twin_out);
        }
    }
```

- [ ] **Step 5: Hook `update_input` and `replace_value`** in edit.rs

In `update_input` (after the `graph_mut().update_input(...)` call, ~503):

```rust
        self.function.graph_mut().update_input(input_id, output_id);
        // The consuming node's inputs changed — it may now be a structural twin.
        let consumer = self.function.graph().node_of_use(input_id);
        self.enqueue_for_recanon(consumer);
```

In `replace_value` (snapshot consumers before the redirect, enqueue after; ~605):

```rust
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool> {
        let into = self.function.producer(new);
        let from = self.function.producer(old);
        // Consumers of `old` will have their inputs rewired to `new` below — flag
        // them for re-canonicalization (replace_all_uses bypasses update_input).
        let consumers: smallvec::SmallVec<[NodeId; 4]> = self
            .function
            .graph()
            .value_uses(old)
            .map(|(consumer, _)| consumer)
            .collect();
        self.function.extend_asm_fingerprint_from(into, from);
        let changed = self.replace_all_uses(old, new)?;
        self.enqueue_killed_def_node(from);
        for consumer in consumers {
            self.enqueue_for_recanon(consumer);
        }
        Ok(changed)
    }
```

- [ ] **Step 6: Add the canonicalization arm to `clean()`** in edit.rs (~401)

```rust
    pub fn clean(&mut self) {
        while let Some(node) = self.dequeue() {
            let flags = self.state.flags[node];
            self.state.flags[node].remove(NodeFlags::OUTPUT_KILLED);
            // Deadness recheck first: a dead node is killed and not canonicalized.
            if flags.contains(NodeFlags::OUTPUT_KILLED) && self.is_node_dead(node) {
                self.kill_node(node);
                continue;
            }
            // Re-canonicalize a node whose inputs changed.
            if flags.contains(NodeFlags::NEEDS_RECANON) {
                self.canonicalize_node(node);
            }
        }
    }
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p strider-ir clean_merges_a_mutated_twin`
Expected: PASS.

- [ ] **Step 8: Run the crate + commit**

Run: `cargo test -p strider-ir` (all green), `cargo clippy -p strider-ir` (no warnings), `cargo fmt -p strider-ir`.

```bash
git add crates/strider-ir/
git commit -m "feat(ir): re-canonicalize mutated nodes at clean() (finish spidir state port)"
```

---

### Task 3: Delete `DedupNodes`, repurpose its tests

**Files:**
- Modify: `crates/strider-opt/src/lib.rs` (drop `pub use opt::dedup_nodes::DedupNodes;` ~76 and `p.add(DedupNodes);` ~156)
- Modify: `crates/strider-opt/src/opt/mod.rs` (drop `pub mod dedup_nodes;`)
- Delete: `crates/strider-opt/src/opt/dedup_nodes/mod.rs`, `crates/strider-opt/src/opt/dedup_nodes/tests.rs`

- [ ] **Step 1: Read `dedup_nodes/tests.rs`** and identify the behavioral cases (the PhiCollapse→twin→merge scenario). These move to Task 4's integration test (do not lose coverage).

Run: `cat crates/strider-opt/src/opt/dedup_nodes/tests.rs`

- [ ] **Step 2: Remove the registration + re-export** (lib.rs)

Delete the line `pub use opt::dedup_nodes::DedupNodes;` and the line `p.add(DedupNodes);` from `default_pipeline()`.

- [ ] **Step 3: Remove the module** (opt/mod.rs)

Delete `pub mod dedup_nodes;`.

- [ ] **Step 4: Delete the pass files**

```bash
git rm crates/strider-opt/src/opt/dedup_nodes/mod.rs crates/strider-opt/src/opt/dedup_nodes/tests.rs
```

- [ ] **Step 5: Build to verify nothing else references `DedupNodes`**

Run: `cargo build -p strider-opt`
Expected: compiles (if any `DedupNodes` reference remains, grep + remove it).
Run: `grep -rn "DedupNodes" crates/` — expected: no hits.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(opt): delete DedupNodes — replaced by clean()-time re-canonicalization"
```

---

### Task 4: Integration — motivating case + no-twins invariant, full gate

**Files:**
- Test: `crates/strider-opt/tests/` (new file `recanonicalization.rs`) or fold into an existing integration test.

- [ ] **Step 1: Write the motivating-case test** (adapted from `dedup_nodes/tests.rs`)

The scenario the deleted `DedupNodes` doc described: after `PhiCollapse` rewires two trivial phis to the same `InitialVar`, two `Truncate(InitialVar)` nodes become twins; the pipeline must merge them so `value_range` carries a guard's bound to a jump-table index. Build that IR (reuse the dedup_nodes test fixture), run `default_pipeline()`, and assert the two `Truncate`s are merged (one reachable `Truncate(InitialVar)`).

```rust
#[test]
fn pipeline_merges_phi_collapse_twins() {
    // (Port the fixture from the deleted dedup_nodes/tests.rs.)
    // After running the pipeline, exactly one Truncate(InitialVar) is reachable.
}
```

- [ ] **Step 2: Write the no-twins invariant test**

```rust
#[test]
fn pipeline_leaves_no_structural_twins() {
    // After a full default_pipeline() run on a representative fixture, no two
    // reachable cacheable single-value-output nodes share (kind, inputs, output).
    // Walk reachable nodes, key each cacheable single-output node, assert no
    // duplicate keys (mirror the old DedupNodes CseKey).
}
```

- [ ] **Step 3: Run them**

Run: `cargo test -p strider-opt recanonicalization` — expect PASS.

- [ ] **Step 4: Full workspace gate**

Run (gate on real exit codes — do NOT pipe in a way that hides failures):
```bash
cargo test --workspace            # expect ~3168 pass (minus DedupNodes' own tests, plus the new ones)
cargo clippy --workspace --all-targets   # no warnings
cargo fmt --check                 # clean (bar the pre-existing strider-py macro-body quirk)
```
Then the Python suite (validate sweep / bindings unaffected):
```bash
cd crates/strider-py && uv run maturin develop --quiet && uv run pytest -q   # 852 pass
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(opt): pipeline re-canonicalization — motivating case + no-twins invariant"
```

---

## Notes for the implementer
- `value_uses(value)` yields `(consumer_node, use_id)` pairs — see `flag_cmp_canonicalize`'s use of it.
- A cacheable kind is always single-value-output, so `node_outputs_exact::<1>` in `canonicalize_node` never errors for a cacheable node; the `Err` arm is defensive.
- Do NOT hook `add_node_input`/`remove_node_input`: they target only non-cacheable variadic nodes (`Call`/`Region`/`Phi`), which `canonicalize` gates out anyway.
- `redirect_input` routes through `update_input`, so it is covered by the `update_input` hook automatically.
