# Memory-dependence narrowing in the memory-SSA walker

## Problem

The shared backward memory-SSA walk (`opt::memory_ssa::may_clobber`) starts
at a `Load`'s memory input and walks the memory-token chain backward until it
finds the nearest definition that may alias the load's location.  On a chain
with many provably-disjoint intervening definitions
(`stack_store → store_const → store_other → … → load`), every walk re-traverses
the whole disjoint prefix even though none of those defs affect the load.

When the walk proves a def is `Disjoint`, the load does not depend on it.  We
can make that knowledge permanent: repoint the load's memory input past the
disjoint prefix onto the nearest definition it *actually* depends on.  The
chain shrinks for that load and for every future lookup that passes through it.

## Design

After `may_clobber` computes its result node `T` (the nearest dependency),
**repoint the originating `Load`'s memory input (slot 0) to `T`'s memory
output when it differs from the load's current memory producer.**

`T` is, by construction, the nearest thing the load depends on along *all*
paths — the walker's existing join semantics already produce exactly the right
target in every case:

| Chain shape between load and `T`        | `T` (redirect target)            |
| --------------------------------------- | -------------------------------- |
| linear disjoint defs                    | nearest real clobber (skip them) |
| transparent phi (all arms agree)        | the agreed def behind it (jump the phi) |
| disagreeing phi (a clobber on one arm)  | the `MemPhi` itself (stop at it)  |
| fully clean                             | `InitialMemory`                  |

No new traversal logic: the redirect target **is** `may_clobber`'s existing
return value.

### Through phis vs. stopping at phis

* **Transparent phi** (every arm agrees on the same live def, or all clean):
  the load observes the same memory state on every control path, so reading the
  agreed def directly is value-identical to reading the phi.  Go *through* —
  the load jumps past the phi onto the agreed def.
* **Disagreeing phi** (a clobber on at least one arm): there is no single live
  def across the merge, so the `MemPhi` is the boundary.  The load is repointed
  *at* the phi (skipping any disjoint defs between it and the phi) and keeps
  observing the merge.

## Soundness

1. **A `Load` is a pure consumer.**  It has a single value output and no memory
   output, so nothing reads "through" a load.  Moving its single incoming
   memory edge is invisible to every other node.  The `MemPhi`, its arms, and
   all intervening stores stay in place for any other consumer — we never touch
   them, only the load's own edge.  (This is why narrowing is confined to the
   originating load and never path-compresses intermediate stores: a store's
   memory output *is* read by others, and "disjoint" is relative to one
   specific load's address.)

2. **The rewrite is permanent; the verdict is recomputed each iteration.  That
   is safe because alias precision is monotone.**  The oracle's default is the
   worst case (`MayAlias` ⇒ treat as clobber); the only motion is
   `MayAlias → Disjoint` as `StackOffsetDetect` proves more offsets.  A verdict
   never degrades back to `MayAlias`.  So a load narrowed past a (then)
   transparent phi at iteration *N* can only ever be narrowed *further in the
   same direction* later — never invalidated.  No pass inserts a clobbering
   store into an existing chain arm; passes fold / forward / collapse only.

3. **CFG rebuild re-derives fresh.**  The indirect-resolution rebuild discards
   the whole graph, including any narrowed edge, so a rebuilt graph starts from
   the un-narrowed chain.

## Placement & callers

Narrowing lives **inside `may_clobber`** so every caller's loads benefit
without duplicating the repoint.  `may_clobber`'s signature changes from
`&Function` to a mutable graph handle; it still returns `T` unchanged (callers
depend on the return value).

* **`load_forward`** already passes its real `Load` node — gains narrowing for
  free.  (If it subsequently forwards and detaches the load, the prior repoint
  is harmless.)
* **`function_args`** currently passes the *mem producer* (`start`) as the
  `load` arg and walks through an immutable `RewriteCtxView`.  Two mechanical
  changes: pass the real `Load` (`node_id`) as the load arg, and switch to the
  mutable handle.  Its post-pass doc note "does not rewrite the graph" becomes
  "shortens load memory edges" — idempotent and orthogonal to arg detection
  (the `arg_index_to_values` result is unchanged).

## Convergence

* Report `Changed` whenever an edge is moved.
* Idempotent: once a load points at its nearest clobber, the next walk returns
  the same `T` and moves nothing → `NoChange`.
* The fixed-point loop already re-runs to quiescence; narrowing terminates
  because each move strictly shortens a finite chain and precision is monotone.

## Test plan (TDD)

Unit tests against `may_clobber` / the two passes, using the mock-graph
builders:

1. **Linear disjoint prefix** — `load → store(disjoint) → store(disjoint) →
   store(MATCH)`: after the walk, load's memory input points at the matching
   store; the disjoint stores are bypassed.
2. **No-op when already nearest** — load already points at its nearest clobber:
   `NoChange`, edge unchanged.
3. **Transparent phi** — both arms reach the same dominating disjoint chain to a
   clean root: load jumps past the phi to `InitialMemory` (or the agreed def).
4. **Disagreeing phi** — one arm clobbers, the other is clean (or a different
   clobber): load is repointed at the `MemPhi`, never past it; still observes
   the merge.
5. **Disjoint-then-phi** — disjoint defs between the load and a (dis)agreeing
   phi are skipped; load lands on the phi.
6. **Idempotence / convergence** — running the pass twice yields `NoChange` on
   the second run; the forwarded value (in `load_forward`) is unchanged by the
   narrowing.
7. **`function_args` still detects the same args** — narrowing a stack-arg
   load's chain does not change which indices are registered in
   `arg_index_to_values`.
8. **Fingerprint contract** — repointing an edge neither shrinks nor drops any
   node's asm-fingerprint (no node is replaced; only an edge moves).

Each test is written failing-first, then the narrowing is added to make it
pass.
