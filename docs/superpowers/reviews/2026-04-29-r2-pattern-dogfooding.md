# R2 — pattern dogfooding pass

## Executive summary

- **Files audited**: 18 across `crates/opt/src/{indirect_branch_resolve,
  constant_fold, dead_branch, load_readonly, redundant_phis, stack_store,
  stack_load_forward, function_args, call_other_elide, sp_expr.rs}` and
  `crates/strider/src/{indirect_resolve_tier2/{orchestrator, inplace},
  strider/insn/control.rs}`. Test files in
  `crates/strider/tests/{tier2_*, jump_table_lifting,
  manual_rewrite}.rs` and `tests/common/tier2_helpers/` were spot-checked
  for migration-worthy matchers.
- **Refactors landed**: 1 (R2-1).
- **Refactors considered + rejected**: 11 (see below).
- **LOC delta**: +215 / -47 (one refactor commit). The +215 includes the
  6 new characterization tests pinning the refactored function's
  contract; the production-code delta alone is roughly +30 / -41.
- **Final test count**: pre 2854 / 0 / 26 → post 2860 / 0 / 26 (+6
  characterization tests, all passing).
- **Clippy**: clean across the workspace at every commit.

## Methodology

Each candidate was audited against R2's three acceptance criteria —
fewer LOC, fewer footguns, better composability — and against the
correctness gates the prompt called out:

- **Behavior preservation**: verified that the new pattern matches the
  same shapes as the old hand-rolled match, including both operand
  orderings of commutative ops.
- **Characterization first**: where existing coverage didn't pin the
  refactored function's contract, a characterization test was written
  *before* touching the production code, ensuring the refactor cannot
  silently change behavior. The R2-1 characterization run flushed out
  one would-be test bug (an `IntConst` inner anchor confused the prior
  code's commutative probe — fixed by switching the test fixture to a
  Load-anchored value).
- **Verification per commit**: `cargo test --workspace` (2854 → 2860)
  and `cargo clippy --workspace --all-targets -- -D warnings` (clean)
  before each commit.

The audit's narrowing principle: many obvious migration targets had
*already* been done in prior rounds (Phase 5, F5 dogfooding pass, R1).
The jump-table shape match in `match_jump_table_shape`,
`extract_idx_and_stride` in `stack_array.rs`, every entry in
`constant_fold/rules.rs`, and the rewrite-rule machinery in
`build_reassoc_and_mask_rules` are already pattern-based. R2's net
yield is therefore narrower than a fresh-codebase pass would produce.

## Refactors landed

### **R2-1** `strip_target_mask` migrates to `pattern::and` / `pattern::or`

- File: `crates/opt/src/indirect_branch_resolve/stack_array.rs:131-205`
- Commit: `c99805a`

**Before** (50 lines, hand-rolled commutative-operand checks):

```rust
match graph.node_kind(producer) {
    NodeKind::IntBinaryOp(IntBinaryOp::And) => {
        if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(producer) {
            if let Some(m) = graph.int_const_val(rhs) {
                mask &= m; current = lhs; continue;
            }
            if let Some(m) = graph.int_const_val(lhs) {
                mask &= m; current = rhs; continue;
            }
        }
        break;
    }
    NodeKind::IntBinaryOp(IntBinaryOp::Or) => {
        if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(producer) {
            let or_const = graph
                .int_const_val(rhs)
                .map(|c| (c, lhs))
                .or_else(|| graph.int_const_val(lhs).map(|c| (c, rhs)));
            if let Some((or_c, other)) = or_const {
                if or_c & mask == 0 { current = other; continue; }
            }
        }
        break;
    }
    _ => break,
}
```

**After** (~25 production lines using `pattern::and` / `pattern::or`):

```rust
let c_var = IntVar::new();
let other_var = Var::new();
let and_p = and_pat(any_int_const(c_var), var(other_var));
if let Some(m) = matcher.match_at(producer, &and_p.into())
    && let (Some(c128), Some(other)) = (m.get_int(c_var), m.get(other_var))
{
    mask &= c128 as u64; current = other; continue;
}
// (or arm mirrors the same shape with `or_pat` and the
// `or_c & mask == 0` predicate.)
break;
```

**Why this was a net win**:

- **Fewer footguns**: the prior code reinvented commutative-operand
  matching by hand (`int_const_val(rhs)` then `int_const_val(lhs)`).
  `pattern::and` / `pattern::or` are auto-commutative and capture the
  non-const operand directly, eliminating the manual fallback chain.
- **Better composability**: the And/Or layer-strip logic is now stated
  declaratively in pattern form, so a future round adding (e.g.) a
  `Truncate` or `Extend` outer wrapper need only add another pattern
  match — not duplicate the manual `node_inputs_exact` + `int_const_val`
  scaffolding.
- **Production-code LOC**: ~50 down to ~25 (excluding the new comments
  documenting the pattern equivalence). Net production-code delta is
  positive after counting comments, but the comments encode the
  soundness reasoning that R1 explicitly prized — they are not
  noise.

**Behavior-equivalence evidence**:

- 6 new characterization tests, all in
  `crates/opt/src/indirect_branch_resolve/stack_array.rs` (lines
  ~635-790):
  - `strip_target_mask_no_wrapper_returns_all_ones`
  - `strip_target_mask_and_with_const_rhs_strips_one_layer`
  - `strip_target_mask_and_with_const_lhs_strips_one_layer`
    (commutative; would have caught the prior `int_const_val(rhs)`-only
    code)
  - `strip_target_mask_arm_thumb_or_then_and_strips_both_layers`
    (canonical ARM-Thumb interworking)
  - `strip_target_mask_or_overlapping_mask_stops_at_or` (the
    fail-closed branch when an OR's set bits overlap the surviving
    mask)
  - `strip_target_mask_nested_ands_compose_via_intersection`

  All 6 were run *first* against the pre-refactor implementation and
  passed, then *again* after the refactor — same results.
- The 3 existing integration tests
  (`classify_stack_array_two_targets_resolves`,
  `classify_stack_array_returns_none_on_non_indexed_load`,
  `classify_stack_array_returns_none_on_unbounded_idx`) still pass.
- `cargo test --workspace`: 2860 / 0 / 26.

**Confidence behavior is preserved**: high. The characterization tests
pin both operand orderings explicitly, the ARM-Thumb interworking
shape (the load-bearing real-world case), the no-strip OR overlap, and
nested-And mask composition. The signature change (from
`(graph: &Graph, ...)` to `(fg: &BuiltFunctionGraph, ...)`) is
mechanical and compile-checked — there's no in-the-wild caller
holding a bare `&Graph`, since the only caller is
`classify_stack_array(fg, ...)` which already has `fg`.

## Refactors considered + rejected

For each, the file:line, what it does, and why a migration would
not (yet) pay off.

1. **`crates/opt/src/load_readonly/mod.rs:60-93`** —
   Load-with-IntConst-address detection. **Reject**: pre-refactor is 5
   lines (`let NodeKind::Load(space) = kind else { continue; }; let
   inputs = ...; if inputs.len() < 2 { continue; }; let addr_input =
   inputs[1]; let Some(addr) = function.int_const_val(addr_input) else
   { continue; };`). Pattern equivalent (`load().addr(any_int_const(v))`
   + `find_all` + extract `space` from `m.root`'s NodeKind) is the same
   length, and we still need `space` extraction since the pattern
   matches *any* space.

2. **`crates/opt/src/dead_branch/mod.rs:32-44`** —
   `If`-with-`BoolConst` detection. **Reject**: 4 lines pre-refactor
   (`if !matches!(*kind, NodeKind::If) { return ... }; ...; let Some(b)
   = bool_const_val(...) else { return ... };`). Pattern would need
   `if_node().cond(bool_const(...))` but the rewrite needs
   `[ctrl_true, ctrl_false]` outputs, the live/dead disambiguation,
   and the cond-side ctrl — none of which the pattern exposes any more
   cleanly than the manual code. The pattern would shorten the
   detection by ~2 lines but the rewrite logic dwarfs that.

3. **`crates/opt/src/indirect_branch_resolve/jump_table.rs:540-580`** —
   `bound_from_if_condition`. **Reject**: blocker is `same_value`,
   which transitively follows trivial phis (lines 600-627 of the same
   file). Pattern's `Var` capture enforces exact `NodeOutputId`
   equality, not the trivial-phi-equivalent semantics. We could match
   the IntCmpOp shape via `int_cmp_any` and then apply `same_value`
   as a `.when()` predicate, but the pattern would then need TWO
   variants (one per operand ordering) for the asymmetric ops because
   `int_cmp_any` only auto-commutates `Equal` / `Carry` / `Scarry` —
   that's no improvement on the manual two-branch `if same_value(lhs,
   ...) { ... } else if same_value(rhs, ...) { ... }`.

4. **`crates/opt/src/indirect_branch_resolve/jump_table.rs:284-359`** —
   `compute_max_mask`. **Reject**: recursive walk over `IntConst`,
   `IntBinaryOp(And)`, `Truncate`, `Extend(ZeroExtend)`,
   `IntBinaryOp(ShiftRight)`. Tree-shaped but the per-node logic
   (taking `min` of recursive child bounds, masking by `type_mask`,
   handling `shift >= 64` defensively) is propagation, not
   structural matching. The pattern crate's matcher is not a
   propagator.

5. **`crates/opt/src/sp_expr.rs:128-164`** — `decompose_sp_inner`'s
   Add and Sub arms. **Reject (with caveat)**: the Add arm's manual
   `int_const_signed(r) ... else int_const_signed(l)` could become
   `add(any_int_const(c), var(other))` with auto-commutativity — same
   pattern as R2-1. **Why we rejected it for now**: `decompose_sp` is
   the hot path for every SP-aware pass (`StackStoreDetect`,
   `StackLoadForward`, `FunctionArgDetect`, `CallStackArgCollect`,
   plus the tier-2 stack-array classifier); a per-call `Pat` /
   `Matcher` allocation might be measurable. Additionally, the
   function takes `&Graph` but `Matcher` requires `&BuiltFunctionGraph`,
   so a migration would ripple through every caller's signature. Flag
   for R3: if we can either (a) profile and confirm the overhead is
   negligible, or (b) figure out a way to construct a `Matcher`
   without a full `BuiltFunctionGraph`, the same auto-commutative
   pattern that R2-1 used would apply cleanly here.

6. **`crates/opt/src/indirect_branch_resolve/stack_array.rs:324-363`** —
   `flatten_add_tree`. **Reject**: recursive over `Add` / `Sub`. Add
   case is just "extract both operands and recurse on each" — pattern
   doesn't help (we visit both operands regardless of order). Sub case
   needs `int_const_signed` (sign-extending u128 → i64 by
   NodeOutputType bit-width); `IntVar`'s `u128` capture loses the
   type-aware sign-extension, requiring extra reconstruction code.

7. **`crates/opt/src/indirect_branch_resolve/classify.rs:138-162`** —
   the `ValuePhi` arm's "every input must be `IntConst`" check.
   **Reject**: variadic-input shape (variable arity). The pattern crate's
   `PhiPat` exposes `.input(idx, p)` but only for `ControlPhi`, not for
   `ValuePhi` — there's no `ValuePhiPat` builder. Adding one would
   require new pattern infra; out of scope for R2.

8. **`crates/opt/src/redundant_phis/mod.rs`** — `remove_phis`'s
   `ControlPhi` / `MemPhi` / `ControlState` simplification.
   **Reject**: the logic depends on positional reachability (per-pred
   liveness via `ControlState.inputs[j]`'s producer) and on phi-token
   side-table semantics. Patterns can't express "kth phi input
   corresponds to kth ControlState predecessor".

9. **`crates/opt/src/stack_load_forward/mod.rs:175-292,442-517`** —
   `probe` and `find_stack_stored_value_at_offset`. **Reject**:
   transitive memory-chain DFS through `StackStore` / `Store` /
   `MemPhi` with cycle detection and per-arm aliasing analysis. Not
   tree-shaped.

10. **`crates/opt/src/function_args/mod.rs:395-575`** —
    `mem_chain_is_dirty`. **Reject**: same shape as #9 — transitive
    memory-chain shadow walk. Not tree-shaped.

11. **`crates/opt/src/stack_store/call_args.rs:50-146`** —
    `collect_stack_args_in_chain_order`. **Reject**: stateful chain
    walker with anchor-base tracking and a per-iteration positional
    expectation. Not tree-shaped.

## Suspicious manual matchers flagged but not touched

- **`crates/opt/src/sp_expr.rs:136-164`** — the Add / Sub arms
  reinvent commutative const detection. R2-1 demonstrates the pattern
  equivalent works; the rejection here is purely about hot-path
  performance and signature ripple. **What would resolve it**:
  microbenchmark of `decompose_sp` before/after a pattern-based
  rewrite. If the overhead is sub-microsecond per call and the
  per-pass-call frequency stays in the thousands not the millions,
  migrate. **Hand-off**: R3.

- **`crates/opt/src/indirect_branch_resolve/jump_table.rs:600-627`** —
  `same_value`'s recursive trivial-phi follow could be expressed as a
  pattern-with-`.when()` if we added a `transitive_through_trivial_phi`
  matcher option. **What would resolve it**: a pattern crate addition
  exposing trivial-phi follow through (sibling to the existing
  `ignore_casts` flag). **Hand-off**: R3 / R5.

- **`crates/strider/tests/manual_rewrite.rs`** — by name "manual" but
  every test exercises a *real* pattern flow (`add(var(x),
  int_const(0))` etc.) end-to-end against a Sleigh-lifted function.
  The test file's name is misleading; nothing to delete. **What would
  resolve it**: rename the test file to something like
  `pattern_rewrite.rs` or `graph_rewriter.rs`. **Hand-off**: R4.

## Open questions for R3 / R4 / R5

- **R3 unification candidate**: the F5 shim layer
  (`crates/strider/src/indirect_resolve_tier2/{classify, inplace,
  jump_table, stack_array}.rs`) re-exports the opt-side classifiers
  with the original `cfg::ResolvedTargets` return type. R3 is
  expected to consolidate; the prompt explicitly told R2 not to touch
  it.
- **R3 hot-path migration**: should `decompose_sp`'s Add/Sub arms move
  to `pattern::add` / `pattern::sub`? Profile first.
- **R4 readability**: `match_stack_array_shape` (post-refactor) still
  has nested `match` on `decompose_sp`'s `Some(SpExpr::Terminal/Phi)
  / None / int-const fallback` (lines ~272-296). The
  inline match is dense; a helper `try_classify_term` returning a
  three-way enum (`Sp`, `Const`, `IdxStride`, `None`) would clarify.
- **R5 missing tests**: `bound_via_predecessor_if`'s ControlState
  multi-pred fail-closed semantics (line ~492) has only ONE failing
  test (`bound_via_predecessor_if_returns_none_when_idx_unrelated_to_cond`).
  The "two preds, both prove the bound, take the max" path looks
  uncovered. R5 should add a fixture exercising that branch directly.
- **R5 missing tests**: `compute_max_mask` cycle handling (line ~291)
  is not directly tested — the recursive call structure makes a
  unit-level cycle-fixture awkward, but a regression test would pin
  the conservative `Some(type_mask)` answer the cycle short-circuit
  returns.
