# Round 13 — Test plan

Branch: `review/ai7`.  Investigated 16 required gaps + 2 additional items.

## Summary

**Genuine gaps: 7. Already-covered: 11.**

## Genuine gaps

### T-1 — `vn_io` partial-write when container is `VarPhi` (phi-live)
- **Scope:** unit; **Where:** new `crates/pcode-lift/tests/vn_io_phi_live.rs`
- **Why:** `vn_io_partial_write.rs` exercises `write_vn(AL, val)` only when container's current value is `InitialVar(RAX)`.  The same path runs when the container is a `VarPhi` at a join point; if `read_reg_vn` returns the wrong output-id for VarPhi, the merge silently clobbers join-point bits.
- **Harness:** `FunctionBuilder` + 2 regions + join VarPhi + `ValueLifter`.  No ELF.
- **Expected:** Post-`write_vn(AL, val)`, `read_vn(EAX)` has `VarPhi(RAX)` in its ancestry.
- **Effort:** S (~60 LOC).

### T-2 — Asm-fingerprint superset-only contract as a proptest
- **Scope:** property; **Where:** new test in `crates/opt/tests/asm_fingerprint_propagation.rs`
- **Why:** Existing `default_pipeline_never_shrinks_asm_fingerprints` is deterministic over a single hand-crafted graph.  Proptest over random node orderings + random `lift_addr` values would surface structural issues the snapshot misses.
- **Harness:** `proptest` (already used in `ir/tests/proptest_graph_invariants.rs`).
- **Expected:** For all generated graphs, post-pipeline node fingerprint ⊇ pre-pipeline fingerprint.
- **Effort:** S (~50 LOC).

### T-3 — `find_all_requirements` 3-pattern shared-capture join
- **Scope:** unit; **Where:** add to `crates/pattern/tests/matching/matcher_api.rs:787+`
- **Why:** Existing tests cover only 2-pattern joins.  3-way join exercises the early-break code path when 3rd pattern disagrees on a shared capture.
- **Harness:** `graphmock` / `Tb`; no ELF.
- **Expected:** `find_all_requirements(&[P1, P2, P3])` returns only tuples where every shared capture agrees across all three.
- **Effort:** S (~40 LOC).

### T-4 — Re-enable `indirect_branch_resolved_aarch64be` after verifying the Or(SP,K) fix
- **Scope:** integration; **Where:** `crates/strider/tests/indirect_branch.rs:215`
- **Why:** Round 12 IRA-2 claimed the gap was fixed; round 13 1B re-derived and found the ignore reason is still real (no `Or` arm in `flatten_add_tree`).  Either fix the `Or` arm OR leave the ignore in place — but the resolution must match the code state.
- **Effort:** S — either remove `#[ignore]` + run + verify, or land the `classify_stack_array` `Or(SP,K)` arm + remove.

### T-5 — `decompose_sp` deep And-chain stack-overflow regression
- **Scope:** scale; **Where:** add to `crates/opt/src/sp_expr.rs` test mod after line 893
- **Why:** Add-chain has a 5000-node overflow regression test; And-chain has only single-level tests.  If And-arm iterative form reverts, no current test catches it.
- **Harness:** in-process; 1000-level `And(And(...And(SP, mask)...), mask)` chain.
- **Expected:** No panic; result `Some(SpExpr::Terminal { offset: 0, .. })` with outermost And as opaque base.
- **Effort:** S (~30 LOC).

### T-6 — `Strider::new` smoke for all 16 arch + CC variants
- **Scope:** unit (smoke); **Where:** new `crates/strider/tests/strider_new_smoke.rs`
- **Why:** ELF-fixture tests only exercise some arches; non-fixture arches (`ArmBe`, `Mips64le`, `Ppc32le`, ...) could break at construction silently.
- **Harness:** no ELF; `Strider::new(arch, regs, cc).expect(...)` for each Arch enum variant.
- **Expected:** All 16 wrappers construct without panic.
- **Effort:** S (~50 LOC).

### T-7 — Cross-arch asm-fingerprint e2e (AArch64 + MIPS)
- **Scope:** integration; **Where:** add to `crates/strider/tests/asm_fingerprints.rs:127+`
- **Why:** Existing `validate_with_options(check_asm_fingerprints: true)` runs only on x86.  AArch64 / MIPS lifter paths have arch-specific branches (NEON, delay-slot handling) that the existing tests don't cover.
- **Harness:** existing ELF fixtures; `analyze(Arch::Aarch64, "arithmetic", "add")` + same for MIPS32.
- **Expected:** `validate_with_options(..., check_asm_fingerprints: true)` returns `Ok(())` on both.
- **Effort:** S (~20 LOC).

## Already-covered

| ID | Description | Existing coverage |
|----|---|---|
| C-1 | Asm-fingerprint dedup-union (out-of-order addresses) | `ir/tests/asm_fingerprint_dedup_union.rs` (3 named tests) |
| C-2 | cfg bounded-lift fall-through → TailCall | `cfg/tests/build_end_to_end.rs::fall_through_past_fn_max_size_terminates_as_tail_call` |
| C-3 | cfg CondBranch with one OOB successor | `cfg/tests/build_end_to_end.rs:163` (both OOB-arm + mixed-OOB cases) |
| C-4 | Stack-array dispatch classifier (8 archs) | `strider/tests/indirect_resolve_classify.rs` |
| C-5 | `*_any([])` vacuous-failure | `pattern/tests/matching/control_flow.rs:48,76` + `stack.rs:221` |
| C-6 | Multi-output `Match::output(c)` Call vs Return | `pattern/tests/matching/control_flow.rs:149,212` |
| C-7 | CallOther ABI ≥20 entries | `target/src/call_other_abi.rs` inline tests (30+ entries) |
| C-8 | PyO3: every typed exception triggered e2e | `strider-py/tests/python/test_typed_errors_e2e.py` |
| C-9 | Bit-exact lift-time canonicalisations | `pcode-lift/tests/value_lifter.rs:555,566,783,806,831` |
| C-10 | `decompose_sp` deep Add-chain overflow | `opt/src/sp_expr.rs:871` (5000-node) |
| C-11 | Per-arch Sleigh + regs smoke (15 presets) | `target/tests/arch_smoke.rs` |

## Total

7 new tests proposed.  All are FAILING-pre-fix or regression-prevention scaffolds.
