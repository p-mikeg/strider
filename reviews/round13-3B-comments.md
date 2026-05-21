# Round 13 — Stale Comment / Dead Reference Sweep

Scope: `crates/**/{src,tests,benches,examples}/**/*.rs` plus
`crates/strider-py/{src,strider,tests}/**/*.{rs,py}`.

Sweep targeted six categories: deleted-symbol names, closed-TODO breadcrumbs,
docstring-vs-code drift, round/wave migration breadcrumbs, broken
path/file/function references, and multi-round-old `TODO: implement` placeholders.

## Summary verdict

| Category | Status |
|---|---|
| 1. Deleted-symbol names (`CallOtherElide`, `NO_OP_USER_OPS`, `OptimizerOnBuilt`, `NodeVar`, `BuiltGraph`, `BuiltFnGraph`) | `OK no stale references` |
| 1b. `Builder::new` / `Builder::with_endianness` as **deleted** symbols still cited in comments | 5 findings |
| 2. Closed `TODO(TaskN)` / `TODO(closed)` / `TODO: implement` | `OK no stale references` (only 3 live TODOs, all pointing to an existing plan) |
| 3. Docstring-vs-code behaviour drift | 4 findings |
| 4. `round-N` / `wave-N` / `W-N` / `R12-…` migration breadcrumbs | 13 findings (vs ~50 stripped in Round 12 W2 — accumulation has restarted) |
| 5. Path / file / function references that no longer exist | 4 findings |
| 6. Multi-round-old `TODO: implement` placeholders | `OK no stale references` |

Total genuine findings: **26**.

---

## Critical issues (factually inaccurate comments)

| File:line | Symbol / breadcrumb | Reason | Proposed edit |
|---|---|---|---|
| `crates/strider/src/orchestrator.rs:264` | `Builder::with_endianness` | Says the field is "consumed by `Builder::with_endianness` per iteration" — but `with_endianness` was deleted; the sleigh is actually consumed by `Builder::for_arch` (see line 958 in the same file). | Replace `Builder::with_endianness` with `Builder::for_arch`. |
| `crates/strider-py/src/cfg.rs:50` | `deprecated Builder::new` | `Builder::new` is not "deprecated" — it was **deleted**. There is no `Builder::new` to call. The comment misleads readers into thinking they could still call it. | Reword to "the deleted `Builder::new` (pre-round-12-W15)" or simply state "`for_arch` is the only way to construct a `cfg::Builder` with a non-default `ArchPreset`". |
| `crates/strider-py/src/sleigh.rs:25` | `deprecated Builder::new` | Same as above — `Builder::new` no longer exists. | Same fix. |
| `crates/opt/src/indirect_branch_resolve/stack_array.rs:149` | `classify.rs:233-269` line-range cross-reference | Cites "Mirrors the `Truncate(_)` and `Extend(_)` arms in `crates/opt/src/indirect_branch_resolve/classify.rs:233-269`". `classify.rs` at lines 230-234 now explicitly states **no dedicated `Truncate(IntConst)` / `Extend(IntConst)` arm** exists (ConstantFold + build-time folding handle them). The comment promises a mirror that no longer exists. | Drop the line-range and reword to "previously mirrored the `Truncate(_)` / `Extend(_)` arms in `classify_anchor_with_rom_and_sp`; those arms were removed when ConstantFold rules 4-6 covered the same shapes". Or remove the cross-reference entirely. |
| `crates/opt/src/indirect_branch_resolve/stack_array.rs:125-126` | `classify_anchor_with_rom_and_sp` mirror claim | Same drift: "exactly mirroring the `Truncate(IntConst)` / `Extend(IntConst)` arms in `classify_anchor_with_rom_and_sp`". The arms are gone (line 230 of `classify.rs`). | Same fix — either re-describe the deletion or drop the claim. |
| `crates/reader/tests/elf_smoke.rs:15` | `strider::analyze_binary` | Module-doc says "matching the convention used by `cfg::cfg_integration` and `strider::analyze_binary`". `cfg::cfg_integration` exists, but `strider::analyze_binary` does **not** — the top-level entry is `strider::run`. | Replace with `strider::run` (or remove the parallel cite). |

---

## Stale `with_endianness` rationale comments

These three comments warn readers against using `Builder::with_endianness`, but
the function no longer exists — a future reader cannot use the bad alternative
even if they wanted to. The rationale survives but loses meaning without the
historical context.

| File:line | Issue | Proposed edit |
|---|---|---|
| `crates/strider/src/orchestrator.rs:954` | "`Builder::with_endianness` would silently default the preset to `X86_64`…" — but the method was deleted in round-12 (W14/W15). | Reword to: "Earlier revisions of `cfg::Builder` exposed a `with_endianness` constructor that defaulted `preset = X86_64`; the deletion is checked in by replacing the only construction path with `for_arch`. Keep this site on `for_arch` so a re-introduction can't regress." |
| `crates/strider/benches/scaling.rs:90` | Same rationale, same dead alternative. | Same fix. |
| `crates/strider/tests/common/mod.rs:216` | Same rationale, same dead alternative. | Same fix. |

These three are *consistent* (same rationale, all three preserved as warning
comments) so a single coordinated rewrite is appropriate.

---

## Round/wave migration breadcrumbs (R12-… / round-12 …)

Round 12 W2 stripped ~50 breadcrumbs; **13 new round-12 breadcrumbs have already
accumulated** in the same code that was cleaned. The pattern is identical to
the one W2 deleted: `(round-12 X-N)` parenthetical citing an internal task
identifier. These names are opaque to a future maintainer six months from
now.

| File:line | Breadcrumb | Proposed edit |
|---|---|---|
| `crates/cfg/src/cfg/mod.rs:68` | `hazard W14 fixed for the map` | Drop `W14` reference; the rationale stands on its own. |
| `crates/cfg/src/cfg/mod.rs:142` | `(round-12 S2.4)` | Drop the parenthetical. |
| `crates/cfg/src/cfg/options.rs:186` | `(round-12 EC-1)` | Drop. |
| `crates/cfg/src/cfg/options.rs:212` | `(round-12 R12-T-G)` | Drop. |
| `crates/cfg/src/cfg/options.rs:215-216` | `(round-12 EC-1)` | Drop. |
| `crates/cfg/src/cfg/builder/region_builder.rs:359` | `(round-12 R-9: the previous comment here described a TailCall fallback for the degenerate single-instruction case, but that path no longer exists…)` | Drop the meta-narrative ("the previous comment here described…"). The constructive statement that follows is the keeper. |
| `crates/cfg/tests/options.rs:30` | `round-12 EC-1` | Drop the round-12 prefix. |
| `crates/cfg/tests/cfg_query.rs:93` | `── W3: region_id_at_start public-API contract ──` | Drop the `W3:` prefix from the section header. |
| `crates/entity-utils/src/worklist.rs:218-219` | `the single-pass … shape introduced in W9 (S4.1)` | Drop "introduced in W9 (S4.1)". |
| `crates/opt/src/pipeline.rs:36` | `(round-12 TY-3)` | Drop. |
| `crates/opt/src/indirect_branch_resolve/mod.rs:104` | `(round-12 TY-2; previously `Result<_, anyhow::Error>`)` | Drop the round-12 prefix; keep the "previously Result" half if it's load-bearing. |
| `crates/pcode-lift/src/vn_io.rs:247` | `Hardened to a runtime error (round-12 EC-3)` | Drop the round-12 prefix; keep the "hardened to a runtime error" intent. |
| `crates/pcode-lift/src/vn_io.rs:325` | `Hardened to a runtime error in round-12 EC-3.` | Drop. |
| `crates/strider/src/orchestrator.rs:67-68` | `(round-12 R12-T-H)` | Drop. |
| `crates/pattern/src/rewrite.rs:155-156` | `**Field visibility note.** Both fields are `pub(crate)` (round-12 R12-T-A)` | Drop `(round-12 R12-T-A)`. |
| `crates/pattern/src/matcher/bindings.rs:24` | `round-12 R12-T-N tightened the fields to `pub(crate)`` | Drop the round-12 prefix; keep the constructive "tightened the fields to `pub(crate)`" rationale. |
| `crates/target/src/call_other_abi.rs:495` | `Regression: round-12 CA-2 — `sysret` and `swapgs` are x86/x86_64-specific user-ops.` | Drop the `round-12 CA-2` prefix; keep "Regression: …" + rationale. |

Recommended pattern for the cleanup: keep the *rationale* sentence (it
documents the invariant), drop the *issue ID* parenthetical (it points at an
ephemeral plan-tracking artifact). Match what Round 12 W2 did, since the
accumulation has re-started.

---

## Docstring-vs-code drift

| File:line | Issue | Proposed edit |
|---|---|---|
| `crates/ir/src/builder/mod.rs:137` | Doc says "the per-call O(V) linear scan in `pcode_lift::find_largest_fitting_register`". `pcode_lift::find_largest_fitting_register` is not a free function — it's `ValueLifter::find_largest_fitting_register` (a method, `pub(crate)`). Line 479 in the same file uses the correct path. | Change to `pcode_lift::ValueLifter::find_largest_fitting_register` to match line 479. |
| `crates/ir/src/dot/label.rs:8` | Path reference `crates/rsleigh/src/ctx_fmt.rs` is wrong — rsleigh is at `../rsleigh/`, NOT inside `crates/`. | Either drop the path hint or use `../rsleigh/src/ctx_fmt.rs`. |
| `crates/cfg/tests/vn_to_name.rs:6` | Same wrong-path issue: cites `crates/ir/src/dot/label.rs` (which is correct) but the upstream `rsleigh::Vn::ctx_fmt` is in `../rsleigh/`, not `crates/rsleigh/`. Worth re-checking when adjacent to the label.rs reference. | No action — this file's reference is to `crates/ir/src/dot/label.rs`, which exists. Listed for adjacency only. |
| `crates/ir/src/function.rs:194,210` | Docs route external callers to `opt::with_rewrite_ctx`, but that function is `pub(crate)` in the `opt` crate — external callers cannot construct via that path. The path that *is* public is `pattern::RewriteCtx::new(graph, entry)`, also mentioned. | Drop the `opt::with_rewrite_ctx` mention (or note it's internal). The public construction path is already named. |

---

## Stale terminology — "the analyzer"

The crate was renamed from `analyzer` to `strider` (and the per-function context
is now `Strider` / `IrStrider`). Multiple prose comments still say "the
analyzer" or "the analyzer's …", which now reads as referring to a non-existent
crate.

| File:line | Phrase | Proposed edit |
|---|---|---|
| `crates/cfg/tests/common/real_binary.rs:10` | `…by the analyzer-crate review` | "by the strider-crate review" or drop the historical reference. |
| `crates/ir/src/graph/mod.rs:68,70` | `Populated at IR construction time by the analyzer.` / `nodes synthesised by tests that don't go through the analyzer.` | Replace "the analyzer" with "the strider lifter" or "the IR builder". |
| `crates/ir/src/builder/mod.rs:224` | `the analyzer's register-aliasing logic` | "the pcode-lift register-aliasing logic" (it now lives in `pcode-lift`). |
| `crates/ir/src/builder/tests.rs:756,999,1219,1237` | `Replicate the analyzer's Piece composition.` / `the analyzer's write_reg_vn` etc. | Replace with `the strider lifter` / `the pcode-lift writer` as appropriate. |
| `crates/ir/src/node_signature.rs:336` | `when built by the analyzer; synthetic graphs may differ` | "when built by the strider lifter". |
| `crates/ir/src/node/output_type.rs:373,383` | `the analyzer's coerce helpers` | Same. |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:278` | `the analyzer's `IntConst` / `And` / `Truncate` /` | Same. |
| `crates/opt/src/known_bits/mod.rs:98` | `if the analyzer's …` | Same. |
| `crates/reader/tests/elf_smoke.rs:22` | `fixtures by the analyzer-crate review` | Drop the historical reference or rename. |
| `crates/strider/src/strider/insn/control.rs:197` | `when the analyzer's write-side coercion …` | "when the lifter's write-side coercion …" |
| `crates/strider/tests/patterns.rs:6,15` | `Analyzer side: Truncate-narrowing rules` / `the analyzer's IR shape` | Replace with "strider lifter". |
| `crates/target/src/calling_convention/tests.rs:385` | `StackStoreDetect and the analyzer's stack-arg machinery` | Same. |
| `crates/ir/src/dot/label.rs:271` | `when the analyzer …` | Same. |
| `crates/ir/src/builder/call.rs:331` | `Memory is the strider layer's call.` | Already uses "strider" — OK, listed for contrast. |

This is technically a Round 13 (terminology) finding, not strictly stale
comments, but it's a documentation-debt signal in the same vein.

---

## Other observations (not findings, but adjacent)

- `crates/opt/src/function_args/tests.rs:737` cites `commit 57005b9`. Specific
  commit SHAs in comments age poorly when histories are rebased. Consider
  documenting the *change* rather than the *commit*. Not a finding — the
  commit's content is also described inline so the comment still reads.
- `crates/pattern/src/matcher/bindings.rs:172` and `crates/pattern/src/pat/mod.rs:47`
  both reference "old typed-Var" / "typed Vars" — a transition that landed
  well before Round 12. Future readers will not know what "typed-Var" means.
  Consider replacing with "the Capture-only API" or just deleting the
  comparison.
- `crates/cfg/tests/vn_to_name.rs:5` references "the rsleigh-4 migration" — a
  version-tagged migration. Reads as load-bearing today but will become
  opaque once `rsleigh-5` ships. No action recommended yet.
- `crates/ir/src/ops/op_kinds.rs:97-98` contains meta-commentary on a fixed
  broken doc-link: `(`pcode_lift` is a separate crate, not under `crate::*` —
  the older `[`crate::pcode_lift`]` doc-link was broken.)`. The fix is in
  place; the explanation of *why the previous doc-link was wrong* serves no
  future reader. Recommend dropping the parenthetical.
- `CLAUDE.md` line 126 asserts `graphmock` is a workspace crate. It is not —
  `crates/graphmock` does not exist; the DSL was inlined into
  `crates/graphwalk/tests/common/mod.rs`. Out of scope for this sweep (it's
  documentation, not source-code comments), but flagging since the comment
  sweep surfaced it.

---

## What I checked (negative findings)

- `OptimizerOnBuilt`, `NodeVar`, `BuiltGraph`, `BuiltFnGraph`, `CallOtherElide`,
  `NO_OP_USER_OPS`: **no occurrences anywhere** (clean).
- `pattern::Var` / `pattern::NodeVar`: **no occurrences** (clean — fully
  migrated to `Capture`).
- `Strider::new`, `Strider::analyze_cfg`, `RewriteCtx::new`,
  `RewriteCtx::for_built`, `validate_with_options`,
  `classify_anchor_with_rom_and_sp`, `with_rewrite_ctx`,
  `build_call_other_modeled`, `build_call_other_terminal`,
  `OptimizerPipeline::run_on_built`: **all exist** at the cited symbol paths.
- All `crates/<x>/src/<y>.rs` paths cited in comments: **all 19 paths exist**
  (no broken file references).
- All `docs/superpowers/specs/...` and `docs/superpowers/plans/...` doc
  references: **all 10 paths exist**.
- `TODO` / `FIXME` / `HACK` / `XXX` survey: only 3 live TODOs, each pointing
  at the same existing plan
  (`docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`).
  No closed-but-not-removed TODOs.
- `TODO(TaskN)` / `TODO(closed)` / `TODO: implement`: **no occurrences**.
- `H10-` / `H11-` / `H12-` / `R10-` / `R11-`: **no occurrences**.

---

## Summary counts

- Critical / factually-wrong: **6**
- Stale rationale (warns against deleted alternative): **3**
- Round/wave migration breadcrumbs (new accumulation since W2 cleanup): **13**
- Docstring-vs-code drift: **4**
- Stale "analyzer" terminology: **14**
- Adjacent observations (not findings): **5**

Grand total of recommended edits: **26 active findings** plus **14 terminology touch-ups**.

The most impactful single change is the round-12 breadcrumb cleanup (matching
Round 12 W2's pattern): a single sweep pass dropping the `(round-12 X-Y)` /
`(round-12 R12-T-Z)` parentheticals would clear 13 of them in one PR.

The 6 critical findings are independent of the breadcrumb sweep and should
land first since they describe behaviour incorrectly.

Report written: `/mnt/c/Users/mikeg/Documents/strider/reviews/round13-3B-comments.md`.
