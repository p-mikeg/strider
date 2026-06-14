# Incremental re-canonicalization — delete `DedupNodes`, canonicalize at `clean()`

Date: 2026-06-14
Status: approved (design), ready for implementation

## Problem

The IR graph deduplicates structurally-identical cacheable nodes at
**creation** time: `NodeCache::get_or_alloc` keys on `(kind, inputs,
output-kinds)`, so building a node that already exists returns the existing
one. **Mutation breaks this asymmetrically.** The edit verbs
(`update_input`, and `replace_all_uses`, which loops `update_input` over every
consumer) call `cache.invalidate(node)` — they *remove* the rewired node from
the cache but never re-insert it. A node whose inputs changed into a structural
twin of an existing node therefore sits in the graph un-canonical. Two nodes
then compute the same value while analyses keyed on node identity (e.g.
`value_range` carrying a guard's bound to a jump-table index) treat them as
unrelated.

Today a dedicated pass, `DedupNodes`, repairs this: a full reverse-post-order
sweep, run every fixpoint iteration, that re-keys every cacheable
single-output node and merges twins via `replace_value`.

## Idea

Make the **mutation** path re-canonicalize the way the **creation** path
already does — but lazily, via the worklist that `EditFunction` already drains
at `clean()`. Replace the periodic O(n) `DedupNodes` sweep with O(affected)
incremental work that runs only on the nodes a rewrite actually touched.

This is, concretely, **finishing a half-completed port of spidir's
`crates/opt/src/state.rs`**. The local `FunctionState` already mirrors spidir's
(`live_nodes`, `roots`, a maybe-dead `queue`, and `NodeFlags`), and `clean()`
is spidir's drain loop — but with only two of spidir's three flag bits
(`ENQUEUED`, `OUTPUT_KILLED`) and only the dead-culling arm of the drain.
spidir's third bit, `CANONICAL`, and the canonicalization arm of the drain are
exactly what is missing.

## Design

### 1. `NodeFlags::CANONICAL` (one new bit, `strider-ir`)

Semantics: "this node has been hashed into the dedup cache as its own canonical
representative." Cleared whenever the node's inputs change.

### 2. Input-change hooks → clear `CANONICAL` + `enqueue` (`strider-ir`)

The `EditFunction` input-mutating verbs — `update_input`, `redirect_input`,
`add_node_input`, `remove_node_input` — clear `CANONICAL` on the mutated node
and `enqueue` it onto the existing `queue`. `replace_value` snapshots `old`'s
consumers *before* the redirect and does the same to each (its internal redirect
goes through the raw `Graph` layer, so it cannot rely on the verb hooks). This
is the cascade seed; it reuses the existing `queue`, `ENQUEUED` bit, and
`enqueue`/`dequeue` machinery unchanged.

### 3. One new arm in the `clean()` drain (`strider-ir`)

The drain loop already pops live nodes (skipping dead ones via `dequeue`) and
rechecks deadness for `OUTPUT_KILLED` nodes. Add: for a dequeued live node that
is **not dead** and **not `CANONICAL`** and is a **cacheable single-value-output**
node, canonicalize it:

- `Some(twin)` → `replace_value(node_out, twin_out)`. This absorbs the
  duplicate's asm-fingerprint into the survivor (superset-only contract), and
  its redirect clears `CANONICAL` + enqueues the duplicate's consumers (the
  cascade); the now-unused duplicate falls dead and is culled by the same drain.
- `None` → set `CANONICAL`.

Deadness is checked first; if the node is dead it is killed and canonicalization
is skipped. The loop already runs until the queue empties, so the cascade
settles with no extra control flow.

### 4. `NodeCache::canonicalize(store, node)` (one new generic primitive, `strider-graph`)

The dual of `get_or_alloc` for an *existing* node whose inputs may have changed:

- not cacheable (`should_cache` false → phis/calls/stores/regions/control) →
  return `None`; such kinds are never deduped.
- hash the node's current `(kind, inputs, output-kinds)` from the store and
  probe the table for a structurally-equal *other* node (the candidate filter
  excludes the node itself);
- twin found → return `Some(twin)` (the caller performs the merge; this method
  touches no edges);
- else re-insert the node under its current hash (it becomes the canonical
  representative) and return `None`.

Reuses the existing `C::hash` / `C::eq` logic; no new hashing policy.

### 5. Delete `DedupNodes` (`strider-opt`)

Remove the pass, its module (`opt/dedup_nodes/`), its pipeline registration
(`lib.rs`), and its re-export. Repurpose its tests (see Testing).

## Soundness

`canonicalize` merges exactly the `(kind, inputs, output-kind)`-equal cacheable
single-output nodes `DedupNodes` did, via the same `replace_value`. Same merges,
different timing.

- A twin shares **all** inputs with the survivor, so merging orphans no input
  subtree and the survivor accumulates the duplicate's uses (never goes dead
  mid-merge).
- The cascade terminates: every merge strictly reduces the live-node count.
- The canonical invariant holds at every `clean()` boundary. Passes observe the
  graph only at `clean()` boundaries (the pipeline calls `clean()` after each
  pass), so "canonical at every `clean()`" is indistinguishable from "always
  canonical" to every consumer — this is the agreed drained-at-`clean()`
  semantics, and it is *more* frequent than the old `DedupNodes`-when-its-turn
  cadence.

## Edge cases

- A queued node already killed by an earlier merge → skipped by `dequeue`'s
  liveness check.
- The twin probe excludes the node itself.
- Non-cacheable kinds (phi/call/store/region/if) → `canonicalize` returns `None`;
  never merged (matches `DedupNodes`).
- A merged `Load`/`Store` twin's `stack_offsets` side-table entry is dropped with
  the dead duplicate; the survivor keeps its (identical) offset — same as
  `DedupNodes` today.
- Mid-pass the cache may transiently hold a twin while the invalidated original
  is queued; `clean()` restores single-key uniqueness at the pass boundary. No
  consumer observes the graph mid-pass.

## Testing (TDD)

- Adapt `DedupNodes`' tests to drive through `clean()` instead of the pass —
  including the motivating case (`PhiCollapse` → two `Truncate(InitialVar)`
  twins → merged → `value_range` carries the guard's bound to the table index).
- New **invariant test**: after a full pipeline run, no two reachable cacheable
  single-output nodes share a structural key.
- New **cascade test**: a twin chain that merges up the consumers in one
  `clean()`.
- Fingerprint superset preserved across a merge (already handled by
  `replace_value`; assert it).
- The full workspace suite (3168 tests) and the optimizer proptest are the
  primary safety net.

## Scope

- `strider-graph`: one new `NodeCache` method.
- `strider-ir`: `NodeFlags::CANONICAL`, enqueue hooks in the four edit verbs +
  `replace_value`, one arm in `clean()`, a `canonicalize_node` helper.
- `strider-opt`: delete `DedupNodes`; repurpose tests.
- No orchestrator / Python API change.

## Non-goals

- No change to creation-time dedup (`get_or_alloc` stays).
- No eager (per-mutation) drain — drained at `clean()` only (agreed option A).
