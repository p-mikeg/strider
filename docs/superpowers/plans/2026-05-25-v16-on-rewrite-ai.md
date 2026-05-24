# strider v16 on rewrite/ai — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Execute the 4-phase v16 structural redesign on a new branch `rewrite/ai` branched from `rewrite/strdier`. The original v16 plan at `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` was written against rewrite/strdier's codebase shape and is directly applicable here. The simplification/ai1 attempt is NOT cherry-pickable (different crate layout, different starting state).

**Architecture:** Single working branch (`rewrite/ai`) off rewrite/strdier. Each of the 4 phases ships independently and gate-clean. Phase 2's Function struct hosts side-tables added by Phase 3 and Phase 4.

**Tech Stack:** Rust workspace at `/mnt/c/Users/mikeg/Documents/strider`. **Strider-prefixed crate names** on rewrite/strdier: `strider-ir`, `strider-analyze`, `strider-lift`, `strider-target`, `strider-reader`, `strider-pattern-macros`, `strider-ir-test-utils`, `strider-py`. Generic crates: `dot`, `entity-utils`, `graphwalk`.

---

## Context — why we're restarting on a different branch

The session previously executed v16 on `simplification/ai1`, completing Phase 1 (ControlState→Region) and Phase 2.1 (BuiltFunctionGraph→Function rename). Mid-flight you flagged that all v16 work + reviews should target `rewrite/ai` (off `rewrite/strdier`), not `simplification/ai1` (off `feature/ai`).

The two branches have diverged dramatically:

| | simplification/ai1 | rewrite/strdier |
|---|---|---|
| Commits since merge-base `5b0f7a8d` | 15 | **460** |
| Crate names | unprefixed (`ir`, `opt`, `cfg`, etc.) | **strider-prefixed** (`strider-ir`, `strider-analyze`, etc.) |
| `BuiltFunctionGraph` type | exists (pre-rename target) | does NOT exist |
| `phi_var_tag` side-table | doesn't exist | exists on Graph |
| `function.rs` content | holds `FunctionGraph` + `BuiltFunctionGraph` types | doc + 1 test only (clean slate for new `Function` struct) |
| File paths in v16 plan | mismatch (plan uses `crates/strider-X`) | **match** (plan was originally written for this codebase) |

The simplification/ai1 work CANNOT be cherry-picked — file paths differ and the starting state is different. Phase 1's rename + Phase 2.1's BuiltFunctionGraph→Function rename do not translate.

## What the original v16 plan covers

`docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` is the authoritative source for the 4-phase task content. It was authored against this exact codebase shape (strider-prefixed crates, fat Graph, no BuiltFunctionGraph). Every file path, code snippet, and command in that plan is directly applicable to rewrite/strdier.

Quick reference to phases (full detail in the existing plan):

1. **Phase 1 — Rename `ControlState` node kind → `Region`** (mechanical sweep, ~40-60 references, 30-60 min, LOW risk)
2. **Phase 2 — Create `Function` struct + move 5 side tables off Graph** (1-2 days, MEDIUM risk; Function = `graph + entry + cc_metadata + 5 NodeId-keyed side tables`)
3. **Phase 3 — Replace `FunctionArg` node kind with arg-index side-table on Function** (1 day, MEDIUM risk; side-table value is `Vec<NodeId>` per your design choice; pattern matcher `function_arg(i)` API reshapes)
4. **Phase 4 — Memory SSA redesign as an optimization** (2-4 days, MEDIUM-HIGH risk; 2 new non-phi node kinds `MemPartition`/`MemUnion`, typed `Memory(Option<MemPartitionId>)` output kind, new `AliasSplit` pass, sunset `StackStoreDetect` + `StackStore`/`StackStorePhi` kinds)

## Lessons from the simplification/ai1 attempt to apply here

1. **Don't trust audit-cited file paths blindly.** Verify with `ls crates/` and `head Cargo.toml` before dispatching agents. (Save as memory: `feedback_anchor_audits_to_workspace.md` — already saved.)
2. **Don't trust review-target branches by default.** This session's confusion came from defaulting to `feature/ai` as the main; always confirm review target before audits. (Save as memory: `feedback_review_branch_target.md` — already saved.)
3. **Inspect the canonical type before assuming it doesn't exist.** Phase 2 on simplification/ai1 took 5 sub-tasks (2.1-2.5) until the user pointed out `BuiltFunctionGraph` was already what `Function` should be. On rewrite/strdier there's no such type — Phase 2 IS a clean-slate "create Function struct" task, matching the original plan.
4. **MemPartition + MemUnion are non-phi.** (Confirmed earlier in conversation.) Memory phi continues to be `MemPhi`, now typed `Memory(Some(P))` inside partitioned subgraphs.

---

## Phase 0 — Branch setup

**Goal:** Create `rewrite/ai` off `rewrite/strdier` and confirm baseline.

### Tasks

#### Task 0.1: Create rewrite/ai branch

- [ ] **Step 1: Verify clean working tree on current branch**

```bash
cd /mnt/c/Users/mikeg/Documents/strider
git status --short
```

Expected: empty output (working tree clean — confirmed by prior `git checkout -- .` cleanup).

- [ ] **Step 2: Verify rewrite/strdier is at known SHA**

```bash
git rev-parse rewrite/strdier
```

Expected: `237e1654a7e483696614c1b1892c65f1772137b2` (matches origin/rewrite/strdier).

- [ ] **Step 3: Create rewrite/ai branch from rewrite/strdier**

```bash
git checkout rewrite/strdier
git checkout -b rewrite/ai
git push -u origin rewrite/ai
```

Expected: new branch `rewrite/ai` created locally + pushed to origin, tracking origin/rewrite/ai.

#### Task 0.2: Baseline gates on rewrite/ai

- [ ] **Step 1: Run all 4 gates from scratch on rewrite/ai**

```bash
cargo build --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo doc --workspace --no-deps 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
```

Expected: all green, modulo whatever pre-existing failures rewrite/strdier ships with. Document any pre-existing failures here for the rest of the v16 work to treat as tolerated:

```
PRE-EXISTING TOLERATED FAILURES on rewrite/ai baseline:
  [populate from cargo test output]
```

- [ ] **Step 2: Note pre-existing failures for future reference**

If there are failures: edit this plan to list them. Future phases' gate runs will tolerate ONLY these tests failing.

If clean: note "baseline 100% green" for the rest of the plan.

---

## Phase 1 — Rename `ControlState` → `Region`

**Refer to:** `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` Phase 1 (lines 87-148 of that document) for full task content.

**Files on rewrite/strdier** (confirmed by Section E of `reviews/rewrite-strdier-v16-prerequisites.md`):
- `crates/strider-ir/src/node/kind.rs` (variant + classifier)
- `crates/strider-ir/src/node_signature.rs`
- `crates/strider-ir/src/builder/{mod,call}.rs`
- `crates/strider-ir/src/validate/{layer_c,mod}.rs`
- `crates/strider-ir/src/walk/mod.rs`
- `crates/strider-ir/src/dot/{mod,label}.rs` (actually `crates/strider-ir/src/graph_dot/{mod,label}.rs` — verify with grep)
- `crates/strider-analyze/src/opt/*/mod.rs` and `*/tests.rs` (heaviest: `redundant_phis`, `dead_branch`)
- `crates/strider-analyze/src/pattern/pat/builders/phi.rs` (control_state DSL builder)
- `crates/cfg/src/**/*.rs`
- `crates/strider-py/src/**/*.rs` + `crates/strider-py/strider/__init__.pyi`
- `CLAUDE.md`

**Lesson from simplification/ai1 sweep:** the simple `sed` pattern misses underscored compound names like `ignore_control_states` (a public API on `MatcherOptions`), `try_walk_through_control_state` (private fn), `check_layer_c_control_state` (validator fn), `ControlStateNonControlPredecessor` (error variant), test function names containing `control_state`, etc. After the initial sweep, run an exhaustive grep and clean up any survivors.

**Tag:** `v16-phase-1-final` after the commit + push.

---

## Phase 2 — Introduce `Function` struct + move 5 side tables off Graph

**Refer to:** `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` Phase 2 (lines 150-380).

**Differences from simplification/ai1 attempt** (which is why we're restarting):

- On rewrite/strdier, **`BuiltFunctionGraph` does not exist**. `function.rs` is a clean slate — we create `Function` from scratch as the original plan describes (no rename, no collision).
- On rewrite/strdier, `Graph` carries 5 NodeId-keyed side tables INCLUDING `phi_var_tag` (which doesn't exist on simplification/ai1). All 5 move to Function:
  1. `stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>`
  2. `call_other_names: SecondaryMap<NodeId, Option<String>>`
  3. `asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>`
  4. `call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>`
  5. `phi_var_tag: SecondaryMap<NodeId, Option<rsleigh::Vn>>`
- `Graph` also carries `entry: Option<NodeId>` and `cc_metadata: Option<CcMetadata>` — these also move to Function.

**Function struct final shape** after Phase 2:

```rust
pub struct Function {
    graph: Graph,
    entry: Option<NodeId>,
    cc_metadata: Option<CcMetadata>,
    asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>,
    stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>,
    call_other_names: SecondaryMap<NodeId, Option<String>>,
    call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>,
    phi_var_tag: SecondaryMap<NodeId, Option<rsleigh::Vn>>,
}
```

After Phase 2 completes, `Graph` is a structural arena only (nodes/inputs/outputs PrimaryMaps + dedup cache + `wide_consts` interner + `initial_var_index`). All function-level overlay state lives on Function.

**Sub-tasks (5 commits):**
- 2.1: Function struct skeleton + 3 TDD tests
- 2.2: Migrate `entry` field
- 2.3: Migrate `cc_metadata`
- 2.4: Migrate 5 side tables (one commit per side table = 5 commits; or one combined commit if no overlap)
- 2.5: Update `FunctionBuilder::build` to return `Function` instead of `Graph`

**Tag:** `v16-phase-2-final` after the final commit + push.

---

## Phase 3 — Drop `FunctionArg` node kind; arg-index side-table on Function

**Refer to:** `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` Phase 3 (lines 382-530).

**User-confirmed design point:** side-table value is `Vec<NodeId>` per index (uniform; register-args case = vec of size 1; stack-args case may have multiple Loads at different widths at the same offset).

**Files on rewrite/strdier** (confirmed by Section E of the prerequisites report):
- `crates/strider-ir/src/node/kind.rs` (delete FunctionArg variant + FunctionArgSource enum)
- `crates/strider-ir/src/node_signature.rs` (remove FunctionArg signature)
- `crates/strider-ir/src/dot/{mod,label}.rs` (remove FunctionArg rendering)
- `crates/strider-ir/src/validate/layer_c.rs` (remove DuplicateFunctionArg invariant)
- `crates/strider-ir/src/function.rs` (add `arg_index_to_nodes: FxHashMap<u32, Vec<NodeId>>` + accessors)
- `crates/strider-analyze/src/opt/function_args/mod.rs` (FunctionArgDetect populates side-table instead of creating nodes)
- `crates/strider-analyze/src/pattern/matcher/function_arg_handle.rs` (FunctionArgHandle wraps arbitrary NodeId)
- `crates/strider-analyze/src/pattern/matcher/mod.rs` (drop FunctionArgIndex lazy cache; `Matcher::function_arg(i)` reads side-table directly)
- `crates/strider-analyze/src/pattern/pat/builders/function_arg.rs` (FunctionArgPat resolves via side-table)
- `crates/strider-py/src/pattern.rs` + `crates/strider-py/src/matcher.rs` (Python mirror)
- Test files using `FunctionArg { … }` shape

**Sub-tasks (4 commits):**
- 3.1: Add `arg_index_to_nodes` side-table on Function + 2 TDD tests
- 3.2: Update FunctionArgDetect to populate side-table (no node creation)
- 3.3: Migrate matcher `function_arg()` API to read side-table; add `function_args(i)` for stack multi-Load case
- 3.4: Delete `FunctionArg` node kind + `FunctionArgSource` enum + validator invariant

**Tag:** `v16-phase-3-final` after the final commit + push.

---

## Phase 4 — Memory SSA redesign (`MemPartition`/`MemUnion` as an optimization)

**Refer to:** `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` Phase 4 (lines 532-880).

**Design locks** (from this conversation):
- Lifter stays unchanged (zero risk to lift + CFG builder layers). The optimizer rewrites unified-Memory subgraphs into partitioned form post-lift.
- Two new node kinds, both NON-PHI (no control input, no phi_token):
  - `MemPartition { partition: MemPartitionId }` — projects a partition out of unified Memory. 1 input, 1 output.
  - `MemUnion` — bundles partition tokens back to unified Memory. N inputs, 1 output.
- `MemPhi` stays as the CFG-merge phi shape; after AliasSplit it can be typed `Memory(Some(P))` instead of `Memory(None)`.
- `NodeOutputKind::Memory` extends to `Memory(Option<MemPartitionId>)` — `None` = unified, `Some(P)` = partition P. Validator enforces this structurally.
- AliasSplit runs always (no opt-in mode).
- MMIO partition deferred (user-confirmed: "we can always add it later"). PartitionDiscovery only emits Stack / Heap / Rom / Unknown for now.

**Background reading:** `reviews/memory-subsystem-deep-dive.md` (524 lines).

**Sub-tasks (10 commits):**
- 4.1: Plan-review subagent verifies design against current code (~45 min)
- 4.2: Add `MemPartitionId` + `AliasClass` enum + `PartitionInfo` + `PartitionTable` infrastructure; `partition_table` field on Function
- 4.3: Extend `NodeOutputKind::Memory` to `Memory(Option<MemPartitionId>)` — all current construction sites pass None
- 4.4: Add `MemPartition` + `MemUnion` node kinds (both non-phi) + signatures + dot labels + classifier
- 4.5: Implement `AliasSplit` optimization pass (the meat of Phase 4; uses existing `decompose_sp` + `walk_mem_chain`)
- 4.6: Migrate `StackLoadForward` to walk `Memory(Some(Stack))` chain (~30% LOC reduction)
- 4.7: Migrate `CallStackArgCollect` to walk `Memory(Some(Stack))` chain (~15% LOC reduction)
- 4.8: Migrate `FunctionArgDetect`'s no-shadow check to walk `Memory(Some(Stack))` chain
- 4.9: Migrate `LoadReadOnly` to fire only on `Memory(Some(Rom))`
- 4.10: Delete `StackStoreDetect` + `StackStore { offset }` + `StackStorePhi` node kinds + `Function::stack_phi_offsets` side-table

**Tag:** `v16-phase-4-final` after the final commit + push.
**Tag:** `v16-final` on the same commit (or merge marker) after Phase 4 completes.

---

## Execution mode

**Subagent-driven development** per `superpowers:subagent-driven-development`:
- Fresh implementer subagent per sub-task with full task text + scene-setting context
- Spec compliance reviewer → code quality reviewer → mark complete → next task
- Continuous execution; no pause between sub-tasks unless a reviewer flags issues

**Model selection per task complexity** (per the skill's guidance):
- Mechanical sweeps (Phase 1, parts of 2.4) → haiku
- Multi-file integration (Phase 2.2-2.5, Phase 3) → sonnet
- Architecture / new code (Phase 4) → opus

**Hard constraints** (carried from the original plan):
- Don't touch `crates/graphwalk/`
- No plan-identifier comments in code or commit messages (no "Phase N" / "Task M" / "Bug X")
- Never `--no-verify` or `commit --amend`
- Push after every commit to `origin/rewrite/ai`
- Per-commit gates: build + clippy `-D warnings` + doc + test (modulo pre-existing tolerated failures established in Task 0.2)
- Per-phase tagging: `v16-phase-N-final`; final tag `v16-final` after Phase 4

## Critical files (one-line index)

- Original v16 plan with full task code: `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md`
- Memory subsystem deep-dive (Phase 4 background): `reviews/memory-subsystem-deep-dive.md`
- Prerequisites audit (this branch's state): `reviews/rewrite-strdier-v16-prerequisites.md`
- Lessons from simplification/ai1 attempt: this document (above)

## Final merge

After all 4 phases complete on `rewrite/ai` and `v16-final` is tagged, merge back into `rewrite/strdier`:

```bash
git checkout rewrite/strdier
git merge --no-ff rewrite/ai -m "Merge v16 structural redesign from rewrite/ai"
git push origin rewrite/strdier
```

Use `--no-ff` to preserve the v16 branch history as a visible merge commit. Don't delete `rewrite/ai` until you've confirmed the merge is healthy on origin.

## Verification end-to-end

After all 4 phases complete:
1. `git log v16-phase-0-baseline..v16-final --oneline` — clean phase tags, ~20-30 commits total
2. `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — all 4 gates green (modulo tolerated failures from Task 0.2)
3. `grep -rn "ControlState\|StackStorePhi\|StackStoreDetect" crates/strider-*/src --include="*.rs"` — zero hits (all renamed/deleted)
4. `grep -rn "BuiltFunctionGraph\|FunctionArg\s*{" crates/strider-*/src --include="*.rs"` — zero hits
5. Function struct exists at `crates/strider-ir/src/function.rs` carrying graph + entry + cc_metadata + 5 side tables + `arg_index_to_nodes` + `partition_table`
6. New `MemPartition` + `MemUnion` kinds present in `crates/strider-ir/src/node/kind.rs`; `NodeOutputKind::Memory(Option<MemPartitionId>)`
7. `AliasSplit` pass present at `crates/strider-analyze/src/opt/alias_split/mod.rs`; runs in the default pipeline
8. `dump_per_region` of a fixture binary produces partitioned IR (sanity test — manual)
