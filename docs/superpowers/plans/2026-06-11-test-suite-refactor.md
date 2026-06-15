# Test Suite Refactor & Edge-Case Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the ~47k-LOC Rust test suite (plus the ~350-test Python suite) easier to write, read, and extend — by extracting shared utilities that kill the measured boilerplate — and add edge-case tests for every complex production function, while guaranteeing that **every edge case currently tested still has a test afterwards**.

**Architecture:** Test-only changes. New shared helpers live in `strider-ir-test-utils` (workspace-wide) and per-crate `test_support` / `tests/common` modules. Refactors consolidate repetitive tests into case-table loops and helper calls (no new deps like rstest — case tables are the established style, see `crates/strider-ir/src/builder/vn_io.rs` tests). New edge-case tests pin **current observed behavior**.

**Tech Stack:** Rust (cargo test, proptest where already present), Python (pytest + parametrize), criterion only for the optional bench task.

**Branch:** `feature/test-refactor` (from develop). Push to `origin feature/test-refactor` after every commit. PR to develop at the end; **do not merge** — the user must approve the merge.

---

## Hard rules for every task

1. **No production code changes.** Only files under `tests/`, `#[cfg(test)]` modules, `test_support` modules, `strider-ir-test-utils`, and Python test files may change. If an edge-case test reveals what looks like a production bug (panic, wrong value), do NOT fix it: write the test pinning the *current* behavior with a `// NOTE: pins current behavior; possible bug:` comment if it's sane to keep, or report it back as a finding if the behavior cannot be sanely pinned. Report all suspected bugs in your final answer.
2. **Edge-case preservation protocol.** You may delete/rename/merge `#[test]` fns freely, but for every removed test name you MUST list in your final report: `old_test_name -> new test fn (and case within it)`. A reviewer verifies this mapping. Never drop an assertion's *distinguishing input* when merging tests into a case table — every old (input, expected) pair becomes a row.
3. **Pin, don't aspire.** New edge-case tests assert what the code actually does today. Run the test, observe, then assert the observed value (sanity-check it first).
4. **Style:** match existing test style in each crate; workspace lints apply (`cargo clippy --workspace` must stay clean). unwrap/expect/panic are fine in tests. Use `entity-utils` collections when keying by NodeId/ValueId. No `Arc`/`Send`/`Sync`. No rstest/new deps (except criterion in the bench task). Never mention plan/task identifiers in code or commit messages.
5. **Gates per task:** `cargo test -p <touched crates>` green, then commit with a conventional message (`test(<area>): ...`), then `git push origin feature/test-refactor`.
6. **Known intentional behaviors — do NOT flag or "fix":** missing CallOther ABI entries are intentional; stack-arg over-collection is intentional policy; LoadForward never synthesises value-Phis; manual kill_node after replace_value is load-bearing.

---

### Task 1: Extract the shared `Tb` graph builder into strider-ir-test-utils

**Files:**
- Create: `crates/strider-ir-test-utils/src/tb.rs` (content = `crates/strider-pattern/tests/pattern_matching/support/graph.rs`, formatting-canonical version)
- Modify: `crates/strider-ir-test-utils/src/lib.rs` (add `mod tb; pub use tb::Tb;`)
- Delete: `crates/strider-pattern/tests/pattern_matching/support/graph.rs`, `crates/strider-orchestrator/tests/pattern_matching/support/graph.rs`
- Modify: every `use ...support::graph::Tb`-style import in both crates' tests to `use strider_ir_test_utils::Tb;` (strider-pattern already dev-deps test-utils? verify; orchestrator: check Cargo.toml, add `strider-ir-test-utils.workspace = true` to dev-deps if missing)

The two copies are formatting-only duplicates (verified by whitespace-stripped diff); imports are only strider-ir + strider-ir-test-utils, so the move is dependency-clean.

- [ ] Move file, fix module wiring + imports
- [ ] Run: `cargo test -p strider-pattern -p strider-orchestrator -p strider-ir-test-utils` → green
- [ ] `cargo clippy -p strider-ir-test-utils -p strider-pattern -p strider-orchestrator` → clean
- [ ] Commit `test(utils): extract shared Tb graph builder into strider-ir-test-utils` + push

### Task 2: strider-opt — test_support utilities + refactor repetitive tests

The survey found these boilerplate shapes in `crates/strider-opt` (~388 tests):
- A: `make_fn(|b| { consts → binop → ... })` preludes (~88×) — fine as-is, keep.
- C: fixed-point while-loops `let mut changed = true; while changed {...}` (~46×)
- D: pipeline setup via `test_support::standard_test()` / `cf_rp_pipeline()` (~72×) — already helpers, keep.
- E: return-value assertions `assert_eq!(return_kind(...)?, NodeKind::IntConst(...))` (~30×)

**Files:**
- Modify: `crates/strider-opt/src/test_support.rs` — add:

```rust
/// Run `pass` repeatedly until it reports no change; returns total iterations.
pub fn run_to_fixed_point(
    pass: &mut dyn Optimizer,
    fg: &mut Function,
    ctx: &mut OptCtx<'_>,
) -> Result<usize> { /* loop over pass.run_one(...).changed() */ }

/// Assert the function's single Return returns exactly `expected` kind.
pub fn assert_return_kind(fg: &Function, expected: NodeKind) { ... }

/// Assert the function's Return value is IntConst with this small payload.
pub fn assert_returns_const(fg: &Function, expected: u64) { ... }
```

(Adapt signatures to whatever `Optimizer::run_one` actually takes — read the trait first.)
- Modify: the ~46 while-loop sites and ~30 return-assert sites across `src/*/tests.rs` to use the helpers.
- Where a file has 3+ near-identical tests differing only in (op, inputs, expected), consolidate into one case-table test, preserving every row + the old test names in row comments or assert messages.

- [ ] Add helpers, refactor sites file-by-file, keep `cargo test -p strider-opt` green throughout
- [ ] Produce the old→new mapping for every removed `#[test]`
- [ ] Commit `test(opt): consolidate fixed-point and return-shape assertions into test_support` + push

### Task 3: strider-opt — new edge-case tests (pin current behavior)

Add tests for the gaps the survey identified (one commit; group by pass; each test small and named for the edge):

- **ConstantFold eval:** div-by-zero (int Div/Sdiv/Rem/Srem with const 0 divisor — observe: fold? skip? keep node?), `i32::MIN % -1`-shaped Srem/Sdiv overflow, Shl/Lshr/Ashr with shift ≥ bit width and == bit width − 1, folds at I1 (1+1, 1&1), wide-const (I128) operands through a fold (likely skipped — pin that).
- **KnownBits:** ZeroExtend vs SignExtend upper-bit contribution, I80/I128 gating (pass must skip, pin), Ashr sign-bit smear, And with all-ones identity at I1.
- **LoadForward / memory_ssa:** MemPhi with 3 predecessors blocks forwarding; partially overlapping store (offset+1, width 4 over width 8) blocks; exact-match store forwards through a longer disjoint chain that *contains* a same-address narrower store (must block).
- **FlagCmpCanonicalize:** malformed/incomplete flag tree (missing OV operand) → no rewrite, graph unchanged; swapped operand order in the inner Equal.
- **StackOffsetDetect:** nested `Add(Add(Add(sp,k1),k2),k3)`, negative offset via `Add(sp, Neg(K))`, offset that wraps i64.
- **FunctionArgDetect:** register args at index 1..3 (not just 0), interleaved register+stack ordinals, function reading no arg registers at all.
- **CallStackArgCollect:** window with a gap (slot 0 and 2 stored, slot 1 not), zero stack args (call with only register args).
- **indirect_branch_resolve table:** entry_count 0, single-entry table, stride mismatch vs entry_size.
- **RedundantPhis/PhiCollapse:** `Phi(x, x)` two identical reachable inputs (observe — does it collapse? pin), Phi feeding Phi cascade, MemPhi in a dead branch culled by DBE+CfgDetach.
- **value_range:** Sless guard, guard via the false edge (inverted), guard on unrelated value leaves index unbounded.

- [ ] Write each test red-green style: run it, observe actual behavior, assert it
- [ ] `cargo test -p strider-opt` green, clippy clean
- [ ] Commit `test(opt): pin edge-case behavior across folding, memory-SSA, and arg passes` + push

### Task 4: strider-ir — refactor + edge-case tests

Refactor (survey-measured):
- Const-vs-nonconst pair tests in `src/builder/tests.rs` (~30×): add a local helper `fn assert_folds_to_const(...)` / `fn assert_emits_node(...)` or a case-table per op family.
- Validate-corruption tests in `src/validate/tests.rs`: extract `fn build_spine(f) -> (entry, mem, ...)` prelude + `fn assert_validation_err(f, entry, pred)` helper.
- Wide-const interning tests: case-table over `[I80, I128, I256, I512]`.

Edge cases to add (pin current behavior):
- `create_node_attributed` const canonicalisation: small→wide promotion for I80 and I256 (only I128 is covered today), masking at exactly 64 bits, IntConst at I1 with payload > 1 (masked to 1?).
- vn_io: UNIQUE-space sub-register containment read; write narrower value into wider container then read back full container (high-bit preservation); >16-byte container sub-register alias → error path (wide-container guard message).
- `dedup_overlapping_largest`: empty input; two identical vns; partial (non-nested) overlap — pin whatever it does.
- `Function::container_of`: CC-referenced register not in tracked set; ad-hoc vn maps to itself.
- NodeCache: Region/Phi/MemPhi/Call never dedup (two identical Regions → distinct ids); eviction — kill a node then create the same shape → fresh node works.
- EditFunction: replace_value where new value's producer is currently dead (revives?); kill then is_root bookkeeping; cull_dead on already-clean graph is a no-op.
- compact/retain_reachable: side-tables (stack_offsets, asm_fingerprints, value_vn, arg_index_to_values) survive remap — build fn with entries, compact, assert lookups still resolve.
- walk family: postorder on a diamond yields each node once; walk_from mid-graph.

- [ ] Refactor first (separate commit), then edge cases (second commit), old→new mapping for removals
- [ ] `cargo test -p strider-ir` green, clippy clean
- [ ] Commits: `test(ir): consolidate builder/validator test boilerplate` and `test(ir): pin edge cases in aliasing, dedup cache, compact, and EditFunction` + push each

### Task 5: strider-pattern — refactor + edge-case tests

Refactor:
- Switch `tests/pattern_matching/support/` to the shared `Tb` (done in Task 1 — verify no leftovers).
- Add to `support/assertions.rs`: `fn commutes(function, build_pat: impl Fn() -> ..., l, r)`-style helper that asserts both operand orders match (kills the 12 hand-written A/B pairs), and a capture-extract-assert helper.
- Template tests: extract the 60-LOC match→instantiate→verify scaffold into a helper.

Edge cases:
- find_joined with 3 patterns sharing one capture; all-patterns-empty early exit; asymmetric captures (capture in A only).
- Multi-sink pattern root derivation (root() Result), cyclic pattern graph → error, disconnected pattern → error.
- Commutative swap interacting with capture conflict: `add(var(x), var(x))` on `5+3` (no match) and on `5+5` (match).
- when_match returning false on first order → swap still attempted (or not — pin).
- Template with unbound capture → instantiate error path (pin error, don't panic-test unless it panics — then `#[should_panic]` with note).
- Cast walk-through: producer with 0 inputs (IntConst) stops walk; deep cast chain (~32 casts) still matches.
- bool_binary I1-guard: same-shaped I64 And does NOT match bool_and.

- [ ] Refactor commit, then edge-case commit, mapping for removals
- [ ] `cargo test -p strider-pattern` green, clippy clean
- [ ] Commits: `test(pattern): shared assertion helpers for commutativity, captures, templates`, `test(pattern): pin matcher and template edge cases` + push each

### Task 6: orchestrator + cfg + lift — refactor + edge-case tests

Refactor:
- `tests/common/indirect_resolve_helpers/classify.rs`: extract a tiny insn-sequence builder (`fn x86_64_snippet(insns: &[(&[u8], &str)]) -> Vec<u8>` that concatenates + pads 64×0xcc) and use it in the 9 scenario builders.
- strider-cfg `cfg_build_end_to_end.rs`: keep helpers, generalize `build_from_bytes*` only if trivial.

Edge cases:
- strider-cfg RegionBuilder: back-jump target exactly at `fn_max_size` boundary; conditional branch whose both targets are OOB; region split where split point is the entry address itself; `fn_max_size` smaller than the first instruction.
- strider-cfg types: PcodeInsnAddr ordering antisymmetry edge (equal machine addr, differing index) — extend existing tables if not present.
- orchestrator: analyze() on a function whose indirect branch never resolves → returned in `unresolved_indirect_branches`, placeholder still present, NOT an error (may already exist — verify, strengthen assertions on the address value); analyze() with pre-seeded `known_targets` skips re-resolution.
- strider-lift: LiftOptions defaults pinned (exists — extend with per_address_ccs knob), lift of a snippet that reads a sub-register written wide (eax write → ax read) asserting mask/shift shape e2e.

- [ ] Refactor + edge cases, per-crate gates: `cargo test -p strider-cfg -p strider-lift -p strider-orchestrator`
- [ ] Commit `test(cfg,lift,orchestrator): snippet builder + boundary edge cases` + push

### Task 7: target + reader — refactor + edge-case tests

Refactor:
- `cc_validation.rs` + unit tests: local `fn vn(off)` is fine; consolidate the negative-validation tests into a case table of (layout-mutation, expected-error-substrings).

Edge cases:
- StackArgs: `offset_of`/`index_of`/`slot_of` at boundaries — offset exactly at base, one byte before base, offset in the middle of a slot, size spanning two slots (index_of None?), slot_of for wider-than-slot anchor; increment 4 vs 8.
- positional_arg_layout: no stack args (`stack: None`) — first_stack_index/stack_offset_of behavior; zero registers + stack-only.
- call_other_abi::classify: unknown name → verify the actual fallback variant (do NOT add new table entries); NoOp and NoReturn arms per preset where entries exist.
- reader: MemRegionsLookupTable read spanning two adjacent regions (fill-all-or-error contract — pin), read at exact region end, zero-length read; relocation applied to a slot at region boundary.

- [ ] Per-crate gates: `cargo test -p strider-target -p strider-reader`
- [ ] Commit `test(target,reader): boundary math and region-contract edge cases` + push

### Task 8: Python test suite — parametrize + conftest consolidation + edge cases

Refactor:
- Consolidate the duplicated graph-builder preludes (`_build_graph`, `_patterns_graph_for`, etc.) into conftest fixtures (`built_graph(arch, case, symbol)` factory fixture).
- Parametrize the builder-finalization tests (ret/if_/call_other/load/store/phi → one `@pytest.mark.parametrize`).
- Move `CountingReader`/`ConstReadOnlyMemory` into a shared helper module.

Edge cases:
- BufferReader: zero-length read, read ending exactly at region end, read starting at last byte, len-1 buffer.
- Pattern builders: invalid arg types raise (TypeError/StriderError — pin which), `.arg(i)` with huge index, reserved capture name paths.
- unresolved_indirect_branches: two unresolved sites in one function → both addresses reported.
- Analyzer per-call overrides: override pipeline_factory honored; override on one call doesn't leak to the next.

Gate: `cd crates/strider-py && uv run pytest -x -q` (build first with `uv run maturin develop` if needed; the Rust side didn't change, so a rebuild may be unnecessary — check it's importable first).

- [ ] Commit `test(py): parametrize builder suites, shared fixtures, boundary edge cases` + push

### Task 9: Criterion micro-benches (small)

- Create `crates/strider-pattern/benches/matcher.rs`: `find_all` of a 3-node pattern over a synthetic ~2k-node function (built via Tb in a loop).
- Create `crates/strider-opt/benches/pipeline.rs`: `default_pipeline().run` over a synthetic constant-folding chain (~2k nodes).
- Add `criterion.workspace = true` (add to workspace deps, `[[bench]] harness = false`) as dev-deps in those two crates only.
- Gate: `cargo bench -p strider-pattern -p strider-opt -- --test` (smoke mode, no full sampling).
- [ ] Commit `bench: matcher and optimizer pipeline micro-benchmarks` + push

### Task 10: Final verification + PR

- [ ] `cargo test --workspace` — all green; compare per-crate result lines against the baseline in `/tmp/baseline_tests.txt` (no suite lost; counts may shift due to consolidation — cross-check with the per-task old→new mappings)
- [ ] `cargo clippy --workspace` — clean
- [ ] `cd crates/strider-py && uv run pytest -q` — green
- [ ] Final code-quality review subagent over the whole diff (`git diff develop...feature/test-refactor`)
- [ ] Open PR to develop via the GitHub compare URL (no gh CLI): `https://github.com/<org>/<repo>/compare/develop...feature/test-refactor` — print the URL with a PR body
- [ ] **Stop and ask the user before any merge.**

## Self-review notes

- Spec coverage: utilities (Tasks 1,2,4,5,6,7,8), edge cases per complex function (3,4,5,6,7,8), benches (9), preservation guarantee (hard rule 2 + Task 10 cross-check), Rust prioritized over Python (8 is one task), branch+PR (10).
- Type consistency: helper signatures are sketches; implementers adapt to real trait signatures after reading them (explicitly instructed).
- No placeholders that hide work: each task names exact files, measured repetition counts, and concrete edge-case lists from the survey.
