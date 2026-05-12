# Round 10 — Test Plan

35 tests across 3 priority sections. Effort split: 18 S (51%), 11 M (31%), 6 L (17%).

---

## 1. HIGH-priority regression tests

### T-1: `contains_addr` on empty-`insns` region at `start_addr` returns correct value
- **Source finding:** round10-1B I-1 (HIGH, confidence 95)
- **Scope:** unit
- **File:** `crates/cfg/tests/region.rs` (existing)
- **Harness:** `cfg::test_api::Region` struct literal with `insns: Vec::new()`
- **Assertions:** Empty region must own its `start_addr`; downstream `find_region_containing_addr` resolves empty-Branch region.
- **Effort:** S

### T-2: `split_region` with `split_index == insns.len()` is a no-op
- **Source finding:** round10-1B I-7 (MED, downstream HIGH)
- **Scope:** unit
- **File:** `crates/cfg/tests/builder_split_region.rs`
- **Harness:** 2-instruction region; split addr beyond last insn
- **Assertions:** Result equals original id; no extra region created; insns preserved.
- **Effort:** S

### T-3: KB cache invalidation across in-place edits
- **Source finding:** round10-1C C-1 (HIGH, confidence 88)
- **Scope:** integration
- **File:** `crates/strider/tests/indirect_resolve_in_place_edits.rs`
- **Harness:** Two `IndirectBranch` placeholders; first resolves to constant tail-call (triggers `apply_tail_call → detach_node_inputs`); second is jump-table load sharing sub-expression
- **Assertions:** Second anchor's resolved target count matches fresh-KB analysis.
- **Effort:** L

### T-4: `FunctionArgDetect` exact-width path absorbs InitialVar fingerprint
- **Source finding:** round10-1C C-2 (HIGH, confidence 85)
- **Scope:** unit
- **File:** `crates/opt/tests/asm_fingerprint_propagation.rs`
- **Harness:** `make_fn_with_var` with `reg_vn` attributed at `0x400`; configure FunctionArgDetect with that varnode as position-0 reg arg
- **Assertions:** Resulting `FunctionArg` node's fingerprint contains `0x400`.
- **Effort:** M

### T-5: `ret().capture(c).when(f)` matches `Return` node via `find_all`
- **Source finding:** round10-1D C-1 (HIGH)
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/control_flow.rs`
- **Harness:** `Tb::empty().ret_nothing()` + `ret().capture(c).when(|_,_,_| true)`
- **Assertions:** `find_all` returns ≥1 hit; capture binds the `Return` node.
- **Effort:** S

### T-6: `int_binary_any(c, _, _)` populates `output(c)` value output
- **Source finding:** round10-1D C-2 (HIGH, confidence 85)
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/variant_agnostic.rs`
- **Harness:** `Tb::empty()` with `Add(1,2)`; match `int_binary_any(c, any(), any())`
- **Assertions:** After fix, `match.output(c)` is `Some` (value output populated).
- **Effort:** S

### T-7: `g.find_all(pattern.mem_phi())` does not raise `TypeError`
- **Source finding:** round10-1F F-01 (HIGH)
- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_pattern_full_coverage.py`
- **Harness:** Lift fixture ELF with conditional branch (produces MemPhi)
- **Assertions:** `g.find_all(mem_phi())` returns list, no TypeError. Same for `value_phi()`.
- **Effort:** M

### T-8: `PyCapture.__hash__` non-negative, stable for many captures
- **Source finding:** round10-1F F-02 (HIGH)
- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_pattern_basics.py`
- **Harness:** Pure Python; create 65536 captures
- **Assertions:** All hashes ≥ 0; all captures hash distinctly into a dict.
- **Effort:** S

### T-9: `recompute_unresolved` returns `Err` when graph is None
- **Source finding:** round10-2C H10-S1 (HIGH)
- **Scope:** unit
- **File:** `crates/strider/src/orchestrator.rs` `#[cfg(test)]`
- **Harness:** Construct `LoopState` with `graph: None`
- **Assertions:** Result is `Err`, error message names "not initialised".
- **Effort:** S

### T-10: `KnownBits::ZeroExtend` with `U128` input bails out (returns None)
- **Source finding:** round10-2C H10-S2 (HIGH)
- **Scope:** unit
- **File:** `crates/opt/tests/known_bits_edge_cases.rs` (new)
- **Harness:** `make_empty_fn` building `ZeroExtend(U128 → U256)`
- **Assertions:** `analyze_known_bits` returns None for the ZeroExtend, OR upper bits are not falsely marked as zero.
- **Effort:** M

### T-11: ELF autoload section parse failure visible in `RelocationStats`
- **Source finding:** round10-2C H10-S3 (HIGH)
- **Scope:** unit
- **File:** `crates/reader/tests/elf_relocations.rs`
- **Harness:** Crafted ELF with truncated section data
- **Assertions:** `stats.skipped_malformed_target > 0` after fix; not only stderr.
- **Effort:** M

### T-12: Python `MemReader.read` exception text in `ReaderError` message
- **Source finding:** round10-2C H10-S4 (HIGH)
- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_typed_errors_e2e.py`
- **Harness:** `MemReader` subclass raising `ValueError("sentinel_text")`
- **Assertions:** "sentinel_text" appears in the raised `ReaderError`.
- **Effort:** M

### T-13: `ReadOnlyMemory.read` `KeyboardInterrupt` propagates
- **Source finding:** round10-2C H10-S5 (HIGH)
- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_typed_errors_e2e.py`
- **Harness:** `ReadOnlyMemory` subclass raising `KeyboardInterrupt`; `LoadReadOnly` pass
- **Assertions:** `pytest.raises(KeyboardInterrupt)` fires; not silently swallowed.
- **Effort:** M

### T-14: `mem_chain_is_dirty` invariant violation surfaces as `Err` in release
- **Source finding:** round10-2C H10-S6 (HIGH)
- **Scope:** unit
- **File:** `crates/opt/tests/function_args_stack_invariant.rs` (new)
- **Harness:** Force degenerate walker with non-singleton result stack
- **Assertions:** Returns `Err`, not `Ok(true)`.
- **Effort:** M

### T-15: `Kb::from_ones_zeros` rejects overlapping bits
- **Source finding:** round10-2D Section 1 (HIGH)
- **Scope:** unit
- **File:** `crates/opt/tests/known_bits_edge_cases.rs` (new)
- **Harness:** Direct ctor call after fields → `pub(crate)`
- **Assertions:** `from_ones_zeros(0xFF, 0xFF)` Err; `(0x0F, 0xF0)` Ok with `ones & zeros == 0`.
- **Effort:** S

### T-16: `BuiltCallingConvention::try_from_parts` rejects SP in arg_passing_regs
- **Source finding:** round10-2D Section 4 (HIGH)
- **Scope:** unit
- **File:** `crates/target/tests/cc_validation.rs` (new)
- **Harness:** CallingConvention with `stack_ptr_reg_name: "rsp"` AND `arg_passing_reg_names: ["rsp", "rdi"]`
- **Assertions:** `try_build` returns Err.
- **Effort:** S

### T-17: `make_int_const(0x1FF, U8)` deduplicates with `make_int_const(0xFF, U8)`
- **Source finding:** round10-1A M-1 (MED but structural-equality break)
- **Scope:** unit
- **File:** `crates/ir/tests/dedup_cache.rs`
- **Harness:** Two `make_int_const` calls differing in unmasked high bits
- **Assertions:** Same NodeId returned (after masking fix).
- **Effort:** S

---

## 2. IMPORTANT additions

### T-18: `int_const_any_of([])` never matches
- **Source finding:** CLAUDE.md vacuous-fail contract
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/wildcards_and_consts.rs`
- **Effort:** S

### T-19: `call().at_any([])` and `stack_store().offset_any([])` vacuously false
- **Source finding:** CLAUDE.md contract
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/control_flow.rs`
- **Effort:** S

### T-20: `Match::get_uint` / `output(c)` semantics on control-flow capture
- **Source finding:** round10-2C M10-S4/M10-S5
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/bindings.rs`
- **Assertions:** `node(c)` Some, `output(c)` None, `get_uint(c, &g)` None.
- **Effort:** S

### T-21: `find_all_requirements` shared-capture cross-product filtering
- **Source finding:** CLAUDE.md contract
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/matcher_api.rs`
- **Harness:** Two-pattern join on shared `base_cap`
- **Assertions:** Every tuple agrees on `base_cap`.
- **Effort:** M

### T-22: `StackLoadForward` forwards through `StackStorePhi` when offsets disjoint
- **Source finding:** round10-1C I-6
- **Scope:** integration
- **File:** `crates/opt/tests/pipeline_with_stack.rs`
- **Effort:** L

### T-23: AArch64 end-to-end ELF lift produces valid IR
- **Source finding:** round10-1E I-7 + cross-arch coverage gap
- **Scope:** integration
- **File:** `crates/strider/tests/aarch64_smoke.rs` (new)
- **Effort:** M

### T-24: MIPS O32 end-to-end lift does not crash
- **Source finding:** round10-1E I-7
- **Scope:** integration
- **File:** `crates/strider/tests/mips_smoke.rs` (new)
- **Effort:** L

### T-25: CallOther dispatch — ARM `swi` reads `r7/r0..r6`, x86 differs; arch-neutral `mfence` agrees
- **Source finding:** round10-1E + CLAUDE.md ABI matrix
- **Scope:** unit
- **File:** `crates/target/tests/callother_dispatch.rs` (new)
- **Effort:** S

### T-26: PyO3 typed exception is `LiftError` (not base `StriderError`) on bad lift
- **Source finding:** round10-1F F-05
- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_typed_errors_e2e.py`
- **Effort:** S

### T-27: `handle_int_sub` width mismatch surfaces as Err or debug-assert
- **Source finding:** round10-1B I-3 (HIGH)
- **Scope:** unit
- **File:** `crates/pcode-lift/tests/value_lifter.rs`
- **Effort:** M

### T-28: Asm-fingerprint dedup-union — same `IntConst(42)` from two different `lift_at` blocks
- **Source finding:** CLAUDE.md asm-fingerprint contract
- **Scope:** unit
- **File:** `crates/opt/tests/asm_fingerprint_propagation.rs`
- **Assertions:** Single deduplicated NodeId carries both addresses.
- **Effort:** S

---

## 3. MED additions

### T-29: Lift-time canonicalisation — `IntSub` lowers to `Add(_, Neg(_))`, no `Sub` survives
- **Scope:** unit; `crates/pcode-lift/tests/value_lifter.rs`; **S**

### T-30: Lift-time canonicalisation — `IntLessEqual` lowers to `BoolNeg(IntLess(_, _))`
- **Scope:** unit; `crates/pcode-lift/tests/value_lifter.rs`; **S**

### T-31: Stack-overflow safety — 1024-deep memory chain in `FunctionArgDetect`
- **Scope:** unit; `crates/opt/tests/mem_chain_depth.rs` (new); **M**

### T-32: `StackStorePhi` without populated `stack_phi_offsets` caught by Layer-C validator (after fix)
- **Scope:** unit; `crates/ir/tests/build_validate_roundtrip.rs`; **M**

### T-33: `ResolvedTargets::multiple(vec![])` returns `Err`
- **Scope:** unit; `crates/opt/src/indirect_branch_resolve/mod.rs`; **S**

### T-34: `is_addr_tail_call(target, start, FunctionBoundary::Bounded{..})` correctness
- **Scope:** unit; `crates/cfg/tests/cfg_query.rs`; **S**

### T-35: Asm-fingerprint shrink-prevention — full pipeline preserves all attributions on every reachable node
- **Scope:** integration; `crates/opt/tests/asm_fingerprint_propagation.rs`; **L**

---

## 4. Coverage summary

### Already covered (no test needed)

| Area | Existing file | Status |
|---|---|---|
| `call().at_any([])` vacuous-false | `pattern/tests/matching/control_flow.rs:48-54` | Covered |
| `int_const_any_of` set membership (non-empty) | `pattern/tests/matching/control_flow.rs:56-75` | Covered |
| Asm-fingerprint per-pass (CF/KB/rewrite_rule) | `opt/tests/asm_fingerprint_propagation.rs` | Partial — missing FunctionArgDetect exact-width (T-4) |
| Asm-fingerprint x86 end-to-end | `strider/tests/asm_fingerprints.rs` | x86 only — gap on AArch64/MIPS (T-23/T-24) |
| `contains_addr` empty region (current pre-fix behavior) | `cfg/tests/region.rs:49-58` | Updated by T-1 |
| `split_region` normal cases | `cfg/tests/builder_split_region.rs` | Missing edge (T-2) |

### Intentionally not added

- `round10-2A-panics.md` — 7 production panics verified justified
- `round10-2C M10-S3` (`stack_phi_offsets` empty-to-None) — intentional behaviour, pinned by existing tests
- `round10-3A` / `round10-3B` — doc-only; no behaviour to pin

### Effort summary

| Effort | Count | Test IDs |
|---|---|---|
| S (<30min) | 18 | T-1, T-2, T-5, T-6, T-8, T-9, T-15, T-16, T-17, T-18, T-19, T-20, T-25, T-26, T-28, T-29, T-30, T-33, T-34 |
| M (1-3h) | 11 | T-4, T-7, T-10, T-11, T-12, T-13, T-14, T-21, T-23, T-27, T-31, T-32 |
| L (>3h) | 6 | T-3, T-22, T-24, T-35 |

**Total: 35 tests.**
