# Round 12 — 3B: stale comment + dead-reference sweep

Audit agent: 3B
Branch: `review/ai6`
Workspace HEAD: `008f530`

## Scope & methodology

Workspace-wide ripgrep across `crates/**/{src,tests,benches,examples}/**/*.rs`
and `crates/strider-py/{src,strider,tests}/**/*.{rs,py}` for:

1. Names of deleted symbols (e.g. `CallOtherElide`, `NO_OP_USER_OPS`,
   `OptimizerOnBuilt`, `NodeVar`, `BuiltGraph`/`BuiltFnGraph`, `fg` rename
   leftovers).
2. Closed `TODO(TaskN)` / `TODO(closed)` patterns.
3. Behaviour drift between docstring and surrounding code.
4. `round-N` / `wave-N` / `W-N` / `R10-…` / `R11-…` / `R9-…` / `H10-…`
   migration breadcrumbs that have outlived their context.
5. Path / file / function references that no longer exist.
6. Multi-round-old `TODO: implement` placeholders.

Existing R11-3B was reviewed via summary table only (per trust model);
the source files referenced therein were re-grep'd directly to confirm
W8/W15 stripped what it claimed to strip and to find any new
accumulation since `3e1aefa` (Round 11 W8).

## Summary

| Category | Count |
|----------|-------|
| Deleted-symbol references | 0 |
| Closed-TODO references | 0 |
| Behaviour-drift descriptions | 0 |
| Migration breadcrumbs (round-N / wave-N / W-N) | 18 |
| Broken paths / dead intra-doc references | 1 |
| Multi-round-old placeholders | 0 |
| Orphan typography / strip residue | 2 |

### Headline

- **R9 / R10 / R11 W8 cleanup held** for the files W8 actually touched
  (`crates/cfg/src/cfg/options.rs`, `crates/cfg/src/cfg/types.rs`,
  `crates/cfg/src/cfg/arch.rs`, `crates/ir/src/builder/mod.rs`,
  `crates/target/src/arch.rs`, `crates/target/src/calling_convention/tests.rs`,
  the indirect-branch resolver doc, etc.).  Spot-checks confirm every
  `R9-2D M3`, `R9-2D H2`, `R9-1A I3`, `(round 9 P5 / R9-2D M6)`,
  `(round 9 V4 / R9-2D H3)`, `round 9 wave 24`, `round 9 Ask-8 R2 F7`,
  `round 9 wave 30 (D3+D4)` flagged in `round11-3B-comments.md` is gone.

- **R8 regression annotations untouched** by W8 — `Regression for
  round8-1A HIGH`, `Regression for round8-correctness-edge-cases`,
  `round8-correctness-invariants H-2`, `round8-2C H4`,
  `round8-correctness-cross-arch §1`, `round8-17 D-1`, `round8-1F MED`,
  `round8-1F HIGH`, `round8-correctness-borrowing HIGH`,
  `round8-repetition-sweep.md`, `Round 8 regression tests`.  Eleven sites
  total.  These were explicitly flagged for removal in
  `round11-2B-naming.md` (rows 44, 50, 56, 59, 71, 79, 85, 86) and
  `round11-summary.md:82` (M-27) but **were never executed** — they
  survived every R11 wave.

- **New R11 breadcrumb accumulation** — five `T-N (round 11)` tags
  introduced during R11 W13 test pinning (`T-23`, `T-14` ×2, `T-25`,
  `T-17`) and the related W9 reference (`introduced in W9 (S4.1)`).
  None of these tags are useful to a future reader; they reference the
  Round 11 test plan, which itself is now historical.

- **Two orphan strip residues** — W8 stripped substring tags from the
  middle of a sentence but left whitespace artefacts and dangling
  cross-references:
  - `crates/target/src/calling_convention/mod.rs:151` —
    `/// Validating constructor .  Builds a` (space-period-double-space
    where `(round 9 V4 / R9-2D H3)` was excised).
  - `crates/strider/src/orchestrator.rs:814` —
    `// (same reasoning as H-4 above).` is now an orphan cross-reference
    (no H-4 exists in the file; the original review-label scaffold was
    stripped).
  - `crates/strider/src/orchestrator.rs:1066` —
    `// ── apply_stall_guard tests (/ I-7) ───────────────────` carries
    a leading `(/ I-7)` orphan with no other context (the original was
    presumably `(round-N H-3 / I-7)` or similar).

## Findings — Migration breadcrumbs (criterion #4)

### F-1 — `Regression for round8-1A HIGH:` doc on `build_int_const_rejects_u256_and_u512`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/tests.rs:1612`
- **Comment:** `/// Regression for round8-1A HIGH: \`build_int_const\` and \`make_int_const\``
- **Why stale:** `round8-1A HIGH` names a closed Round 8 finding.  The
  test docstring explains the invariant cleanly on its own (next two
  lines).  The leading prefix is round-archaeology.
- **Suggestion:** `/// Regression: \`build_int_const\` and \`make_int_const\` must reject…`
- **Already flagged in** `round11-2B-naming.md:44` — not executed.

### F-2 — `Regression for round8-correctness-edge-cases H1` on `decompose_sp_does_not_stack_overflow_on_deep_chain`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/sp_expr.rs:865`
- **Comment:** `/// Regression for round8-correctness-edge-cases H1: \`decompose_sp\``
- **Why stale:** Round 8 finding ID.  The body of the docstring fully
  describes the invariant and the failure mode.
- **Suggestion:** `/// Regression: \`decompose_sp\` must not blow the thread stack…`
- **Already flagged in** `round11-2B-naming.md:50` — not executed.

### F-3 — `Regression for round8-correctness-edge-cases H2` on `step_through_stack_store_phi_empty_offsets_returns_may_alias`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/sp_expr.rs:896`
- **Comment:** `/// Regression for round8-correctness-edge-cases H2: a \`StackStorePhi\``
- **Why stale:** Same pattern as F-2.
- **Suggestion:** `/// Regression: a \`StackStorePhi\` with empty…`
- **Already flagged in** `round11-2B-naming.md:50` — not executed.

### F-4 — `Regression for round8-correctness-invariants H-2` on `bool_neg_fingerprint_absorbed_into_inner_cond`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/if_cond_inversion/tests.rs:167`
- **Comment:** `/// Regression for round8-correctness-invariants H-2: when the \`If\`'s`
- **Why stale:** Round 8 finding ID prefix; rest of the docstring is
  self-contained.
- **Suggestion:** `/// Regression: when the \`If\`'s \`BoolNeg(cond)\` becomes dead…`
- **Already flagged in** `round11-2B-naming.md:56` — not executed.

### F-5 — `Regression for round8-2C H4` on `apply_tail_call_rejects_non_integer_target_type`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:414`
- **Comment:** `/// Regression for round8-2C H4: \`apply_tail_call\` must propagate an`
- **Why stale:** Round 8 finding ID prefix.
- **Suggestion:** `/// Regression: \`apply_tail_call\` must propagate an \`Err\`…`
- **Already flagged in** `round11-2B-naming.md:59` — not executed.

### F-6 — Orphan `H0` heading in inplace.rs
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:305`
- **Comment:** `// H0 — calling-convention threading tests for the in-place editors.`
- **Why stale:** `H0` is the round-8 finding-ID label; it survives here
  as a bare opaque tag with no defining context anywhere in the file.
  The rest of the comment block is descriptive enough on its own.
- **Suggestion:** `// Calling-convention threading tests for the in-place editors.`
- **Already flagged in** `round11-2B-naming.md:59` (as "H0 — calling-
  convention threading tests") — not executed.

### F-7 — `Regression for round8-17 D-1` on `x86_memory_fences_classify_as_pure_with_mem_edge`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/call_other_abi.rs:822`
- **Comment:** `/// Regression for round8-17 D-1: x86/x86_64 memory fences (mfence,`
- **Why stale:** Round 8 finding ID.  The docstring already explains
  what the test pins (`PURE_WITH_MEM_EDGE` for mfence/sfence/lfence).
- **Suggestion:** `/// Regression: x86/x86_64 memory fences (mfence, sfence, lfence)…`

### F-8 — `round8-correctness-cross-arch §1` in `strider/benches/scaling.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/benches/scaling.rs:92`
- **Comment:** `// round8-correctness-cross-arch §1.`
- **Why stale:** The preceding 4 lines correctly state the invariant
  (`with_endianness` defaults preset to X86_64 — use `for_arch`).
  CLAUDE.md also documents this.  The bare round-8 cite is noise.
- **Suggestion:** Drop the trailing `// round8-correctness-cross-arch §1.` line.
- **Already flagged in** `round11-2B-naming.md:71` — not executed.

### F-9 — `See round8-correctness-cross-arch §1.` in `strider/tests/common/mod.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs:219`
- **Comment:** `// See round8-correctness-cross-arch §1.`
- **Why stale:** Duplicate of F-8; preceding comment is self-contained.
- **Suggestion:** Drop the `// See round8-correctness-cross-arch §1.` line.
- **Already flagged in** `round11-2B-naming.md:79` — not executed.

### F-10 — `reviews/round8-repetition-sweep.md (#1)` in `opt/src/test_support.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/test_support.rs:10`
- **Comment:** `//! \`reviews/round8-repetition-sweep.md\` (#1) — the same logic was duplicated 14× across opt's white-box test modules.`
- **Why stale:** Promoting a helper module out of inline copy-paste is
  done.  The "where this came from" cite is round-archaeology.  Stating
  "this is the single shared helper" is enough.
- **Suggestion:** Replace last two sentences with: `Promoted from a
  per-test-file inline pattern duplicated across opt's white-box test
  modules.`

### F-11 — `reviews/round8-repetition-sweep.md (#1)` in `opt/src/stack_load_forward/tests.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_load_forward/tests.rs:28`
- **Comment:** `// Delegate to the shared helper promoted in \`test_support\` — see \`reviews/round8-repetition-sweep.md\` (#1).`
- **Why stale:** Same as F-10.
- **Suggestion:** `// Delegate to the shared helper promoted in \`test_support\`.`

### F-12 — `reviews/round8-repetition-sweep.md (#5)` in `strider/src/test_utils.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/test_utils.rs:12-14`
- **Comment:** `//! Promoted from the per-test-file inline pattern flagged by //! \`reviews/round8-repetition-sweep.md\` (#5) — 18 sites duplicated the //! same 3-line setup.`
- **Why stale:** Same as F-10.  The substantive "this consolidates 18
  call-sites" is fine; the file-cite is round-archaeology.
- **Suggestion:** `//! Promoted from a per-test-file inline pattern that
  duplicated the same 3-line setup across the strider test suite.`

### F-13 — `T-23 (round 11)` on `enqueue_dedup_at_ten_thousand_scale`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/entity-utils/src/worklist.rs:217-219`
- **Comment:** `/// T-23 (round 11): \`enqueue\` deduplicates at 10k-item scale.  Pins the /// single-pass \`if workset.insert(e) { push }\` shape introduced in W9 /// (S4.1) — re-enqueueing the same id never duplicates the queue.`
- **Why stale:** `T-23 (round 11)` ties the test to a closed test-plan
  ID; `introduced in W9 (S4.1)` ties the impl shape to a closed wave.
  The test name + body fully describe what is being pinned.
- **Suggestion:** `/// Pins single-pass dedupe in \`enqueue\` at 10k-item scale.  Re-enqueueing the same id must never duplicate the queue.`

### F-14 — `T-14 (round 11)` on `bit_mask_u128_for_u256_and_u512_is_u128_max`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs:443`
- **Comment:** `/// T-14 (round 11): \`bit_mask_u128\` for \`U256\` and \`U512\` must return /// \`u128::MAX\` — the conservative \`u128\`-width approximation, since /// these widths exceed the carrier.  Pins the \`bits >= 128\` guard.`
- **Why stale:** `T-14 (round 11)` prefix; rest is fine.
- **Suggestion:** `/// \`bit_mask_u128\` for \`U256\` and \`U512\` must return…`

### F-15 — `T-14 (round 11)` on `get_unsigned_int_for_u256_passes_through_small_values`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs:452`
- **Comment:** `/// T-14 (round 11): \`get_unsigned_int\` for \`U256\`/\`U512\` passes through /// values within the \`u128\` carrier (no false rejection).`
- **Why stale:** Same as F-14.
- **Suggestion:** Drop the `T-14 (round 11):` prefix.

### F-16 — `T-25 (round 11): Kb constructor invariant` separator
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/tests.rs:469`
- **Comment:** `// ── T-25 (round 11): Kb constructor invariant ────────────────────────────────`
- **Why stale:** Section-header breadcrumb naming a closed test-plan ID.
- **Suggestion:** `// ── Kb constructor invariant ──────────────────────────────────────`

### F-17 — `T-17 (round 11): default pipeline composition` separator
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/optimizer_pipeline_subsets.rs:142`
- **Comment:** `// ── T-17 (round 11): default pipeline composition ────────────────────────────`
- **Why stale:** Same pattern as F-16.
- **Suggestion:** `// ── default pipeline composition ──────────────────────────────`

### F-18 — `wave-1 M3 fix` in `reader/tests/elf_relocations.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/reader/tests/elf_relocations.rs:93`
- **Comment:** `// + dynsym + dynstr + rela.dyn + program headers.  The wave-1 M3 fix // is verified by:`
- **Why stale:** Wave-N breadcrumb naming the audit task that introduced
  the bucket-naming fix.  The "verified by:" enumeration that follows
  is the substantive content.
- **Suggestion:** `// + dynsym + dynstr + rela.dyn + program headers.  The bucket-naming fix is verified by:`

### F-19 — `the wave-2 fix (\`>=\` → \`>\`)` in `strider/src/orchestrator.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:1068`
- **Comment:** `// These tests pin the wave-2 fix (\`>=\` → \`>\`) by exercising the // stall-guard behavior directly via the extracted helper.`
- **Why stale:** "wave-2 fix" is migration archaeology.  The actual
  invariant (`>=` vs. `>`) is fine to keep — but should not be
  framed as a delta against a pre-shipping state.
- **Suggestion:** `// These tests pin the strict-growth comparison
  (\`>\`, not \`>=\`) in the stall guard by exercising the extracted
  helper directly.`

## Findings — Orphan strip residue (criterion #5 / #3 hybrid)

### F-20 — Space-period orphan after `Validating constructor`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:151`
- **Comment:** `/// Validating constructor .  Builds a`
- **Why stale:** W8's substring-strip of `(round 9 V4 / R9-2D H3)` left
  a literal `constructor .` (space-before-period).  Cosmetic; renders
  with awkward spacing in rustdoc.
- **Suggestion:** `/// Validating constructor.  Builds a`

### F-21 — Orphan `(same reasoning as H-4 above)` cross-reference
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:814`
- **Comment:** `// surface unsupported clobber-reg sizes as Err // (same reasoning as H-4 above).`
- **Why stale:** `H-4` is a round-8 finding label.  No matching `H-4`
  exists anywhere else in the file (or any other file in the workspace).
  Reader cannot follow the reference.
- **Suggestion:** Drop the `(same reasoning as H-4 above).` clause, or
  inline the actual reasoning: the upstream comment around `vn_size_to_node_output_type` already covers it.

### F-22 — Orphan `(/ I-7)` separator label
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:1066`
- **Comment:** `// ── apply_stall_guard tests (/ I-7) ───────────────────`
- **Why stale:** The `(/ I-7)` looks like a half-stripped review-ID
  pair `(round-N H-3 / I-7)`.  The slash sits naked at the start of
  the parens.  Reader cannot decode the residue.
- **Suggestion:** `// ── apply_stall_guard tests ──────────────────────`

## Findings — Round-9 wave-25 breadcrumbs in Python tests

### F-23 — `Round 9 H-8 regression:` section + 3 docstrings in `test_pattern_match.py`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/tests/python/test_pattern_match.py:118, 122, 148, 167`
- **Comments:**
  - line 56: `# ── Round 8 regression tests ───────────────────────────────────────────────`
  - line 118: `# ── Round 9 H-8 regression: KeyboardInterrupt / SystemExit propagation ───`
  - line 122: `"""Round 9 H-8: a \`.when()\` predicate that raises \`KeyboardInterrupt\`…`
  - line 148: `"""Round 9 H-8: a \`.when()\` predicate that raises \`SystemExit\` must…`
  - line 167: `"""Round 9 H-8 companion: ordinary predicate exceptions…`
- **Why stale:** Section headers and docstring prefixes naming Round 8
  / Round 9 finding IDs.  W8 explicitly excluded this file
  (only `test_typed_errors_e2e.py` and `test_optimizer_pipeline.py`
  were touched in strider-py's test directory).  W8's stated rule
  was "drop the round/H tag, keep the substantive description"; the
  fix was simply forgotten here.
- **Suggestion:** Drop every `Round N H-N` / `Round 8 regression`
  prefix; the docstring body of each test fully describes the
  pinned invariant.
- **Already flagged in** `round11-2B-naming.md:85` — not executed.

### F-24 — `Round 9 wave 25 (I-10):` ×3 in `test_typed_errors_e2e.py`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/tests/python/test_typed_errors_e2e.py:164, 196, 231`
- **Comments:** Three docstrings opening with `"""Round 9 wave 25 (I-10): trigger \`<ErrorType>\` by…`.
- **Why stale:** R11 W8 explicitly touched this file (its diff strips
  `Strengthened by round 10 R10-1F F-05:` at line 152) but skipped
  these three `Round 9 wave 25 (I-10):` prefixes.  Likely missed
  because they sit in test docstrings rather than free-floating
  comments; the W8 grep matched only the `R10-1F` form.
- **Suggestion:** Drop `Round 9 wave 25 (I-10):` from each; the rest
  of every docstring (which already describes how the error is
  triggered and why) stands on its own.

### F-25 — `Round 10 T-3 regression:` on `classify_anchor_is_idempotent_on_unchanged_graph`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_resolve_classify.rs:283`
- **Comment:** `/// Round 10 T-3 regression: calling \`classify_anchor\` twice on the // same graph (without optimization between calls) must produce the // same verdict.`
- **Why stale:** Round 10 finding ID prefix; the rest of the docstring
  (3 paragraphs, full failure-mode example) is self-contained and
  explains the invariant without reference to a finding ID.
- **Suggestion:** `/// Regression: calling \`classify_anchor\` twice on
  the same graph must produce the same verdict…`

## Findings — Closed-task references (criterion #2)

No closed `TODO(TaskN)` references found.  The three live `TODO:
remove after incremental indirect-resolve lands` markers
(`cfg/src/cfg/decode_cache.rs:35`, `strider/src/orchestrator.rs:287`,
`strider/src/strider/pipeline.rs:43`) all reference the same
real plan file `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`
which still exists; these are legitimate open work.

The opaque `(Task 15)`, `(Task 1)`, `(Task 4)`, `(Task 5)`, `(Task 16)`
references flagged in `round10-3B-comments.md:321-333`,
`round11-2B-naming.md:42,63,69,70,76` are still present:

- `crates/cfg/src/cfg/builder/indirect_resolve.rs:191` — `(Task 15).`
- `crates/opt/src/constant_fold/tests.rs:1428` — `Comprehensive tests added in Task 2.E`
- `crates/opt/src/stack_store/tests.rs:586` — `Comprehensive tests added in Task 5`
- `crates/reader/tests/elf_converters.rs:277` — `Task 1: a FILTER-REJECTED malformed section is silent, not an error`
- `crates/reader/tests/mem_region.rs:283` — `it's only meaningful once Task 1 privatizes them.`
- `crates/strider/tests/common_smoke.rs:18` — `convention (Task 16) and the BE shift formula fix (Task 4).`

These are pre-existing, were flagged twice, and have not been
executed.  Recording for round 12 visibility under criterion #5
(reference is unresolvable — there is no Task-N table anywhere
indexing what these IDs mean; nine different documents in
`docs/superpowers/plans/` use overlapping `Task N` numbering).

### F-26 — Bare `(Task 15)` cite in `indirect_resolve.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/indirect_resolve.rs:191`
- **Comment:** `// (Task 15).`
- **Why stale:** Opaque ID with no context.  Already flagged twice
  (round10-3B-comments.md E1; round11-2B-naming.md row 1).
- **Suggestion:** Either delete or expand into prose; existing
  preceding paragraph already describes "skip the second pipeline
  run when the load-folding step didn't fold anything."

### F-27 — `Comprehensive tests added in Task 2.E` separator
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/constant_fold/tests.rs:1428`
- **Comment:** `// ── Comprehensive tests added in Task 2.E ─────────────────────────────────────`
- **Suggestion:** `// ── Comprehensive shift/extension/cast tests ──────────────────────`

### F-28 — `Comprehensive tests added in Task 5` separator
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_store/tests.rs:586`
- **Comment:** `// ── Comprehensive tests added in Task 5 ──────────────────────────────────────`
- **Suggestion:** Rename to describe the test bucket, drop the Task tag.

### F-29 — `Task 1:` finding-ID prefix in `elf_converters.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/reader/tests/elf_converters.rs:277`
- **Comment:** `// ── Task 1: a FILTER-REJECTED malformed section is silent, not an error ──`
- **Suggestion:** `// ── A FILTER-REJECTED malformed section is silent, not an error ──`

### F-30 — `it's only meaningful once Task 1 privatizes them` in `mem_region.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/reader/tests/mem_region.rs:283`
- **Comment:** `/// still passes; it's only meaningful once Task 1 privatizes them.`
- **Why stale:** Speculates about a future change that may never
  happen.  Test as-written passes regardless; the comment is dead.
- **Suggestion:** Drop the `it's only meaningful…` sentence.

### F-31 — `(Task 16)` and `(Task 4)` in `common_smoke.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common_smoke.rs:18`
- **Comment:** `// Smoke-test the BE MIPS path — exercises both the new mips_o32 calling // convention (Task 16) and the BE shift formula fix (Task 4).`
- **Why stale:** Two opaque Task references.
- **Suggestion:** `// Smoke-test the BE MIPS path — exercises both the
  \`mips_o32\` calling convention and the BE shift-formula fix.`

### F-32 — `T-20:` finding-ID prefix on `match_value_accessors_on_control_flow_capture_return_none`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/tests/matching/control_flow.rs:201`
- **Comment:** `/// T-20: a control-flow \`Capture\` (bound to a Return node, which has`
- **Why stale:** Test-plan ID prefix.  Body is descriptive.
- **Suggestion:** Drop `T-20: `.

### F-33 — `T-1 (M-1):` finding-ID prefix on `phi_input_addresses_predecessor_slot_not_phi_token`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/tests/matching/ssa.rs:97`
- **Comment:** `/// T-1 (M-1): \`phi_for(vn).input(idx, p)\` must address predecessor`
- **Why stale:** Two stacked review-IDs.  Body explains the invariant.
- **Suggestion:** Drop the `T-1 (M-1): ` prefix.

### F-34 — `T-16 from round10-test-plan.md` in `cc_validation.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/tests/cc_validation.rs:4`
- **Comment:** `//! T-16 from round10-test-plan.md: pin that listing the SP varnode //! in \`arg_passing_regs\` produces a clear validation \`Err\` rather //! than a downstream miscompile.`
- **Why stale:** Path-tagged test-plan reference.  Path is real
  (`reviews/round10-test-plan.md` exists), but the body fully
  describes the invariant; the prefix is breadcrumb-noise.
- **Suggestion:** `//! Pin that listing the SP varnode in
  \`arg_passing_regs\` produces…`

### F-35 — `T-30` cross-reference note in `value_lifter.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/tests/value_lifter.rs:891`
- **Comment:** `// Note: T-30 (IntLessEqual lowering shape) is covered by the existing // \`lift_int_less_equal_lowers_to_boolneg_less\` test earlier in this file.`
- **Why stale:** Reader has to know what T-30 is to evaluate the
  cross-reference.  The named test exists in the same file — the
  pointer is fine, the tag is not.
- **Suggestion:** `// IntLessEqual lowering shape is covered by the
  existing \`lift_int_less_equal_lowers_to_boolneg_less\` test
  earlier in this file.`

### F-36 — `(T-2: build routes through try_from_parts)` in `cc_validation.rs`
- **Location:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/tests/cc_validation.rs:92`
- **Comment:** `.expect("x86_64_systemv must build cleanly (T-2: build routes through try_from_parts)");`
- **Why stale:** Test-plan ID inside an `.expect` message.  Without
  context, "(T-2: …)" is opaque to anyone reading test failures.
- **Suggestion:** Drop the leading "T-2:" tag; keep "build routes
  through try_from_parts" as the substantive hint.

## Findings — Behaviour drift (criterion #3)

None found.  Spot-checks confirmed:

- `target/src/call_other_abi.rs:822` test references `PURE_WITH_MEM_EDGE`
  — that const still exists at line 220.
- `opt::indirect_branch_resolve` docs accurately describe the
  classifier producer-shape arms.
- `CallOtherElide` / `NO_OP_USER_OPS` deletions are clean across the
  workspace (zero hits).
- `OptimizerOnBuilt` trait collapse (R11 W15) is clean: zero hits in
  source comments.
- All `RewriteCtx` / `RewriteCtxView` docstrings match the current
  API in `crates/pattern/src/rewrite.rs`.
- CLAUDE.md re-reads cleanly against `crates/target/src/calling_convention/mod.rs`
  (link-register-handling note matches; CC presets enumeration
  matches; `try_from_parts` invariant list at line 154-163 matches
  the actual checks at 176-196).

## Findings — Deleted-symbol references (criterion #1)

None.

## Findings — Multi-round-old placeholders (criterion #6)

None.  No `TODO: implement` / `TODO: figure out` / `unimplemented!` /
empty function-body placeholders found that have outlived their plan.

## Positive findings

- Live `TODO: remove after incremental indirect-resolve lands` markers
  (3 sites) all point at a real plan file and identify the surface
  area that becomes redundant on landing.  Good shape for an open
  TODO.
- Per-pass module-level docstrings in `crates/opt/src/**/mod.rs`
  (`StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`,
  `IfCondInversion`, `RedundantPhis`, `DeadBranchElimination`,
  `LoadReadOnly`, `KnownBits`) accurately describe what the pass
  does, what shape it expects, and what it produces — no drift.
- `crates/pattern/src/matcher/match_result.rs:200-225` — the
  `clobber_start` derivation comment for CallOther accurately
  describes the per-CallOther override length contract introduced
  by the precise-ABI work.

## Recommendation roll-up for round 12 fix-sweep

Recommend bundling F-1 through F-19 and F-23 through F-36 as a single
"round-N migration breadcrumb strip" pass (mostly mechanical prefix
removal, no behavioural risk).  F-20 / F-21 / F-22 are also pure
text-edits.  Net: 32 single-line / paragraph edits across 20 files,
zero source-functional change.  All flagged sites were either
documented in round11-2B-naming.md or round11-3B-comments.md and
deferred to W8 (which only executed a subset), or accumulated *during*
round 11 (the `T-N (round 11)` family).

No deleted-symbol drift, no closed-TODO drift, no behaviour drift —
the codebase is well-maintained on those axes.  The remaining noise is
narrative scaffolding from the multi-round audit process itself.
