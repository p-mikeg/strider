# Strider — Next-Round Code Review Prompt

> **Purpose.** This file is a self-contained prompt the user can paste back into Claude later to drive a fresh, independent code review of the strider workspace.  It assumes the previous round (round 7) has already landed — see `reviews/round7-*.md` for the prior outputs — and that the codebase has continued to evolve since.  This round must rederive its findings from the *current* code, not from prior reviews.

---

## How to use

Paste everything between the `=== BEGIN PROMPT ===` and `=== END PROMPT ===` markers below as a fresh user message.  The agent will then drive the review autonomously, using subagents and skills.  Approve any tool prompts that come up during execution.

---

=== BEGIN PROMPT ===

I want you to do another round of deep code review on the strider workspace at `/mnt/c/Users/mikeg/Documents/strider`.  This is round **8** — round 7's outputs live under `reviews/round7-*.md` and round 7's clearing/finalize plans live under `docs/superpowers/plans/2026-05-08-*.md`.

## Trust model — strict

- **Do NOT read `reviews/round7-*.md` or any earlier-round audit as authoritative input.**  You may at most note that an item was flagged before and re-derive the finding from scratch.  The previous reviews are stale relative to the current branch state — the code has evolved since they were written.
- **Do NOT trust comments, docstrings, CLAUDE.md, or per-crate READMEs as evidence.**  They are inputs to be *verified* against code.
- **Do NOT trust the previous reviews' conclusions.**  Each finding in your final summary must cite a code location (`file:line`) and explain its reasoning from code shape alone.
- Verify all rsleigh-touching claims by reading `../rsleigh/sleigh/src/**` directly — that crate is the upstream authority for pcode opcode behaviour, varnode semantics, and the per-arch SLA / PSPEC files.
- Verify ABI claims against published specs (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PPC ELF v1 / v2) — names them in your finding, but trust the *implementation* of `target::CallingConvention::*` against those specs first.

## Required skills

Invoke each of these via the `Skill` tool **before any major step** — `using-superpowers` enforces this:

- `superpowers:using-superpowers` (always — sets the discipline)
- `superpowers:writing-plans` (when sketching the review plan)
- `superpowers:test-driven-development` (when proposing missing tests)
- `superpowers:systematic-debugging` (when investigating any HIGH-severity finding — never guess)
- `pr-review-toolkit:code-reviewer` (per-crate audits — see Round 1 below)
- `pr-review-toolkit:silent-failure-hunter` (Round 2C)
- `pr-review-toolkit:type-design-analyzer` (Round 2D)
- `pr-review-toolkit:code-simplifier` (Round 5)
- `pr-review-toolkit:comment-analyzer` (Round 3B — stale comment sweep)

If a skill applies even at the 1% level, invoke it.  The skills aren't optional — they encode the discipline that produces independent, code-derived findings.

## Subagent discipline

Spawn fresh subagents in **parallel** for independent rounds.  Each agent must:

1. Receive a self-contained prompt — assume the agent has zero context for this conversation.
2. Read code directly via `Read`/`Grep`/`Glob` — not via inherited memory.
3. Be told explicitly: do NOT read `reviews/round7-*.md` or any earlier-round output.
4. Produce a single Markdown report at `reviews/round8-<topic>.md` with HIGH/MED/LOW findings, each with `file:line` and a concrete fix.
5. Output format per finding:
   ```
   ### <Finding title>
   - **Severity:** HIGH/MED/LOW
   - **Where:** crates/foo/src/bar.rs:42-58
   - **What's wrong:** <evidence from code, not from comments>
   - **Verified against:** <rsleigh path / IR signature / sibling pass / ABI spec>
   - **Fix:** <concrete patch or rewrite plan>
   ```

When you launch multiple subagents for independent work, send them in a **single message with multiple Agent tool uses** so they run concurrently.

## Concrete asks (numbered)

1. **Correctness.**  Graph must faithfully represent the assembly across all edge cases — register aliasing (every width in `vn_mask`), bounded lift (`is_addr_tail_call` half-open semantics), indirect branches (every shape the resolver claims to handle), memory-edge clobbering, phi shapes (VarPhi/MemPhi/StackStorePhi/ValuePhi arity rules), NaN ordering, sign / zero extension, NaN-aware float comparisons, lift-time canonicalisations (`IntSub`, `IntLessEqual`, `IntSlessEqual`, `IntNotEqual`, `FloatSub`, `FloatNotEqual`, `FloatLessEqual`, `FLOAT_NAN`), CallOther ABI classifications.  Verify the asm-fingerprint superset contract holds across every opt pass.
2. **Simplicity.**  Identify modules / functions / passes / structs / API surface to delete or merge without sacrificing correctness.  Look for duplicated patterns across opt passes, redundant wrapper types, dead test-only `pub`, unused features (per `target::CallingConvention` presets, per `SleighArch` presets, per pattern free constructor, per NodeKind variant).
3. **Naming.**  Look for unclear / misleading / half-renamed identifiers anywhere — not just `tier1`/`tier2`.  Verify that meaning of every term in CLAUDE.md / per-crate README matches the actual code.
4. **Unused features.**  Confirm by code inspection that every `pub` item has at least one external consumer (within the workspace OR via Python bindings).  Items unreachable from any consumer are candidates for deletion.
5. **Python binding parity.**  Verify every IR / pattern / opt / target / strider feature accessible from Rust is also reachable from `strider-py`'s Python API, OR is documented as deliberately Rust-only (with rationale).  GIL handling correctness on every callback path.  Typed exception coverage on every fallible Python entry.
6. **Multiple rounds.**  Run **at least 2 independent rounds** of audit on every code area.  The previous review found that 6B caught a HIGH-severity bug (IfCondInversion VarPhi corruption) after 1C, 1E, 2D had all read the same file without spotting it — multi-round, no shared state, fresh subagents is what moves the needle.
7. **Test plan.**  Where coverage is sparse, propose specific tests with file path, scope (unit / integration / property / scale), exact harness/fixture, expected assertions.  Use TDD discipline — failing test FIRST, then fix.
8. **Stale comments.**  Verify every `pub` item's docstring matches the actual code.  Hunt for `TODO`s linked to closed work, references to deleted symbols, half-rename leftovers, comments that describe behaviour the surrounding code doesn't implement.
9. **Production panics.**  No `panic!` / `unwrap()` / `expect()` / `unreachable!()` / `assert!()` in non-test code paths.  Audit every occurrence; if not justified by a by-construction invariant, propose `Result` propagation.  Annotate justified ones with `#[allow(clippy::expect_used)]` and a code comment naming the invariant.
10. **CLAUDE.md / READMEs / doc consistency.**  Verify the root README's claims, every per-crate README's public-surface enumeration, every `pub fn` doc.  Trust ONLY the code; flag every drift.
11. **Skills.**  Skim the existing `crates/strider/.claude/skills/*/SKILL.md` set.  Identify any new skill that would help future contributors (or any existing skill that has decayed against the current code).
12. **Scale.**  Verify behaviour at ~10k IR nodes — recursion-induced stack-overflow risk in any function that walks a memory or control chain without an explicit depth bound and without an iterative form, asymptotic complexity in hot paths (`Matcher::find_all`, `find_all_requirements`, `validate`, `create_node` dedup, opt pipeline iteration, orchestrator fixed-point), memory growth (zombie pollution after the destructive pipeline, side-table bloat), Python GIL hold-time on long-running analyses.
13. **Type design.**  Audit every `pub` struct / enum / trait for: leaky encapsulation (public fields where invariants exist), primitive obsession (`u32` where a newtype would express intent), types that fail to express their invariants, partial-state types (struct with all fields populated to "valid" sentinels rather than `Option<_>` or a sum type for the genuine states).
14. **Silent failures.**  Hunt every `unwrap_or` / `unwrap_or_default` / `if let Ok` followed by ignore-Err / `.ok()?` / `match … _ => return None` swallowing real errors.  Distinguish "intentional fallback for a documented optional path" from "swallowed bug".
15. **Helpers / generalization.**  Identify recurring patterns across the codebase (graph traversal, error-wrapping boilerplate, opt-pass scaffolding, side-table mutations) and propose helpers / newtypes / extension traits that consolidate them.  But do NOT force-fit — if a candidate generalisation has 3 sites with subtly different semantics, document the differences and recommend skip.

## Recommended round structure

### Round 0 — orient
Read CLAUDE.md, the workspace `Cargo.toml`, the per-crate `Cargo.toml` files, every per-crate README.  Build a mental model of what's in each crate.  Run `cargo build --workspace --all-targets` and `cargo clippy --workspace --all-targets --no-deps -- -D warnings` and `cargo test --workspace` to baseline the current state.  Note the warning / failure count (must be 0 for both build and clippy; tests must all pass).  Run `cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/ --ignore=tests/python/test_arm64_kernel_lift_bugs.py -q` for the Python side.

### Round 1 — deep per-crate audit (parallel; 6 subagents)
Six `feature-dev:code-reviewer` agents, one per crate group:

| # | Crates | Special focus |
|---|--------|---------------|
| 1A | `ir` | Graph dedup correctness; asm-fingerprint contract (superset + union on cache hits); validate Layer A/B/C reachability scoping; `FunctionBuilder::lift_at` / `LiftAddrGuard` invariants; `node_signature` panic sites; type-design of `BuiltFunctionGraph` (Deref to Graph; `from_graph_and_entry_for_rewrite`'s contract) |
| 1B | `pcode-lift` + `cfg` | `vn_io` register-aliasing for every supported width (1/2/4/8/10/16; check whether 32/64 should be added); sub-register partial-write semantics; CondBranch single-OOB-successor edge cases; bounded-lift `is_addr_tail_call`; CallOther classification dispatch |
| 1C | `opt` | Each pass: rewrite + no-op + idempotency + ordering interactions; FlagCmpCanonicalize correctness against AArch64 cmp encoding; IfCondInversion canonicalisation invariant including phi-value swap; KnownBits soundness; StackStoreDetect / StackLoadForward partial-overlap semantics with endianness; LoadReadOnly bounds; RedundantPhis + DeadBranchElimination interaction with detached zombies; iterative vs recursive memory-chain walks (any function that walks `MemPhi` predecessors needs an iterative form for 10k-store binaries) |
| 1D | `pattern` | Commutativity tables; capture binding agreement across patterns; `find_all_requirements` cross-product correctness + early-exit; `*_any` set-membership empty-set vacuous failure; `.when()` predicate scope (`&Graph` not `&BuiltFunctionGraph`); lift-time canonicalisation aliases match what IR actually emits; `RewriteCtx<'g>` newtype contract; `Matcher::for_graph` vs `Matcher::new` API surface |
| 1E | `strider` + `target` + `reader` | Orchestrator fixed-point convergence + monotonicity; `Decision { FixedPoint, StableOnly, Rebuild }` semantics; stall-budget reset across Rebuild; GraphRewriter re_optimize; target CallingConvention varnode resolution per arch (verify register names exist in the corresponding sleigh spec); `apply_elf_relocations_autoload` correctness on partial regions; `BuiltCallingConvention` accessor coverage |
| 1F | `strider-py` + `dot` + `graphwalk` + `entity-utils` | PyO3 boundary error mapping (every Rust error → typed Python exception, no panic-to-abort); GIL release in `strider.run` for the pure-Rust path; callback-reader re-acquisition; str-keyed capture interning correctness across borrows; unsafe blocks (PyO3 `set_var`, `*const Graph` in PartialMatch); Python tests calling unsupported features; graphwalk traversal termination; entity-utils `EntitySet::insert` returns `bool`; `DenseEntitySet` migration completeness |

### Round 2 — cross-cutting passes (parallel; 4 subagents)
- **2A — production-panic hunt.**  `feature-dev:code-reviewer`.  Walk every `unwrap()` / `expect()` / `panic!()` / `unreachable!()` / `assert!()` outside `#[cfg(test)]` / `tests/` / `examples/` / `benches/`.  For each: justified by a by-construction invariant or unjustified?  If unjustified, propose error variant + propagation path.
- **2B — naming sweep.**  List every occurrence of unclear / misleading / half-rename leftover identifiers.  Look for: `tier`, `_v1`/`_v2`, `old_`/`legacy_`/`tmp_`, `Var` vs `Capture`, leftover references to deleted symbols, file names that don't match what they contain.  Propose a concrete rename mapping.
- **2C — silent-failure hunt.**  `pr-review-toolkit:silent-failure-hunter`.  Look for `unwrap_or`, `unwrap_or_default`, `if let Ok` followed by ignore-Err, `.ok()` discarding errors, `match … _ => return …` swallowing.  Pay special attention to: relocation autoload, CFG decoder cache, indirect-branch resolver giving up on classifier mismatch, PyO3 conversions returning default values, opt-pass classifier `.ok()?` discards.
- **2D — type-design analyser.**  `pr-review-toolkit:type-design-analyzer`.  Audit `pattern::{Pat, Capture, Match, RewriteCtx, Matcher}`, `ir::{Graph, BuiltFunctionGraph, NodeOutputKind, NodeKind, FunctionBuilder, LiftAddrGuard}`, `target::{CallingConvention, BuiltCallingConvention, BuiltCallingConventionParts}`, `strider::{RunConfig, AnalyzeOutcome, Strider}`, `opt::{OptimizerPipeline, Optimizer, OptimizerOnBuilt}`.  Flag: leaky encapsulation, primitive obsession, types that fail to express invariants, partial-state types.

### Round 3 — verification + comments (parallel; 2 subagents)
- **3A — trust-only-the-code verification.**  Sample ≥ 25 specific claims from CLAUDE.md and per-crate READMEs.  For each, find the code and confirm or refute purely from code shape.  Emit a CLAUDE.md correctness diff and per-crate README diff.
- **3B — stale comment sweep.**  `pr-review-toolkit:comment-analyzer`.  Flag every comment block that names a deleted symbol, has a `TODO(TaskNN)` whose task is closed (`git log` + the existing plans dir), or describes behaviour that doesn't match the surrounding code.  Especially look at `pub fn` / `pub struct` doc-strings — those are user-facing.

### Round 4 — test-gap analysis
`feature-dev:code-architect`.  Consume Round 1 outputs.  Emit `reviews/round8-test-plan.md` listing each missing test with: scope, file path, harness/fixture, expected assertions, estimated effort.  Use TDD discipline — failing test FIRST.  Required gaps to investigate (not exhaustive):

- Asm-fingerprint dedup-union: a node created twice from different machine addrs must merge fingerprints.
- Asm-fingerprint shrink-prevention: invariant test that no pass produces a node whose fingerprint is a strict subset of any contributor's.
- vn_io sub-register partial-write where parent is phi-live.
- cfg `RegionBuilder::build` bounded-lift terminator on OOB cur_addr.
- region_builder.rs single-instruction-CondBranch-with-one-OOB-successor edge case (round 7's B1 fix should hold; verify).
- Stack-array dispatch resolver shape end-to-end.
- pattern `find_all_requirements` shared-capture-disagreement filtering.
- pattern `int_const_any_of([])` / `at_any([])` / `offset_any([])` empty-set vacuous-failure tests.
- Multi-output `Match::output(c)` selection rules (which output binds when the matched node has 2+ value outputs?).
- ARM/AArch64/MIPS end-to-end ELF fixture coverage.
- CallOther ABI dispatch — coverage matrix per arch + per opcode.
- PyO3 every typed exception (`StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`, `UnresolvedIndirectBranchError`, `UnknownCallOtherError`) raised by an end-to-end Python test.
- Iterative-form regression for any recursive memory-chain walk: chain of 1k+ stores must not stack-overflow.

### Round 5 — consolidation + simplification
`pr-review-toolkit:code-simplifier`.  Consume Rounds 1–3.  Emit `reviews/round8-simplifications.md` with:

- Modules / functions / structs / passes / CC presets / SleighArch presets to delete and why.
- Helper consolidation candidates (duplicated traversal, error wrapping, fixture builders).
- API shrinkage (visibility tightening from `pub` to `pub(crate)` where external consumers don't exist; verify each via grep).
- Generalization opportunities — but for each, check actual call sites first; if the abstraction loses load-bearing per-site semantics, document and skip.

### Round 6 — skill audit
Skim `crates/strider/.claude/skills/*/SKILL.md` against the current code.  For each skill: does its procedure still apply?  Does it reference deleted symbols?  Are there gaps where a new skill would help?  Propose new skills or revisions.  Don't author yet — just design.

### Round 7 — final consolidation
A single `claude-md-management:revise-claude-md`-style synthesis that integrates every prior-round output into:

1. `reviews/round8-summary.md` — executive summary with prioritised fix backlog (HIGH → LOW), grouped by theme (correctness, simplicity, naming, dead code, panics, tests, docs, py-parity, skills, scale, type-design, silent failures).
2. `reviews/round8-claudemd-diff.md` — concrete CLAUDE.md edits.
3. `reviews/round8-readme-diffs.md` — concrete per-crate README edits.

## Acceptance criteria

- [ ] Every numbered ask (1–15) has a corresponding section in `reviews/round8-summary.md` with concrete actions.
- [ ] Round 8 summary lists every HIGH-severity finding with `file:line` + a proposed fix.
- [ ] CLAUDE.md correctness diff exists with ≥ 25 spot-checked claims.
- [ ] Per-crate README diff lists drift in every crate that has one.
- [ ] Test plan lists ≥ 12 missing tests with exact `file:line` scaffolding.
- [ ] Naming sweep produces a concrete rename mapping (every flagged identifier has a target name).
- [ ] Production-panic audit lists every unjustified `unwrap`/`expect`/`panic!` with proposed `Result` plumbing.
- [ ] Type-design audit produces a list of `pub` struct/enum candidates for visibility tightening or newtype wrapping.
- [ ] Silent-failure audit produces a list of `.ok()?` / `unwrap_or` sites with a propose-or-document decision per site.
- [ ] Skill audit produces a list of revisions or new-skill proposals.
- [ ] `cargo build --workspace`, `cargo clippy --workspace --all-targets --no-deps -- -D warnings`, `cargo test --workspace`, and `pytest tests/python/` all green at the start AND end of the review.
- [ ] No source code is edited during the review — this is a review effort, not implementation.  The output is the set of `reviews/round8-*.md` reports.  Implementation is a follow-up task that the user will explicitly approve.

## Out of scope

- **Editing source code.**  Only documentation under `reviews/round8-*.md` is allowed.
- **Authoring new tests.**  Only the test plan is in scope; writing the actual tests is follow-up.
- **Authoring new skills.**  Only the skill design is in scope.
- **Per-crate README rewrites.**  Only the diff is in scope.

## Critical files to consult

These are the surfaces most worth inspecting directly during the audit:

- IR core: `crates/ir/src/{lib,graph/{mod,store,access},function,validate/{mod,layer_a,layer_b,layer_c},walk,node_signature,builder/{mod,lift_addr,call,nodes,vars}}.rs`
- Lifter: `crates/pcode-lift/src/{lib,vn_io,value_lifter}.rs`
- CFG: `crates/cfg/src/cfg/{builder/{mod,region_builder,indirect_resolve},types,decode_cache,query,options}.rs`
- Optimizer: every `crates/opt/src/<pass>/mod.rs`, plus `pipeline.rs`, `sp_expr.rs`, `worklist.rs`, `indirect_branch_resolve/{mod,jump_table,stack_array,link_register,tail_call,classify}.rs`, `stack_load_forward/mod.rs`, `function_args/mod.rs`
- Pattern: `crates/pattern/src/{lib,rewrite,matcher/{mod,bindings,match_result,walk,walk_through,function_arg_handle},pat/{mod,traits,node_pat,any,guards,builders/*,ctor/*}}.rs`
- Strider: `crates/strider/src/{orchestrator,rewrite,indirect_resolve/{mod,classify,inplace},strider/{mod,pipeline,vn_io,insn/{mod,control}}}.rs`
- Target: `crates/target/src/{arch,call_other_abi,calling_convention/mod}.rs` cross-checked against `../rsleigh/sleigh/src/**`
- PyO3: `crates/strider-py/src/{lib,errors,pattern,graph,opt,reader,arch,cc,run,strider_cls,sleigh,cfg}.rs`

## Verification

The review is itself a research effort — the "verification" is the quality of the final summary, not a build.  The acceptance criteria above are the bar.  At the end, leave a short note in `reviews/round8-summary.md` describing:

- Total HIGH / MED / LOW finding counts.
- How many findings were re-derivations of round-7 items vs. genuinely new.
- Which subagents found load-bearing items the others missed (the multi-round signal).

=== END PROMPT ===

---

## Notes for the user

- The prompt explicitly forbids reading `reviews/round7-*.md` so the next round derives findings independently.  The previous round's outputs stay on disk as historical context.
- The prompt is sized to drive ~3 hours of subagent work plus ~1 hour of consolidation.  Approve tool prompts as they appear; don't context-switch the agent mid-run unless something is genuinely going wrong.
- After the review lands, you'll have the `reviews/round8-*.md` set — at that point a follow-up prompt of the form "land everything not refuted from round 8" runs the same play we ran for round 7.
