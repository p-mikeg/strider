# Round 9 — Skill Audit

**Branch:** `feature/ai`. Audited 14 existing `SKILL.md` files under
`crates/strider/.claude/skills/` and cross-referenced against findings from
Round-9 reviews 1A-1F, 2A-2D, 3A, 3B, EA1-EA3, and the Ask-8 correctness
sub-passes (R1-R5).

## Part A — Existing skill audit

Per-skill verdict, accuracy of description, line-number staleness, and trigger
fitness. R9-3A flagged stale line numbers in two skills; this audit confirms
both still drift relative to current code.

### 1. `strider-pattern-author` — KEEP (minor UPDATE)
- **Description accurate?** Yes. Procedure aligned with current `pat::ctor` /
  `Matcher` surface.
- **Triggers fit?** Yes — covers Rust + Python authoring.
- **Line numbers:** No specific line numbers cited; the file paths (`crates/pattern/src/call.rs`, `crates/pattern/src/capture.rs`, `crates/pattern/src/matcher.rs`) need confirmation: the actual layout is `crates/pattern/src/pat/ctor/` and `crates/pattern/src/matcher/`. R9-3A note 70 already flagged this.
- **Update:** Replace the path roots `crates/pattern/src/{call,capture,matcher}.rs` → `crates/pattern/src/pat/ctor/{call,…}.rs`, `crates/pattern/src/var.rs` (Capture lives there per R9-2D L8), `crates/pattern/src/matcher/mod.rs`. Add a pitfall: `IntCmpOp::LessEqual` / `SlessEqual` / `Borrow` and `FloatCmpOp::NotEqual` / `LessEqual` are non-existent (R9-1F-01 caught this in Python tests).

### 2. `strider-opt-pass-author` — UPDATE
- **Description accurate?** Mostly. Misses the EA1-Finding 1 issue.
- **Line numbers:** `crates/ir/src/graph/store.rs:160` is **stale** (R9-3A Issue E). Actual `extend_asm_fingerprint_from` is at line 184; the comparable `:108-160` block in the fingerprint-audit skill points at the wrong functions. R9-1A also cites these accessors near `crates/ir/src/graph/store.rs:127-200`.
- **Critical missing guidance:** Round-9 EA1 Finding 1 found `pattern::rewrite_rule` only attributes the **outermost** RHS node — multi-node rewrites (e.g. ConstantFold's `(a&C1)|(b&C2)) & C3 → (a&(C1&C3))|(b&(C2&C3))`) leave fresh inner nodes with empty fingerprints. The skill currently says "rewrite_rule is fine for single-output replacement"; that is too permissive for multi-node RHS shapes. Add a step: "If the RHS has any **fresh** intermediate nodes, do NOT use `rewrite_rule`. Construct nodes manually with `create_node_attributed` (or call `extend_asm_fingerprint_from` per intermediate). The `flag_cmp_canonicalize` skill is the canonical example."
- **Update:** Bump line numbers to `:184`, add the multi-node-RHS warning, cross-reference `validate_with_options(check_asm_fingerprints: true)` as the regression detector for this class of bug.

### 3. `strider-fingerprint-audit` — UPDATE
- **Description accurate?** Yes; the contract description matches `extend_asm_fingerprint_from` in `crates/ir/src/graph/store.rs`.
- **Line numbers:** **STALE** (R9-3A Issue D). Cites `:108-160`; actual range covering `asm_fingerprint` (line 132) through `extend_asm_fingerprint_from` (line 184) is approximately `:127-200`.
- **Procedure missing:** Should explicitly call out the EA1-Finding 1 multi-node-RHS pattern as the highest-likelihood violation source after a new pass lands. Currently steps 3-4 say "for each newly-created node confirm a matching `extend_asm_fingerprint_from`", but doesn't say "audit every intermediate of a multi-node `rewrite_rule` body — `rewrite_rule` only attributes the root."
- **Triggers fit?** Yes.
- **Update:** Update the `:108-160` reference, add the rewrite_rule-multi-node trap to step 6 / Pitfalls, and cite EA1-Finding 1 as the worked example.

### 4. `strider-indirect-shape-author` — KEEP
- **Description accurate?** Yes. Covers tier-1 vs tier-2 split, classifier arms, in-place rewrite, orchestrator `Decision` wiring, fingerprint propagation, MAX_TABLE_ENTRIES cap.
- **Line numbers:** None cited at function granularity; risk low.
- **Triggers fit?** Yes.
- **Note:** R9-2B I-5 wants `tier-1`/`tier-2` doc-prose normalised to `cfg-time resolver` / `IR-level resolver`. The skill itself uses both interchangeably; this is consistent with the codebase and doesn't need editing yet.

### 5. `strider-callother-abi` — KEEP
- **Description accurate?** Yes; matches the current two-function `classify_arch_specific` / `classify_arch_independent` split in `crates/target/src/call_other_abi.rs`.
- **Line numbers:** Cites `:12` for `CallOtherAbi`; current layout still has the struct definition near the top of the file, low staleness risk.
- **Triggers fit?** Yes — `UnknownCallOtherError` is the canonical entry trigger.
- **Note:** EA3-CRITICAL-1 (`sysret` misclassified as `NoReturn`) is the kind of mistake this skill should *prevent* but doesn't explicitly call out. The "decide the class" step could add: "If the user-op resumes execution at a different ring/IP (sysret, sysexit, iret), it is NOT `NoReturn` — it returns to the saved IP and should be `Call(...)` or `PURE_WITH_MEM_EDGE`." Not a blocker; leave for opportunistic update.

### 6. `strider-target-arch` — KEEP
- **Description accurate?** Yes.
- **Triggers fit?** Yes.
- **Line numbers:** Approximate (`aarch64_aapcs64 line 249`, `arm_aapcs line 289`, …). The CC-preset skill `strider-cc-preset-extend` carries more recent line numbers for the same presets (~line 359 / 399 / 440 / 460). Both can drift.
- **Note:** No update needed; the skill correctly defers CC-only work to `strider-cc-preset-extend`.

### 7. `strider-py-binding` — KEEP
- **Description accurate?** Yes; module list (`arch.rs`, `cc.rs`, etc.) matches `crates/strider-py/src/`.
- **Triggers fit?** Yes.
- **Note:** R9-1F-03 (Graph.optimize silent no-op on second call) and R9-1F-04 (KeyboardInterrupt swallowed) are both Python-binding pitfalls but neither is severe enough to block the skill. Could add a pitfall row pointing at "do not swallow `PyKeyboardInterrupt` / `PySystemExit` from user callbacks" but the skill already says "never `panic!`; use typed exceptions" which covers the intent.

### 8. `strider-fixture-author` — KEEP
- **Description accurate?** Yes; matches `fixtures/Makefile`, `per_arch_test!`, `lift_fixture` driver.
- **Line numbers:** Cites `crates/strider/tests/common/mod.rs:410` for the macro and `:140` for `strider_for(arch)`. Spot-checked R9-1E §Simplification candidates that mention `:140` + `:176-182`; line range is reasonable.
- **Triggers fit?** Yes.

### 9. `strider-cc-preset-extend` — KEEP
- **Description accurate?** Yes; the LR-as-callee-saved tradeoff narrative matches CLAUDE.md and the round-8/round-9 verification in R9-1E.
- **Triggers fit?** Yes.
- **Note:** The skill explicitly lists `x86_64_systemv` as canonical and warns against `x86_64_systemv_abi`; this is consistent with R9-3A Issue B.

### 10. `strider-orchestrator-extend` — KEEP
- **Description accurate?** Yes; the `Builder::for_arch` foot-gun + `Decision` exhaustiveness + `RegionIndex` invariant story is current.
- **Line numbers:** Cites `orchestrator.rs:837` (CFG construction) and `tests/common/mod.rs:220` / `benches/scaling.rs:93`. R9-1B Finding 3 confirms test files at these locations are still on `for_arch` post-round-8.
- **Note:** R9-1E LOW finding (`LoopState::sleigh` field doc says `Builder::with_endianness`) is a code-side stale comment, not a skill problem.

### 11. `strider-debug-pattern` — KEEP
- **Description accurate?** Yes.
- **Line numbers:** Mentions `crates/ir/src/graph/store.rs` for `asm_fingerprint` API but no specific line — low staleness risk.
- **Triggers fit?** Yes.

### 12. `strider-validation-invariant-extend` — KEEP
- **Description accurate?** Yes; layer A/B/C split, reachability scoping rationale, and opt-in `ValidateOptions` flow are correct.
- **Line numbers:** Cites `layer_c.rs:228-253` and `:222-227` for `check_layer_c_function_arg_uniqueness`. Spot-check matches the round-8 fix description.
- **Note:** R9-Ask8-R2 Finding 2 (Layer-C ControlState non-empty path not reachability-gated) is exactly the class of bug this skill exists to prevent. The skill could mention it as a worked example, but it's not strictly necessary.

### 13. `strider-flagcmp-rule-author` — KEEP
- **Description accurate?** Yes.
- **Line numbers:** Cites `mod.rs:310` for `build_rules`, `:150-153` rough region — the round-9 R9-1C Issue 2 (`replace_all_uses` return value discarded at `:150-153`) confirms this region is still correct.
- **Triggers fit?** Yes.

### 14. `strider-cli-runner` — KEEP
- **Description accurate?** Yes; the example reads `fixtures/out/x86/arithmetic.elf::add` per `crates/strider/examples/strider.rs:14-15`.
- **Triggers fit?** Yes.

### Summary table

| Skill | Verdict | Reason |
|-------|---------|--------|
| pattern-author | UPDATE | Path roots `crates/pattern/src/{call,capture,matcher}.rs` are stale; actual is `crates/pattern/src/pat/ctor/…` and `crates/pattern/src/var.rs` |
| opt-pass-author | UPDATE | Line `:160` → `:184`; add multi-node-rewrite_rule fingerprint trap (EA1 Finding 1) |
| fingerprint-audit | UPDATE | Line range `:108-160` → `:127-200`; add multi-node rewrite_rule worked example |
| indirect-shape-author | KEEP | Accurate |
| callother-abi | KEEP | Accurate |
| target-arch | KEEP | Accurate |
| py-binding | KEEP | Accurate |
| fixture-author | KEEP | Accurate |
| cc-preset-extend | KEEP | Accurate |
| orchestrator-extend | KEEP | Accurate |
| debug-pattern | KEEP | Accurate |
| validation-invariant-extend | KEEP | Accurate |
| flagcmp-rule-author | KEEP | Accurate |
| cli-runner | KEEP | Accurate |

No DELETE candidates. The 14 existing skills are all well-scoped and broadly current.

## Part B — Proposed new skills

The Round-9 reviews surface five recurring failure modes that no current
skill front-loads guidance for. Each proposal below is concrete (files,
verification steps, exit criteria) and tied to a documented round-9 finding
that justifies its existence.

### Proposed skill 1: `strider-rewrite-rule-multinode-audit`

- **When-to-invoke:** "I added a `pattern::rewrite_rule` rule to ConstantFold / KnownBits / etc.", "the rewrite RHS has more than one fresh node", "validate Layer-C `check_asm_fingerprints` is failing on a non-exempt intermediate node after my new rule", "I'm porting a hand-built rewrite to `rewrite_rule`."
- **Files:** `crates/pattern/src/rewrite.rs` (mechanism), `crates/opt/src/constant_fold/rules.rs` (canonical multi-node rules), `crates/opt/src/flag_cmp_canonicalize/mod.rs` (manual-construction template), `crates/ir/src/graph/store.rs:127-200` (fingerprint API), `crates/ir/src/validate/mod.rs` (`validate_with_options`).
- **Procedure:**
  1. Identify the RHS shape. If it is a single `NodeKind` with no fresh subtree, `rewrite_rule` is fine. If it has any fresh intermediate node (a new `IntConst`, a new `Add`, etc. that did not exist on the LHS), STOP — `rewrite_rule` will only attribute the outermost.
  2. Inspect the rewrite_rule body in `crates/pattern/src/rewrite.rs:91-92`: the `extend_asm_fingerprint_from(new_node, root_node)` call fires once for the root. Confirm by reading the source.
  3. Switch to manual construction: build each RHS node with `create_node_attributed` or call `extend_asm_fingerprint_from(intermediate, root)` per intermediate. Mirror `flag_cmp_canonicalize::build_int_cmp` / `build_bool_neg` (`crates/opt/src/flag_cmp_canonicalize/mod.rs`).
  4. Add a regression test: build a mock LHS where every leaf has a known `asm_fingerprint`, run the pass, then call `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` and assert no `MissingAsmFingerprint` error is reported.
- **Verification:**
  - `cargo test --package opt <pass>::tests::asm_fingerprint_multinode_rhs`.
  - `cargo test --package opt validate_with_options`.
  - `cargo clippy --workspace -- -D warnings`.
- **Exit criteria:** Layer-C `check_asm_fingerprints` passes on the rewritten graph; every fresh RHS intermediate has a non-empty fingerprint; the regression test fails before the fix and passes after.
- **Justification:** R9-EA1 Finding 1 (HIGH) — multi-node ConstantFold rules silently violate the superset contract today.

### Proposed skill 2: `strider-builder-for-arch-migration`

- **When-to-invoke:** "I'm writing a new test that builds a Cfg directly", "I see `Builder::with_endianness` in a test file", "lifting non-x86_64 fails with `UnknownCallOtherError`", "an arch-specific user-op (`swi`, `CallHyperVisor`, MIPS `syscall`) is misclassified", "PR review found a `Builder::new` or `with_endianness` call in a test or bench."
- **Files:** `crates/cfg/src/cfg/builder/mod.rs` (`Builder::for_arch` vs `Builder::with_endianness` vs `Builder::new`), `crates/strider/src/orchestrator.rs:837`, `crates/strider/tests/common/mod.rs:220`, `crates/strider/benches/scaling.rs:93`, **and** the failure sites: `crates/strider/tests/indirect_branch.rs:91` (R9-Ask8-R5 Critical 1), `crates/cfg/tests/known_targets.rs:30,71,104,143,158,203` (R9-Ask8-R5 Important I-2), `crates/cfg/tests/indirect_dispatch.rs:159` (R9-1B Finding 3).
- **Procedure:**
  1. `grep -rn "Builder::with_endianness\|Builder::new" crates/*/{src,tests,benches,examples}` — every hit is a candidate.
  2. For each hit, decide: **does this test/bench need cross-arch correctness?** If it lifts non-x86_64 bytes or uses a non-x86_64 Sleigh, migrate.
  3. Replacement form: `cfg::Builder::for_arch(&target::SleighArch::<arch>(), sleigh, addr, opts).build()`. Pull the `SleighArch` from a parameter or `target` import; do not synthesise.
  4. Production code in `orchestrator.rs:837` is already on `for_arch` — confirm it stays that way; never regress.
  5. If the test is genuinely x86_64-only by construction (uses `jmp rax` byte sequences), leave the call but add a `// x86_64-only: byte sequences are SDM-encoded` comment AND prefer `Builder::for_arch(&target::SleighArch::x86_64(), …)` for consistency.
- **Verification:**
  - `cargo test --package strider tests::indirect_branch::<arch>` for every non-x86_64 arch.
  - `cargo test --package cfg known_targets`.
  - `cargo clippy --workspace -- -D warnings`.
  - Confirm no `UnknownCallOtherError` raised on a previously-passing fixture.
- **Exit criteria:** Zero `Builder::with_endianness` / `Builder::new` calls in any non-x86_64-only path; cross-arch tests still pass; CallOther dispatch reaches the correct preset row.
- **Justification:** R9-1B Finding 3 + R9-Ask8-R5 C-1 + R9-Ask8-R5 I-2 — three distinct test files still have the foot-gun; this is the most-mentioned issue across the round.

### Proposed skill 3: `strider-silent-failure-audit`

- **When-to-invoke:** "I see an `unwrap_or_default` / `.ok()?` / `unwrap_or` / bare `let _ = …` in production code", "this function returns `Option` but the contract is `Result`", "a `try_into().ok()?` swallows a `TryFromIntError` that should propagate", "PR review flagged a silent fallback".
- **Files:** Hot spots from R9-2C: `crates/strider/src/orchestrator.rs:786, 729-731` (size-conversion drop), `crates/strider/src/indirect_resolve/classify.rs:49-57` (KnownBits eprintln-and-skip), `crates/strider-py/src/pattern.rs:459-484` (raw-pointer hazard + KeyboardInterrupt swallow), `crates/reader/src/elf.rs:584` (Section relocation mislabel), `crates/opt/src/known_bits/mod.rs:179, 220` (shift rhs `unwrap_or(u64::MAX)`).
- **Procedure:**
  1. Classify the swallow: **(a) recoverable optional fallback** (poison recovery, validator-enforced invariant, fixed-point classifier give-up) — leave with comment; **(b) silent unsoundness** — propagate via `Result`.
  2. For (b), the canonical fix shape is to change the function signature from `Option<T>` to `Result<Option<T>, anyhow::Error>` (or to a typed error variant), bubble the error up through the calling chain, and let the orchestrator's `anyhow::Result` surface it.
  3. Pattern-match against the R9-2C taxonomy:
     - **TryFromIntError** in size conversions — usually a real bug; propagate.
     - **Mutex poison** — recover via `unwrap_or_else(|p| p.into_inner())` (already done in 4 places, see R9-2C OK table).
     - **Validator-enforced invariant violations** — replace `.ok()?` with `.expect("invariant: …, validator-enforced")`.
     - **Python callback exception** in `wrap_when` — capture the first `PyErr` in a `RefCell<Option<PyErr>>` and re-raise after the walk; don't print-and-discard.
  4. Add a regression test: construct the malformed input, assert the new error type is raised.
- **Verification:**
  - `cargo test --package <crate>` for the touched crate.
  - `cargo clippy --workspace -- -D warnings -W clippy::unwrap_in_result -W clippy::map_err_ignore`.
  - For Python-side fixes: `uv run pytest crates/strider-py/tests/python/test_typed_errors_e2e.py`.
- **Exit criteria:** Every `(b)`-class swallow in the touched module is converted to a typed error path; a test exercises the previously-silent failure; the rest of the silent-failure inventory in R9-2C is unaffected.
- **Justification:** R9-2C HIGH findings #1-#5 + R9-correctness-borrowing ISSUE-1/4 — five HIGH silent-failure sites, plus eight MEDIUMs. No existing skill covers the audit pattern.

### Proposed skill 4: `strider-public-api-encapsulation`

- **When-to-invoke:** "I'm adding a new public struct in a crate", "this `pub` field has a documented invariant that the type can't enforce", "PR review flagged `pub <field>` with a `// must …` comment", "a future bug could violate this invariant if we don't seal the field." Concretely: BuiltFunctionGraph, PcodeInsnAddr, BuiltCallingConventionParts, IndirectBranchResolve setup fields.
- **Files:** R9-2D HIGH targets: `crates/ir/src/function.rs:117` (`from_graph_and_entry_for_rewrite`), `crates/cfg/src/cfg/types.rs:50-55` (`PcodeInsnAddr`), `crates/target/src/calling_convention/mod.rs:127` (`BuiltCallingConventionParts`), `crates/ir/src/function.rs:59-79` (`BuiltFunctionGraph::{call_clobbered, ret_val_regs, …}`), `crates/opt/src/indirect_branch_resolve/mod.rs:112-159` (`IndirectBranchResolve` setup).
- **Procedure:**
  1. Identify the partial-state hazard: which field combinations are mutually-incompatible? Which fields have arity invariants? Which fields are derived/cached? Document each.
  2. Decide between three encapsulation strategies:
     - **Sum type**: replace `(fn_max_size: Option<u64>, allow_code_before_start: bool)` with `enum FunctionBoundary { Unbounded { allow_…: bool }, Bounded { max_size: u64 } }` (R9-2D M3).
     - **Builder + fail-fast validation**: keep the field surface but introduce a `from_parts_validated` constructor that asserts disjointness / arity (R9-2D H3).
     - **Visibility tightening**: change `pub` to `pub(crate)` and expose accessors (`entry()`, `graph()`). Mechanical but touches every external caller; do for `BuiltFunctionGraph::{variables,entry,graph,call_clobbered,…}` and `PcodeInsnAddr` per R9-2D H1+H2+H4.
  3. For deletion candidates (`BuiltFunctionGraph::from_graph_and_entry_for_rewrite`, R9-2D H1): migrate the 5 test sites to `Matcher::for_graph(graph, entry)` first, then delete the constructor. The pattern crate already exposes the right replacement.
  4. Audit the public-API snapshot test in strider-py and update if a Python wrapper changed.
- **Verification:**
  - `cargo test --workspace`.
  - `cargo clippy --workspace -- -D warnings`.
  - `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py`.
  - For deletions: `cargo doc --workspace --no-deps` should not link to the removed item.
- **Exit criteria:** Every `pub` field with a documented invariant either has compiler-enforced encapsulation or carries a `from_parts_validated` validator; the `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` partial-state hazard is removed.
- **Justification:** R9-2D HIGH H1-H5 (5 distinct types with public-fields hazards) — the audit calls these "the most acute round-9 type-design issues" and no existing skill covers the encapsulation pattern.

### Proposed skill 5: `strider-doc-line-number-refresh`

- **When-to-invoke:** "PR review caught a stale line number in a doc-comment", "a SKILL.md cites a line number that no longer points at the documented function", "round-N doc-verify flagged drift", "I just renamed a public function and want to find every doc that cites it."
- **Files:** Round-9 hot spots: `crates/strider/.claude/skills/strider-fingerprint-audit/SKILL.md:24` (cites `:108-160`, actual `:127-200` per R9-3A D), `crates/strider/.claude/skills/strider-opt-pass-author/SKILL.md:30` (cites `:160`, actual `:184` per R9-3A E), CLAUDE.md and crate READMEs (R9-3A A/B/C and R9-3B 1-25). The R9-3B sweep enumerates 25 distinct doc-drift sites.
- **Procedure:**
  1. Run `rg -n '<filename>:<lineno>' crates/*/.claude/skills/ docs/ crates/*/README.md CLAUDE.md README.md` for any function whose body changed or whose location moved.
  2. For each hit, open the cited file and confirm the line still points at the documented entity. If not, update.
  3. Common drift sources: `crates/ir/src/graph/store.rs` (asm_fingerprint API was last shifted in round 7), `crates/target/src/calling_convention/mod.rs` (presets shift as new ones are inserted), `crates/opt/src/flag_cmp_canonicalize/mod.rs:310` (`build_rules` table grows).
  4. Prefer **range** citations (`:127-200`) over **point** citations (`:160`) for skill-level docs since they tolerate small shifts; reserve point citations for tight invariants (e.g. the round-8 line-117 anchor for `from_graph_and_entry_for_rewrite`).
  5. R9-3B catalogues the API-drift class (e.g. `FunctionBuilder::build_call_other` no longer exists; the doc still cites it). When updating a line number, also confirm the cited *symbol name* is current.
- **Verification:**
  - `cargo doc --workspace --no-deps --document-private-items` should produce no `unresolved link` warnings.
  - `rg -n '\b(ValidationFailed|build_call_other|x86_64_systemv_abi|with_endianness)\b' crates/*/src/ crates/*/README.md docs/ CLAUDE.md README.md` — every hit is a stale-symbol candidate.
  - Spot-check one cited line per skill against the actual file.
- **Exit criteria:** Every `:<lineno>` citation in `*.md` resolves to the documented entity; no `unresolved link` rustdoc warnings; the R9-3A and R9-3B drift inventories are zero.
- **Justification:** R9-3A Issues D + E + 21 R9-3B drift sites — line-number maintenance is recurring overhead that a focused skill can systematise.

### Skills considered and dropped

- **`strider-pattern-author`** — already exists.
- **`strider-opt-pass-author`** — already exists.
- **`strider-indirect-shape-author`** — already exists.
- **`strider-callother-abi`** — already exists.
- **`strider-target-arch`** — already exists.
- **`strider-py-binding`** — already exists.
- **`strider-fingerprint-audit`** — already exists; the multi-node-rewrite_rule trap should be folded into it (UPDATE) rather than spun out.
- **`strider-cc-preset-extend`** — already exists.
- **`strider-orchestrator-extend`** — already exists.
- **`strider-debug-pattern`** — already exists.
- **`strider-validation-invariant-extend`** — already exists.

The Ask-8 R3/R4/R5 reviews confirm the existing skill set covers the
correctness, edge-case, and cross-arch surfaces well; the gaps proposed
above are the residual cross-cutting concerns (multi-node rewrite_rule,
Builder::with_endianness foot-gun, silent failure audit, public-API
encapsulation, doc-line-number drift) that none of the 14 existing skills
front-loads.
