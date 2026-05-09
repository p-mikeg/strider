# Round 9 — Ask-8 R4: Boundary / Edge Case audit

**Branch:** `feature/ai`. Independent audit; 13 boundary categories examined across `opt`, `ir`, `cfg`, `pcode-lift`, `pattern`, `reader`, `strider`.

## Summary

**No issues with confidence ≥ 80 found.** All 13 boundary categories pass. The codebase applies defensive patterns consistently.

## Categories examined

### 1. Empty inputs (`&[T]` assumed len ≥ 1)

- `opt/src/redundant_phis/mod.rs:50-53` — `inputs.is_empty()` guard before `inputs[0]`. Safe.
- `opt/src/sp_expr.rs:412` — `inputs.len() < 2` guard before `bases[0]`. Safe.
- `opt/src/indirect_branch_resolve/classify.rs:159-169` — empty `targets` returns `None`. Safe.
- `opt/src/indirect_branch_resolve/classify.rs:235-249` — `inputs.first()` returns Option. Safe.

### 2. Single-element inputs (phi with 1 predecessor)

`redundant_phis/mod.rs` single-ctrl collapse: position lookup safe; arity invariant enforced by Layer C. `stack_load_forward/mod.rs::realize`: `windows(2)` vacuously true for 1 element; `first().copied()` correct.

### 3. Max-arity / u64::MAX addresses

`cfg/src/cfg/builder/region_builder.rs::next_pcode_addr` uses `checked_add`. `reader/src/lib.rs::MemRegion::new` uses `checked_add`/`checked_sub`. `opt/src/indirect_branch_resolve/jump_table.rs::read_table_entries` uses `checked_mul`/`checked_add`. All safe.

### 4. NaN / Infinity / Signed-zero in float ops

`opt/src/constant_fold/eval_float.rs`: all use Rust f32/f64 IEEE 754. `0.0/0.0 → NaN`, NaN comparisons return false correctly, `sqrt(-x) → NaN`, signed-zero `+0.0 == -0.0 → true`. F80 returns `None` (unfolded), correctly documented.

### 5. INT_MIN sign-extension and negation

`opt/src/constant_fold/rules.rs:416`: `wrapping_neg` of INT_MIN returns INT_MIN, masked to type width. Correct two's-complement.
`eval_int.rs:75-93,102-119`: Sdiv/Srem detect `int_min && -1` and return `None`, leaving unfolded.
`eval_int.rs:60-67`: SShiftRight INT_MIN by `r >= bits` returns mask (all-ones) when sign bit set. Correct.

### 6. Address u64::MAX in bounded lift

`next_pcode_addr` uses `checked_add` — terminate region cleanly on overflow. Bounded `is_branch_tail_call_nocheck` correctly classifies u64::MAX targets as TailCall.

### 7. Instructions at `start` / `start + fn_max_size - 1` boundary

`addr == start_addr`: BTreeMap `range(..=addr).next_back()` finds entry exactly. `addr == start + fn_max_size - 1`: decoded; fall-through OOB check fires before next decode. Safe.

### 8. NodeId(u32::MAX) / arena exhaustion

cranelift_entity uses u32 keys. ~800 GB RAM required for 4 billion nodes — physically unreachable. Accepted platform limitation, no defensive check needed.

### 9. Empty `stack_phi_offsets` for `StackStorePhi`

`opt/src/sp_expr.rs::step_through_stack_store_phi:131-160`: `offsets.is_empty()` guard returns `Alias::MayAlias`. Confirmed fixed in round 8; test pin in place.

### 10. Empty multi-arg sets

`int_const_any_of([])`, `at_any([])`, `offset_any([])`: all vacuously fail (any-on-empty-iterator → false). Confirmed by Python tests.

### 11. Very deep recursion

All graph walkers iterative: `decompose_sp` (worklist), `stack_load_forward::probe` (heap PhiFrame stack + cycle guard), `walk_control_for_if_bound_iter` (JoinNext frames), `walk_graph` (worklist). `same_value` has budget-64 cap. No stack overflow risk.

### 12. Empty CFG / single-region CFG

Single-region: RedundantPhis on `(None, None)` arm: `simplified = false`, no panic. `add_region` accepts empty instruction list (documented for OOB-CondBranch case).

### 13. `vn_mask` widths at boundaries

| Width | Result | OK |
|-------|--------|----|
| 1 byte (AL) | `0xFF` | ✓ |
| 2 (AX) | `0xFFFF` | ✓ |
| 4 (EAX) | `0xFFFF_FFFF` | ✓ |
| 8 (RAX) | `0xFFFF_FFFF_FFFF_FFFF` | ✓ |
| 10 (F80) | `(1<<80)-1` | ✓ |
| 16 (XMM) | `u128::MAX` | ✓ |
| 32 (YMM) | `u128::MAX` (degraded, documented) | ✓ |
| 64 (ZMM) | `u128::MAX` (degraded, documented) | ✓ |

Sub-register aliasing within >16-byte containers correctly returns `Err`. BE shift formula `container.size - reg.size - (reg.addr_off - container.addr_off)` cannot underflow given `find_largest_fitting_register`'s geometric containment check.

## Coverage

| Crate | Files reviewed |
|-------|---------------|
| opt | constant_fold/{eval_float, eval_int, rules}, sp_expr, redundant_phis, stack_load_forward, indirect_branch_resolve/{classify, jump_table, stack_array}, known_bits |
| ir | iterators, walk, validate |
| cfg | cfg/builder/region_builder |
| pcode-lift | vn_io |
| pattern | pat/ctor/{wildcards, call, stack_store} |
| reader | lib |
| strider | orchestrator (indirect-branch loop), indirect_resolve/classify |

**No HIGH or MED issues. Below-threshold note: `redundant_phis:75,97` `inputs[j+1]` direct indexing relies on Layer C arity invariant rather than runtime check. Triggering a panic would require bypassing both builder and validator.**
