# Strider — Next-Round Code Review Prompt

> **Purpose.** This file is a self-contained prompt the user can paste back into Claude later to drive a fresh, independent code review of the strider workspace.  It assumes both round 7 and round 8 have already landed — see `reviews/round7-*.md` and `reviews/round8-*.md` for the prior outputs — and that the codebase has continued to evolve since.  This round must rederive its findings from the *current* code, not from prior reviews.

---

## How to use

Paste everything between the `=== BEGIN PROMPT ===` and `=== END PROMPT ===` markers below as a fresh user message.  The agent will then drive the review autonomously, using subagents and skills.  Approve any tool prompts that come up during execution.

---

=== BEGIN PROMPT ===

I want you to do another round of deep code review on the strider workspace at `/mnt/c/Users/mikeg/Documents/strider`.  This is round **9** — round 7's outputs live under `reviews/round7-*.md`, round 8's under `reviews/round8-*.md`, and round 8's implementation commits were `8920d8a` / `d3e0d10` / `339b3f6` on branch `review/ai2`.

## Trust model — strict

- **Do NOT read `reviews/round7-*.md`, `reviews/round8-*.md`, or any earlier-round audit as authoritative input.**  You may at most note that an item was flagged before and re-derive the finding from scratch.  The previous reviews are stale relative to the current branch state — the code has evolved since they were written.  In particular, round-8 fixed every HIGH finding it identified; the resolver classifier surface, the orchestrator's `Builder::for_arch` migration, the `pat_builder_finalise!` macro, the `pure_pass_class!` macro, the `crates/strider/src/test_utils.rs` module, the `count_reachable` helper in `crates/opt/src/test_support.rs`, the `multiple-pymethods` PyO3 feature, and the `KnownBitsMap` SecondaryMap migration are all *new shape* relative to round 7's snapshot — so a "round 7 said X" reference is doubly stale here.
- **Do NOT trust comments, docstrings, CLAUDE.md, or per-crate READMEs as evidence.**  They are inputs to be *verified* against code.  CLAUDE.md was edited in round 8 to reflect the new shape; verify each claim against the source nonetheless.
- **Do NOT trust the previous reviews' conclusions.**  Each finding in your final summary must cite a code location (`file:line`) and explain its reasoning from code shape alone.
- Verify all rsleigh-touching claims by reading `../rsleigh/sleigh/src/**` directly — that crate is the upstream authority for pcode opcode behaviour, varnode semantics, and the per-arch SLA / PSPEC files.
- Verify ABI claims against published specs (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PPC ELF v1 / v2) — name them in your finding, but trust the *implementation* of `target::CallingConvention::*` against those specs first.

## Two emphases this round

This round has two top-level emphases that override the per-ask priorities below when they conflict.  When in doubt, lean into these — and when they conflict with each other, **emphasis A wins**: a simplification proposal that changes observable behaviour is rejected, even when it shrinks the codebase.

The two emphases are intentionally complementary: emphasis A drives the bar up on correctness; emphasis B drives the bar down on size and cognitive load.  Together they're what "ship-quality" means for a tool whose job is to produce a faithful IR of arbitrary machine code.

### Emphasis A — correctness of the code against itself AND against the lifted representation AND against the underlying assembly

Round 8 verified specific HIGH-severity correctness bugs (orchestrator preset, U512 type confusion, BoolNeg fingerprint drop, etc.) and shipped fixes.  This round goes a layer deeper: every layer of the system must agree with every adjacent layer.

Three triangulation axes:

1. **Code-vs-code self-consistency.**  When the same operation is performed in two or more places (e.g. SP decomposition in `decompose_sp` AND in `match_stack_array_shape` AND in `CallStackArgCollect`; CallOther dispatch in `cfg::region_builder` AND in `strider::IrStrider::handle_call_other`; varnode-aliasing in `pcode_lift::vn_io::read_vn` AND in `write_vn`; commutativity tables in `pattern::matcher::commutativity` AND in the build-RHS path; lift-time canonicalisation in `pcode_lift::value::*` AND in the pattern crate's lowered aliases like `sub` / `int_le`), do the implementations agree on every input?  Pin a finding for every divergence — even a documentation drift counts here.  Look for places where the same invariant is *declared* in one comment and *enforced* in only some of the consumers.

2. **IR-vs-lifted-representation correctness.**  Every IR `NodeKind` must precisely model what the underlying pcode opcode does, with no information loss and no information addition:
   - For arithmetic: the IR's `IntBinaryOp::Add` on `(a, b)` must produce the same numeric result as rsleigh's `OpBehaviorIntAdd::evaluateBinary(a, b)` for *every* input, including `INT_MIN + INT_MIN`, `0 + 0`, sub-byte operands, mismatched-width operands.  Similarly for every cmp / cast / shift / unary.
   - For memory: `Load(VnSpace)` must read from exactly the same VnSpace the pcode `LOAD` opcode reads from, with the same byte order, the same effective-width semantics, and the same memory-chain ordering against `Store` / `Call`.
   - For control: `If(cond) { true_branch, false_branch }` must dispatch on `cond ≠ 0` (the C-true convention sleigh uses), not on a boolean type.  `IndirectBranch(target)` must lift to the same address-set the underlying `BRANCHIND` opcode would dispatch to.
   - For sub-register aliasing: `pcode_lift::vn_io` must produce the *exact* shift+mask sequence the architecture uses to read AL from RAX, AH from EAX, S0 from D0/Q0, etc.  Verify against the Intel SDM / ARM ARM byte-level semantics — the AArch64 V0/D0/S0 overlap rule, x87 80-bit ST*-as-byte-array, x86 AH-as-second-byte-of-AX vs EAX-low.
   - For lift-time canonicalisations: `IntSub(a, b)` lowers to `Add(a, Neg(b))`; verify on `a - INT_MIN` (the case where the modular negation matters) that the IR's result matches rsleigh's `OpBehaviorIntSub::evaluateBinary(a, INT_MIN)`.  Same scrutiny for every other canonicalisation.
   - For CallOther: the per-op `implicit_reads` / `implicit_writes` / `memory_edge` must match the ISA reference's documented semantics for every entry in the table.  Spot-check by tracing what pcode rsleigh actually emits for the user-op (e.g. for `cpuid`, rsleigh writes the EAX/EBX/ECX/EDX results via separate post-CALLOTHER pcode `LOAD`s of a temp pointer; this means `implicit_writes` for `cpuid` is correctly empty — but verify by reading the relevant SLA spec).
   - For asm-fingerprints: the contract is that every reachable non-exempt node must carry every contributing-asm-instruction address.  Verify the contract holds end-to-end by lifting a real binary and walking every non-exempt node — do all of them have at least one fingerprint entry, and does each entry trace back to a real machine address that the lifter actually visited?

3. **Lifted-IR-vs-assembly correctness.**  Pick a real binary on each supported arch and verify the IR's behaviour matches what the binary *actually does* at runtime.  This is the deepest check — it catches bugs neither code-vs-code nor IR-vs-pcode can catch (an architectural pcode bug in rsleigh, a missing canonicalisation, a clobber-set mismatch).
   - **Return-value flow.**  For each arch, lift a function returning `int`, `long`, `struct{int,int}`, `float`, `double`, `__int128` — verify the IR's `Return` node's value inputs precisely match the registers the ABI specifies AND match what `objdump -d` shows the callee writing before `ret`.
   - **Clobber footprint per Call.**  Verify both the *positive* case (caller-saved IS clobbered post-call) AND the *negative* case (callee-saved is NOT — its value flows through the Call unchanged).  A function that reads RBX before AND after a call must show the same `NodeOutputId` in both reads in the IR.
   - **Memory chain after a call.**  `LoadReadOnly` and `StackLoadForward` must stop forwarding across normal calls AND must forward across `x86_64_all_preserving` calls (`__fentry__` / `mcount` hooks).  Pick a real Linux-kernel binary with `__fentry__` and verify that rodata loads after the hook still fold to constants.
   - **Indirect-branch resolution against ground truth.**  For each shape the resolver claims to handle (link-register return, jump-table, stack-array dispatch, tail-call, the new `Truncate(IntConst)` and `Extend(IntConst)` arms landed in round-8 follow-up), find a real binary exhibiting that shape and verify the resolved target set matches the symbol-table truth (`nm` / `addr2line`) for at least three call sites per shape.  The 7 ignored tests in `crates/strider/tests/indirect_branch.rs` document specific shapes that fail (`aarch64-be Or(SP,K) + Truncate-wrapped labels`, `mips64 PIC GOT-indirect`, `ppc32/64 uncharacterised`); the documented fix paths in those test ignore-reasons should be fact-checked here — does the actual lifter produce what they claim?

For each finding under emphasis A, the fix proposal must include a concrete regression test that lifts a real instruction and asserts the resulting IR shape — fixture-based, not hand-built mock graphs.

**Cross-tie to emphasis B**: every simplification proposal must explicitly prove behavioural equivalence with the pre-simplification code.  When the proposed change is "delete this dead branch", show that no input reaches the branch.  When the change is "merge two passes", show that the merged pass produces the same IR for every reachable input.  When the change is "inline this helper", show that no caller relied on the helper's typed boundary (e.g. an `expect`-on-`?` boundary) for error context.  Behaviour-preserving simplifications are wins; behaviour-changing simplifications are out-of-scope (they're correctness fixes that happen to shrink code, and they belong under emphasis A's bucket with a regression test).

### Emphasis B — simplification: extract, delete, merge, inline (in that order of leverage)

Round 7 and round 8 each landed targeted helper extractions (`PyMatch::with_graph`, `cc::build_cc_for_sleigh`, `pure_pass_class!` macro, `pat_builder_finalise!` macro using `multiple-pymethods`, `count_reachable` promoted to `crates/opt/src/test_support.rs`, `crates/strider/src/test_utils.rs` exposing `strider_x86_64()` / per-arch wrappers).  This round does an exhaustive simplification sweep — and "simplification" here is broader than just extracting helpers.

The brief is: **reduce total LOC AND reduce reader cognitive load** (the second condition matters because a one-line "clever" replacement of three obvious lines is a regression).  Every proposal must net out positive on both axes.

The simplification toolkit, ranked by leverage:

1. **Delete dead code.**  The biggest LOC win and the lowest risk.  Hunt for:
   - `pub` items with zero external consumers (verify by `grep` across the workspace AND across `strider-py`'s Python surface).  An `unused_must_use`-style audit, but for whole functions / structs / variants / trait impls / CC presets / `SleighArch` presets / pattern free constructors.
   - Test fixtures that no test in the workspace references.
   - `#[cfg(any())]`-disabled or commented-out code.
   - `// removed:` tombstone comments referencing symbols that have already been deleted (round-8 noted some `CallOtherElide` tombstones in `opt/src/lib.rs:148-151,181-183`; round 7 explicitly preserved them as breadcrumbs — verify whether that justification still applies, then either delete or note "intentional historical breadcrumb").
   - `Default` impls / `Clone` derives / etc. that nothing constructs.
   - Re-exports for items that aren't reached from any external consumer.

2. **Merge similar code.**  Includes both helper extraction (the round-8 emphasis) AND structural merges:
   - Two opt passes that do nearly-the-same work merged into one with a config flag.
   - Two `NodeKind` variants that the lifter / opt / pattern paths always handle identically (e.g. `IntCmpOp::Less` and `IntCmpOp::Sless` if the entire codebase always treats them via a `is_less_family()` helper — which is unlikely but worth checking).
   - Two trait impls on the same type that overlap (e.g. `From<T>` and a `to_pat()` method that does the same thing).
   - Two error variants that always coincide.
   - Two test helpers in `tests/common/mod.rs` and `src/test_support.rs` that do the same job (round 8 promoted helpers from `tests/common` to `src/test_support`; verify no third copy survives).

3. **Inline single-callsite helpers.**  The inverse of extraction.  When a `fn helper(...)` is called from exactly one place AND the helper's body is < 5 lines, inline it — the abstraction is paying no rent.  Especially common for:
   - `pub(super)` helpers in opt passes that only the parent module uses.
   - `fn build_X(...)` helpers in `flag_cmp_canonicalize` / `if_cond_inversion` rule builders called from one rule each.
   - Any helper whose body is `self.field.foo()` or `(*self.inner).clone()`.

4. **Replace bespoke patterns with stdlib idioms.**  Often the original author didn't know about a stdlib feature.  Examples to scan for:
   - Manual `.iter().filter(|n| reachable.contains(n)).filter(|n| pred(...))` chains where `reachable.intersection(...)` or a single combined predicate is shorter (when the result is the same).
   - Hand-rolled `let Some(x) = opt else { return None; }` chains where `?` would suffice (caller's return type permitting).
   - `match x { Some(v) => v, None => return None }` → `x?`.
   - `.collect::<Vec<_>>().len()` → `.count()`.
   - `for item in vec.iter() { mem.push(item.clone()) }` → `mem.extend_from_slice(&vec)`.
   - Manual saturating-arithmetic patterns where `usize::saturating_add` / `i64::checked_add` apply.
   - `.unwrap_or_else(|_| f())` where the closure's only purpose is `f()` — use `.unwrap_or_else(f)` directly.

5. **Tighten visibility (fewer `pub` = smaller API surface = simpler).**  Round 8 deferred this with the rationale that several items are needed cross-crate.  Verify each `pub` declaration's actual external consumer; if there's none, propose `pub(crate)` or `pub(super)`.  A smaller `pub` set is a smaller API contract — the codebase is simpler from the outside-in.

6. **Drop redundant wrappers.**  Newtypes that don't carry an invariant or a method are pure overhead.  If `pub struct Foo(Inner);` has no methods and the `Inner` type is already public, just expose `Inner` directly.  Examples to check: any `struct WrapperPat(Pat)` with an `impl From<Pat> for WrapperPat` and nothing else; any `struct Built<X>(X)` that's only ever `.0`-accessed.

7. **Collapse partial-state types into proper sum types.**  When a `struct` has fields populated to "zero/None/empty" sentinels in some construction paths and to "real" values in others, that's a partial-state type — model it as `enum { Real { ... }, Empty }` instead.  Round 8 noted `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` produces a partial-state form; propose either making the partiality explicit in the type OR converting all consumers to use the full form.

Per-finding criteria (every proposal must answer all of these):

- **Net LOC delta.**  After applying the proposal, does the codebase have fewer lines?  A 1-LOC helper that replaces a 1-LOC pattern is net zero (skip).
- **Net cognitive delta.**  After the proposal, is the call site easier to read?  A `macro_rules!` that hides 4 lines but makes the call site `pure_pass_class!("Foo" => PyFoo)` is a win because the macro name is self-documenting; a `helper_X!()` that hides 4 lines but the call site is now `helper_X!(state, ctx, mode)` with no clarity about what's happening is a loss.
- **Three-occurrence threshold (for extraction).**  Patterns repeated ≥ 3 times, ≥ 3 LOC each are MED; ≥ 5 occurrences OR ≥ 6 LOC each are HIGH.  Two-occurrence repetition is a LOW finding.
- **Load-bearing-different vs accidentally-different (for extraction).**  When two repetitions look the same but produce different runtime behaviour, the difference is *load-bearing* — propose unifying only after explicitly noting why each variant exists.  If a candidate site has subtly different ordering, error handling, or ownership shape, document the difference and decide skip vs. unify per-case.
- **Don't force-fit `macro_rules!`.**  Use `macro_rules!` only for syntactic patterns that genuinely repeat (e.g. the `pure_pass_class!` / `pat_builder_finalise!` shapes).  For semantic repetition (data-flow patterns, walks, error-wrap-and-propagate), prefer a function or extension trait.  Macros lose IDE support, defeat type inference, and are read-once / write-many.
- **Visibility tightening as a side-effect.**  When a helper is extracted, the original implementation may have been over-public — propose the minimum visibility for the new helper.
- **Skip when the indirection cost exceeds the duplication cost.**  If a proposed helper is < 3 LOC AND the call sites are < 5, document the decision and skip.

The Round 5 simplifications report should land at `reviews/round9-simplifications.md` with sections matching the toolkit above:

1. Code to delete (largest expected LOC win).
2. Code to merge (helper extractions + structural merges).
3. Single-callsite helpers to inline.
4. Bespoke patterns to replace with stdlib idioms.
5. Visibility to tighten.
6. Wrappers to drop.
7. Partial-state types to convert.

Aim for 50–80 concrete entries with a per-category total LOC delta at the top.  Round 8's simplifications report had 30 entries; this round goes broader.

## Coverage requirement — every line of source must be inspected

This review is **exhaustive**, not sampled.  Every `.rs` file under `crates/*/src/`, every `.rs` file under `crates/*/tests/` and `crates/*/benches/`, every `.py` file under `crates/strider-py/tests/python/`, every `Cargo.toml`, every `*.md` file under `crates/*/README.md` + the root README + CLAUDE.md + `crates/strider/.claude/skills/*/SKILL.md`, must be **read in full** by at least one subagent during the rounds below.

Concretely:

- **Inventory first.**  The Round 0 orientation step must produce `reviews/round9-coverage-manifest.md` listing every file in scope (use `find crates -name '*.rs' -o -name '*.toml'` + `find crates/strider-py/tests -name '*.py'` + the doc set).  Every file in that manifest must be ticked off as "inspected by subagent X" by the time Round 7 (final consolidation) runs.
- **No globbing skips.**  If a file is short, glance through it and tick it.  If a file is long (e.g. `crates/opt/src/constant_fold/rules.rs`, `crates/pattern/src/matcher/mod.rs`, `crates/strider/src/orchestrator.rs`, `crates/strider-py/src/pattern.rs`), read it in 200-line chunks until the whole file is covered.  When a subagent reports its findings it should also report which files it covered fully, partially, or not at all; partial / not-at-all entries become Round 1.5 follow-up tasks for a fresh subagent.
- **Tests count.**  Test files surface missing-coverage and stale-fixture issues that source-only review misses — read every `tests/` and `benches/` file too, including the strider-py `tests/python/`.
- **Generated / auto-formatted code is in scope** if it lives under `crates/*/src/`.  Skip only `target/`, `.git/`, `node_modules/`, `__pycache__/`, build artefacts.
- **Exception (allowed skips):** the `../rsleigh` upstream crate is *consulted* (verifying claims) but not *audited* — it's a third-party dep, out of scope for this review.

The Round 7 final summary must include a coverage table: for each crate, "X of Y .rs files inspected fully, Z partially, W skipped".  A non-trivial skip count is a signal that the round needs another sweep — surface it explicitly rather than papering over it.

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
3. Be told explicitly: do NOT read `reviews/round7-*.md`, `reviews/round8-*.md`, or any earlier-round output.
4. Produce a single Markdown report at `reviews/round9-<topic>.md` with HIGH/MED/LOW findings, each with `file:line` and a concrete fix.
5. Output format per finding:
   ```
   ### <Finding title>
   - **Severity:** HIGH/MED/LOW
   - **Where:** crates/foo/src/bar.rs:42-58
   - **What's wrong:** <evidence from code, not from comments>
   - **Verified against:** <rsleigh path / IR signature / sibling pass / ABI spec / objdump trace>
   - **Fix:** <concrete patch or rewrite plan>
   - **Regression test (when applicable):** <fixture-based test pinning the fix>
   ```

When you launch multiple subagents for independent work, send them in a **single message with multiple Agent tool uses** so they run concurrently.

## Concrete asks (numbered)

1. **Correctness — code vs code self-consistency** *(emphasis A axis 1)*.  For every operation implemented in 2+ sites, verify the implementations agree on every input.  Sites to start from: SP decomposition, CallOther dispatch, varnode aliasing, commutativity tables, lift-time canonicalisations.  Pin every divergence as a HIGH finding, even if it's documentation-only drift.

2. **Correctness — IR vs lifted representation** *(emphasis A axis 2)*.  Every `NodeKind` must precisely model the underlying pcode opcode.  Verify by lifting one real instruction per opcode family and comparing IR shape to rsleigh's documented semantics.  Cover: arithmetic (Add/Sub/Mul/Div/Mod, signed and unsigned), shifts (Shl/Shr/Sar), bit-ops (And/Or/Xor/Not), comparisons (Equal/Less/Sless and the lowered Le/Sle/Ne shapes), casts (Truncate/ZeroExtend/SignExtend, IntToFloat/FloatToInt/FloatToFloat, IntBitsToFloat/FloatBitsToInt), float arith (Add/Mul/Div, Sub-as-lowered, NaN-aware Less/Equal/LessEqual), memory (Load/Store with VnSpace), control (If/IndirectBranch/Call/CallOther/Return), phis (VarPhi/MemPhi/ValuePhi/StackStorePhi), wide constants (`IntConstWide` U256/U512), sub-register aliasing.  Sample at least 30 instruction-IR pairs.

3. **Correctness — lifted IR vs real assembly** *(emphasis A axis 3)*.  Per arch (x86, x86_64, ARM, ARM-BE, ARM-Thumb, AArch64, AArch64-BE, MIPS32-LE/BE, MIPS64-LE/BE, PPC32-LE/BE, PPC64-LE/BE), pick one representative function from `fixtures/out/<arch>/` and verify:
   - Return-value registers match the ABI.
   - Caller-saved registers are clobbered, callee-saved are not.
   - Memory chain monotonicity (every `Store` advances the chain; every `Call` advances unless the per-address override sets `no_memory_clobber`).
   - Indirect branches resolve to the right targets (the 7 ignored tests in `crates/strider/tests/indirect_branch.rs` document specific shapes — fact-check the documented fix path against the actual lifter output).
   - Per-arch CallOther entries fire correctly (e.g. ARM `swi` is reading r7/r0..r6 and writing r0; AArch64 HVC/SMC are reading x0..x7 and writing x0..x3).
   - Every fix proposal includes a regression test that lifts the relevant instruction.

4. **Simplicity — exhaustive simplification sweep** *(emphasis B)*.  Apply the seven-category toolkit (delete dead code, merge similar code, inline single-callsite helpers, replace bespoke patterns with stdlib idioms, tighten visibility, drop redundant wrappers, collapse partial-state types).  For every entry, answer the per-finding criteria (net LOC delta, net cognitive delta, three-occurrence threshold for extraction, load-bearing distinction, indirection-cost veto).  Specific things to look for that prior rounds either missed or under-explored:
   - **Dead code that's been hiding behind a `#[allow(dead_code)]` or `pub(crate)` re-export with no actual consumer**: walk every `pub`/`pub(crate)` item and verify at least one consumer exists.
   - **Redundant wrapper types** (a `struct Foo(Inner)` with no methods worth wrapping for; verify by checking whether all `Foo` consumers immediately destructure to `Inner`).
   - **Unused features**: every `target::CallingConvention` preset, every `SleighArch` preset, every pattern free constructor, every NodeKind variant, every pcode opcode arm in `pcode_lift::value::*`, every `#[pyfunction]` exported from `strider-py` — verify each via `grep` for at least one external consumer.  Items with zero consumers go in the delete bucket.
   - **Stale `#[ignore]` test reasons** that no longer match what the test does (the round-8 sweep cleaned R1/R2/Tier-N — check if more accumulated, especially the 7 ignored `indirect_branch_resolved_*` tests whose ignore-reasons claim specific lifter shapes — verify the shapes are still what the lifter produces).
   - **Helper extraction candidates** organised by the three-occurrence + load-bearing analysis.
   - **Single-callsite helpers** that should be inlined (the inverse).
   - **Hand-rolled patterns** where stdlib has a one-line equivalent (`?` instead of `let Some(_) = _ else { return None }`, `.count()` instead of `.collect::<Vec<_>>().len()`, etc.).
   - **Partial-state types** (a struct populated to sentinel values in some paths and to real values in others) candidate for sum-type rewrite.

5. **Naming.**  Look for unclear / misleading / half-renamed identifiers anywhere — not just round-7's `tier1`/`tier2` and round-8's `x86_64_systemv_abi`/`r1_placeholder`.  Verify that the meaning of every term in CLAUDE.md / per-crate README / SKILL.md matches the actual code.  Look for: leftover one-letter or short test names that only made sense in their original context, abbreviations whose expansion would be clearer, type names that don't capture the type's actual role.  Propose a concrete rename mapping for every flagged identifier.

6. **Unused features.**  Confirm by code inspection that every `pub` item has at least one external consumer (within the workspace OR via Python bindings).  Items unreachable from any consumer are candidates for deletion.  Check both ways: walk down from `lib.rs`'s `pub use` re-exports to find every public surface, then walk back up from each surface to find at least one external call site.

7. **Python binding parity.**  Verify every IR / pattern / opt / target / strider feature accessible from Rust is also reachable from `strider-py`'s Python API, OR is documented as deliberately Rust-only (with rationale).  GIL handling correctness on every callback path.  Typed exception coverage on every fallible Python entry.  Round 8 added `multiple-pymethods` to the PyO3 feature set; verify no new code path leaks panics that the typed-exception layer should catch.

8. **Multiple rounds of correctness audit, rotating focus** *(retained from round 8 with refinements)*.
   - **Round-1 pass (per-crate)**: typing / signature / arity errors — does the code do what its types say it does?
   - **Round-2 pass (re-audit, fresh subagent per crate, cannot read round-1's output)**: invariant-violation errors — does this code maintain the invariants its callers depend on?
   - **Round-3 pass (re-audit, fresh subagent per crate, cannot read round-1 or round-2)**: concurrency / aliasing / borrowing errors — for any code that holds a `&mut` while doing work that could re-enter or invalidate the borrow target, are the lifetimes provably sound?
   - **Round-4 pass (re-audit, focused on edge cases)**: boundary errors — empty / single / max-arity inputs, NaN / inf / signed-zero floats, INT_MIN sign-extension, address `u64::MAX`, instruction at `addr = start_addr` boundary, the very last node id in the arena, the lifetime-zero-overlap case for `StackStorePhi`.
   - **Round-5 pass (cross-arch consistency)**: pick one finding per round and verify it across every arch.

   Each pass produces its own subagent report (`reviews/round9-correctness-types.md`, `round9-correctness-invariants.md`, `round9-correctness-borrowing.md`, `round9-correctness-edge-cases.md`, `round9-correctness-cross-arch.md`).

9. **Test plan.**  Where coverage is sparse, propose specific tests with file path, scope (unit / integration / property / scale), exact harness/fixture, expected assertions.  Use TDD discipline — failing test FIRST, then fix.

10. **Stale comments.**  Verify every `pub` item's docstring matches the actual code.  Hunt for `TODO`s linked to closed work, references to deleted symbols, half-rename leftovers, comments that describe behaviour the surrounding code doesn't implement.

11. **Production panics.**  No `panic!` / `unwrap()` / `expect()` / `unreachable!()` / `assert!()` in non-test code paths.  Audit every occurrence; if not justified by a by-construction invariant, propose `Result` propagation.  Annotate justified ones with `#[allow(clippy::expect_used)]` and a code comment naming the invariant.

12. **CLAUDE.md / READMEs / doc consistency.**  Verify the root README's claims, every per-crate README's public-surface enumeration, every `pub fn` doc.  Round 8 landed a CLAUDE.md correctness diff and 12 per-crate READMEs (`reviews/round8-claudemd-diff.md`, `reviews/round8-readme-diffs.md`); verify against the post-round-8 state.

13. **Skills.**  Skim `crates/strider/.claude/skills/*/SKILL.md`.  Identify any new skill that would help future contributors (or any existing skill that has decayed against the current code).  Round 8 added 6 new skills (orchestrator-extend, cc-preset-extend, fixture-author, flagcmp-rule-author, cli-runner, validation-invariant-extend); verify each against the actual procedure they describe.

14. **Scale.**  Verify behaviour at ~10k–100k IR nodes — recursion-induced stack-overflow risk in any function that walks a memory or control chain without an explicit depth bound, asymptotic complexity in hot paths (`Matcher::find_all`, `find_all_requirements`, `validate`, `create_node` dedup, opt pipeline iteration, orchestrator fixed-point), memory growth (zombie pollution after the destructive pipeline, side-table bloat), Python GIL hold-time on long lifts.  Round 8 migrated `WorkSet`/`KnownBitsMap`/`detach_unreachable_nodes` to `DenseEntitySet`/`SecondaryMap`; verify the migrations are complete (no remaining `FxHashSet<NodeId>` / `FxHashMap<NodeOutputId, _>` in hot paths) and look for the next tier of perf wins.

15. **Type design.**  Audit every `pub` struct / enum / trait for: leaky encapsulation (public fields where invariants exist), primitive obsession (`u32` where a newtype would express intent), types that fail to express their invariants, partial-state types (struct with all fields populated to "valid" sentinels rather than `Option<_>` or a sum type for the genuine states).  Round 8's deferred items included `Pattern` trait sealing, `MatchCtx::graph` tightening, `PcodeInsnAddr` field-access leak (~30 sites) — verify these are still tradeoff calls or have moved.

16. **Silent failures.**  Hunt every `unwrap_or` / `unwrap_or_default` / `if let Ok` followed by ignore-Err / `.ok()?` / `match … _ => return None` swallowing real errors.  Distinguish "intentional fallback for a documented optional path" from "swallowed bug".  Round 8 cleaned the `flag_cmp_canonicalize::unwrap_or(a)` and `apply_tail_call::unwrap_or(U64)` and `anchor_contexts::unwrap_or(&empty_ctx)` instances — verify no new silent failures crept back in.

17. **Helpers / generalisation** *(emphasis B; see top-level criteria above)*.  Identify recurring patterns across the codebase (graph traversal, error-wrapping boilerplate, opt-pass scaffolding, side-table mutations, test-fixture builders).  Apply the three-occurrence threshold + load-bearing-different vs. accidentally-different distinction.  For each accepted candidate, propose the helper signature, location, and migration shape at every call site.

18. **Build / lint / test baseline.**  At Round 0 and at the end of Round 7, the workspace must satisfy:
    - `cargo build --workspace --all-targets` clean.
    - `cargo clippy --workspace --all-targets -- -D warnings` clean.
    - `cargo test --workspace` all passing (currently 123 suites; round 8 set `[lib] test = false` on `strider-py` because the `multiple-pymethods` feature pulls in Python symbols the test harness can't resolve).
    - `cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/` all passing (currently 759 + 14 skipped).

    Surface any drift in the Round 0 report; surface any regression in the Round 7 final summary.

## Recommended round structure

### Round 0 — orient
Read CLAUDE.md, the workspace `Cargo.toml`, the per-crate `Cargo.toml` files, every per-crate README, every existing `crates/strider/.claude/skills/*/SKILL.md`, the round-8 implementation commit messages (`git log --no-decorate review/ai2 -- ':!reviews/'` for the post-round-7 commits).  Build a mental model of what's in each crate.  Run the four baseline checks above.  Produce `reviews/round9-coverage-manifest.md` listing every file in scope.

### Round 1 — deep per-crate audit (parallel; 6 subagents)
Six `feature-dev:code-reviewer` agents, one per crate group:

| # | Crates | Special focus (round 9) |
|---|--------|-------------------------|
| 1A | `ir` | Graph dedup correctness; asm-fingerprint contract; validate Layer A/B/C reachability scoping; `FunctionBuilder::lift_at` / `LiftAddrGuard` invariants; `node_signature` panic sites; `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` partial-state (still `pub`?); `KnownBitsMap` / `WideConstStorage` / `IntConstWide` correctness against the new `make_int_const(U256/U512)` rejection guard |
| 1B | `pcode-lift` + `cfg` | `vn_io` register-aliasing for *every* supported width (1/2/4/8/10/16/32/64; round 8 added 32 / 64); the new `Truncate(IntConst)` / `Extend(IntConst)` resolver-arm peeling at `crates/opt/src/indirect_branch_resolve/classify.rs`; `Builder::for_arch` vs `Builder::with_endianness` invariants (round 8 migrated tests/common + benches/scaling); `is_addr_tail_call` half-open semantics |
| 1C | `opt` | Each pass: rewrite + no-op + idempotency + ordering interactions; `flag_cmp_canonicalize` `Option<Capture>` rhs binding (round 8 refactor); `IfCondInversion` BoolNeg fingerprint absorption; `KnownBits::analyze` `SecondaryMap`-based correctness (round 8 migration); `StackLoadForward` BE narrow-path attribution (round 8 fix); `decompose_sp` iterative form (round 8 conversion); `IndirectBranchResolve` `Truncate`/`Extend` peel arms (round-8 follow-up) |
| 1D | `pattern` | `pat_builder_finalise!` macro (round-8 follow-up: 15 builders go through it; verify each invocation lands in the `multiple-pymethods` second-pymethods block correctly); `RewriteCtx<'g>` newtype; commutativity tables; `Match::get_vn` per-CallOther override length (round 8 fix); lift-time canonicalisation aliases (`sub`, `int_le`, `int_sle`); `find_all_requirements` cross-product correctness |
| 1E | `strider` + `target` + `reader` | `crates/strider/src/test_utils.rs` (round-8 follow-up new module); orchestrator `Builder::for_arch` migration; per-Call `try_write_inner` deadlock guard (`PyGraph` mutating methods); `target::call_other_abi` table coverage incl. mfence/sfence/lfence (round 8 added); LR-as-callee-saved deliberate tradeoff for AArch64/ARM/PPC; `apply_elf_relocations_autoload` correctness |
| 1F | `strider-py` + `dot` + `graphwalk` + `entity-utils` | PyO3 `[lib] test = false` rationale (round-8 follow-up); `multiple-pymethods` macro behaviour at compile time + runtime; `cc::build_cc_for_sleigh` helper (round-8 follow-up); `pure_pass_class!` macro (round-8 follow-up); `PyMatch::with_graph` helper (round-8 follow-up); GIL release in `strider.run`; PyO3 unsafe blocks; `DenseEntitySet`/`SecondaryMap` migration completeness |

### Round 2 — cross-cutting passes (parallel; 4 subagents)
- **2A — production-panic hunt.**  `feature-dev:code-reviewer`.  Walk every `unwrap()` / `expect()` / `panic!()` / `unreachable!()` / `assert!()` outside `#[cfg(test)]` / `tests/` / `examples/` / `benches/`.  For each: justified by a by-construction invariant or unjustified?
- **2B — naming sweep.**  List every occurrence of unclear / misleading / half-rename leftover identifiers.  Round 8 cleaned R1/R2/Tier-N + `x86_64_systemv_abi` + `r1_placeholder.rs`; look for the next tier.
- **2C — silent-failure hunt.**  `pr-review-toolkit:silent-failure-hunter`.  Look for `unwrap_or`, `unwrap_or_default`, `.ok()?`, `if let Ok` followed by ignore-Err.
- **2D — type-design analyser.**  `pr-review-toolkit:type-design-analyzer`.  Audit every `pub` struct / enum / trait for: leaky encapsulation, primitive obsession, partial-state types.

### Round 3 — verification + comments (parallel; 2 subagents)
- **3A — trust-only-the-code verification.**  Sample ≥ 25 specific claims from CLAUDE.md, per-crate READMEs, and every existing `SKILL.md`.  For each, find the code and confirm or refute purely from code shape.
- **3B — stale comment sweep.**  `pr-review-toolkit:comment-analyzer`.  Flag every comment block that names a deleted symbol, has a `TODO(TaskNN)` whose task is closed, or describes behaviour that doesn't match the surrounding code.

### Round 4 — test-gap analysis
`feature-dev:code-architect`.  Consume Round 1 outputs.  Emit `reviews/round9-test-plan.md`.

### Round 5 — consolidation + simplification
`pr-review-toolkit:code-simplifier`.  Consume Rounds 1–3.  Emit `reviews/round9-simplifications.md` per emphasis B's seven-category toolkit:

1. Code to delete.
2. Code to merge.
3. Single-callsite helpers to inline.
4. Bespoke patterns to replace with stdlib idioms.
5. Visibility to tighten.
6. Wrappers to drop.
7. Partial-state types to convert.

50–80 entries with per-category LOC delta + projected post-implementation workspace LOC reduction at the top.

### Round 6 — skill audit
Skim every existing skill against the current code.  Propose new skills or revisions.

### Round 7 — final consolidation
A single synthesis that integrates every prior-round output into:

1. `reviews/round9-summary.md` — executive summary with prioritised fix backlog.
2. `reviews/round9-claudemd-diff.md` — concrete CLAUDE.md edits.
3. `reviews/round9-readme-diffs.md` — concrete per-crate README edits.

The summary must include a "what's-new-vs-round-8" section: which findings are genuinely new (introduced post-round-8) vs. which are pre-existing bugs round 8 missed vs. which are pre-existing-and-known-deferred.

## Acceptance criteria

- [ ] Every numbered ask (1–18) has a corresponding section in `reviews/round9-summary.md` with concrete actions.
- [ ] Emphasis A (correctness against itself + against pcode + against assembly) produces three reports: `reviews/round9-correctness-self-vs-self.md`, `reviews/round9-correctness-ir-vs-pcode.md`, `reviews/round9-correctness-ir-vs-assembly.md`.  Each finding cites code locations + ABI / opcode references + (for axis 3) the real binary's `objdump` trace.
- [ ] Emphasis B (simplification) produces `reviews/round9-simplifications.md` with 50–80 entries grouped into the seven-category toolkit (delete, merge, inline, stdlib idioms, visibility, drop wrappers, sum-type partial states).  Header includes the per-category total LOC delta and the projected post-implementation workspace LOC reduction.
- [ ] Ask 8 (multi-round correctness) produces five separate reports — `round9-correctness-types.md`, `round9-correctness-invariants.md`, `round9-correctness-borrowing.md`, `round9-correctness-edge-cases.md`, `round9-correctness-cross-arch.md` — each from a fresh subagent that did not read the others.
- [ ] Round 9 summary lists every HIGH-severity finding with `file:line` + a proposed fix + (for emphasis A findings) the regression test scaffolding.
- [ ] CLAUDE.md correctness diff lists every drift from the current code.
- [ ] Per-crate README diff lists drift in every crate that has one.
- [ ] Test plan lists ≥ 15 missing tests with exact `file:line` scaffolding.
- [ ] Naming sweep produces a concrete rename mapping (every flagged identifier has a target name).
- [ ] Production-panic audit lists every unjustified `unwrap`/`expect`/`panic!` with proposed `Result` plumbing.
- [ ] Type-design audit produces a list of `pub` struct/enum candidates for visibility tightening or newtype wrapping, with verification that the proposed visibility is achievable.
- [ ] Silent-failure audit produces a list of `.ok()?` / `unwrap_or` sites with a propose-or-document decision per site.
- [ ] Skill audit produces a list of revisions or new-skill proposals.
- [ ] `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `pytest tests/python/` all green at the start AND end of the review.
- [ ] No source code is edited during the review.  The output is the set of `reviews/round9-*.md` reports.  Implementation is a follow-up task that the user will explicitly approve.
- [ ] `reviews/round9-coverage-manifest.md` exists and shows every `.rs` / `.py` / `Cargo.toml` file under `crates/` ticked off as "inspected fully" by at least one subagent.

## Out of scope

- **Editing source code.**  Only documentation under `reviews/round9-*.md` is allowed.
- **Authoring new tests.**  Only the test plan is in scope; writing the actual tests is follow-up.
- **Authoring new skills.**  Only the skill design is in scope.
- **Per-crate README rewrites.**  Only the diff is in scope.

## Critical files to consult

These are the surfaces most worth starting from — but they are **not exhaustive**.  The coverage requirement above demands every `.rs` / `.py` / `Cargo.toml` under `crates/` be inspected.

- IR core: `crates/ir/src/{lib,graph/{mod,store,access,compact},function,validate/{mod,layer_a,layer_b,layer_c},walk,node_signature,builder/{mod,lift_addr,call,nodes,vars},wide_const,ops/{mod,builder,consts,op_kinds,rewrite}}.rs`
- Lifter: `crates/pcode-lift/src/{lib,vn_io,value/{mod,arithmetic,boolean,cast,float,integer,mem_load,misc_value}}.rs`
- CFG: `crates/cfg/src/cfg/{builder/{mod,region_builder,split,indirect_resolve},types,decode_cache,query,options}.rs`
- Optimizer: every `crates/opt/src/<pass>/mod.rs`, plus `pipeline.rs`, `sp_expr.rs`, `worklist.rs`, `test_support.rs`, `indirect_branch_resolve/{mod,jump_table,stack_array,classify,inplace}.rs`, `stack_load_forward/mod.rs`, `function_args/mod.rs`, `flag_cmp_canonicalize/mod.rs`, `if_cond_inversion/mod.rs`
- Pattern: `crates/pattern/src/{lib,rewrite,error,matcher/{mod,bindings,match_result,walk,walk_through,function_arg_handle,commutativity,cast_mask},pat/{mod,traits,node_pat,any,guards,builders/*,ctor/*}}.rs`
- Strider: `crates/strider/src/{lib,errors,orchestrator,rewrite,test_utils,indirect_resolve/{mod,classify,inplace},strider/{mod,pipeline,vn_io,insn/{mod,control}}}.rs`
- Target: `crates/target/src/{lib,arch,call_other_abi,calling_convention/mod}.rs` cross-checked against `../rsleigh/sleigh/src/**`
- Reader: `crates/reader/src/{lib,elf}.rs`
- PyO3: `crates/strider-py/src/{lib,errors,pattern,graph,opt,reader,arch,cc,run,strider_cls,sleigh,cfg,matcher,dot}.rs`
- Skills: `crates/strider/.claude/skills/*/SKILL.md` (round 8 added 6; round 7 had 8)
- Tests: every `crates/*/tests/*.rs`, every `crates/strider-py/tests/python/*.py`, the 7 ignored `indirect_branch_resolved_*` tests in `crates/strider/tests/indirect_branch.rs` (each ignore-reason names a specific resolver gap)

After working through the anchor list, every subagent must continue through the rest of its assigned crate's source until the coverage manifest's tick-list is complete for that crate — including `lib.rs`, every sub-module, every `tests/` and `benches/` file.

## Verification

The review is itself a research effort — the "verification" is the quality of the final summary, not a build.  The acceptance criteria above are the bar.  At the end, leave a short note in `reviews/round9-summary.md` describing:

- Total HIGH / MED / LOW finding counts.
- How many findings were genuinely new (post-round-8) vs. pre-existing bugs round 8 missed vs. pre-existing-and-deferred.
- Which subagents found load-bearing items the others missed (the multi-round signal).
- For emphasis A: the count of findings per axis (code-vs-code, IR-vs-pcode, IR-vs-assembly).
- For emphasis B: per-category total LOC delta (delete / merge / inline / stdlib / visibility / wrappers / partial-state), the workspace's pre-review LOC, the projected post-implementation LOC, and the per-category cognitive-load assessment (subjective, but called out for the top 5 entries per category).

=== END PROMPT ===

---

## Notes for the user

- The prompt explicitly forbids reading `reviews/round7-*.md` and `reviews/round8-*.md` so the next round derives findings independently.  The previous rounds' outputs stay on disk as historical context.
- The two emphases (A: triangulated correctness, B: exhaustive helper extraction) override conflicting per-ask priorities.  The subagent prompts inherit that ordering.
- The prompt is sized to drive ~4–5 hours of subagent work plus ~1–2 hours of consolidation (longer than round 8 because of the assembly-vs-IR ground-truth verification).  Approve tool prompts as they appear.
- After the review lands, you'll have the `reviews/round9-*.md` set — at that point a follow-up prompt of the form "land everything not refuted from round 9" runs the same play we ran for rounds 7 and 8.
