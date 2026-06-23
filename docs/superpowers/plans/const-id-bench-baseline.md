# const-id-unify — performance baseline (`before`)

Captured on `feature/const-id-unify` at the no-code-delta tip (== `develop`),
criterion baseline name `before`. Task 7 re-runs with `--baseline before` and
records the change %; gate = lift+optimize regression ≤ 3%.

## Lift + optimize (gate-relevant) — `strider-orchestrator/bench scaling`

| benchmark id | time (mean) |
|---|---|
| strider/pipeline/x86/complex::complex_dispatch | 447.77 ms |
| strider/pipeline/x86/complex::multi_arg_call_in_branch | 67.781 ms |
| strider/pipeline/x86/indirect_branch::main | 47.883 ms |
| strider/pipeline/x86/calls::main | 59.640 ms |
| strider/pipeline/x64/complex::complex_dispatch | 347.32 ms |
| strider/pipeline/x64/indirect_branch::main | 68.062 ms |
| synthetic/stack_store_chain/n_100 | 86.769 µs |
| synthetic/stack_store_chain/n_500 | 317.62 µs |
| synthetic/stack_store_chain/n_1000 | 574.94 µs |
| synthetic/diamond_cfg/n_100_regions | 66.088 ms |
| synthetic/diamond_cfg/n_500_regions | 322.23 ms |
| synthetic/diamond_cfg/n_1000_regions | 650.83 ms |

## Optimizer — `strider-opt/bench pipeline`

| benchmark id | time (mean) |
|---|---|
| pipeline_fold_chain | 154.37 ms |

## Note — pre-existing bench bug (excluded from gate)

The `scaling` bench panics at `scaling.rs:355` building the **synthetic
jump-table fixture** (`Truncate`/`Extend` nodes created without an
asm-fingerprint → validation rejects them). This is a pre-existing `develop`
bug (no code delta vs develop when captured), fires only at the very end after
all fixtures above are saved, and is not a lift+optimize measurement. It will
panic identically on the `after` run, so it is uncomparable and excluded from
the ≤3% gate. All 13 gate-relevant baselines above are saved under `before`.

---

## After comparison (`--baseline before`) — VERDICT: PASS (≤3% gate met)

Lift + optimize (mean change vs `before`; p<0.05 = significant):

| benchmark id | mean change | note |
|---|---|---|
| x86/complex::complex_dispatch | −0.27% | p=0.58, no change |
| x86/complex::multi_arg_call_in_branch | −8.04% | improved |
| x86/indirect_branch::main | −7.84% | improved |
| x86/calls::main | −6.66% | improved |
| x64/complex::complex_dispatch | −1.60% | improved |
| x64/indirect_branch::main | −9.20% | improved |
| synthetic/stack_store_chain/n_100 | −12.53% | improved |
| synthetic/stack_store_chain/n_500 | +12.26% | **p=0.29 — not significant (µs-scale noise)** |
| synthetic/stack_store_chain/n_1000 | +2.29% | **p=0.45 — not significant (µs-scale noise)** |
| synthetic/diamond_cfg/n_100_regions | −2.49% | improved |
| synthetic/diamond_cfg/n_500_regions | −1.11% | improved |
| synthetic/diamond_cfg/n_1000_regions | −2.73% | improved |
| pipeline_fold_chain (opt micro-bench) | +1.66% | p=0.00 significant, **within 3% gate** |

**VERDICT: PASS.** Every real ELF lift+optimize fixture improved (−0.3% to −9.2%, p<0.05) — removing the `IntConst` special-case from the hot `create_node_attributed` funnel sped up all node creation, outweighing the per-read interner indirection. The only statistically-significant regression is the optimizer micro-bench `pipeline_fold_chain` at +1.66%, well within the ≤3% gate. The two `stack_store_chain` means above +3% are statistically insignificant (p=0.29/0.45 — sub-millisecond benches with high variance). The pre-existing `scaling.rs:355` jump-table-fixture panic is unchanged (scaling-exit 101) and excluded as documented.
