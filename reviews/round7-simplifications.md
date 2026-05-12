# Round 7 — Simplification Proposal

Rolls every Round 7 audit's removable / consolidatable / shrinkable item into one
shippable plan. Each row cites file:line; each section ends with an estimated
LOC delta and order-of-execution. All citations are absolute paths under
`/mnt/c/Users/mikeg/Documents/strider`.

Trust model: this proposal was written from `round7-*.md` (no `*-r6.md`).

---

## Section A — Code & module deletions

| # | Target | File:line | Justification | LOC | Risk |
|---|--------|-----------|---------------|-----|------|
| A1 | `crates/graphmock/` standalone crate | `crates/graphmock/Cargo.toml`, `crates/graphmock/src/lib.rs` | Sole consumer is `graphwalk`'s `[dev-dependencies]` (verified by grep: only `graphwalk/tests/{preorder,postorder}.rs` import it; `graphwalk/Cargo.toml` is the only manifest entry). Round 7 py-support audit §G recommends this exact move. | ~150 | None — internal-only |
| A2 | `_unused_marker` dummy + unused `Mutex` import | `crates/strider-py/src/reader.rs:686-689` | Dead code; `#[allow(dead_code)]` betrays intent. Round 7 py-support L2. | 4 | None |
| A3 | `PyCfg::inner()` accessor | `crates/strider-py/src/cfg.rs:23-30` | Zero callers in `crates/strider-py/src/`; `#[allow(dead_code)]` is a tell. Round 7 py-support L3. | 8 | None |
| A4 | `ValidationError::InputPointsToMissingOutput` variant | `crates/ir/src/validate/mod.rs:177-182`; documented dead at `crates/ir/src/validate/layer_b.rs:44-49` | Variant exists but is *never emitted* — the layer-B comment explicitly says it isn't checked. Round 7 ir V-1 / D-1. Either implement (preferred per panic audit) or delete. **Proposal: delete the variant** (the actual reachability is enforced upstream by `validate` Layer A's reachable-set scoping); a future "raw-Graph caller" path can re-add a fresh variant when the use case lands. | 6 | Low — public enum variant; `pub use validate::ValidationError`. Bumps semver (workspace-internal only). |
| A5 | `cfg::SpecialTerm::CallIndirect` (suggested by prompt) | — | **Already absent.** `SpecialTerm` lives at `crates/strider/src/strider/pipeline.rs:490-504` and has only `Unresolved`, `Switch`, `TailCall`. No-op for this round. | 0 | n/a |
| A6 | `opt::CallOtherElide` cleanup verification | `crates/opt/src/lib.rs:149-151,181-183` | Round 7 comments §A1 + opt audit §"CallOtherElide cleanup" both confirm zero matches in source. Two doc-strings still reference the deleted pass as "historical context." **Proposal: trim the breadcrumbs to one line each** to reduce visual noise. | 4 | None — comment-only |
| A7 | `pattern::float_is_nan` Python registration | `crates/strider-py/src/pattern.rs:881-885` | Registered but always raises `PatternError("not yet implemented")`. Listed in `EXPECTED_PATTERN` snapshot (`tests/python/test_public_api_snapshot.py:113`). The IR has *no* `FloatIsNan` node — pcode-lift lowers `FloatNan(x)` to `BoolNeg(FloatEqual(x,x))` (verified `pcode-lift/src/value/float.rs:78-90`). **Proposal: implement as `bool_neg(float_eq(x, x))`** (one line; matches the lift-time canonicalisation). Drops a public-API trap. | 4 net (≈ +5 −5) | None — symmetric with lifter |
| A8 | `_unused_marker` companion: stale `pattern.rs` TODO | `crates/strider-py/src/pattern.rs:20-21` | Comment says "op-variant accessors not yet exposed"; they exist at `matcher.rs:119-181`. Round 7 py-support L1. | 3 | None |

**Section A est. LOC removable:** ≈ 175 lines.

---

## Section B — Helper consolidation

| # | Duplicate area | Sites | Proposal | LOC saved |
|---|----------------|-------|----------|-----------|
| B1 | Graph traversal | `graphwalk::PreOrder/PostOrder` (entity-based, used by `ir::walk::walk_graph`); `pattern::Matcher::preorder` cache (`crates/pattern/src/matcher/mod.rs:115-138`); ad-hoc DFS in opt (`opt::stack_load_forward::find_stack_stored_value_at_offset` at `mod.rs:489`, `opt::function_args::mem_chain_is_dirty` at `function_args/mod.rs:393`, `opt::sp_expr::decompose_sp` at `sp_expr.rs:247`) | The `graphwalk` API (entity-set tracker + `PreOrder`/`PostOrder`) already covers the iterative shape needed by all three opt sites that Round 7 scale audit flagged as *recursive without depth bound* (A1, A2, A3). **Convert each opt site to the explicit-stack DFS pattern already used by `walk_control_for_if_bound_iter` (`indirect_branch_resolve/jump_table.rs:407`).** Stack-overflow risk drops, line count is roughly neutral, but mental model unifies. | 0–20 net |
| B2 | PyO3 error wrapping | `crates/strider-py/src/errors.rs` (5 converters: `into_strider_err`, `into_lift_err`, `into_reader_err`, `into_pattern_err`, `into_rewrite_err`) | Round 7 py-support §A confirmed all 5 used at 109 sites across 11 files; there is **no** direct `?` path that bypasses them. **No simplification needed.** Audit closes clean. | 0 |
| B3 | Mock graph fixtures | Sole mock-graph crate is `graphmock`; no parallel implementations elsewhere. | Move into `crates/graphwalk/tests/common/graph.rs` (see A1). One crate-boundary indirection eliminated; no actual logic duplication exists. | (see A1) |
| B4 | SP-expression decomposition / memo | `opt::sp_expr` is the only SP tracker; `opt::stack_load_forward::probe` (`mod.rs:182`) **already uses** sp_expr's memo via `decompose_sp_with_memo`. No external duplicate found. | No work needed. | 0 |
| B5 | CallOther name lookup | `Graph::call_other_name` (`crates/ir/src/graph/store.rs`) stores `Option<String>` keyed on `NodeId`; `target::call_other_abi::classify` (`target/src/call_other_abi.rs`) returns `&'static CallOtherClass` keyed on `(ArchPreset, &str)`. Same name flowing through two spaces. | **Replace `Option<String>` with `Option<Arc<str>>`** (or interned `&'static str` via a per-process intern table when names are statically classified). Allocation savings: each CallOther node currently heap-allocates its name. With `Arc<str>` clones are pointer-bumps. **Estimated savings: ~24 bytes/CallOther × N CallOthers** (memory, not LOC). **LOC delta: minimal (~5 lines in store.rs / builder/call.rs).** | 0–5 |
| B6 | `asm_fingerprint_exempt` set duplicated in doc | `crates/ir/src/graph/mod.rs:96` (doc) vs. `crates/ir/src/validate/layer_c.rs:164-177` (function) — **doc invented `IfCase` and omitted `MemPhi` / `VarPhi` / `ValuePhi` / `StackStorePhi`.** | Either lift the exempt set into a shared `const` named array consumed by both, OR have the doc cite `validate::layer_c::asm_fingerprint_exempt` as the single source of truth. Round 7 ir C-1, comments §A3, pattern §5. | 5 |

**Section B est. LOC removable:** ≈ 20 + memory wins via `Arc<str>`.

---

## Section C — Visibility tightening (`pub` → `pub(crate)`)

Confirmed externally-unused `pub` items:

| # | Item | File:line | Verification | Action |
|---|------|-----------|--------------|--------|
| C1 | `node_signature::{Signature, SlotList, Slot, ExpectedOutputKind, SlotRole}` | `crates/ir/src/node_signature.rs:24, 45, 67, 79, 116` | Module declared `mod node_signature;` (private) at `crates/ir/src/lib.rs:49`; therefore types already inaccessible outside `ir`. The only consumers are `ir::dot::mod.rs:6,117`, `ir::validate::layer_a.rs:3,14`, `ir::validate::mod.rs:14`. **Cosmetic: drop `pub` to `pub(crate)`** for clarity (Round 7 ir D-2). No semver impact. | Mechanical |
| C2 | `Matcher::options_for_test` | `crates/pattern/src/matcher/mod.rs:189` | `pub` despite `_for_test` suffix (Round 7 types §1). External consumers: zero in `crates/`. | Rename → `Matcher::options` and document as "active config for diagnostics", **OR** demote to `pub(crate)` if no external diagnostic consumer exists. |
| C3 | `BuiltFunctionGraph::from_graph_and_entry` | `crates/ir/src/function.rs:94-103` | External consumers: `crates/opt/src/pipeline.rs:100`, `crates/strider/src/rewrite.rs:131`, plus three pattern-crate **test** files (`tests/get_vn_*.rs`). The two production consumers both `mem::take(graph)` and never read the empty CC fields — see Section D / D2. **Demote to `pub(crate)`** after the D2 ctx-newtype lands. | Sequential after D2 |
| C4 | `BuiltFunctionGraph` field access | `crates/ir/src/function.rs:46-79` | Every field is `pub`; allows `&mut graph.nodes` from any holder of `&mut BuiltFunctionGraph`. Round 7 types §5 / ir C-2 reasoning. | Demote fields to `pub(crate)`; expose `&Graph` / `entry()` / `variables()` accessors. |
| C5 | `BuiltCallingConvention` field access | `crates/target/src/calling_convention/mod.rs:88-140` | All 8 fields `pub`. `CallingConvention` itself is properly private; the *built* version's disjointness invariant is doc-only. Round 7 types §4. | Demote fields; expose accessors; add `debug_assert!` for disjointness in `build()`. |
| C6 | `Cfg.entry` / `Cfg.sleigh` / `Cfg.graph` | `crates/cfg/src/cfg/mod.rs:37-60` | `pub` fields permit invalidation. Round 7 types §8. | Demote to `pub(crate)`; expose `entry()`, `take_sleigh() -> Option<...>`, `graph()`/`graph_mut()`. |
| C7 | `Outputs` / `Inputs` `Index` impls | `crates/ir/src/iterators.rs:37-40, 91-97` | By-design panic on OOB. ~30 production callers, all locally guarded. Round 7 panics audit §"Risky indexing." | Either delete the `Index` impls (forces `.get(N)?`) **or** add `#[doc = "# Panics …"]` and a workspace-wide audit comment. |

**Section C est. LOC removable:** 0 net (visibility-only); semver-internal tightening.

---

## Section D — API shrinkage / surface reduction

| # | Item | File:line | Action | Rationale |
|---|------|-----------|--------|-----------|
| D1 | `PyPat::ordered()` silent no-op | `crates/strider-py/src/pattern.rs:432-438` | **Remove** the method, OR convert to `Err(PatternError("ordered() is supported on typed builders only — use int_binary(..) / bool_binary(..) / float_binary(..)"))`. Round 7 pattern §4, py-support C2. | Removing a no-op trap closes a documented footgun. The typed-builder route already exists (`crates/strider-py/src/pattern.rs:1720, 1762, 1804`). |
| D2 | `BuiltFunctionGraph::from_graph_and_entry` empty-CC contract leak | `crates/ir/src/function.rs:94-103`; consumers at `crates/opt/src/pipeline.rs:100`, `crates/strider/src/rewrite.rs:131` | Demote to `pub(crate)`; introduce `pub struct GraphRewriteCtx<'a> { graph: &'a mut Graph, entry: NodeId }` for `pattern::rewrite_rule`. Both production callers already `mem::take(graph)` — they pay the contract leak for **zero** gain. Round 7 types §5 / strider-target C1. | Eliminates a documented "empty-fields-are-safe-because-only-graph+entry-are-read" comment from two call sites. |
| D3 | `OptimizerPipeline::add` accepts any pass | `crates/opt/src/pipeline.rs:180-220`; pre-builts at `lib.rs:106-194` | Phantom-type as `OptimizerPipeline<Stable>` / `<Destructive>` / `<Full>`; per-pass marker trait (`StableOptimizer`, `DestructiveOptimizer`). `default_pipeline() -> OptimizerPipeline<Full>`; the orchestrator's stable loop body type-checks that no `Destructive` pass slips into mid-iteration runs. Round 7 types §6. | Today the contract is doc-only and caught only by `pipeline_subsets.rs` shape tests. Encoding it removes the "must remember" rule. |
| D4 | `cfg::Builder::new()` defaults to X86_64 + Little | `crates/cfg/src/cfg/builder/mod.rs:94-118` | Replace with `Builder::for_arch(SleighArch)` that sets endianness + preset atomically; deprecate `new()`. Round 7 pcode-lift-cfg D2. | Direct cfg consumers (Python / examples / ad-hoc tests) silently misclassify CallOthers + decode wrong-endian on non-x86_64. |
| D5 | `pattern::float_is_nan` (Python) | `crates/strider-py/src/pattern.rs:881-885` | Implement as `bool_neg(float_eq(x, x))` (matches lifter at `pcode-lift/src/value/float.rs:78-90`). Round 7 pattern doc claim, py-support M2. | Removes a public-API stub that always raises despite being in the snapshot. |
| D6 | `IfPat`'s symmetric-matching docstring lie | `crates/pattern/src/pat/ctor/control.rs:120-127`; mirrored at `crates/strider-py/src/pattern.rs:1564-1568` | Delete the "symmetric matching" paragraph; replace with a one-line note that `opt::IfCondInversion` canonicalises `If(BoolNeg(C))` upstream. Round 7 pattern §1, comments §C3/D. | Doc lie, not a bug. Easy fix; high user-visibility (this is the `if_node()` constructor). |
| D7 | `PhiPat` + `phi()` claim "any phi node" | `crates/pattern/src/pat/ctor/control.rs:39-48`; `crates/pattern/src/pat/builders/phi.rs:36-48`; mirrored Python at `pattern.rs:550-553, 567-577` | Either rename `phi()` → `var_phi()` and add `mem_phi()` / `value_phi()` ctors, OR make `phi()` truly polymorphic. Round 7 pattern §2, comments §C5/D. | Doc lie + a real coverage gap (memory-flow / `StackLoadForward`-introduced value phis are silently unmatchable). |
| D8 | `pattern::float_cmp_any` mentions nonexistent `FloatCmpOp::NotEqual` | `crates/pattern/src/pat/ctor/variant_agnostic.rs:197` | Rewrite docstring: "`Equal` is commutative; `NotEqual` and `LessEqual` are not IR primitives — use `pattern::float_ne` / `pattern::float_le` aliases." Round 7 pattern §3, comments §C4/D. | Doc lie referencing a deleted variant. |
| D9 | `Graph::asm_fingerprints` exempt-set doc lists ghost `IfCase` | `crates/ir/src/graph/mod.rs:96` | Remove `IfCase`; either enumerate the actual exempt set or cite `validate::layer_c::asm_fingerprint_exempt`. Round 7 ir C-1, comments §A3/D, pattern §5. | Documentation contract drift. |
| D10 | `FunctionBuilder::set_lift_addr(Option<u64>)` paired-call funnel | `crates/ir/src/builder/mod.rs:137, 376-386`; sole bracket-pair at `crates/strider/src/strider/insn/mod.rs:35-39` | Replace with scope guard `lift_at<R>(addr, impl FnOnce(&mut Self) -> R) -> R` (Drop-time clears). Round 7 types §3. | Convention-only contract becomes structurally enforced. |
| D11 | `validate_with_options` not in Python | `crates/strider-py/src/graph.rs` | Wrap. Round 7 ir PY-1, py-support M4. | Public-API parity. |
| D12 | `apply_elf_relocations_autoload` Python knob | `crates/strider-py/src/reader.rs:213-215` (one-step) vs. `:270` (two-step) | Make `add_region_from_elf` accept `autoload: bool = True`, route through autoload by default. Round 7 strider-target M6. | Same binary, different output sections covered today — silent. |

**Section D est. LOC delta:** ~−40 (removing dead docstrings, ordered() no-op, float_is_nan stub) +≈ +120 (newtypes + phantom-type wrappers + accessors). **Net +80** but eliminates ≥4 documented contract leaks.

---

## Section E — Naming consolidation

Roll up Round 7 naming into a 3-commit batch (per Round 7 naming §C):

| Commit | Scope | Files | Risk |
|--------|-------|-------|------|
| **E-A** | Comment / docstring rewrites only — every `tier 1` / `tier-1` / `tier 2` / `tier-2` (≈ 80 of 121 occurrences) → `cfg-time mini-graph resolver` / `IR-level indirect-branch resolver`. | All 37 files listed in `round7-naming.md` §A.4 (cfg, opt, strider, strider tests). | None — comment-only |
| **E-B** | Internal field rename: `Rule.cap_a` → `lhs_capture`, `Rule.cap_b` → `rhs_capture`. | `crates/opt/src/flag_cmp_canonicalize/mod.rs:104-106, 127-129, 235-264`. Single-file mechanical rename. | None — `Rule` is private |
| **E-C** | Test fn renames in-place + private-enum rename: `tier_1_*` / `tier_2_*` test fns; `SpecialTerm::Unresolved` → `SpecialTerm::PendingIndirect { target_vn, addr }` (struct fields). | `crates/cfg/tests/{indirect_resolve,known_targets}.rs`; `crates/strider/tests/{abi,indirect_branch,graph_rewriter,indirect_resolve_classify,jump_table_lifting,r1_placeholder,tier2_*}.rs`; `crates/strider/src/strider/pipeline.rs:494,510,542`. | None — internal |
| **E-D** | File renames (last; atomic git-mv): `crates/strider/tests/tier2_orchestrator.rs` → `tests/orchestrator.rs`; `crates/strider/tests/tier2_optimizer_tiers.rs` → `tests/optimizer_pipeline_subsets.rs`; update cross-reference in `crates/opt/tests/pipeline_subsets.rs:14`. | 2 file renames + 1 cross-ref edit. | None — Cargo auto-discovers |
| **E-E** (Round 7 ir / pattern) | Stale-doc one-shot batch: `IfCase` deletion (D9), strider-py TODO (A8), if_node symmetric-matching (D6), phi() "any phi" (D7), float_cmp_any NotEqual (D8). | 5 files, ~30 lines of doc edits. | None |

**Section E est. LOC removable:** 0 (rename-only, but vocabulary debt cleared).

---

## Section F — Test deduplication

| # | Overlap | Files | Action | LOC |
|---|---------|-------|--------|-----|
| F1 | `bounded_lift_collapses_cond_branch_with_both_targets_oob_to_tail_call` (strider integration test) | `crates/strider/tests/bounded_lift_tail_call.rs:242-274` | This test asserts strider's *end-to-end* lift collapses a `CondBranch` with both successors OOB to `Call(IntConst(target)) + Return`. The cfg-side primitive (the `(true, true) → TailCall` collapse path) is **not currently covered** at `crates/cfg/tests/region_builder_process.rs` (only `cond_branch_enqueues_both_cases` at line 196 is there for the both-in-range case). **Recommendation: add a cfg unit test for the both-OOB → `RegionTerminator::TailCall` collapse and KEEP the strider integration test** — they cover different layers (strider verifies the IR shape, cfg verifies the terminator decision). The "redundancy" is apparent only: removing the strider test would lose IR-shape coverage. | 0 |
| F2 | `cond_branch_with_one_oob_target` (HIGH per pcode-lift-cfg §C3 / silent-failures §H6) | None today | The `(true, false) | (false, true) AND insns.len() == 1` path at `crates/cfg/src/cfg/builder/region_builder.rs:356-385` silently drops the in-range edge. There is **no test of any kind** for this case in cfg/strider. **Add** one alongside the existing `region_builder_process.rs` cases. (Independent of dedup; surfaces while reading F1.) | +30 |
| F3 | `pipeline_subsets` shape tests | `crates/opt/tests/pipeline_subsets.rs` and `crates/strider/tests/tier2_optimizer_tiers.rs` (renamed in E-D) | Both compare optimizer-name lists against pinned strings. The opt-side test is N+1 (catches drift in `default_pipeline()`); the strider-side test is M+1 (catches strider's `build_*_optimizer_pipeline` layered on top). **Keep both** — they pin different layers. | 0 |
| F4 | `from_graph_and_entry` test fixtures | `crates/pattern/tests/get_vn_with_callother_clobber.rs:34, 69, 112`; `get_vn_with_call_override.rs:89` | Three call sites construct empty-CC `BuiltFunctionGraph`s in the same shape. After D2 ctx-newtype lands, the three sites collapse to a `GraphRewriteCtx` builder helper. | ~30 |

**Section F est. LOC removable:** ~30 (after D2); +30 added (F2 missing test). **Net 0**, but a HIGH-severity untested branch (F2) closes.

---

## Execution order (cleanest diff series)

**Step 1 — comment-only sweep (zero risk):**
- E-A (tier1/tier2 → cfg-time / IR-level)
- E-E (ir/pattern stale doc batch: IfCase, if_node symmetric, phi "any phi", float_cmp_any NotEqual, strider-py TODO)
- A6 (CallOtherElide breadcrumb trim)

**Step 2 — dead code deletions (very low risk):**
- A2, A3 (strider-py dead code)
- A4 (`InputPointsToMissingOutput` variant)
- A8 (Round 7 stale TODO)

**Step 3 — visibility tightening (semver-internal):**
- C1 (node_signature `pub` → `pub(crate)`)
- C2 (`Matcher::options_for_test` → `options` or `pub(crate)`)
- C7 (Outputs/Inputs `Index` impls — pick remove or doc)

**Step 4 — graphmock collapse (single PR):**
- A1 — move into `graphwalk/tests/common/graph.rs`; drop the standalone crate; update workspace `Cargo.toml`.

**Step 5 — internal struct/enum renames (single PR each):**
- E-B (`Rule.cap_a` / `cap_b`)
- E-C (test-fn renames + `SpecialTerm::Unresolved` → `PendingIndirect`)

**Step 6 — file renames:**
- E-D (atomic git-mv of two test files; update one cross-ref)

**Step 7 — public-surface API trims:**
- D1 (PyPat::ordered remove or raise)
- D5 (float_is_nan implement)
- D11 (`validate_with_options` Python wrap)
- D12 (`add_region_from_elf` autoload knob)

**Step 8 — type-system upgrades (each its own PR):**
- D2 (`GraphRewriteCtx` newtype; demote `from_graph_and_entry`)
- C3, C4 (BuiltFunctionGraph field privacy + accessors) — bundles with D2
- C5 (BuiltCallingConvention field privacy)
- C6 (Cfg field privacy + `take_sleigh`)
- D4 (`Builder::for_arch(SleighArch)` constructor; deprecate `new`)
- D10 (`lift_at` scope guard)
- D3 (phantom-typed `OptimizerPipeline<Stable|Destructive|Full>`)
- D6, D7, D8, D9 (pattern docstring corrections — can ship in step 1 if isolated to comments only)

**Step 9 — test gap close (independent):**
- F2 (cfg test for single-insn CondBranch with one OOB successor)

**Step 10 — helper consolidation (longest-tail):**
- B1 (convert recursive opt sites to iterative — Round 7 scale A1, A3 are also CRIT)
- B5 (CallOther name `Arc<str>` migration)
- B6 (asm-fingerprint exempt-set single source of truth)
- F4 (after D2: collapse fixture duplicates)

---

## Total estimated LOC removable

| Section | Est. LOC delta |
|---------|---------------|
| A — deletions | **−175** |
| B — consolidation | **−20** to −40 (plus memory/perf wins) |
| C — visibility | 0 (semver-internal) |
| D — API shrinkage | **+80** (new newtypes/phantom-types, but eliminates 4+ contract leaks) |
| E — naming | 0 (rename-only) |
| F — tests | **−30** (after D2) **+30** (F2) → net 0, closes one HIGH gap |

**Net source-line removal: ~195 lines.** **Net additions: ~80 lines** of structural type-system improvements that turn doc-only contracts into compile-time guarantees.

The dominant wins are not LOC — they are:
1. Four documented contract leaks eliminated (D2, D3, D4, D10).
2. Three documented public-surface lies fixed (D6, D7, D8).
3. Two HIGH-severity Round 7 findings closed by the same series (single-OOB CondBranch via F2; opt destructive-passes-everywhere via D3).
4. Workspace crate count drops by one (`graphmock`).
5. Vocabulary debt ("tier 1 / tier 2") cleared.

Each step in the execution order can ship as one PR; steps 1, 2, 3, 9 run in parallel safely.
