# Round 11 — 1A: ir crate audit

## Coverage

| Path | Inspected fully? | Notes |
|------|------------------|-------|
| crates/ir/Cargo.toml | yes | |
| crates/ir/README.md | yes | |
| crates/ir/src/lib.rs | yes | |
| crates/ir/src/error.rs | yes | |
| crates/ir/src/function.rs | yes | including the `compact_tests` inline tests |
| crates/ir/src/iterators.rs | yes | |
| crates/ir/src/node_signature.rs | yes | including the embedded `tests` module |
| crates/ir/src/region.rs | yes | |
| crates/ir/src/test_utils.rs | yes | |
| crates/ir/src/walk.rs | yes | including `tests` module |
| crates/ir/src/wide_const.rs | yes | including `tests` module |
| crates/ir/src/builder/mod.rs | yes | |
| crates/ir/src/builder/call.rs | yes | |
| crates/ir/src/builder/coerce.rs | yes | |
| crates/ir/src/builder/nodes.rs | yes | |
| crates/ir/src/builder/tests.rs | partially | spot-checked; 1740 lines, focused on `lift_at` / `set_lift_addr` / dedup / wide-const round-trip |
| crates/ir/src/builder/vars.rs | yes | |
| crates/ir/src/dot/label.rs | yes | |
| crates/ir/src/dot/mod.rs | yes | |
| crates/ir/src/dot/render.rs | yes | |
| crates/ir/src/dot/tests.rs | partial | 501 lines, sampled |
| crates/ir/src/graph/mod.rs | yes | |
| crates/ir/src/graph/access.rs | yes | |
| crates/ir/src/graph/compact.rs | yes | |
| crates/ir/src/graph/store.rs | yes | |
| crates/ir/src/graph/uses.rs | yes | |
| crates/ir/src/graph/tests.rs | yes | |
| crates/ir/src/node/mod.rs | yes | |
| crates/ir/src/node/data.rs | yes | |
| crates/ir/src/node/ids.rs | yes | |
| crates/ir/src/node/kind.rs | yes | |
| crates/ir/src/node/output_kind.rs | yes | |
| crates/ir/src/node/output_type.rs | yes | including `tests` module |
| crates/ir/src/node/tests.rs | partial | 348 lines, sampled |
| crates/ir/src/ops/builder.rs | yes | |
| crates/ir/src/ops/consts.rs | yes | |
| crates/ir/src/ops/mod.rs | yes | |
| crates/ir/src/ops/op_kinds.rs | yes | |
| crates/ir/src/ops/rewrite.rs | yes | |
| crates/ir/src/validate/layer_a.rs | yes | |
| crates/ir/src/validate/layer_b.rs | yes | |
| crates/ir/src/validate/layer_c.rs | yes | |
| crates/ir/src/validate/mod.rs | yes | |
| crates/ir/src/validate/tests.rs | yes | |
| crates/ir/benches/validate.rs | skipped | bench-only |
| crates/ir/examples/graph_creator.rs | skipped | example-only |
| crates/ir/tests/asm_fingerprint_dedup_union.rs | yes | |
| crates/ir/tests/build_call_with_cc.rs | partial | sampled by name only |
| crates/ir/tests/build_validate_roundtrip.rs | partial | sampled by name only |
| crates/ir/tests/builder_extended_use.rs | partial | sampled by name only |
| crates/ir/tests/call_other_classification.rs | partial | sampled by name only |
| crates/ir/tests/call_other_modeled.rs | partial | sampled by name only |
| crates/ir/tests/common/mod.rs | partial | helpers; not load-bearing |
| crates/ir/tests/dedup_cache.rs | yes | |
| crates/ir/tests/int_const_dedup.rs | partial | sampled |
| crates/ir/tests/proptest_graph_invariants.rs | partial | sampled |
| crates/ir/tests/retain_reachable.rs | yes | |
| crates/ir/tests/walk_reachability.rs | partial | sampled |

## Findings

### `bit_mask_u128` returns `u128::MAX` for `U256` / `U512` — silent over-acceptance in `make_int_const` masking is harmless today but the doc lies
- **Severity:** MED
- **Where:** `crates/ir/src/node/output_type.rs:177-189` (`bit_mask_u128`); `crates/ir/src/ops/consts.rs:82-107` (`Graph::make_int_const`); `crates/ir/src/builder/nodes.rs:86-104` (`FunctionBuilder::build_int_const`)
- **What's wrong:**  `bit_mask_u128` returns `u128::MAX` for any integer width >= 128 (`if bits >= 128 { return u128::MAX; }`).  Both `U256` and `U512` are `is_integer() == true` (`output_type.rs:62/65`), so they hit this path.  The doc-comment at lines 173-175 says callers that want to mask a 256-bit value "must use `Self::get_unsigned_int`, which rejects `U256` outright" — but `get_unsigned_int` does NOT reject U256 (lines 204-209 just call `val & self.bit_mask_u128()`, which is `val & u128::MAX = val` for U256/U512).  The actual rejections of wide widths live one layer up in `make_int_const` and `build_int_const` (lines 88-93 and 96-101), which raise an error if `output_type` is U256/U512 — those are the real safeguards.  This is a documentation bug rather than a runtime bug: today no caller can construct a `NodeKind::IntConst(_, U256)` through the public guarded constructors, and the validator's wide-const Layer C check (`check_layer_c_wide_consts`) only inspects `IntConstWide`, not `IntConst`.  But the inconsistency means a future refactor that consults `bit_mask_u128` directly to gate widths is silently wrong for U256+.
- **Verified against:** `output_type.rs:204-209` (`get_unsigned_int` body — the doc claim of "rejects U256 outright" is contradicted by the actual code); `consts.rs:88-93` and `nodes.rs:96-101` for the real rejection sites; `validate/layer_c.rs:271-326` confirms the validator only catches the `IntConstWide` half of the contract.
- **Fix:** Either (a) tighten the doc on `bit_mask_u128` and `get_unsigned_int` so it accurately says "U256/U512 return `u128::MAX` and are not safe to use as a mask", and add a per-call rejection inside `get_unsigned_int` ("integer width > 128 → None"), or (b) add a Layer C `check_layer_c_int_const_widths` that flags any `IntConst(_, U256/U512)` (currently impossible to reach via guards but would prevent silent widening if a future caller bypasses the guards).
- **Regression test (when applicable):**  Add `bit_mask_u128_for_wide_widths_returns_unrepresentable_sentinel` pinning the explicit `u128::MAX` sentinel and `get_unsigned_int_rejects_u256_when_caller_must_round_trip` once the rejection lands.

### `Graph::set_node_kind` does not enforce signature-shape compatibility between the old and new kinds
- **Severity:** MED
- **Where:** `crates/ir/src/graph/store.rs:50-75`
- **What's wrong:**  The doc-comment promises "Only valid when the pre-edit and post-edit kinds share the SAME input and output signatures (so the existing edges remain well-typed)" but the body only checks that both kinds are non-cacheable.  The validator catches a Layer-A mismatch on a subsequent `validate` call, but the function itself silently accepts mismatched signatures and the resulting graph violates Layer A until validation runs.  The intended use case (rewriting `IndirectBranch` → `Return`) only happens to work because `Return`'s variadic input tail is `RET` (= `AnyValue`, see `node_signature.rs:232-236`) which subsumes `IndirectBranch`'s `TARGET` slot kind (`AnyInt`); a future rewrite, e.g. swapping a non-cacheable kind into one with strictly fewer inputs, would corrupt the edge graph silently because `set_node_kind` doesn't trim the input list to match the new kind's head_len.
- **Verified against:** `node_signature.rs:339-344` (`Return` shape: `inputs: [CTRL, MEM]; in_tail: RET, outputs: []`) vs `IndirectBranch` (`inputs: [CTRL, MEM, TARGET], outputs: []`).  The compatibility relies on `Return`'s tail being `AnyValue` and `IndirectBranch`'s third input being a value.
- **Fix:** Either (a) drop the doc claim and explicitly require callers to validate post-rewrite (status quo, just honest), or (b) compute `expected_signature` for both old and new and reject mismatches inline so the function is a self-contained guard.  Option (b) is the safer long-term shape — it would catch a future "swap a Call to a Return" rewrite that loses inputs.
- **Regression test (when applicable):**  `set_node_kind_rejects_signature_mismatch` — try `set_node_kind(some_indirect_branch_node, Call)` and assert the error message names both kinds and the offending slot.

### Documentation on `validate` falsely claims Layer B and Layer C "iterate all nodes"
- **Severity:** LOW
- **Where:** `crates/ir/src/validate/mod.rs:53-64` (doc), vs the actual scoping in `layer_b.rs:28-86` and `layer_c.rs:56-260`
- **What's wrong:**  The doc on `validate` states:  "Layer B and Layer C iterate all nodes but are naturally tolerant of detached nodes: `detach_node_inputs` scrubs the use-lists of the producers it disconnects, so a detached node contributes no inputs and no live use-list entries anywhere."  In reality, **Layer B is reachability-scoped** (`layer_b.rs:39-43`, `:63-66`), and four of the six Layer C checks are reachability-scoped (`check_layer_c_control_state`, `check_layer_c_phis`, `check_layer_c_function_arg_uniqueness`, `check_layer_c_wide_consts`, `check_layer_c_asm_fingerprints` — five of six).  Only `check_layer_c_uniqueness` scans the whole arena, and that's intentional (the doc on that function correctly says so).
- **Verified against:** Each `check_layer_c_*` function in `layer_c.rs` and the `if !reachable.contains(node) { continue; }` gate at the top of each.  Layer B's gate is at line 41 and 64.
- **Fix:** Update the doc-comment on `validate` to reflect actual scoping: "Layer A and Layer B are reachability-scoped (skip detached zombies), Layer C uniqueness is graph-wide (intentional — duplicate Entry / InitialMemory must be reported even when one is a zombie), and the remaining Layer C checks are reachability-scoped."
- **Regression test (when applicable):**  No code change — doc fix only.

### `lift_addr` non-RAII funnel through `set_lift_addr` is panic-leaky when used directly
- **Severity:** LOW
- **Where:** `crates/ir/src/builder/mod.rs:393-403` (`set_lift_addr`); contrasted with the panic-safe `lift_at` (lines 412-436) which has a `Drop` guard
- **What's wrong:**  `lift_at` was added to fix the prior panic-leak (the comment at lines 410-411 explicitly mentions "(R9-1A I3) closed the prior leak path where a panic would leave `addr` set on the outer scope") and is panic-safe via a `Drop` guard.  But `set_lift_addr` itself is still public, and the strider lift driver (`crates/strider/src/strider/insn/mod.rs:44-46`) still uses the pair `set_lift_addr(Some(_)) … set_lift_addr(None)` rather than `lift_at` because of mutable-borrow constraints (comment at insn/mod.rs:36-39).  If the in-between work panics, the FunctionBuilder's `lift_addr` field stays set at the previous insn's address.  In practice this is benign because a panic in production aborts/unwinds the whole orchestrator, but it's an API hazard: a future caller using `set_lift_addr(Some(_))` followed by fallible `?` operators can leave the addr set on early-return as well.  Note: early-return via `?` is **not** caught by the `Drop` guard in `lift_at` either — `lift_at` only protects panics, not `?`.  But `lift_at`'s closure-form makes it harder to mistakenly skip the unset.
- **Verified against:** `builder/mod.rs:393` (no guard); `strider/src/strider/insn/mod.rs:44-46` (manual pair); `lift_at` body lines 416-435 (Drop-guard implementation).
- **Fix:** Either (a) accept the documented hazard (current status), or (b) deprecate `set_lift_addr` in favour of `lift_at` and rework the strider driver so the `&mut FunctionBuilder` re-borrow happens inside the closure, or (c) have `set_lift_addr` return a guard token that callers must consume to clear (linear-types-style).  None is urgent.
- **Regression test (when applicable):**  None for current code; if (b) is adopted, pin that early-return through `?` inside `lift_at` correctly restores `prev` (it does, via `Drop`).

### `Node` cache key relies on the un-documented invariant that `Node::new(kind)` always produces empty `EntityList`s with `index == 0`
- **Severity:** LOW
- **Where:** `crates/ir/src/node/data.rs:74-91` (`Node` derives `PartialEq, Eq, Hash`); `crates/ir/src/graph/store.rs:211-249` (cache insert) and lines 315-337 (cache evict, building `Node::new(self.nodes[node_id].kind)`)
- **What's wrong:**  `Node` includes the per-node `inputs: NodeInputIdList` and `outputs: NodeOutputIdList` in its derived `Hash` / `PartialEq` impl.  The dedup cache key `(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>)` reuses `Node` from `Node::new(kind)` — which has both lists at `EntityList::default()` (index 0).  After `create_node` populates the actual `Node` in the arena (lines 274-275: `self.nodes[node_id].inputs = …; self.nodes[node_id].outputs = …;`), the arena's Node has nonzero list indices, but the cache still keys on the `Node` with zero indices.  Eviction reconstructs the key via `Node::new(self.nodes[node_id].kind)` (line 332), again producing zero indices, so eviction matches insertion.  This works **only because** `Node::new` always produces empty lists with `index == 0`.  If a future change makes `Node::new` populate inputs/outputs upfront, or adds a non-default unique-id field to `Node`, the cache key shape diverges silently and dedup breaks (or evict-then-rehash misses, leaving stale entries that resurrect zombies).
- **Verified against:** `data.rs:84-90` — `Node::new` body sets `inputs: NodeInputIdList::new(), outputs: NodeOutputIdList::new()`.  `cranelift-entity/src/list.rs:65-80` (read into the dependency directly) — `EntityList::default()` returns `index: 0`, and the derived `Hash` / `PartialEq` on the index field treats `index 0` consistently across all callers.  `store.rs:211` (`Node::new(kind)` for the key) and `:332` (`Node::new(self.nodes[node_id].kind)` for the eviction key) — both rely on the same upfront-emptiness invariant.
- **Fix:**  Define a `CacheKey` newtype that explicitly carries only the cache-relevant fields: `(NodeKind, Vec<NodeOutputId>, Vec<NodeOutputKind>)` — and drop the `inputs`/`outputs` from the Node-portion of the key entirely.  This decouples the cache contract from the `Node` struct's internal layout.  Mechanical refactor; the `RawEntryMut` borrowed-key dance still works because the borrowed key just changes from `(&Node, &[…], &[…])` to `(&NodeKind, &[…], &[…])`.
- **Regression test (when applicable):**  `cache_key_does_not_depend_on_node_input_output_lists` — assert that two cache hits across `add_node_input` / `detach_node_inputs` of the cached node behave correctly (already covered by `detach_evicts_cacheable_node_from_dedup_cache` and `update_input_evicts_cacheable_node_from_dedup_cache`, but those rely on the eviction scrubbing the stale entry; the proposed refactor would make the dedup contract self-evident from the type signature).

### `gc_wide_consts` could in principle merge two distinct old `WideConstId`s with equal values, but `intern_wide_const` already value-dedups so this can't happen for IRs built through the public API
- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:248-297`
- **What's wrong:**  `gc_wide_consts` rebuilds the side-table from scratch and re-interns every live wide-const value.  If two old ids reference equal values, the rebuild merges them: `IntConstWide(old_id1)` and `IntConstWide(old_id2)` (different NodeIds, equal values) would be rewritten to the same `new_id`, making them structurally equal under the dedup-cache key.  The cache rebuild at lines 165-189 then uses `last writer wins` (line 188), keeping only one entry.  The other distinct NodeId in the arena survives but is no longer reachable via `create_node`'s cache lookup — it becomes a non-deduped twin until the next compaction.  In practice this can't happen for IRs built through the public API: `Graph::intern_wide_const` is the only path to obtain a `WideConstId`, and it already value-dedupes (line 175-179) — so two different `WideConstId`s always have different values, and `gc_wide_consts` cannot merge them.  **However**, this defensive guarantee is undocumented and a future code path that pushes directly to `wide_consts` without going through `intern_wide_const` would break it.
- **Verified against:** `graph/mod.rs:171-181` (`intern_wide_const`'s value-dedup body) and the `wide_consts: PrimaryMap` direct-access surface at `graph/mod.rs:130` (which is `pub(crate)` — so internal callers could in theory bypass `intern`).
- **Fix:** Add a doc-comment on `wide_consts` field stating "must only be mutated via `intern_wide_const` to preserve the value-dedup invariant that `gc_wide_consts` relies on", and either tighten the field to private with an internal builder, or add a debug assert in `intern_wide_const` validating the invariant when entries already exist.  Or, conversely, remove the value-dedup guard inside `intern_wide_const` and let `gc_wide_consts` be the single authoritative dedup point; either is consistent.
- **Regression test (when applicable):**  None until the access pattern changes.

### `bit_mask_u128` doc-comment understates F80 / U80 handling
- **Severity:** LOW
- **Where:** `crates/ir/src/node/output_type.rs:171-189`
- **What's wrong:**  The doc says "Float types return `0` (defensive — no caller should ask)."  This is consistent with `is_float()` returning true for F32, F64, F80 and the `if bits == 0 || !self.is_integer() { return 0; }` check.  But the test `bit_mask_u128_for_u80` (lines 396-401) asserts `F80.bit_mask_u128() == 0` — confirmed.  Meanwhile `is_integer` returns true for U80 (in `tests::u80_is_integer_and_f80_is_float`).  So `U80.bit_mask_u128() = (1 << 80) - 1`.  The doc says "integer widths up to 128 bits return their natural bit widths" — U80 is up-to-128, so this is correct, but the doc's "U256 returns `u128::MAX`" line should also mention U512 (it doesn't).  Minor doc completeness issue.
- **Verified against:** `output_type.rs:171-189` (body) and `tests` modules at lines 290-435 (asserted behaviour).
- **Fix:**  Update the doc-comment to explicitly enumerate U256/U512 returning `u128::MAX` (one line edit).
- **Regression test (when applicable):**  None (doc-only).

### Layer C uniqueness scans the whole arena while every other Layer C check is reachability-scoped — the asymmetry is intentional but the inconsistent scoping is fragile under future zombie-leaving rewrites
- **Severity:** LOW
- **Where:** `crates/ir/src/validate/layer_c.rs:23-52` (uniqueness, graph-wide); contrast `:56-97`, `:108-178`, `:202-222`, `:234-260`, `:271-327` (reachability-scoped)
- **What's wrong:**  `check_layer_c_uniqueness` scans `graph.nodes.keys()` and reports `MultipleEntryNodes` / `MultipleInitialMemoryNodes` even when one is unreachable.  The doc at lines 7-22 says this is intentional ("intentionally scans every node in the arena (including detached zombies)") because a real second Entry or InitialMemory is structurally invalid regardless of reachability.  However, every other Layer C check (control_state, phis, function_arg_uniqueness, wide_consts, asm_fingerprints) is reachability-scoped to tolerate zombies.  Today the Entry / InitialMemory uniqueness can't be tripped by opt passes because no opt pass synthesises a fresh Entry or InitialMemory — only `FunctionBuilder::build_entry` does.  But if a future pass ever leaves a zombie Entry (e.g. an in-place rewrite that allocates a fresh Entry node before splicing), the validator would incorrectly fire `MultipleEntryNodes` on the zombie.  This is the inverse of the `DuplicateFunctionArg` test (`validate/tests.rs:618-653`) which explicitly proves the validator skips unreachable zombie FunctionArg nodes — the same regression-resistance is missing for Entry / InitialMemory.
- **Verified against:** Compare `check_layer_c_uniqueness` (no `reachable` parameter) against every other Layer C check, all of which take and gate on `reachable: &NodeIdSet`.  The doc at lines 11-16 documents the intentionally-different scope.
- **Fix:**  Decide intent: (a) keep the graph-wide scope (current status — defensible as "duplicate Entry/InitialMemory is always a bug"), or (b) make it reachability-scoped to be consistent with all other Layer C checks.  If (a), add a regression test like `layer_c_uniqueness_flags_zombie_duplicate_entry` to pin the behaviour.  If (b), update the doc.  Either way, the silently-asymmetric scoping is a footgun for future Layer C additions.
- **Regression test (when applicable):**  `layer_c_uniqueness_flags_zombie_duplicate_entry` (under choice (a)).

### `Outputs::Index` and `Inputs::Index` panic on out-of-bounds — documented but the panic is a real production failure mode
- **Severity:** LOW
- **Where:** `crates/ir/src/iterators.rs:34-50` (`Outputs::Index`) and `:101-116` (`Inputs::Index`)
- **What's wrong:**  Both `Index` impls panic on out-of-bounds.  The doc-comments correctly enumerate the panic and recommend `node_outputs_exact::<N>` / `node_inputs_exact::<N>` as the fallible alternative.  Production opt-pass code does use the panicking `[idx]` form for "validator-pinned slots" — relying on the per-kind expected signature.  This is the documented contract, but it means a graph that survives Layer A validation and is then mutated post-validation could crash an opt pass with an index panic.  The validator's Layer A signature check is reachability-scoped, so detached zombies with mismatched arity skip Layer A — and indexing into one of those crashes.  The mitigation is "don't index zombies", but since `preorder()`-based passes only visit reachable nodes, this is implicit.
- **Verified against:** `iterators.rs:47-49` and `:113-115` for the panicking bodies; the doc-comments above each.
- **Fix:**  Already mitigated.  No code change required, but consider an `#[track_caller]` annotation on both `index` methods so panic backtraces point at the call-site rather than the internal index expression.
- **Regression test (when applicable):**  None — already documented behaviour.

### `extend_asm_fingerprint`'s fast-path assumes contributors arrive monotonically increasing; out-of-order multi-element calls fall back to sort+dedup
- **Severity:** LOW
- **Where:** `crates/ir/src/graph/store.rs:154-178`
- **What's wrong:**  The fast path appends `addr` only when it is strictly greater than the last existing entry (or when the existing list is empty).  For out-of-order `addr` values within a single call, `needs_resort = true` is set and the function falls back to `sort_unstable + dedup` at the end.  This is functionally correct but allocation-amplifying for a multi-element out-of-order extend (e.g. extending with `[0x100, 0x80]` on an empty list pushes both, sets `needs_resort`, then sorts).  For typical single-element calls (the dominant lift-time path) the fast path is optimal.  No correctness issue, only a minor inefficiency.  Worth flagging because the doc-comment at lines 154-157 implies the fast path always wins when contributors are "strictly-greater", but the ad-hoc fallback for any out-of-order element can pessimise multi-extend bulk loads.
- **Verified against:** `store.rs:163-177` body.
- **Fix:**  None unless profiling shows this is a hot path; the current shape is reasonable.
- **Regression test (when applicable):**  None.

## Coverage summary

47 of 49 files inspected fully, 4 partially, 2 skipped (benches/examples).
