# Round 10 — CLAUDE.md Correctness Diff

Concrete edits derived from R10-3A (doc-verify) and R10-3B (stale-comment sweep). Apply only after user approval.

---

## Edit 1 — Line 85-86: pipeline composition mixes crate-level and strider-level

**Source:** R10-3A Partial-1.

**Current:**
```
Three pre-built top-level pipelines: `default_pipeline()` (all passes),
`stable_default_pipeline()` (rewrites that survive phi-input growth — `ConstantFold` + `KnownBits` +
`FlagCmpCanonicalize` + `IfCondInversion`), `destructive_default_pipeline()` (node-removal passes
safe only at fixed point — `RedundantPhis` + `DeadBranchElimination`).
```

**Issue:** Description matches `opt::stable_default_pipeline()` (4 passes) but is then immediately followed by `Strider::build_stable_optimizer_pipeline` text without separating the two.

**Proposed:**
```
Three pre-built top-level pipelines from the **opt crate**:

- `default_pipeline()` — ConstantFold + KnownBits + FlagCmpCanonicalize + IfCondInversion + RedundantPhis + DeadBranchElimination.
- `stable_default_pipeline()` — first 4 of the above (rewrites that survive phi-input growth).
- `destructive_default_pipeline()` — last 2 (node-removal passes, safe only at fixed point).

`Strider::build_stable_optimizer_pipeline()` layers `StackStoreDetect` + `StackLoadForward`
fixed-point passes plus a `FunctionArgDetect` post-pass on top of `opt::stable_default_pipeline()`,
yielding a 7-pass stable pipeline. `Strider::build_destructive_optimizer_pipeline()` adds
`CallStackArgCollect` post-pass.
```

---

## Edit 2 — Line 89: `Optimizer` / `OptimizerOnBuilt` clarification

**Source:** R10-3A confirmed.

The current text added in round 9 wave 31 (`"Most passes implement Optimizer; OptimizerOnBuilt is a companion trait..."`) is correct. **No edit required** — already accurate.

---

## Edit 3 — `with_built` ghost references in nearby prose

**Source:** R10-3B HIGH.

Search CLAUDE.md for any `with_built` mention that was not removed in round 9 wave 28; replace with `with_rewrite_ctx`. (One nearby reference may have lingered.)

Run `grep -n with_built CLAUDE.md` post-edit; expect zero hits.

---

## Edit 4 — verify `IfCondInversion` `OptimizerOnBuilt` clarification (already in)

**Source:** R10-3A confirmed.

Wave 31 added the trait-split clarification at line 89. No further edit needed.

---

## Verified-correct claims (no edit needed)

R10-3A's 18 confirmed claims sampled across CLAUDE.md include:
- Line 50-52: `NodeOutputType` variant list with U80/U512/F80 ✓
- Line 89: `RewriteCtx<'_>` Deref<Target=Graph> + preorder/preorder_kind ✓
- Line 147: `vn_mask` widths 1/2/4/8/10/16/32/64 ✓
- Line 75-77: CC presets list (`x86_cdecl`, `x86_64_systemv`, `aarch64_aapcs64`, etc.) ✓
- Line 109-114: 8 lift-time canonicalisation aliases ✓
- The `opt::indirect_branch_resolve` "NOT in default_pipeline()" note ✓
- LR-as-callee-saved deliberate tradeoff for AArch64/ARM/MIPS/PPC ✓

---

## Application

If user approves these edits:

1. Apply Edit 1 (split crate-level vs strider-level pipeline composition).
2. Run `grep -n with_built CLAUDE.md` to confirm Edit 3 is unnecessary or apply.
3. Verify against current line numbers (CLAUDE.md may have drifted by ±2).
4. Commit: `docs: round 10 CLAUDE.md correctness fixes`.

---

## Out of scope for CLAUDE.md (handled elsewhere)

- Per-crate README corrections — see `round10-readme-diffs.md`.
- `*.rs` doc-comment fixes — bundled with code changes when relevant.
- SKILL.md updates — see `round10-skill-audit.md`.
