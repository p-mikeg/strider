# Round 7 — CLAUDE.md Verification (code as ground truth)

22 claims sampled across CLAUDE.md sections. Each verdict was derived from reading the implementing code (no comment trust).

## Summary

| Verdict | Count |
|---------|-------|
| CONFIRMED | 16 |
| FALSE / STALE | 6 |
| AMBIGUOUS | 0 |

## CONFIRMED claims (no action)

| # | Claim | Code location |
|---|-------|---------------|
| 1 | `anyhow::Result` workspace-wide | `crates/ir/src/error.rs:8` |
| 2 | `CallOtherClass {NoOp, NoReturn, Call(CallOtherAbi)}` shape | `target/src/call_other_abi.rs:35-49` (test exhaustive at :756) |
| 3 | `Graph::asm_fingerprint{,_set,_extend,_extend_from}` API | `ir/src/graph/store.rs:108,120,130,160` |
| 4 | `FunctionBuilder::lift_addr` field + threading | `ir/src/builder/mod.rs:137,376,393`; `strider/src/strider/pipeline.rs:380-402` |
| 6 | `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` | `ir/src/validate/mod.rs:36,75-114` |
| 7 | `StackStorePhi` fixed arity 3 | `ir/src/validate/layer_c.rs:141-143` |
| 8 | `Match: Clone` | `pattern/src/matcher/match_result.rs:19` |
| 9 | Lift-time `IntSub`/`IntLessEqual` lowerings | `pattern/src/pat/ctor/int.rs:56-59,107-109` |
| 10 | `find_all_requirements` shared-capture filter via `prefix_agrees` | `pattern/src/matcher/mod.rs:435-472` |
| 11 | `IfCondInversion` swap logic | `opt/src/if_cond_inversion/mod.rs:58-76` |
| 12 | `strider::run(config) -> Result<BuiltFunctionGraph>` | `strider/src/orchestrator.rs:174` |
| 15 | `default_pipeline()` excludes `LoadReadOnly` | `opt/src/lib.rs:185-194` |
| 17 | Asm-fingerprint exempt set in CLAUDE.md matches `layer_c.rs` | `ir/src/validate/layer_c.rs:165-176` |
| 18 | `MemoryMap.apply_elf_relocations` uses autoload | `strider-py/src/reader.rs:264-270` |
| 19 | `Match::stack_offset / stack_phi_offsets` + Some(&[])→None collapse | `pattern/src/matcher/match_result.rs:244-279` |
| 20 | `pattern::{sub, int_le, int_sle, float_sub, float_ne, float_le}` aliases | `pattern/src/pat/ctor/{int.rs:56,107,116; float.rs:39,88,100}` |

---

## FALSE / STALE claims — required corrections

### #5 — `NodeKind` has `FloatIsNan`, `Piece`, `Extract { lsb, len }`, `Insert { lsb, len }` — FALSE
- **Where:** CLAUDE.md "IR Node Model" section (Integer / Float groupings)
- **Code:** `ir/src/node/kind.rs` — none of these variants exist. The IR has `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`, `Truncate`, `Extend`, `Popcount`, `Lzcount`. No `FloatIsNan`/`Piece`/`Extract`/`Insert`.
- **Implication for Round 1D pattern audit's finding #2 (`PhiPat` only matches `VarPhi`):** `Piece`/`Extract`/`Insert` are not actually IR nodes — the lifter handles these opcodes via composition (e.g. shift+mask). The pattern crate correctly does not expose pattern ctors for them.
- **Fix:** Remove these variants from CLAUDE.md.

### Bonus E — `IfCase(bool)` listed as `NodeKind` — FALSE
- **Where:** CLAUDE.md "Conditional branch:" line.
- **Code:** No `IfCase` in `NodeKind`. `If` has two control outputs (slot 0 = true, slot 1 = false); there is no separate IfCase node.
- **Fix:** Remove `IfCase(bool)` from CLAUDE.md.

### Bonus B — `IntConst(u64)` payload — FALSE (should be `u128`)
- **Where:** CLAUDE.md "Integer:" section
- **Code:** `ir/src/node/kind.rs:132` — `IntConst(u128)`
- **Fix:** Update CLAUDE.md to `IntConst(u128)`.

### #13 — SleighArch presets list — FALSE (incomplete)
- **Where:** CLAUDE.md `target` section
- **Code:** `target/src/arch.rs:23-39` — `ArchPreset` includes Ppc32Be, Ppc32Le, Ppc64Be, Ppc64Le. Test at `:748-751` confirms `SleighArch::ppc32be()`, `ppc32le()`, `ppc64be()`, `ppc64le()`.
- **Fix:** Append the four PowerPC presets to the CLAUDE.md preset list.

### #14 — CallingConvention presets list — FALSE (severely incomplete; 16 missing)
- **Where:** CLAUDE.md `target` section
- **Code:** `target/src/calling_convention/mod.rs` — additionally has: `x86_64_all_preserving` (:185), `powerpc_sysv32` (:357), `powerpc64_elf_v1` (:393), `powerpc64_elf_v2` (:426), `x86_linux_kernel` (:546), `x86_64_linux_kernel` (:560), `aarch64_linux_kernel` (:568), `arm_linux_kernel` (:575), `mips_linux_kernel_o32` (:582), `mips_linux_kernel_n64` (:589), `x86_linux_syscall` (:604), `x86_64_linux_syscall` (:622), `aarch64_linux_syscall` (:638), `arm_linux_syscall` (:659), `mips_linux_syscall_o32` (:675), `mips_linux_syscall_n64` (:690).
- **Fix:** Append the 16 additional presets to the CLAUDE.md CC preset list.

### #16 — `stable_default_pipeline()` description — FALSE (omits `FlagCmpCanonicalize`)
- **Where:** CLAUDE.md "Three pre-built top-level pipelines" line
- **Code:** `opt/src/lib.rs:106-126` — `stable_default_pipeline()` adds 4 passes: `ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`. CLAUDE.md says only `ConstantFold + KnownBits + IfCondInversion`.
- **Fix:** Update CLAUDE.md to include `FlagCmpCanonicalize` in the stable pipeline.

---

## Notes

- Claim 16 also needs a clarification regarding `IndirectBranchResolve`: it IS an `Optimizer` impl (`opt/src/indirect_branch_resolve/mod.rs:223`) but it is NOT in any of the 3 named pipelines (`default_pipeline`, `stable_default_pipeline`, `destructive_default_pipeline`). The orchestrator instantiates it directly. CLAUDE.md should be explicit: "instantiated by `strider::orchestrator`, not part of `default_pipeline()`".
- Claim 17 corroborates Round 1A finding (`graph/mod.rs:96` doc lists ghost `IfCase` as exempt — but `layer_c.rs::asm_fingerprint_exempt` is correct). Two consistent sources of evidence.

## Top corrections

1. **Remove ghost NodeKind variants** (`FloatIsNan`, `Piece`, `Extract`, `Insert`, `IfCase`).
2. **Fix `IntConst(u64) → IntConst(u128)`**.
3. **Add 4 PowerPC SleighArch presets**.
4. **Add 16 CallingConvention presets** (PowerPC + Linux kernel + Linux syscall families).
5. **Add `FlagCmpCanonicalize` to `stable_default_pipeline()` description**.
6. **Clarify `IndirectBranchResolve` is not in default pipelines**.
