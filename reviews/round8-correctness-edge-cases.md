# Round 8 / Ask 18-4 — Boundary / edge-case correctness audit

**Branch:** `review/ai2`.  Independent audit.

## HIGH

### H1: `decompose_sp` mutual recursion overflows the call stack on deep SP-expression chains

- **Confidence:** 87.
- **Severity:** HIGH.
- **Where:** `crates/opt/src/sp_expr.rs:247` (`decompose_sp`), `:277` (`decompose_sp_inner`); recursion at `:301`/`:303`.
- **Boundary category:** Deep recursion on large inputs / memory-chain extremes.
- **What happens:** `decompose_sp` → `decompose_sp_inner` → for `IntBinaryOp::Add`, recurses into `decompose_sp` on the left operand.  The memo only caches `Some(_)` results and each node is visited once on the way down, so for a straight-line chain `sp - 1 - 1 - ... - 1` of N Add nodes the call stack grows N frames.  Rust's default thread stack is 8 MB; at ~4000-8000 nodes the process aborts with stack overflow.  Reachable for x86 functions with long prologues / many constant-step SP adjustments / spill-heavy optimised binaries.
- **Note:** `probe` in `stack_load_forward/mod.rs` was converted to an explicit worklist for the same reason.  `decompose_sp` was missed.
- **Fix:** Convert `decompose_sp` + `decompose_sp_inner` to an explicit iteration stack (same approach as `probe`).  Alternatively, add a depth counter and return `None` beyond ~1000.

### H2: `step_through_stack_store_phi` treats empty `stack_phi_offsets` as `PassThrough`, producing unsound alias analysis

- **Confidence:** 80.
- **Severity:** HIGH (latent — production path always populates offsets).
- **Where:** `crates/opt/src/sp_expr.rs:131-152`.
- **Boundary category:** Empty container / zero-predecessor phi.
- **What happens:** `Graph::stack_phi_offsets` returns `&[i64]` backed by `SecondaryMap<NodeId, Vec<i64>>` whose default is `[]`.  When the side-table entry is empty, `any_overlap = false` and the function returns `PassThrough`, declaring the StackStorePhi provably non-aliasing.  Correct conservative answer for unknown offsets is `MayAlias`.
- **Production safety:** `StackStoreDetect` always calls `fg.set_stack_phi_offsets(new_node, offsets)` immediately after creating each `StackStorePhi` (`detect.rs:64`), so the production path is safe today.  No validator check enforces that a reachable `StackStorePhi` has non-empty offsets, so any future inliner / mock graph / manual builder that creates the node without populating offsets would produce unsound forwarding.
- **Fix:**
  ```rust
  let offsets = graph.stack_phi_offsets(node);
  if offsets.is_empty() {
      return AliasStep::MayAlias;
  }
  let any_overlap = offsets.iter().any(|&k| !ranges_disjoint(k, store_size, query_off, query_size));
  ```

## Confirmed correct (informational)

- `ranges_disjoint` with `i64::MAX` size: saturating_add + saturation-equality check; unit-tested.
- `eval_int_binary` div-by-zero and INT_MIN/-1: both guarded.
- `eval_float_*` NaN propagation: delegates to Rust f32/f64 arithmetic; IEEE 754 compliant.
- `read_table_entries` `MAX_TABLE_ENTRIES = 4096` cap + `checked_mul`/`checked_add`: no overflow.
- `is_addr_tail_call` with `fn_max_size = Some(0)`: degenerate but intended (every address OOB of `[start, start)`).
- `find_all_requirements` with 0 or 1 patterns: cross-product correctly empty/trivial.
- `KnownBits` returning `None` for U80/U128/U256: conservative.
- `bit_mask_u128` ≥ 128 width returns `u128::MAX`: documented degraded behavior; sound for u128-valued masks.
- `cap()` formula `2 * pending + 4` with saturating arithmetic: saturates at `usize::MAX`.
- `MemRegion::new` overflow guard: explicitly rejects `start_addr + data.len() > u64::MAX`.
- `apply_link_register` target-value removal guard at line 61: redundant (always true) but behaviourally correct (cross-ref `round8-1C-opt.md` finding M1).
- `RedundantPhis` positional index `inputs[j+1]`: safe (`j` from `position()` over `ctrl_inputs` of equal length).
- `decompose_sp_phi` index `bases[0]` at line 381: safe (`inputs.len() < 2` guard at line 361).
- `FunctionBuilder::largest_container_for` `saturating_add` for varnode offsets: sound per inline comment.
- `classify_arch_specific` / `classify_arch_independent`: exhaustive match; tested.

## Summary

- **2 HIGH** — both in `crates/opt/src/sp_expr.rs`: deep-recursion stack overflow risk in `decompose_sp`; unsound `PassThrough` default in `step_through_stack_store_phi` for empty offsets (latent until a non-`StackStoreDetect` builder creates one).
- **No medium or low findings above the 80-confidence threshold.**
