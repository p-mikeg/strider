# Round 8 / 2B — Naming sweep

**Branch:** `review/ai2`.  Independent audit.

## Concrete rename / doc-fix mapping

| Old | New | Where | Rationale |
|-----|-----|-------|-----------|
| `"lifts to \`Return(target_value)\`"` (doc) | `"lifts to \`IndirectBranch(target_value)\`"` | `crates/strider/src/strider/pipeline.rs:498-500` | `PendingIndirect`'s variant doc says the handler emits `Return(target_value)`, but `handle_unresolved_indirect_branch` calls `self.builder.build_indirect_branch(target_value)` which produces `NodeKind::IndirectBranch`, not `Return`.  Confirmed by `insn/control.rs:321`.  Factual mismatch in a private-enum doc. |
| `build_optimizer_pipeline` docstring missing `StackLoadForward` | Insert "3. `opt::StackLoadForward` inside the fixed-point loop" between current items 2 and 3 | `crates/strider/src/strider/pipeline.rs:178-188` | The code adds `StackLoadForward` (lines 195-198) but the numbered doc list jumps from `StackStoreDetect` directly to `CallStackArgCollect`.  Readers cannot infer the load-forwarding pass is included. |
| `"match the legacy \`analyze_cfg(cfg)\` behaviour"` | `"match the \`analyze_cfg(cfg)\` default behaviour"` | `crates/strider/src/strider/pipeline.rs:86` | `analyze_cfg` is still the primary public API.  "Legacy" implies superseded/deprecated; misleads `AnalyzeOptions` callers. |
| `r1_placeholder.rs` (filename) | `indirect_branch_lift_placeholder.rs` | `crates/strider/tests/r1_placeholder.rs` | "r1" prefix is a stale internal milestone label ("Round 1" of indirect-branch fixedpoint spec).  File contents are stable production-code tests for `UnresolvedIndirectBranch → IndirectBranch` placeholder lift. |
| `x86_64_systemv_abi` | `x86_64_systemv` | `crates/target/src/calling_convention/mod.rs:278`, `crates/strider-py/src/cc.rs:17`, `crates/strider-py/strider/__init__.pyi:51`, all callers | Inconsistent suffix.  All other CC presets omit `_abi`: `x86_cdecl`, `arm_aapcs`, `aarch64_aapcs64`, `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`.  Public API symbol — needs deprecation alias. |
| `R1.3`, `R1.4`, `R2`, `R3`, `R4`, `R5`, `Tier 1`, `Tier 2` (in doc comments) | Descriptive prose | `crates/strider/tests/r1_placeholder.rs:32,48,105,107`; `crates/strider/tests/indirect_resolve_classify.rs:10,30,88,127,157`; `crates/strider/tests/indirect_resolve_jump_table.rs:1,17,18,41,95`; `crates/strider/tests/indirect_branch.rs:1,16,18,107,182,190`; `crates/cfg/tests/region_terminator.rs:278,281`; `crates/cfg/tests/known_targets.rs:3`; `crates/cfg/tests/indirect_dispatch.rs:182,311,334`; `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:655`; `crates/strider/tests/common/indirect_resolve_helpers/classify.rs:39,47,59,70,128,349,436,811,859` | Opaque milestone labels from the internal design spec.  The most critical: `r1_placeholder.rs:105` ("Tier 2 (R2) walks this table") and `indirect_resolve_classify.rs:10` ("the orchestrator (R3) will hand to the classifier"). |
| `lift_and_seat` (private method) | `lift_and_store` (or `absorb_results`) | `crates/strider/src/orchestrator.rs:342` | "Seat" is non-standard jargon.  Doc explains it ("seat the resulting graph and region index onto `self`") but `store_results` reads without commentary. |
| `analyze_cfg_with_unresolved` (stale ref in doc comments) | `analyze_cfg` / `analyze_cfg_with` | `crates/strider/tests/indirect_resolve_classify.rs:4`; `crates/strider/tests/indirect_resolve_jump_table.rs:16` | `analyze_cfg_with_unresolved` does not exist in the codebase.  Module-level doc comments reference it by name as if it were the entry point. |

## File / directory renames

| Current | Proposed | Priority |
|---------|----------|----------|
| `crates/strider/tests/r1_placeholder.rs` | `crates/strider/tests/indirect_branch_lift_placeholder.rs` | MED |

## Stale references to deleted symbols

- `crates/opt/src/lib.rs:149, 182` — historical tombstone notes mention deleted `CallOtherElide` pass in `pub` API docstrings.  Accurate as history but the format is unusual (a link to git log or the spec doc would be cleaner).  LOW.

## Confidence-filtered non-findings

The following spec items were verified clean:

- **`tier`, `_v1`/`_v2`, `old_`, `legacy_`, `tmp_`, `r1_`, `r2_`** in `*.rs` *identifiers*: zero matches.  Only doc-comment references and the `r1_placeholder.rs` filename.
- **`\bVar\b` (vs `Capture`)**: zero matches; `Var/NodeVar` split fully replaced by `Capture`.
- **`unresolved_`**: only `unresolved_branches` (on `IrStrider`/`AnalyzeOutcome`) and `LoopState::unresolved` — both accurate to what they hold.
- **`CallOtherElide`**: only the two doc-comment tombstones; not referenced in any `*.rs` identifier.
- **`BuiltCallingConventionParts`**: still `pub` and exported; doc explicitly states "Used by callers (typically tests)."  Intentional.
- **`Tier` / `tier_1` / `tier_2`** as Rust *identifiers*: zero matches.  Only prose doc comments.
- **`powerpc64_elf_v1` / `powerpc64_elf_v2`**: `_v1/_v2` is the ELF ABI version (ELFv1 vs ELFv2), not a revision counter.  Not stale.
- **`PURE` / `PURE_WITH_MEM_EDGE` / `NO_OP` / `NO_RETURN` constants** in `call_other_abi.rs`: private `const`s used only in match table.  Names clear in context.
- **`int_const_with!` macro**: name reads as "build IntConst with value from LHS captures."  `__strider_ctx` hygiene var is internal.
- **`UnkSytemRegRead`** (typo in Sleigh-produced user-op): literal string emitted by GHIDRA's Sleigh lifter — not our name to fix.

## Summary

- **HIGH (≥80 confidence)**: 3 — `PendingIndirect` doc error; `build_optimizer_pipeline` doc omitting `StackLoadForward`; "legacy" label on `analyze_cfg`.
- **MED (50-79)**: 4 — `r1_placeholder.rs` filename; `x86_64_systemv_abi` `_abi` suffix; stale `R1`/`R2`/`Tier` milestone labels; stale `analyze_cfg_with_unresolved` reference.
- **LOW (<50)**: 1 — `CallOtherElide` tombstones in `opt/src/lib.rs`.

The **highest-impact fix** is the `SpecialTerm::PendingIndirect` docstring — `pipeline.rs:498-500` will mislead anyone maintaining the `handle_unresolved_indirect_branch` dispatch path.
