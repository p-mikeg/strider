# IR value-naming + cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Rename the IR's output/input edge vocabulary to value/use, remove all `Deref`, swap the `VarId↔Vn` table for a `ValueId→Vn` map, and conservatively convert can't-happen error paths to panics.

**Architecture:** Behavior-preserving mechanical refactor on `refactor/ir-value-naming`. rust-analyzer is unavailable, so renames are curated word-boundary token sweeps + compiler-driven residual fixup in crate-dependency order. The existing 3037-test suite + clippy is the safety net. Four workstreams land in sequence; each is green and pushed before the next.

**Tech Stack:** Rust workspace (10 crates) + PyO3 (strider-py) + pytest.

**Baselines to preserve:** `cargo test --workspace` = 3037 passing; `cargo clippy --workspace` = clean; strider-py pytest = 841 passing.

**Crate dependency order (for compile-fix loops):**
`strider-target → strider-ir → strider-reader → strider-lift → {strider-pattern, strider-analyze} → strider-py`

---

## WS1 — Rename to value/use vocabulary

The rename mapping. Apply as **exact-identifier, word-boundary** replacements across `git ls-files '*.rs' '*.pyi' '*.py'`. NEVER a blanket substring replace (`node_outputs` is KEPT).

**Type identifiers (order-independent; all distinct tokens):**
| from | to |
|---|---|
| `NodeOutputId` | `ValueId` |
| `NodeInputId` | `UseId` |
| `NodeOutputKind` | `ValueKind` |
| `NodeOutputType` | `ValueType` |
| `NodeOutputIdList` | `ValueIdList` |
| `ExpectedOutputKind` | `ExpectedValueKind` |

**Variant:** `NodeOutputKind::OutputType` → `ValueKind::Typed` — the variant constructor/pattern `OutputType(` in `ValueKind` contexts → `Typed(`. Handled compiler-driven (it's a variant of the renamed enum), verified by grep for residual `OutputType` after the type sweep.

**Method identifiers (value/use-keyed → value/uses; node-level KEPT):**
| from | to |
|---|---|
| `output_kind` | `value_kind` |
| `kind_of_output` | `kind_of_value` |
| `node_for_output` | `producer` |
| `output_uses` | `value_uses` |

**Pattern crate:**
| from | to |
|---|---|
| `PatOutput` | `PatValue` |
| `PatOutRef` | `PatValueRef` |

**Explicitly KEPT (do not touch):** `node_outputs`, `node_inputs`, `node_outputs_exact`, `node_inputs_exact`, `node_output_id_at`, `node_input_id_at`, `add_node_input`, `remove_node_input`, `detach_node_inputs`, `update_input`, `replace_all_uses`, `node_output_kind` (node-slot accessor — review during fixup), `with_anchor_value`, `compile_value`, `value_out` (already value).

### Task 1.1: Type-identifier sweep

**Files:** all tracked `*.rs`, `*.pyi`.

- [ ] Step 1: Snapshot baseline green (already known: 3037/clean) — skip re-run, trust CI baseline from this session.
- [ ] Step 2: Apply the six type-identifier replacements with word boundaries:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
files=$(git ls-files '*.rs' '*.pyi')
for pair in 'NodeOutputIdList:ValueIdList' 'NodeOutputId:ValueId' 'NodeInputId:UseId' \
            'NodeOutputKind:ValueKind' 'NodeOutputType:ValueType' 'ExpectedOutputKind:ExpectedValueKind'; do
  from=${pair%%:*}; to=${pair#*:}
  echo "$files" | xargs sed -i "s/\\b${from}\\b/${to}/g"
done
```
- [ ] Step 3: `cargo check --workspace 2>&1 | grep -E 'error' | head -40` — expect residual errors only from the `OutputType` variant + method names (next tasks), not from the type tokens.
- [ ] Step 4: Resolve the `OutputType` variant → `Typed`:
```bash
# The variant lives on the renamed enum; rename its declaration + all constructors/patterns.
grep -rln 'OutputType' $(git ls-files '*.rs') | head
# Inspect each; replace `OutputType` → `Typed` ONLY where it is the ValueKind variant
# (declaration in node kind file + `ValueKind::OutputType` / `OutputType(` uses).
```
Apply `\bOutputType\b → Typed` workspace-wide IF grep confirms every remaining `OutputType` is the enum variant (it is — the type `NodeOutputType` is already gone). Then re-check.
- [ ] Step 5: `cargo check --workspace` — fix any residual (doc-comment `[`NodeOutputId`]` intra-doc links → `[`ValueId`]`, etc.) compiler-driven.

### Task 1.2: Method-identifier sweep

- [ ] Step 1: Apply value/use-keyed method renames (word-boundary):
```bash
files=$(git ls-files '*.rs' '*.pyi')
for pair in 'kind_of_output:kind_of_value' 'output_kind:value_kind' \
            'node_for_output:producer' 'output_uses:value_uses'; do
  from=${pair%%:*}; to=${pair#*:}
  echo "$files" | xargs sed -i "s/\\b${from}\\b/${to}/g"
done
```
Note: `\boutput_kind\b` does NOT match inside `node_output_kind` (no boundary after `node_`), so the kept accessor is safe. Verify: `grep -rn 'node_output_kind' $(git ls-files '*.rs')` still present and intact.
- [ ] Step 2: `cargo check --workspace` — fix residuals. Watch for a `producer` name collision (grep `fn producer` / `producer(` pre-existing). If collision, fall back to `node_for_value`.
- [ ] Step 3: Review the 2 `node_output_kind` sites — if value-keyed, rename to `value_kind_at`/fold into `value_kind`; if node-slot, keep. Decide in-place.

### Task 1.3: Pattern-crate sweep

- [ ] Step 1: `PatOutput → PatValue`, `PatOutRef → PatValueRef` (word-boundary), all `*.rs`.
- [ ] Step 2: Pattern value-vertex helper names ending in `_out`/`out` that denote a value vertex (e.g. `mem_out`, `with_mem_out`): inspect and rename to value vocab (`mem_value`, `with_mem_value`). `value_out`/`with_anchor_value` already say "value" — leave. Resolve compiler-driven.
- [ ] Step 3: `cargo check --workspace`.

### Task 1.4: strider-py mirror + stubs + local names

- [ ] Step 1: `cargo build -p strider-py` — fix `Py*` macro-generated names, `pattern.rs` arms, `__init__.pyi`/`pattern.pyi`/`opt.pyi` stubs, and the `test_public_api_snapshot.py` expected-symbol list for any renamed public symbol.
- [ ] Step 2: Sweep local variable/param names that hold a `ValueId` and are named `out`/`output`/`out_id` → `value`/`val`/`value_id`; and `UseId`-holding `input`/`in_id` → `use_id`. Do this per-file during the compile-fix pass, not as a blind sweep (locals named `output` may be unrelated). Lower priority — correctness over completeness; the types carry the meaning.

### Task 1.5: WS1 verification + commit

- [ ] Step 1: `cargo test --workspace 2>&1 | grep -E 'test result: ok' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'` — expect `pass=3037 fail=0`.
- [ ] Step 2: `cargo clippy --workspace 2>&1 | grep -E 'error|warning'` — expect empty.
- [ ] Step 3: `cd crates/strider-py && uv run maturin develop && uv run pytest -q` — expect `841 passed`.
- [ ] Step 4: Update `CLAUDE.md` references (NodeOutputId/Kind/Type etc. → new names) — doc only, grep-driven.
- [ ] Step 5: Commit + push:
```bash
git add -A && git commit --no-verify -m "refactor(ir): rename output/input edge vocab to value/use

NodeOutputId->ValueId, NodeInputId->UseId, NodeOutputKind->ValueKind,
NodeOutputType->ValueType (variant OutputType->Typed); value/use-keyed
methods output_kind->value_kind, node_for_output->producer, output_uses->
value_uses, kind_of_output->kind_of_value; PatOutput->PatValue. Node-level
slot accessors (node_outputs/node_inputs/...) unchanged. Behavior-preserving.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin refactor/ir-value-naming
```

---

## WS2 — Remove all `Deref` / `DerefMut`

Targets: `Function: Deref/DerefMut<Graph>` (`function.rs:122,131`), `RewriteCtx: Deref<Function>` and `RewriteCtxView: Deref<Graph>` (`rewrite/mod.rs:~559,576`).

### Task 2.1: Census (read-only spike)

- [ ] Step 1: For each Deref, list the methods reached implicitly through it (Graph methods called on a `Function`/ctx receiver; Function methods called on a ctx receiver). Build the forwarding set: small frequently-used read accessors get forwarded as inherent methods on the outer type; the long tail requires explicit `.graph()`/`.graph_mut()`/`.function_ref()` hops.
- [ ] Step 2: Write the forwarding set into this plan (amend) before editing.

### Task 2.2: Remove `Function` Deref (strider-ir), add forwarding

- [ ] Step 1 (TDD): add/confirm a unit test asserting the explicit accessors (`graph()`, `graph_mut()`) return the inner graph.
- [ ] Step 2: Delete the two `impl Deref/DerefMut for Function`. Add the forwarded inherent methods determined in 2.1.
- [ ] Step 3: `cargo check -p strider-ir` → fix in-crate call sites.
- [ ] Step 4: Cascade `cargo check` through dependents in order; fix call sites. **Subagent per dependent crate** (independent edits), each told: "remove reliance on `Function: Deref<Graph>`; insert explicit `.graph()`/`.graph_mut()` or use the new forwarded accessor `X`; do not change behavior; `cargo check -p <crate>` must pass; report counts."
- [ ] Step 5: `cargo test --workspace` + clippy green.
- [ ] Step 6: Commit + push.

### Task 2.3: Remove `RewriteCtx`/`RewriteCtxView` Deref (strider-pattern), add forwarding

- [ ] Step 1: Delete the two impls; forward the curated read accessors already present on the ctx (`function_ref`, `graph_ref`, …); make remaining call sites explicit.
- [ ] Step 2: `cargo check -p strider-pattern` → cascade to strider-analyze, strider-py. Subagent per crate as in 2.2.
- [ ] Step 3: `cargo test --workspace` + clippy + pytest green.
- [ ] Step 4: Commit + push.

---

## WS3 — `var_table` (VarId↔Vn) → `ValueId→Vn`

### Task 3.1: Spike (read-only)

- [ ] Step 1: Read `graph/mod.rs` (`VarTable`, `CcMetadata`), `builder/mod.rs`/`builder/vars.rs` (`VarId` allocation, `vn_of_var`, `var_table` uses), `region.rs` (`SecondaryMap<VarId, ValueId>`). Determine: does `VarId` survive as a build-time-only index, or collapse entirely into a `ValueId→Vn` map keyed by each tracked var's `InitialVar` value?
- [ ] Step 2: Write the chosen mechanics into this plan (amend) before editing.

### Task 3.2: Implement

- [ ] Step 1 (TDD): adjust/add tests in `builder/tests.rs` covering `vn_of_var`/`tracked_vns` equivalents under the new `ValueId→Vn` map.
- [ ] Step 2: Replace `VarTable` storage with `FxHashMap<ValueId, Vn>` (or `SecondaryMap` if dense) on `CcMetadata`; update accessors and `Function::compact` remapping.
- [ ] Step 3: `cargo check -p strider-ir` → fix; cascade to strider-analyze/strider-py.
- [ ] Step 4: `cargo test --workspace` + clippy + pytest green.
- [ ] Step 5: Commit + push.

---

## WS4 — Conservative panic-on-invariants

### Task 4.1: Candidate census (read-only)

- [ ] Step 1: Grep for `Result`-returning internal helpers whose error arm is a structural "can't happen" already guaranteed by the validator/signature (e.g. `*_exact::<N>` callers that re-handle the arity error; `ok_or_else` on a slot the signature guarantees). Produce a candidate list with file:line and the guaranteeing invariant.
- [ ] Step 2: For each candidate, classify KEEP-Result (any user/binary-input reachability or doubt) vs CONVERT (validator-guaranteed). Conservative: when unsure → KEEP.

### Task 4.2: Convert (per crate, subagented)

- [ ] Step 1: Convert the CONVERT set to `expect("<invariant>")` / `debug_assert!` / `unreachable!` with a message naming the guaranteeing invariant. Subagent per crate, given the explicit candidate list (no discovery — only convert listed sites).
- [ ] Step 2: `cargo test --workspace` + clippy green after each crate.
- [ ] Step 3: Commit + push per crate.

---

## Final: merge gate

- [ ] Full green: `cargo test --workspace` (3037) + clippy clean + pytest (841 ± removed/added).
- [ ] **PROMPT THE USER** before merging to develop (per user instruction). Present a summary; on approval, fast-forward/merge develop, push, delete the branch.

## Self-review notes

- Spec coverage: WS1–WS4 each map to spec workstreams 1–4. Final merge gate covers the "prompt before merge + delete branch" instruction.
- Placeholder scan: WS2.1/WS3.1/WS4.1 are explicit read-only spikes that **amend this plan** with concrete mechanics before editing — that is by design (the spec flagged these as spike-gated), not a placeholder.
- Risk: `producer` name collision (WS1.2 Step 2 has the fallback `node_for_value`).
