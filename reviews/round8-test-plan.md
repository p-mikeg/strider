# Round 8 — Test Plan

**Branch:** `review/ai2`.  Synthesized from Round 1-3 and Asks 16-18 review reports.

Each entry traces to a specific HIGH or MED finding.  **Effort**: XS (<30 min), S (30-90 min), M (2-4 h), L (needs new ELF fixture + Makefile target).

---

## A. Asm-fingerprint contract enforcement

### A-1: `IfCondInversion` drops BoolNeg fingerprint
- **Finding:** `round8-correctness-invariants.md` H-2 — `invert()` at `crates/opt/src/if_cond_inversion/mod.rs:101-105` makes `BoolNeg` dead but never calls `extend_asm_fingerprint_from(inner_node, bool_neg_node)`.
- **Scope:** unit. **Location:** `crates/opt/tests/asm_fingerprint_propagation.rs`.
- **Harness:** Build `If(BoolNeg(IntConst(1) == IntConst(0)))` with explicit lift-addr 0x100 on BoolNeg and 0x104 on the inner Equal.  Run `IfCondInversion`.  Assert the surviving inner-cond node's fingerprint contains 0x100.
- **Effort:** S.

### A-2: `StackLoadForward` BE narrow path emits un-attributed `ShiftRight`
- **Finding:** `round8-correctness-invariants.md` H-1.
- **Scope:** unit.  **Location:** `crates/opt/tests/asm_fingerprint_propagation.rs`.
- **Harness:** BE-target synthetic graph with `StackStore` U32 → `Load` U16 at same offset.  After pass, walk reachable nodes; assert no non-exempt node has empty fingerprint.
- **Effort:** M.

### A-3: `FlagCmpCanonicalize` rule fingerprint coverage via `validate_with_options`
- **Finding:** `round8-1C-opt.md` LOW.
- **Scope:** unit.  **Location:** `crates/opt/tests/asm_fingerprint_propagation.rs`.
- **Harness:** Synthetic AArch64 flag-tree shape covered by RULES.  After pass, call `validate_with_options(check_asm_fingerprints: true)`.
- **Effort:** S.

### A-4: End-to-end `validate_with_options` on MIPS BE arithmetic fixture
- **Finding:** `round8-correctness-invariants.md` H-1 (BE coverage gap).
- **Scope:** integration.  **Location:** `crates/strider/tests/asm_fingerprints.rs`.
- **Harness:** `analyze(Arch::Mips32be, "arithmetic", "add")` + `validate_with_options(check_asm_fingerprints: true)`.
- **Effort:** XS.

## B. Validator Layer-C false positives

### B-1: `check_layer_c_function_arg_uniqueness` zombie false positive
- **Finding:** `round8-1A-ir.md` MED.
- **Scope:** unit.  **Location:** `crates/ir/tests/build_validate_roundtrip.rs`.
- **Harness:** Build graph with two `FunctionArg` for same index; GC one via `retain_reachable`; call `validate`; assert `Ok`.
- **Effort:** M.

### B-2: Layer-C validate scales correctly on zombie-heavy graphs
- **Finding:** `round8-16-perf.md` Finding C.
- **Scope:** unit.  **Location:** `crates/ir/tests/build_validate_roundtrip.rs`.
- **Harness:** Run `RedundantPhis` to create zombies, then `validate`; assert `Ok` with `arena_count > preorder_count`.
- **Effort:** S.

## C. Bounded-arithmetic / wide-const correctness

### C-1: `build_int_const` rejects U512 with `Err`
- **Finding:** `round8-1A-ir.md` HIGH.
- **Scope:** unit.  **Location:** `crates/ir/tests/build_validate_roundtrip.rs`.
- **Harness:** Direct call `b.build_int_const(42u64, NodeOutputType::U512)`; assert `is_err()`.
- **Effort:** XS.

### C-2: `handle_insert` for U128 destination preserves upper 64 bits
- **Finding:** `round8-1B-pcode-lift-cfg.md` IMPORTANT.
- **Scope:** unit.  **Location:** `crates/pcode-lift/tests/value_lifter.rs`.
- **Harness:** `Insn { opcode: Insert, output: u128_reg, inputs: [src, const(64), const(64)] }`.  Inspect AND-mask IntConst — must be 128-bit covering bits 64-127.
- **Effort:** M.

### C-3: `handle_extract` for >64-bit narrow output
- **Finding:** `round8-1B-pcode-lift-cfg.md` IMPORTANT.
- **Scope:** unit.  **Location:** `crates/pcode-lift/tests/value_lifter.rs`.
- **Harness:** `Insn { opcode: Extract, output: u128_narrow_reg, inputs: [u128_src, const(0), const(80)] }`.  AND-mask must be `(1u128 << 80) - 1`.
- **Effort:** M.

## D. Cross-arch CallOther via `strider::run` (orchestrator preset bug)

### D-1: AArch64 HVC/SMC does not raise `UnknownCallOtherError` via `strider::run`
- **Finding:** `round8-correctness-cross-arch.md` CRITICAL.
- **Scope:** integration.  **Location:** new `crates/strider/tests/cross_arch_callother_run.rs`.
- **Harness:** AArch64 HVC bytes (`0xD4000002` LE) + `aarch64_aapcs64`; `strider::run`; assert `is_ok()`.
- **Effort:** S.

### D-2: ARM `swi` reads correct register set via `strider::run`
- **Finding:** `round8-correctness-cross-arch.md` CRITICAL.
- **Scope:** integration.  **Location:** `crates/strider/tests/cross_arch_callother_run.rs`.
- **Harness:** ARM `swi #0` bytes; assert resulting CallOther has per-node clobber override containing `r0`.
- **Effort:** S.

### D-3: `common::analyze` test helper preset discrepancy
- **Finding:** `round8-correctness-cross-arch.md` CRITICAL.
- **Scope:** unit.  **Location:** `crates/strider/tests/cross_arch_callother_run.rs`.
- **Harness:** Compare `Builder::with_endianness` vs `Builder::for_arch` on AArch64 bytes; document divergence; flip to positive assertion after fix.
- **Effort:** S.

## E. CallOther ABI table coverage

### E-1: `mfence`/`sfence`/`lfence` classify correctly
- **Finding:** `round8-17-graph-soundness.md` D-1 HIGH.
- **Scope:** unit.  **Location:** new `crates/target/tests/call_other_fence.rs`.
- **Harness:** `target::call_other_abi::classify(preset, name)` for each fence × `{X86, X86_64}`; assert `Some(Call(abi))` with `memory_edge=true`.
- **Effort:** XS.

### E-2: `mfence` lifts via `strider::run` without error
- **Finding:** `round8-17-graph-soundness.md` D-1.
- **Scope:** integration.  **Location:** new `crates/strider/tests/fence_ops.rs`.
- **Harness:** `mfence` (`0x0F 0xAE 0xF0`) + `ret` (0xC3); `strider::run`; assert `Ok`.
- **Effort:** S.

### E-3: `mfence` fixture in `fixtures/cases/builtins.c`
- **Finding:** `round8-17-graph-soundness.md` RT-3.
- **Scope:** fixture-based.  **Location:** `fixtures/cases/builtins.c` + `fixtures/Makefile` + `crates/strider/tests/builtins.rs`.
- **Effort:** L.

## F. Indirect-branch resolver multi-arch coverage

### F-1: AArch64 link-register at ≥3 sites
- **Finding:** `round8-17-graph-soundness.md` E (gap).
- **Scope:** integration.  **Location:** `crates/strider/tests/indirect_resolve_classify.rs`.
- **Harness:** `analyze(Arch::Aarch64, "calls", "multiple_calls")`; count `ResolvedTargets::LinkRegister`; assert ≥3.
- **Effort:** S.

### F-2: AArch64 jump-table fixture + end-to-end
- **Finding:** `round8-17-graph-soundness.md` E (gap).
- **Scope:** fixture-based.  **Location:** `fixtures/cases/switch.c` (AArch64 variant) + Makefile + `crates/strider/tests/jump_table_lifting.rs`.
- **Effort:** L.

### F-3: Lift-time canonicalisation shape verification
- **Finding:** `round8-17-graph-soundness.md` F (gap).
- **Scope:** integration.  **Location:** `crates/strider/tests/arithmetic.rs`.
- **Harness:** `analyze("arithmetic", "sub_expr")`; assert zero `Sub` nodes; ≥1 `IntUnaryOp::Neg`.
- **Effort:** XS.

## G. PyO3 boundary

### G-1: `PyVnSpace.__hash__` consistent with `__eq__`
- **Finding:** `round8-1F-strider-py-aux.md` HIGH.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_sleigh.py`.
- **Harness:** Two `VnSpace.ram()` instances; assert equal hashes; `dict` lookup works.
- **Effort:** XS.

### G-2: `Match.__getitem__` returns correct Python int for U128 with bit 127 set
- **Finding:** `round8-1F-strider-py-aux.md` MED.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_pattern_match.py`.
- **Harness:** Synthetic graph with `IntConst(2**128 - 1)`; assert `m["v"] == 2**128 - 1` and `> 0`.
- **Effort:** S.

### G-3: `Graph.find_all` with mutating `.when()` predicate raises typed error, not deadlock
- **Finding:** `round8-correctness-borrowing.md` HIGH.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_pattern_match.py`.
- **Harness:** `.when(lambda m: g.reoptimize())`; assert raises `StriderError`, not blocks.
- **Effort:** M.

### G-4: `Match::get_vn` for `CallOther` with override length differing from default
- **Finding:** `round8-1D-pattern.md` HIGH.
- **Scope:** unit.  **Location:** `crates/pattern/tests/get_vn_with_callother_clobber.rs`.
- **Harness:** Function-default clobber `[rax, rbx]` (len 2); per-node override `[rcx]` (len 1); assert `m.get_vn(c, &bfg) == Some(rcx)`.
- **Effort:** S.

## H. PowerPC CR canonicalisation gap

### H-1: PPC32be `clamp` lifts and validates
- **Finding:** `round8-correctness-cross-arch.md` MED.
- **Scope:** integration.  **Location:** new `crates/strider/tests/ppc_cr_lift.rs`.
- **Harness:** `analyze(Arch::Ppc32be, "control", "clamp")`; assert validates and contains ≥1 `If`.
- **Effort:** S.

### H-2: PPC32be switch documents resolution outcome
- **Finding:** `round8-correctness-cross-arch.md` MED.
- **Scope:** integration.  **Location:** `crates/strider/tests/ppc_cr_lift.rs`.
- **Harness:** `run_orchestrator_on(Arch::Ppc32be, "switch", "dispatch_value")`; pin current behaviour (`Ok` with no placeholders, or `Err(UnresolvedIndirectBranch)`); flip to positive when CR rules land.
- **Effort:** S.

## I. `decompose_sp` deep-recursion regression

### I-1: 5000-node SP chain does not stack-overflow
- **Finding:** `round8-correctness-edge-cases.md` H1 HIGH.
- **Scope:** unit.  **Location:** new `crates/opt/tests/sp_expr_deep.rs`.
- **Harness:** Synthetic `sp - 1 - 1 - ... - 1` chain (5000 nodes); run `StackStoreDetect`; assert `is_ok()` (no abort).
- **Effort:** M.

## J. StackStorePhi empty-offsets sound default

### J-1: Empty offsets returns `MayAlias`, not `PassThrough`
- **Finding:** `round8-correctness-edge-cases.md` H2 HIGH.
- **Scope:** unit.  **Location:** `crates/opt/tests/pipeline_with_stack.rs`.
- **Harness:** Manually create `StackStorePhi` without `set_stack_phi_offsets`; assert `step_through_stack_store_phi` returns `MayAlias`.
- **Effort:** S.

### J-2: Validator opt-in check for empty `stack_phi_offsets`
- **Finding:** `round8-correctness-edge-cases.md` H2.
- **Scope:** unit.  **Location:** `crates/ir/tests/build_validate_roundtrip.rs`.
- **Harness:** Reachable `StackStorePhi` with empty side-table; `validate_with_options(check_stack_phi_offsets: true)`; assert `Err`.
- **Effort:** M.

## K. Production panic regression checks

### K-1: `flag_cmp_canonicalize` rhs binding produces distinct outputs
- **Finding:** `round8-2C-silent-failures.md` H1.
- **Scope:** unit.  **Location:** new `crates/opt/tests/flag_cmp_rhs_bind.rs`.
- **Harness:** Eligible flag shape; run pass; assert `lhs_output != rhs_output` post-replacement.
- **Effort:** S.

### K-2: `apply_tail_call` with non-integer target propagates `Err`
- **Finding:** `round8-2C-silent-failures.md` H4.
- **Scope:** unit.  **Location:** `crates/opt/tests/indirect_branch_resolve.rs`.
- **Harness:** `IndirectBranch` placeholder with target type `NodeOutputKind::Memory`; assert `apply_tail_call` returns `Err`.
- **Effort:** S.

### K-3: Missing `AnchorCallingContext` propagates `Err`
- **Finding:** `round8-2C-silent-failures.md` H5.
- **Scope:** unit.  **Location:** `crates/opt/tests/indirect_branch_resolve.rs`.
- **Harness:** `unresolved_anchors=[addr_A]` with empty `anchor_contexts`; `IndirectBranchResolve.optimize`; assert `Err`.
- **Effort:** S.

## L. Naming / doc-fix verification

### L-1: `pcode-lift` lib doc "(planned)" gone
- **Finding:** `round8-3B-comments.md` HIGH.
- **Scope:** CI lint.  **Harness:** `grep -r "(planned)" crates/pcode-lift/src/` returns empty.
- **Effort:** XS.

### L-2: `strider::run` example points at real fixture
- **Finding:** `round8-3B-comments.md` HIGH.
- **Scope:** integration.  **Location:** `crates/strider/examples/strider.rs`.
- **Harness:** `cargo run -p strider --example strider` exits 0.
- **Effort:** XS.

### L-3: `Graph::asm_fingerprints` doc exempt set matches validator
- **Finding:** `round8-3B-comments.md` HIGH.
- **Scope:** unit.  **Location:** `crates/ir/tests/build_validate_roundtrip.rs`.
- **Harness:** Enumerate `KNOWN_EXEMPT_KINDS`; assert each is in `asm_fingerprint_exempt`.
- **Effort:** XS.

### L-4: Python `PhiPat` does not match `MemPhi`
- **Finding:** `round8-3B-comments.md` HIGH.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_pattern_full_builders.py`.
- **Harness:** `g.find_all(pattern.phi())` and `g.find_all(pattern.mem_phi())` produce disjoint root sets.
- **Effort:** S.

### L-5: `match.vn(c)` returns `None` for VarPhi/FunctionArg bindings
- **Finding:** `round8-3B-comments.md` HIGH.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_pattern_match.py`.
- **Effort:** XS.

### L-6: `MemPhiPat` and `ValuePhiPat` re-exported from `pattern::lib`
- **Finding:** `round8-1D-pattern.md` MED.
- **Scope:** compile.  **Location:** `crates/pattern/tests/matching.rs`.
- **Harness:** `fn _accepts(_: pattern::MemPhiPat) {}` etc.
- **Effort:** XS.

### L-7: `EXPECTED_PATTERN` snapshot bidirectional
- **Finding:** `round8-1F-strider-py-aux.md` MED.
- **Scope:** Python unit.  **Location:** `crates/strider-py/tests/python/test_public_api_snapshot.py`.
- **Harness:** `assert actual == EXPECTED_PATTERN` (both directions); add the missing `bit_not`, `xor`, `phi_for`, `mem_phi`, `value_phi`, `int_cmp`, `initial_var_for`, `function_arg_reg`, `function_arg_stack`, `signed_int_const`, `CastMask`, `PhiPat`, `MemPhiPat`, `ValuePhiPat`, `CallOtherPat`, `FunctionArgPat`, `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`, `IfPat`, `RetPat` to `EXPECTED_PATTERN`.
- **Effort:** XS.

## Coverage summary

| ID | Theme | Effort |
|----|-------|--------|
| A-1..A-4 | Asm-fingerprint | S, M, S, XS |
| B-1, B-2 | Validator Layer-C false positives | M, S |
| C-1..C-3 | Wide-const correctness | XS, M, M |
| D-1..D-3 | Cross-arch CallOther via `strider::run` | S, S, S |
| E-1..E-3 | mfence/sfence/lfence | XS, S, L |
| F-1..F-3 | Indirect resolver multi-arch | S, L, XS |
| G-1..G-4 | PyO3 boundary | XS, S, M, S |
| H-1, H-2 | PPC CR | S, S |
| I-1 | `decompose_sp` deep recursion | M |
| J-1, J-2 | StackStorePhi empty offsets | S, M |
| K-1..K-3 | Panic regression | S, S, S |
| L-1..L-7 | Doc + naming verification | XS×6, S |

**Total: 37 entries.**  Fixtures requiring Makefile changes: E-3 (mfence_barrier), F-2 (AArch64 switch).

## New files

- `crates/strider/tests/cross_arch_callother_run.rs` — D-1, D-2, D-3
- `crates/strider/tests/fence_ops.rs` — E-2
- `crates/target/tests/call_other_fence.rs` — E-1
- `crates/strider/tests/ppc_cr_lift.rs` — H-1, H-2
- `crates/opt/tests/sp_expr_deep.rs` — I-1
- `crates/opt/tests/flag_cmp_rhs_bind.rs` — K-1

## Existing files to extend

- `crates/opt/tests/asm_fingerprint_propagation.rs` — A-1, A-2, A-3
- `crates/strider/tests/asm_fingerprints.rs` — A-4
- `crates/ir/tests/build_validate_roundtrip.rs` — B-1, B-2, C-1, J-2, L-3
- `crates/pcode-lift/tests/value_lifter.rs` — C-2, C-3
- `crates/strider/tests/indirect_resolve_classify.rs` — F-1
- `crates/strider/tests/arithmetic.rs` — F-3
- `crates/pattern/tests/get_vn_with_callother_clobber.rs` — G-4
- `crates/opt/tests/indirect_branch_resolve.rs` — K-2, K-3
- `crates/opt/tests/pipeline_with_stack.rs` — J-1
- `crates/strider/tests/jump_table_lifting.rs` — F-2
- `crates/strider-py/tests/python/test_sleigh.py` — G-1
- `crates/strider-py/tests/python/test_pattern_match.py` — G-2, G-3, L-5
- `crates/strider-py/tests/python/test_pattern_full_builders.py` — L-4
- `crates/strider-py/tests/python/test_public_api_snapshot.py` — L-7
- `crates/pattern/tests/matching.rs` — L-6
- `fixtures/cases/builtins.c` + `fixtures/Makefile` + `crates/strider/tests/builtins.rs` — E-3
