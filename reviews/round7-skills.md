# Round 7 — Strider Skill Bundle Design

A bundle of Claude Code skills tailored to common strider development workflows.
This document is a *design* (not the skill bodies); each section specifies the
eight required fields for one skill so the skills can be authored next.

> **Trust note:** designed without reading any `*-r6.md` review.  Background
> taken from `/CLAUDE.md`, `crates/**/src/**`, and `reviews/round7-*.md`.

---

## Summary table

| # | Skill name | Primary trigger | Files touched | Verify command |
|---|------------|-----------------|----------------|----------------|
| 1 | `strider-pattern-author` | "write a pattern that matches X" | `crates/<consumer>/src/**.rs`, sometimes `crates/strider-py/tests/python/test_pattern_*.py` | `cargo test --package <consumer>` (or `pytest crates/strider-py/tests/python/test_pattern_*.py`) |
| 2 | `strider-debug-pattern` | "this pattern doesn't match", "match returns empty" | `cfg.html` / `graph-opt.html` dumps; pattern source | manual: rerun the failing test; then `cargo test --package <consumer> <name> -- --nocapture` |
| 3 | `strider-opt-pass-author` | "add a new opt pass", "rewrite NodeKind X into Y" | `crates/opt/src/<pass>/{mod.rs,tests.rs}`, `crates/opt/src/lib.rs`, `crates/opt/src/pipeline.rs` (if pipeline registration), `crates/strider-py/src/opt.rs` (parity) | `cargo test --package opt --package strider --package strider-py` |
| 4 | `strider-fingerprint-audit` | "asm-fingerprint propagation", "validate fingerprints" | new pass / rewrite call sites | `cargo test --package ir asm_fingerprint` and `cargo test --package opt -- --include-ignored` |
| 5 | `strider-indirect-shape-author` | "resolve <new dispatch shape>", "BranchIndirect placeholder X is unresolved" | `crates/opt/src/indirect_branch_resolve/{classify.rs,inplace.rs,<new_shape>.rs}`, `crates/strider/src/indirect_resolve/`, fixture binary under `fixtures/` | `cargo test --package opt indirect_branch_resolve` and `cargo test --package strider` |
| 6 | `strider-callother-abi` | "add CallOther ABI for <op>", `UnknownCallOtherError` | `crates/target/src/call_other_abi.rs`, optionally `fixtures/` | `cargo test --package target call_other` and the failing `cargo test --package strider <fixture>` |
| 7 | `strider-target-arch` | "add support for <arch>", "register a new SleighArch" | `crates/target/src/arch.rs`, `crates/target/src/calling_convention/mod.rs`, `crates/strider-py/src/{arch.rs,cc.rs}`, possibly `crates/pcode-lift/src/vn_io.rs` if novel reg shapes | `cargo test --package target` and `pytest crates/strider-py/tests/python/test_arch.py` |
| 8 | `strider-py-binding` | "expose X to Python", "add a Python binding for Y" | `crates/strider-py/src/<module>.rs`, `crates/strider-py/src/lib.rs`, `crates/strider-py/tests/python/test_<feature>.py`, possibly `crates/strider-py/src/errors.rs` | `uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_<feature>.py` |

---

## 1. `strider-pattern-author`

### Trigger phrases
- "Write a pattern that matches a `malloc` call with `size > 1024`."
- "Match a memory read at `SP+offset` where the offset is a constant."
- "Match a return value flowing out of a call to function `0x401000`."
- "Find every `If(...)` whose condition is `x & 1 == 0`."

### When NOT to use
- The user already has a working pattern and wants to debug *why it doesn't match* — route to `strider-debug-pattern`.
- The user is rewriting the IR (replacing matched nodes with fresh ones) — that is `pattern::rewrite_rule` territory, covered by `strider-opt-pass-author`.
- The request is about Python ergonomics on top of an existing Rust pattern — route to `strider-py-binding`.

### Inputs
- Asm shape the user wants to match (snippet, opcode mnemonic, or natural-language description).
- Target arch (commutativity differs only at the IR level, but arch determines what `Vn`s appear).
- Whether the pattern runs on optimised IR (default) or pre-opt IR (rare; affects `IfCondInversion`/canonicalisation assumptions).
- Whether it is a Rust pattern (`crates/pattern`) or Python pattern (`crates/strider-py/src/pattern.rs`).

### Procedure
1. **Pick the root builder** by IR root kind:
   - `CallPat` — call sites; chain `.at(addr)` / `.at_any([…])` for direct calls, `.target(p)` for indirect, `.arg(idx, p)` for argument matching.
   - `RetPat` — function returns; `.preceded_by(call_pat)` to anchor to the preceding direct ctrl predecessor (typically `ControlState`); `.ret_val(idx, p)` for value matching.
   - `LoadPat` / `StorePat` — generic memory ops; bind `.space(s)` and `.addr(p)`.
   - `StackStorePat` / `StackStorePhiPat` — only after `StackStoreDetect` has run; use `.offset(K)` or `.offset_any({…})` (set-membership AND-combines with `.offset(K)`).
   - `IfPat` — branch decisions; `.cond(p)`, `.true_branch(p)`, `.false_branch(p)`. **Canonical direct layout only** — `IfCondInversion` must have run.
   - `PhiPat` / `phi_for(vn)` — currently matches `VarPhi` only; for `MemPhi` / `ValuePhi` use a raw `Pat` until those ctors land (see `round7-pattern.md` issue #2).
   - Sub-pattern leaves: `int_const(n)`, `signed_int_const(n)`, `int_const_any_of([…])`, `var(c)`, `any()`, `predicate(f)`.
2. **Use lift-time-canonicalisation aliases** — these match the *lowered* IR, not the source-level op:
   - `sub(a,b)` → `Add(a, Neg(b))`.
   - `int_le(a,b)` / `int_sle(a,b)` → `BoolNeg(IntLess(b,a))` (operand swap is intentional).
   - `float_sub(a,b)`, `float_ne(a,b)`, `float_le(a,b)` — see CLAUDE.md table.
   - **Do not** write `IntCmpOp::NotEqual` / `IntCmpOp::LessEqual` / `IntCmpOp::SlessEqual` / `IntCmpOp::Borrow` / `FloatCmpOp::NotEqual` / `FloatCmpOp::LessEqual` / `IntBinaryOp::Sub` / `FloatBinaryOp::Sub`. None exist as IR primitives.
3. **Bind captures** consistently — same `Capture` reused across patterns means "must bind to the same node and value-output". For Python, prefer the str-keyed form (`add("x", "x")`) — strings intern globally.
4. **Choose a matcher entry point**:
   - Single pattern: `Matcher::find_all(&pat)`.
   - N patterns over one preorder walk, no shared captures: `find_all_multi`.
   - N patterns with shared captures (cross-pattern join): `find_all_requirements` — runs the cross-product filter that enforces shared-capture binding agreement.
5. **Add `.when(predicate)` guards** for value-class conditions (e.g. `size > 1024`) using typed extractors (`m.get_uint(c, &graph)`, `m.get_int(c, &graph)`, etc.). `.when` runs AFTER structural matching.
6. **Capture commutativity caveats** — `add` / `mul` / `and` / `or` / `xor` / `IntCmpOp::{Equal,Carry,Scarry}` / `FloatCmpOp::Equal` and bool equivalents try both orders automatically. To force LTR, switch to the typed dispatcher `int_binary("Add", a, b).ordered()`. Free ctors don't accept `.ordered()` — see `round7-pattern.md` issue #4.

### Verification step
- Rust: write a unit test in the consumer crate with a small fixture graph (`graphmock`) or a real lifted ELF in `fixtures/out/`. Run:
  - `cargo test --package <consumer> <test_name>`
- Python: add to `crates/strider-py/tests/python/test_pattern_<topic>.py` and run:
  - `uv run pytest crates/strider-py/tests/python/test_pattern_<topic>.py -k <name>`

### Exit criteria
- The new pattern matches at least one lifted graph from `fixtures/`.
- A negative test asserts it does *not* match a control case.
- `cargo clippy --workspace -- -D warnings` clean.

### Pitfalls / footguns
- **`IfCondInversion`** must have run. `IfPat` is direct-layout-only — patterns over un-optimised IR will silently miss the inverted shape.
- **`IntCmpOp::Equal` / `Carry` / `Scarry` are commutative** — don't assume slot 0 vs slot 1.
- **`int_le(a,b)` swaps operands** internally to `BoolNeg(IntLess(b,a))` — when adding extra captures around the operands, place them on the *original* `a`/`b` argument positions; the alias does the swap for you.
- **`PhiPat` only matches `VarPhi`** today. For `MemPhi` / `ValuePhi`, build the `Pat::Phi(vn)` shape manually until ctors land.
- **Python `PyPat.ordered()` on a free-ctor result is a no-op** (round7-pattern issue #4).  Use `int_binary("Add", a, b).ordered()` for ordered matching.

---

## 2. `strider-debug-pattern`

### Trigger phrases
- "My pattern returns zero matches but the asm clearly has the shape."
- "`find_all` returns empty for what should be a `Call(at=0x401000)`."
- "Pattern works on one fixture but not another."

### When NOT to use
- The pattern has never been written — go via `strider-pattern-author`.
- The error is a panic / `PatternError` raise — most of those are syntactic; route to `strider-py-binding` if it's a Python wrapping issue.

### Inputs
- The failing pattern source.
- The fixture binary or `BuiltFunctionGraph` HTML dump (`graph-opt.html`).
- Optionally, an `asm_fingerprint` value for a node the user *expected* to be matched.

### Procedure
1. **Dump the IR**. From the failing test, add a one-liner: `std::fs::write("/tmp/dump.html", graph.to_html(...))?;` (Rust) or `graph.to_html("/tmp/dump.html")` (Python). Open the HTML; the dark/light DOT output is in `crates/dot`.
2. **Decide which IR layer the pattern is querying**:
   - Pre-opt (raw lift) — none of `ConstantFold`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`, `FlagCmpCanonicalize` have run.
   - Stable-default — only the indirect-fixedpoint-stable subset has run (`ConstantFold` + `KnownBits` + `IfCondInversion`).
   - Default (post-orchestrator) — full pipeline, including destructive passes; this is the typical query target.
3. **Re-check lift-time canonicalisation**:
   - Looking for `Sub`? The IR has only `Add(_, Neg(_))`.
   - Looking for `If(BoolNeg(C))`? `IfCondInversion` removed it; rewrite as `If(C)` with branches swapped.
   - Looking for `LessEqual` / `NotEqual`? Use the `int_le` / `float_ne` aliases (compositions, not primitives).
4. **Check `StackStoreDetect`** classification: bare `Store(SP+K, …)` won't match `StackStorePat` until the pass runs. Run the full default pipeline (or at minimum `StackStoreDetect`) before querying.
5. **Use the `asm_fingerprint`** to cross-reference: take the captured node's fingerprint via `m.asm_fingerprint(c, &graph)` and confirm it includes the expected machine address. An *empty* fingerprint on a non-exempt node means the lifter or a pass dropped the contract — open `strider-fingerprint-audit`.
6. **Check capture sharing**: in `find_all_requirements`, a shared `Capture` between patterns must bind to identical `(NodeId, NodeOutputId)`. If you see "matches individually but no joined match", it's a binding mismatch.
7. **Symmetric / commutative matching trap**: typed builders default to commutative for the listed commutative ops (round7-pattern issue C). Free `add`/`mul`/`and`/`or`/`xor` ctors are also commutative. To force ordering, use the typed `int_binary(...).ordered()` dispatcher.

### Verification step
- After fix, rerun the failing test:
  - `cargo test --package <consumer> <test_name> -- --nocapture`
  - or `uv run pytest <path>::<test> -v`

### Exit criteria
- The originally-failing pattern matches the expected node(s).
- A new regression test pins the shape so it can't silently regress.

### Pitfalls / footguns
- "Just disable the optimiser" — usually wrong; consumers run on optimised IR.
- Rebuilding the HTML dump after every edit is faster than reasoning about the graph in your head.
- Forgetting to apply the same arch / CC settings the production code uses (commutativity is the same, but stack-store classification depends on the CC's stack-pointer Vn).

---

## 3. `strider-opt-pass-author`

### Trigger phrases
- "Add an optimisation pass that folds `(x << K) >> K` into a sign-extend."
- "I want a pass that rewrites NodeKind A into B."
- "Scaffold a new pass under `crates/opt/src/`."

### When NOT to use
- The rewrite is a single pattern with a single substitute — `pattern::rewrite_rule` (and `strider::GraphRewriter`) is simpler than a full pass.
- The user is fixing a bug in an existing pass — go via `systematic-debugging` instead.

### Inputs
- A description of the rewrite (input shape → output shape).
- Whether the pass is **stable** (rewrites that survive new phi inputs in a later strider iteration), **destructive** (node removal — only safe at fixed point), or a **post-pass** (runs once after convergence).
- Whether the rewrite needs the calling convention or ROM image (e.g. needs SP, endianness, or `.rodata`).

### Procedure
1. **Decide the trait impl**: `Optimizer` for graph-level passes; `OptimizerOnBuilt` if you operate on a `BuiltFunctionGraph`. Look at `if_cond_inversion::IfCondInversion` (`OptimizerOnBuilt`) and `constant_fold::ConstantFold` (`Optimizer`) for templates.
2. **Create file layout**: `crates/opt/src/<pass>/{mod.rs,tests.rs}` — match neighbouring passes (`if_cond_inversion`, `redundant_phis`).
3. **Register the type** in `crates/opt/src/lib.rs`:
   - Add `mod <pass>;`
   - Add `pub use <pass>::<Type>;`
4. **Pick a pipeline placement** in `crates/opt/src/pipeline.rs`:
   - `default_pipeline()` — runs in the strider top-level fixed point.
   - `stable_default_pipeline()` — must NOT remove `VarPhi`/`MemPhi`/`ControlState`/`If` nodes that the orchestrator's `RegionIndex` pins.
   - `destructive_default_pipeline()` — runs once at fixed-point exit.
   - `add_post_pass` — runs once after convergence (e.g. `CallStackArgCollect`, `FunctionArgDetect`).
5. **Implement the rewrite**:
   - Prefer `pattern::rewrite_rule` for matchable subtrees with a single output replacement.
   - Hand-write surgery only when you need use-list edits the rewrite engine can't express (e.g. branch swap in `IfCondInversion`).
   - **Idempotency**: the pass must be a no-op when applied to its own output (the fixed-point loop will call it repeatedly). Test this directly.
   - **Phi-input contract**: pre-existing phi nodes with new predecessors are added in later strider iterations. Stable passes must not delete a phi or `ControlState` whose `NodeId` the orchestrator tracks.
6. **Asm-fingerprint propagation** (REQUIRED for any node creation):
   - Every newly-created `NodeId` that replaces or derives from an existing node MUST inherit the contributors via `Graph::extend_asm_fingerprint_from(new, contributor)`.
   - The contract is *superset-only*: never shrink, never replace with a node whose fingerprint is a strict subset of an ancestor's.
   - If the rewrite has multiple contributors, call `extend_asm_fingerprint_from` once per contributor.
   - See `crates/opt/src/flag_cmp_canonicalize/mod.rs` for the canonical example.
7. **Tests** in `tests.rs`: include all four shapes —
   - happy-path rewrite (input → expected output);
   - no-op (a graph that does NOT match the input shape stays bit-identical);
   - idempotency (running the pass twice == once);
   - interaction (composes with `ConstantFold` / `KnownBits` / the next pass without infinite loops);
   - and one fingerprint test asserting `extend_asm_fingerprint_from` propagated correctly (`cargo test --package ir asm_fingerprint` style).
8. **Python parity** if the pass is user-facing: add a wrapper class in `crates/strider-py/src/opt.rs` and update `PipelineState::from_default()`. (See `round7-opt.md` IMP-2 — there is no compile-time sync; manually mirror.)

### Verification step
- `cargo test --package opt <pass>` (the new pass's unit tests).
- `cargo test --package strider` (regression on the orchestrator's pipeline).
- `cargo test --package ir asm_fingerprint` (fingerprint contract).
- `cargo clippy --workspace -- -D warnings`.
- If touching Python: `uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_optimizer_pipeline.py`.

### Exit criteria
- The pass appears in the chosen pipeline (or as a post-pass) and is reachable via `default_pipeline()` / `stable_default_pipeline()` / `destructive_default_pipeline()` as appropriate.
- All existing strider tests still pass — no destabilisation of the indirect-branch fixed point.
- `validate_with_options { check_asm_fingerprints: true }` passes after the pass on at least one fixture.
- Python wrapper added (if applicable) and parity test green.

### Pitfalls / footguns
- **Adding a destructive rewrite to `stable_default_pipeline`** — invalidates the orchestrator's per-iteration `RegionIndex`. Always test against `cargo test --package strider switch_jump_table`.
- **Forgetting `extend_asm_fingerprint_from`** — silently breaks the proof-of-correctness contract. Pin with a test that asserts non-empty fingerprint on the rewritten root.
- **Production `expect()` / `unwrap()`** — rejected by `clippy::expect_used`/`unwrap_used` in non-test code (see `round7-opt.md` IMP-1). Use `?` propagation with `anyhow::anyhow!` context.
- **Recursive graph-walking helpers** — pathological inputs blow the 8 MB Rust stack (see `round7-opt.md` CRIT-1). Convert to iterative `Vec<…>` worklists or add a `MAX_DEPTH` guard.
- **Forgetting Python parity** — `PipelineState::from_default()` reconstructs Rust's `default_pipeline` by hand; adding a Rust pass without updating Python silently desyncs (round7-opt.md IMP-2).

---

## 4. `strider-fingerprint-audit`

### Trigger phrases
- "Verify asm-fingerprint propagation through this new pass."
- "Did my rewrite preserve the fingerprint contract?"
- "Run validate with check_asm_fingerprints on this fixture."

### When NOT to use
- You're authoring a brand-new pass — that's `strider-opt-pass-author` (which already includes this).
- The fingerprint check is failing because the lifter never set one — escalate to lift-side investigation, not pass-side.

### Inputs
- A failing test or a fixture binary path.
- The pass(es) under audit.

### Procedure
1. **Read the contract**: `Graph::asm_fingerprint(id) -> &[u64]`, `extend_asm_fingerprint(id, &[u64])`, `extend_asm_fingerprint_from(dst, src)`. Superset-only; structurally identical (cacheable) nodes share the union.
2. **Enable opt-in validation** in your test: switch `validate(graph, entry)` to `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })`. Layer C will flag every reachable non-exempt node with an empty fingerprint. Exempt kinds are listed in `crates/ir/src/validate/layer_c.rs::asm_fingerprint_exempt` (`Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`).
3. **For each newly-created node in the pass**, check there is a matching `extend_asm_fingerprint_from(new, contributor)` call. The contributor is the node whose semantics the new node preserves — typically the matched root, or the multi-input nodes whose values are unioned.
4. **Spot-check from a pattern match**: in a unit test, capture the rewritten root, then `m.asm_fingerprint(c, &graph)` should be a superset of every machine address in the input shape.
5. **For lift-time changes**: the lift driver wraps `process_insn` in `set_lift_addr(Some(addr)) … set_lift_addr(None)`, so every node created during the insn picks up `addr`. If you bypass this funnel (e.g. constructing nodes outside the per-insn block), call `extend_asm_fingerprint` explicitly.
6. **Cache-hit case**: when `Graph::create_node` deduplicates, the side-table entry must be the *union* of every contributor — see `asm_fingerprint_dedup_cache_hit_unions_via_extend` test.

### Verification step
- `cargo test --package ir asm_fingerprint` — round-trips the side-table contract.
- `cargo test --package opt -- --include-ignored` — ignored slot for the heavy fingerprint-validation tests.
- For a real binary: enable `check_asm_fingerprints: true` in a fixture-driven test and `cargo test --package strider <fixture_test>`.

### Exit criteria
- `validate_with_options(...)` passes on the optimised graph for the audit fixture.
- A test asserts `m.asm_fingerprint(c, &graph)` is non-empty and contains the expected addresses.

### Pitfalls / footguns
- Forgetting that Layer C is **opt-in** — default `validate` does not check fingerprints, so legacy mock-graph tests stay green even with empty fingerprints.
- Calling `set_asm_fingerprint(id, …)` (overwrite) inside a pass — this *can* shrink the set. Use `extend_asm_fingerprint*` exclusively in passes.
- Treating dedup as "empty fingerprint is fine" — it's not. Two structurally-identical nodes share one entry that is the union; the union must be non-empty.

---

## 5. `strider-indirect-shape-author`

### Trigger phrases
- "Add a new indirect-branch shape: <description>."
- "Resolve indirect calls that index a global function table."
- "BranchIndirect placeholder isn't classified — I see `UnresolvedIndirectBranch`."

### When NOT to use
- The shape is already classified but the in-place edit produces wrong IR — debug the inplace step, not add a new arm.
- The branch is direct (constant target) — that's CFG / lift-time territory.

### Inputs
- A small fixture binary that exhibits the shape (`fixtures/`, with a Makefile build rule).
- A walked example: which producer node feeds the placeholder?  Get this from `BranchIndirect` placeholder's anchored output via `classify_anchor`.

### Procedure
1. **Build a fixture** in `fixtures/` with a Makefile rule. Keep it minimal — one function exhibiting one shape, ELF in `fixtures/out/<arch>/`.
2. **Confirm tier-1 doesn't classify it**: the cfg-time mini-graph in `cfg::indirect_resolve` runs the opt pipeline locally; if it can't classify, the placeholder propagates to tier 2.
3. **Add a classifier arm** in `crates/opt/src/indirect_branch_resolve/<new_shape>.rs`, mirroring `jump_table.rs` / `stack_array.rs`:
   - Takes a producer `NodeOutputId` (the value feeding `BranchIndirect`).
   - Walks the producer subgraph (iteratively! — see `round7-opt.md` CRIT-1).
   - Returns `Option<ResolvedTargets>` (`LinkRegister` / `Single` / `Multiple`).
   - Respects `MAX_TABLE_ENTRIES = 4096` for any enumeration step.
4. **Wire into `classify_anchor*`**: extend `crates/opt/src/indirect_branch_resolve/classify.rs` to try the new shape after the existing arms. Order matters: cheap classifiers first.
5. **In-place edit** in `crates/opt/src/indirect_branch_resolve/inplace.rs` (+ orchestrator-level bridge in `crates/strider/src/indirect_resolve/inplace.rs`):
   - `LinkRegister` → `apply_link_register` (append ABI ret-val regs to placeholder Return).
   - `Single` (tail call) → `apply_tail_call` (rewrite as `Call` + `Return` chain at the tail).
   - `Multiple` (jump table) → leave for orchestrator CFG rebuild via `ResolvedTargets` propagation.
6. **Orchestrator `Decision` impact** (`crates/strider/src/orchestrator.rs::LoopState`):
   - In-place edits → `Decision::StableOnly` (rerun stable pipeline, no rebuild).
   - Multiple-target table → `Decision::Rebuild` (rebuild CFG with `with_known_targets` map).
   - All anchors resolved & no edits → `Decision::FixedPoint`.
7. **Tests**:
   - `crates/opt/src/indirect_branch_resolve/<new_shape>_tests.rs` — graph-mock unit tests for the classifier.
   - `crates/strider/tests/<shape>_test.rs` — end-to-end against the fixture ELF.
   - Cap-respecting test: a mocked oversized table must return `None`.

### Verification step
- `cargo test --package opt indirect_branch_resolve`
- `cargo test --package strider`
- `cargo test --package strider <new_fixture_test>`
- `pytest crates/strider-py/tests/python/test_indirect_branch_debug.py` (if exposing).

### Exit criteria
- The fixture binary lifts to a fully-resolved IR (no `UnresolvedIndirectBranchError`).
- No regression in `test_switch_jump_table.py` or `test_strider.py`.
- Fingerprint audit (skill 4) passes on the new shape.

### Pitfalls / footguns
- Recursing without a bound — convert all walks to iterative or add `MAX_DEPTH` guard.
- Skipping `MAX_TABLE_ENTRIES = 4096` — buggy `KnownBits` masks can otherwise force a 4 GiB enumeration.
- Adding a new shape to **tier 2** when tier 1 (cfg-time) could handle it — tier 1 sees only the single region but is cheaper; prefer tier 1 when possible.
- Forgetting to wire the orchestrator `Decision` — the loop will diverge or loop forever.
- Re-running `RedundantPhis` / `DeadBranchElimination` mid-fixedpoint — they invalidate the per-iteration `RegionIndex`.

---

## 6. `strider-callother-abi`

### Trigger phrases
- "Add a CallOther ABI for `<opname>`."
- "I'm hitting `UnknownCallOtherError: <name>`."
- "Lifting fails on `cpuid` / `rdtsc` / `<sleigh user-op>`."

### When NOT to use
- The user-op is fully described by Sleigh's pcode operands and has no implicit side-effects — this should be `NoOp`, not a new ABI entry. Still classify it; just pick `NoOp`.
- The user-op is an architecture-private trap that should terminate the function — `NoReturn` is the answer.

### Inputs
- The exact user-op name as Sleigh emits it (case-sensitive — pull from the failing `UnknownCallOtherError` message).
- The arch preset (`X86_64`, `Arm`, …) — relevant only if the ABI varies by arch.
- The ISA reference for the instruction (so we know which registers it implicitly reads/writes and whether it touches memory).

### Procedure
1. **Locate the table**: `crates/target/src/call_other_abi.rs`. Two functions — `classify_arch_specific` (for ABI-varies-by-arch entries; currently `swi`, `syscall`, `CallHyperVisor`, `CallSecureMonitor`) and `classify_arch_independent` (everything else).
2. **Decide the class**:
   - `NoOp` — no IR node emitted, control/memory unchanged, pcode-explicit output discarded. (e.g. `cpuid` if the lifter doesn't track results — but only if you really don't care; otherwise `Call(abi)`).
   - `NoReturn` — terminates the region; emits a dangling-output terminal CallOther. Use for trap instructions (`ud2`, `hlt`).
   - `Call(CallOtherAbi { implicit_reads, implicit_writes, memory_edge })` — most cases.
3. **Fill `CallOtherAbi`**:
   - `implicit_reads` — register names beyond Sleigh's pcode `inputs[1..]`. Use the *exact* Sleigh register name (case-sensitive — `RAX` on x86_64, `r0` on ARM, `x0` on AArch64).
   - `implicit_writes` — registers the op writes/clobbers beyond pcode `output`. Each becomes one extra clobber output slot.
   - `memory_edge` — `true` for ops with observable memory effects (syscall, port I/O, cache writeback); `false` for pure register-level ops (`cpuid`, `rdtsc`, NEON math).
4. **Add a doc comment** explaining the ABI source (ISA manual section, ELF ABI doc, kernel source path). Match the style of the existing `swi` / `syscall` entries.
5. **Verify register names resolve** on the target's Sleigh spec — strider rejects unknown names at lift time. Cross-check against `rsleigh::sla_spec::SLA_SPEC_<arch>`'s register table.
6. **Test**:
   - Add a unit test in `crates/target/src/call_other_abi.rs::tests` (or sibling) asserting `classify(preset, "<name>") == Some(CallOtherClass::Call(abi))`.
   - Add an integration test against a fixture that emits the user-op (build with `fixtures/Makefile`).

### Verification step
- `cargo test --package target call_other`
- `cargo test --package strider <fixture_test>` — the failing fixture should now lift cleanly (no `UnknownCallOtherError`).
- `cargo clippy --workspace -- -D warnings`.

### Exit criteria
- `classify(preset, "<name>")` returns the right class.
- The originally-failing `UnknownCallOtherError` is gone for all affected fixtures.
- A test pins the entry so accidental deletion regresses.

### Pitfalls / footguns
- **Register-name capitalisation differs by arch**: x86_64 uses `RAX`, AArch64 uses `x0`, ARM uses `r0`. The strict-on-emission policy converts a typo into a build break.
- **`memory_edge: false` is wrong for any I/O or syscall-like op** — subsequent loads will commute through it incorrectly.
- **Putting an arch-varying entry in `classify_arch_independent`** — collides across presets (e.g. `swi`).
- **Picking `NoOp` to avoid filling out the ABI** — silently drops side effects. If the op writes a register and you say `NoOp`, downstream patterns over that register are wrong.

---

## 7. `strider-target-arch`

### Trigger phrases
- "Add support for RISC-V."
- "Add an `arm_thumb_be` preset."
- "Register a new `SleighArch` and `CallingConvention`."

### When NOT to use
- The arch is already supported but you want a new *calling convention* on it — that's a CC-only addition (still in `target`, but skip the SleighArch step).
- You're modifying register aliasing for an existing arch — go to `crates/pcode-lift/src/vn_io.rs`.

### Inputs
- The Sleigh `.sla` and `.pspec` names (look in `../rsleigh/src/sla_spec.rs` and `pspec.rs` for the `SLA_SPEC_*` / `PSPEC_*` constants).
- ABI documentation: arg-passing regs, return-value regs, callee-saved regs, stack-arg layout, return-stack-pop delta, link-register name.
- Endianness.

### Procedure
1. **Add an `ArchPreset` variant** in `crates/target/src/arch.rs::ArchPreset`. Keep the granularity per-preset (BE/LE/Thumb each get their own variant).
2. **Add a constructor** in `crates/target/src/arch.rs::SleighArch` (e.g. `SleighArch::riscv64()`). Wire `sla_spec`, `pspec`, `endianness`, `preset`.
3. **Add a CC preset** in `crates/target/src/calling_convention/mod.rs`. Mirror the structure of `aarch64_aapcs64` or `arm_aapcs`:
   - Stack-pointer reg name.
   - Integer + float arg regs (positional).
   - Integer + float return-value regs.
   - Callee-saved regs.
   - `stack_arg_offsets` — positional offsets for stack-passed args.
   - `ret_stack_pop` — `0` for callee-cleanup ABIs (AAPCS), non-zero for caller-cleanup (cdecl pops `4`/`8`).
   - Link-register varnode name (`Some("ra")` / `Some("lr")` / `None`).
4. **Verify register names against the Sleigh spec**: `CallingConvention::build` will fail if a name doesn't resolve. Cross-check by lifting one instruction with rsleigh and inspecting its `Vn` table.
5. **Add CallOther entries** for arch-specific user-ops (see skill 6) — e.g. RISC-V's `ECALL` will need a syscall ABI entry in `classify_arch_specific`.
6. **Register-aliasing**: if the arch has overlapping registers strider doesn't handle (something other than the documented widths 1, 2, 4, 8, 10, 16 bytes), extend `crates/pcode-lift/src/vn_io.rs::vn_mask` and `find_largest_fitting_register`.
7. **Python parity**: add a `SleighArch` factory and CC factory in `crates/strider-py/src/arch.rs` and `crates/strider-py/src/cc.rs`.
8. **Fixture**: build a small `hello` binary in `fixtures/Makefile` for the new arch, plus a smoke test in `crates/strider-py/tests/python/test_arch.py`.

### Verification step
- `cargo test --package target` (CC builds, register-name resolution).
- `cargo test --package strider` (full pipeline on a fixture).
- `uv run pytest crates/strider-py/tests/python/test_arch.py crates/strider-py/tests/python/test_smoke.py`.

### Exit criteria
- `SleighArch::<arch>()` and `CallingConvention::<cc>()` build without error.
- Lifting the new fixture ELF produces a valid `BuiltFunctionGraph` (`validate` passes).
- At least one Python smoke test exercises the new arch.

### Pitfalls / footguns
- Sleigh register names are case-sensitive and arch-specific. `RAX` ≠ `rax` ≠ `eax`.
- Forgetting `ret_stack_pop` — wrong stack-frame size, breaks `CallStackArgCollect`.
- Missing `lr` / link-register Vn — breaks `LinkRegister` indirect-branch resolution.
- Not updating Python — the test surface still uses the CC, so the `MemoryMap` / `Strider` constructors need a Py-side factory.
- Forgetting to add an `ArchPreset` variant means `call_other_abi::classify_arch_specific` can't dispatch ABI variants.

---

## 8. `strider-py-binding`

### Trigger phrases
- "Expose `<X>` from Rust to Python."
- "Add a Python binding for `<rust API>`."
- "I want to call `<rust function>` from Python."

### When NOT to use
- The Python module already exposes the API — the user wants ergonomic improvements; route to a focused refactor instead.
- The user wants to add a *test* using existing bindings — that's just pytest.

### Inputs
- The Rust API to expose (function or type).
- The intended Python module path (`strider`, `strider.opt`, `strider.pattern`, `strider.errors`).

### Procedure
1. **Pick the source file**: `crates/strider-py/src/<module>.rs`. Existing modules: `arch`, `cc`, `cfg`, `dot`, `errors`, `graph`, `matcher`, `opt`, `pattern`, `reader`, `run`, `sleigh`, `strider_cls`.
2. **Define the PyO3 class / function**:
   - `#[pyclass(name = "X", module = "strider.<sub>", frozen)]` for opaque wrappers.
   - `#[pymethods] impl PyX { #[new] fn new(...) -> PyResult<Self> { ... } }`.
   - For free functions: `#[pyfunction]` + register in `lib.rs`'s `#[pymodule]`.
3. **Error handling — never `panic!` / `unwrap` on user input**:
   - Rust panics from PyO3 land as Python `PanicException` and abort the Python process under `abi3-py39`. Always return `PyResult<T>` with a typed exception via `crate::errors::into_*_err`.
   - Use the existing taxonomy: `StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`, `UnresolvedIndirectBranchError`, `UnknownCallOtherError`. Don't add new exception types lightly.
   - Pattern crate: `into_pattern_err`. Reader: `into_reader_err`. Lifting: `into_lift_err`. Rewriting: `into_rewrite_err`. Generic: `into_strider_err`.
4. **Optional Python-side ergonomics**:
   - String-keyed capture interning (see `pattern.rs::intern_capture` for the model).
   - Subclassable ABCs for callback-style readers (`MemReader`, `ReadOnlyMemory`).
5. **Add tests** in `crates/strider-py/tests/python/test_<feature>.py`. Run with `uv`:
   - `uv sync --group dev`
   - `uv run maturin develop`
   - `uv run pytest crates/strider-py/tests/python/test_<feature>.py`
6. **Update the public-API snapshot** (`crates/strider-py/tests/python/test_public_api_snapshot.py`) — adding new symbols updates the snapshot intentionally; `pytest --snapshot-update` is the canonical refresh.
7. **Update the type stubs** (`.pyi`) if the package ships any.
8. **Sync risk** for things like `default_pipeline` mirrors in Python (`opt::PipelineState::from_default`) — every Rust pass needs a Python wrapper added by hand. See `round7-opt.md` IMP-2 for the recommended Rust-side `optimizer_count()` assertion.

### Verification step
- `uv run maturin develop` — builds and installs the local wheel.
- `uv run pytest crates/strider-py/tests/python/test_<feature>.py -v`.
- `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py` — confirms the API surface didn't change unexpectedly.
- `cargo clippy --workspace -- -D warnings`.

### Exit criteria
- The new symbol is importable as `from strider.<module> import <name>`.
- A test exercises the happy path and at least one error path (assert `pytest.raises(StriderError)` or a more specific subclass).
- Public-API snapshot updated and reviewed.
- `maturin build --release` produces a wheel without warnings.

### Pitfalls / footguns
- **Don't propagate `panic!`** — use typed `PyResult<_>` with `into_*_err`.
- **`PyPat::ordered()` no-op trap** (round7-pattern issue #4) — when wrapping a builder method, mirror Rust's signature exactly; don't silently no-op.
- **`from_default()` desync** — every Rust-side pass added must be added to the Python `PipelineState::from_default`.
- **GIL pitfalls** — long-running Rust work should release the GIL via `py.allow_threads(...)` to keep Python responsive.
- **Test fixtures must use `uv run`** — the local-built abi3 wheel under `target/wheels/` is what the tests import; running plain `pytest` against the system Python imports stale wheels.
- **Memory ownership** — wrapping `&BuiltFunctionGraph` requires an `Arc<…>` because Python objects outlive Rust scopes. Look at `PyGraph` / `PyMatcher` for the pattern.

---

## Skill bundle install

### Suggested location

Two options, in order of preference:

1. **`crates/strider/.claude/skills/<name>/SKILL.md`** — co-located with the
   project. Picked up automatically by Claude Code when the working directory
   is anywhere inside `/mnt/c/Users/mikeg/Documents/strider`. Best when the
   skills should travel with the repo (recommended for these eight).

2. **`~/.claude/skills/strider/<name>/SKILL.md`** — user-global. Useful if a
   single developer wants the skills to apply across multiple checkouts but
   not be checked into the repo.

For this bundle, use option 1 — every skill references in-tree paths, and the
skills change in lockstep with the code (`call_other_abi.rs` table edits, new
opt-pass scaffolding conventions).

### File layout per skill

```
crates/strider/.claude/skills/<skill-name>/
└── SKILL.md
```

`SKILL.md` frontmatter (YAML) is required and matches the official Skill
schema:

```markdown
---
name: <skill-name>
description: <one-line description matching the trigger phrases above>
---

<procedure body — sections from the design above>
```

The `description` field drives auto-invocation: Claude triggers the skill when
the user's natural-language request matches the description. Use the
"Trigger phrases" list above to write descriptions that match real prompts.

### Invocation

- **Auto** — Claude reads each skill's description on session start and
  auto-invokes when a user message matches.
- **Explicit** — user types `/<skill-name>` to force invocation.
- **Programmatic** — agents (including this one) can call them via the
  `Skill` tool in the harness.

### Cross-referencing

Several skills hand off to each other. Make this explicit in each skill body:

- `strider-pattern-author` → `strider-debug-pattern` (when the pattern
  doesn't match).
- `strider-opt-pass-author` → `strider-fingerprint-audit` (mandatory final
  step before completion).
- `strider-indirect-shape-author` → `strider-fingerprint-audit` (same).
- `strider-target-arch` → `strider-callother-abi` (any arch-specific user-op
  needs an entry).
- `strider-py-binding` → `strider-pattern-author` (when wrapping pattern
  ctors).

### Maintenance

When `CLAUDE.md` changes (especially the "lift-time canonicalisation" table or
the optimiser-pipeline lists), audit the affected skills. Several pitfalls
above quote `CLAUDE.md` verbatim — those quotes will drift if not maintained.
A lightweight CI hook (or a periodic `claude-md-improver` run) can flag stale
references.
