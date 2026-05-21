# Round 13 — 1A: `ir` crate audit

Branch: `review/ai7` · Scope: `crates/ir/src/**`, `crates/ir/tests/**`, `crates/ir/benches/**`, `Cargo.toml`, `README.md` (~56 .rs).

## Verdict

**1 LOW finding (doc-comment drift); all other focus areas verified clean.**

## Findings

### IR-1 — Layer B doc comment contradicts implementation
- **Severity:** LOW (confidence 90)
- **Where:** `crates/ir/src/validate/mod.rs:61` (doc) vs `crates/ir/src/validate/layer_b.rs:39-41, 63-65` (impl).
- **What's wrong:** Top-level `validate` doc says *"Layer B and Layer C iterate all nodes but are naturally tolerant of detached nodes."* The actual `check_layer_b` impl gates both the backward sweep (`if !reachable.contains(source) { continue; }` lines 39-41) and the forward check (`if !reachable.contains(node) { continue; }` lines 63-65) on reachability. Layer B does NOT iterate all nodes — it only checks reachable nodes. The doc overstates coverage; the code is more conservative than advertised.
- **Verified against:** `layer_b.rs` directly — both gates explicit.
- **Fix:** Replace *"Layer B and Layer C iterate all nodes but are naturally tolerant of detached nodes"* with *"Layer A and Layer B are scoped to reachable nodes; only `check_layer_c_uniqueness` intentionally scans all nodes in the arena."*
- **Regression test:** N/A (doc-only fix).

## Categories verified clean

✓ **Graph dedup / `IntConst` masking** — `Graph::make_int_const` (`crates/ir/src/ops/consts.rs:87`) masks `val.into() & ty.bit_mask_u128()` before `create_node`. `FunctionBuilder::build_int_const` delegates to `make_int_const` without bypassing the mask. No alternate path to `NodeKind::IntConst(unmasked)` found in builder code.

✓ **Asm-fingerprint superset contract** — `FunctionBuilder::create_node` (`crates/ir/src/builder/mod.rs:448-454`) calls `extend_asm_fingerprint(node_id, &[addr])` unconditionally after `graph.create_node()`; since `create_node` returns the existing `NodeId` on a cache hit, the union is applied to the shared entry. `extend_asm_fingerprint` (`graph/store.rs:175-199`) never removes existing entries — it only appends, then deduplicates. `extend_asm_fingerprint_from` (`store.rs:205-214`) clones the source before mutating so `dst == src` is guarded correctly. Cache-hit union pinned by `tests/asm_fingerprint_dedup_union.rs`.

✓ **Validator reachability scoping** — Layer A scoped (`validate/mod.rs:96-101`: `if !reachable.contains(node) { continue; }`). `check_layer_c_uniqueness` intentionally scans all nodes (Entry/InitialMemory uniqueness). `check_layer_c_control_state`, `check_layer_c_phis`, `check_layer_c_function_arg_uniqueness`, `check_layer_c_wide_consts`, `check_layer_c_asm_fingerprints` all gate on reachability. Matches CLAUDE.md.

✓ **`FunctionBuilder` lift-addr funnel** — `FunctionBuilder::create_node` (`builder/mod.rs:442-454`) captures `self.lift_addr` before delegating to `graph_mut().create_node()` and unions it on return. `build_int_const` (`builder/nodes.rs:95-101`) takes the same pattern separately because it calls `graph_mut().make_int_const()` directly and then unions the addr. No builder `build_*` method bypasses the fingerprint recording path.

✓ **`node_signature` single source of truth** — `expected_signature` in `node_signature.rs` is the only definition of per-`NodeKind` slot shapes. The `expected_signature_covers_every_node_kind` test pins exhaustive coverage including `IntConstWide`. `slot_counts_match_kind` is used only in the `debug_assert` inside `set_node_kind`, not in production paths.

✓ **`from_graph_and_entry_for_rewrite`** — `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` (`function.rs:218-228`) is `#[doc(hidden)]`, carries an explicit contract warning, and is used only in `compact_tests` (same file) plus a handful of pattern test scaffolds. Production opt paths use `pattern::RewriteCtx` as documented.

✓ **Wide-constant correctness** — `Graph::intern_wide_const` (`graph/mod.rs:171-181`) dedups by value using `wide_const_dedup: FxHashMap<WideConstStorage, WideConstId>`. `make_int_const` rejects `U256`/`U512` with an explicit error. `build_int_const_wide` validates `value.byte_size() == expected` before interning. Layer C `check_layer_c_wide_consts` verifies no dangling `WideConstId` and no width mismatch.

✓ **`compact()` / `retain_reachable` side-table remapping** — `graph/compact.rs:194-228` rebuilds all four side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`) from the old→new pairs. `gc_wide_consts` is called before cache rebuild (step 6b before step 7), preserving the dedup-cache invariant. The `asm_fingerprints` remap uses `std::mem::take` (moves, not copies) and only writes non-empty entries — empty fingerprints stay as `SecondaryMap` defaults in the new table, consistent with `asm_fingerprint()` returning an empty slice for unset nodes.

## Coverage table

| File | Status |
|---|---|
| `src/lib.rs` | Fully read |
| `src/graph/{mod,store,access,compact,uses}.rs` | Fully read |
| `src/graph/tests.rs` | Skipped (test scaffolding) |
| `src/builder/{mod,nodes,call,vars,coerce}.rs` | Fully read |
| `src/builder/tests.rs` | Skipped (test-only) |
| `src/validate/{mod,layer_a,layer_b,layer_c}.rs` | Fully read |
| `src/validate/tests.rs` | Skipped (test-only) |
| `src/node_signature.rs` | Fully read |
| `src/function.rs` | Fully read |
| `src/region.rs` | Fully read |
| `src/walk.rs` | Fully read |
| `src/wide_const.rs` | Fully read |
| `src/node/{kind,output_type,output_kind}.rs` | Fully read |
| `src/node/data.rs` | Partial (header + struct defs) |
| `src/node/{ids,mod,tests}.rs` | Skipped (boilerplate / test-only) |
| `src/ops/{mod,consts,rewrite}.rs` | Fully read |
| `src/ops/builder.rs` | Partial (header) |
| `src/ops/op_kinds.rs` | Skipped (enum-only) |
| `src/iterators.rs`, `src/error.rs`, `src/test_utils.rs` | Skipped (boilerplate / test-only) |
| `src/dot/*` | Skipped (rendering-only) |
| `tests/asm_fingerprint_dedup_union.rs`, `tests/dedup_cache.rs` | Fully read |
| `tests/retain_reachable.rs` | Partial (first 60 lines) |
| Other `tests/*.rs`, `benches/*.rs`, `examples/*.rs` | Skipped |
