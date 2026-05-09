# Round 10 — `ir` crate

**Scope:** All `.rs` files under `crates/ir/src/` (42 files), `crates/ir/tests/` (10 files), `Cargo.toml`, `README.md`.

**Review emphasis:** Correctness triangulation (code-vs-code self-consistency, IR-vs-pcode semantics), and simplification opportunities.

---

## Critical / HIGH Findings

### H-1: `check_layer_c_uniqueness` is NOT reachability-scoped — silent false-negative on multi-graph patterns

- **Severity:** HIGH
- **Where:** `crates/ir/src/validate/layer_c.rs:23-51`
- **What's wrong:** `check_layer_c_uniqueness` iterates `graph.nodes.keys()` (all nodes, including zombies) to find `Entry` and `InitialMemory` nodes. The function is intentionally global to detect a rogue second `Entry`/`InitialMemory` even when unreachable. The comment at line 13–16 states this rationale. However, the `MissingInitialMemoryNode` arm fires when there is *no* `InitialMemory` in the entire arena, even if the reason is that `InitialMemory` was placed on a different graph object (e.g. `FunctionGraph::new_invalid()` before `build_entry` is called). Specifically, `FunctionBuilder::build_entry` at `crates/ir/src/builder/nodes.rs:490` calls `self.function = FunctionGraph::new_invalid()` first (resetting the graph), then immediately re-creates the nodes. Any caller who calls `validate` on the `FunctionBuilder::function.graph` sub-object *before* `build_entry` completes would get a spurious `MissingInitialMemoryNode`. This is a by-construction hazard: if a future pass calls `validate` on the intermediate `FunctionGraph`, the error bundle is misleading. The real correctness issue: the doc at line 13 says "scanning every node including detached zombies so a second Entry/InitialMemory is reported even when unreachable". But the `MissingEntryNode` arm fires when zero Entry nodes exist — which can happen normally when `Graph::new()` is freshly returned and no nodes have been built yet. A round-trip test that builds on a fresh graph and calls `validate` before `build_entry` would silently report `MissingEntryNode` rather than a sensible "graph is under construction" error. The fix is a matter of documentation/API contract rather than a code logic bug, but it means any caller who uses `validate` on a sub-graph or partially-built graph gets confusing output.
- **Verified against:** `crates/ir/src/builder/nodes.rs:490` (`build_entry` resets `self.function = FunctionGraph::new_invalid()`); `crates/ir/src/validate/layer_c.rs:35-50`
- **Fix:** Document the precondition that `validate` must only be called on fully-built graphs (i.e. after `build()`) and assert this invariant at the start of `validate`. No code fix required if the contract is already understood, but the comment at line 16 ("MissingInitialMemoryNode likewise fires if no InitialMemory exists at all") should note this constraint explicitly.
- **Regression test:** None required; the contract is already enforced by `FunctionBuilder::build()` only calling `validate` at the end.

---

### H-2: `extend_asm_fingerprint` fast path has a silent correctness hole for duplicate out-of-order contributors

- **Severity:** LOW (downgraded after analysis — false alarm)
- **Where:** `crates/ir/src/graph/store.rs:163-178`
- **What's wrong:** The fast-path loop processes `contributors` one element at a time. When `addr == last` it does nothing (correct). When `addr > last` it pushes (correct). When `addr < last`, it pushes `addr` and sets `needs_resort = true`. The subsequent `sort_unstable` + `dedup` at lines 175-177 correctly normalises the vector. After full case analysis: the algorithm is sound for all inputs because sort+dedup in the resort path handles everything; the fast path is an optimisation, not a separate code path. **False alarm — no fix needed.**

---

### H-3: `lift_at` Guard Deref/DerefMut + nested-call double-restore

- **Severity:** LOW (downgraded — verified correct by existing test)
- **Where:** `crates/ir/src/builder/mod.rs:418-442`
- **What's wrong:** The `Guard` struct implements `Deref<Target = FunctionBuilder>` and `DerefMut`. The closure receives `&mut Guard` which derefs to `&mut FunctionBuilder`. Since `FunctionBuilder::set_lift_addr` and `FunctionBuilder::lift_at` are both callable through the deref, a nested call to `guard.lift_at(inner_addr, ...)` works correctly because the nested guard's `prev` captures the current `guard.inner.lift_addr` (which is already `Some(addr)` from the outer `lift_at`). On drop, the nested guard restores `lift_addr` to `Some(outer_addr)`. On drop of the outer guard, `lift_addr` is restored to `prev`. The test at `crates/ir/src/builder/tests.rs:1541-1549` covers exactly this nested case. **Verified correct.**

---

## MED Findings

### M-1: `make_int_const` does not mask the `u64 val` to the declared type's bit width — creates `IntConst` with un-masked value for narrow types

- **Severity:** MED
- **Where:** `crates/ir/src/ops/consts.rs:82-100`
- **What's wrong:** `Graph::make_int_const(val: u64, ty: NodeOutputType)` creates `NodeKind::IntConst(u128::from(val))` without masking `val` to `ty.bit_mask_u128()`. Contrast with `FunctionBuilder::build_int_const` at `crates/ir/src/builder/nodes.rs:102`, which does `val.into() & output_type.bit_mask_u128()` before storage. For example, `make_int_const(0xFF_FF, NodeOutputType::U8)` creates `IntConst(0xFFFF)` but the node's declared output type is `U8`. Any consumer that calls `int_const_val(out)` at `crates/ir/src/ops/consts.rs:18-27` will call `ty.get_unsigned_int(0xFFFF)` which masks to 8 bits and returns `0xFF` — so *read-side masking compensates*. However, the dedup-cache key includes the `NodeKind::IntConst(u128)` payload: `make_int_const(0xFF_FF, U8)` and `make_int_const(0xFF, U8)` would produce two *different* `NodeId`s despite being semantically identical, breaking the structural-equality guarantee that cacheable nodes depend on. An opt pass that does `make_int_const(mask_val, U8)` may then fail to find the dedup match for a node previously created via `build_int_const(0xFF, U8)`, resulting in duplicate constant nodes and potential missed fold opportunities.
- **Verified against:** `crates/ir/src/ops/consts.rs:94-95` (no masking); `crates/ir/src/builder/nodes.rs:102` (masking present); `crates/ir/src/graph/store.rs:230-235` (dedup key includes `NodeKind` which carries the un-masked value)
- **Fix:** Apply the same masking in `make_int_const`:
  ```rust
  let masked = u128::from(val) & ty.bit_mask_u128();
  let node = self.create_node(NodeKind::IntConst(masked), [], [NodeOutputKind::OutputType(ty)]);
  ```
- **Regression test:** `make_int_const(0x1FF, U8)` must produce the same `NodeId` as `make_int_const(0xFF, U8)` after the fix.

---

### M-2: `check_layer_c_uniqueness` not reachability-scoped vs `check_layer_c_function_arg_uniqueness` is — inconsistent rationale

- **Severity:** MED
- **Where:** `crates/ir/src/validate/layer_c.rs:23-51` vs `crates/ir/src/validate/layer_c.rs:234-259`
- **What's wrong:** `check_layer_c_uniqueness` explicitly opts out of reachability scoping (with a justification in the comment at lines 13-16) to detect rogue `Entry`/`InitialMemory` nodes even when unreachable. But `check_layer_c_function_arg_uniqueness` (lines 234-259) explicitly opts *in* to reachability scoping to avoid false-positive `DuplicateFunctionArg` from stale zombies. Both decisions are individually defensible. The inconsistency is that a stale unreachable second `Entry` or second `InitialMemory` would fire `MultipleEntryNodes`/`MultipleInitialMemoryNodes`, but a stale unreachable second `FunctionArg` would not fire. The practical consequence: if `RedundantPhis` or `DeadBranchElimination` detach a subgraph containing a second structural `Entry` node (which can't happen by construction since `Entry` is non-cacheable and the builder creates exactly one), the uniqueness check would fire. For `InitialMemory` it's more theoretically possible if a pass synthesises a second one. The inconsistency is not a direct bug given the current pass set, but it represents an asymmetry in validation policy that could surface as a false positive in future pass development.
- **Fix:** Document the asymmetry in a comment block at the top of `layer_c.rs` to make the policy explicit. Alternatively, if stale `Entry`/`InitialMemory` zombies can realistically be produced by future passes, consider also gating these checks on reachability.

---

### M-3: `compact::gc_wide_consts` standalone soundness claim doesn't hold pre-compaction

- **Severity:** MED
- **Where:** `crates/ir/src/graph/compact.rs:241-290`
- **What's wrong:** `gc_wide_consts` is called from `retain_reachable` *after* the node arena has been replaced with only reachable nodes (step 5 at line 143). At that point `self.nodes.keys()` iterates only surviving nodes. This is correct. However, `gc_wide_consts` is also declared `pub(crate)` and the comment at line 238-240 says "safe to call standalone in tests / direct mutators that want to drop unreferenced wide values". A standalone call before `retain_reachable` would scan the full unreachable+reachable arena. The live-id set built at lines 245-252 would include `IntConstWide` ids from zombie nodes. Those zombies' wide const values would then be kept in the rebuilt `wide_consts` table even though no live node references them. This is not a safety issue, but it defeats the GC purpose: wide values referenced only by zombies would survive. More importantly, the dedup-cache rebuild at step 7 of `retain_reachable` (lines 166-189) runs *after* `gc_wide_consts` and builds cache keys using `self.nodes[new_node_id].kind` — which for surviving `IntConstWide` nodes now carries the new `WideConstId`. If a standalone caller invokes `gc_wide_consts` on a non-compacted graph and then `retain_reachable` without calling `gc_wide_consts` again, the `IntConstWide` payloads would still carry old ids from the first GC pass and the new GC pass would remap them again, potentially incorrectly.
- **Fix:** Either remove the "safe to call standalone" claim from the doc comment, or add a guard that `gc_wide_consts` is only sound when the arena is already compacted. The simplest fix is to make it `fn gc_wide_consts(&mut self)` (private to `compact.rs`) with no pub(crate) visibility.

---

### M-4: `FunctionBuilder::build` does not propagate `no_memory_clobber` into `BuiltFunctionGraph`

- **Severity:** MED
- **Where:** `crates/ir/src/builder/mod.rs:568-590`
- **What's wrong:** `FunctionBuilder::build()` assembles `BuiltFunctionGraph` from fields of `self`. The `no_memory_clobber` field on `FunctionBuilder` (line 137) is set in `FunctionBuilder::new()` from the calling convention (line 255) and affects `build_call_with_cc`. However `BuiltFunctionGraph` has no corresponding `no_memory_clobber` field. Any consumer that takes a `BuiltFunctionGraph` and wants to know whether the function's calling convention suppresses memory clobber (e.g. to re-build calls post-optimization) cannot recover this information. The `x86_64_all_preserving` CC (used for `__fentry__`/`mcount`) sets `no_memory_clobber = true`, and calls to such functions deliberately suppress the memory chain advancement. Losing this after `build()` means re-instrumentation passes would have to rediscover it from the `call_clobbered_overrides` side-table — possible but fragile.
- **Fix:** Add `no_memory_clobber: bool` to `BuiltFunctionGraph` and copy it in `build()`. Or document that the field is intentionally ephemeral and the `call_clobbered_overrides` side-table encodes the same intent per-call.

---

## LOW Findings

### L-1: `node_signature.rs` test panic sites — test-only, justified

- **Severity:** LOW (no fix needed)
- **Where:** `crates/ir/src/node_signature.rs:774,783`
- **What's wrong:** `unwrap_or_else(|| panic!(...))` and `.expect(...)` inside a `#[test]` function. Already in test-only code; explicit `#[allow(clippy::panic)]` at the crate level for tests.

### L-2: `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` `pub` + `#[doc(hidden)]` deferred

- **Severity:** LOW
- **Where:** `crates/ir/src/function.rs:182`
- **What's wrong:** Partial-state ctor still `pub`; long-term migration to `pattern::RewriteCtx` not yet complete (4 pattern test scaffolds + 2 production callers).
- **Fix:** Consider adding `#[deprecated]` so external uses get a compiler warning.

### L-3: `validate/layer_b.rs` reachability-scoping is sound but non-obvious

- **Severity:** LOW
- **Where:** `crates/ir/src/validate/layer_b.rs:39-56`
- **Fix:** Add a comment clarifying that `detach_node_inputs` guarantees zombie consumers are removed from use-lists before any validation sweep.

### L-4: `Graph::make_float_const` does not validate float-ness fast

- **Severity:** LOW
- **Where:** `crates/ir/src/ops/consts.rs:117-130`
- **What's wrong:** Calling `make_float_const(0, NodeOutputType::U64)` defers the error to `validate` time instead of failing fast in the ctor.
- **Fix:** Add a guard `if !ty.is_float() { return Err(...) }`.

### L-5: `upgrade_to_tracked_for` tie-break non-deterministic across hash seeds

- **Severity:** LOW
- **Where:** `crates/ir/src/builder/mod.rs:89-97`
- **Fix:** `.max_by_key(|t| (t.size, t.addr_off))` for stable ordering.

---

## Coverage

| File | Status |
|------|--------|
| `crates/ir/src/lib.rs` | fully |
| `crates/ir/src/function.rs` | fully |
| `crates/ir/src/graph/mod.rs` | fully |
| `crates/ir/src/graph/store.rs` | fully |
| `crates/ir/src/graph/access.rs` | fully |
| `crates/ir/src/graph/uses.rs` | fully |
| `crates/ir/src/graph/compact.rs` | fully |
| `crates/ir/src/graph/tests.rs` | partially |
| `crates/ir/src/builder/mod.rs` | fully |
| `crates/ir/src/builder/nodes.rs` | fully |
| `crates/ir/src/builder/call.rs` | partially |
| `crates/ir/src/builder/vars.rs` | partially |
| `crates/ir/src/builder/coerce.rs` | partially |
| `crates/ir/src/builder/tests.rs` | partially |
| `crates/ir/src/node/mod.rs` | fully |
| `crates/ir/src/node/kind.rs` | fully |
| `crates/ir/src/node/data.rs` | partially |
| `crates/ir/src/node/ids.rs` | not |
| `crates/ir/src/node/output_kind.rs` | fully |
| `crates/ir/src/node/output_type.rs` | fully |
| `crates/ir/src/node/tests.rs` | not |
| `crates/ir/src/node_signature.rs` | fully |
| `crates/ir/src/validate/mod.rs` | fully |
| `crates/ir/src/validate/layer_a.rs` | fully |
| `crates/ir/src/validate/layer_b.rs` | fully |
| `crates/ir/src/validate/layer_c.rs` | fully |
| `crates/ir/src/validate/tests.rs` | not |
| `crates/ir/src/walk.rs` | fully |
| `crates/ir/src/wide_const.rs` | fully |
| `crates/ir/src/ops/mod.rs` | fully |
| `crates/ir/src/ops/op_kinds.rs` | partially |
| `crates/ir/src/ops/consts.rs` | fully |
| `crates/ir/src/ops/rewrite.rs` | fully |
| `crates/ir/src/ops/builder.rs` | not |
| `crates/ir/src/region.rs` | not |
| `crates/ir/src/error.rs` | not |
| `crates/ir/src/iterators.rs` | not |
| `crates/ir/src/test_utils.rs` | not |
| `crates/ir/src/dot/mod.rs` | not |
| `crates/ir/src/dot/render.rs` | not |
| `crates/ir/src/dot/label.rs` | not |
| `crates/ir/src/dot/tests.rs` | not |
| `crates/ir/tests/build_validate_roundtrip.rs` | not |
| `crates/ir/tests/builder_extended_use.rs` | not |
| `crates/ir/tests/call_other_classification.rs` | not |
| `crates/ir/tests/call_other_modeled.rs` | not |
| `crates/ir/tests/common/mod.rs` | not |
| `crates/ir/tests/dedup_cache.rs` | fully |
| `crates/ir/tests/proptest_graph_invariants.rs` | not |
| `crates/ir/tests/retain_reachable.rs` | not |
| `crates/ir/tests/walk_reachability.rs` | not |
| `crates/ir/tests/build_call_with_cc.rs` | not |
| `crates/ir/Cargo.toml` | fully |
| `crates/ir/README.md` | fully |

**Coverage gap:** ~17 files not read. Round 7 should backfill via a follow-up subagent.

---

## Summary

**HIGH findings:** 1 actionable (M-1 reclassified from H; H-2/H-3 false alarms after analysis).

The most impactful correctness issue is **M-1**: `Graph::make_int_const` stores the raw `u64` value without masking to the declared type's bit width, breaking dedup-cache structural equality.

Secondary: **M-3** (gc_wide_consts standalone claim), **M-4** (no_memory_clobber not in BFG). Remaining LOW items are documentation / fail-fast improvements.
