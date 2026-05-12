# Cat. 2 — `entity_utils::Memo<K, V>` newtype: skipped

Source: `reviews/round7-generalize.md` §2 (Memo / fixed-point caches).

## Decision

**Skip.**  The three call sites the audit identifies do not share enough
shape to factor through one ergonomic helper without re-introducing all
the bespoke logic at the call site as configuration.

## Call sites inspected

1. **`crates/opt/src/sp_expr.rs::decompose_sp`** (canonical example).
   - Cache: `FxHashMap<NodeOutputId, Option<SpExpr>>`.
   - Cycle set: `FxHashSet<NodeId>`.
   - Cache key (`NodeOutputId`) and cycle key (`NodeId`) are **different
     types** — the memo lookup is by output, the cycle gate is by node.
   - Caches `Some(_)` results unconditionally; **never caches `None`**
     because a `None` could be either "genuinely not SP-rooted" (safe to
     recompute) or "cycle-truncated on this call path" (must NOT be
     cached, because a different call path may resolve it).  This
     asymmetry is load-bearing for soundness — see the `decompose_sp`
     doc-comment and the `decompose_sp_does_not_cache_none_results` test.

2. **`crates/opt/src/function_args/mod.rs::mem_chain_is_dirty`**.
   - Cache: `FxHashMap<(NodeOutputId, i64, i64), bool>` (composite key).
   - Cycle set: `FxHashSet<NodeOutputId>`.
   - Caches **only at the outermost recursion frame** (`is_outermost =
     seen.is_empty()` on entry).  Sub-call results are not written back
     because they reflect cycle handling relative to the parent's
     already-explored set.  Cycle returns `false` (clean), no caching.

3. **`crates/opt/src/stack_load_forward/mod.rs::find_stack_stored_value_at_offset`**.
   - Cache: `FxHashMap<(NodeOutputId, i64, NodeOutputType), Option<NodeOutputId>>`.
   - **No cycle set at all** — the helper bails on `MemPhi`
     unconditionally.
   - Caches every result unconditionally.

(The `probe` walker in the same `stack_load_forward` module is
iterative and uses an `FxHashSet<NodeOutputId>` only as a `MemPhi`
cycle guard with no value memo at all — a fourth distinct shape.)

## Why no single newtype fits

The audit's own write-up flags the risk: *"a naïve
`Memo::get_or_compute` would silently break the cycle case"*.  Concretely,
a `Memo<K, V>` that wrapped `FxHashMap<K, V>` would have to be
parameterised over **all** of:

- cache key type vs cycle key type (different in site 1);
- cache predicate (always vs `Some(_)`-only vs outermost-only);
- cycle return value (`None` / `false` / N/A);
- whether a cycle set exists at all (site 3 has none).

By the time the helper exposes those four knobs, the call site is no
shorter than the inline form, every soundness rule has migrated into a
configuration argument, and the type signature has become a small DSL.
The audit also proposed a `DfsCtx<K, V>` alternative bundling the
visited set with the memo, but that does not fix the cycle-key /
cache-key type mismatch in `decompose_sp` (the canonical case the audit
nominates) and would still need the per-site soundness predicate.

## Conclusion

Each site's cycle / cache rules are a load-bearing local invariant that
the `decompose_sp` doc-comment, the `mem_chain_is_dirty` outermost-only
comment, and the `find_stack_stored_value_at_offset` module preamble
already explain at the point of use.  Folding them into a shared
abstraction would either erase those invariants or migrate them into
constructor flags — both options are worse than the current inline
form.  The CLAUDE.md guidance ("better to add a focused helper for one
pattern than a generic abstraction that's worse than the inline form")
applies straightforwardly: this generalization is correctly classified
as *useful but not load-bearing* in the audit, and the shape mismatch
across call sites tips the balance toward leaving the inline forms
alone.
