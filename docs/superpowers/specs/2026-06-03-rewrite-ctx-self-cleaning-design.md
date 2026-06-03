# Self-Cleaning `RewriteCtx` + Shared `OptCtx` Design

**Goal:** Consolidate all IR-rewrite machinery into `strider-opt`, and give it a
`RewriteCtx` whose `FunctionState` automatically performs the recursive
dead-node cleanup that every pass currently does by hand — while keeping
asm-fingerprints correct. Make `OptCtx` the single home for cross-optimization
config and shared analysis caches (e.g. `AliasMode` becomes global).

**Architecture (3 parts):** (1) relocate `rewrite` from `strider-pattern` into
`strider-opt`; (2) a self-cleaning `RewriteCtx` backed by an incrementally
maintained `FunctionState` (live-node set + roots + kill-queue); (3) `OptCtx`
holding shared config + caches, threaded `&mut` into every pass.

**Tech stack:** Rust workspace; `strider-ir` (sea-of-nodes IR), `strider-pattern`
(matching/templating), `strider-opt` (passes + pipeline), `entity-utils`
(`DenseEntitySet`, `Worklist`). The reference design comes from a friend's
multi-function IR (spidir-like); we adopt its `FunctionState`/edit-context shape
minus its tracing, `NodeCache` hash-consing (strider's `Graph` already dedups
cacheable nodes), and `CANONICAL` re-canonicalization (strider's fixed-point
pipeline already re-runs rules).

---

## Part 1 — Relocate `rewrite` into `strider-opt`

`crates/strider-pattern/src/rewrite.rs` moves to `crates/strider-opt/src/rewrite/`
(a module directory), **merging** the existing
`crates/strider-opt/src/rewrite_ext.rs` into it — its `replace_value`,
`redirect_input`, and `remove_region_predecessors` become first-class
`RewriteCtx` methods rather than an extension trait.

Rewrite depends one-way on matching/templating, so the split is clean: it *uses*
`strider_pattern::{Matcher, Pattern, Template, instantiate, MatchPat,
TemplatePat, Capture}` and `error::is_skip`. None of those depend on rewrite.

**strider-pattern after:** drop `rewrite.rs`; remove the 9 `lib.rs` re-exports
(`RewriteCtx`, `RewriteCtxView`, `GraphRewriter`, `rewrite_rule`,
`rewrite_rule_runtime`, `apply_rules_in_order`, `boxed_rule`, `BoxedRule`,
`GraphRewriteCtxExt`). Move the rewrite tests out (see Part 4).

**Consumers re-point:**
- strider-opt passes: `strider_pattern::RewriteCtx` → `crate::RewriteCtx`.
  The `Optimizer` trait (already in `strider-opt/src/pipeline.rs`) references
  `crate::RewriteCtx` instead of `strider_pattern::RewriteCtx`.
- strider-orchestrator (`orchestrator/mod.rs`): `strider_pattern::RewriteCtxView`
  + `GraphRewriteCtxExt` → `strider_opt::…`.
- strider-py (`function.rs`): `strider_pattern::{rewrite_rule_runtime,
  GraphRewriter}` → `strider_opt::…`.

This part is a pure move + import rewrite: **no behavior change**, gate stays
green.

---

## Part 2 — `FunctionState` + self-cleaning `RewriteCtx`

### State

```rust
struct FunctionState {
    live_nodes: DenseEntitySet<NodeId>,   // logically-live set (arena culling happens at compact)
    roots: Vec<NodeId>,                   // live input-less nodes (Entry / consts / InitialVar / InitialMemory)
    queue: Worklist<NodeId>,              // maybe-dead producers awaiting the drain
    flags: SecondaryMap<NodeId, NodeFlags>, // ENQUEUED | OUTPUT_KILLED
}

struct RewriteCtx<'g> {                    // concrete struct; NO traits/bounds — all edits go through it
    function: &'g mut Function,
    state:    &'g mut FunctionState,
}
```

`NodeFlags` keeps only `ENQUEUED` and `OUTPUT_KILLED` (the reference's `CANONICAL`
is dropped — no re-canonicalization here).

### Invariants, maintained incrementally by every edit

Because **all** function edits go through `RewriteCtx`, it keeps `live_nodes`
and `roots` exact without re-walking:

- `create_node` / `create_node_attributed`: `live_nodes.insert(node)`; if the
  node has no inputs, `roots.push(node)`.
- `add_node_input` to a node that is currently input-less: drop it from `roots`
  (it now has an input). (Handles `Region`/`Phi`/`Return` built incrementally —
  momentarily input-less at creation, then gaining inputs.)
- `kill_node`: remove from `live_nodes` and `roots`.

`populate(function)` seeds both once via `GraphWalkInfo::compute_full` (live set
+ roots), then `kill_node`s every reachable-but-dead node so use counts start
correct — the reference's initial raw-postorder cull, minus enqueuing live nodes.

### Edit API (mirrors the reference, fingerprint-aware)

All take `&mut self`; none expose `DerefMut` to `Function` (so there is no raw
`set_asm_fingerprint` escape — fingerprints only grow).

- `create_node(kind, inputs, outputs) -> NodeId`, `create_node_attributed(…,
  contributors)` — the latter unions contributor fingerprints (superset-only).
- `replace_value(old, new) -> bool` — absorb `old`'s producer fingerprint into
  `new`'s producer, redirect every use `old → new`, then `will_detach_value(old)`
  enqueues `old`'s producer as maybe-dead.
- `replace_value_and_kill(old, new)` — `kill_node(old.producer)` then redirect.
- `set_node_input(node, idx, new)` / `update_input(use_id, new)` —
  `will_detach_value(displaced)`, then rewire.
- `remove_node_input(node, idx)` — `will_detach_value(displaced)`, then remove.
- `kill_node(node)` — for each input, `will_detach_value`; `detach_node_inputs`;
  mark dead (`live_nodes`/`roots` removal).
- `remove_region_predecessors(region, idxs)` — structural `Region`/`Phi` slot
  surgery built on `remove_node_input` (so it auto-enqueues).

`will_detach_value(value)`: if `value` loses its last use **and** its producer
`!node_kind.has_side_effects()`, enqueue the producer `OUTPUT_KILLED`.

### Drain (`clean`)

```
while let Some(node) = queue.dequeue() {
    if flags.take(OUTPUT_KILLED) && is_node_dead(node) {  // dead = no live uses on any output && !has_side_effects
        kill_node(node);   // kill_node's will_detach_value enqueues node's now-orphaned operands → recursion
    }
}
```

This is the reference's `canonicalize_outstanding` minus the `canonicalize_node`
call. **Drain runs after every pass that reported `Changed`** (and once after
`populate`), per the chosen timing. `sp_memo` is cleared at each drain point
(see Part 3).

### Cheap walks + order-agnostic iteration (from the cached state)

- `reverse_postorder()` / `postorder()` — the forward def→use post-order run
  **straight from the cached `roots`, restricted to cached `live_nodes`** (one
  pass; skips `GraphWalkInfo::compute_full`). Used by the order-sensitive
  consumers: the peephole seed and the `GraphRewriter` candidate walk.
- `live_of_kind(pred) -> impl Iterator<NodeId>` — filters `live_nodes` directly,
  **no traversal**. The order-agnostic passes switch to this: `function_args`
  (over `InitialVar`), `load_readonly` (`Load`), `stack_offset` (`Store`/`Load`),
  `cfg_detach` (`Region`).

`decompose_sp` is unaffected — it keeps `Graph::reverse_postorder(producer(value))`,
a *value-cone* walk seeded at a node, unrelated to the function-global cache.

### `has_side_effects` gate

Add `NodeKind::has_side_effects(&self) -> bool`, defined (for now) as
`self.has_control_flow()`. Side-effecting nodes (Call, CallOther, Store, Return,
If, Region, …) are never auto-killed even at zero uses. This gates both
`will_detach_value` and `is_node_dead`.

### Fingerprint correctness

`replace_value` absorbs `old`'s producer fingerprint into `new`'s producer
*before* redirecting, so the live result keeps the killed node's contributions;
killing an already-dead node then loses nothing. `rewrite_rule_impl`'s
`absorb_fingerprints_into_fresh_subtree` (interior fresh-node attribution) is
unchanged.

### Passes stop doing it by hand

`dead_branch`, `phi_collapse`, `region_collapse`, `load_forward`, `cfg_detach`,
and `indirect_branch_resolve` (`inplace.rs` `detach_placeholder`) drop their
manual `detach_node_inputs` / use-count / dead-node code and rely on the
auto-cleanup. Behavioral output is unchanged; their existing tests stay green.

### Pipeline integration

`OptimizerPipeline::run` owns the `FunctionState`: `populate` before the
fixed-point loop, then per iteration run each pass via
`opt.apply(&mut rctx, &mut octx)` and `rctx.clean()` after any pass that returned
`Changed`; post-passes the same. The final `Function::compact` (existing) removes
the now-dead nodes from the arena.

---

## Part 3 — `OptCtx` as shared config + caches

```rust
struct OptCtx<'mem> {
    rom:               Option<&'mem dyn ReadOnlyMemory>,  // existing
    alias_mode:        AliasMode,                          // now GLOBAL (was per-pass)
    call_clobbers_args: bool,                              // was on FunctionArgDetect
    sp_memo:           SpExprMemo,                         // shared decompose_sp memo (was per-pass, rebuilt)
    arg_layout:        PositionalArgLayout,                // prebuilt from the function's CC
}

trait Optimizer {
    fn apply(&self, rctx: &mut RewriteCtx<'_>, octx: &mut OptCtx<'_>) -> Result<OptimizationResult>;
}
```

`OptCtx` is threaded `&mut` (the `sp_memo` cache is mutable).

- **`AliasMode` global:** `LoadForward`, `FunctionArgDetect`, `CallStackArgCollect`
  drop their `alias_mode` field + chainable setter and read `octx.alias_mode`.
  The orchestrator sets `octx.alias_mode` once (from the `LiftDriver`'s mode)
  instead of `.alias_mode(mode)` on each pass at pipeline-build time.
- **`call_clobbers_args`:** moves off `FunctionArgDetect` onto `OptCtx`.
- **`sp_memo`:** the `decompose_sp` memo (`FxHashMap<ValueId, Option<SpExpr>>`),
  today rebuilt per pass per run, becomes one shared cache. **Cleared at each
  drain point** (whenever the graph changed) — full within-pass sharing, zero
  cross-pass staleness risk (the only staleness vector is an in-place edit to an
  SP-bearing node's inputs; clearing on change makes it moot). *Revisitable later
  if profiling wants longer-lived caching.*
- **`arg_layout`:** `PositionalArgLayout::from_convention(function.default_cc())`,
  prebuilt once on `OptCtx` rather than per `FunctionArgDetect` run.

Orchestrator (`ctx_from_rom` → a fuller `OptCtx` builder) constructs `OptCtx`
from the per-run ROM + the driver's `alias_mode` + the function's CC.

---

## Part 4 — Testing (TDD throughout)

New `FunctionState` unit tests, written test-first (red → green), live in
`strider-opt`:

- **Recursive kill:** kill a node whose sole operand then has no other use →
  the operand (no side effects) is recursively culled; a shared operand with
  another live use survives.
- **Side-effect gate:** a zero-use `Store` (or other `has_control_flow` node)
  is NOT auto-killed.
- **Fingerprint preservation:** `replace_value(old, new)` leaves `new`'s
  producer carrying `old`'s fingerprint (superset), and a subtree replacement
  keeps interior contributors.
- **`populate` culls pre-existing dead nodes** and seeds `roots`/`live_nodes`
  correctly (input-less nodes in `roots`; unreachable nodes excluded).
- **Roots maintenance:** `create_node` of an input-less node adds a root; the
  first `add_node_input` removes it; `kill_node` removes it.

Relocated rewrite tests (`rewrite_build.rs`, `pattern_matching/rewrite.rs`) move
to strider-opt and stay green. Each migrated pass keeps its existing behavioral
tests green (output unchanged). Full gate before merge: `cargo test --workspace`
(currently 3075) + `cargo clippy --workspace` (0 warnings) + `pytest` (841).

---

## Part 5 — Build order (one branch, staged commits, gated each)

1. `NodeKind::has_side_effects()` = `has_control_flow()`.
2. Relocate `rewrite` → `strider-opt` (mechanical move + import rewrite + tests
   move; no behavior change).
3. `FunctionState` + self-cleaning edit API (TDD) wired into the pipeline
   (`populate` + per-changed-pass `clean`); roots/live maintained incrementally;
   cheap `reverse_postorder`/`live_of_kind`.
4. Migrate passes off by-hand cleanup; switch order-agnostic passes to
   `live_of_kind`.
5. `OptCtx` config + caches (`AliasMode` global, `call_clobbers_args`, `sp_memo`,
   `arg_layout`); `&mut OptCtx`; orchestrator wiring.

---

## Error handling

Per project convention: panics/`unwrap`/`expect` are allowed for structural
invariants the validator guarantees and the user cannot violate (e.g. "a
`RewriteCtx` wraps a built function with an entry node", "a producing node index
resolves to a node vertex"). Genuinely-fallible operations (pattern match
buildability, ROM reads) return `anyhow::Result`. The drain and edit ops operate
on validator-guaranteed structure, so they may panic on a broken invariant
rather than thread `Result` everywhere.

## Deferred / explicitly out of scope

- Longer-lived `sp_memo` caching across drains (we clear on change for now).
- A richer `has_side_effects` than `has_control_flow` (revisit when a non-control
  side-effecting node appears that should block culling).
- Multi-function `ModuleState` (the reference's `ModuleState`/`func_states`) —
  strider is single-function per analysis; not needed.
