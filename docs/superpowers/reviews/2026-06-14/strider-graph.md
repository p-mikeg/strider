# Deep code audit — `strider-graph`

Date: 2026-06-14
Scope: `crates/strider-graph/src/{graph,node_cache,storage,cache,iter,ids,petgraph_view,lib}.rs`
Method: read-only. Verified each finding against actual code + real call paths in
`strider-ir` / `strider-opt` / `strider-pattern`. Comments/doc-comments treated as
claims to verify, not ground truth.

Overall: the crate is in good shape. The hash-on-demand cache is sound, eviction is
O(1), collision coexistence and twin coexistence are correctly handled, use-list
maintenance is bidirectionally consistent across every mutation verb, and compaction
re-keys the cache. Findings below are mostly hardening / API-ergonomics / test-gap,
with one MED soundness footgun in the generic `replace_all_uses` self-replace return
value (compensated by every current caller, but latent).

---

## Findings

### GR-1 — `replace_all_uses(v, v)` reports `true` (uses replaced) while doing nothing
- Dimension: SOUNDNESS / EDGE CASE
- Severity: MED
- Confidence: HIGH
- Location: `graph.rs:467-474` (`replace_all_uses`), `iter.rs:136-143`
  (`replace_current_with`), `graph.rs:435-445` (`update_input`).
- What & why: `replace_all_uses` walks `old`'s use-list with `replace_current_with`,
  which unconditionally returns `true` after calling `update_input(current, new_val)`.
  When `old == new_val`, `update_input` early-returns as a no-op (`graph.rs:436-438`),
  but the cursor still advances and `replace_current_with` still returns `true`. So
  `replace_all_uses(v, v)` returns `true` ("at least one use replaced") even though it
  redirected nothing and `v` retains every use. The documented contract is "Returns
  `true` if at least one use was replaced" — a self-replace replaces zero uses yet
  returns `true`. Verified the only production caller, `EditFunction::replace_all_uses`
  (`strider-ir/src/function/edit.rs:611-635`), defends itself by special-casing
  `old != new` before delegating, so no current bug — but the generic primitive's
  return value is wrong, and a future direct caller would be misled (e.g. a
  fixed-point loop that treats `true` as "made progress" would spin).
- Proposed fix: guard at the top of `replace_all_uses`:
  `if old == new_val { return false; }` (and/or document the return as "true iff `old`
  had ≥1 use", which still mis-describes self-replace). The early guard is the honest
  fix and lets `EditFunction` drop its own `old != new` branch.

### GR-2 — No edge-case test for re-canonicalizing into an existing bucket / collision / self-replace
- Dimension: EDGE CASE (test gap)
- Severity: MED
- Confidence: HIGH
- Location: test suite `tests/proptest_invariants.rs`; mechanism in
  `node_cache.rs:128-157` (`canonicalize`), `83-117` (`get_or_alloc`).
- What & why: the suite covers dedup hit, distinct keys, invalidate+recreate, rebuild
  after compaction, sentinel-hash collision in `get_or_alloc`, and one
  `canonicalize_merges_a_mutated_twin`. Gaps that exercise load-bearing branches with
  no coverage:
  1. `canonicalize` re-insert branch (`node_cache.rs:151-155`): a mutated node with NO
     twin must be re-inserted as its own representative, then a later identical
     `create_node` must dedup to it. No test asserts the re-insertion actually
     re-establishes the entry (the existing twin test returns `Some(a)` and the "unique"
     `d` is freshly created, so the re-inserted-self path's *dedup-ability afterwards* is
     never checked).
  2. `canonicalize` under a hash collision: a mutated node whose new shape collides
     (same hash, different structure) with an existing entry must NOT be reported as a
     twin. Only `get_or_alloc` collision is tested (`sentinel_hash_still_caches_and_evicts`).
  3. self-replace: `replace_all_uses(v, v)` (see GR-1) — no test pins the return value
     or the no-op behaviour.
  4. self-loop / a node consuming its own output via `add_node_input` then
     `canonicalize` — never constructed.
- Proposed fix (names only, do not author here):
  `canonicalize_reinserts_unique_mutated_node_then_dedups`,
  `canonicalize_ignores_hash_collision_non_twin` (use a `SentinelHashPolicy`-style
  always-collide hash + `canonicalize`), `replace_all_uses_self_is_noop_returns_false`,
  `add_self_loop_input_then_canonicalize`.

### GR-3 — `reachable_by_inputs` pushes duplicates; stack can transiently hold O(E) ids
- Dimension: RUNTIME / EDGE CASE
- Severity: LOW
- Confidence: HIGH
- Location: `graph.rs:602-617`.
- What & why: the worklist marks `visited[node]` only at pop time and pushes every
  input-producer unconditionally (`graph.rs:612-614`). A high-fan-in value (e.g. a
  constant consumed by thousands of nodes — exactly the `stress_10k_nodes` shape, or a
  shared SP/memory token) gets its producer pushed once per consuming edge, so the
  `stack` peaks at O(E) entries even though `order` stays O(V). Total time is still
  O(V+E), but the peak allocation is larger than necessary. Not O(n²), so LOW.
- Proposed fix: mark `visited[node] = true` at push time (check-then-push) instead of
  pop time, or skip pushing an already-visited producer. Either bounds the stack to
  O(V). Use `entity_utils::Worklist`/`DenseEntitySet` (already a project preference) to
  combine the dedup with the visited set.

### GR-4 — `corrupt_*` injectors are `#[cfg(feature = "test-injectors")]` but compiled into the public API
- Dimension: GENERALIZATION / hygiene
- Severity: LOW
- Confidence: HIGH
- Location: `graph.rs:476-497`; `Cargo.toml` `[features] test-injectors = []`.
- What & why: `corrupt_clear_first_use` / `corrupt_retarget_input` are `pub` and gated
  behind a normal (non-`dev`) cargo feature. A normal feature is additive and can be
  enabled by any downstream crate (or transitively unified by cargo), exposing
  graph-corruption verbs on the production `Graph`. They exist only to feed
  `strider-ir`'s use-list-consistency validator tests. Verified no production caller
  (`grep corrupt_` finds only the definitions).
- Proposed fix: move these into a `#[cfg(any(test, feature = "test-injectors"))]`
  block that is only enabled as a `dev-dependency` feature of the consuming test crate,
  or expose them via a `#[doc(hidden)]` + clearly test-only module. At minimum mark
  `#[doc(hidden)]` so they never surface in docs. (They're already feature-gated, hence
  LOW.)

### GR-5 — `canonicalize`'s soundness silently depends on "every input mutation went through `invalidate`"
- Dimension: SOUNDNESS (invariant fragility)
- Severity: LOW
- Confidence: MED
- Location: `node_cache.rs:142-156`; mutation verbs `graph.rs:392-461`.
- What & why: `canonicalize` re-hashes the node's CURRENT structure and, finding no
  twin, re-inserts only `if self.node_hashes[node] == HASH_NONE`
  (`node_cache.rs:151`). This is correct *only* because every structural mutation verb
  (`add_node_input`/`remove_node_input`/`update_input`/`detach_node_inputs`) calls
  `cache.invalidate` first, driving the stored hash to `HASH_NONE`. If a future verb
  mutated inputs without invalidating, the node would still be in the table under its
  STALE hash bucket, `canonicalize` would skip the re-insert (hash != NONE), and the
  node would be permanently mislocated — `invalidate` would later `expect`-panic
  (`node_cache.rs:178-181`) trying to find it under the new hash. The invariant is real
  and currently upheld, but it's load-bearing and only documented in prose. Verified
  all four current verbs invalidate.
- Proposed fix: add a debug-assert in `canonicalize` that, when it re-reads a
  not-`HASH_NONE` node, the stored hash equals the freshly recomputed `h`
  (`debug_assert_eq!(self.node_hashes[node], h)` in the no-twin branch before the
  `HASH_NONE` check). That converts the silent invariant into a loud test/dev failure
  the moment a verb forgets to invalidate.

### GR-6 — Accessor duplication: `node_kind` exists on both `Graph` and `RawStore`; `kind_of` vs `node_kind`
- Dimension: GENERALIZATION (duplication)
- Severity: LOW
- Confidence: HIGH
- Location: `storage.rs:185-189` (`kind_of`, pub), `storage.rs:212-216` (`node_kind`,
  pub(crate)) — two identical methods; `graph.rs:103-105` (`Graph::node_kind`).
- What & why: `RawStore` has both `kind_of(&self, NodeId) -> &N` (pub, cacher-facing)
  and `node_kind(&self, NodeId) -> &N` (pub(crate), graph-facing) with byte-identical
  bodies. The split is purely "which audience reads it"; there is no behavioural
  difference. Minor surface bloat in the generic core.
- Proposed fix: collapse to one (`kind_of`) and have `Graph::node_kind` call it; drop
  the `pub(crate) node_kind` on `RawStore`. Pure cleanup.

### GR-7 — `Graph::value_kind` requires `V: Copy` but the value-by-ref companion already covers all callers
- Dimension: GENERALIZATION (minor)
- Severity: LOW
- Confidence: MED
- Location: `graph.rs:114-131` (`value_kind` + `value_kind_ref`).
- What & why: both a `V: Copy` by-value getter and a by-ref getter exist. This is fine
  and intentional (ergonomic `== V::Foo` for the IR's `Copy` `ValueKind`), but note for
  completeness: `strider-pattern` (whose `V` may be non-`Copy`) uses only
  `value_kind_ref` (verified: 4 call sites, all `value_kind_ref`). The `Copy` getter is
  IR-convenience leaking a `Copy`-shaped assumption into the generic core. Not a bug —
  the bound is correctly method-local — just worth confirming it earns its keep.
- Proposed fix: none required; keep as-is (it's the documented ergonomic trade-off).
  Listed only so a future "remove the Copy getter" proposal can be pre-empted: it has a
  real IR caller base.

---

## Things verified sound (no finding)

- **Hash-on-demand cache**: `HashTable<NodeId>` + `SecondaryMap<NodeId,u64>` stores no
  owned keys; equality re-reads structure via `C::eq`. Collisions coexist (bucket walk),
  twins coexist (documented + correct), eviction is O(1) via cached hash. (`node_cache.rs`)
- **Sentinel handling**: `HASH_NONE = u64::MAX` remapped to `0` by `avoid_sentinel`
  before every store; membership = "stored hash ≠ sentinel". `get_or_alloc` sets
  `node_hashes[node]` AFTER `insert_unique`, and the rehash closure only re-hashes
  pre-existing members (all of which have valid hashes), so the not-yet-set new node is
  never read during a resize. Sound. (`node_cache.rs:99-116`)
- **Use-list bidirectional consistency**: every mutation verb invalidates the cache
  then link/unlinks; `remove_node_input` correctly decrements trailing `input_index`s;
  `replace_current_with` reads `next` before redirecting. Repeated-operand `Add(x,x)`
  yields 2 uses (tested). (`graph.rs:392-474`, `storage.rs:259-294`)
- **Compaction**: two-pass copy (nodes+outputs then inputs) ensures every remapped id
  exists before edges are rewritten; rebuilds use-lists and re-keys the cache; bumps
  generation; returns injective old→new remap. (`graph.rs:513-599`)
- **Id stability under `Clone`**: `RawStore` + `NodeCache` cloned verbatim, ids
  unchanged — faithful independent duplicate. (`graph.rs:57-66`)
- **`NeverCacheable`**: `should_cache=false` short-circuits before `hash`/`eq`, so
  non-`Hash`/`Eq` payloads (pattern `Box<dyn Fn>`) are storable. (`cache.rs`)
- **`invalidate` O(1)**: locates the bucket via cached hash, `expect` is guarded by the
  documented "non-sentinel ⇒ present" invariant (currently upheld; see GR-5).
