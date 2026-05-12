# Round 8 / 1D — `pattern` crate audit

**Branch:** `review/ai2`.  Independent audit; round-7 reviews not consulted.

## Coverage

All 39 source files under `crates/pattern/src/**/*.rs` and all 34 test files under `crates/pattern/tests/**/*.rs` plus `Cargo.toml` inspected fully.

## Findings

### CRITICAL: `Match::get_vn` for `CallOther` with per-node override returns `None` when override length differs from function default

- **Severity:** HIGH (silent wrong-result on every per-CallOther override whose length differs from the function default).
- **Where:** `crates/pattern/src/matcher/match_result.rs:204-226`
- **What's wrong:** `clobber_start` is computed by comparing `total_outputs` against `2 + clobber_len` and `3 + clobber_len`, where `clobber_len = graph.call_other_clobbered.len()` — the **function-level default**.  When a per-CallOther override is present (`call_clobbered_override`), the override may have a different length.  If `override_len != function_default_len`, then `total_outputs` equals `2 + override_len` or `3 + override_len`, neither of which matches the function-default formulae, and the code falls through to `return None` ("shape we don't recognise") — incorrectly returning `None` even when the slot is valid.

  The existing test `get_vn_for_callother_clobber_slot_uses_override_when_set` does not catch this because the override length and the function-default length both equal 1.
- **Verified against:** `crates/ir/src/builder/call.rs::build_call_other_modeled` which sets the per-CallOther override to `implicit_writes_vns` (whose length is the per-CallOther ABI's `implicit_writes` count, not the function default).
- **Fix:**
  ```rust
  let actual_clobber_len = graph.graph
      .call_clobbered_override(node)
      .map_or(graph.call_other_clobbered.len(), |ov| ov.len());
  let clobber_start: u32 = if total_outputs == 2 + actual_clobber_len {
      2
  } else if total_outputs == 3 + actual_clobber_len {
      3
  } else {
      return None;
  };
  ```

### MED: `MemPhiPat` and `ValuePhiPat` not re-exported from `lib.rs`

- **Severity:** MED (API hole; types are returned by ctors but cannot be named externally).
- **Where:** `crates/pattern/src/lib.rs:162-167`
- **What's wrong:** `mem_phi()` returns `MemPhiPat`, `value_phi()` returns `ValuePhiPat`.  The builder re-export list at lines 162-167 names `PhiPat` but omits both new types.  External callers cannot write `fn take_mem_phi(p: pattern::MemPhiPat)`.  They can still call `.into()` to land in `Pat`, but holding the typed builder by name fails.
- **Fix:**
  ```rust
  pub use pat::{
      BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat,
      IfPat, IntBinaryOpPat, LoadPat, MemPhiPat, PhiPat, RetPat, StackStorePat,
      StackStorePhiPat, StorePat, ValuePhiPat,
  };
  ```

### MED: `GuardPat::try_match_node` falls back to default trait impl which silently fails on zero-output nodes

- **Severity:** MED (silent no-match for `ret().when(f)` and similar).
- **Where:** `crates/pattern/src/pat/guards.rs` (no override of `try_match_node`).
- **What's wrong:** `GuardPat` impls `Pattern` but does not override `try_match_node`.  The default trait impl (`crates/pattern/src/pat/traits.rs:77`) iterates `node_outputs` and calls `try_match` per slot.  For zero-output nodes (`Return`), `node_outputs` is empty, the loop body never runs, and the method returns `false`.

  A caller writing `ret().when(predicate)` produces a `GuardPat` whose `try_match_node` is the default — the `Return` node is never matched, silently.  Tests don't cover this combinator + zero-output combo.
- **Verified against:** `CapturePat::try_match_node` in `crates/pattern/src/pat/any.rs:90-116` correctly handles zero-output nodes by delegating to `match_node_id`.  `GuardPat` should mirror that pattern, BUT the `GuardFn::Output` signature requires a `NodeOutputId` which doesn't exist for zero-output nodes.
- **Fix:** Either:
  1. Document the limitation in `GuardPat` doc-string and reject `.when(f)` on `Pat`s whose root has no value output; OR
  2. Override `try_match_node` to delegate `match_node_id` and run the predicate against a synthetic "no value output" sentinel, calling only `Bindings`-flavored predicates.

## Areas verified correct

- **Commutativity table** (`crates/pattern/src/matcher/commutativity.rs`): all entries algebraically commutative — `Add`, `Mul`, `And`, `Or`, `Xor` (int + bool), `FloatAdd`/`Mul` (IEEE 754), `IntCmpOp::{Equal, Carry, Scarry}`, `FloatCmpOp::Equal`.  `IntCmpOp::Sborrow` correctly NOT listed (signed subtraction is non-commutative).
- **Lift-time canonicalisation aliases**: all 6 emit the lowered shape `pcode-lift::value_lifter` produces.
- **`Matcher::new` vs `Matcher::for_graph`**: split is correct; `rewrite_rule` uses `for_graph` (no BFG dependency).
- **`RewriteCtx::new`/`for_built`**: both present, HRTB lifetimes correctly expressed on `boxed_rule` / `apply_rules_in_order`.
- **`*_any` empty-set vacuous failure**: confirmed for `int_const_any_of([])`, `CallPat::at_any([])`, `StackStorePat::offset_any([])`.
- **`phi()` / `mem_phi()` / `value_phi()`**: each uses `KindSpec::variant` keyed on the right discriminant.
- **`int_const_wide` sentinel**: discriminant gate via sentinel `WideConstId(0)`, post-match closure compares actual values.
- **`Bindings::mark`/`restore`** backtracking: append-only `Vec` + `truncate` is correct.
- **`find_all_requirements` cross-product**: O(N₁·N₂); early-exit when `acc` becomes empty.  No premature pruning beyond that — confirmed (could be faster but not wrong).
- **`IfPat` direct-layout-only**: depends on `IfCondInversion` pre-canonicalisation.

## Summary

- **1 HIGH** — `get_vn` CallOther override length mismatch.
- **2 MED** — `MemPhiPat`/`ValuePhiPat` re-export hole; `GuardPat::try_match_node` zero-output silent failure.
