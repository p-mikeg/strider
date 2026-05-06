# Round 7 — Concrete Test Plan

Test cases needed to close the gaps surfaced by Rounds 1–6. Cross-references use `[R7-X]` to cite the relevant audit report.

## Already-covered (DO NOT re-add)

The test-plan agent verified these claimed-gap items have existing coverage:
- `IfCondInversion` unit tests at `crates/opt/src/if_cond_inversion/tests.rs` (4 tests).
- `find_all_requirements` disagreement filter at `strider-py/tests/python/test_find_all_requirements.py:105-116`.
- `at_any([])` / `offset_any([])` vacuous failure at `test_call_builder.py:131-136` and `test_stack_offset_recovery.py:99-102`.
- `validate_with_options { check_asm_fingerprints: true }` at `strider/tests/asm_fingerprints.rs`.
- Python pipeline count sync at `strider-py/tests/python/test_optimizer_pipeline.py:62-83`.
- `DuplicateFunctionArg` Layer-C at `ir/src/validate/tests.rs:466-473`.

## Priority 1 — Bug Regressions (12 tests, must land with the corresponding fix)

| # | Title | File | Type | What It Catches | Effort |
|---|-------|------|------|-----------------|--------|
| 1 | CondBranch sole-insn OOB drops in-range edge | `cfg/tests/region_builder_process.rs` | Unit | `region_builder.rs:381-385` silently emits `TailCall { in_range }` & never enqueues in-range successor [R7-pcode-lift-cfg C3] | M |
| 2 | PyCapture hash collision for ids ≥ 10 | `strider-py/tests/python/test_pattern_basics.py` | Py unit | `pattern.rs:54` hashes `len(repr(...))`; ids 10–99 all hash to 11 [R7-py-support C1] | S |
| 3 | `find_stack_stored_value` deep-chain stack overflow | `opt/tests/regress_deep_chain.rs` (new) | Unit | 1000-disjoint-`StackStore` chain triggers unbounded recursion [R7-opt CRIT-1] | M |
| 4 | `mem_chain_is_dirty` deep-chain stack overflow | same new file | Unit | 1000-`MemPhi` chain triggers unbounded recursion [R7-scale A1] | M |
| 5 | `swapgs` must advance memory edge | `target/src/call_other_abi.rs` (test) | Unit | Reclassify from `PURE` to `PURE_WITH_MEM_EDGE` [R7-correctness D2] | S |
| 6 | `sysret` must NOT be `NoReturn` | `target/src/call_other_abi.rs` (test) | Unit | Reclassify from `NoReturn` to `Call(abi)` with `RCX`/`R11` reads [R7-correctness D1] | S |
| 7 | AArch64 `x30` must NOT be in `callee_saved_regs` | `target/src/calling_convention/tests.rs` | Unit | AAPCS64 §6.1.1: `bl` clobbers x30 [R7-correctness A2] | S |
| 8 | ARM `lr` must NOT be in `callee_saved_regs` | same | Unit | AAPCS §5.1.1: `bl` clobbers lr [R7-correctness A3] | S |
| 9 | `IfCondInversion` swap also swaps VarPhi value inputs | `opt/src/if_cond_inversion/tests.rs` | Unit | Currently swaps ControlState inputs but NOT corresponding VarPhi value slots — silent data-flow corruption [R7-correctness C1, NEW] | L |
| 10 | `PyMemoryMap::ReadOnlyMemory` honors arch endianness | `strider-py/tests/python/test_memory_map.py` | Py unit | `reader.rs:578` always uses `from_le_bytes`; BE arches corrupt [R7-silent-failures H1] | S |
| 11 | `PyReadOnlyMemoryAdapter::read` propagates Python exceptions | `strider-py/tests/python/test_callback_reader.py` | Py unit | `reader.rs:511,515` `.ok()?` swallows exceptions [R7-silent-failures H2] | S |
| 12 | `analyze_known_bits` error propagates from classifier | `opt/tests/regress_known_bits_error.rs` (new) | Unit | `classify.rs:54,78` use `.ok()?` and silently return None on Kb::merge contradiction [R7-silent-failures H3] | M |
| 13 | `run.rs` custom-pipeline path must use ROM | `strider-py/tests/python/test_run.py` | Py integ | `let _ = rom;` at `run.rs:197` discards user ROM [R7-silent-failures H5] | S |
| 14 | `build_call_other_terminal` must close region | `ir/src/builder/tests.rs` | Unit | Subsequent `build_*` should fail with `NoCurrentRegion` [R7-ir SC-4] | S |

## Priority 2 — Coverage gaps (8 tests)

1. **Asm-fingerprint dedup-cache UNION on cache hit** — `ir/src/builder/tests.rs`. Build same `IntConst(42)` twice with different `lift_addr`; verify both addresses present in fingerprint.
2. **Asm-fingerprint shrink prevention across pipeline** — `opt/tests/asm_fingerprint_propagation.rs`. For every reachable node before/after `default_pipeline.run`, assert `post_len >= pre_len`.
3. **vn_io sub-register partial-write with phi-live parent** — new file `pcode-lift/tests/vn_io_partial_write.rs`. `mov al, 0xFF` → `movzx eax, ax` must depend on `InitialVar(rax)` (preserve upper byte).
4. **`int_const_any_of([])` vacuous fail (Rust)** — `pattern/tests/matching/arithmetic.rs`. Empty + non-matching + matching cases.
5. **KnownBits `SignExtend` upper-bit propagation** — new file `opt/tests/known_bits_sign_extend.rs`. `SignExtend(IntConst 0x81 : U8 → U64)` should fold to `IntConst(0xFFFFFFFFFFFFFF81)`.
6. **Python typed errors actually raised** — `strider-py/tests/python/test_smoke.py`. Trigger each typed exception (StriderError, LiftError, ReaderError, PatternError, RewriteError, UnresolvedIndirectBranchError, UnknownCallOtherError) end-to-end.
7. **AArch64 e2e lift produces valid IR** — new file `strider/tests/aarch64_lift.rs`. Synthetic `ret` → `validate(&graph, entry).is_ok()`.
8. **`phi()` matches MemPhi as well as VarPhi** — `pattern/tests/matching/control_flow.rs`. Diamond with both phi kinds; `phi()` must match both (after the fix from Round 1D / R7-pattern #2).

## Priority 3 — Property / fuzz tests (4 tests)

1. **Pattern alias round-trip** — new file `pattern/tests/matching/aliases.rs`. For each lowered alias (`sub`, `int_le`, `int_sle`, `float_sub`, `float_ne`, `float_le`), build the explicit lowered IR and assert the alias matches it.
2. **Stack-array indirect-branch shape end-to-end** — `strider/tests/indirect_resolve_classify.rs`. Add `build_stack_array_scenario(targets: &[u64])`. Currently zero coverage of the stack-array classifier arm.
3. **`StackLoadForward` + `StackStoreDetect` converge in ≤ 2 iters** — new file `opt/tests/pipeline_with_stack.rs`. After the second pass run, both `changed()` returns false.
4. **`OptimizerPipeline` idempotency** — new file `opt/tests/pipeline_fixedpoint.rs`. Run `default_pipeline()` twice; node count and fingerprints unchanged.

## Priority 4 — Scale benchmarks (4, in `strider/benches/scaling.rs`)

1. **Chain-of-N stores** for N ∈ {100, 500, 1000} — measures `find_stack_stored_value`.
2. **Diamond CFG of N regions** — measures `initial_var_index` rebuild-per-edit hot spot (R7-scale B11).
3. **Wide jump-table N targets** — measures `region_id_at_start` linear scan (R7-pcode-lift-cfg D3) plus orchestrator iteration count.
4. **`find_all_requirements` shared-capture join** — measures cross-product blowup (R7-scale B2).

## Files-to-create summary

```
crates/opt/tests/regress_deep_chain.rs
crates/opt/tests/regress_known_bits_error.rs
crates/opt/tests/known_bits_sign_extend.rs
crates/opt/tests/asm_fingerprint_propagation.rs
crates/opt/tests/pipeline_with_stack.rs
crates/opt/tests/pipeline_fixedpoint.rs
crates/pattern/tests/matching/aliases.rs
crates/pcode-lift/tests/vn_io_partial_write.rs
crates/strider/tests/aarch64_lift.rs
```

## Files-to-modify summary

```
crates/cfg/tests/region_builder_process.rs            (P1.1)
crates/ir/src/builder/tests.rs                        (P1.14, P2.1)
crates/opt/src/if_cond_inversion/tests.rs             (P1.9 — VarPhi swap)
crates/pattern/tests/matching/control_flow.rs         (P2.8)
crates/pattern/tests/matching/arithmetic.rs           (P2.4)
crates/strider-py/tests/python/test_pattern_basics.py (P1.2)
crates/strider-py/tests/python/test_memory_map.py     (P1.10)
crates/strider-py/tests/python/test_callback_reader.py(P1.11)
crates/strider-py/tests/python/test_run.py            (P1.13)
crates/strider-py/tests/python/test_smoke.py          (P2.6)
crates/strider/tests/indirect_resolve_classify.rs     (P3.2)
crates/strider/benches/scaling.rs                     (P4.1–P4.4)
crates/target/src/call_other_abi.rs                   (P1.5, P1.6 tests)
crates/target/src/calling_convention/tests.rs         (P1.7, P1.8 tests)
```

Total new tests: ~28. Effort: ~2 engineer-days for P1, ~2 days for P2, ~1 day for P3, ~1 day for P4 = ~6 days end-to-end.
