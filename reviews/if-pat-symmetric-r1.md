# IfPat-symmetric + const-fold review (r1)

> Branch: `feature/if-pat-symmetric` against `feature/ai`. 3 commits, ≈+250 LoC. Build clean, 0 test failures, clippy clean.

## Summary

No correctness bugs. The binding-scope handling, output-index mapping, and exhaustive const-gate are all correct. Four documentation/test-coverage findings worth applying — total +17 LoC.

| # | Where | What | Confidence | ΔLoC |
|---|---|---|---|---|
| F-4 | `pattern/tests/matching/if_pat_symmetric.rs` | Missing test: shared `Capture` across `cond` and branch | 81 | +8 |
| F-5 | `opt/src/constant_fold/tests.rs:~779` | Comment clarity on `no_fold_bool_xor_false` | 80 | +2 |
| F-6 | `pattern/src/pat/builders/branch.rs:49-52` | `true_branch` rustdoc says "also matches" — should be "is tried against" | 80 | +3 |
| F-7 | `pattern/src/pat/builders/branch.rs:1-18, 43-46` | Module doc should note absence of `.ordered()` opt-out | 80 | +4 |

## Correctness analysis

- **Binding-scope correctness:** `mark`/`restore` is taken once at the top of `try_match`, before any `try_layout` call. Direct-layout failures correctly roll back any partial captures (e.g. cond matched but branch failed). Swap then starts from a clean state.
- **Output index mapping:** `(true_out_idx, false_out_idx) = (1, 0)` in the swapped case correctly maps source-level true to IR output 1 (the physical false-edge under `Not(cond)`) and vice versa.
- **`bool_const(true)` gate:** `KindSpec::Exact(NodeKind::BoolConst(true))` only fires when the payload is exactly `true`. Cannot misfire on `false`. Verified by `no_fold_bool_xor_false` test.
- **Dead code removal:** `ConsumersSpec::Indexed` removal is complete — exhaustive match in `node_pat.rs::try_once` would fail to compile if any consumer remained. Build passes.

## No issues found

- Binding leak when cond matches direct but branch fails (mark rolls it back correctly)
- Cast walk-through interaction (`Cast(Not(x))` not produced by lifter; xor-with-true rule canonicalises the common compiler emission)
- Commutative retry of `xor(x, bool_const(true))` (handled by `NodePat::try_match_common`'s commutative path)

## No pre-merge blockers

All findings are improvements, not correctness fixes. Safe to merge as-is.
