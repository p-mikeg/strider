# IR Node Canonicalization

**Date:** 2026-05-03
**Branch:** `refactor/ir-node-canonicalization`
**Goal:** Remove redundant `NodeKind` enum variants whose semantics are already
expressible via other nodes, so that pattern matching across different
assemblies sees one canonical shape per operation rather than several.

## Motivation

Pattern queries today must enumerate semantically-equivalent shapes by hand:
`add(x, IntConst(-K))` vs `sub(x, IntConst(K))`, `If(C){t}{f}` vs
`If(BoolNeg(C)){f}{t}`, `LessEqual(a,b)` vs `BoolNeg(Less(b,a))`. Each duplicate
form is a place where a real bug can hide because the pattern only covered one
arm. Lifting these into canonical shapes at lift time (or via a single
canonicalization pass) produces a smaller, more uniform IR.

The constraint stated by the project owner: *"the mission of the graph is to
be sound but change as little as possible between different assembly."* Every
removal in this spec is sound (no information loss) and reduces shape variance
across architectures.

## Removals

### Integer comparisons

| Variant | Lowering | Rationale |
|---------|----------|-----------|
| `IntCmpOp::Borrow` | delete (dead variant); `Less` doc-comment notes it also represents unsigned-borrow per rsleigh `IntLess` | rsleigh has no `IntBorrow` opcode (`IntLess = 15` doc says "also indicates a borrow on unsigned subtraction"). The strider lifter never emits `IntCmpOp::Borrow`. The eval table returns `l<r`, identical to `Less`. |
| `IntCmpOp::LessEqual(a,b)` | `BoolNeg(Less(b,a))` at lift time | Mirrors existing `IntNotEqual → BoolNeg(IntEqual)` precedent ([crates/pcode-lift/src/value/arithmetic.rs:74](crates/pcode-lift/src/value/arithmetic.rs#L74)). |
| `IntCmpOp::SlessEqual(a,b)` | `BoolNeg(Sless(b,a))` at lift time | Same. |

### Integer arithmetic

| Variant | Lowering | Rationale |
|---------|----------|-----------|
| `IntBinaryOp::Sub(a,b)` | `Add(a, IntUnaryOp::Neg(b))` (after rename, where post-rename `Neg` is two's-complement negate) | Removes the `Add(x, -K)` vs `Sub(x, K)` shape ambiguity that pattern callers currently must enumerate. Preserves wrap semantics: `a - b ≡ a + (-b) (mod 2^W)`. |

### Float operations

| Variant | Lowering | Soundness |
|---------|----------|-----------|
| `FloatBinaryOp::Sub(a,b)` | `FloatAdd(a, FloatUnaryOp::Neg(b))` | IEEE 754: `a - b ≡ a + (-b)` for finite values; for NaN/inf the same identity holds for the bit pattern (negation flips the sign bit). |
| `FloatCmpOp::NotEqual(a,b)` | `BoolNeg(FloatEqual(a,b))` | Sound under IEEE 754: `Equal` is false when either operand is NaN, so `!Equal` is true (= correct `NotEqual`). |
| `FloatCmpOp::LessEqual(a,b)` | `BoolBinaryOp::Or(FloatLess(a,b), FloatEqual(a,b))` | NaN-aware: cannot use `BoolNeg(Less(b,a))` because both `Less` and `LessEqual` are false when either operand is NaN, while `BoolNeg(Less(...))` would be true. |

### `IfPat` symmetric matching

The pattern crate currently tries two layouts when matching `if_node().cond(C)`:
direct `If(C){t}{f}` and inverted `If(BoolNeg(C)){f}{t}`. Replace this with an
eager IR-rewrite pass `IfCondInversion` that produces the canonical form once,
so the matcher only handles the direct layout.

## Renames

The IR currently uses `IntUnaryOp::Neg` for bitwise NOT (`~x`) and
`IntUnaryOp::Not` for two's-complement (`-x`), inherited from rsleigh's Sleigh
nomenclature where `IntNeg = ~` and `Int2Comp = -`. This is documented as a
foot-gun in `crates/opt/src/constant_fold/rules.rs:415-421`. Swap the names:

| Before | After | Semantics |
|--------|-------|-----------|
| `IntUnaryOp::Neg` | `IntUnaryOp::BitNot` | Bitwise NOT (`~x`) |
| `IntUnaryOp::Not` | `IntUnaryOp::Neg` | Two's-complement (`-x`) |

The pcode-lift dispatch site for `Opcode::IntNeg` gets a comment:
`// rsleigh's IntNeg opcode is bitwise-NOT (legacy Sleigh nomenclature) → IntUnaryOp::BitNot`.

`BoolUnaryOp::Neg` (logical NOT, `!x`) is conventional and stays.

## Phase plan

Phases run sequentially. Each phase ends with `cargo test --workspace` + `cargo
clippy --workspace` clean before the next begins. Each phase is a separate
commit so individual phases can be reverted if needed.

**Phase 0 — Setup.** Create worktree on `refactor/ir-node-canonicalization`,
write this spec, verify clean baseline. *(commit: spec doc)*

**Phase 1 — Delete `IntCmpOp::Borrow`.** Smallest, dead variant. Proves the
worktree/CI loop. Edits: enum + eval table + `Less` doc comment +
`node_signature.rs` + `dot/label.rs` + `strider-py` cmp dispatcher (if
applicable).

**Phase 2 — Rename `IntUnaryOp::{Neg,Not}` → `{BitNot,Neg}`.** Two-step to
eliminate the swap-risk:
  1. Add `BitNot`, migrate every `IntUnaryOp::Neg` site to `BitNot`. Each site
     committed and tested.
  2. Delete old `Neg`, rename `Not → Neg`. Add lifter-site comment.

**Phase 3 — `IntCmpOp::{LessEqual, SlessEqual}` lift-time lowering.**
Failing test first: lifting an `IntLessEqual` opcode produces shape
`BoolNeg(Less(rhs, lhs))`. Then add `handle_int_less_equal` /
`handle_int_sless_equal` in pcode-lift. Delete enum variants. Update
`jump_table.rs` predecessor-bound walker to recognize `BoolNeg(Less(b, idx))`
as `<=` (bound = N+1, not N).

**Phase 4 — `IntBinaryOp::Sub` lift-time lowering.** Largest blast radius.
Failing test first: `IntSub a, b` produces shape `Add(a, Neg(b))` (post-rename).
Then `handle_int_sub` handler. Delete `Sub` variant. Update:
  - `eval_int.rs`: drop Sub arm.
  - Reassoc rules in `constant_fold/rules.rs`: drop the four sub-keyed rules.
    Add `Add(x, Neg(IntConst(C))) → Add(x, IntConst(-C))` rule. Verify the
    `add_add` rule covers all simplifications previously handled by
    `(x ± C1) ± C2`.
  - `sp_expr.rs` (`StackStoreDetect`): the Add/Sub chain walker. New test for
    negative-offset SP store via the lowered shape.
  - Pattern crate: keep `sub(a, b)` builder as ergonomic alias that emits
    `add(a, neg(b))` internally. Existing call-sites unchanged.
  - `strider-py`: drop `Sub` from int-binary dispatcher; verify `pattern.sub`
    Python ctor maps to the lowered shape.
  - Cache-correctness test: two `IntSub a, b` lifts dedup to the same node.

**Phase 5 — Float lowerings.** `FloatBinaryOp::Sub`,
`FloatCmpOp::NotEqual`, `FloatCmpOp::LessEqual`. Failing test per lowering.
Mirror Phase 4 mechanics (handlers, enum delete, eval-table update,
`strider-py` dispatchers).

**Phase 6 — `IfCondInversion` pass + `IfPat` symmetric matching deletion.**
Failing test: graph with `If(BoolNeg(C)){then=A}{else=B}` ends with cond `C`
and branches swapped after the pass. Convergence test:
`If(BoolNeg(BoolNeg(C)))` collapses to `If(C)` with no branch swap (even
parity). Implement the pass as a small dedicated module in `opt`; runs in
`stable_default_pipeline` after `constant_fold`. Delete the inverted-layout
matching code in `IfPat`. Update existing IfPat tests that exercised the
symmetric matching to instead exercise the canonicalization pass.

**Phase 7 — Final review.** Full workspace test + clippy clean. CLAUDE.md
update to reflect the post-refactor IR. Anti-regression validator test:
constructing a graph with a removed variant fails validation. One-subagent
code review (`feature-dev:code-reviewer`) against this spec; address findings.

## Test policy

- Every lift-time lowering: dedicated pcode-lift test asserting the produced
  IR **shape** (canonicalization is a structural invariant, not a behavioral
  one — assert the shape).
- Every removed variant: search for tests constructing it; either delete or
  rewrite to the lowered shape. No "this variant exists" assertions remain.
- Phase 4: cache-correctness test (two `IntSub` ops dedup) + perf-baseline
  check (capture wall time before and after; fail phase if regression > 2x).
- Phase 6: convergence test for `BoolNeg(BoolNeg(C))`.
- Phase 7: validator-rejection test for each removed variant.

## Risks

1. **Phase 4 (Sub) is the dangerous phase.** Touches every opt pass that
   pattern-matches address arithmetic: `StackStoreDetect`, `StackLoadForward`,
   the indirect-branch jump-table classifier, and the constant-fold reassoc
   rules. Sequenced last among the variant removals so prior phases stabilize
   the test loop first.
2. **Node-count growth.** Every subtraction becomes two nodes (`Add` +
   `Neg`). Most binaries are subtraction-heavy. Phase 4 captures a wall-clock
   baseline; > 2× regression fails the phase.
3. **`IfCondInversion` introduces new graph-surgery infrastructure.** Cannot
   be expressed as a `pattern::rewrite_rule` because rule rewrites can't swap
   branch consumers. Written as a small dedicated module with its own tests;
   bounded surface area.

## Out of scope (per project owner)

- `ShiftLeft` / `ShiftRight` removal.
- `Sless` removal (kept as primitive).
- `Mul → Shl` for power-of-2 stride canonicalization.
- Tier-3 pattern-crate higher-level builders (`indexed_addr`, `add_offset`).
- `CastToFloat` lowering at IR layer (already handled by post-pass).
- Higher-level shape canonicalizations (`(x + C1) + C2 → x + (C1+C2)` etc.
  are already in the existing reassoc rules).
