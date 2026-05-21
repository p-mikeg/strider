# Round 12 — 1A: ir crate audit

## Coverage

| Path | Inspected fully? | Notes |
|------|------------------|-------|
| crates/ir/Cargo.toml | yes | |
| crates/ir/README.md | yes | |
| crates/ir/src/lib.rs | yes | |
| crates/ir/src/error.rs | yes | |
| crates/ir/src/function.rs | yes | |
| crates/ir/src/region.rs | yes | |
| crates/ir/src/iterators.rs | yes | |
| crates/ir/src/walk.rs | yes | tests skimmed |
| crates/ir/src/wide_const.rs | yes | |
| crates/ir/src/node_signature.rs | yes | |
| crates/ir/src/test_utils.rs | yes | |
| crates/ir/src/builder/mod.rs | yes | |
| crates/ir/src/builder/nodes.rs | yes | |
| crates/ir/src/builder/call.rs | yes | |
| crates/ir/src/builder/coerce.rs | yes | |
| crates/ir/src/builder/vars.rs | yes | |
| crates/ir/src/builder/tests.rs | partial | spot-read fingerprint/lift_at tests |
| crates/ir/src/graph/mod.rs | yes | |
| crates/ir/src/graph/store.rs | yes | |
| crates/ir/src/graph/access.rs | yes | |
| crates/ir/src/graph/uses.rs | yes | |
| crates/ir/src/graph/compact.rs | yes | |
| crates/ir/src/graph/tests.rs | partial | spot-read |
| crates/ir/src/node/mod.rs | yes | |
| crates/ir/src/node/data.rs | yes | |
| crates/ir/src/node/ids.rs | yes | |
| crates/ir/src/node/kind.rs | yes | |
| crates/ir/src/node/output_kind.rs | yes | |
| crates/ir/src/node/output_type.rs | yes | |
| crates/ir/src/node/tests.rs | partial | |
| crates/ir/src/ops/mod.rs | yes | |
| crates/ir/src/ops/op_kinds.rs | yes | |
| crates/ir/src/ops/builder.rs | yes | |
| crates/ir/src/ops/consts.rs | yes | |
| crates/ir/src/ops/rewrite.rs | yes | |
| crates/ir/src/validate/mod.rs | yes | |
| crates/ir/src/validate/layer_a.rs | yes | |
| crates/ir/src/validate/layer_b.rs | yes | |
| crates/ir/src/validate/layer_c.rs | yes | |
| crates/ir/src/validate/tests.rs | partial | scanned for coverage gaps |
| crates/ir/src/dot/mod.rs | yes | |
| crates/ir/src/dot/label.rs | yes | |
| crates/ir/src/dot/render.rs | yes | |
| crates/ir/src/dot/tests.rs | partial | sampled |
| crates/ir/src/builder/tests.rs | partial | fingerprint/lift_at section read |
| crates/ir/tests/asm_fingerprint_dedup_union.rs | yes | |
| crates/ir/tests/build_call_with_cc.rs | partial | |
| crates/ir/tests/build_validate_roundtrip.rs | partial | |
| crates/ir/tests/builder_extended_use.rs | skipped | size check only |
| crates/ir/tests/call_other_classification.rs | partial | |
| crates/ir/tests/call_other_modeled.rs | partial | |
| crates/ir/tests/common/mod.rs | yes | |
| crates/ir/tests/dedup_cache.rs | yes | |
| crates/ir/tests/int_const_dedup.rs | yes | |
| crates/ir/tests/proptest_graph_invariants.rs | yes | |
| crates/ir/tests/retain_reachable.rs | yes | |
| crates/ir/tests/walk_reachability.rs | partial | |
| crates/ir/benches/validate.rs | partial | header only |
| crates/ir/examples/graph_creator.rs | partial | header only |

## Findings

No HIGH-severity findings.  3 MED findings.  2 LOW findings.  Crate is in good shape post-W4/W14 encapsulation; production paths are well-defended.

### `Graph::create_node` accepts unmasked / out-of-range `IntConst` payloads, breaking the dedup-cache invariant
- **Severity:** MED
- **Where:** crates/ir/src/graph/store.rs:224-299 (`create_node`); contrast crates/ir/src/ops/consts.rs:67-94 (`make_int_const` which masks).
- **What's wrong:** `Graph::make_int_const(val, ty)` masks `val` to `ty.bit_mask_u128()` *before* feeding the dedup cache (consts.rs:87), and a regression test pins the contract (tests/int_const_dedup.rs:17-34: `make_int_const(0x1FF, U8) == make_int_const(0xFF, U8)`).  The lower-level `Graph::create_node(NodeKind::IntConst(0x1FF), [], [OutputType(U8)])` does *not* mask, so a caller using `create_node` directly can plant a `IntConst(0x1FF, U8)` node distinct from `IntConst(0xFF, U8)`.  Validator Layer A only checks the output's `ExpectedOutputKind` (AnyInt → passes), so the malformed payload survives validation.  Symmetric issue with `U256`/`U512`: `make_int_const` rejects wide types (consts.rs:77-82) but `create_node(IntConst(0), U256)` succeeds, semantically wrong because the value should be stored via `IntConstWide` + `wide_consts` (CLAUDE.md explicitly says "`IntConst(u128)` rejects them").  No production caller in the audit was found doing this — every internal lift path goes through `make_int_const` / `build_int_const` — but the API surface invites the bug.
- **Verified against:** crates/ir/src/ops/consts.rs:67-94 (the masking source-of-truth) and crates/ir/tests/int_const_dedup.rs.  Production callers verified via `grep "NodeKind::IntConst("` across the workspace: all hits in `opt/`/`pattern/`/`strider/` use the constructor for *reading* (`if let NodeKind::IntConst(v) = ...`) rather than constructing; the only constructive hits in production code are `opt::indirect_branch_resolve::inplace.rs:154,296` and `opt::indirect_branch_resolve::stack_array.rs:764`, all of which carry already-masked values by construction (so no current bug — only future regression risk).
- **Fix:** Move the masking logic from `make_int_const` into `Graph::create_node`'s cacheable path — when `kind == NodeKind::IntConst(v)` and the requested output is `OutputType(ty)` with `ty.is_integer()`, replace the node's kind with `IntConst(v & ty.bit_mask_u128())` before forming the cache key.  Reject `IntConst(_)` with `U256`/`U512` output the same way `make_int_const` does (return a fresh node-less error path or `debug_assert!`).  Alternatively, demote `Graph::create_node` to `pub(crate)` and force all external builders through `make_int_const` / typed `build_*` helpers — but that's a larger surface change.
- **Regression test:** Add `Graph::create_node(IntConst(0x1FF), [], [U8])` followed by `Graph::create_node(IntConst(0xFF), [], [U8])` and assert the two return the same `NodeId`.  Mirror with `IntConst` × `U256` and assert it errors (or that the result's kind is rewritten to `IntConstWide`).

### `BuiltFunctionGraph::compact()` can drop a reachable `InitialMemory` and produce a graph that fails `validate`
- **Severity:** MED
- **Where:** crates/ir/src/graph/compact.rs:67-231 (`retain_reachable`), crates/ir/src/function.rs:273-285 (`compact`), crates/ir/src/validate/layer_c.rs:23-52 (`check_layer_c_uniqueness`).
- **What's wrong:** `walk_graph` (the basis of `retain_reachable`) follows control-out + data-in.  `InitialMemory`'s `Memory` output is reached only via data-in from a consumer (Load/Store/MemPhi/CallOther/etc).  If the lifted function has no memory consumer, walking from the entry never visits `InitialMemory`, and `retain_reachable` drops it.  The next `validate` would then surface `MissingInitialMemoryNode` (Layer C uniqueness scans the whole arena and requires exactly one).  In production today, `strider::orchestrator.rs:497` and `strider_py::run.rs:282` both call `compact()` but the lifter wires `InitialMemory` into the per-region `MemPhi.inputs[1]` (builder/vars.rs:114) and every Return/Call carries a `memory` input, so the chain is always live — the bug is latent.  But it surfaces immediately on a contrived `compact_remaps_entry_and_drops_zombies`-style test (function.rs:313-333) that builds an Entry-only graph.
- **Verified against:** crates/ir/src/walk.rs (the walk semantics) and the compaction tests that, notably, never re-validate after compaction.
- **Fix:** Either (a) seed `retain_reachable`'s reachable set with the function's `InitialMemory` node id alongside `entry` (the builder owns the id via `FunctionBuilder.function.entry_memory.source_id`); or (b) loosen Layer C uniqueness to only fail if InitialMemory exists more than once but not at all when the graph has no memory consumers; or (c) document `compact` as "do not validate post-compact for memory-free graphs" and stop there.  Option (a) is the cleanest.
- **Regression test:** Build `FunctionBuilder::empty()` → region → `set_entry_region` → `set_region` → `build_return(None, &[])` → `build()` → `compact()` → `validate(&bfg.graph, bfg.entry)` and assert `Ok(())`.  Pre-fix, this fires `MissingInitialMemoryNode`.

### Public `pub fn add_node_input` / `remove_node_input` lets callers desynchronise a phi from its owning ControlState without validator coverage
- **Severity:** MED
- **Where:** crates/ir/src/graph/uses.rs:85-142 (`add_node_input`, `remove_node_input`), crates/ir/src/validate/layer_c.rs:108-178 (`check_layer_c_phis`).
- **What's wrong:** `add_node_input` and `remove_node_input` are `pub`, so external opt passes can mutate any non-cacheable node's inputs.  Layer C's phi check (layer_c.rs:167-176) flags `PhiValueArityMismatch` when a `VarPhi`/`MemPhi`'s value-input count diverges from its owning `ControlState`'s predecessor count.  But the validator runs only at `FunctionBuilder::build()` time (function.rs:583) — opt passes operating directly on a `Graph` (not a `FunctionBuilder`) can call these APIs and then never re-validate.  The orchestrator runs `default_pipeline().run(&mut graph, entry)` which calls `validate` at the end (per docs), but `set_node_kind` on `IndirectBranch → Return` adjusts arity via `add_node_input`/`remove_node_input` (cited in store.rs:62-65); a buggy resolver could land an arity-mismatched node and the orchestrator's final validate would catch it.  The contract is enforced — but a debug-mode `debug_assert!` matching the new arity to the controlling ControlState (when the mutated node is a phi) would catch the mistake at the mutation site rather than at the next `OptimizerPipeline::run` exit.
- **Verified against:** opt/src/redundant_phis/mod.rs and opt/src/indirect_branch_resolve/inplace.rs which both call `add_node_input`/`remove_node_input` on phis/IndirectBranch.  Validator coverage exists (layer_c.rs:167-176) but only at the pipeline boundary.
- **Fix:** Add a `debug_assert!` inside `add_node_input` / `remove_node_input` for the phi-arity case: when `self.node_kind(node_id)` is `VarPhi`/`MemPhi`, the new value-input count must equal the owning `ControlState`'s predecessor count *after* the mutation.  Locate the owner via input[0] (`PhiToken`'s producer).  Production-grade enforcement via `Result<()>` already exists at the API boundary; the missing piece is a fail-fast check during mid-pass mutation.
- **Regression test:** A unit test that builds a 2-pred join with `VarPhi[phi_tok, v0, v1]`, calls `add_node_input(phi, v2)` *without* adding a third predecessor to the ControlState, and asserts the debug-assert fires.  Combined with an existing test that runs the full validator and confirms the same condition surfaces as `PhiValueArityMismatch` to pin the production contract.

### Stale comment on `BuiltFunctionGraph` CC fields says fields are `pub` but they are `pub(crate)`
- **Severity:** LOW
- **Where:** crates/ir/src/function.rs:120-125.
- **What's wrong:** Comment block says "The fields themselves remain `pub` for back-compat (the workspace has ~30+ direct-field readers), but new code should use these accessors — they're the migration path for tightening field visibility to `pub(crate)` in a future round."  Looking at the actual field declarations (function.rs:57, 71, 79, 95, 104), every CC field is already `pub(crate)`.  The migration has happened; the comment is stale.
- **Verified against:** Cross-referenced with the field declarations at lines 57 (`variables`), 71 (`call_clobbered`), 79 (`ret_val_regs`), 95 (`call_other_clobbered`), 104 (`no_memory_clobber`) — all `pub(crate)`.
- **Fix:** Rewrite the comment to describe the current state — `pub(crate)` fields plus read-only `*_regs` / `*_map` / `no_memory_clobber` accessors, plus the test-only setters (`set_*_for_test`) for synthetic graph construction.
- **Regression test:** N/A (doc-only fix).

### `set_node_kind` debug-assertion uses the new kind's signature, not the old one's slot kinds — could silently accept a slot-kind mismatch
- **Severity:** LOW
- **Where:** crates/ir/src/graph/store.rs:75-96 (`set_node_kind`), crates/ir/src/node_signature.rs:422-441 (`slot_counts_match_kind`).
- **What's wrong:** `set_node_kind` checks `slot_counts_match_kind(self, node_id, &kind)` which compares the node's current slot *counts* against the new kind's expected counts (head_len exact-match / >= variadic head_len).  It does not check slot *kinds* — the kind-level check is left to the full `validate` pass that runs at pipeline exit.  In practice, the only caller is `opt::indirect_branch_resolve::inplace::apply_link_register` (inplace.rs:70) which rewrites `IndirectBranch → Return` and `IndirectBranch`'s signature is `[CTRL, MEM, TARGET]` while `Return`'s head is `[CTRL, MEM]` + variadic `RET` tail.  The caller manipulates arity beforehand; slot counts after rewrite are `>= 2` (head_len of variadic Return) so the assertion accepts.  But: the third input slot (TARGET = `AnyInt`) flows into a `RET` slot (`AnyValue`).  Both accept any int, so this happens to be fine.  However, the *general* contract — "post-mutation slot kinds match the new kind's expected signature" — is not enforced.  A future `set_node_kind` use that swaps, say, `Foo[Control]` for `Bar[Bool]` slot 0 would pass the count check while violating the kind contract until the next `validate`.
- **Verified against:** The `slot_counts_match_kind` body (node_signature.rs:427-440) confirms only counts are checked.
- **Fix:** Extend `slot_counts_match_kind` (or add a sibling `signature_matches_kind`) that also walks each slot index and verifies `kind_matches(slot.kind, graph.output_kind(input_i))` and the output dual.  Keep it `debug_assert!`-only so production has no overhead.  The current single caller (`apply_link_register`) is safe by happenstance — verify by inspection or pin via test.
- **Regression test:** Construct a synthetic non-cacheable node `Foo` with one Control input, call `set_node_kind` to mutate it to `Bar` whose signature expects a Bool input at slot 0.  Assert the debug assertion fires.

## Coverage summary

39 of 39 files in scope (40 including `tests/builder_extended_use.rs` which I size-checked but did not deep-read) inspected fully, 9 partially, 1 skipped (`builder_extended_use.rs` — size-checked but not read; the test patterns there are documented in surrounding files).
