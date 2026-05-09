# Round 8 / 1A — `ir` crate audit

**Branch:** `review/ai2`.  Independent audit.

## Coverage

All `crates/ir/src/**/*.rs` (52 files full, 4 partial test bodies, 2 skipped rendering-only) and `crates/ir/tests/**/*.rs` inspected.  `crates/ir/Cargo.toml` and `crates/ir/benches/validate.rs` reviewed.

## Findings

### HIGH: `build_int_const` and `make_int_const` silently accept `U512`, producing a type-confused `IntConst` node

- **Confidence:** 92.
- **Severity:** HIGH.
- **Where:** `crates/ir/src/builder/nodes.rs:96-102`; `crates/ir/src/ops/consts.rs:87-97`.
- **What's wrong:** Both functions guard against `NodeOutputType::U256` but **not** `U512`.  When called with `U512`:
  - The guard passes.
  - `bit_mask_u128()` returns `u128::MAX` because `bit_width() = 512 >= 128` (`output_type.rs:184-186`).
  - `val.into() & u128::MAX` passes the full 128-bit value unchanged.
  - The resulting `NodeKind::IntConst(val)` is created with `NodeOutputType::U512` output — claiming 512 bits, storing 128.
- **Validator misses it:**
  - Layer A: `expected_signature(IntConst) = outputs:[AnyInt]`; U512 satisfies `is_integer()`.
  - Layer C `check_layer_c_wide_consts` gates on `IntConstWide`, not `IntConst`.
- **Fix:**
  ```rust
  if matches!(output_type, NodeOutputType::U256 | NodeOutputType::U512) {
      return Err(anyhow!(
          "build_int_const({output_type:?}) not supported - IntConst storage is u128; \
           use build_int_const_wide for U256/U512"
      ));
  }
  ```

### MED: `check_layer_c_function_arg_uniqueness` is not reachability-scoped — false-positive `DuplicateFunctionArg` from zombies

- **Confidence:** 83.
- **Severity:** MED.
- **Where:** `crates/ir/src/validate/layer_c.rs:222-244`.
- **What's wrong:** Iterates `graph.nodes.keys()` without a `reachable` gate.  Every other Layer C per-node check (`check_layer_c_phis`, `check_layer_c_asm_fingerprints`, `check_layer_c_wide_consts`) is reachability-scoped.  If `RedundantPhis` leaves an old `FunctionArg` as a zombie and a new canonical one exists, this raises `DuplicateFunctionArg` on a structurally valid graph.
- **Fix:** Add `reachable: &NodeIdSet` parameter and `if !reachable.contains(node) { continue; }` guard.  Update the call site in `validate/mod.rs`.

### MED: `lift_at()` does not restore `lift_addr` on panic, leaving stale attribution

- **Confidence:** 80.
- **Severity:** MED.
- **Where:** `crates/ir/src/builder/mod.rs:417-426`.
- **What's wrong:** `body(self)` runs between save (`prev = self.lift_addr`) and restore (`self.lift_addr = prev`).  A panic inside `body` leaks `addr` into the outer scope.  The doc comment at lines 409-415 acknowledges this.  `LiftAddrGuard` (`builder/lift_addr.rs:31-35`) handles it correctly via `Drop`.  Currently low-impact (no `catch_unwind` callers), but the API contract is broken.
- **Fix:** Either (a) document the `catch_unwind` constraint and recommend `LiftAddrGuard` directly, or (b) emulate the guard via a local drop-bomb that calls `set_lift_addr` on drop.

## Areas verified correct

- **Graph dedup correctness:** `raw_entry_mut().from_hash()` borrows the cache key without alloc; `evict_cache_entry_if_cacheable` runs before every input mutation.
- **`retain_reachable()` 7-pass algorithm:** all four primary side-tables remapped; `gc_wide_consts()` runs before dedup-cache rebuild so `IntConstWide` payloads carry post-GC ids.
- **Production `.expect()` sites in compact.rs / function.rs:** all bounded by algorithm invariants, all annotated `#[allow(clippy::expect_used)]` with justifications.
- **`asm_fingerprint_exempt` set:** matches CLAUDE.md spec exactly (Entry, InitialMemory, InitialVar, FunctionArg, ControlState, MemPhi, VarPhi, ValuePhi, StackStorePhi).
- **`node_signature.rs` exhaustive match:** covers every `NodeKind` variant.
- **Layer A reachability scoping:** correctly delegated by caller in `validate/mod.rs`.
- **Layer B use-list bidirectionality:** sweeps reachable source outputs and reachable consumer inputs.
- **`Outputs::Index<usize>` / `Inputs::Index<usize>` panics:** documented; production callers (`build_call_with_cc`) always within bounds.

## Summary

| # | Severity | Confidence | Title |
|---|---|---|---|
| 1 | HIGH | 92 | `build_int_const`/`make_int_const` silently accept U512 |
| 2 | MED | 83 | `check_layer_c_function_arg_uniqueness` not reachability-scoped |
| 3 | MED | 80 | `lift_at()` does not restore `lift_addr` on panic |
