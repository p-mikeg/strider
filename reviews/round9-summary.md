# Round 9 — Executive Summary

**Branch:** `review/ai3` (forked from `feature/ai`).
**Date:** 2026-05-09.
**Scope:** Independent re-derivation of correctness/simplification audit; round-7/round-8 reports not consulted.

20 parallel agents + 3 sequential agents produced 24 reports under `reviews/round9-*.md`. This document is the consolidated backlog.

---

## Headline counts

| Severity | Count | Themes |
|----------|-------|--------|
| **HIGH** (correctness bugs / unjustified silent failures) | **9** | sysret, fingerprint contract violations × 2, silent-drop × 5, dangling Python pointer |
| **IMPORTANT** (latent traps, doc/API drift, spec gaps) | **30+** | partial-state types, factually-wrong comments, Builder::with_endianness misuse, AArch64 test coverage, missing typed-exception tests |
| **MED** | **15+** | poison-recovery patterns, deprecated alias, stats counter mislabel, opaque milestone labels |
| **LOW** | many | doc precision, naming inconsistencies, breadcrumb tombstones |

Critical-severity production panics: **0**. All 8 panic sites in production code are justified, annotated with `#[allow]` or `debug_assert!`, and have inline-comment invariants (R9-2A).

---

## HIGH-severity backlog (9 items — prioritised for next-round implementation)

| # | Finding | Source | Where | Effort |
|---|---------|--------|-------|--------|
| 1 | `sysret` classified `NoReturn` — kernel exit truncated from CFG | EA3 C-1 | `target/call_other_abi.rs:259` | M |
| 2 | `ConstantFold::rule_and_dist` and other multi-node rules emit fresh nodes with empty asm-fingerprints; fails `validate_with_options(check_asm_fingerprints: true)` | EA1 F1 | `opt/constant_fold/rules.rs:62-71` + `pattern/rewrite.rs:91-92` | M |
| 3 | `FunctionArgDetect` exact-width path drops Load fingerprint on direct-width replacement | Ask-8 R2 F1 | `opt/function_args/mod.rs:329-336` | M |
| 4 | `read_or_init_var` silently drops unsupported-size args/ret regs (under-models Call footprint) | 2C #1 | `strider/orchestrator.rs:786` | L |
| 5 | `build_anchor_calling_context` clobber loop silently drops unsupported-size clobbers (false-positive forward substitution) | 2C #2 | `strider/orchestrator.rs:729-731` | L |
| 6 | `classify_anchor_with_rom_and_sp` eprintln+None on KB contradiction (real bug masked as UnresolvedIndirectBranch) | 2C #3 | `strider/indirect_resolve/classify.rs:49-57` | M |
| 7 | `wrap_when` leaves dangling raw `*const ir::Graph` when `try_borrow` fails | Ask-8 R3 / 2C #4 | `strider-py/pattern.rs:462-464` | M |
| 8 | `wrap_when` swallows `KeyboardInterrupt`/`SystemExit` (Ctrl-C cannot interrupt slow `find_all`) | 2C #5 / 1F-04 | `strider-py/pattern.rs:475-484` | S |
| 9 | `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` partial-state ctor — external mutation of `call_clobbered`/`ret_val_regs` silently breaks `Match::get_vn`. Round 8's `RewriteCtx` is the right shape; migrate and delete | 2D H1 / H4 | `ir/function.rs:117 + 59-79` | M |

All 9 have regression tests specified in `round9-test-plan.md`.

---

## IMPORTANT backlog by theme

### Correctness — IR/lifter

- **Indirect-branch classifier dead code & sign-extension bug** — Dead `Truncate(_)` and `Extend(_)` arms in `classify.rs:233-269`; `Extend(SignExtend, IntConst)` arm uses `(*k) as u64` instead of `get_signed_int` (1B F1/F2, 1C Issue 1).
- **AArch64-BE / MIPS-PIC GOT / PPC indirect dispatch gaps** — 7 `#[ignore]`-d arch tests with documented but not-yet-implemented classifier shapes (EA3 IMP-1, IMP-2, IMP-3).
- **PPC32/64 `ret_val_regs_float: ["f1", "f2"]` over-approximates** — ABI returns scalar floats in `f1` only; `f2` only for 128-bit `long double` (EA3 IMP-7).
- **`handle_int_sub` neg width mismatch** — theoretical, in-spec input never triggers but defensive fix recommended (EA3 IMP-4).
- **ARM-BE VFP register aliasing drops float chain** — documented gap; fix path: BE-aware containment in `find_largest_fitting_register` (EA3 IMP-5).
- **Stall budget decrements on count-stable iterations** — pathological anchor-replacement cycles can exhaust budget prematurely (Ask-8 R2 F7).
- **`check_layer_c_control_state` zombie gap** — non-empty-input path skips reachability gate; could produce false-positive errors on detached zombies (Ask-8 R2 F2).

### Type design / encapsulation (R9-2D)

- **H2** — `cfg::PcodeInsnAddr` / `MachineInsnAddr` `pub` fields with documented "do not reorder" `Ord` invariant; ~30 deep-field-access sites.
- **H3** — `BuiltCallingConventionParts::from_parts` performs no validation (typo overlapping arg/callee-saved sets silently miscompiles).
- **H5** — `IndirectBranchResolve::{unresolved_anchors, anchor_contexts}` lockstep enforced only by runtime check, not by type.
- **9 visibility-tightening candidates** ranging from `LiftAddrGuard` zero-callers to `target::SleighArch` field exposure.

### Doc/CLAUDE.md drift (R9-3A)

- ir/README.md `NodeOutputType` description omits U80, U512, F80 (conf 100).
- CLAUDE.md line 77 lists `x86_64_systemv_abi` as canonical; the canonical name is `x86_64_systemv` (conf 100).
- pcode-lift/README.md `vn_mask` width list omits 32 and 64 (conf 100).
- 2 SKILL.md files cite stale line numbers (`store.rs:108-160`, `:160`) — actual is `:127-200` and `:184` (conf 90).

### Stale comments / factually-wrong docs (R9-2B, R9-3B)

- 3 critical comment errors (conf 92-98): `apply_tail_call` "returns Unimplemented" (false), `Strider::analyze_cfg_with_unresolved` (non-existent), `indirect_branch.rs` "stack-array not yet implemented" (false).
- ~25 stale `tier-1`/`tier-2` doc-prose sites (round-7 cleaned up identifiers; doc-prose unchanged).
- ~22 opaque `(R2)`/`(R3)`/`F7`/`G7`/`(W7)` plan-round breadcrumbs.
- 3 user-facing API doc errors: `UnknownCallOtherError` claims non-existent `FunctionBuilder::build_call_other`; `FunctionBuilder::build` doc names non-existent `ValidationFailed`; root README quickstart uses deprecated `x86_64_systemv_abi()`.

### Test coverage gaps (R9-test-plan)

- 8 HIGH-priority regression tests (one per HIGH item above).
- 11 IMPORTANT additions including AArch64 callee-saved parallel test, `validate_with_options` Layer-C unit tests, `find_all_requirements` Rust unit, 3 unskipped typed-exception tests.

### Cross-arch issues (R9-correctness-cross-arch, R9-1B F3)

- **C-1 (conf 95)** — `crates/strider/tests/indirect_branch.rs:91` uses `Builder::with_endianness` which hardcodes `preset = X86_64`; non-x86_64 arches misclassify any CallOther.
- 6 sites in `cfg/tests/known_targets.rs` and 1 in `cfg/tests/indirect_dispatch.rs` use the same wrong constructor (currently harmless because tests use x86_64 byte sequences, but a trap).

---

## Simplification opportunities (R9-simplifications)

50+ entries in 7 categories. Headlines:

| Category | Items | Notable |
|----------|-------|---------|
| Delete | 20 | LiftAddrGuard re-export, partial-state ctor, dead classifier arms, deprecated alias, ~22 plan-breadcrumbs, 3 factually-wrong docs |
| Merge | 4 | tier-1/tier-2 prose unification (~25 sites); GOT-PLT vs generic Section-error stats classification |
| Inline | 3 | `read_or_init_var` (needs Result conversion first); explicit DO-NOT-INLINE for pattern aliases |
| Stdlib | 3 | poison-recovery helper consolidation across 4 sites |
| Visibility | 9 | BuiltFunctionGraph CC fields, PcodeInsnAddr fields, BuiltCallingConventionParts, IndirectBranchResolve, SleighArch, FunctionGraph; add Send+Sync to Optimizer traits |
| Drop wrappers | 4 | partial-state ctor (covered by Delete D2); tighten MachineInsnAddr field access |
| Partial-state | 8 | RunConfig fn_max_size/allow_code_before coupling → `enum FunctionBoundary`; AnalyzeOptions sort invariant → `SortedVns` newtype; ResolvedTargets::Multiple non-empty invariant |

**Net deletion estimate:** ~150 LOC.

**Suggested ordering:**
1. **Batch 1** (low risk, high yield): all stale-prose deletes + V1 + M1 + the 3 critical comment errors
2. **Batch 2** (mechanical migrations): visibility tightening V2-V9
3. **Batch 3** (type surface): partial-state migration + builder pattern for CC parts

---

## Skill bundle (R9-skill-audit)

**Existing 14 skills:** all KEEP. 3 need UPDATE:
- `strider-fingerprint-audit` — fix stale line range
- `strider-opt-pass-author` — fix stale `extend_asm_fingerprint_from` line + add multi-node-rewrite_rule trap warning
- `strider-pattern-author` — fix stale `crates/pattern/src/{call,capture,matcher}.rs` paths

**5 new skills proposed:**
1. `strider-rewrite-rule-multinode-audit` — covers EA1 F1
2. `strider-builder-for-arch-migration` — covers cross-arch C-1, M-3, M-4
3. `strider-silent-failure-audit` — covers 2C #1-5
4. `strider-public-api-encapsulation` — covers 2D H1-H5
5. `strider-doc-line-number-refresh` — mechanical citation maintenance

---

## Round 9 acceptance criteria — status

| Ask | Status |
|-----|--------|
| 1. Correctness across all edge cases | ✓ — 9 HIGH + 30+ IMPORTANT findings catalogued |
| 2. Simplicity / consolidation | ✓ — R9-simplifications.md (50+ entries, 7 categories) |
| 3. tier-1/tier-2 rename | ✓ — concrete mapping (M1) |
| 4. Unused features for deletion | ✓ — D1 (LiftAddrGuard), D5 (deprecated alias), D7 (breadcrumbs), D11-D14 (stale headers) |
| 5. Missing Python parity | ✓ — I-5 (Graph.optimize drain), I-10 (3 unskipped exceptions), H-4/H-5 (when-predicate hardening) |
| 6. Multiple rounds | ✓ — Wave 1 (20 parallel) + Wave 2 (3 sequential) + Round 7 (this) |
| 7. Test plan | ✓ — R9-test-plan.md (23 items, S/M/L effort) |
| 8. Stale comments | ✓ — R9-3B (~30 findings) + R9-2B (3 critical) |
| 9. No production panics | ✓ — R9-2A confirms 0 unjustified |
| 10. CLAUDE.md/README correctness | ✓ — R9-claudemd-diff.md + R9-readme-diffs.md |
| 11. Skill design | ✓ — R9-skill-audit.md (14 audited, 5 proposed) |

**Verification at HEAD:**
- `cargo build --workspace`: ✓ green
- 20 review reports + coverage manifest committed at `67c5db2` and pushed to `review/ai3`
- `cargo test --workspace` and `pytest`: not re-run this round (no source changes — all reports are documentation)

---

## Recommended next-round implementation phases

**Phase A — Low-risk doc/cleanup batch (1-2 days):**
- Fix 3 critical comment errors (R9-2B C-1/C-2/C-3)
- Apply 2 doc fixes (R9-3A: ir/README, pcode-lift/README, CLAUDE.md `x86_64_systemv`)
- Fix `Builder::with_endianness` → `for_arch` in 8 test sites (D15, M-3, M-4)
- Update 2 SKILL.md stale line numbers (R9-3A Issues D, E)
- Apply tier-1/tier-2 prose unification (M1)
- Drop opaque milestone breadcrumbs (D7)

**Phase B — HIGH-severity correctness fixes with regression tests (1 week):**
- All 9 HIGH items + their specified regression tests
- Each fix as its own commit with the regression test in the same commit

**Phase C — Simplification + visibility (1 week):**
- Visibility tightening batch (V1-V9)
- Partial-state migrations (P2 FunctionBoundary, P5 ResolvedTargets::multiple ctor)
- Delete LiftAddrGuard re-export and dead classifier arms

**Phase D — Skill bundle (1 day):**
- Update 3 existing SKILL.md
- Implement 5 new skills

---

## Files in this round

- `round9-coverage-manifest.md` — 405 in-scope file enumeration
- `round9-1A-ir.md` through `round9-1F-strider-py-aux.md` — 6 per-crate
- `round9-2A-panics.md`, `round9-2B-naming.md`, `round9-2C-silent-failures.md`, `round9-2D-types.md` — 4 cross-cutting
- `round9-3A-doc-verify.md`, `round9-3B-comments.md` — 2 verification
- `round9-EA1-self-vs-self.md`, `round9-EA2-ir-vs-pcode.md`, `round9-EA3-ir-vs-assembly.md` — 3 emphasis-A triangulation
- `round9-correctness-{types,invariants,borrowing,edge-cases,cross-arch}.md` — 5 Ask-8 rotation
- `round9-test-plan.md` — 23-item test plan (Round 4)
- `round9-simplifications.md` — 50+ simplification entries (Round 5)
- `round9-skill-audit.md` — 14 existing + 5 new skills (Round 6)
- `round9-summary.md` — this file (Round 7)
- `round9-claudemd-diff.md` — CLAUDE.md correctness diff
- `round9-readme-diffs.md` — per-crate README correctness diffs

**Total:** 23 review documents + this summary.
