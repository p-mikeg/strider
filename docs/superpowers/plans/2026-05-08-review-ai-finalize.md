# review/ai finalize — land every deferred item

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:test-driven-development for every behaviour change; superpowers:writing-plans for any non-trivial subtask.

**Goal:** Land every item in the deferred list of the prior round on `review/ai`, with no further deferrals.

**Strategy:**
- Quick verifications (no implementation needed if already correct) — sequential, 5 min each.
- Mechanical refactors (L3, L5) — dispatch agent.
- Test authoring (O2-O15) — dispatch agent.
- Substantial refactors (M1, J1, J2, L2, B1) — sequential, TDD where applicable.
- Small additions (G7, G9) — sequential, after substantial work merges.

**Iterative-only constraint:** No new recursion. When refactoring an existing recursive function, opportunistically convert.

---

## Phase 1 — quick verifications (sequential, ~5 min each)

### V1 — G10: `cfg::SpecialTerm::CallIndirect` absent
- Grep `crates/cfg/src` for `CallIndirect` in `SpecialTerm` context.
- If absent: no commit needed.

### V2 — MIPS `n64.arg_passing_regs` `t0..t3` semantics
- Read `target/src/calling_convention/mod.rs` mips_n64 preset.
- Cross-reference Sleigh's MIPS64 register table (probably resolves `t0..t3` to `$8..$11` per the older MIPS naming).
- If Sleigh's resolution agrees with the N64 ABI's `a4..a7` register assignment: no functional change, but rename comment to clarify.
- If disagrees: CRITICAL — file a fix.

### V3 — `rdtscp` ECX clobber
- Read `crates/target/src/call_other_abi.rs` for `rdtscp` entry.
- If a separate "rdtscp" entry exists with `ECX` in implicit_writes: done.
- If not: add it (Intel SDM RDTSCP writes EAX/EDX/ECX).

---

## Phase 2 — mechanical refactors (parallel via agent)

### L3 — `apply_elf_relocations_with_extender(..., F)` closure-based
- File: `crates/reader/src/elf.rs`.
- The autoload variant currently does "Pass 1: scan for missing sites; Pass 2: call apply_elf_relocations".
- Refactor so the body is parameterised by a closure `extend_on_missing: F` that returns `Option<MemRegion>` (autoload returns Some(section); non-autoload returns None).

### L5 — collapse `is_branch_tail_call` / `_nocheck`
- File: `crates/cfg/src/cfg/builder/region_builder.rs`.
- Two variants — the `_check` adds an insn_index validation.
- Inline the check at its 2 callers; drop `_check`.

### Tests already covered (verify, no commit needed)
- O11 `default_pipeline_idempotent` — exists.
- Some alias roundtrips — partially exist.

---

## Phase 3 — test authoring (parallel via agent)

Author the remaining P2/P3/P4 tests from `reviews/round7-test-plan.md`:

### P2 (coverage gaps)
- O2 asm-fingerprint shrink-prevention pipeline test
- O3 vn_io sub-register partial-write with phi-live parent
- O5 Python typed-error tests (each typed exception triggered end-to-end)
- O6 AArch64 e2e lift produces valid IR
- O7 mem_phi / value_phi pattern matching test (pair with H1)

### P3 (property tests)
- O8 Pattern alias round-trip (verify gaps)
- O9 Stack-array indirect-branch shape end-to-end
- O10 StackLoadForward + StackStoreDetect convergence ≤ 2 iters

### P4 (benchmarks in `crates/strider/benches/scaling.rs`)
- O12 chain-of-N stores
- O13 diamond CFG of N regions
- O14 wide jump-table N targets
- O15 find_all_requirements shared-capture join

---

## Phase 4 — substantial refactors (sequential, TDD)

### M1 — `Graph::create_node_attributed(kind, inputs, output_kinds, contributors)`

**Files:**
- `crates/ir/src/graph/store.rs` — add the helper.
- ~15 opt-pass call sites that currently do `create_node + extend_asm_fingerprint_from`.

**Steps:**
1. Add `Graph::create_node_attributed` that wraps `create_node` and unions each contributor's fingerprint into the new node.
2. Add a unit test in `ir/src/builder/tests.rs` proving the contract.
3. Migrate call sites one pass at a time:
   - `opt::constant_fold` (rules.rs)
   - `opt::dead_branch`
   - `opt::flag_cmp_canonicalize`
   - `opt::function_args`
   - `opt::indirect_branch_resolve::inplace`
   - `opt::known_bits`
   - `opt::redundant_phis`
   - `opt::stack_load_forward`
   - `opt::stack_store::detect`
4. After each pass migration, run `cargo test --workspace`.

### J2 — `BuiltFunctionGraph::from_graph_and_entry` → `pub(crate)` + `RewriteCtx` newtype

**Files:**
- `crates/ir/src/function.rs` — change visibility.
- `crates/strider/src/rewrite.rs` — replace dummy-BuiltFunctionGraph trick with `RewriteCtx`.
- `crates/opt/src/pipeline.rs` (the `with_built` shim, if it uses the same trick).

**Steps:**
1. Add `pub struct RewriteCtx<'g> { graph: &'g mut Graph, entry: NodeId }` somewhere (in `pattern` or `ir`).
2. Change `pattern::rewrite_rule` signature to accept `&mut RewriteCtx` instead of `&mut BuiltFunctionGraph`.
3. Migrate `GraphRewriter::apply_rule` to construct `RewriteCtx` instead of stealing graph+entry into a dummy `BuiltFunctionGraph`.
4. Demote `BuiltFunctionGraph::from_graph_and_entry` to `pub(crate)`.
5. Run all tests.

### J1 — Phantom-typed `OptimizerPipeline<Phase>`

**Files:**
- `crates/opt/src/pipeline.rs`.
- Every caller that constructs an `OptimizerPipeline`.

**Steps:**
1. Introduce `Phase` zero-sized markers: `Stable`, `Destructive`, `Full`.
2. Make `OptimizerPipeline` generic over `<P: Phase>`.
3. `add` only callable on `Stable<Stable>` / `Full`; destructive passes only on `Destructive` / `Full`.
4. Migrate `default_pipeline()` → `OptimizerPipeline<Full>`, `stable_default_pipeline()` → `<Stable>`, `destructive_default_pipeline()` → `<Destructive>`.
5. Update `Strider::build_*_optimizer_pipeline` and Python wrappers.
6. Run all tests.

### L2 — `RedundantPhis` VarPhi/ControlState collapse helper

**Files:**
- `crates/opt/src/redundant_phis/mod.rs`.

**Steps:**
1. Identify the duplicated "find unique reachable ctrl predecessor" logic in VarPhi-collapse and ControlState-collapse arms.
2. Extract `unique_reachable_ctrl(graph, node) -> Option<NodeOutputId>`.
3. Both arms call the helper.
4. Run pass tests.

### B1 — full CondBranch single-insn OOB fix

Two options; choose simpler:

**Option A — relax `add_region` non-empty invariant for Branch terminator:**
1. Allow empty `insns` Vec when terminator is `RegionTerminator::Branch`.
2. Update strider's IR-layer per-region driver to handle empty regions (just emit the ControlState + edge to the next region).
3. Verify with a regression test for the silent-edge-loss case.

**Option B — extend `SpecialTerm` with `BranchSkipCondBranch`:**
1. Add variant; `skips_opcode` matches `CondBranch`.
2. cfg keeps the lone CondBranch insn AND emits Branch terminator.
3. IR-layer skips the trailing CondBranch when `SpecialTerm::BranchSkipCondBranch` is set.
4. Verify with a regression test.

Option A is preferred (smaller IR-layer change).

---

## Phase 5 — small additions (G7, G9)

### G7 — Python `OptimizerPipeline.from_default()` sync test
- Expose `OptimizerPipeline::optimizer_count(&self) -> usize` on the Rust side.
- Wrap on the Python side as `OptimizerPipeline.optimizer_count` property.
- Pytest: `OptimizerPipeline.default().optimizer_count == opt::default_pipeline().optimizer_count()` (or assert exact integer the Rust pipeline currently has).

### G9 — `.when()` predicate exception surfaces on stderr (pytest)
- pytest with `capsys`: define a predicate that raises `ValueError("oops")`; run `find_all`; assert `"oops"` appears in `capsys.readouterr().err`.

---

## Phase 6 — final verification

- `cargo test --workspace` clean.
- `cargo clippy --workspace -- -D warnings` clean.
- `cd crates/strider-py && uv run maturin develop --release && uv run pytest` clean (excluding pre-existing arm64 fixture failures).
- `git log --oneline review/ai ^feature/ai` — confirm every change is its own commit.

---

## Rollback safety

If a substantial refactor (M1/J1/J2/L2/B1) breaks too many tests or a load-bearing invariant, prefer to:
1. Revert that single commit.
2. Document the blocker in a `reviews/round7-deferred-final.md` file.
3. Move to the next item.

The branch must end in a state where `cargo test --workspace` passes.
