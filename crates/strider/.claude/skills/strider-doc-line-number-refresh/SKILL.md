---
name: strider-doc-line-number-refresh
description: Refresh stale `crates/<x>/src/<y>.rs:<line>` citations in CLAUDE.md, READMEs, and SKILL.md files when the underlying code drifts.
---

# strider-doc-line-number-refresh

## When to use

Triggers:
- "this skill cites `store.rs:160` but the function is at line 184"
- "CLAUDE.md says `vn_mask` accepts widths 1/2/4/8/10/16 but the code accepts 1/2/4/8/10/16/32/64"
- a contributor adds a new pass / variant / field and needs to update all the docs that reference it
- round-9 R9-3A doc-verify or R9-3B stale-comment sweep flagged a citation drift

## When NOT to use

- Adding new docs (use `strider-pattern-author` / `strider-opt-pass-author` etc. for content).
- Fixing comments inside `.rs` files (the targets here are CLAUDE.md, per-crate READMEs, and SKILL.md files).

## Procedure

1. Identify every doc file that cites the symbol:
   ```
   rg --type=md "<symbol_name>" /mnt/c/Users/mikeg/Documents/strider/
   ```
   Includes: `CLAUDE.md`, `README.md`, every `crates/<x>/README.md`, every `.claude/skills/<name>/SKILL.md`.
2. For each citation:
   - If the citation includes a line number (`store.rs:160`), check the actual line:
     ```
     grep -n "fn <symbol_name>" /mnt/c/Users/mikeg/Documents/strider/crates/<x>/src/<y>.rs
     ```
   - If the line drifted, update the citation.
   - If the symbol was renamed or moved, follow the rename and update both the symbol name and the path.
3. For variant/field lists (e.g. `NodeOutputType` enum, `vn_mask` widths, CC preset list):
   - Read the source enum / function to enumerate the actual variants.
   - Compare against the doc list. Add missing entries; remove deleted ones.
4. For tombstone references (a deleted function still mentioned by name):
   - If the doc explains a rename or migration, keep the historical mention but mark it explicitly: "deleted in round N" or "renamed to X".
   - If the doc is just stale, remove the reference.

## Where citations cluster

| File | Common citations |
|---|---|
| CLAUDE.md | Module-level summaries, `NodeOutputType` variants, CC presets, `vn_mask` widths, lift-time canonicalisations |
| `crates/ir/README.md` | NodeOutputType, NodeKind, validator layers, asm-fingerprint API |
| `crates/opt/README.md` | Pass list with brief descriptions, pre-built pipelines |
| `crates/target/README.md` | CC preset list, Note on link-register handling |
| `crates/pcode-lift/README.md` | `vn_mask` width support, register-aliasing semantics |
| Skills | Specific `path:line` references in their procedures, often pointing at function bodies |

## Verification

- `rg --type=md "<old_line_number>"` returns zero hits (or only intentional historical mentions).
- `rg --type=md "<symbol_name>" | wc -l` matches the expected reference count.
- `cargo build --workspace` and `cargo test --workspace --exclude strider-py` still pass (citations are doc-only; should never affect compilation).

## Examples (round-9 phase A applied these)

- `strider-fingerprint-audit/SKILL.md:24` — `store.rs:108-160` was stale; actual `asm_fingerprint` is at line 132 and `extend_asm_fingerprint_from` is at line 184. Updated to `:132-184`.
- `strider-opt-pass-author/SKILL.md:30` — `store.rs:160` for `extend_asm_fingerprint_from` was stale. Updated to `:184`.
- `crates/ir/README.md:50` — `NodeOutputType` listed `U8`–`U256` only; missing `U80`, `U512`, `F80`. Updated to enumerate all 12 variants.
- `crates/pcode-lift/README.md:49` — `vn_mask` width list said "1, 2, 4, 8, 10, 16"; missing 32 (YMM) and 64 (ZMM). Added.
- `CLAUDE.md:77` — listed deprecated `x86_64_systemv_abi`; renamed to `x86_64_systemv` (deprecated alias deleted in phase C).

## Anti-patterns

- "Fixing" a citation by re-grepping for the symbol every time the skill runs. The `path:line` form is for human navigation; if it's drifting frequently, the underlying file is volatile and the citation should reference the symbol name only (e.g. `crates/ir/src/graph/store.rs::extend_asm_fingerprint_from`).
- Padding the citation with a wide line range (`:50-200`) to avoid drift — defeats the navigation point. Cite the function start line; live with refreshes.
- Deleting historical context. A rename note like "renamed from X in round 8" helps contributors reading older code or commit history.
