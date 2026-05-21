# Round 13 — 1C: `opt` crate audit

Branch: `review/ai7` · Scope: `crates/opt/src/**` (~51 .rs), `crates/opt/tests/**`, `Cargo.toml`, `README.md`.

## Verdict

**1 MED finding (production `expect()` in `FlagCmpCanonicalize`); all other focus areas clean.**

## Findings

### OPT-FCC-1 — Production `expect()` panics in `FlagCmpCanonicalize::try_apply_rule`
- **Severity:** MED (confidence 82)
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:134-143`
- **What's wrong:** Two `.expect()` calls (each wrapped in `#[allow(clippy::expect_used)]`) execute on every candidate node on every pipeline iteration.  The invariant argument is that `match_at` returning `Some(m)` guarantees the LHS-captured `Capture`s resolve to value outputs.  Correct for the current 9-rule table built by `build_rules()`.  But the `#[cfg_attr(test, allow(clippy::...))]` at `lib.rs:38-46` only silences Clippy diagnostics in test builds — it does NOT prevent panic in production.  A future rule misassigning `lhs_capture`/`rhs_capture` to a Capture not actually placed in the LHS pattern panics rather than propagating `Err`.
- **Fix:** Replace both `.expect(...)` with `.ok_or_else(|| anyhow!(...))?`.  The function already returns `Result<bool>`, so no API change.
- **Regression test:** Construct a `Rule` whose `lhs_capture` was never placed in the LHS pattern; call `try_apply_rule` and assert it returns `Err(...)` rather than panicking.

## Categories verified clean

✓ **Round-12 `fg → ctx` rename** — every `impl Optimizer` signature uses `ctx: &mut pattern::RewriteCtx<'_>`.  No stale `fg` parameter names remain.

✓ **`OptimizerOnBuilt → Optimizer` collapse** — `OptimizerOnBuilt` does not appear anywhere.  Blanket `impl<T: Optimizer> OptimizerRaw for T` at `pipeline.rs:166-174`.

✓ **TY-3 `after_replace` infallible** — returns `Self`.  The internal `.expect()` is annotated as a by-construction invariant (cursor invariant upheld by `replace_all_uses`'s while-guard) and `#[allow]`-suppressed.

✓ **TY-2 `ResolvedTargets::multiple`** — returns `Option<Self>` (`mod.rs:106`).

✓ **H3 `classify_jump_table` signature** — `(ctx, anchor_output, rom, known)`; no `_link_register_vn`.

✓ **`IfCondInversion` canonicalisation invariant** — `invert()` redirects cond input from `BoolNeg(X)` to `X` absorbing the BoolNeg's fingerprint into `inner_node`, then swaps true/false consumer input IDs.  Runs after `ConstantFold`.

✓ **`FlagCmpCanonicalize` asm-fingerprint propagation** — every `build_rhs` calls `extend_asm_fingerprint_from(new_node, root)` on every intermediate node it creates.  Both binary and unary rule shapes fully covered.

✓ **`KnownBits` wide-input bail** — `u64_type_mask()` returns `None` for U80/U128/U256/Bool/floats; every arm either returns `Ok(None)` or gates on `?` from `u64_type_mask`.  No silent truncation.

✓ **`KnownBits` large-literal-shift** — `ShiftLeft`/`ShiftRight` arms return `(ones:0, zeros:type_mask)` when `rhs_kb.ones >= bit_width`, matching Sleigh's `>= bit_width → 0` semantics.

✓ **`StackStoreDetect` SP-decompose** — `decompose_sp` handles `IntBinaryOp::And` with a constant mask by producing `Terminal { base: <And output>, offset: 0 }`; alignment chains track offset correctly from the opaque base.

✓ **`StackLoadForward` partial-overlap endianness** — `realize::Narrow`: LE uses `Truncate(data)`, BE uses `Truncate(ShiftRight(data, (store_size - load_size) * 8))`.  Intermediate nodes use `create_node_attributed(..., &[load])` so fingerprints land on every chain node.

✓ **`StackLoadForward` `StackStorePhi` disjoint arm** — `probe` checks each per-predecessor offset; empty offsets return `MayAlias` (sound conservative).

✓ **`LoadReadOnly` size > 8 guard** — explicit check before `ReadOnlyMemory::read`; silently skips wide loads (U80/U128/U256/U512) rather than truncating.

✓ **`RedundantPhis` + `DeadBranchElimination` zombie interaction** — `detach_unreachable_nodes` not escalated to `Changed` (result discarded).  DBE leaves the If attached when `dead_subgraph_escapes`, preventing Layer A violations.

✓ **`indirect_branch_resolve` classifier shape coverage** — `classify_anchor_with_rom_and_sp` covers all five arms (`IntConst` → Single, `InitialVar(lr)` → LinkRegister, `ValuePhi` of consts → Multiple, `Load` → jump-table + stack-array fallthrough, `IntBinaryOp::And` → ARM Thumb interworking).  All fail-closed to `None`.

✓ **Jump-table `bound_via_predecessor_if` on_true direction** — output_idx == 0 maps to true branch.  `bound_from_if_condition` returns `None` immediately for `!on_true_branch`.

✓ **`FunctionArgDetect` Result discipline + fingerprint** — `mem_chain_is_dirty` returns `Err` on malformed MemPhi / Call inputs; register-arg path absorbs `InitialVar` fingerprint into `FunctionArg`.

✓ **Production panics** — only the two `expect` in `flag_cmp_canonicalize/mod.rs` (reported above) plus the documented `pipeline.rs:after_replace` expect (justified) outside `#[cfg(test)]`.  No `unwrap`/`panic!`/`unreachable!`/`assert!` in production code.

## Coverage table

| Focus area | File(s) checked | Verdict |
|---|---|---|
| Per-pass rewrite/no-op/idempotency | All 11 pass files | clean |
| `FlagCmpCanonicalize` correctness | `flag_cmp_canonicalize/mod.rs` | OPT-FCC-1 (MED) |
| `IfCondInversion` invariant | `if_cond_inversion/mod.rs` | clean |
| `KnownBits` soundness | `known_bits/mod.rs` | clean |
| `StackStoreDetect` SP-decompose | `stack_store/detect.rs`, `sp_expr.rs` | clean |
| `StackLoadForward` partial-overlap / phi | `stack_load_forward/mod.rs` | clean |
| `LoadReadOnly` size guard | `load_readonly/mod.rs` | clean |
| `RedundantPhis` + DBE zombie interaction | `redundant_phis/mod.rs`, `dead_branch/mod.rs` | clean |
| `indirect_branch_resolve` classifier | `classify.rs`, `jump_table.rs`, `stack_array.rs`, `inplace.rs` | clean |
| `FunctionArgDetect` Result / fingerprint | `function_args/mod.rs` | clean |
| `Optimizer`/`OptimizerRaw` trait split | `pipeline.rs` | clean (R12 confirmed) |
| `after_replace` infallible (R12 TY-3) | `pipeline.rs:41-61` | clean |
| `ResolvedTargets::multiple` Option (R12 TY-2) | `mod.rs:106` | clean |
| `classify_jump_table` param (R12 H3) | `jump_table.rs:61-66` | clean |
| Production panics | All non-test .rs files | 1 MED finding |
