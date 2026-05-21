# Round 12 — Test Plan

**Date:** 2026-05-11 · **Branch:** `review/ai6`

19 missing tests derived from round-12 audit findings (1A–1F, 2A–2D, 3A–3B) and direct code inspection. All tests are FAILING pre-fix or pin contracts with no existing coverage. Grouped by crate area.

## IR

### T-1: Asm-fingerprint shrink-prevention with out-of-order addresses
- **Scope:** unit
- **Where:** add to `crates/ir/tests/asm_fingerprint_dedup_union.rs`
- **Why:** `crates/ir/src/graph/store.rs:182-198` — `extend_asm_fingerprint` has a `needs_resort` path for out-of-order contributors. All three existing tests feed addresses in strictly ascending order, leaving the sort+dedup branch uncovered. Round-12 prompt explicitly requires this test.
- **Harness:** `FunctionBuilder::empty()`, create one `IntConst` node, `set_asm_fingerprint(node, vec![0x2000u64])`, then `extend_asm_fingerprint(node, &[0x1000u64, 0x3000u64])` (descending first element), then `extend_asm_fingerprint(node, &[])`.
- **Expected:** `asm_fingerprint(node) == [0x1000, 0x2000, 0x3000]`. Empty-extend leaves fingerprint unchanged.
- **Effort:** S

### T-2: `validate_with_options` Layer-C catches non-exempt node with empty fingerprint
- **Scope:** unit
- **Where:** add to `crates/ir/tests/asm_fingerprint_dedup_union.rs`
- **Why:** `crates/ir/src/validate/layer_c.rs:199-220` — opt-in Layer-C check. Only e2e coverage uses a real binary (`crates/strider/tests/asm_fingerprints.rs`). A mock-graph unit test pins the `ValidationError` variant.
- **Harness:** `FunctionBuilder::empty()`, `build_int_const(42u64, U32)` without calling `set_lift_addr`, `build_return(Some(c), &[])`, `build()`. Call `validate_with_options(&g.graph, g.entry, ValidateOptions { check_asm_fingerprints: true })`.
- **Expected:** Returns `Err(_)` with non-empty error list. Plain `validate(&g.graph, g.entry)` returns `Ok(())` on the same graph (confirms opt-in only).
- **Effort:** S

### T-3: `compact()` on a no-memory-consumer graph does not lose `InitialMemory`
- **Scope:** unit
- **Where:** add to `crates/ir/tests/retain_reachable.rs`
- **Why:** Round-12 1A MED — `crates/ir/src/graph/compact.rs:67-231`: `retain_reachable` walks control-out+data-in. `InitialMemory` is only reachable through a consumer. A no-consumer graph loses `InitialMemory` after compact, then `validate` fires `MissingInitialMemoryNode` (`layer_c.rs:23-52`). Pre-fix the test fails.
- **Harness:** `FunctionBuilder::empty()` → region → `set_entry_region` → `build_return(None, &[])` → `build()` → `compact()` → `validate(&bfg.graph, bfg.entry)`.
- **Expected:** `validate` returns `Ok(())`. Pre-fix: returns `Err` with `MissingInitialMemoryNode`.
- **Effort:** S

## pcode-lift / cfg

### T-4: `vn_io` sub-register partial write where container is phi-live
- **Scope:** integration
- **Where:** new test in `crates/pcode-lift/tests/vn_io_phi_live.rs` or extend `crates/strider/tests/read_reg_vn_truncate.rs`
- **Why:** Round-12 prompt requirement. `crates/pcode-lift/src/vn_io.rs:290-384` — `write_reg_vn` reads the current container value before merging the sub-register slice. When the container at a join point is `VarPhi`, a regression that bypasses the phi silently produces a stale value. No unit test exercises the phi-live path for sub-register writes.
- **Harness:** x86 bytes: block-0 writes `AH = 0xFF`; block-1 writes `AL = 0x42`; blocks converge on a use of `AX`. Lift via `strider_x86()`, `analyze_cfg`. Walk for the `VarPhi` on `EAX`/`AX` at the join.
- **Expected:** `VarPhi` for the container register exists at the join with two value inputs. Neither input is a bypass `IntConst(0)`. Merged value, if both preds are constant, is `0xFF42`.
- **Effort:** M

### T-5: Single-instruction CondBranch with one OOB successor produces empty-insns + Branch region
- **Scope:** unit (cfg white-box)
- **Where:** add to `crates/cfg/tests/build_end_to_end.rs`
- **Why:** `crates/cfg/src/cfg/builder/region_builder.rs:362-383` — when one successor is OOB, `self.insns.pop()` removes the trailing CondBranch insn. Comment at line 372: "even when this leaves the region empty (single-instruction case), `add_region` now accepts empty regions terminated with Branch." Existing test uses a 2-instruction region (pop leaves 1 insn). The degenerate single-insn case (pop leaves 0 insns) is uncovered.
- **Harness:** x86 bytes: `je backward-to-self` (rel8 = `0xFE`, 2 bytes) at `0x1000`, `fn_max_size=2`. Taken target = `0x1000` (in-range, backward). Fall-through = `0x1002` (OOB). Build the cfg.
- **Expected:** `cfg.graph[cfg.entry].insns.is_empty() == true`; terminator is `RegionTerminator::Branch`; the successor edge goes to the in-range backward target.
- **Effort:** M

### T-6: Lift-time canonicalisations for `IntNotEqual`, `FLOAT_NAN`, `PtrSub` are uncovered
- **Scope:** unit
- **Where:** add to `crates/pcode-lift/tests/value_lifter.rs`
- **Why:** `crates/pcode-lift/src/value/arithmetic.rs:74-95` (`handle_int_not_equal`) and `crates/pcode-lift/src/value/float.rs:78-90` (`handle_float_nan`) implement 2 of 8 lift-time canonicalisations with zero direct test coverage in `value_lifter.rs`. `PtrSub` (`cast.rs:245-266`) also uncovered.
- **Harness:** Synthetic pcode byte sequences fed to `ValueLifter`, same style as existing `lift_int_less_equal_lowers_to_boolneg_less`.
- **Expected:** `IntNotEqual` → `BoolUnaryOp::Neg(IntCmpOp::Equal(...))`. `FLOAT_NAN(x)` → `BoolUnaryOp::Neg(FloatCmpOp::Equal(x, x))`. `PtrSub(a,b)` → `IntBinaryOp::Add(a, IntUnaryOp::Neg(b))`.
- **Effort:** M

## opt

### T-7: Iterative `mem_chain_is_dirty` does not stack-overflow on a 1000-store chain
- **Scope:** scale / regression
- **Where:** add to `crates/opt/src/function_args/tests.rs` or `crates/strider/tests/stack.rs`
- **Why:** `crates/opt/src/function_args/mod.rs:480-560` — `mem_chain_is_dirty` walks the memory chain. W6 iterative rewrite is only tested with tiny chains. The existing 5000-node `decompose_sp` test (`crates/opt/src/sp_expr.rs:865-894`) covers the SP-expression path; the memory-chain path has no scale test.
- **Harness:** Build a graph with 1000 sequential `Store` nodes (each consuming prior `Store`'s memory output) with one `Call` in the chain. Run `FunctionArgDetect`.
- **Expected:** No panic / stack overflow. `mem_chain_is_dirty` returns `Ok(true)` for the chain.
- **Effort:** M

### T-8: `strider_for_arch` smoke test for all 9 wrapper variants
- **Scope:** unit (smoke)
- **Where:** new file `crates/strider/tests/test_utils_smoke.rs`
- **Why:** `crates/strider/src/test_utils.rs:36-95` defines 9 wrappers. `bug_on_lifts_cleanly.rs` uses some but not all. A broken CC preset / missing `.sla` would surface silently otherwise.
- **Harness:** Each wrapper in its own `#[test]` function; they panic on failure.
- **Expected:** All 9 (x86_64, x86, aarch64, arm, mips_o32, mips_o32_be, ppc32, ppc64le, ppc64be) construct without panic. PPC64BE (ELF v1) specifically absent today.
- **Effort:** S

## pattern

### T-9: `int_const_any_of([])` / `at_any([])` / `offset_any([])` vacuous-failure unit tests
- **Scope:** unit
- **Where:** add to `crates/pattern/tests/matching/wildcards_and_consts.rs`
- **Why:** Round-12 1D verified vacuous-fail by code reading only. No Rust unit test calls `find_all` on these empty-set patterns. Python tests exist but don't cover the Rust core.
- **Harness:** Graphs with `IntConst(42)`, a `Call`, a `StackStore`. Call `find_all` on each empty-set pattern.
- **Expected:** All three return empty `Vec`. No panic.
- **Effort:** S

### T-10: `find_all_requirements` with 3-pattern shared-capture disagreement
- **Scope:** unit
- **Where:** add to `crates/pattern/tests/matching/matcher_api.rs`
- **Why:** `crates/pattern/src/matcher/mod.rs:469-485` handles `n ≥ 3` patterns. All existing tests use 2-pattern joins only. A 3-pattern case where P1+P2 agree but P3 disagrees on a shared capture exercises the early-break code path.
- **Harness:** Graph with bases A, B: `store(A+8,0)`, `store(A+16,0)`, `store(B+8,99)`. P1=`store(add(var(s),int_const(8)),0)`. P2=`store(add(var(s),int_const(16)),0)`. P3=`store(add(var(s),int_const(8)),int_const(99))`.
- **Expected:** `find_all_requirements(&[&p1,&p2,&p3])` returns empty `Vec` (P3's `s=B` conflicts with P1+P2's `s=A` consensus).
- **Effort:** S

### T-11: `Match::output(c)` for a value-producing capture on `Call` returns `Some`
- **Scope:** unit
- **Where:** add to `crates/pattern/tests/matching/control_flow.rs`
- **Why:** `crates/pattern/src/matcher/match_result.rs:200-234` — control-flow `output` returning `None` is tested; the complementary case (value-producing capture on Call returning `Some` to the correct slot) is not.
- **Harness:** Mock graph via `Tb` with a `Call` node having a return-value output. Match `call().ret_output(0, any()).capture(c)`.
- **Expected:** `m.output(c)` is `Some(_)`; the `NodeOutputId` indexes into a value output slot (not Control or Memory).
- **Effort:** M

## strider

### T-12: Stack-array IR-level dispatch explicitly exercises IR classifier
- **Scope:** integration (ELF fixture)
- **Where:** extend `crates/strider/tests/indirect_branch.rs`
- **Why:** ARM/AArch64 LE tests pass but don't assert the IR-level `classify_stack_array` arm actually fired (vs. cfg-time short-circuiting). Add an assertion that `graph.unresolved_branches.is_empty() == false` after `analyze_cfg` and before optimizer.
- **Harness:** Extend `assert_no_unresolved_indirect_branch` to inspect post-`analyze_cfg` state.
- **Expected:** For ARM and AArch64, `unresolved_branches` non-empty post-`analyze_cfg`. After optimizer + stack-array classifier, all resolve.
- **Effort:** M

### T-13: Cross-arch CallOther round-trip — ARM `swi`, AArch64 SMC, x86_64 `syscall`
- **Scope:** integration
- **Where:** extend `crates/strider/tests/call_other_precise_abi.rs`
- **Why:** Existing tests cover only x86_64 `cpuid` and AArch64 `UnkSytemRegRead`. ARM `swi`, AArch64 `CallSecureMonitor`, x86_64 `syscall` — the canonical cross-arch CallOthers — have no IR-round-trip tests.
- **Harness:** ARM: `svc 0 + bx lr` via `strider_arm()`. x86_64: `0x0F 0x05 0xC3` (`syscall;ret`) via `strider_x86_64()`. AArch64: `smc #0 + ret` via `strider_aarch64()`. Pattern-match by name.
- **Expected:** ARM `swi` CallOther has ≥9 inputs (ctrl+mem+r7+r0..r6). AArch64 `CallSecureMonitor` ≥10. x86_64 `syscall` 5 outputs (ctrl+mem+3 clobbers RAX/RCX/R11). No `UnknownCallOtherError`.
- **Effort:** M

### T-14: Asm-fingerprint contract end-to-end on AArch64 and ARM binaries
- **Scope:** integration (ELF fixture)
- **Where:** extend `crates/strider/tests/asm_fingerprints.rs`
- **Why:** All 7 existing tests use `Arch::X86`. AArch64 / ARM have different lifting paths (NEON, Thumb interworking, LR propagation). Round-12 prompt requires cross-arch coverage.
- **Harness:** `analyze(Arch::Aarch64, "arithmetic", "add")` + same for ARM. Walk reachable non-exempt nodes. Run `validate_with_options(check_asm_fingerprints: true)`.
- **Expected:** Zero reachable non-exempt nodes with empty fingerprints on AArch64 and ARM. `validate_with_options` returns `Ok(())`.
- **Effort:** S

## target / reader

### T-15: CallOther ABI dispatch matrix — ≥ 20 entries in `callother_dispatch.rs`
- **Scope:** unit
- **Where:** extend `crates/target/tests/callother_dispatch.rs`
- **Why:** `crates/target/src/call_other_abi.rs` has ≥25 entries; tests cover only 5. Uncovered: x86_64 `syscall`, aarch64 `CallHyperVisor`/`CallSecureMonitor`, x86 `rdpkru_u32`/`rdtsc`/`rdtscp`/`rdmsr`/`wrmsr`/`readfsbase`/`readgsbase`/`writefsbase`/`writegsbase`, arch-independent `sfence`/`lfence`/`swapgs`, NoOp/NoReturn entries. Each uncovered entry is a silent-regression risk.
- **Harness:** `classify(preset, name)` — pure table lookup, no deps.
- **Expected:** ≥20 distinct assertions; e.g. `classify(X86_64, "syscall")` → `Call` with `implicit_reads[0]=="RAX"`; `classify(Aarch64, "CallHyperVisor")` → `Call(implicit_reads=["x0",…,"x7"])`; `classify(X86_64, "rdtscp")` → `Call` with `implicit_writes` containing `"ECX"`; `classify(X86, "wrmsr")` → `Call(memory_edge=true)`; `classify(X86_64, "sfence")` → `Call(memory_edge=true)`.
- **Effort:** S

## strider-py

### T-16: Python `RewriteError` triggered via a second distinct pathway + strip stale prefixes
- **Scope:** integration (Python e2e)
- **Where:** extend `crates/strider-py/tests/python/test_typed_errors_e2e.py`
- **Why:** `RewriteError` is currently triggered only via a Call LHS root. A second pathway pins that the error class is not hardwired to one path. The three existing tests at lines 164, 196, 231 carry stale `Round 9 wave 25 (I-10):` prefixes (round-12 3B F-24); bundle strip with this addition.
- **Harness:** Craft a graph with `VarPhi` as a rewrite root. Attempt `graph.rewrite(phi_pat, rhs_pat)`. Strip stale prefixes.
- **Expected:** A second `RewriteError` pathway raises `errors.RewriteError`. All 6 typed exception subclasses are each raised by ≥1 test in the file.
- **Effort:** M

### T-17: Python end-to-end ARM `swi` round-trip via `strider.run`
- **Scope:** integration (Python e2e)
- **Where:** new file `crates/strider-py/tests/python/test_arm_callother_roundtrip.py`
- **Why:** Round-12 1F: Python binding feature-complete but no Python test calls `strider.run` with ARM bytes asserting CallOther node shape. ARM `swi` is the canonical cross-arch divergence point.
- **Harness:** `strider.MemoryMap.add_region(0x1000, bytes)` with ARM LE `svc 0` + `bx lr`. `strider.run(arch=SleighArch.arm(), cc=CallingConvention.arm_aapcs(), …)`. `graph.find_all(pattern.call_other())`.
- **Expected:** `strider.run` completes; `find_all(call_other())` returns ≥1 match. `match.asm_fingerprint(c)` returns non-empty list. No `UnknownCallOtherError`.
- **Effort:** M

## Cross-cutting

### T-18: proptest — asm-fingerprint superset-only invariant under arbitrary address sequences
- **Scope:** property
- **Where:** add to `crates/ir/tests/proptest_graph_invariants.rs`
- **Why:** The superset-only fingerprint contract is strong enough for fuzz-style testing. The out-of-order `needs_resort` path and dedup path interact in ways point tests cannot exhaustively cover.
- **Harness:** `proptest!` with generated `Vec<u64>` initial + `Vec<u64>` contributors. Call `set_asm_fingerprint`, then `extend_asm_fingerprint`. Check invariants.
- **Expected:** `old_fp.iter().all(|a| new_fp.contains(a))`. `new_fp.windows(2).all(|w| w[0] <= w[1])` (sorted). Dedup. `new_fp.len() >= old_fp.len()`.
- **Effort:** S

### T-19: Bit-exact lift-time canonicalisation — `IntNotEqual` against a real x86 binary
- **Scope:** integration (ELF fixture)
- **Where:** new test in `crates/strider/tests/lift_canonicalize_regression.rs`
- **Why:** Round-12 prompt requires "one regression per canonicalisation pinning IR shape against a real lifted insn." `IntNotEqual → BoolNeg(IntEqual)` has no e2e test. A fixture function `int ne(int a, int b) { return a != b; }` lifts a `sete`/`setne` sequence — the IR must contain `BoolUnaryOp::Neg(IntCmpOp::Equal(...))`.
- **Harness:** `per_arch_test!("arithmetic", "ne", graph_has_boolneg_int_eq)` — adds `int ne(int a, int b) { return a != b; }` to `fixtures/cases/arithmetic.c`. Walk `g.preorder()`.
- **Expected:** ≥1 `BoolUnaryOp::Neg` node whose input is `IntCmpOp::Equal`. No "NotEqual"-labelled node kind anywhere reachable.
- **Effort:** M (requires fixture function)

---

**Total:** 19 tests · IR (3) · pcode-lift/cfg (3) · opt (2) · pattern (3) · strider (3) · target/reader (1) · strider-py (2) · cross-cutting (2)

All 19 are FAILING pre-fix or pin contracts with no existing test coverage.
