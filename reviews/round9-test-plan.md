# Round 9 — Test Plan

23 actionable test additions/fixes. Format per entry: source, scope (unit | integration | e2e-python), file path, harness, assertions in pseudo-code, effort (S | M | L).

---

## HIGH (8)

### H-1 — `ConstantFold::rule_and_dist` inner-node fingerprint gap

**Source:** EA1 Finding 1 (`crates/opt/src/constant_fold/rules.rs:62-71`).
The `((a & C1) | (b & C2)) & C3 → (a & (C1&C3)) | (b & (C2&C3))` rewrite emits two fresh `And(_, IntConst(_))` nodes; `pattern::rewrite_rule` only attributes the outermost `Or`. Inner `And` nodes carry empty fingerprints — fails `validate_with_options(check_asm_fingerprints: true)`.

- **Scope:** unit
- **File:** `crates/opt/tests/asm_fingerprint_propagation.rs`
- **Harness:** `make_empty_fn` + `ConstantFold` (mirrors `constant_fold_and_mask_merge_preserves_fingerprints`)
- **Assertions:**
  - Build `((x&0xFF)|(y&0x0F))&0xF0` with fingerprints set on every input
  - Run ConstantFold; for each surviving non-exempt node assert non-empty fingerprint
  - Assert `validate_with_options(check_asm_fingerprints: true)` returns `Ok(())`
- **Effort:** M

### H-2 — `FunctionArgDetect` exact-width consumer fingerprint gap

**Source:** Ask-8 R2 Finding 1 (`crates/opt/src/function_args/mod.rs:329-336`).
When `load_ty == max_type`, `replace_all_uses(old_out, new_out)` redirects without `extend_asm_fingerprint_from`. Non-exempt downstream consumers lose Load's contributing addresses.

- **Scope:** unit
- **File:** `crates/opt/tests/asm_fingerprint_propagation.rs`
- **Harness:** `make_sp_fn` + `FunctionArgDetect`
- **Assertions:**
  - Build `Load(SP+offset) @0x200 → Add(load, IntConst(1)) @0x204`; set fingerprints
  - Run FunctionArgDetect (exact-width path)
  - Assert surviving Add fingerprint contains `0x200`
  - Assert `validate_with_options(check_asm_fingerprints: true)` returns `Ok(())`
- **Effort:** M

### H-3 — `sysret` classified `NoReturn` (post-fix regression pin)

**Source:** EA3 CRITICAL-1 (`crates/target/src/call_other_abi.rs:259`).
SYSRET returns to user-mode at RCX/R11; `NO_RETURN` truncates kernel-entry CFGs.

- **Scope:** unit
- **File:** `crates/target/src/call_other_abi.rs` (inline `#[cfg(test)]`)
- **Assertions:**
  - `assert_ne!(classify(X86_64, "sysret"), Some(CallOtherClass::NoReturn))`
  - Lift bytes `48 0F 07 C3` (SYSRET REX.W ; RET) and assert no dangling-output CallOther
- **Effort:** M (depends on chosen replacement classification)

### H-4 — `wrap_when` leaves dangling graph pointer when `try_borrow` fails

**Source:** Ask-8 R3 ISSUE-1 / 2C #4 (`crates/strider-py/src/pattern.rs:462-464`).

- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_when_predicate.py` (new)
- **Assertions:**
  ```python
  proxy_ref = []
  def capture(m): proxy_ref.append(m); return True
  g.find_all(any().when(capture))
  # Stale access must return None or raise — never segfault.
  assert proxy_ref[0].get_int(c) is None
  ```
- **Effort:** M

### H-5 — `wrap_when` swallows `KeyboardInterrupt`/`SystemExit`

**Source:** 2C #5 / 1F-04 (`crates/strider-py/src/pattern.rs:475-480`).

- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_when_predicate.py` (same)
- **Assertions:**
  ```python
  def raise_interrupt(m): raise KeyboardInterrupt
  with pytest.raises(KeyboardInterrupt):
      g.find_all(any().when(raise_interrupt))
  # Same for SystemExit
  ```
- **Effort:** S (after fix)

### H-6 — `read_or_init_var` silently drops unsupported-size register

**Source:** 2C #1 (`crates/strider/src/orchestrator.rs:786`).
`vn.size.try_into().ok()?` swallows sizes 3/5/7/9 etc.

- **Scope:** integration
- **File:** `crates/strider/tests/orchestrator_indirect_branch.rs`
- **Assertions:** Build CC with synthetic 24-bit ret-val reg; assert `run()` returns `Err(UnsupportedConventionRegSize)` after fix; pre-fix asserts truncated Call-node footprint to document the drop.
- **Effort:** L (needs public per-address-CC API for synthetic varnodes)

### H-7 — `build_anchor_calling_context` clobber loop drops unsupported sizes

**Source:** 2C #2 (`crates/strider/src/orchestrator.rs:729-731`).

- **Scope:** integration
- **File:** `crates/strider/tests/orchestrator_indirect_branch.rs`
- **Assertions:** CC with 3-byte clobber reg; post-fix assert clobbered_kinds list length matches expected; pre-fix documents the drop.
- **Effort:** L

### H-8 — `classify_anchor_with_rom_and_sp` eprintln+None on KB contradiction

**Source:** 2C #3 (`crates/strider/src/indirect_resolve/classify.rs:49-57`).

- **Scope:** integration
- **File:** `crates/strider/tests/indirect_resolve_classify.rs`
- **Assertions:** Construct fixture forcing KB contradiction; post-fix assert classifier returns `Err`, pre-fix asserts `None`.
- **Effort:** L (requires understanding KB contradiction trigger)

---

## IMPORTANT (11)

### I-1 — `Extend(SignExtend, IntConst)` wrong dispatch target

**Source:** 1C Issue 1 / 1B Finding 1 (`crates/opt/src/indirect_branch_resolve/classify.rs:252-269`).

- **Scope:** unit
- **File:** `crates/opt/tests/indirect_branch_resolve.rs`
- **Assertions:**
  - Build `Extend(SignExtend, IntConst(0xFFFF_FFFF, U32), U64)` bypassing ConstantFold
  - Post-fix: `Some(Single(0xFFFF_FFFF_FFFF_FFFF))`; pre-fix: `Some(Single(0x0000_0000_FFFF_FFFF))`
  - ZeroExtend control case stays `0x0000_0000_FFFF_FFFF`
- **Effort:** M

### I-2 — `check_layer_c_control_state` zombie gap

**Source:** Ask-8 R2 Finding 2 (`crates/ir/src/validate/layer_c.rs:56-91`).

- **Scope:** unit
- **File:** `crates/ir/src/validate/tests.rs`
- **Assertions:** Build minimal graph + detached unreachable `ControlState` with non-Control input; post-fix: `validate(...).is_ok()`; pre-fix: `Err(ControlStateNonControlPredecessor)`.
- **Effort:** S

### I-3 — AArch64 `x30` callee-saved parallel test

**Source:** Ask-8 R5 I-1 (`crates/target/src/calling_convention/tests.rs`).

- **Scope:** unit
- **File:** same
- **Assertions:**
  ```rust
  let built = CallingConvention::aarch64_aapcs64().build(&regs).unwrap();
  let x30 = regs.name_to_vn("x30").unwrap();
  assert_eq!(built.link_register_vn(), Some(x30));
  assert!(built.callee_saved_regs().contains(&x30));
  ```
- **Effort:** S

### I-4 — `indirect_branch.rs` test uses `Builder::with_endianness`

**Source:** Ask-8 R5 C-1 (`crates/strider/tests/indirect_branch.rs:91`).

- **Scope:** integration (fix only)
- **Fix:** `Builder::with_endianness(...)` → `Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts)`
- **Effort:** S

### I-5 — `Graph.optimize(pipeline)` second call no-op

**Source:** 1F-03 (`crates/strider-py/src/graph.rs:302-308`).

- **Scope:** e2e-python
- **File:** `crates/strider-py/tests/python/test_optimizer_pipeline.py`
- **Assertions:**
  ```python
  pipe = strider.OptimizerPipeline.empty(); pipe.add(strider.opt.ConstantFold())
  g.optimize(pipe)  # drains
  with pytest.raises(strider.errors.StriderError):
      g.optimize(pipe)  # post-fix: error
  ```
- **Effort:** S

### I-6 — `test_int_cmp_op_recovery` allows phantom op names

**Source:** 1F-01 (`crates/strider-py/tests/python/test_pattern_full_builders.py:351-355`).

- **Scope:** e2e-python (fix only)
- **Fix:** narrow allowed set to `{Equal, Less, Sless, Carry, Scarry, Sborrow}`
- **Effort:** S

### I-7 — Stall budget decrements on count-stable iterations

**Source:** Ask-8 R2 Finding 7 (`crates/strider/src/orchestrator.rs:400-408`).

- **Scope:** integration
- **File:** `crates/strider/tests/orchestrator_indirect_branch.rs`
- **Assertions:** Fixture where each iteration resolves one anchor + materialises one new (count constant, real progress); chain length > stall_budget; post-fix `run()` succeeds, pre-fix returns `Err("resolver stalled")`.
- **Effort:** L

### I-8 — `validate_with_options` Layer-C fingerprint check unit test

**Source:** Ask-8 R2 + correctness-invariants.

- **Scope:** unit
- **File:** `crates/ir/src/validate/tests.rs`
- **Assertions:**
  - Negative: graph with non-exempt unattributed `IntConst`, opt-in flag → `Err`
  - Positive: graph with all nodes attributed via `lift_addr` → `Ok(())`
  - Without the flag: even unattributed graph passes `validate(...)`
- **Effort:** S

### I-9 — `find_all_requirements` shared-capture disagreement Rust unit test

**Source:** 1D verify (Python test exists; no Rust unit test).

- **Scope:** unit
- **File:** `crates/pattern/tests/matching/matcher_api.rs`
- **Assertions:**
  - Disagreeing patterns (`call().capture(c)` + `int_const(5).capture(c)`) on graph with no Call → empty result
  - Agreeing patterns (`int_const(5).capture(c)` × 2) → ≥1 joined tuple
- **Effort:** S

### I-10 — Three skipped typed-exception tests

**Source:** 1F (`crates/strider-py/tests/python/test_typed_errors_e2e.py`).

- **Scope:** e2e-python
- **Assertions:**
  - `RewriteError`: build multi-output node + invalid substitution
  - `UnknownCallOtherError`: bytes `CD 80 C3` (INT 0x80 ; RET on x86 — not in table)
  - `UnresolvedIndirectBranchError`: bytes `FF E0 C3` (JMP RAX with RAX unknown), `max_iterations=1`
- **Effort:** M

### I-11 — `int_const_any_of([])` empty-set vacuous failure (Rust)

**Source:** 1D verify note.

- **Scope:** unit
- **File:** `crates/pattern/tests/matching/wildcards_and_consts.rs`
- **Assertions:**
  ```rust
  a::none(&g, int_const_any_of(std::iter::empty::<u64>()));
  a::none(&g, int_const_any_of([] as [u64; 0]));
  ```
- **Effort:** S

---

## MED (4)

### M-1 — `Tb::neg` dispatches `BitNot` instead of `Neg`

**Source:** 1D (`crates/pattern/tests/matching/support/graph.rs:182-184`).

- **Fix:** `IntUnaryOp::BitNot` → `IntUnaryOp::Neg`
- **Regression test** (`crates/pattern/tests/matching/arithmetic.rs`): build `t.neg(v)`; assert resulting node kind is `IntUnaryOp::Neg`
- **Effort:** S

### M-2 — ELF `Section` failure increments wrong stats counter

**Source:** EA1 Finding 2 / 1E (`crates/reader/src/elf.rs:584`).

- **Scope:** unit
- **File:** `crates/reader/tests/elf_relocations.rs`
- **Assertions:** Mock ELF with `RelocationTarget::Section(u32::MAX)`; post-fix `stats.skipped_malformed_target == 1`, pre-fix `stats.skipped_unresolved_target == 1`.
- **Effort:** M

### M-3 — `cfg/tests/known_targets.rs` `Builder::with_endianness`

**Source:** Ask-8 R5 I-2 (lines 30, 71, 104, 143, 158, 203).

- **Fix:** Replace each call with `Builder::for_arch(&arch, sleigh, base, opts)`.
- **Effort:** S

### M-4 — `cfg/tests/indirect_dispatch.rs` `Builder::new` (ARM)

**Source:** 1B Finding 3 (line 159).

- **Fix:** `Builder::new(sleigh, base, opts)` → `Builder::for_arch(&SleighArch::arm(), sleigh, base, opts)`
- **Effort:** S

---

## Coverage Notes (already-tested gaps)

- Bounded-lift OOB terminator on `cur_addr`: covered by `crates/cfg/tests/build_end_to_end.rs:155-215`
- `vn_io` sub-register aliasing: per-arch integration tests exercise the full width table
- `find_all_requirements` Python disagreement: covered (Python only — see I-9 for missing Rust unit)
- `at_any([])` and `offset_any([])` vacuous failure: `control_flow.rs:48`, `stack.rs:221`
- `validate_with_options` E2E: `crates/strider/tests/asm_fingerprints.rs:57-64` (I-8 adds unit-level path)

---

## Files affected

**New:** `crates/strider-py/tests/python/test_when_predicate.py` (H-4, H-5)

**Modified for additions:** `crates/opt/tests/asm_fingerprint_propagation.rs` (H-1, H-2), `crates/opt/tests/indirect_branch_resolve.rs` (I-1), `crates/ir/src/validate/tests.rs` (I-2, I-8), `crates/target/src/calling_convention/tests.rs` (I-3), `crates/strider-py/tests/python/test_optimizer_pipeline.py` (I-5), `crates/strider/tests/orchestrator_indirect_branch.rs` (H-6, H-7, I-7), `crates/strider/tests/indirect_resolve_classify.rs` (H-8), `crates/strider-py/tests/python/test_typed_errors_e2e.py` (I-10), `crates/pattern/tests/matching/wildcards_and_consts.rs` (I-11), `crates/pattern/tests/matching/arithmetic.rs` (M-1 regression), `crates/reader/tests/elf_relocations.rs` (M-2), `crates/target/src/call_other_abi.rs` (H-3 inline)

**Modified for fixes:** `crates/strider/tests/indirect_branch.rs` (I-4), `crates/strider-py/tests/python/test_pattern_full_builders.py` (I-6), `crates/pattern/tests/matching/support/graph.rs` (M-1), `crates/cfg/tests/known_targets.rs` (M-3), `crates/cfg/tests/indirect_dispatch.rs` (M-4)

**Total:** 23 items (8 HIGH, 11 IMPORTANT, 4 MED). Effort split: 11 S, 6 M, 6 L.
