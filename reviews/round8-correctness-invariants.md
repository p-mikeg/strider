# Round 8 / Ask 18-2 — Invariant-violation correctness audit

**Branch:** `review/ai2`.  Independent audit.

## HIGH

### H-1: `StackLoadForward::realize` BE narrow path — intermediate `ShiftRight` node has empty fingerprint

- **Severity:** HIGH.
- **Where:** `crates/opt/src/stack_load_forward/mod.rs:360-364` (node creation), `:117-124` (attribution).
- **Invariant violated:** Asm-fingerprint superset contract — every reachable non-exempt node must carry ≥1 contributor address.
- **Trigger:** Any Big-Endian target (`mipsbe32`, `aarch64be`, `ppc32be`, etc.) where `StackLoadForward` forwards a load whose byte-width is strictly narrower than the stored value's width.  `realize` emits `Truncate(ShiftRight(data, shift))`.  The `ShiftRight` node (`shr`, line 360) is created with plain `create_node`, not `create_node_attributed`.  `try_forward_load` then calls `fg.get_node_from_output(forwarded)` on the outermost output (`trunc`) and absorbs only into `trunc` (lines 123-124).  The `shr` node is reachable, non-exempt, and permanently carries an empty fingerprint.  The inline comment at `:117-122` ("the union semantics … keep us superset-correct") is incorrect for the multi-node case.
- **Fix:** Replace the two `fg.create_node` calls in the `Endianness::Big` arm with `fg.create_node_attributed(…, &[load])` calls.  Alternatively, have `realize` accept the load `NodeId` and call `extend_asm_fingerprint_from(shr, load)` after node creation.

### H-2: `IfCondInversion::invert` — `BoolNeg`'s fingerprint silently dropped

- **Severity:** HIGH.
- **Where:** `crates/opt/src/if_cond_inversion/mod.rs:101-105` (`invert` function).
- **Invariant violated:** Asm-fingerprint superset contract — a rewrite that discards a node (or makes it dead) must absorb its fingerprint into surviving nodes.
- **Trigger:** Any function where an `If(BoolNeg(X))` exists and `BoolNeg` has no consumers other than the `If`.  `invert` redirects the `If`'s cond input from `BoolNeg`'s output to `X`'s output (line 105: `graph.update_input(cond_input_id, inner)`).  After redirect, `BoolNeg` has zero consumers and is dead.  The pass does not call `extend_asm_fingerprint_from(X_node, bool_neg_node)`.  `BoolNeg`'s asm addresses are permanently lost.
- **Fix:**
  ```rust
  let inner_node = graph.get_node_from_output(inner);
  graph.extend_asm_fingerprint_from(inner_node, bool_neg_node);
  graph.update_input(cond_input_id, inner);
  ```

## MED

### M-1: Orchestrator `apply_in_place_edits` scans `all_node_ids()`, resurrecting zombie `InitialVar` nodes

- **Severity:** MED.
- **Where:** `crates/strider/src/orchestrator.rs:529-536`.
- **Invariant violated:** `FunctionArgDetect`'s post-detection invariant — after the post-pass runs, all argument-register reads in the reachable graph should be through canonical `FunctionArg` nodes, not raw `InitialVar`.
- **Trigger:** Any function with indirect branches (driving the fixed-point loop) where `FunctionArgDetect` runs as a stable-pipeline post-pass.  Sequence:
  1. Stable pipeline → `FunctionArgDetect` replaces `InitialVar(reg)` uses with `FunctionArg`; `InitialVar(reg)` becomes unreachable.
  2. `detach_unreachable_nodes` skips `InitialVar` (zero inputs, `worklist.rs:90` guard).  Zombie persists.
  3. `apply_in_place_edits` scans `graph.graph.all_node_ids()` (line 530), inserts the zombie into `initial_var_index` (lines 531-535).
  4. New Call-site argument reads via `read_or_init_var` return the zombie's output, wiring the raw `InitialVar` directly into the new Call.
  5. Pattern queries using `function_arg(i)` miss the resurrected raw read.
- **Fix:** Use `graph.graph.preorder(graph.entry)` instead of `all_node_ids()`, matching `FunctionArgDetect`'s own scan (`function_args/mod.rs:146`):
  ```rust
  for nid in graph.graph.preorder(graph.entry) {
      if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
          && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
      {
          initial_var_index.insert(*existing, out);
      }
  }
  ```

## Coverage

| # | Invariant | Verdict |
|---|-----------|---------|
| 1 | Asm-fingerprint superset contract (no-shrink, fold absorbs, cache hits union) | **2 violations** (H-1, H-2) |
| 2 | Dedup-cache structural equivalence | OK |
| 3 | Validator Layer-A/B/C reachability scoping | OK (Layer C uniqueness intentionally whole-arena) |
| 4 | Single Entry, single InitialMemory | OK |
| 5 | Monotonic memory chain (Call/Store/CallOther advance chain) | OK |
| 6 | Pcode → IR lifting determinism | **Partial** (M-1 zombie resurrection in same iteration) |
| 7 | Pattern dedup + capture binding agreement | OK (`bind_capture` returns false on conflict; `restore` is `truncate(mark.0)`) |
| 8 | CC presets register-name resolution | OK (`CallingConvention::build` errors on missing) |
| 9 | `from_graph_and_entry_for_rewrite` empty-CC contract | OK (`override_clobber_vars` only on real `BuiltFunctionGraph`) |
| 10 | Strict-progress orchestrator (stall budget, iter cap) | OK |
| 11 | Compact GC completeness (all 4 side-tables + wide_consts) | OK |
| 12 | Wide-const interning (same value → same WideConstId) | OK |

## Summary

- **2 HIGH** — both fingerprint contract violations in optimization passes (`StackLoadForward` BE narrow; `IfCondInversion`).
- **1 MED** — orchestrator zombie-node resurrection breaking `FunctionArgDetect` invariant on indirect-branch functions.
