# Round 9 / 1E — `strider` + `target` + `reader` audit

**Branch:** `review/ai3`. Independent audit; round-7 / round-8 reports not consulted.

## Findings

### MED — `RelocationTarget::Section` resolution failure mislabels malformed-ELF as `skipped_unresolved_target`

- **Confidence:** 88.
- **Severity:** MED.
- **Where:** `crates/reader/src/elf.rs:584`.
- **What's wrong:** The generic relocation path's `RelocationTarget::Section(idx)` arm increments `stats.skipped_unresolved_target` when `obj.section_by_index(idx)` returns `Err`. Per the `RelocationStats` doc at lines 380-387, `skipped_unresolved_target` is for legitimately unresolvable cases (undefined externs, weak symbols); a malformed section index belongs in `skipped_malformed_target`. The symbol-targeted paths at lines 523 (GOT/PLT) and 572 (generic) correctly use `skipped_malformed_target` for failed `symbol_by_index` calls — only the section path is inconsistent.
- **Fix:**
  ```rust
  RelocationTarget::Section(idx) => match obj.section_by_index(idx) {
      Ok(sec) => sec.address(),
      Err(_) => {
          stats.skipped_malformed_target += 1;  // was: skipped_unresolved_target
          continue;
      }
  },
  ```
  Plus update `skipped_malformed_target`'s doc to drop the parenthetical "or the relocation target is neither Symbol nor Section" — that case dispatches to `record_unsupported` (incrementing `skipped_unsupported_kind`), not the malformed bucket.

### LOW — Stale field doc in `LoopState::sleigh` references deleted `Builder::with_endianness`

- **Confidence:** 82.
- **Severity:** LOW (doc-only).
- **Where:** `crates/strider/src/orchestrator.rs:217-219`.
- **What's wrong:** Field doc says "consumed by `Builder::with_endianness` per iteration." Actual call at line 837 uses `Builder::for_arch`. The round-8 migration updated the call site but not the comment.
- **Fix:** Replace `Builder::with_endianness` with `Builder::for_arch` in the doc.

## Areas verified correct

- **`test_utils.rs` — `probe_regs()` correctness.** All 4 per-arch wrappers (`strider_x86_64`, `strider_x86`, `strider_aarch64`, `strider_arm`) use the right CC preset.
- **`Builder::for_arch` migration.** `orchestrator.rs:837`, `tests/common/mod.rs:220`, `benches/scaling.rs:93` all use `for_arch`. No `with_endianness` in any build path.
- **LR-as-callee-saved tradeoff.** Documented in CLAUDE.md line 79 with the indirect-branch-resolver rationale.
- **`locate_and_write` `checked_add` overflow guard.** In place at elf.rs:933-937.
- **`apply_elf_relocations_autoload`** partial-region correctness. Extender stages new regions only when site is uncovered (line 679 `already_covered` check).
- **Decision { FixedPoint, StableOnly, Rebuild }** semantics + stall budget reset on Rebuild (line 441).
- **`GraphRewriter::re_optimize`** accepts caller-supplied pipeline (no hardcoded destructive subset).
- **`mfence` / `sfence` / `lfence`** classified as `PURE_WITH_MEM_EDGE` at call_other_abi.rs:345-347; tested by `x86_memory_fences_classify_as_pure_with_mem_edge`.
- **`rdmsr` / `wrmsr` / `rd*fsbase` / `wr*fsbase`** classifications correct.

## ABI correctness spot-checks (Emphasis A)

| Preset | Arg regs | Callee-saved | Return | ret_stack_pop | Verdict |
|--------|----------|--------------|--------|---------------|---------|
| `x86_64_systemv` | RDI/RSI/RDX/RCX/R8/R9 | RBX/RBP/R12-R15 | RAX/RDX (XMM0/1) | 8 | ✓ AMD64 SysV §3.2 |
| `aarch64_aapcs64` | x0-x7 | x19-x28/x29/x30 (LR tradeoff) | x0/x1 + q0/q1 | 0 | ✓ AAPCS64 §6.4.2 |
| `arm_aapcs` | r0-r3 | r4-r11 + lr (LR tradeoff) | r0/r1 + d0/d1 | 0 | ✓ AAPCS §6.5 |
| `powerpc64_elf_v2` | r3-r10 | r2/r14-r31 + LR (LR tradeoff) | r3/r4 | 0 | ✓ ELFv2 §2.2 |

## Simplification candidates (Emphasis B)

- `test_utils.rs` is missing MIPS / PowerPC wrappers — minor coverage gap (only x86_64, x86, aarch64, arm provided; `tests/common::strider_for(arch)` covers all 16).
- `tests/common/mod.rs::analyze` (line 176-182) duplicates the `probe_regs` + `Strider::new` pattern that `strider_for(arch)` (line 140) already encapsulates. Minor consolidation opportunity.
- `CallingConvention::x86_64_systemv_abi` deprecated alias at `mod.rs:305` — no remaining callers; can be removed once strider-py downstream tests migrate.

## Coverage

Files fully read: `strider/src/{lib,test_utils,orchestrator,rewrite}.rs`, `strider/src/strider/{mod,pipeline}.rs`, `strider/tests/common/mod.rs`, `strider/benches/scaling.rs`, `target/src/{arch,call_other_abi,calling_convention/mod,calling_convention/tests}.rs`, `reader/src/{lib,elf}.rs`.

Files NOT covered: `strider/src/strider/{insn/control,insn/mod,vn_io}.rs`, `strider/src/indirect_resolve/{classify,inplace}.rs`, integration test files other than `common/mod.rs`. Peripheral to special-focus items.

## Summary

- **1 MED** — relocation `Section` mislabel (round-8 fixed analogous symbol-path mislabel; this finding extends to the section path that round 8 missed).
- **1 LOW** — stale field doc post round-8 migration.
- All round-9 special-focus items verified.
