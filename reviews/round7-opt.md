# Round 7 — opt Crate Audit

Independent review (code-only) of `crates/opt/src/**` (all passes + pipeline scaffolding + sp_expr).

---

## CRITICAL

### CRIT-1 — `find_stack_stored_value_at_offset` recursive without depth bound — HIGH (conf 85)
- **Where:** `crates/opt/src/stack_load_forward/mod.rs:489-549`
- **Evidence:**
  - Recurses on disjoint `StackStore` (lines 526-528) and passthrough `Store` (lines 539-542).
  - `walk_memo` insertion happens at line 548 *after* recursive calls return; mid-recursion lookups are cache misses, so the memo is not a cycle/depth guard.
  - The sibling function `probe` was already converted to iterative (`Vec<PhiFrame>` work-stack at line 205); this one was missed.
  - Pathological binary with deep stack-prologue store chains (~10k stores in a region) overflows the 8 MB default Rust stack.
- **Fix:** Mirror `probe`'s iterative form: convert to `Vec<NodeOutputId>` worklist, or add a `MAX_CHAIN_DEPTH = 1024` guard with `Ok(None)` early-return.

---

## IMPORTANT

### IMP-1 — Production `expect()` in `flag_cmp_canonicalize` — MED (conf 82)
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:128` (also lines 161, 175 — those are locally safe, freshly-built single-output nodes)
- **Evidence:** `m.output(rule.cap_a).expect("Capture a must bind to a value output")`. Depends on `pattern::Matcher` cross-crate contract. `#[allow(clippy::expect_used)]` makes it firing in release builds.
- **Fix:** Use `?` propagation: `.ok_or_else(|| anyhow::anyhow!("cap_a not bound in successful flag-tree match"))?`.

### IMP-2 — Python `PipelineState::from_default()` no compile-time sync — MED (conf 80)
- **Where:** `crates/strider-py/src/opt.rs:55-71`
- **Evidence:** Manually reconstructs `opt::default_pipeline()` by listing each pass. Adding a Rust-side pass silently desyncs Python.
- **Fix:** Add a Rust-side test asserting `opt::default_pipeline().optimizer_count() == N`; OR expose `optimizer_names()` on `OptimizerPipeline` and assert from a Python integration test.

---

## MINOR

### MIN-1 — `KnownBits` does not propagate through `Extend(SignExtend)` — LOW (conf 80)
- **Where:** `crates/opt/src/known_bits/mod.rs:290`
- **Evidence:** `Extend(ZeroExtend)` handled at line 254. `SignExtend` falls to wildcard `_ => return Ok(None)`. Sound (returns "unknown") but missed optimization.
- **Impact:** Jump-table index-bound analysis (`bound_via_predecessor_if`) misses bounds on `SignExtend(small_u8)` indices on MIPS / ARM Thumb.
- **Fix:** When the input's MSB is known 0, propagate `zeros = kb.zeros | (type_mask ^ input_mask)`. Code sketch in agent report.

### MIN-2 — "tier-2" classifier naming in opt comments — LOW (conf 80)
- **Where:**
  - `sp_expr.rs:25` (public type doc)
  - `stack_load_forward/mod.rs:406, 415, 458, 469, 487`
  - `indirect_branch_resolve/jump_table.rs:1`
  - `indirect_branch_resolve/stack_array.rs:1`
- **Fix:** Replace "tier-2 indirect-branch classifier" with the concrete classifier name (`stack-array indirect-branch classifier` / `jump-table classifier`). This matches the actual module names.

---

## Verified-Correct (no issues found)

| Pass | Rating | Notes |
|------|--------|-------|
| **ConstantFold** | CLEAN | Shift semantics (≥width → 0) match Sleigh's `OpBehaviorIntLeft::evaluateBinary`. Signed-div guards prevent `INT_MIN/-1` overflow. F80 returns None. |
| **KnownBits** | LOW (MIN-1) | Otherwise sound. |
| **FlagCmpCanonicalize** | MED (IMP-1) | 9 flag rules verified semantically correct. |
| **IfCondInversion** | CLEAN | Swap logic correct; double-BoolNeg handled cross-pass via ConstantFold. |
| **RedundantPhis** | CLEAN | Stale `stack_phi_offsets` entries are harmless (NodeId no longer reachable). |
| **DeadBranchElimination** | CLEAN | `dead_idx + 1` phi-arity arithmetic correct (slot 0 is phi-token). |
| **LoadReadOnly** | CLEAN | OOB reads return None; no double-endianness swap. |
| **StackStoreDetect** | CLEAN | SP decomposition correct via sp_expr; phi token handling verified. |
| **StackLoadForward `probe`** | CLEAN | Iterative; stack-safe. |
| **StackLoadForward `find_stack_stored_value_at_offset`** | CRIT-1 | Recursive — see above. |
| **IndirectBranchResolve** | CLEAN | Both classifiers stack-safe; `MAX_TABLE_ENTRIES = 4096`. |
| **CallStackArgCollect** | CLEAN | Gap-free prefix logic correct. |
| **FunctionArgDetect** | CLEAN | Sub-register fallback + mem-chain dirtiness verified. |

### Pipeline scaffolding
- `MAX_ITERS = 1024` cap with `anyhow::bail!` — no unbounded loop.
- Stable / destructive subsets correctly partitioned.
- Validate called at end with proper error aggregation.

### CallOtherElide cleanup
- Zero matches for `CallOtherElide` or `NO_OP_USER_OPS` in `crates/opt/src/`. Comments at `lib.rs:149-151, 181-183` are accurate historical context. **No stale references.**

### TODO/FIXME/HACK
- None in `crates/opt/src/**/*.rs`.

### Python parity
- All 11 passes wrapped: ConstantFold, KnownBits, FlagCmpCanonicalize, IfCondInversion, RedundantPhis, DeadBranchElimination, LoadReadOnly, StackStoreDetect, StackLoadForward, FunctionArgDetect, CallStackArgCollect.
- `from_default()` / `from_stable_default()` / `from_destructive_default()` all match Rust pipeline funcs (modulo IMP-2 sync risk).

### Asm-fingerprint contract
- `FlagCmpCanonicalize` calls `extend_asm_fingerprint_from(new_node, root)` for every `build_int_cmp` / `build_bool_neg`. Superset contract preserved.

---

## Top Findings

1. **CRIT-1 (HIGH)** Stack overflow risk in `find_stack_stored_value_at_offset` — fix by mirroring `probe`'s iterative form.
2. **IMP-1 (MED)** Production `expect` in `flag_cmp_canonicalize.rs:128` violates "no panic" policy.
3. **IMP-2 (MED)** Python pipeline composition can silently desync from Rust.
4. **MIN-1 (LOW)** KnownBits SignExtend missing — affects jump-table classifier on MIPS/ARM Thumb.
5. **MIN-2 (LOW)** "tier-2" naming in public sp_expr.rs doc + multiple comments.
