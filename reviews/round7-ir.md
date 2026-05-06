# Round 7 — ir Crate Audit

Independent review of `crates/ir/src/**` (no prior reviews consulted).

---

## CRITICAL / HIGH

### C-1 / SC-1 — Stale phantom node kind `IfCase` in fingerprint exemption doc — HIGH (conf 100)
- **Where:** `crates/ir/src/graph/mod.rs:96`
- **Evidence:** Doc lists `IfCase` as exempt; `NodeKind` has no `IfCase` variant. The actual `asm_fingerprint_exempt()` (`validate/layer_c.rs:164-177`) correctly omits it. The doc comment is wrong.
- **Fix:** Remove `IfCase` from the doc comment at `graph/mod.rs:96`.

### C-2 / P-1 / SC-2 — `NodeOutputType::info` uses `self as usize` with falsely-claimed test guard — HIGH (conf 95+)
- **Where:** `crates/ir/src/node/output_type.rs:51,68-70`
- **Evidence:** Comment at line 51 claims a test `type_info_table_matches_variants` asserts ordering; **no such test exists** anywhere in the file or the workspace. `info(self)` does `&TYPE_INFO[self as usize]`. Adding a new variant out-of-order silently misindexes or OOB-panics every hot type-system path (`as_str`, `byte_size`, `bit_width`, `is_bool`, `is_integer`, `is_float`).
- **Fix:** Either add a `const _: () = assert!(NodeOutputType::Bool as usize == 0 && NodeOutputType::U8 as usize == 1 && …);` block or replace with an explicit `match`. Then either add the promised test or delete the false claim.

### P-3 — `compact.rs:127` `expect` on cross-reachability invariant — HIGH (conf 87)
- **Where:** `crates/ir/src/graph/compact.rs:126-129`
- **Evidence:** Panics if a reachable node has an input pointing to an unreachable producer. Invariant ("every input's producer is reachable iff its owner is reachable") is graph-level, not language-level. Mid-optimization graphs (or external code) violating it crash the process.
- **Fix:** Make `retain_reachable` return `anyhow::Result` and propagate a proper error.

---

## MEDIUM

### V-1 — `InputPointsToMissingOutput` variant declared but never emitted — MED (conf 95)
- **Where:** `crates/ir/src/validate/mod.rs:177-182`; `validate/layer_b.rs:44-49`
- **Evidence:** Variant exists in the public enum; Layer B comment explicitly says it's not checked.
- **Fix:** Either implement the check (iterate inputs, verify `output_id` is a valid key in `graph.outputs`) or delete the variant.

### V-2 — `check_layer_c_phis` runs on all nodes (zombie-included) — MED (conf 90)
- **Where:** `crates/ir/src/validate/layer_c.rs:102-157`
- **Evidence:** No reachability filter. A zombie phi with one phi-token input but no value inputs would skip the empty-input early-return at line 116 only if it had ≥1 input total — but more importantly, partially-detached phis (mid-optimization) generate spurious `PhiValueArityMismatch`.
- **Fix:** Accept a `reachable: &EntitySet<NodeId>` and skip non-reachable nodes (matching Layer A scoping).

### NK-1 — CLAUDE.md lists `FloatIsNan`, `Piece`, `Extract`, `Insert` as IR node kinds — none exist — MED (conf 100)
- **Where:** `crates/ir/src/node/kind.rs` (no such variants); CLAUDE.md "IR Node Model" section
- **Evidence:** Documentation/code mismatch.
- **Fix:** Remove from CLAUDE.md or add the variants (Round 3 will resolve which).

### SC-3 — `InputPointsToMissingOutput` doc creates false-confidence — MED (conf 88)
- See V-1.

### SC-4 — `build_call_other_terminal` doesn't mark region terminated — MED (conf 85)
- **Where:** `crates/ir/src/builder/call.rs:185-207`
- **Evidence:** Builder leaves `cur_region` alive; subsequent `build_*` calls succeed silently, producing IR inconsistency. Comment warns but no guard.
- **Fix:** Call `terminate_cur_region()` inside `build_call_other_terminal`, or add a runtime assertion that the region has no further instructions.

### PY-1 — `validate_with_options` not exposed to Python — MED (conf 95)
- **Where:** `crates/strider-py/src/graph.rs`
- **Fix:** Wrap `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: bool })`.

### PY-2 — `Graph::node_kind` not exposed to Python — MED (conf 90)

### PY-3 — `Graph::call_other_name` not exposed to Python — MED (conf 88)

---

## LOW

### V-3 — Layer A correctly scopes to reachable; documented design. No issue.

### W-1 — `walk_graph` uses dense visited-set; terminates on cycles. No issue.

### FB-1 — `lift_addr` correctly threaded through all node-creation paths via `FunctionBuilder::create_node` wrapper. Verified at `builder/mod.rs:393-405`. No issue.

### FB-3 — `Region.variables: SecondaryMap<VarId, NodeOutputId>` returns `NodeOutputId(0)` (the Entry control output) for unset entries — silent wrong-edge risk if regions are constructed with sparse `initial_variables` maps. Pre-initialization in `set_entry_region` mitigates but doesn't enforce. **Fix:** Use `Option<NodeOutputId>` or assert initialization.

### P-2 — `compact.rs:117` first `expect` ("just installed in pass 1") — JUSTIFIED.

### P-4 — `function.rs:149-150` `expect("entry must survive its own compaction")` — JUSTIFIED but should document.

### P-5 — `Outputs::index` panics on OOB — caller convenience that violates "no panic" policy. **Fix:** mark `panics on OOB` in doc or remove.

### D-1 — `InputPointsToMissingOutput` is dead code (V-1).

### D-2 — `Signature` / `SlotList` / `Slot` / `ExpectedOutputKind` / `SlotRole` are `pub` in `node_signature.rs`; verify external use; if none, demote to `pub(crate)`.

### N-1 — `build_call_other_terminal` vs `build_call_other_modeled` naming asymmetry; no `_noop` builder (NoOp is just skipped at lift). Cosmetic.

### PY-4 — `Graph::compact` / `BuiltFunctionGraph::compact` not exposed to Python.

### PY-5 — `Graph::all_node_ids` iteration not exposed; only `node_count`.

---

## Verified-Correct (no issues found)

- **Cache key correctness:** `Load(VnSpace::Ram)` vs `Load(VnSpace::Other)` distinguished via `NodeKind` hash; `IntConst` width via `output_kinds` Vec; `build_int_const` masks value before node creation.
- **Asm-fingerprint dedup-cache hit:** `FunctionBuilder::create_node` calls `extend_asm_fingerprint(node_id, &[addr])` after cache hit → unions correctly.
- **Side-tables preserved through `retain_reachable`:** all four (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`) remapped or dropped consistently.
- **`is_cacheable`:** correctly excludes Call/Return/CallOther/variadic; correctly includes pure ops including `StackStore { space, offset }`.
- **`validate(&graph, entry)` called at end of `FunctionBuilder::build()`** — confirmed at `builder/mod.rs:531`.

---

## Findings Count

| Section | HIGH | MED | LOW |
|---------|------|-----|-----|
| Correctness / Graph | 2 | 2 | 0 |
| Validation | 0 | 2 | 2 |
| FunctionBuilder | 0 | 0 | 3 |
| NodeKind | 0 | 1 | 1 |
| walk.rs | 0 | 0 | 2 |
| Panics | 1 | 2 | 2 |
| Dead Code | 0 | 0 | 3 |
| Stale Comments | 2 | 2 | 1 |
| Naming | 0 | 0 | 2 |
| Python Parity | 0 | 3 | 2 |
| **Total** | **5** | **12** | **18** |

## Top 5 Impactful Items

1. **C-2/P-1 (HIGH)** `NodeOutputType::info` indexing without guard + nonexistent test claim. Adding a variant out of order silently corrupts type metadata.
2. **C-1/SC-1 (HIGH)** Phantom `IfCase` in fingerprint-exemption doc.
3. **V-1 (MED)** `InputPointsToMissingOutput` is dead error variant.
4. **P-3 (MED)** `compact.rs:127` panicking `expect` on graph invariant.
5. **SC-4 (MED)** `build_call_other_terminal` allows further builder calls into the same region.
