# Round 10 — Per-crate README + SKILL.md Diffs

Concrete edits from R10-3A / R10-3B for files outside CLAUDE.md.

---

## opt/README.md

### Edit 1 — Line 9: `Optimizer` trait method named `run`, takes only `&mut graph`

**Source:** R10-3A Refuted-1, R10-3B confirms.

**Current:**
```
`Optimizer` — every pass implements this trait. `run(&mut graph) -> Result<OptimizationResult>`
```

**Proposed:**
```
`Optimizer` — most passes implement this trait. `optimize(&self, graph: &mut Graph, entry: NodeId) -> Result<OptimizationResult>`.
`OptimizerOnBuilt` is the companion trait whose `optimize_built(&self, function: &mut pattern::RewriteCtx<'_>) -> Result<OptimizationResult>`
is wrapped by a blanket impl so both traits participate in the same pipeline.
```

### Edit 2 — Line 11-12: `OptimizationResult` variant `Unchanged`

**Source:** R10-3A Refuted-2.

**Current:**
```
`OptimizationResult::{Changed { ... }, Unchanged}`
```

**Proposed:**
```
`OptimizationResult::{Changed, NoChange}` (both unit variants).
```

### Edit 3 — `OptimizerOnBuilt` parameter type

**Source:** R10-3A Refuted-3.

Any line citing `&mut BuiltFunctionGraph` for `OptimizerOnBuilt::optimize_built` → replace with `&mut pattern::RewriteCtx<'_>`. Round 9 wave 28 migrated this.

---

## ir/README.md

No high-priority refutations found. Wave 31 already updated the `NodeOutputType` variant list to include U80/U128/U256/U512/F80.

---

## pattern/README.md

No high-priority refutations.

---

## cfg/README.md

No high-priority refutations.

---

## strider/README.md

No high-priority refutations.

---

## strider-py/README.md

No high-priority refutations.

---

## reader/README.md

No high-priority refutations.

---

## target/README.md

No high-priority refutations.

---

## pcode-lift/README.md

No high-priority refutations. Wave 31 updated `vn_mask` width list to include 32/64.

---

## dot/README.md, graphwalk/README.md, entity-utils/README.md

No findings.

---

## Root README.md

No high-priority refutations. Wave 29 updated `x86_64_systemv()` (no more deprecated `_abi`).

---

## SKILL.md updates (per R10-skill-audit.md)

### strider-orchestrator-extend/SKILL.md — Step 1 + Pitfalls

- **Current:** "CFG construction at `crates/strider/src/orchestrator.rs:837`"
- **Fix:** `:837` → `:908`.

### strider-builder-for-arch-migration/SKILL.md — Step 87

- **Current:** "`crates/cfg/src/cfg/builder/mod.rs:113` sets `preset: target::ArchPreset::X86_64`"
- **Fix:** `:113` → `:114`.

### strider-debug-pattern/SKILL.md — Step 2

- **Current:** "stable-default runs `ConstantFold` + `KnownBits` + `IfCondInversion`"
- **Fix:** Add `FlagCmpCanonicalize` to the list.

### strider-fixture-author/SKILL.md — Step 5

- **Current:** "15 today: ..."
- **Fix:** Update to 16 (added `ppc32le`).

### strider-flagcmp-rule-author/SKILL.md — Step 2

- **Current:** "around `mod.rs:310`"
- **Fix:** `:310` → `:314`.

### strider-py-binding/SKILL.md — Step 4

- **Current:** `pattern.rs::intern_capture`
- **Fix:** `intern_str` (actual function name in `crates/strider-py/src/pattern.rs:125`).

### strider-pattern-author/SKILL.md — Step 3

Same `intern_capture → intern_str` rename as strider-py-binding.

### strider-validation-invariant-extend/SKILL.md — Step 2 pitfall

- **Current:** "`crates/ir/src/validate/layer_c.rs:228-253` and the comment at lines 222-227"
- **Fix:** `:228-253` → `:234-258`; `:222-227` → `:224-232`.

### strider-cc-preset-extend/SKILL.md — Step 1 (significant)

All 10 CC preset line numbers stale. See R10-skill-audit.md for the correct values:

| Preset | Stale | Correct |
|---|---|---|
| `x86_cdecl` | 590ish | 708 |
| `x86_64_systemv` | 278ish | 381 |
| `x86_64_all_preserving` | 321ish | 414 |
| `aarch64_aapcs64` | 359 | 452 |
| `arm_aapcs` | 399 | 492 |
| `mips_o32` | 440 | 533 |
| `mips_n64` | 460ish | 567 |
| `powerpc_sysv32` | 501 | 594 |
| `powerpc64_elf_v1` | 538 | 631 |
| `powerpc64_elf_v2` | 572 | 674 |

Consider switching to symbol-name-only citations (e.g. `pub fn x86_64_systemv()`) to avoid future drift.

### strider-doc-line-number-refresh/SKILL.md — Examples

- **Current:** "Updated to `:127-200`"
- **Fix:** `:127-200` → `:132-184` (matches the live `strider-fingerprint-audit/SKILL.md`).

### strider-public-api-encapsulation/SKILL.md — V6 example

- **Current:** Lists V6 (`Cfg::start_addr_to_region_id` → `pub(crate)`) as completed.
- **Fix:** Move to "deferred" with rationale: "tightened then reverted in round 9 — `cfg/tests/cfg_query.rs` constructs `Cfg` via struct-literal syntax."

---

## In-`.rs`-file comment fixes (R10-3B)

These are not README edits but listed here for completeness; apply during code-edit phase.

### `crates/opt/src/pipeline.rs:137`

Self-contradictory migration note: "from `&mut pattern::RewriteCtx<'_>` to `&mut pattern::RewriteCtx<'_>`" — both ends identical (copy-paste error). Should read "from `&mut ir::BuiltFunctionGraph` to `&mut pattern::RewriteCtx<'_>`".

### `crates/pattern/src/rewrite.rs:13-19`

`pattern::rewrite_rule` doc claims `&mut BuiltFunctionGraph` but signature returns `&mut RewriteCtx<'g>`.

### `crates/strider/src/rewrite.rs:60-94`

`GraphRewriter::apply_rule` doc describes a `mem::take` BFG-swap that doesn't exist in the body.

### `crates/cfg/src/cfg/types.rs:119`

`RegionTerminator::Switch` doc says "reserved for the future jump-table resolver and is not constructed by the cfg builder today" — actually constructed at `region_builder.rs:508`.

### `crates/opt/src/pipeline.rs:364`

Test comment names non-existent error variant `ValidationFailed`. Real type is `ValidationErrors`.

### `crates/ir/src/builder/mod.rs:571`

Body comment names non-existent helper `build_call_other`. Should be `build_call_other_modeled`.

### `crates/ir/src/function.rs:153,173` + `crates/pattern/src/rewrite.rs:161` + `crates/strider/src/rewrite.rs:65`

`with_built` ghost references — replace with `with_rewrite_ctx` (the surviving function).

### `crates/ir/src/node/kind.rs:38,68`

Relative path doc links pointing at deleted `*.rs` files; targets are now `*/mod.rs`.

---

## Per-crate README inventory

| Crate | README status | Edit needed? |
|-------|---------------|--------------|
| cfg | correct | — |
| dot | correct | — |
| entity-utils | correct | — |
| graphwalk | correct | — |
| ir | correct | — |
| opt | needs Edits 1, 2, 3 | yes |
| pattern | correct | — |
| pcode-lift | correct | — |
| reader | correct | — |
| strider | correct | — |
| strider-py | correct | — |
| target | correct | — |
| Root README.md | correct | — |

**Total README edits:** 1 file (opt — three edits).

**Total SKILL.md edits:** 11 files needing line-number / cross-reference fixes; 1 (strider-cc-preset-extend) with all 10 entries stale.
