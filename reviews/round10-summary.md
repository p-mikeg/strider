# Round 10 — Executive Summary

**Branch:** `review/ai4` (forked from `review/ai3`).
**Date:** 2026-05-09.
**Scope:** Independent re-derivation of correctness/simplification audit. Rounds 7, 8, and 9 outputs **not consulted** per the trust-only-the-code instruction.

16 review documents produced under `reviews/round10-*.md`. This is the consolidated backlog.

---

## Headline counts

| Severity | Count | Themes |
|----------|-------|--------|
| **HIGH** (correctness bugs / unjustified silent failures) | **17** | KB-cache invalidation, fingerprint-attribution gap, `GuardPat` zero-output, `*_any` capture-output gap, partial-state ctors, Python typed-exception coverage, ELF autoload diagnostic, hash truncation |
| **IMPORTANT** | **30+** | Visibility tightening, `StackStorePhi` chain forwarding, MIPS/PPC RELATIVE relocations, cross-arch CallOther dispatch, multi-line off-by-one stale comments |
| **MED** | **15+** | Stack-overflow safety on deep memory chains, Layer-C validator gaps, `is_addr_tail_call` 4-tuple primitive obsession |
| **LOW** | many | Doc precision, naming inconsistencies, breadcrumb tombstones |

**Critical-severity production panics:** **0**. All 7 panic sites in production code (3 in `ir`, 4 in `opt`) are justified by `#[allow(clippy::expect_used)]` and inline invariant comments. Verified by R10-2A.

---

## HIGH-severity backlog (17 items)

| # | Finding | Source | Where | Effort |
|---|---------|--------|-------|--------|
| 1 | `IndirectBranchResolve` per-iteration KB cache invalidated by in-place `apply_tail_call` edits | R10-1C C-1 | `crates/opt/src/indirect_branch_resolve/mod.rs:347` | L |
| 2 | `FunctionArgDetect` exact-width path drops InitialVar fingerprint into `FunctionArg` | R10-1C C-2 | `crates/opt/src/function_args/mod.rs:189-198` | M |
| 3 | `GuardPat` missing `try_match_node` override — `ret().capture(c).when(f)` silently never matches | R10-1D C-1 | `crates/pattern/src/pat/guards.rs:52-81` | S |
| 4 | `*_any` variant-agnostic captures bind `output: None`; downstream `get_uint`/`get_int` silently None | R10-1D C-2 | `crates/pattern/src/pat/ctor/variant_agnostic.rs:67-73` | S |
| 5 | `PyMemPhiPat`/`PyValuePhiPat` missing from `PatLike` — `find_all(mem_phi())` raises `TypeError` | R10-1F F-01 | `crates/strider-py/src/pattern.rs:237-255` | S |
| 6 | `PyCapture.__hash__` returns `u32 as isize` — 32-bit collision risk | R10-1F F-02 | `crates/strider-py/src/pattern.rs:99-104` | S |
| 7 | `LoopState::recompute_unresolved` returns empty Vec on missing graph instead of Err | R10-2C H10-S1 | `crates/strider/src/orchestrator.rs:607-609` | S |
| 8 | `KnownBits::ZeroExtend` `unwrap_or(0)` corrupts analysis on unsupported widths | R10-2C H10-S2 | `crates/opt/src/known_bits/mod.rs:279` | M |
| 9 | ELF autoload section parse failure logs to stderr only — invisible to caller | R10-2C H10-S3 | `crates/reader/src/elf.rs:778-784` | M |
| 10 | `PyMemReader.read` collapses Python exception, "not mapped", and wrong-type into one error | R10-2C H10-S4 | `crates/strider-py/src/reader.rs:496-513` | M |
| 11 | `PyReadOnlyMemoryAdapter.read` doesn't re-raise `KeyboardInterrupt`/`SystemExit` | R10-2C H10-S5 | `crates/strider-py/src/reader.rs:575-593` | M |
| 12 | `function_args::mem_chain_is_dirty` `unwrap_or(true)` in release masks invariant violations | R10-2C H10-S6 | `crates/opt/src/function_args/mod.rs:521-522` | M |
| 13 | `opt::Kb` `pub` ones/zeros fields with documented "must never overlap" — invariant unenforced by ctor | R10-2D | `crates/opt/src/known_bits/mod.rs:38-43` | S |
| 14 | `BuiltCallingConvention::from_parts` unvalidated `pub` form — typo in CC overlapping arg/callee-saved silently miscompiles | R10-2D | `crates/target/src/calling_convention/mod.rs` | S |
| 15 | `BuiltFunctionGraph` 5 `pub` fields with documented post-build-mutation hazard warnings — fields still pub | R10-2D | `crates/ir/src/function.rs:45-100` | M |
| 16 | `Region.contains_addr` returns false on empty-Branch regions — downstream duplicate-region risk | R10-1B I-1 | `crates/cfg/src/cfg/types.rs:221-236` | S |
| 17 | `handle_int_sub` neg width sized from `inputs[1].size` rejects mismatched-width Sleigh emissions | R10-1B I-3 | `crates/pcode-lift/src/value/arithmetic.rs:161-167` | M |

All 17 have regression tests scaffolded in `round10-test-plan.md`.

---

## IMPORTANT backlog by theme

### Correctness — IR / lifter
- **MIPS / PPC RELATIVE relocations missing** in `image_relative_reloc` (R10-1E I-5). Function-pointer tables on MIPS / PPC64 ET_DYN binaries silently read zero.
- **`make_int_const` no value masking** (R10-1A M-1) — breaks dedup-cache structural equality.
- **`compact::gc_wide_consts` standalone soundness claim** doesn't hold pre-compaction (R10-1A M-3).
- **`FunctionBuilder::build` doesn't propagate `no_memory_clobber`** to `BuiltFunctionGraph` (R10-1A M-4).
- **`StackLoadForward::probe`** missing `StackStorePhi` arm — inconsistent with `function_args::mem_chain_is_dirty` (R10-1C I-6).
- **`StackStorePhi` zombie key leakage** in `Graph::stack_phi_offsets` (R10-1C I-9).
- **`split_region`** doesn't guard `split_index == insns.len()` (R10-1B I-7).
- **`read_reg_vn` / `write_reg_vn`** lack defensive shift-bounds check (R10-1B I-6).
- **`resolve_const_loads`** chained-Load fixed-point (verify termination); single-pass design caveats (R10-1B I-4).

### Type design / encapsulation (R10-2D)
- 4 HIGH (Kb, BFG, BuiltCallingConvention from_parts, partial-state ctors).
- 6 MED (Cfg.start_addr_to_region_id pub field, Switch.target_value Option-state, IndirectBranchResolve lockstep fields, ResolvedTargets::Multiple non-empty invariant, NodeKind closed-but-not-`#[non_exhaustive]`, StackStorePhi side-table-dependent invariants).
- ~15 visibility tightening candidates (`FunctionGraph`, iterators, `GraphDotDumperState`, `IrStrider`, `ProcessInsnRes`, etc.).

### Doc / CLAUDE.md drift (R10-3A)
- 27 claims sampled: 18 confirmed, 3 refuted (all in `opt/README.md` / `strider-opt-pass-author/SKILL.md`), 6 partial (stale line numbers).
- HIGH refutation: `Optimizer` trait's method is `optimize` (not `run`); takes `(graph, entry)` (not just `graph`); `OptimizationResult` variant is `NoChange` (not `Unchanged`); `OptimizerOnBuilt::optimize_built` parameter is `&mut RewriteCtx<'_>` (not `&mut BuiltFunctionGraph`) post-wave-28.
- CLAUDE.md:85-86 conflates `opt::stable_default_pipeline` (4 passes) with strider's layered version (7 passes).

### Stale comments / factually-wrong docs (R10-3B)
- 14 findings (7 HIGH, 4 MED, 3 LOW).
- HIGH: `with_built` ghost references (4 sites) — round 9 wave 28 renamed to `with_rewrite_ctx`.
- HIGH: `pipeline.rs:137` self-contradictory migration note (`from RewriteCtx to RewriteCtx`).
- HIGH: `pattern::rewrite_rule` doc claims `&mut BuiltFunctionGraph` but signature returns `&mut RewriteCtx<'g>`.
- HIGH: `GraphRewriter::apply_rule` doc describes a `mem::take` BFG-swap that doesn't exist in the body.
- HIGH: `RegionTerminator::Switch` doc says it's "reserved for the future"; actually constructed at `region_builder.rs:508`.
- HIGH: 2 test/builder comments name non-existent symbols (`ValidationFailed` variant, `build_call_other` helper).

### Cross-cutting tombstones (R10-2B)
- ~120+ "Round 9 X-N" / "Ask-N RN FN" / "wave N" prefixes in doc-comments — clutter for future readers, no semantic value once round 9 is history.
- `TODO(Task17)` opaque tracker label (3 sites).
- `R5` / `R3` / `R1` plan-round breadcrumbs (~5 sites).

### Naming half-renames (R10-2B)
- `OptionsBuilder::lifter_options` field — residual from `LifterOptions` rename.
- `BuiltFunctionGraph::ret_val_regs_slice` — odd one out among `_regs` accessor trio.
- `with_built` references in 4 doc strings (alongside R10-3B).
- `bfg` (76 sites in tests) vs `fg` (~1600 sites in production).
- `*_for_test` `pub` methods that should be `#[cfg(test)] pub` (`Matcher::options_for_test`, `Match::new_for_test`, `resolve_indirect_target_for_test`).
- `intern_capture` (skill cite) vs actual `intern_str` (code).

### Test coverage gaps (R10-test-plan)
- 35 tests proposed, 17 HIGH-priority regression tests + 11 IMPORTANT additions + 7 MED.
- AArch64 / MIPS end-to-end ELF coverage gap (T-23/T-24).
- `find_all_requirements` shared-capture cross-product (T-21).
- 1024-deep memory chain stack-overflow safety (T-31).
- CallOther dispatch matrix per arch + per opcode (T-25).
- PyO3 typed-exception-by-subclass tests (T-26).

---

## Simplification opportunities (R10-simplifications)

60 entries with **net delta ~−510 LOC** (~0.9% of 57,100). Larger cognitive uplift from tombstone reduction.

| Category | Active items | LOC delta |
|----------|-------------|-----------|
| Delete | 9 | ~−185 |
| Merge | 6 | ~−95 |
| Inline | 4 | ~−22 |
| Stdlib idioms | 7 | ~−38 |
| Visibility | 14 | ±0 (API-surface tighten) |
| Drop wrappers | 1 | ~−60 |
| Partial-state | 4 | ~−122 |

**Suggested ordering:**
1. **Batch A (low-risk-high-yield)** — doc/test-only edits totalling ~−260 LOC: Round-9 tombstone strip, `with_built` ghost refs, dead diagnostic tests, R10-3A/3B doc refutations.
2. **Batch B (mechanical)** — `bfg`→`fg`, `_for_test` cfg-gating, `pub`→`pub(crate)` for inferentially-private types, helper-extraction for `replace_all_uses+fingerprint`, `?`-propagation for known_bits / orchestrator silent failures.
3. **Batch C (type surface)** — `BuiltFunctionGraph` field tightening, `Kb` invariant enforcement, `from_graph_and_entry_for_rewrite` deletion, `FunctionBoundary` / `SortedVns` resurrect-or-delete decisions.

---

## Skill audit (R10-skill-audit)

19 existing skills audited:

| Verdict | Count |
|---------|-------|
| KEEP-AS-IS | 7 |
| NEEDS-UPDATE | 12 |
| OBSOLETE | 0 |

**Significant decay:** `strider-cc-preset-extend` has 10 wrong line numbers (entire CC preset table drifted). `strider-doc-line-number-refresh` cites itself (meta-doc inconsistency). `strider-opt-pass-author` and `strider-orchestrator-extend` have stale line refs.

**2 new skill proposals:**
1. `strider-wide-const-author` — covers U256/U512 IntConstWide / WideConstId construction.
2. `strider-asm-fingerprint-design-sync` — keeps the per-pass fingerprint plan in lockstep with `layer_c::asm_fingerprint_exempt`.

---

## Round 10 acceptance criteria — status

| Ask | Status |
|-----|--------|
| 1. Correctness — code-vs-code self-consistency | ✓ — R10-1A through 1F, R10-2A/2C |
| 2. Correctness — IR-vs-pcode | ✓ — R10-1B (vn_io widths, lift-time canonicalisation), R10-1A |
| 3. Correctness — IR-vs-assembly | partial — cross-arch coverage gap noted (T-23/T-24); deferred ABI-vs-binary spot-check |
| 4. Simplicity / consolidation | ✓ — R10-simplifications.md (60 entries) |
| 5. Naming | ✓ — R10-2B with rename mapping |
| 6. Unused features | ✓ — R10-2D visibility tightening table + R10-simplifications S-1 through S-9 |
| 7. Python parity | ✓ — R10-1F F-01 (PyMemPhiPat/PyValuePhiPat gap) |
| 8. Multi-round correctness | ✓ — R1 (6 per-crate), R2 (4 cross-cutting), R3 (2 verification) |
| 9. Test plan | ✓ — R10-test-plan.md (35 tests) |
| 10. Stale comments | ✓ — R10-3B (14 findings) |
| 11. Production panics | ✓ — R10-2A (0 unjustified) |
| 12. CLAUDE.md / READMEs | ✓ — R10-3A (27 claims sampled, 3 refuted) |
| 13. Skills | ✓ — R10-skill-audit.md (19 audited, 2 new proposed) |

---

## What's-new-vs-round-9

**Genuinely new findings (post-round-9 implementation):**
- R10-1C C-1 (KB cache invalidation across in-place edits) — exposed by wave-28 RewriteCtx migration making the cache pattern explicit.
- R10-3B HIGH (multiple stale `with_built` references + self-contradictory `pipeline.rs:137`) — round 9 wave 28 left these doc-rot artifacts.
- R10-2C H10-S2 (`KnownBits::ZeroExtend` unwrap_or(0)) — sister `SignExtend` arm uses correct bail; ZeroExtend lagged.

**Pre-existing bugs round 9 missed:**
- R10-1A M-1 (`make_int_const` no masking) — dedup invariant pre-dates round 9.
- R10-1A M-3 (`gc_wide_consts` standalone claim) — pre-existing soundness gap.
- R10-1A M-4 (`no_memory_clobber` not propagated to BFG) — pre-existing.
- R10-1B I-3 (`handle_int_sub` width mismatch handling) — wave-31 IMP-4 fixed one direction; introduced subtle mismatch in another.
- R10-1B I-1 (`Region.contains_addr` empty case).
- R10-1D C-1 / C-2 (GuardPat / `*_any` binding) — pre-existing, never tested at the affected interface.
- R10-1F F-01 (PyMemPhiPat / PyValuePhiPat in PatLike) — pre-existing API gap.
- R10-1F F-02 (PyCapture hash truncation) — pre-existing.

**Pre-existing-and-known-deferred (from round 9):**
- IMP-1 AArch64-BE stack-array (still `#[ignore]`d).
- IMP-2 MIPS64 PIC GOT-indirect.
- IMP-3 PPC32/64 stack-array.
- IMP-5 ARM-BE VFP register aliasing.
- IMP-7 PPC float over-approximation.

---

## Files in this round

- `round10-coverage-manifest.md` — file inventory + baselines
- `round10-1A-ir.md` through `round10-1F-strider-py-aux.md` — 6 per-crate audits (Round 1)
- `round10-2A-panics.md`, `round10-2B-naming.md`, `round10-2C-silent-failures.md`, `round10-2D-types.md` — 4 cross-cutting passes (Round 2)
- `round10-3A-doc-verify.md`, `round10-3B-comments.md` — 2 verification passes (Round 3)
- `round10-test-plan.md` — 35 missing tests (Round 4)
- `round10-simplifications.md` — 60 simplification entries (Round 5)
- `round10-skill-audit.md` — 19 skills audited + 2 proposed (Round 6)
- `round10-summary.md` — this file (Round 7)

**Totals:** 16 review documents, ~4,150 lines.

---

## Verification at HEAD

- `cargo build --workspace`: ✓ green at start of round 10.
- `cargo test --workspace`: ✓ 123 binaries, 0 failures.
- `cd crates/strider-py && uv run pytest`: ✓ 766 passed, 11 skipped.
- `cargo clippy --workspace --lib`: warns on pre-existing `expect()` in `dot/src/lib.rs:203` (pre-round-10 issue).

No source code edited during the review.
