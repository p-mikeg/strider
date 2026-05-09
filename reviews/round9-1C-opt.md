# Round 9 / 1C — `opt` crate audit

**Branch:** `review/ai3`. Independent audit; round-7 / round-8 not consulted.

## Critical

None above the 80-confidence threshold.

## Important

### Issue 1 — `Extend(SignExtend, IntConst)` arm produces wrong dispatch target for sign-negative narrow constants

**Confidence:** 82.

**Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:252-269`.

`NodeKind::Extend(_)` arm handles both `ZeroExtend` and `SignExtend` identically with `(*k) as u64`. For `SignExtend`, the inner `IntConst`'s `u128` value holds the narrow unextended bits. Example: `Extend(SignExtend, IntConst(0xFFFF_FFFF), U32→U64)` has `k = 0xFFFF_FFFF`; `(*k) as u64 = 0xFFFF_FFFF`; correct sign-extended target is `0xFFFF_FFFF_FFFF_FFFF`.

**Mitigating:** `ConstantFold` rule 6 folds `SignExtend(IntConst(v))` correctly using `get_signed_int` before the classifier runs in production pipelines. `FunctionBuilder::extend_if_needed` also folds eagerly. The shape is unlikely to reach the classifier in production.

**Residual risk:** Direct callers building `Extend(SignExtend, ...)` manually and skipping ConstantFold get a silently wrong `Single(wrong_addr)`. The unit test only exercises ZeroExtend.

**Fix:** Branch on the extend op:
```rust
NodeKind::Extend(op) => {
    if let Some(&inner) = inputs.first()
        && let NodeKind::IntConst(k) = graph.kind_of_output(inner)
    {
        let target = match op {
            ExtendOp::ZeroExtend => (*k) as u64,
            ExtendOp::SignExtend => {
                let in_ty = graph.output_kind(inner).as_value()?;
                in_ty.get_signed_int(*k)? as u128 as u64
            }
        };
        return Some(ResolvedTargets::Single(target));
    }
    None
}
```

### Issue 2 — `try_apply_rule` discards `replace_all_uses` return value, producing spurious `Changed` on dead-node matches

**Confidence:** 82.

**Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:150-153`.

`replace_all_uses` returns `Ok(false)` when `root_out` has zero consumers (dead/zombie node). Return value discarded; `try_apply_rule` returns `Ok(true)` (Changed) regardless. Wastes one extra fixed-point iteration. Currently mitigated because the pass uses `for node in function.preorder()` (reachability traversal), so dead nodes rarely visited. Contract still broken.

**Fix:**
```rust
let replaced = function.graph.replace_all_uses(root_out, new_out)?;
Ok(replaced)
```

## Special Focus Verified Correct

1. **`flag_cmp_canonicalize` rule table consistency** — binary uses `rule()` with `Some(c)`; unary Thumb uses `rule_unary()` with `None`. Consistent.
2. **`IfCondInversion` fingerprint absorption** — `extend_asm_fingerprint_from(inner_node, bool_neg_node)` at line 112 before `update_input` at line 113.
3. **`KnownBits::analyze` `SecondaryMap` correctness** — `Kb::default()` ones=0, zeros=0; `all_known` returns false; absent entries never trigger false-positive folding.
4. **`StackLoadForward` BE narrow path** — `create_node_attributed(..., &[load])` for `ShiftRight` and `Truncate` intermediates.
5. **`decompose_sp` memoization formula** — `offset.wrapping_sub(*accum_at_level)` correctly computes leaf-relative offset.
6. **`decompose_sp` cycle-detection rollback** — only spine nodes set during this call stack are rolled back.
7. **`apply_link_register` unconditional `remove_node_input`** — line 67 fires after `matches!` guard.
8. **`step_through_stack_store_phi` empty-offsets returns MayAlias** — fail-safe correct.
9. **`apply_in_place_edits` zombie fix** — walks `unresolved_anchors` (pinned list), no preorder.
10. **`apply_tail_call` fingerprint absorption** — `IntConst`, `Call`, `Return` all absorb `placeholder_fingerprint`.

## Emphasis A — Asm-Fingerprint Contract Walk

All passes verified correct: `ConstantFold` (via `rewrite_rule`), `KnownBits` (line 469), `FlagCmpCanonicalize` (each RHS builder), `IfCondInversion` (line 112), `RedundantPhis` (3 sites), `DeadBranchElimination` (line 114), `StackLoadForward` (line 127 + intermediates), `LoadReadOnly` (`after_replace`), `IndirectBranchResolve` (`apply_tail_call`). `FunctionArgDetect` does not absorb but `FunctionArg` is exempt by design.

## Emphasis B — Simplification Opportunities

No dead passes. All registered passes reachable from at least one pipeline variant. Visibility consistent. No high-impact inlining candidates beyond clippy enforcement.

## Coverage

All `crates/opt/src/**/*.rs` (mod, impl, tests) and all `crates/opt/tests/*.rs` and `crates/opt/benches/*.rs` files read in full.
