# Round 9 / 1A — `ir` crate audit

**Branch:** `feature/ai`. Independent audit. Trust model: no reliance on doc comments or CLAUDE.md.

## Critical

None. All core invariants verified sound:

- **Dedup cache**: `hash_borrowed_key` matches owned-tuple Hash; `raw_entry_mut().from_hash()` zero-alloc on hit; `evict_cache_entry_if_cacheable` called before every mutation.
- **Asm-fingerprint contract**: `extend_asm_fingerprint` strictly additive; `_from` has self-extension guard; `create_node_attributed` unions on cache hit; `FunctionBuilder::create_node` auto-attributes from `lift_addr`.
- **Validator layers**: A reachability-scoped, B reachable-source-restricted, C mixed (intentional uniqueness checks global; phi/arg/fingerprint reachability-gated).
- **`compact.rs` `expect()`**: `graph_walk_succs` backward-data walk guarantees reachable inputs' producers are reachable.
- **`WideConstStorage`**: `intern_wide_const` deduplicates; `gc_wide_consts` rebuilds side-table post-compaction.
- **`build_int_const` / `make_int_const`**: explicitly reject U256/U512.

## Important

### I1 — `LiftAddrGuard` exported `pub` with zero instantiations anywhere

**Confidence:** 95.

**Where:** `crates/ir/src/lib.rs:60` and `crates/ir/src/builder/lift_addr.rs:16-35`.

`LiftAddrGuard` re-exported from `lib.rs:60` but `LiftAddrGuard::set` has zero call sites in the codebase. Strider's per-region driver explicitly avoids it (would conflict with sibling field accesses) and uses `set_lift_addr(Some) … set_lift_addr(None)` pair instead.

**Fix:** Remove from `pub use` line. Keep `pub(crate)` or make `pub(super)` pending future caller. Alternatively, document the borrow-checker constraint and `#[doc(hidden)]` to suppress.

### I2 — `read_variable_optional` is `pub` but only called from sibling builder module

**Confidence:** 85.

**Where:** `crates/ir/src/builder/vars.rs:17`.

Only call site: `crates/ir/src/builder/call.rs:118`. No external crate uses it.

**Fix:** Change to `pub(super)` or `pub(crate)`. Non-breaking.

### I3 — `lift_at` does not restore `lift_addr` on panic; documented `LiftAddrGuard` mitigation is unreachable

**Confidence:** 80.

**Where:** `crates/ir/src/builder/mod.rs:417-426`.

`lift_at` saves `prev`, runs `body`, restores `lift_addr = prev` on normal return only. Panic in `body` leaves `lift_addr = addr` for rest of thread's execution. Doc points to `LiftAddrGuard` as the RAII fix — but `LiftAddrGuard::set` is never called (I1).

**Fix option A:** Strengthen doc to say `lift_at` must not be used when body can panic; add `#[track_caller]` `debug_assert!(!std::thread::panicking())` guard.

**Fix option B:** Add a local drop-bomb:
```rust
struct Restore<'a>(&'a mut Option<u64>, Option<u64>);
impl Drop for Restore<'_> { fn drop(&mut self) { *self.0 = self.1; } }
```

## Low-confidence notes

- **S1 — `update_input` cache gap (conf 75)**: `update_input(in0, c_out)` evicts old key but doesn't insert new one. Subsequent `create_node` with same inputs creates duplicate. Known intentional gap to avoid updating all cache keys for partial rewrites. Validator doesn't check.
- **S2 — `check_layer_c_control_state` non-empty zombie path (conf 75)**: Empty-input path correctly reachability-gated; non-empty path not gated. Cannot occur via public builder API but theoretically possible via `test_only_*` helpers.

## Deletion candidates

1. `ir::LiftAddrGuard` — zero external callers (I1).
2. `FunctionBuilder::read_variable_optional` — `pub` → `pub(super)` (I2).

## Coverage

Full read of: lib, graph/{mod,store,uses,access,compact,tests}, validate/{mod,layer_a,layer_b,layer_c}, builder/{mod,lift_addr,vars,coerce}, function, walk, wide_const, ops/consts. Partial: builder/{nodes,call,tests}, validate/tests, node/kind, node_signature, region. Skipped (low risk): ops/{builder,mod,op_kinds,rewrite}, iterators, error, test_utils, dot/*, tests/*.
