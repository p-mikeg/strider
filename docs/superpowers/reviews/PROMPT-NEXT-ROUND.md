# Strider — Next-Round Code Review Prompt

> **Purpose.** This file is a self-contained prompt the user can paste back into Claude later to drive a fresh, independent code review of the strider workspace.  It assumes prior rounds 7–10 have already landed — see `reviews/round7-*.md`, `reviews/round8-*.md`, `reviews/round9-*.md`, and `reviews/round10-*.md` — and that the codebase has continued to evolve since.  This round must rederive its findings from the *current* code, not from prior reviews.

---

## How to use

Paste everything between the `=== BEGIN PROMPT ===` and `=== END PROMPT ===` markers below as a fresh user message.  The agent will then drive the review autonomously, using subagents and skills.  Approve any tool prompts that come up during execution.

---

=== BEGIN PROMPT ===

I want you to do another round of deep code review on the strider workspace at `/mnt/c/Users/mikeg/Documents/strider`.  This is round **11**.

## Trust model — strict

- **Do NOT read `reviews/round7-*.md`, `reviews/round8-*.md`, `reviews/round9-*.md`, `reviews/round10-*.md`, or any earlier-round audit as authoritative input.**  You may at most note that an item was flagged before and re-derive the finding from scratch.  The previous reviews are stale relative to the current branch state — the code has evolved since they were written, sometimes substantially.  Each finding in your final summary must be derived independently from the current source.
- **Do NOT trust comments, docstrings, CLAUDE.md, or per-crate READMEs as evidence.**  They are inputs to be *verified* against code.  Documentation drift is one of the things this review is hunting for.
- **Do NOT trust the previous reviews' conclusions.**  Each finding in your final summary must cite a code location (`file:line`) and explain its reasoning from code shape alone.
- Verify all rsleigh-touching claims by reading `../rsleigh/sleigh/src/**` directly — that crate is the upstream authority for pcode opcode behaviour, varnode semantics, and the per-arch SLA / PSPEC files.
- Verify ABI claims against published specs (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PPC ELF v1 / v2) — name them in your finding, but trust the *implementation* of `target::CallingConvention::*` against those specs first.

## Two emphases this round

This round has two top-level emphases that override the per-ask priorities below when they conflict.  When in doubt, lean into these — and when they conflict with each other, **emphasis A wins**: a simplification proposal that changes observable behaviour is rejected, even when it shrinks the codebase.

The two emphases are intentionally complementary: emphasis A drives the bar up on correctness; emphasis B drives the bar down on size and cognitive load.  Together they're what "ship-quality" means for a tool whose job is to produce a faithful IR of arbitrary machine code.

### Emphasis A — correctness of the code against itself AND against the lifted representation AND against the underlying assembly

Three triangulation axes:

1. **Code-vs-code self-consistency.**  When the same operation is performed in two or more places (e.g. SP decomposition; CallOther dispatch; varnode-aliasing read/write inverses; commutativity tables; lift-time canonicalisation in the lifter AND in the pattern crate's lowered aliases like `sub` / `int_le`), do the implementations agree on every input?  Pin a finding for every divergence — even a documentation drift counts here.  Look for places where the same invariant is *declared* in one comment and *enforced* in only some of the consumers.

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
   - **Memory chain after a call.**  `LoadReadOnly` and `StackLoadForward` must stop forwarding across normal calls AND must forward across all-preserving (zero-side-effect) hooks like `__fentry__` / `mcount`.  Pick a real Linux-kernel binary with `__fentry__` and verify that rodata loads after the hook still fold to constants.
   - **Indirect-branch resolution against ground truth.**  For each shape the resolver claims to handle (link-register return, jump-table, stack-array dispatch, tail-call, `Truncate(IntConst)` and `Extend(IntConst)` arms), find a real binary exhibiting that shape and verify the resolved target set matches the symbol-table truth (`nm` / `addr2line`) for at least three call sites per shape.  Any `#[ignore]`'d tests in `crates/strider/tests/indirect_branch.rs` document specific shapes that fail; the documented fix paths in those test ignore-reasons should be fact-checked here — does the actual lifter produce what they claim?
   - **Lift-time canonicalisations are bit-exact.**  Every canonicalisation in the lifter must lower to a shape that, when re-evaluated, produces the same numeric result as the original opcode for every input — including pathological boundaries (`INT_MIN`, NaN, infinities, signed-zero, sub-byte widths).

For each finding under emphasis A, the fix proposal must include a concrete regression test that lifts a real instruction and asserts the resulting IR shape — fixture-based, not hand-built mock graphs.

**Cross-tie to emphasis B**: every simplification proposal must explicitly prove behavioural equivalence with the pre-simplification code.  When the proposed change is "delete this dead branch", show that no input reaches the branch.  When the change is "merge two passes", show that the merged pass produces the same IR for every reachable input.  When the change is "inline this helper", show that no caller relied on the helper's typed boundary (e.g. an `expect`-on-`?` boundary) for error context.  Behaviour-preserving simplifications are wins; behaviour-changing simplifications are out-of-scope (they're correctness fixes that happen to shrink code, and they belong under emphasis A's bucket with a regression test).

### Emphasis B — simplification: extract, delete, merge, inline (in that order of leverage)

The brief is: **reduce total LOC AND reduce reader cognitive load** (the second condition matters because a one-line "clever" replacement of three obvious lines is a regression).  Every proposal must net out positive on both axes.

The simplification toolkit, ranked by leverage:

1. **Delete dead code.**  The biggest LOC win and the lowest risk.  Hunt for:
   - `pub` items with zero external consumers (verify by `grep` across the workspace AND across `strider-py`'s Python surface).  An `unused_must_use`-style audit, but for whole functions / structs / variants / trait impls / CC presets / `SleighArch` presets / pattern free constructors.
   - Test fixtures that no test in the workspace references.
   - `#[cfg(any())]`-disabled or commented-out code.
   - `// removed:` or `// formerly:` tombstone comments referencing symbols that have already been deleted.
   - `Default` impls / `Clone` derives / etc. that nothing constructs.
   - Re-exports for items that aren't reached from any external consumer.
   - `#[allow(dead_code)]` items whose declared "future use" never materialised.
   - `#[deprecated]` items past their migration window: if all callers are `#![allow(deprecated)]` test scaffolds, propose either deletion or a hard rename.

2. **Merge similar code.**  Includes both helper extraction AND structural merges:
   - Two opt passes that do nearly-the-same work merged into one with a config flag.
   - Two `NodeKind` variants that the lifter / opt / pattern paths always handle identically.
   - Two trait impls on the same type that overlap (e.g. `From<T>` and a `to_pat()` method that does the same thing).
   - Two error variants that always coincide.
   - Two test helpers in `tests/common/mod.rs` and `src/test_support.rs` that do the same job.

3. **Inline single-callsite helpers.**  The inverse of extraction.  When a `fn helper(...)` is called from exactly one place AND the helper's body is < 5 lines, inline it — the abstraction is paying no rent.  Especially common for:
   - `pub(super)` helpers in opt passes that only the parent module uses.
   - `fn build_X(...)` helpers in rule builders called from one rule each.
   - Any helper whose body is `self.field.foo()` or `(*self.inner).clone()`.

4. **Replace bespoke patterns with stdlib idioms.**  Often the original author didn't know about a stdlib feature.  Examples to scan for:
   - Manual `.iter().filter(|n| reachable.contains(n)).filter(|n| pred(...))` chains where `reachable.intersection(...)` or a single combined predicate is shorter (when the result is the same).
   - Hand-rolled `let Some(x) = opt else { return None; }` chains where `?` would suffice (caller's return type permitting).
   - `match x { Some(v) => v, None => return None }` → `x?`.
   - `.collect::<Vec<_>>().len()` → `.count()`.
   - `for item in vec.iter() { mem.push(item.clone()) }` → `mem.extend_from_slice(&vec)`.
   - Manual saturating-arithmetic patterns where `usize::saturating_add` / `i64::checked_add` apply.
   - `.unwrap_or_else(|_| f())` where the closure's only purpose is `f()` — use `.unwrap_or_else(f)` directly.

5. **Tighten visibility (fewer `pub` = smaller API surface = simpler).**  Verify each `pub` declaration's actual external consumer; if there's none, propose `pub(crate)` or `pub(super)`.  A smaller `pub` set is a smaller API contract — the codebase is simpler from the outside-in.

6. **Drop redundant wrappers.**  Newtypes that don't carry an invariant or a method are pure overhead.  If `pub struct Foo(Inner);` has no methods and the `Inner` type is already public, just expose `Inner` directly.

7. **Collapse partial-state types into proper sum types.**  When a `struct` has fields populated to "zero/None/empty" sentinels in some construction paths and to "real" values in others, that's a partial-state type — model it as `enum { Real { ... }, Empty }` instead.

Per-finding criteria (every proposal must answer all of these):

- **Net LOC delta.**  After applying the proposal, does the codebase have fewer lines?  A 1-LOC helper that replaces a 1-LOC pattern is net zero (skip).
- **Net cognitive delta.**  After the proposal, is the call site easier to read?  A `macro_rules!` that hides 4 lines but makes the call site `pure_pass_class!("Foo" => PyFoo)` is a win because the macro name is self-documenting; a `helper_X!()` that hides 4 lines but the call site is now `helper_X!(state, ctx, mode)` with no clarity about what's happening is a loss.
- **Three-occurrence threshold (for extraction).**  Patterns repeated ≥ 3 times, ≥ 3 LOC each are MED; ≥ 5 occurrences OR ≥ 6 LOC each are HIGH.  Two-occurrence repetition is a LOW finding.
- **Load-bearing-different vs accidentally-different (for extraction).**  When two repetitions look the same but produce different runtime behaviour, the difference is *load-bearing* — propose unifying only after explicitly noting why each variant exists.  If a candidate site has subtly different ordering, error handling, or ownership shape, document the difference and decide skip vs. unify per-case.
- **Don't force-fit `macro_rules!`.**  Use `macro_rules!` only for syntactic patterns that genuinely repeat.  For semantic repetition (data-flow patterns, walks, error-wrap-and-propagate), prefer a function or extension trait.  Macros lose IDE support, defeat type inference, and are read-once / write-many.
- **Visibility tightening as a side-effect.**  When a helper is extracted, the original implementation may have been over-public — propose the minimum visibility for the new helper.
- **Skip when the indirection cost exceeds the duplication cost.**  If a proposed helper is < 3 LOC AND the call sites are < 5, document the decision and skip.

The simplifications report should land at `reviews/round11-simplifications.md` with sections matching the toolkit above:

1. Code to delete (largest expected LOC win).
2. Code to merge (helper extractions + structural merges).
3. Single-callsite helpers to inline.
4. Bespoke patterns to replace with stdlib idioms.
5. Visibility to tighten.
6. Wrappers to drop.
7. Partial-state types to convert.

Aim for 50–80 concrete entries with a per-category total LOC delta at the top.

## Coverage requirement — every line of source must be inspected

This review is **exhaustive**, not sampled.  Every `.rs` file under `crates/*/src/`, every `.rs` file under `crates/*/tests/` and `crates/*/benches/`, every `.py` file under `crates/strider-py/tests/python/`, every `Cargo.toml`, every `*.md` file under `crates/*/README.md` + the root README + CLAUDE.md + `crates/strider/.claude/skills/*/SKILL.md`, must be **read in full** by at least one subagent during the rounds below.

Concretely:

- **Inventory first.**  The Round 0 orientation step must produce `reviews/round11-coverage-manifest.md` listing every file in scope (use `find crates -name '*.rs' -o -name '*.toml'` + `find crates/strider-py/tests -name '*.py'` + the doc set).  Every file in that manifest must be ticked off as "inspected by subagent X" by the time Round 7 (final consolidation) runs.
- **No globbing skips.**  If a file is short, glance through it and tick it.  If a file is long, read it in 200-line chunks until the whole file is covered.  When a subagent reports its findings it should also report which files it covered fully, partially, or not at all; partial / not-at-all entries become Round 1.5 follow-up tasks for a fresh subagent.
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
3. Be told explicitly: do NOT read `reviews/round7-*.md`, `reviews/round8-*.md`, `reviews/round9-*.md`, `reviews/round10-*.md`, or any earlier-round output.
4. Produce a single Markdown report at `reviews/round11-<topic>.md` with HIGH/MED/LOW findings, each with `file:line` and a concrete fix.
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

1. **Correctness — code vs code self-consistency** *(emphasis A axis 1)*.  For every operation implemented in 2+ sites, verify the implementations agree on every input.  Sites to start from (find the actual call sites by grep — these are *suggestions*, not an exhaustive list):
   - SP decomposition: every consumer of stack-pointer arithmetic in the opt and strider crates.
   - CallOther dispatch: cfg-time terminator classification AND IR-emission-time classification.
   - Varnode aliasing: read AND write in `pcode_lift::vn_io` (must be inverses for any matched pair).
   - Commutativity: pattern matcher's table AND the build-RHS path AND every per-op family in pattern ctor modules.
   - Lift-time canonicalisations: lifter's lowering AND the pattern crate's lowered aliases.
   - Asm-fingerprint contract: every opt pass that creates a node must absorb contributors via `extend_asm_fingerprint_from`.
   - Validator Layer A/B/C reachability scoping: every Layer-C check should follow the same scoping rule.

   Edge cases to specifically verify across paired sites: register aliasing for every supported width (1, 2, 4, 8, 10, 16, 32, 64); bounded-lift `is_addr_tail_call` half-open semantics; indirect branches for every shape the resolver claims to handle; memory-edge clobbering on `Call` / `Store` / `CallOther`; phi shapes (VarPhi / MemPhi / StackStorePhi / ValuePhi arity rules); NaN ordering on float compares; sign / zero extension; all eight lift-time canonicalisations (`IntSub`, `IntLessEqual`, `IntSlessEqual`, `IntNotEqual`, `FloatSub`, `FloatNotEqual`, `FloatLessEqual`, `FLOAT_NAN`); CallOther ABI classifications.

   Pin every divergence as a HIGH finding, even if it's documentation-only drift.

2. **Correctness — IR vs lifted representation** *(emphasis A axis 2)*.  Every `NodeKind` must precisely model the underlying pcode opcode.  Verify by lifting one real instruction per opcode family and comparing IR shape to rsleigh's documented semantics.  Cover: arithmetic (Add/Sub/Mul/Div/Mod, signed and unsigned), shifts (Shl/Shr/Sar), bit-ops (And/Or/Xor/Not), comparisons (Equal/Less/Sless and the lowered Le/Sle/Ne shapes), casts (Truncate/ZeroExtend/SignExtend, IntToFloat/FloatToInt/FloatToFloat, IntBitsToFloat/FloatBitsToInt), float arith (Add/Mul/Div, Sub-as-lowered, NaN-aware Less/Equal/LessEqual), memory (Load/Store with VnSpace), control (If/IndirectBranch/Call/CallOther/Return), phis (VarPhi/MemPhi/ValuePhi/StackStorePhi), wide constants (`IntConstWide` U256/U512), sub-register aliasing.  Sample at least 30 instruction-IR pairs.

3. **Correctness — lifted IR vs real assembly** *(emphasis A axis 3; deepest correctness check)*.  Per arch (x86, x86_64, ARM, ARM-BE, ARM-Thumb, AArch64, AArch64-BE, MIPS32-LE/BE, MIPS64-LE/BE, PPC32-LE/BE, PPC64-LE/BE), pick one representative function from `fixtures/out/<arch>/` and verify:
   - **Return-value flow.**  Lift functions returning `int`, `long`, `struct {int,int}`, `float`, `double`, and `__int128`.  Verify the IR's `Return` node's value inputs precisely match what the ABI says the caller reads (RAX/RDX, X0/X1, V0, R3/R4, F1, etc.) and the callee actually wrote.  Cross-check against the published ABI (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PPC ELF v1 / v2) AND against `objdump -d` of a real toolchain output.
   - **Clobber footprint.**  For each `Call` node, the IR's clobber output set must equal the caller-saved register set per the resolved CC.  Verify both the *positive* case (caller-saved regs ARE clobbered post-call) AND the *negative* case (callee-saved regs are NOT clobbered — their values flow through the Call unchanged).  Compare against real assembly: a function that reads a callee-saved reg before AND after a call must have the IR show the same `NodeOutputId` for both reads.
   - **Memory chain after a call.**  Calls advance the memory edge by default (modulo `no_memory_clobber`).  Verify that `LoadReadOnly` and `StackLoadForward` correctly stop forwarding across normal calls AND correctly forward across all-preserving calls.  Pick a real Linux-kernel binary with `__fentry__` instrumentation and verify the rodata loads after the hook still fold to constants.
   - **CallOther implicit reads / writes.**  For each entry in `target::call_other_abi::classify`, fetch the Intel SDM / ARM ARM / MIPS Reference / PowerPC ISA reference for the underlying instruction and verify `implicit_reads` covers every register the instruction reads and `implicit_writes` covers every register it writes.  **Sample at least 20 entries** — `cpuid`, `rdtsc`, `rdtscp`, `rdmsr`, `wrmsr`, `swapgs`, `wrgsbase`, `wrfsbase`, `mfence`, `sfence`, `lfence`, `cmpxchg16b`, `xsetbv`, `xgetbv`, `monitor`, `mwait`, `int 0x80`, `syscall`, `sysret`, `sysenter`, plus per-arch `swi` / SMCCC entries.
   - **Indirect-branch resolution against real targets.**  For each shape the resolver claims to handle (link-register return, jump-table, stack-array dispatch, tail-call, the `Truncate(IntConst)` / `Extend(IntConst)` arms), find a real binary exhibiting that shape and verify the resolved target set matches the symbol-table truth (`nm` / `addr2line`) for **at least three call sites per shape**.  The `#[ignore]`'d tests in `crates/strider/tests/indirect_branch.rs` document specific shapes; fact-check the documented fix paths against the actual lifter output.
   - **Lift-time canonicalisations are bit-exact.**  Verify each canonicalisation on **at least one real lifted instruction**, comparing the IR against a hand-derived expected pcode trace from rsleigh.  Special edge cases: `IntSub` on `(a, INT_MIN)` (modular negation), `FloatLessEqual` on NaN inputs, `FLOAT_NAN(x)` on signaling NaN.

   **For each finding under ask 3, the fix proposal must include a regression test that lifts a real instruction and asserts the resulting IR shape — fixture-based, not hand-built mock graphs.**

4. **Simplicity — exhaustive simplification sweep** *(emphasis B)*.  Apply the seven-category toolkit (delete dead code, merge similar code, inline single-callsite helpers, replace bespoke patterns with stdlib idioms, tighten visibility, drop redundant wrappers, collapse partial-state types).  For every entry, answer the per-finding criteria (net LOC delta, net cognitive delta, three-occurrence threshold for extraction, load-bearing distinction, indirection-cost veto).

5. **Naming.**  Look for unclear / misleading / half-renamed identifiers anywhere.  Verify that the meaning of every term in CLAUDE.md / per-crate README / SKILL.md matches the actual code.  Look for: leftover one-letter or short test names that only made sense in their original context, abbreviations whose expansion would be clearer, type names that don't capture the type's actual role, "round-N" / "wave-N" / "tier-N" / migration-narrative breadcrumbs in source comments or test names.  Propose a concrete rename mapping for every flagged identifier.

6. **Unused features.**  Confirm by code inspection that every `pub` item has at least one external consumer (within the workspace OR via Python bindings).  Items unreachable from any consumer are candidates for deletion.  Check both ways: walk down from `lib.rs`'s `pub use` re-exports to find every public surface, then walk back up from each surface to find at least one external call site.

7. **Python binding parity.**  Verify every IR / pattern / opt / target / strider feature accessible from Rust is also reachable from `strider-py`'s Python API, OR is documented as deliberately Rust-only (with rationale).  GIL handling correctness on every callback path.  Typed exception coverage on every fallible Python entry.

8. **Multiple rounds of correctness audit, rotating focus.**
   - **Round-1 pass (per-crate)**: typing / signature / arity errors — does the code do what its types say it does?
   - **Round-2 pass (re-audit, fresh subagent per crate, cannot read round-1's output)**: invariant-violation errors — does this code maintain the invariants its callers depend on?
   - **Round-3 pass (re-audit, fresh subagent per crate, cannot read round-1 or round-2)**: concurrency / aliasing / borrowing errors — for any code that holds a `&mut` while doing work that could re-enter or invalidate the borrow target, are the lifetimes provably sound?
   - **Round-4 pass (re-audit, focused on edge cases)**: boundary errors — empty / single / max-arity inputs, NaN / inf / signed-zero floats, INT_MIN sign-extension, address `u64::MAX`, instruction at `addr = start_addr` boundary, the very last node id in the arena, the lifetime-zero-overlap case for `StackStorePhi`.
   - **Round-5 pass (cross-arch consistency)**: pick one finding per round and verify it across every arch.

   Each pass produces its own subagent report (`reviews/round11-correctness-types.md`, `round11-correctness-invariants.md`, `round11-correctness-borrowing.md`, `round11-correctness-edge-cases.md`, `round11-correctness-cross-arch.md`).

9. **Test plan.**  Where coverage is sparse, propose specific tests with file path, scope (unit / integration / property / scale), exact harness/fixture, expected assertions.  Use TDD discipline — failing test FIRST, then fix.

10. **Stale comments.**  Verify every `pub` item's docstring matches the actual code.  Hunt for `TODO`s linked to closed work, references to deleted symbols, half-rename leftovers, comments that describe behaviour the surrounding code doesn't implement, and any "round-N wave-N" / "R1-..." migration breadcrumbs that have outlived their context.

11. **Production panics.**  No `panic!` / `unwrap()` / `expect()` / `unreachable!()` / `assert!()` in non-test code paths.  Audit every occurrence; if not justified by a by-construction invariant, propose `Result` propagation.  Annotate justified ones with `#[allow(clippy::expect_used)]` and a code comment naming the invariant.

12. **CLAUDE.md / READMEs / doc consistency.**  Verify the root README's claims, every per-crate README's public-surface enumeration, every `pub fn` doc.  Documentation that names deleted symbols, references stale APIs, or describes behaviour the code doesn't implement is a HIGH finding.

13. **Skills.**  Skim `crates/strider/.claude/skills/*/SKILL.md`.  Verify each cited file path, function name, and line number against the current code.  Identify any new skill that would help future contributors (or any existing skill that has decayed against the current code).

14. **Scale + Performance at thousands-of-nodes scale.**  Verify the codebase is *optimised* for ~10k–100k IR nodes, not merely correct.  Per hot path, derive the asymptotic complexity from the code (don't trust comments), then check:
    - **Recursion-induced stack-overflow risk** in any function that walks a memory or control chain without an explicit depth bound and without an iterative form.
    - **Allocation per call.**  `Vec::new()` / `HashMap::new()` / `Box::new(...)` inside loops over reachable nodes.  Even when the inner Vec is small, M iterations × per-call allocator hit dominates real-world cost.  Propose `SmallVec<[_; N]>`, reused scratch buffers, or pre-sized-with-capacity allocations.
    - **Hash sets / maps over `NodeId` / `NodeOutputId` / `NodeInputId`.**  Replace with `entity_utils::DenseEntitySet` / `cranelift_entity::SecondaryMap` where the entity is dense — the bit-vector / array shape skips the FxHash entirely and gets cache-local indexing.  No remaining `FxHashSet<NodeId>` / `FxHashMap<NodeOutputId, _>` should exist in hot paths.
    - **Repeated full-graph scans inside fixed-point loops.**  Anything in `OptimizerPipeline::run`'s loop body or `strider::orchestrator::step` that walks `graph.all_node_ids()` is O(N) per iteration.  Identify scans that can be cached / amortised across iterations or replaced with a worklist.
    - **`HashMap` keys with heap allocations.**  Verify hot HashMaps don't unnecessarily clone keys on insert.
    - **`Match::find_all_requirements` cross-product blowup.**  For shared-capture queries with M patterns each matching N nodes, the worst case is O(N^M).  Verify whether better pruning (sort by pattern selectivity, prune at first disagreement instead of full-scan) is feasible without breaking the binding-agreement contract.
    - **Worst-case wall-clock budget.**  For a synthetic 10k-node graph through `default_pipeline()`, the audit must produce a measured P50 / P95 from any existing benchmark harness.  Any pass exceeding 100 ms / 10k nodes flags as a performance hot-spot.
    - **Memory residency.**  After `orchestrator::run` finishes, what's still alive?  Side-tables (`asm_fingerprints`, `wide_consts`, `call_other_names`, `stack_phi_offsets`, `call_clobbered_override`) keyed on detached-zombie `NodeId`s are wasted bytes.  Verify `compact()` is called when expected and that all side-tables are GC'd by it.
    - **Python GIL hold-time on long lifts.**  Verify the callback-reader path (where the inner reader re-acquires GIL per read) doesn't ping-pong the GIL excessively for I/O-intensive workloads.

15. **Type design.**  Audit every `pub` struct / enum / trait for: leaky encapsulation (public fields where invariants exist), primitive obsession (`u32` where a newtype would express intent), types that fail to express their invariants, partial-state types (struct with all fields populated to "valid" sentinels rather than `Option<_>` or a sum type for the genuine states).

16. **Silent failures.**  Hunt every `unwrap_or` / `unwrap_or_default` / `if let Ok` followed by ignore-Err / `.ok()?` / `match … _ => return None` swallowing real errors.  Distinguish "intentional fallback for a documented optional path" from "swallowed bug".

17. **Helpers / generalisation** *(emphasis B; see top-level criteria above)*.  Identify recurring patterns across the codebase (graph traversal, error-wrapping boilerplate, opt-pass scaffolding, side-table mutations, test-fixture builders).  Apply the three-occurrence threshold + load-bearing-different vs. accidentally-different distinction.  For each accepted candidate, propose the helper signature, location, and migration shape at every call site.

18. **Build / lint / test baseline.**  At Round 0 and at the end of Round 7, the workspace must satisfy:
    - `cargo build --workspace --all-targets` clean.
    - `cargo clippy --workspace --all-targets -- -D warnings` clean.
    - `cargo test --workspace` all passing.
    - `cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/` all passing.

    Surface any drift in the Round 0 report; surface any regression in the Round 7 final summary.

## Recommended round structure

### Round 0 — orient
Read CLAUDE.md, the workspace `Cargo.toml`, the per-crate `Cargo.toml` files, every per-crate README, every existing `crates/strider/.claude/skills/*/SKILL.md`.  Build a mental model of what's in each crate.  Run the four baseline checks above.  Produce `reviews/round11-coverage-manifest.md` listing every file in scope.

### Round 1 — deep per-crate audit (parallel; 6 subagents)
Six `feature-dev:code-reviewer` agents, one per crate group:

| # | Crates | Special focus |
|---|--------|---------------|
| 1A | `ir` | Graph dedup correctness; asm-fingerprint contract (super-set + union on cache hits, no shrink); validate Layer A/B/C reachability scoping; FunctionBuilder lift-addr funnel; node_signature panic sites; partial-state `BuiltFunctionGraph` constructors; `KnownBitsMap` / `WideConstStorage` / `IntConstWide` correctness; wide-constant rejection guards |
| 1B | `pcode-lift` + `cfg` | `vn_io` register-aliasing for *every* supported width (1/2/4/8/10/16/32/64); sub-register partial-write semantics; CondBranch single-OOB-successor edge; bounded-lift `is_addr_tail_call`; CallOther classification dispatch; cfg `Region` semantics (empty regions, contains_addr, split_region edge cases); resolver-arm peeling for Truncate(IntConst) / Extend(IntConst) |
| 1C | `opt` | Each pass: rewrite + no-op + idempotency + ordering interactions; `FlagCmpCanonicalize` correctness; `IfCondInversion` canonicalization invariant; `KnownBits` soundness vs unknown-bit truncations + wide-input bails; `StackStoreDetect` SP-decompose-and-chains; `StackLoadForward` partial-overlap endianness + StackStorePhi disjoint-offset arm; `LoadReadOnly` bounds; `RedundantPhis` + `DeadBranchElimination` interaction with detached zombies; `indirect_branch_resolve` classifier shape coverage; `function_args` Result discipline + fingerprint propagation |
| 1D | `pattern` | Commutativity tables (which ops commute), capture binding agreement across patterns, find_all_requirements cross-product correctness, `*_any` set-membership empty-set vacuous failure, `.when()` predicate scope, lift-time canonicalization aliases (`sub`, `int_le`, `int_sle`, `float_*`) match what IR actually emits, `Match::asm_fingerprint`/`stack_offset`/`stack_phi_offsets` accessor correctness, GuardPat semantics, RewriteCtx / RewriteCtxView field-tightening tradeoffs |
| 1E | `strider` + `target` + `reader` | Orchestrator fixed-point convergence + monotonicity, indirect-resolve `Decision { FixedPoint, StableOnly, Rebuild }`, `LoopState::step` stall-guard ordering, `locate_spliced_call` ControlState chain walking, `GraphRewriter::re_optimize`, target `CallingConvention` varnode resolution per arch (verify register names exist in the corresponding sleigh spec), `apply_elf_relocations_autoload` correctness + rollback on Err, MIPS / PPC RELATIVE / GLOB_DAT / JUMP_SLOT arms, `RelocationStats` programmatic surface, per-arch test_utils wrapper coverage, `Builder::for_arch` migration completeness |
| 1F | `strider-py` + `dot` + `graphwalk` + `entity-utils` | PyO3 boundary error mapping (every Rust error → typed Python exception, no panic-to-abort); str-keyed capture interning correctness across borrows; unsafe blocks; Python tests calling unsupported features; graphwalk traversal termination + `ControlFlow<()>` ergonomics; entity-utils `DenseEntitySet`/`Worklist` invariants + `test_and_set` clarity at call sites; `multiple-pymethods` macro behaviour; GIL release in `strider.run`; KeyboardInterrupt/SystemExit propagation through every Python callback; public API snapshot completeness |

### Round 2 — cross-cutting passes (parallel; 4 subagents)
- **2A — production-panic hunt.**  `feature-dev:code-reviewer`.  Walk every `unwrap()` / `expect()` / `panic!()` / `unreachable!()` / `assert!()` outside `#[cfg(test)]` / `tests/` / `examples/` / `benches/`.  For each: justified by a by-construction invariant or unjustified?
- **2B — naming sweep.**  List every occurrence of unclear / misleading / half-rename leftover identifiers, including any "round-N" / "wave-N" / "tier-N" / "R10-..." migration breadcrumbs in source.
- **2C — silent-failure hunt.**  `pr-review-toolkit:silent-failure-hunter`.  Look for `unwrap_or`, `unwrap_or_default`, `.ok()?`, `if let Ok` followed by ignore-Err.
- **2D — type-design analyser.**  `pr-review-toolkit:type-design-analyzer`.  Audit every `pub` struct / enum / trait for: leaky encapsulation, primitive obsession, partial-state types.

### Round 3 — verification + comments (parallel; 2 subagents)
- **3A — trust-only-the-code verification.**  Sample ≥ 25 specific claims from CLAUDE.md, per-crate READMEs, and every existing `SKILL.md`.  For each, find the code and confirm or refute purely from code shape.
- **3B — stale comment sweep.**  `pr-review-toolkit:comment-analyzer`.  Flag every comment block that names a deleted symbol, has a `TODO(TaskNN)` whose task is closed, describes behaviour that doesn't match the surrounding code, or carries a "round-N" / migration-narrative breadcrumb that has outlived its context.

### Round 4 — test-gap analysis
`feature-dev:code-architect`.  Consume Round 1 outputs.  Emit `reviews/round11-test-plan.md` listing each missing test with: scope, file path, harness/fixture, expected assertions, estimated effort.  Use TDD discipline — failing test FIRST.

Required gaps to investigate (not exhaustive):

- **Asm-fingerprint dedup-union**: a node created twice from different machine addrs must merge fingerprints.
- **Asm-fingerprint shrink-prevention**: invariant test that no pass produces a node whose fingerprint is a strict subset of any contributor's.
- **vn_io sub-register partial-write where parent is phi-live** (e.g. AL written in one predecessor, RAX flowing in unmodified from the other).
- **cfg `RegionBuilder::build` bounded-lift terminator on OOB cur_addr.**
- **Single-instruction-CondBranch-with-one-OOB-successor edge case.**
- **Stack-array dispatch resolver shape end-to-end** for each arch where the lifter produces it.
- **pattern `find_all_requirements` shared-capture-disagreement filtering.**
- **pattern `int_const_any_of([])` / `at_any([])` / `offset_any([])` empty-set vacuous-failure tests.**
- **Multi-output `Match::output(c)` selection rules** (which output binds when the matched node has 2+ value outputs?).
- **ARM/AArch64/MIPS end-to-end ELF fixture coverage** for any currently-`#[ignore]`'d shapes in `crates/strider/tests/indirect_branch.rs`.
- **CallOther ABI dispatch coverage matrix** — per arch + per opcode (verify the ≥ 20 entries from ask 3 each have at least one fixture-based test that lifts the relevant instruction).
- **PyO3 every typed exception** (`StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`, `UnresolvedIndirectBranchError`, `UnknownCallOtherError`) raised by an end-to-end Python test.
- **Iterative-form regression for any recursive memory-chain walk**: chain of 1k+ stores must not stack-overflow.
- **Per-arch `strider_for_arch` wrappers** — one smoke test per variant to keep them in sync with their corresponding CC presets.
- **Bit-exact lift-time canonicalisations** (per ask 3): one regression per canonicalisation pinning the IR shape against a real lifted instruction, not a hand-built mock.
- **Cross-arch CallOther round-trip via `strider::run`** — at minimum one ARM `swi`, one AArch64 SMCCC, one x86_64 syscall lifted end-to-end.
- **Asm-fingerprint contract end-to-end**: lift a real binary, walk every reachable non-exempt node, assert each has at least one fingerprint entry tracing to a real machine address.

### Round 5 — consolidation + simplification
`pr-review-toolkit:code-simplifier`.  Consume Rounds 1–3.  Emit `reviews/round11-simplifications.md` per emphasis B's seven-category toolkit:

1. Code to delete.
2. Code to merge.
3. Single-callsite helpers to inline.
4. Bespoke patterns to replace with stdlib idioms.
5. Visibility to tighten.
6. Wrappers to drop.
7. Partial-state types to convert.

50–80 entries with per-category LOC delta + projected post-implementation workspace LOC reduction at the top.

### Round 6 — skill audit
Skim every existing skill against the current code.  Propose new skills or revisions.  Verify every cited file path, function name, and line number.

### Round 7 — final consolidation
A single synthesis that integrates every prior-round output into:

1. `reviews/round11-summary.md` — executive summary with prioritised fix backlog.
2. `reviews/round11-claudemd-diff.md` — concrete CLAUDE.md edits.
3. `reviews/round11-readme-diffs.md` — concrete per-crate README edits.

## Acceptance criteria

- [ ] Every numbered ask (1–18) has a corresponding section in `reviews/round11-summary.md` with concrete actions.
- [ ] Emphasis A produces three reports: `reviews/round11-correctness-self-vs-self.md`, `reviews/round11-correctness-ir-vs-pcode.md`, `reviews/round11-correctness-ir-vs-assembly.md`.  Each finding cites code locations + ABI / opcode references + (for axis 3) the real binary's `objdump` trace.
- [ ] Emphasis B produces `reviews/round11-simplifications.md` with 50–80 entries grouped into the seven-category toolkit.  Header includes the per-category total LOC delta and the projected post-implementation workspace LOC reduction.
- [ ] Ask 8 produces five separate reports — `round11-correctness-types.md`, `round11-correctness-invariants.md`, `round11-correctness-borrowing.md`, `round11-correctness-edge-cases.md`, `round11-correctness-cross-arch.md` — each from a fresh subagent that did not read the others.
- [ ] Round 11 summary lists every HIGH-severity finding with `file:line` + a proposed fix + (for emphasis A findings) the regression test scaffolding.
- [ ] Ask 14 produces a measured P50 / P95 table per pass over a synthetic 10k-node fixture; flagged hot-spots (> 100 ms) listed with proposed fixes.
- [ ] Ask 3 produces `reviews/round11-correctness-ir-vs-assembly.md` documenting per-arch ABI verification against real binaries.  Every finding includes the lifted-from-real-code regression test that pins it.
- [ ] CLAUDE.md correctness diff lists every drift from the current code.
- [ ] Per-crate README diff lists drift in every crate that has one.
- [ ] Test plan lists ≥ 15 missing tests with exact `file:line` scaffolding.
- [ ] Naming sweep produces a concrete rename mapping (every flagged identifier has a target name).
- [ ] Production-panic audit lists every unjustified `unwrap`/`expect`/`panic!` with proposed `Result` plumbing.
- [ ] Type-design audit produces a list of `pub` struct/enum candidates for visibility tightening or newtype wrapping.
- [ ] Silent-failure audit produces a list of `.ok()?` / `unwrap_or` sites with a propose-or-document decision per site.
- [ ] Skill audit produces a list of revisions or new-skill proposals.
- [ ] `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `pytest tests/python/` all green at the start AND end of the review.
- [ ] No source code is edited during the review.  The output is the set of `reviews/round11-*.md` reports.  Implementation is a follow-up task that the user will explicitly approve.
- [ ] `reviews/round11-coverage-manifest.md` exists and shows every `.rs` / `.py` / `Cargo.toml` file under `crates/` ticked off as "inspected fully" by at least one subagent.

## Out of scope

- **Editing source code.**  Only documentation under `reviews/round11-*.md` is allowed.
- **Authoring new tests.**  Only the test plan is in scope; writing the actual tests is follow-up.
- **Authoring new skills.**  Only the skill design is in scope.
- **Per-crate README rewrites.**  Only the diff is in scope.

## Critical files to consult

These are the surfaces most worth starting from — but they are **not exhaustive**.  The coverage requirement above demands every `.rs` / `.py` / `Cargo.toml` under `crates/` be inspected.

- IR core: `crates/ir/src/{lib,graph/{mod,store,access,compact},function,validate/{mod,layer_a,layer_b,layer_c},walk,node_signature,builder/{mod,call,nodes,vars},wide_const,ops/{mod,builder,consts,op_kinds,rewrite}}.rs`
- Lifter: `crates/pcode-lift/src/{lib,vn_io,value/{mod,arithmetic,boolean,cast,float,integer,mem_load,misc_value}}.rs`
- CFG: `crates/cfg/src/cfg/{builder/{mod,region_builder,split,indirect_resolve},types,decode_cache,query,options}.rs`
- Optimizer: every `crates/opt/src/<pass>/mod.rs`, plus `pipeline.rs`, `sp_expr.rs`, `worklist.rs`, `test_support.rs`, `indirect_branch_resolve/{mod,jump_table,stack_array,classify,inplace}.rs`, `stack_load_forward/mod.rs`, `function_args/mod.rs`, `flag_cmp_canonicalize/mod.rs`, `if_cond_inversion/mod.rs`, `known_bits/mod.rs`, `stack_store/{mod,detect}.rs`
- Pattern: `crates/pattern/src/{lib,rewrite,error,matcher/{mod,bindings,match_result,walk,walk_through,function_arg_handle,commutativity,cast_mask},pat/{mod,traits,node_pat,any,guards,builders/*,ctor/*}}.rs`
- Strider: `crates/strider/src/{lib,errors,orchestrator,rewrite,test_utils,indirect_resolve/{mod,classify,inplace},strider/{mod,pipeline,vn_io,insn/{mod,control}}}.rs`
- Target: `crates/target/src/{lib,arch,call_other_abi,calling_convention/mod}.rs` cross-checked against `../rsleigh/sleigh/src/**`
- Reader: `crates/reader/src/{lib,elf}.rs`
- PyO3: `crates/strider-py/src/{lib,errors,pattern,graph,opt,reader,arch,cc,run,strider_cls,sleigh,cfg,matcher,dot}.rs`
- Skills: `crates/strider/.claude/skills/*/SKILL.md`
- Tests: every `crates/*/tests/*.rs`, every `crates/strider-py/tests/python/*.py`, any `#[ignore]`'d tests in `crates/strider/tests/indirect_branch.rs` (each ignore-reason names a specific resolver gap)

After working through the anchor list, every subagent must continue through the rest of its assigned crate's source until the coverage manifest's tick-list is complete for that crate — including `lib.rs`, every sub-module, every `tests/` and `benches/` file.

## Verification

The review is itself a research effort — the "verification" is the quality of the final summary, not a build.  The acceptance criteria above are the bar.  At the end, leave a short note in `reviews/round11-summary.md` describing:

- Total HIGH / MED / LOW finding counts.
- For emphasis A: the count of findings per axis (code-vs-code, IR-vs-pcode, IR-vs-assembly).
- For emphasis B: per-category total LOC delta (delete / merge / inline / stdlib / visibility / wrappers / partial-state), the workspace's pre-review LOC, the projected post-implementation LOC, and the per-category cognitive-load assessment (subjective, but called out for the top 5 entries per category).
- Which subagents found load-bearing items the others missed (the multi-round signal).

=== END PROMPT ===

---

## Notes for the user

- The prompt explicitly forbids reading `reviews/round7-*.md` through `reviews/round10-*.md` so the next round derives findings independently.  The previous rounds' outputs stay on disk as historical context.
- The two emphases (A: triangulated correctness, B: exhaustive simplification) override conflicting per-ask priorities.  The subagent prompts inherit that ordering.
- The prompt is sized to drive ~4–5 hours of subagent work plus ~1–2 hours of consolidation.  Approve tool prompts as they appear.
- After the review lands, you'll have the `reviews/round11-*.md` set — at that point a follow-up prompt of the form "land everything not refuted from round 11" runs the same play we ran for rounds 7–10.
