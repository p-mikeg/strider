# Round 11 — Test Plan

## Section 1 — Correctness Regressions

### T-1: `PhiPat::input(0, p)` addresses the phi-token, not predecessor-0
- **Source finding:** `round11-1D-pattern.md` — MED
- **Severity of underlying bug:** MED
- **Scope:** unit
- **File:** `crates/pattern/tests/matching/phi_input_index.rs` (new)
- **Harness:** Mock `BuiltFunctionGraph` with `VarPhi(vn)` whose inputs are `[phi_token_from_cs, IntConst(0), IntConst(1)]`.
- **Assertions:** `find_all(phi_for(vn).input(0, int_const(0)))` returns 1 match after fix; currently returns 0 because `PhiPat::input(0)` addresses raw slot 0 (phi-token) not predecessor-0 value at slot 1.  After fix (`idx + 1` offset), the count flips.
- **Effort:** M

### T-2: `CallingConvention::build` bypasses `try_from_parts` validator
- **Source finding:** `round11-1E-strider-target-reader.md` — F-1 (88)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/target/tests/cc_validation.rs`
- **Harness:** Synthetic CC with RSP in `arg_passing_regs` against `x86_64()` regs.
- **Assertions:** Currently `build` returns `Ok`; after fix routes through `try_from_parts`, returns `Err` naming the SP overlap.  All 17 existing presets still resolve cleanly (regression guard).
- **Effort:** M

### T-3: `R_MIPS_REL32` writes 8 bytes on MIPS64 (should be 4)
- **Source finding:** `round11-1E-strider-target-reader.md` — F-2 (80)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/reader/tests/elf_relocations.rs`
- **Harness:** Hand-built MIPS64 ELF with one `R_MIPS_REL32` entry; sentinel bytes `0xDE` at offsets 4–11.
- **Assertions:** After `apply_elf_relocations`, sentinel bytes unchanged; `RelocationStats.applied == 1`.  Companion MIPS32 test pins the correct 4-byte arm.
- **Effort:** L

### T-4: RDPKRU `handle_call_other` clobber overwrites modeled value output
- **Source finding:** `round11-1E-strider-target-reader.md` — F-4 (85)
- **Severity:** MED
- **Scope:** integration
- **File:** `crates/strider/tests/call_other_precise_abi.rs`
- **Harness:** Bytes `[0x0F, 0x01, 0xEE, 0xC3]` (RDPKRU, RET) at 0x1000 via `MemoryMap`.
- **Assertions:** EAX-tracking variable resolves to the modeled `CallOther` value-output (not a clobber slot); modeled output has ≥1 use-list consumer.
- **Effort:** L

### T-5: `PyMemReaderAdapter` swallows `KeyboardInterrupt` / `SystemExit`
- **Source finding:** `round11-1F-strider-py-aux.md` — F-1 (92)
- **Severity:** HIGH
- **Scope:** python e2e
- **File:** `crates/strider-py/tests/python/test_read_only_memory_kbd_interrupt.py`
- **Harness:** `MemReader` subclass raising `KeyboardInterrupt` on every `read`; non-empty `MemoryMap`.
- **Assertions:** `pytest.raises(KeyboardInterrupt)` after fix.  Currently raises `ReaderError`.  Mirror test for `SystemExit`.
- **Effort:** M

### T-6: Non-x86 Python `build_cfg` snapshot uses wrong `ArchPreset`
- **Source finding:** `round11-1F-strider-py-aux.md` — F-2 (90)
- **Severity:** HIGH
- **Scope:** python e2e
- **File:** `crates/strider-py/tests/python/test_build_cfg_arch_preset.py` (new)
- **Harness:** AArch64 bytes `0x00 0x00 0x20 0xD4` (`brk #0`) + `0xC0 0x03 0x5F 0xD6` (`ret`).  `brk` lifts as `NoReturn` CallOther under AArch64.
- **Assertions:** `result.cfg`'s region count equals `result.graph`'s reachable ControlState count.  Currently mismatches (X86_64 default → wrong CallOther class).
- **Effort:** M

### T-7: `strider.run` orchestrator path raises generic `StriderError` instead of `LiftError`
- **Source finding:** `round11-1F-strider-py-aux.md` — F-5 (82)
- **Severity:** MED
- **Scope:** python e2e
- **File:** `crates/strider-py/tests/python/test_typed_errors_e2e.py`
- **Harness:** Empty `MemoryMap`, `entry=0x1000`, `allow_code_before_start_addr=True`.
- **Assertions:** `pytest.raises(errors.LiftError)` (not bare `StriderError`).  Tighten the existing `test_lift_error_subclass_when_explicit_lift_fails` to drop the `UnknownCallOtherError` branch.
- **Effort:** S

### T-8: `mem_chain_is_dirty` returns false for a malformed zero-input `Call` node
- **Source finding:** `round11-2C-silent-failures.md` — F1 (MED)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/opt/tests/function_args_dirty_chain.rs` (new)
- **Harness:** Mock graph with `Call` whose input list has only the control edge.
- **Assertions:** `mem_chain_is_dirty(load_input)` returns `Err` after fix; currently `Ok(false)` (the unsafe direction for aliasing).
- **Effort:** M

### T-9: `apply_in_place_edits` silently skips `InitialVar` with wrong output arity
- **Source finding:** `round11-2C-silent-failures.md` — F2 (MED)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/strider/tests/orchestrator_indirect_resolution.rs`
- **Harness:** `InitialVar(vn)` constructed with empty output list via `Graph::create_node`.
- **Assertions:** Currently the `if let Ok([out]) = ...` skips silently; after fix, `apply_in_place_edits` returns `Err` naming the `Vn` and "InitialVar output arity".
- **Effort:** L

### T-10: `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- **Source finding:** `round11-1C-opt.md` — Finding 1 (HIGH; 89 errors)
- **Severity:** HIGH
- **Scope:** integration (CI gate)
- **Files to fix:** `crates/opt/src/{test_support.rs:18, stack_store/tests.rs:6, constant_fold/tests.rs:15-27, known_bits/tests.rs:8-15, load_readonly/tests.rs:24-31, if_cond_inversion/tests.rs:43-59}`
- **Assertions:** Delete shadow `return_value`/`return_kind`/`find_unique_if` test helpers; remove unused `BuiltFunctionGraph` imports; `cargo clippy --all-targets -- -D warnings` exits 0; `cargo test --package opt` still passes.
- **Effort:** M

### T-11: Empty-insns `Branch` region drops the IR outgoing control edge
- **Source finding:** `round11-1B-pcode-lift-cfg.md` — MED
- **Severity:** MED
- **Scope:** integration
- **File:** `crates/strider/tests/control.rs`
- **Harness:** Synthetic CondBranch where fallthrough is OOB but taken-branch is in-bounds (forces empty Branch region).
- **Assertions:** `validate(graph, entry)` returns `Ok(())`; `unresolved_branches.is_empty()`; two reachable `ControlState` nodes connected.
- **Effort:** L

### T-12: `ReadOnlyMemory.read` adapter calls 2-arg; pin contract
- **Source finding:** `round11-1F-strider-py-aux.md` — F-3 (85), F-4 (84)
- **Severity:** MED
- **Scope:** python e2e
- **File:** `crates/strider-py/tests/python/test_callback_reader.py`
- **Harness:** `LoggingRom(ReadOnlyMemory)` with 2-arg `read(addr, size)` and a call-arg log.
- **Assertions:** Every logged call has 2 args.  A 3-arg subclass forces `TypeError` (negative test).
- **Effort:** S

### T-13: `set_node_kind` accepts signature-mismatched kinds silently
- **Source finding:** `round11-1A-ir.md` — Finding 2 (MED)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/ir/tests/build_validate_roundtrip.rs`
- **Harness:** `IndirectBranch` (3 inputs, 0 outputs) → swap to `Call` (3 inputs, multiple outputs).
- **Assertions:** After fix, `set_node_kind` returns `Err` naming both kinds and the slot-count mismatch.  Same-shape swap `IndirectBranch → Return` still returns `Ok(())` (valid path).
- **Effort:** M

### T-14: `bit_mask_u128` doc says U256 is rejected; code returns `u128::MAX`
- **Source finding:** `round11-1A-ir.md` — Finding 1 (MED)
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/ir/src/node/output_type.rs` inline tests
- **Assertions:** `U256.get_unsigned_int(0xABCDu128) == Some(0xABCDu128)` (pins current behaviour); `U256.bit_mask_u128() == u128::MAX`; `FunctionBuilder::build_int_const(_, U256)` returns `Err` (the real rejection site).  Forces a doc-or-code reconciliation.
- **Effort:** S

## Section 2 — Documentation Pins

### T-15: Six malformed merged doc-comments render literal `///` in rustdoc
- **Source finding:** `round11-3B-comments.md` — Findings 1–6 (HIGH)
- **Severity:** HIGH
- **Scope:** integration
- **Files to fix:** `crates/cfg/src/cfg/options.rs:9, 99`; `crates/cfg/src/cfg/types.rs:79`; `crates/ir/src/builder/mod.rs:410`; `crates/target/src/arch.rs:150`; `crates/target/src/calling_convention/tests.rs:682`
- **Assertions:** `cargo doc --workspace --no-deps -W rustdoc::broken-intra-doc-links` produces 0 warnings (currently 89+).
- **Effort:** M

### T-16: `target` README lists wrong `ArchPreset` casing (`X8664` vs `X86_64`)
- **Source finding:** `round11-3A-doc-verify.md` — Finding 22 (REFUTED)
- **Severity:** LOW
- **Scope:** unit
- **File:** `crates/target/tests/arch_smoke.rs`
- **Assertions:** `let _ = [ArchPreset::X86_64, MipsBe32, MipsLe32, MipsBe64, MipsLe64];` compiles.  Test breaks if any variant is renamed to the README's wrong casing, forcing README sync.
- **Effort:** S

### T-17: `build_optimizer_pipeline` doc undercounts (lists 4, has 6)
- **Source finding:** `round11-3B-comments.md` — Finding 35 (HIGH)
- **Severity:** HIGH
- **Scope:** unit
- **File:** `crates/strider/tests/optimizer_pipeline_subsets.rs`
- **Assertions:** `optimizer_names()` contains both `"FlagCmpCanonicalize"` and `"IfCondInversion"`; base-pass count `>= 6`.  Pins the implementation so the doc is forced to update.
- **Effort:** S

### T-18: SKILL `strider-target-arch` CC preset line citations off by 60–300 lines
- **Source finding:** `round11-skill-audit.md` — strider-target-arch NEEDS-UPDATE-MAJOR
- **Severity:** LOW
- **Scope:** unit (living test)
- **File:** `crates/target/tests/cc_validation.rs`
- **Assertions:** `grep -n` for each preset returns a line within ±5 of a stored `const`.  Mark `#[ignore]` for normal CI; included in `--include-ignored skill_freshness`.
- **Effort:** S

## Section 3 — Cross-Arch Coverage Gaps

### T-19: `arm_be` preset missing from `*_preset_resolves` and endianness smoke tests
- **Source finding:** `round11-1E-strider-target-reader.md` — N-4
- **Severity:** LOW
- **Scope:** unit
- **File:** `crates/target/tests/arch_smoke.rs`
- **Assertions:** `arm_be_preset_resolves` calls `assert_preset_resolves("arm_be", SleighArch::arm_be())`.  Add `("arm_be", _, Endianness::Big)` to the parametric `presets_endianness_matches_arch` cases.
- **Effort:** S

### T-20: MIPS64 `R_MIPS_REL32` adjacent-byte corruption (cross-arch view of T-3)
- **Source finding:** `round11-1E` — F-2
- **Severity:** MED
- **Scope:** unit
- **File:** `crates/reader/tests/elf_relocations.rs`
- **Assertions:** Same as T-3 with companion MIPS32 fixture.
- **Effort:** L (consolidated with T-3)

### T-21: AArch64 `strider.run` snapshot CFG disagrees with IR (cross-arch view of T-6)
- **Source finding:** `round11-1F` — F-2
- **Severity:** HIGH
- **Scope:** python e2e
- **File:** Same as T-6.
- **Effort:** M

## Section 4 — Existing-Deferred Items

| Round-10 item | Round-11 finding | Test |
|---------------|------------------|------|
| T-3 (MIPS reloc) | 1E F-2 | T-3 |
| T-9 (apply_in_place_edits skip) | 2C F2 | T-9 |
| T-10 (clippy gate) | 1C Finding 1 | T-10 |
| T-14 (bit_mask doc contradiction) | 1A Finding 1 | T-14 |
| T-22 (arm_be smoke) | 1E N-4 | T-19 |

All five are reachable via failing-test scaffolds in earlier sections.  No unreachability claim required.

### T-22: `decompose_sp` And-arm uses recursion inside an iterative function
- **Source finding:** `round11-1C-opt.md` — Finding 4
- **Severity:** LOW
- **Scope:** scale
- **File:** `crates/opt/src/sp_expr.rs` inline tests
- **Harness:** 1000-deep nested `And(And(... And(sp_expr, mask) ..., mask), mask)` chain.
- **Assertions:** `decompose_sp` at depth 1000 returns without stack overflow.  Mark `#[ignore]` until And-arm is converted to iterative.
- **Effort:** M

## Section 5 — Performance / Scale Tests

### T-23: `Worklist::enqueue` two-pass — pin single-pass contract
- **Source finding:** `round11-1F` — F-6 (80)
- **Severity:** LOW (perf)
- **Scope:** unit
- **File:** `crates/entity-utils/src/worklist.rs` inline tests
- **Assertions:** After collapsing `enqueue` to `if self.workset.insert(entity) { ... }`, add `enqueue_dedup_at_ten_thousand_scale`: 10k items in / 10k unique out.
- **Effort:** S

### T-24: `KnownBits` stale known-map causes extra fixed-point iterations on 10k+ nodes
- **Source finding:** `round11-1C-opt.md` — Finding 14
- **Severity:** LOW
- **Scope:** scale
- **File:** `crates/strider/benches/scaling.rs`
- **Harness:** `run_diamond_cfg(5000)`.
- **Assertions:** Pipeline converges in `<= 3` passes on the diamond fixture.  Catches regressions that push convergence to 5+.
- **Effort:** M

### T-25: `Kb { ones, zeros }` struct-literal bypasses the disjointness invariant
- **Source finding:** `round11-2D-types.md` — Finding 12 (HIGH)
- **Severity:** HIGH (silent wrong analysis)
- **Scope:** unit
- **File:** `crates/opt/src/known_bits/mod.rs` inline tests
- **Assertions:** `Kb::try_new(0xFF, 0xFF)` returns `Err`; `Kb::try_new(0xF0, 0x0F)` returns `Ok`; `Kb::default() == Kb { ones: 0, zeros: 0 }`.  After tightening fields to `pub(crate)`, `#[doc = compile_fail]` example pins the encapsulation.
- **Effort:** S

## Summary Table

| Section | Tests | IDs | Estimated effort |
|---------|-------|-----|------------------|
| 1. Correctness regressions | 14 | T-1 through T-14 | ~28 hr (2 L + 8 M + 4 S) |
| 2. Documentation pins | 4 | T-15 through T-18 | ~4 hr (1 M + 3 S) |
| 3. Cross-arch coverage gaps | 3 | T-19 through T-21 | ~4 hr (1 L + 1 M + 1 S) |
| 4. Existing-deferred items | 1 net new (T-22) | T-22 | ~2 hr (1 M) |
| 5. Performance / scale | 3 | T-23 through T-25 | ~4 hr (1 M + 2 S) |
| **Total** | **25** | | **~42 hr** |

Effort key: S = ≤30 min, M = ≤2 hr, L = ≥half-day.
