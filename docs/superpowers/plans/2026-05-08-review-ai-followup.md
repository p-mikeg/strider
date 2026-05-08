# review/ai follow-up — finish all outstanding non-refuted items

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:test-driven-development for every code change. Bug fixes get a failing test first; refactors keep all existing tests green. **Iterative-only:** do not introduce new recursion (user explicitly prefers iterative). When refactoring an already-recursive function, take the opportunity to convert.

**Goal:** Land every audit finding from `reviews/round7-*.md` that was not REFUTED in the verification round, on top of the 22 commits already on `review/ai`.

**Architecture:** Six work groups, each one or two commits per fix, TDD-style.

---

## Phase 0 — verification gate

- [ ] Confirm we're on `review/ai` and clean.
- [ ] Baseline test suite: `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `pytest crates/strider-py/tests/python/ --ignore=tests/python/test_arm64_kernel_lift_bugs.py` all green.

---

## Group G — Correctness / silent-failure (10 fixes)

Highest priority. Each is one commit with a failing test first.

### G1 — `PyPat::when()` predicate exception propagation
**Where:** `crates/strider-py/src/pattern.rs:370-396`
**Bug:** Predicate exception printed-and-swallowed via `e.print(py); false`; non-bool return → `unwrap_or(false)`.
**Fix:** Surface predicate failures via `eprintln!` (mirroring C3) so the silent fail-on-error becomes visible.

### G2 — ELF malformed symbol index bucketed as weak-extern
**Where:** `crates/reader/src/elf.rs:507-590`
**Bug:** Malformed-ELF symbol/section index errors hit the same bucket as legitimate weak-extern.
**Fix:** Distinguish the two cases — return a separate `RelocationStats` count or surface a typed error.

### G3 — `elf.rs:729` `unwrap_or(true)` conflates SHT_NOBITS
**Where:** `crates/reader/src/elf.rs:729`
**Bug:** `sec.data().map(...).unwrap_or(true)` conflates a section-parse error with a legitimate SHT_NOBITS section.
**Fix:** Use explicit `match` that distinguishes the two; return a typed error on parse failure.

### G4 — `Kb::from_const` U128/U256 collapse
**Where:** `crates/opt/src/known_bits/mod.rs:38-46`
**Bug:** Three `unwrap_or(0)` precondition fallbacks silently collapse U128/U256 inputs to "fully unknown".
**Fix:** Either reject U128/U256 explicitly (return `None`) or document the limitation in the function-level comment.

### G5 — `clear_graph_ptr` silent no-op on poisoned mutex
**Where:** `crates/strider-py/src/pattern.rs:270-274`
**Bug:** Silent no-op on poisoned mutex; `with_graph` then dereferences a stale pointer.
**Fix:** Return an error (or panic with clear message) on poison; document the contract.

### G6 — `PyMemoryMap` ignores `_space` parameter
**Where:** `crates/strider-py/src/reader.rs:567-580`
**Bug:** `read(_space, addr, size)` discards the space tag — REGISTER reads return RAM bytes.
**Fix:** Gate the read on `space == VnSpace::RAM`, return `None` otherwise (mirroring `PyReadOnlyMemoryAdapter::read` at line 507).

### G7 — Python `OptimizerPipeline.from_default()` sync test
**Where:** new test under `crates/strider-py/tests/python/`
**Bug (`round7-opt.md` IMP-2):** Manually-listed default pipeline can silently desync from `opt::default_pipeline()`.
**Fix:** Add a Rust-side test that asserts `opt::default_pipeline().optimizer_count()` matches what Python registers (or expose `optimizer_names()` and assert from Python).

### G8 — `FlagCmpCanonicalize::rhs_thumb_b` fingerprint-superset gap
**Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs` (the `rhs_thumb_b` Rule)
**Bug (`round7-correctness.md` E2):** Returns the captured node `a` directly without `extend_asm_fingerprint_from(a, root)`.
**Fix:** Add the fingerprint extension call before returning.

### G9 — Predicate exception backtrace test
**Where:** `crates/strider-py/tests/python/test_pattern_when.py` (new or existing).
**Bug:** Coverage gap: no test forces `.when()` predicate exception to surface.
**Fix:** Pytest that subclasses with a raising predicate; capture stderr; assert exception text.

### G10 — `Cfg::SpecialTerm::CallIndirect` dead-variant cleanup
**Where:** `crates/cfg/src/cfg/types.rs` — verify it's still absent (was already absent per Round-1A audit).
**Action:** Confirm grep shows no `CallIndirect` SpecialTerm variant; if present, delete; if absent, no-op.

---

## Group H — Pattern crate gaps (1 commit)

### H1 — `mem_phi()` and `value_phi()` constructors
**Where:** `crates/pattern/src/pat/{builders/phi.rs, ctor/control.rs}`
**Bug (`round7-pattern.md` #2):** `phi()` only matches `VarPhi`; `MemPhi` and `ValuePhi` have no constructors.
**Fix:** Add `MemPhiPat` / `ValuePhiPat` builders + `mem_phi()` / `value_phi()` ctors. Failing test that constructs a graph with both kinds and asserts each ctor matches the right one.

---

## Group I — Python parity (1-2 commits)

### I1 — `Graph::node_kind` / `node_outputs` / `node_inputs` introspection
**Where:** `crates/strider-py/src/graph.rs`
**Bug (`round7-py-support.md` M3):** No per-node introspection from Python.
**Fix:** Wrap each accessor. Return `NodeKind` as `str` (or a typed enum class). Add Python tests.

### I2 — `Graph::asm_fingerprint(node_id)` accessor
**Where:** same as I1.
**Fix:** Wrap; return `list[int]`. Test.

### I3 — `Graph::call_other_name(node_id)` accessor
**Where:** same.
**Fix:** Wrap; return `str | None`. Test.

### I4 — `validate_with_options { check_asm_fingerprints }`
**Where:** same; mirror Rust's `ValidateOptions`.
**Fix:** Wrap as `Graph.validate(check_asm_fingerprints: bool = False)`. Test that passes with default; fails when enabled on a non-fingerprinted graph.

### I5 — `Graph::compact()`
**Where:** same.
**Fix:** Wrap. Test that node count drops after detaching.

---

## Group J — Type-design (3 commits)

### J1 — Phantom-typed `OptimizerPipeline<Phase>`
**Where:** `crates/opt/src/pipeline.rs`
**Bug (`round7-types.md` #3):** Adding a destructive pass to a stable pipeline is doc-only enforcement.
**Fix:** Introduce `Phase` zero-sized markers (`Stable`, `Destructive`, `Full`); `add` is gated by phase; compile-time error to mix.

### J2 — `BuiltFunctionGraph::from_graph_and_entry` privacy + `RewriteCtx`
**Where:** `crates/ir/src/function.rs:94-103` + `crates/strider/src/rewrite.rs:130-139`
**Bug (`round7-types.md` #1):** Public ctor with empty `variables`/`call_clobbered`/`ret_val_regs` is a contract leak.
**Fix:** Make `from_graph_and_entry` `pub(crate)`; introduce `pub struct RewriteCtx<'g> { graph: &'g mut Graph, entry: NodeId }` for `pattern::rewrite_rule`. Call sites updated.

### J3 — `iterators.rs` `Index<usize>` documents panic
**Where:** `crates/ir/src/iterators.rs:37-40, 91-97`
**Fix:** Add doc-comments declaring "panics on OOB"; suggest `get(idx)` for fallible access. Do NOT remove the impls — they're caller-convenient.

(`BuiltCallingConvention` field privacy and `set_lift_addr` scope-guard deferred — larger refactors with API ripple.)

---

## Group K — Naming (3 commits)

### K1 — `Rule.cap_a` / `cap_b` → `lhs_capture` / `rhs_capture`
**Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs` (~7 sites in one file)
**Fix:** Mechanical rename; all-internal.

### K2 — Tier-1 / tier-2 comment-only sweep
**Where:** ~80 lines across orchestrator.rs, indirect_resolve/, opt/sp_expr.rs, etc.
**Fix:** Replace "tier 1" with "cfg-time mini-graph resolver"; "tier 2" with "IR-level indirect-branch resolver"; "tier-2 fixed-point loop" with "indirect-resolution fixed-point loop". No code changes.

### K3 — File + test-fn renames
**Where:**
- `tests/tier2_orchestrator.rs` → `tests/orchestrator_indirect_resolution.rs`
- `tests/tier2_optimizer_tiers.rs` → `tests/optimizer_pipeline_subsets.rs`
- 5 test fn renames `tier_1_*` / `tier_2_*` → cfg-time / ir-level prefixes
- `SpecialTerm::Unresolved` → `SpecialTerm::PendingIndirect` (cfg-internal enum, 4 sites)

---

## Group L — Logic-flow folds (2-3 commits)

### L1 — `is_addr_tail_call` half-open form (F-1)
**Where:** `crates/cfg/src/cfg/query.rs:25-45`
**Fix:** Replace bifurcated branch with `(target < lower) || (target >= upper)` + sentinel for unbounded. Existing tests assert behaviour.

### L2 — `RedundantPhis` VarPhi / ControlState collapse (F-12)
**Where:** `crates/opt/src/redundant_phis/mod.rs:32-162`
**Fix:** Extract `unique_reachable_ctrl` helper consolidating the iterator-singularity dance.

### L3 — `apply_elf_relocations_with_extender` + `locate_and_write` (F-7 + F-15)
**Where:** `crates/reader/src/elf.rs`
**Fix:** Collapse autoload + non-autoload variants behind a closure; extract repeated find-region/write block.

### L4 — `first_output_matching` helper (F-17)
**Where:** `crates/ir/src/region.rs:294-351`
**Fix:** Extract helper for `region_entry_control` / `region_entry_memory` (differ only by predicate).

### L5 — Collapse `is_branch_tail_call` / `_nocheck` (F-14)
**Where:** `crates/cfg/src/cfg/builder/region_builder.rs:215-243`
**Fix:** Inline the cheap insn_index check at the 2 call sites that need it; drop the variant pair.

---

## Group M — Generalization (2 commits)

### M1 — `Graph::create_node_attributed(kind, inputs, output_kinds, contributors)` helper
**Where:** `crates/ir/src/graph/store.rs` and ~15 opt-pass sites
**Fix:** Add helper that creates the node AND extends fingerprints from each contributor in one call. Callers replace `create_node + extend_asm_fingerprint_from` pairs.

### M2 — PyO3 error-converter macro (Cat. 7)
**Where:** `crates/strider-py/src/errors.rs`
**Fix:** Replace 5 nearly-identical converters with a single macro or generic `into_typed_err<E>(err)`.

---

## Group N — Stale comments (1 commit)

Mechanical doc-only sweep:
- `cfg/types.rs:103-105` — drop "legacy mapping retained until indirect-branch resolver lands" (resolver landed).
- `strider/pipeline.rs:201-210` — doc lists 5 passes, code composes 7 (add `FlagCmpCanonicalize` + `IfCondInversion`).
- `Graph::asm_fingerprints` exempt-set doc at `graph/mod.rs:96` — drop phantom `IfCase` from the list (already done? verify).
- `cfg/builder/region_builder.rs:435` — drop "tier-2 feedback shape" reference.
- Any other "TODO(Task17)" without an active issue link — leave (still tracked).

---

## Group O — Tests (P2/P3/P4 from `round7-test-plan.md`)

Each test is one commit (or batched if related).

### O1 — Asm-fingerprint dedup-cache UNION on cache hit (P2.1)
### O2 — Asm-fingerprint shrink-prevention pipeline test (P2.2)
### O3 — vn_io sub-register partial-write with phi-live parent (P2.3)
### O4 — `int_const_any_of([])` vacuous-fail (P2.4) — already covered? verify.
### O5 — Python typed-error actually-raised tests (P2.6)
### O6 — AArch64 e2e lift produces valid IR (P2.7)
### O7 — `phi()` / `mem_phi()` / `value_phi()` matches the right kind (P2.8 — combined with H1)
### O8 — Pattern alias round-trip (P3.1) — for sub, int_le, int_sle, float_sub, float_ne, float_le.
### O9 — Stack-array indirect-branch shape end-to-end (P3.2)
### O10 — `StackLoadForward + StackStoreDetect` convergence ≤ 2 iters (P3.3)
### O11 — `OptimizerPipeline` idempotency (P3.4)

P4 benchmarks (in `crates/strider/benches/scaling.rs`):
### O12 — chain-of-N-stores
### O13 — diamond CFG of N regions
### O14 — wide jump-table N targets
### O15 — `find_all_requirements` shared-capture join

---

## Group P — Documentation

### P1 — Root `README.md` Python-focused rewrite
- Trim Rust-API sections (lines ~193-416).
- Add bounded-lift / `function_max_size`.
- Add `Match.asm_fingerprint(c)` from Python.
- Add `find_all_requirements` examples beyond the field-offset one.
- Add troubleshooting "why didn't my pattern match?".

### P2 — 12 per-crate READMEs
Each crate gets ~100-200 LOC: purpose, public surface, internal architecture, key invariants, tests, gotchas. Crates: `cfg`, `dot`, `entity-utils`, `graphwalk`, `ir`, `opt`, `pattern`, `pcode-lift`, `reader`, `strider`, `target`. (graphmock removed.)

### P3 — 8 skills authored
Per `round7-skills.md` design:
- `strider-pattern-author`
- `strider-debug-pattern`
- `strider-opt-pass-author`
- `strider-fingerprint-audit`
- `strider-indirect-shape-author`
- `strider-callother-abi`
- `strider-target-arch`
- `strider-py-binding`

Install location: `crates/strider/.claude/skills/<name>/SKILL.md`.

---

## Verification (per group)

After each group's commits:
- [ ] `cargo build --workspace`.
- [ ] `cargo test --workspace`.
- [ ] `cargo clippy --workspace -- -D warnings`.
- [ ] For Python-touching changes: `cd crates/strider-py && uv run maturin develop --release && uv run pytest tests/python/ --ignore=tests/python/test_arm64_kernel_lift_bugs.py`.

## Final verification

- [ ] `git log --oneline review/ai ^feature/ai` — every fix is its own commit.
- [ ] Final summary: implemented vs. deferred (none expected this round).
