# Round 10 — Skill Audit

Reviewing all 19 skills under `crates/strider/.claude/skills/`. Each cited path / function / line verified against current code.

---

## Per-skill verdicts

### strider-callother-abi — KEEP-AS-IS
All citations verified. `crates/target/src/call_other_abi.rs::classify(preset, name)` at line 61, `classify_arch_specific` at line 76, `classify_arch_independent` at line 246, `CallOtherAbi` struct at line 12 — all match.

### strider-cli-runner — NEEDS-UPDATE (minor)
- Step 2 cites `Builder::new(...)` in `crates/strider/examples/strider.rs` — verified accurate. Worth noting in the skill that `Builder::new` defaults LE+X86_64 so it's safe for x86 but not for other arches.
- Step 7 `validate_with_options` description is accurate but marginally ambiguous about field access.
- **Fix:** Add note to step 2 explaining Builder::new's default safety.

### strider-debug-pattern — NEEDS-UPDATE
- **Step 2:** Says `stable_default_pipeline` runs `ConstantFold + KnownBits + IfCondInversion`. Actual is `ConstantFold + KnownBits + FlagCmpCanonicalize + IfCondInversion` (4 passes — `crates/opt/src/lib.rs:134`).
- **Fix:** Add `FlagCmpCanonicalize` to the list.

### strider-fixture-author — NEEDS-UPDATE
- **Step 5:** Says "15 today" — actual `per_arch_test!` macro expands to **16** (added `ppc32le`).
- **Fix:** Update count + list to match `tests/common/mod.rs:424-439`.

### strider-flagcmp-rule-author — NEEDS-UPDATE (minor)
- **Step 2:** "around `mod.rs:310`" — `build_rules` actually starts at line **314**.
- **Fix:** Update `:310` → `:314`.

### strider-indirect-shape-author — NEEDS-UPDATE
- **Step 5:** Says `crates/strider/src/indirect_resolve/inplace.rs` "hosts" the apply functions. They are *defined* in `crates/opt/src/indirect_branch_resolve/inplace.rs` and *re-exported* from strider.
- **Fix:** Clarify "defined in opt, re-exported from strider".

### strider-orchestrator-extend — NEEDS-UPDATE
- **Step 1 & Pitfalls:** "CFG construction at `orchestrator.rs:837`" — actual is line **908**.
- Other line references are within ±1 of actual.
- **Fix:** Update orchestrator call-site citation `:837` → `:908`.

### strider-py-binding — NEEDS-UPDATE (minor)
- **Step 4:** References `pattern.rs::intern_capture` — actual function is `intern_str` at `crates/strider-py/src/pattern.rs:125`.
- **Fix:** Rename in skill.

### strider-target-arch — KEEP-AS-IS
All cited file paths and procedure accurate.

### strider-validation-invariant-extend — NEEDS-UPDATE (minor)
- **Step 2 pitfall:** Cites `layer_c.rs:228-253` and comment at `:222-227`. Actual: `check_layer_c_function_arg_uniqueness` at line 234, comment at `:224-232`.
- **Fix:** Update `:228-253` → `:234-258`; `:222-227` → `:224-232`.

### strider-opt-pass-author — NEEDS-UPDATE (minor)
- The `Optimizer` vs `OptimizerOnBuilt` description is current per wave 28; one minor clarification: `Optimizer::optimize` takes `(&mut Graph, NodeId)` but the framework bridges to `RewriteCtx` internally.
- `extend_asm_fingerprint_from` at `crates/ir/src/graph/store.rs:184` — verified.
- **Fix:** Clarify in step 1 that new passes should prefer `OptimizerOnBuilt`.

### strider-cc-preset-extend — NEEDS-UPDATE (significant)
**All 10 CC preset line numbers in step 1 are wrong** (drift from earlier rounds):
- `x86_cdecl`: 590ish → **708**
- `x86_64_systemv`: 278ish → **381**
- `x86_64_all_preserving`: 321ish → **414**
- `aarch64_aapcs64`: 359 → **452**
- `arm_aapcs`: 399 → **492**
- `mips_o32`: 440 → **533**
- `mips_n64`: 460ish → **567**
- `powerpc_sysv32`: 501 → **594**
- `powerpc64_elf_v1`: 538 → **631**
- `powerpc64_elf_v2`: 572 → **674**
- **Fix:** Update all 10 line refs. Consider switching to symbol-name-only citations to avoid future drift (e.g., `pub fn x86_64_systemv()`).

### strider-builder-for-arch-migration — KEEP-AS-IS
All 8 migration sites and the `cfg::Builder::for_arch` description match the current code state.

### strider-silent-failure-audit — KEEP-AS-IS
All anti-patterns and round-9 references verified.

### strider-doc-line-number-refresh — NEEDS-UPDATE (meta)
- The "example" cites `strider-fingerprint-audit/SKILL.md:24 → :127-200` but the actual current value is `:132-184`. Internal example inconsistency in the doc-refresh skill itself.
- **Fix:** Update example to `:132-184`.

### strider-rewrite-rule-multinode-audit — KEEP-AS-IS
All cited paths and tests verified.

### strider-public-api-encapsulation — NEEDS-UPDATE
- **V6 example claim:** "`cfg::Cfg::start_addr_to_region_id` was tightened to `pub(crate)`." Actual code at `crates/cfg/src/cfg/mod.rs:72` has the field still `pub` — V6 was attempted then reverted (comment at 66-71 says "tightened then reverted in round 9 — `cfg/tests/cfg_query.rs` constructs `Cfg` via struct-literal syntax").
- **Fix:** Move V6 from "completed" list to "deferred" with the revert rationale.

### strider-pattern-author — NEEDS-UPDATE (minor)
- **Step 3:** References `pattern.rs::intern_capture`. Function is `intern_str`.
- **Fix:** Rename.

### strider-fingerprint-audit — KEEP-AS-IS
All citations verified post-wave-31 line-number refresh: `store.rs:132-184`, `validate/mod.rs:83`, `layer_c.rs:184`, exempt kinds list match.

---

## New Skill Proposals

### Proposal 1: `strider-wide-const-author`

**Gap:** No skill covers authoring patterns or IR nodes that use `IntConstWide(WideConstId)` for U256/U512 values. CLAUDE.md notes `Graph::wide_consts` interning and that `IntConst(u128)` rejects wide values. Patterns over wide constants require different constructors than `int_const(n)`.

**Trigger phrases:** "add a U256 constant", "match a 512-bit mask", "`IntConst(u128)` panics on my constant", "wide-const interning".

**Procedure:** Read `Graph::wide_consts` interning API; use `IntConstWide(WideConstId)` for IR construction; use `validate`'s Layer-C `check_layer_c_wide_consts` check; ensure fingerprint propagation; test with `U256`/`U512`-typed nodes.

**Verification:** `cargo test --package ir wide_const`, `validate_with_options(check_asm_fingerprints: true)` on a graph with wide constants.

### Proposal 2: `strider-asm-fingerprint-design-sync`

**Gap:** The asm-fingerprint contract has a spec doc (`docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md`) and a per-pass plan, but no skill covers the workflow for updating the per-pass plan after adding a new pass or rewrite. Currently this is scattered across `strider-fingerprint-audit`, `strider-opt-pass-author`, `strider-flagcmp-rule-author`, `strider-rewrite-rule-multinode-audit`.

**Trigger phrases:** "update the asm-fingerprint design doc", "new exempt kind for the validator", "the per-pass fingerprint plan is stale".

**Procedure:** Read the spec, cross-reference every exempt kind against `layer_c.rs::asm_fingerprint_exempt`, update the per-pass table, run `validate_with_options(check_asm_fingerprints: true)` on the fixture matrix.

**Verification:** `cargo test --package ir asm_fingerprint`, full fixture run with Layer-C check enabled.

---

## Counts

| Category | Count |
|---|---|
| KEEP-AS-IS | 7 |
| NEEDS-UPDATE | 12 |
| OBSOLETE | 0 |
| New skill proposals | 2 |

**KEEP-AS-IS (7):** `strider-callother-abi`, `strider-target-arch`, `strider-silent-failure-audit`, `strider-builder-for-arch-migration`, `strider-rewrite-rule-multinode-audit`, `strider-fingerprint-audit`, `strider-indirect-shape-author`.

**NEEDS-UPDATE (12):** `strider-cli-runner`, `strider-debug-pattern`, `strider-fixture-author`, `strider-flagcmp-rule-author`, `strider-orchestrator-extend`, `strider-py-binding`, `strider-validation-invariant-extend`, `strider-opt-pass-author`, `strider-cc-preset-extend`, `strider-doc-line-number-refresh`, `strider-public-api-encapsulation`, `strider-pattern-author`.

**Single biggest concentration of stale references:** `strider-cc-preset-extend` (10 wrong line numbers).

**Lowest-risk fix:** `strider-debug-pattern` step-2 pass list (one missing pass name).

**Most awkward finding:** `strider-doc-line-number-refresh` cites an outdated value as its motivating example — meta-doc inconsistency.
