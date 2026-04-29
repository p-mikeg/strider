# Strider — Simplification & Consolidation Review

**Status:** awaiting your approval per item before any code is touched.
**Scope:** entire workspace, ~64K LoC across 13 crates.
**Method:** 6 parallel deep-readers + targeted spot-checks of every load-bearing claim. Findings without a file:line anchor have been dropped.

---

## TL;DR

The codebase is **better-factored than typical** for its size. The big architectural moves (sea-of-nodes IR, three-layer validator, pattern-DSL query engine, separate `pcode-lift`) are sound. But there are concrete pockets of cruft worth fixing:

1. **Back-compat shims that survived F2** — `BuiltFunctionGraph` wrapper impls in `crates/ir/src/ops/{builder,consts,rewrite}.rs` (~140 lines of pure delegation).
2. **Test-helper duplication** — 10+ ad-hoc `sp_vn` / `reg_vn` variants and 17+ `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)` repetitions across opt, ir, pattern.
3. **CLAUDE.md is significantly drifted** — references a non-existent `ir-macros` crate and `match_value!` macro; omits the real `target`, `pcode-lift`, `strider-error` crates.
4. **An over-specialized cache** in `strider/src/cache/` (~650 LoC + 690-line test) used by exactly one consumer (the tier-2 orchestrator) and not part of strider's public mission.
5. **`indirect_resolve_tier2::classify`** is a thin pass-through to `opt::classify_anchor*`. Acceptable as a façade, but currently the file is 90% unit tests of the underlying `opt` function — those tests belong in `opt`.

What I am explicitly **not** proposing:
- No merging of constant-fold and known-bits (they're complementary; verified).
- No collapsing of the pattern-builder family into one type (the type-safety is real).
- No restructuring of the IR's three validation layers (genuine independent layers).
- No replacing of `opt::WorkSet` with `entity_utils::Worklist` (different perf profiles; not a clear win).
- No new "test-fixtures" *crate* — a `crates/ir/src/test_support.rs` module gated on `#[cfg(any(test, feature = "test-utils"))]` is sufficient and avoids a workspace-graph fanout.

---

## H1. Delete the `BuiltFunctionGraph` back-compat wrappers in `ir::ops`

### Where
- [crates/ir/src/ops/builder.rs:58-119](crates/ir/src/ops/builder.rs#L58-L119) — `make_value_node`, `make_int_bits_to_float_node`, `make_float_to_float_node` etc., each labeled `Back-compat wrapper around [Graph::…]`.
- [crates/ir/src/ops/consts.rs:103-148](crates/ir/src/ops/consts.rs#L103-L148) — six methods (`int_const_val`, `bool_const_val`, `float_const_val`, `make_int_const`, `make_bool_const`, `make_float_const`), each pure forwarding.
- [crates/ir/src/ops/rewrite.rs:38-55](crates/ir/src/ops/rewrite.rs#L38-L55) — `replace_all_uses`, explicitly documented "Kept so existing callers that hold a `BuiltFunctionGraph` continue to compile after F2's `Optimizer` trait refactor".

### What to do
Delete the three `impl BuiltFunctionGraph` blocks. Update call sites to use `&mut built.graph.<method>` instead of `&mut built.<method>`. The canonical impls on `Graph` are already public and the migration is mechanical.

### Why this is safe
- The doc comments on every wrapper say "back-compat" — they were left behind on purpose during F2 to avoid a flag-day migration. F2 has settled.
- `cargo check --workspace` will surface every remaining caller. Each migration is one-line: replace `built.foo(…)` with `built.graph.foo(…)`.
- Zero behavioral change — pure forwarding.

### Cost / impact
~140 lines deleted; ~30-50 call sites touched (mechanical). One commit.

---

## H2. Consolidate IR-test boilerplate into `ir::test_support`

### The duplication, verified
**`sp_vn` / `sp32_vn` / `sp64_vn`** — defined separately in:
- [crates/opt/tests/common/mod.rs:99](crates/opt/tests/common/mod.rs#L99)
- [crates/ir/src/builder/tests.rs:794](crates/ir/src/builder/tests.rs#L794) (as `sp_vn_u64`)
- [crates/opt/benches/stack_store.rs:10](crates/opt/benches/stack_store.rs#L10)
- [crates/opt/src/stack_store/tests.rs:9](crates/opt/src/stack_store/tests.rs#L9)
- [crates/pattern/tests/matching/support/graph.rs:32](crates/pattern/tests/matching/support/graph.rs#L32)
- [crates/opt/src/redundant_phis/tests.rs:6](crates/opt/src/redundant_phis/tests.rs#L6)
- [crates/opt/src/function_args/tests.rs:18,87,351](crates/opt/src/function_args/tests.rs#L18) (three variants in one file!)
- [crates/opt/src/stack_load_forward/tests.rs:8,19](crates/opt/src/stack_load_forward/tests.rs#L8)

**`reg_vn(off, size)`** — duplicated at:
- [crates/opt/tests/common/mod.rs:88](crates/opt/tests/common/mod.rs#L88)
- [crates/pattern/tests/matching/support/graph.rs:19](crates/pattern/tests/matching/support/graph.rs#L19)
- [crates/opt/src/constant_fold/tests.rs:144](crates/opt/src/constant_fold/tests.rs#L144)
- [crates/opt/src/dead_branch/tests.rs:7](crates/opt/src/dead_branch/tests.rs#L7)

**`FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)`** — appears verbatim 17+ times across the workspace (opt tests/benches, ir builder/tests, pattern support, indirect-branch tests).

**`return_value(fg)` extracting `Return`'s value input** — duplicated in:
- [crates/ir/src/builder/tests.rs:26-32](crates/ir/src/builder/tests.rs#L26)
- [crates/opt/tests/common/mod.rs:54-60](crates/opt/tests/common/mod.rs#L54)
- [crates/opt/src/constant_fold/tests.rs:26-32](crates/opt/src/constant_fold/tests.rs#L26)

### What to do
Add `crates/ir/src/test_support.rs`, gated `#[cfg(any(test, feature = "test-utils"))]`. Expose:

```rust
pub mod vn {
    pub fn sp(size: u32) -> rsleigh::Vn { … }      // canonical: REGISTER@0x20, configurable size
    pub fn sp32() -> rsleigh::Vn { sp(4) }
    pub fn sp64() -> rsleigh::Vn { sp(8) }
    pub fn reg(off: u64, size: u32) -> rsleigh::Vn { … }
}

pub mod build {
    pub fn empty_fn() -> Result<BuiltFunctionGraph> { … }   // FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).build()
    pub fn empty_fn_with_var(vn: rsleigh::Vn) -> Result<BuiltFunctionGraph> { … }
    pub fn empty_fn_with_sp() -> Result<BuiltFunctionGraph> { … }    // tracked sp, registered as stack pointer
    pub fn return_const(value: u64, ty: NodeOutputType) -> Result<BuiltFunctionGraph> { … }
}

pub mod inspect {
    pub fn return_value(fg: &BuiltFunctionGraph) -> Option<NodeOutputId> { … }
    pub fn return_kind(fg: &BuiltFunctionGraph) -> Option<&NodeKind> { … }
    pub fn count_reachable<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, f: F) -> usize { … }
    pub fn find_node<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, f: F) -> Option<NodeId> { … }
}
```

Add a `[features] test-utils = []` and `[dev-dependencies] ir = { workspace = true, features = ["test-utils"] }` to opt/, pattern/, strider/.

### Why a module + cargo feature, not a new crate
- Avoids a third-party-style dependency boundary. `ir::test_support` is internal scaffolding, not an API.
- Keeps fixtures in lock-step with `FunctionBuilder` — they live in the same crate.
- A separate `ir-test-utils` crate would create a circular-ish path (opt depends on ir; opt's tests would depend on ir-test-utils which depends on ir).
- Rust's standard idiom for this case.

### Migration order (mechanical)
1. Create `ir::test_support` with the canonical helpers.
2. Convert `crates/opt/tests/common/mod.rs` to re-export `ir::test_support::vn::*` and friends — keeps the existing public surface for opt-wide tests.
3. Replace per-file `sp_vn` / `reg_vn` definitions one file at a time. Run `cargo test -p opt -p ir -p pattern` after each.
4. The pattern crate's [tests/matching/support/graph.rs:19-34](crates/pattern/tests/matching/support/graph.rs#L19) already serves as a clean migration target — it's a *good* abstraction in miniature, just duplicated.

### Cost / impact
~250-400 lines eliminated across ~15 test files. One commit per migration step (5-7 commits). Zero behavior change.

---

## H3. Move `crates/strider/src/cache` → internal-only or onto orchestrator

### Verified facts
The cache's public API (`RegionIrCache`, `RegionIrEntry`, `lift_new_regions_into`, `extend_predecessors_into`, `invalidate_split_regions`) is used by exactly two places:
- [crates/strider/src/indirect_resolve_tier2/orchestrator.rs](crates/strider/src/indirect_resolve_tier2/orchestrator.rs) (production)
- [crates/strider/tests/tier2_cache.rs](crates/strider/tests/tier2_cache.rs) (tests of the cache itself)

Nothing else in the workspace touches it. The cache's *job* — pinning `NodeId`s across iterations to preserve phi-extension targets in the indirect-branch fixed-point loop — is **inherent to the orchestrator's algorithm**, not a strider-public concept.

### What to do — option A (preferred)
Move `crates/strider/src/cache/` to `crates/strider/src/indirect_resolve_tier2/cache/` and demote its API from `pub` to `pub(super)`. Move `tests/tier2_cache.rs` to `indirect_resolve_tier2/cache/tests.rs` (it's already a unit-test of the cache submodule, not an integration test of strider).

### Option B (if you want a stricter cut)
Inline the cache into the orchestrator module entirely. The cache is 650 LoC across 5 files, but each submodule (entry/lift/extend/invalidate) is small enough that one `cache.rs` file (~500 LoC after deduplication of doc preamble) is reasonable.

### Why
- Public API surface should reflect public *purpose*. The cache is an implementation detail.
- The 690-line test file is hand-rolled fixtures because it's testing a niche correctness invariant. Co-locating it with the cache makes that visible.
- No external project (per CLAUDE.md, no strider-py yet) reaches in.

### Cost / impact
File moves + visibility tightening. No logic change. Documentation update.

---

## H4. Move `tier2/classify.rs` unit tests into `opt`

### Where
[crates/strider/src/indirect_resolve_tier2/classify.rs](crates/strider/src/indirect_resolve_tier2/classify.rs) is **60 lines of shim** (3 functions delegating to `opt::classify_anchor*`) followed by **~270 lines of unit tests** that exercise `opt`'s classifier through that shim.

The shim is reasonable as a stable public path (the orchestrator and integration tests want a strider-side entry point — fine). But the unit tests aren't testing the shim — they're testing `opt::classify_anchor`'s match arms (IntConst, InitialVar, ValuePhi, etc.). They belong in `opt`.

### What to do
Move the test module `mod tests` from `strider/src/indirect_resolve_tier2/classify.rs` into `crates/opt/src/indirect_branch_resolve/classify.rs` (or a new `tests.rs` next to it). Trim the strider file to ~60 lines of shim plus a brief `// integration tests live in tests/tier2_classify.rs` comment.

### Why
- Tests are closest to the code under test. The shim's correctness is "calls `opt::classify_anchor` with same args" — which is by inspection.
- `opt::indirect_branch_resolve/classify.rs` (456 LoC) currently has no per-arm unit tests according to my survey. Moving these in fills a gap.
- Reduces strider's source-file size and clarifies that `tier2/classify.rs` is purely a façade.

### Cost / impact
~270 LoC moved between files; no behavior change. One commit.

---

## M1. Document or eliminate the SP-walk loop scaffolding

### Pattern (verified)
Three opt passes call `sp_expr::decompose_sp` inside a similar pre-order traversal-with-memo loop:
- [crates/opt/src/stack_store/detect.rs:31](crates/opt/src/stack_store/detect.rs#L31) — `decompose_sp(&fg.graph, addr, sp_vn, memo, &mut visiting)` inside a node walk.
- [crates/opt/src/stack_store/call_args.rs:91](crates/opt/src/stack_store/call_args.rs#L91) — same shape.
- [crates/opt/src/function_args/mod.rs:39](crates/opt/src/function_args/mod.rs#L39) — same shape.
- (`stack_load_forward/mod.rs` likely also; not fully audited)

The hot inner call (`decompose_sp`) is already shared. What's repeated is *the iteration scaffolding* — collect preorder, iterate, run `decompose_sp`, route output.

### What to do
Add a small helper to `sp_expr.rs`:
```rust
pub fn for_each_sp_addressed_load_or_store<F>(
    fg: &BuiltFunctionGraph,
    sp_vn: rsleigh::Vn,
    mut f: F,
) where F: FnMut(NodeId, NodeOutputId, SpExpr) { … }
```
…or keep the per-pass loops if the divergences (which nodes each pass cares about, what each does on `None`) are too pass-specific to factor cleanly.

### Honest assessment
This is **lower-impact than I initially thought** — the loops differ in (a) the node-kind filter at the top, (b) what they do with the `SpExpr` result, (c) whether they mutate the graph mid-walk. The shared part is ~15-20 lines of "iterate preorder; call decompose_sp; pattern-match the result" that, once inlined into a closure-taking helper, may not feel cleaner than the inline version.

### Recommendation
**Survey first, factor only if the shared shape is genuinely the same.** I'd skip this unless we want to add a 4th pass. Drop to L tier.

---

## M2. Update CLAUDE.md — significant documentation drift

### Verified drift
1. **`ir-macros` crate is described in CLAUDE.md but does not exist.** Confirmed: `crates/ir-macros` does not exist; no `match_value!` macro is defined or used anywhere in the workspace; `grep -rn "match_value!"` returns nothing. The macro the CLAUDE description praises ("Used heavily by `opt` passes") is fictional in the current tree.
2. **`pcode-lift` crate exists, is active (~2K LoC, 12 files), but isn't mentioned in CLAUDE.md.** Per `git log`, `pcode-lift` was extracted from `strider` ("pcode-lift: move all value-op handlers from strider", "pcode-lift: move vn_io + register-aliasing from strider").
3. **`strider-error` crate exists with 9 dependents, not in CLAUDE.md.**
4. **`target` crate is in workspace deps but undocumented in CLAUDE.md.**
5. **The "External Dependency: rsleigh" section is correct;** rsleigh remains an out-of-tree path dep.

### What to do
A surgical edit to CLAUDE.md:
- Delete the `ir-macros` paragraph and the `match_value!` example.
- Add `pcode-lift` (value-op lifter, factored out of strider; called by `strider/src/strider/insn/` and `cfg`).
- Add `strider-error` (cross-crate error backtrace machinery; macro-driven via `define_error!`; consumed by 9 crates including ir/cfg/opt).
- Add `target` (whatever that crate exports — to be confirmed during the edit).
- Update the "Crate Dependency Flow" diagram.

### Why this matters
CLAUDE.md is the *authoritative* mental model for future work in this repo. Drifted docs cost real time when the next person (or agent) is wrong about the crate graph.

### Cost / impact
30 minutes. One commit. High value-per-minute.

---

## M3. Tighten redundant module boundaries in `ir::ops`

### Where
After H1 deletes the `BuiltFunctionGraph` blocks, the `ir::ops` directory becomes:
- [ops/builder.rs](crates/ir/src/ops/builder.rs) — Graph-side `make_value_node`, `make_int_bits_to_float_node`, `make_float_to_float_node`, … (~70 LoC after H1).
- [ops/consts.rs](crates/ir/src/ops/consts.rs) — `int_const_val` / `make_int_const` etc. (~100 LoC after H1).
- [ops/rewrite.rs](crates/ir/src/ops/rewrite.rs) — `replace_all_uses` (~30 LoC after H1).
- [ops/op_kinds.rs](crates/ir/src/ops/op_kinds.rs)
- [ops/mod.rs](crates/ir/src/ops/mod.rs)

Three of these become very thin. Suggest: collapse `builder.rs`, `consts.rs`, `rewrite.rs` into a single `ops/graph_ext.rs` once H1 lands. Keep `op_kinds.rs` separate (it's the enum taxonomy).

### Cost / impact
Pure org. Skip if you prefer the per-concern splits.

---

## L1. Pattern crate is well-organized; one tiny touch only

Per a focused audit (agent 3), pattern's apparent fragmentation is intentional and load-bearing:
- The 7 builder files split by **node family** (data vs control) — the type-safety distinction (`.capture(Var)` for value-producing, `.capture_node(NodeVar)` for control) is real and worth keeping.
- The 9 ctor files mirror the IR's enum taxonomy (`IntBinaryOp` vs `BoolBinaryOp` vs `FloatBinaryOp`) — collapsing them would muddle types.
- `walk.rs` (single-step consumer lookup, 88 lines incl. tests) and `walk_through.rs` (transparent recursion past casts/ControlState, 168 lines) **solve different problems** despite their similar names. Confirmed by reading both.
- The three `rewrite.rs` files (in `ir/ops/`, `pattern/`, `strider/`) form a clean composition chain (graph mutation primitive → rule closure → graph-walk loop). No duplication.

### One tiny win
The `.ordered()` method is duplicated across `IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat` at:
- [crates/pattern/src/pat/builders/binary_op.rs:44](crates/pattern/src/pat/builders/binary_op.rs#L44)
- [crates/pattern/src/pat/builders/binary_op.rs:79](crates/pattern/src/pat/builders/binary_op.rs#L79)
- [crates/pattern/src/pat/builders/binary_op.rs:116](crates/pattern/src/pat/builders/binary_op.rs#L116)

A small private trait `MaybeCommutative { fn ordered(self) -> Self; }` would unify them. ~20 LoC saved. Skip unless you're already in this file.

---

## L2. CFG crate — no changes proposed

Verified well-factored:
- builder/ split (mod / region_builder / indirect_resolve / split) reflects orthogonal concerns.
- types/query/mod separation is clean.
- `test_api.rs` is appropriate — only used within cfg's own tests + strider's tier-2 (which legitimately consumes `ResolvedTargets`).
- `cfg/dot.rs`, the `dot` crate, and `ir/dot/` are correctly stratified (engine vs CFG renderer vs IR renderer).

No simplification recommended for cfg.

---

## L3. Smaller crates — keep as-is

- **graphwalk** (376 LoC): well-designed; only 1-2 active callers, but it's foundational.
- **graphmock** (283 LoC): test-only DSL for graphwalk's own tests. Correctly isolated. Don't move.
- **entity-utils**: `Worklist<T>` is unused outside its own tests; opt has its own `WorkSet` (using `FxHashSet`). Could consolidate, but opt's choice is a deliberate micro-opt. Skip.
- **reader, pcode-lift, strider-error**: all healthy, active, well-scoped. No changes.

---

## What I am NOT proposing (and why)

- ❌ **Merge `ConstantFold` and `KnownBits`.** They're complementary: ConstantFold needs full operand values; KnownBits propagates partial bit info. Neither subsumes the other.
- ❌ **Collapse `validate::layer_{a,b,c}.rs` into one file.** Each layer has an independent traversal/check shape. Keeping them split makes the "validation strategy" diagram in CLAUDE.md match the code.
- ❌ **A unified `OptimizationPass` trait** on top of every pass. Boilerplate is real (~15 lines/pass × 8 passes), but the shape divergences (propagating-with-worklist vs detection-once) make a single trait awkward. The current `Optimizer` trait already covers the common surface.
- ❌ **A new `test-fixtures` crate.** Use an `ir::test_support` module gated on a cargo feature instead. Simpler, no graph fanout.
- ❌ **Delete the `cache::tests.rs` 690-line test suite.** It's testing a niche correctness invariant (NodeId pinning under phi-extension) that genuinely needs hand-built fixtures; pattern/graphmock helpers wouldn't expose the right knobs.
- ❌ **Restructure `pat/builders/` or `pat/ctor/`.** They are fragmented but intentionally so — the type-safety story is non-trivial.

---

## Suggested phasing (5 PRs)

I'd execute in this order, with each PR independent and reversible:

### PR 1 — CLAUDE.md drift fix (M2)
Quick win, lowest risk, biggest value-per-minute. Establishes accurate ground truth before touching code.

### PR 2 — Delete ops/ back-compat wrappers (H1)
Mechanical, ~140 LoC out, ~30-50 call sites updated. Pure forwarding deletions.

### PR 3 — Test consolidation: `ir::test_support` (H2)
Add the module + feature, migrate test helpers across opt/ir/pattern incrementally. Probably 3-5 internal commits, single PR. ~250-400 LoC out.

### PR 4 — Move classifier tests to `opt`; tighten cache module (H3 + H4)
Two related cleanups in strider. Combined because they both narrow strider's public surface.

### PR 5 — Optional cleanup (M3, L1)
After H1/H2 lands, decide on collapsing the slim `ops/{builder,consts,rewrite}.rs` files and the `.ordered()` trait. May not be worth a PR on its own.

---

## Verification protocol I will follow when executing

Per the auto-memory's standing instruction (TDD + systematic-debugging + verification-before-completion):
- Every PR: `cargo test --workspace && cargo clippy --workspace -- -D warnings` must pass.
- For PR 2 (wrapper deletions): grep for `built\.\(make_value_node\|make_int_const\|replace_all_uses\)` and confirm zero hits before commit.
- For PR 3 (test consolidation): each sp_vn migration is one file at a time, with `cargo test -p <crate>` between.
- For PR 4 (cache visibility tightening): `pub` → `pub(super)` is compiler-checked; if anything outside the orchestrator broke, we'd see it immediately.
- I will not skip pre-commit hooks. I will not amend pushed commits.

---

---

# Section 2 — Logic consolidation via `pattern` extensions

> Added after your follow-up: focus on *logic* that can be replaced (e.g. by extending `pattern`'s power) rather than on test boilerplate. The original 5 PRs above stand; this section is **additive**.

## Where we are today

Pattern adoption is partial. Recent migrations have already moved several shape-matchers from hand-written `match` chains to pattern queries:
- [match_jump_table_shape](crates/opt/src/indirect_branch_resolve/jump_table.rs#L161) — uses `load().addr(add(any_int_const(base_var), mul(var(idx_var), any_int_const(stride_var))))`. The pre-migration version was 4 hand-rolled commutativity arms.
- [extract_idx_and_stride](crates/opt/src/indirect_branch_resolve/stack_array.rs#L392) — pattern. Comment: "pattern-DSL form replaces the prior arm-by-arm dispatch on `NodeKind`".
- [strip_target_mask](crates/opt/src/indirect_branch_resolve/stack_array.rs#L163) — uses `and_pat`, `or_pat`.
- [bound_from_if_condition](crates/opt/src/indirect_branch_resolve/jump_table.rs#L467) — uses `int_cmp_any` with a `Var` for the operator.
- [constant_fold/rules.rs](crates/opt/src/constant_fold/rules.rs) — declarative rewrite rules with `rewrite_rule(lhs, rhs)`.

But several heavy hand-rolled walkers remain. Each one is justified at the *call site* by a specific gap in `pattern`. If we close those gaps, the call-site code shrinks substantially.

## The five hand-rolled hotspots, with the gap they expose

### G1 — `same_value`: trivial single-input phi unwrap
[crates/opt/src/indirect_branch_resolve/jump_table.rs:525](crates/opt/src/indirect_branch_resolve/jump_table.rs#L525), 40 LoC.

The comment is explicit:
> The pattern accepts any LHS; we still verify it refers to the dispatch's `idx_output`. `same_value` walks through trivial single-input phis, **which patterns can't express directly**: intermediate orchestrator iterations omit RedundantPhis, so the dispatch region's read of `idx` is wrapped in a single-input ControlPhi distinct from the `If`'s direct read.

**What's missing in pattern:** `MatcherOptions::ignore_control_states` exists ([crates/pattern/src/matcher/walk_through.rs](crates/pattern/src/matcher/walk_through.rs)) and walks past join `ControlState`s. There is **no equivalent option for trivial single-input ControlPhi/ValuePhi** — and that's exactly the case that arises when RedundantPhis hasn't run yet.

**Proposed extension:** add `MatcherOptions::ignore_trivial_phis` (or fold into `ignore_control_states`). When a sub-pattern fails to match directly against an output, and that output's producer is a `ControlPhi(_)` / `ValuePhi` with exactly two inputs (token + one value), recurse on the value input. Same machinery as the existing cast-mask walk-through.

**Result:** delete `same_value` (40 LoC) and the `if !same_value(graph, lhs, idx_output) { return None; }` guard at [bound_from_if_condition:498](crates/opt/src/indirect_branch_resolve/jump_table.rs#L498). The pattern in `bound_from_if_condition` already binds `idx_var` against the dispatch's `idx_output` directly via `var(idx_var)` — once the matcher walks through trivial phis automatically, the comparison just works.

---

### G2 — `flatten_add_tree`: n-ary additive flattening
[crates/opt/src/indirect_branch_resolve/stack_array.rs:335](crates/opt/src/indirect_branch_resolve/stack_array.rs#L335), 50 LoC + the loop in `match_stack_array_shape` that consumes its output.

The function recursively walks `Add`/`Sub(_, Const)` trees to collect a flat list of additive terms plus a constant offset. ARM lifters emit nested trees; x86/x64 emit flat ones. The flattening is what makes the address-shape match work across architectures.

**What's missing in pattern:** the `add` pattern is binary. There is no "match an arbitrary Add/Sub tree, bind every leaf and the running constant" combinator. Today this would be expressed as a recursive Pat — but Pat doesn't have a recursive constructor, and the pattern-DSL builder API is fundamentally non-recursive.

**Proposed extension:** add an `additive_terms` Pat combinator:
```rust
pub fn additive_terms() -> AdditiveTermsPat;
impl AdditiveTermsPat {
    pub fn term(self, p: impl Into<Pat>) -> Self;          // require this leaf appear (binding)
    pub fn const_offset(self, v: IntVar) -> Self;          // capture summed constant
    pub fn extra_terms(self, vars: &[Var]) -> Self;        // capture remaining leaves
}
```
Implementation: a custom `Pat` variant with a single recursive matching routine that traverses Add/Sub-rhs-const trees (capped at 32 nodes for the same DoS guard the current code has). Reuses the `int_const_signed` helper from `sp_expr`.

**Result:** `flatten_add_tree` deleted; the flattening loop in [match_stack_array_shape:235-265](crates/opt/src/indirect_branch_resolve/stack_array.rs#L235) compresses to ~10 lines that consume the matcher's bindings.

---

### G3 — `decompose_sp` outside the pattern world
[crates/opt/src/sp_expr.rs](crates/opt/src/sp_expr.rs), ~120 LoC of recursive descent over `InitialVar(sp)`, `Add(_, Const)`, `Sub(_, Const)`, `ControlPhi(sp)`.

The function is good code — short, focused, well-tested. Its problem is that it **lives outside the pattern crate**, so when one of the indirect-resolve passes wants to say "match a Load whose address is sp + K" or "match an address that's sp + idx*stride + K," the pattern itself can't express the SP-relative part. Today's stack-array matcher has to first run pattern shape detection, *then* invoke `decompose_sp` separately on each non-mul term ([stack_array.rs:281](crates/opt/src/indirect_branch_resolve/stack_array.rs#L281)).

**Proposed extension:** add a `sp_relative` Pat that wraps `decompose_sp`:
```rust
pub fn sp_relative(sp_vn: rsleigh::Vn, offset: IntVar) -> Pat;
```
Implementation: a custom `Pat` variant whose matcher runs `decompose_sp` and on `Some(SpExpr::Terminal { offset, .. })` binds `offset` as the captured signed constant. The `Phi` case fails the match (the existing call sites already treat `SpExpr::Phi` as a bail).

**Result:**
- `match_stack_array_shape` becomes a single pattern: `load().addr(additive_terms().term(sp_relative(sp_vn, base_offset_var)).term(mul_or_shl(idx, stride_var)))`. The 70 LoC residual logic in `match_stack_array_shape` (the term-by-term decompose loop) collapses to ~5 lines.
- Future passes that want "Load[sp + K]" matching get it as a one-liner instead of having to manually pull `decompose_sp` out of `sp_expr`.

**Honest caveat:** `decompose_sp` takes a memo by `&mut`. Pattern matchers today are `&self`-only. Embedding `decompose_sp` in pattern means either (a) pattern grows mutable per-match scratch storage, or (b) callers pre-build a memo and the pattern receives it via a context. (b) is cleaner and matches how the existing `Matcher` struct is shaped — it already pre-indexes Call/Return/If nodes per-graph, so adding a per-graph SpExprMemo is a small extension.

---

### G4 — `walk_control_for_if_bound`: backward control walk with join logic
[crates/opt/src/indirect_branch_resolve/jump_table.rs:345](crates/opt/src/indirect_branch_resolve/jump_table.rs#L345), 70 LoC.

This walks the control predecessors of a placeholder Return upward, looking for an `If` whose true-edge bounds a target index. At a `ControlState` (join) it recurses into every predecessor and takes the max of the returned bounds, failing closed if any predecessor doesn't prove a bound. Cycle-protected with a per-walk visited set.

**What's missing in pattern:** pattern has `Contains(p)` for **forward** control-chain search (find any descendant control node matching `p`). There is **no symmetric backward variant** and no way to express "search backward through the control DAG, taking max over joins."

**Proposed extension:** *don't* try to push this into pattern. Joins-with-aggregation are not pattern-shaped. Instead, factor the **traversal scaffolding** out of `walk_control_for_if_bound` into a generic backward control walker, parameterized over the per-`If` predicate and the join-aggregator:
```rust
// In ir::walk (or new ir::walk::control)
pub fn walk_back_control<T, F, J>(
    fg: &BuiltFunctionGraph,
    start: NodeOutputId,    // a Control output to walk upward from
    mut visit_if: F,        // (cond_out, on_true_branch) -> Option<T>
    join: J,                // fold predecessors' Option<T>s into Option<T> at ControlState
) -> Option<T>
where F: FnMut(NodeOutputId, bool) -> Option<T>,
      J: Fn(Option<T>, Option<T>) -> Option<T>;
```
The visited-set-per-predecessor and the "walk transparently through unrelated control nodes via slot 0" logic become library code. The pass-specific bit ("does this If's condition give me a bound on idx?") stays in the pass.

**Result:** `walk_control_for_if_bound` shrinks from 70 LoC to a 6-line call into `walk_back_control` with `visit_if = bound_from_if_condition` and `join = max_or_fail`. The infrastructure becomes reusable for any future pass that wants to scan dominators (e.g., null-check elision, range analysis on a different operand).

**Honest caveat:** this primitive is *not* obviously called by anything else today. I'd hold off on shipping it until we have a second consumer in mind, or accept that the first consumer is a clarity win on a single function.

---

### G5 — `node_known_bits`: big-switch dispatch
[crates/opt/src/known_bits/mod.rs:90-340](crates/opt/src/known_bits/mod.rs#L90), ~250 LoC of `match kind { NodeKind::IntConst(...) => ..., NodeKind::IntBinaryOp(IntBinaryOp::And) => ..., NodeKind::Truncate => ..., NodeKind::Extend(_) => ..., ... }`.

This isn't really a *match-shape* problem; it's a *dispatch* problem. The function is doing case analysis on NodeKind to compute output bits from input bits — every variant has different math. Pattern doesn't help here because there's no structural query: each arm is "I know I'm an `And`; combine my two operands' bit info."

**Verdict: leave it alone.** This is what big-switch dispatch is for. Trying to express it as pattern rules (e.g., "match `and(any_int_const(c), var(x))` and emit `Kb { ones: x.ones & c, zeros: x.zeros | !c }`") would invert the control flow without saving lines or improving readability. KnownBits is a forward dataflow analyzer, not a rewrite engine. Skip.

---

### G6 — `probe` (memory-chain walk in stack_load_forward)
[crates/opt/src/stack_load_forward/mod.rs:189](crates/opt/src/stack_load_forward/mod.rs#L189), ~200 LoC.

Walks the memory dependence chain backward from a `Load`'s mem input, looking for a satisfying `StackStore` or proving disjointness. The walk has its own per-step state (load offset, load size, current memory), per-step decisions (overlap → forward; disjoint → keep walking; unknown → bail), and per-NodeKind logic (StackStore / Store / MemPhi / Call).

**What's missing in pattern:** memory-chain walks don't fit the pattern model at all. Pattern matches a *shape*; this is a *dataflow query with state*.

**Verdict: leave it alone.** Same reasoning as G5. The memory-chain walker is not duplicated elsewhere (one consumer), so generalization has no second user. Skip.

---

## Traversal generalizations (separate from pattern extensions)

### T1 — A `data_op_preorder()` filter

You suggested it directly: "traversal that ignores control nodes except `If`s." Multiple passes do `for nid in fg.preorder()` and then `match graph.node_kind(nid)` to skip `Entry` / `ControlState` / `ControlPhi` / `MemPhi` / `InitialMemory` / `InitialVar` (the structural nodes), wanting only the operation nodes (IntConst, IntBinaryOp, Load, Store, Call, If, Return).

**Concrete sites that would benefit:**
- [crates/opt/src/stack_store/detect.rs:107](crates/opt/src/stack_store/detect.rs#L107)
- [crates/opt/src/constant_fold/mod.rs:85](crates/opt/src/constant_fold/mod.rs#L85)
- [crates/opt/src/known_bits/mod.rs:388](crates/opt/src/known_bits/mod.rs#L388)
- [crates/opt/src/dead_branch/mod.rs:142](crates/opt/src/dead_branch/mod.rs#L142)
- [crates/opt/src/indirect_branch_resolve/inplace.rs:180](crates/opt/src/indirect_branch_resolve/inplace.rs#L180)
- [crates/opt/src/indirect_branch_resolve/mod.rs:391](crates/opt/src/indirect_branch_resolve/mod.rs#L391)

**Proposed:** add a kind-class taxonomy in `ir::node::kind` (cheap helper, not a new enum):
```rust
impl NodeKind {
    pub fn is_structural(&self) -> bool { matches!(self, Entry | InitialMemory | InitialVar(_) | ControlState | ControlPhi(_) | MemPhi | StackStorePhi { .. }) }
    pub fn is_data_op(&self) -> bool { !self.is_structural() }
}
```
Then `BuiltFunctionGraph::data_op_preorder()` is a one-liner filter on `preorder()`.

**Result:** the inner `match` skips at every site become trivial. Each pass keeps its own `match` for the *operations* it cares about, but the boilerplate-skip arms vanish. ~3-5 LoC saved per site × 6 sites = small, but it's the kind of small that compounds: a new pass author skips an entire category of "remember to handle ControlState somehow" thinking.

**Honest assessment:** marginal. This is a one-line change with one-line callers; mostly an ergonomics + naming improvement.

### T2 — The `inputs.len() < N` defensive checks

[Twenty sites](#) do `let inputs = graph.node_inputs(node); if inputs.len() < 2 { return None; }` or similar. The arities are encoded in `node_signature::expected_signature` and validator Layer A enforces them. The defensive checks are belt-and-suspenders.

The `node_inputs_exact::<N>(node) -> Result<[NodeOutputId; N]>` API already exists and is the right primitive — it's just not used uniformly. Some sites prefer `node_inputs(...).get(0)` because the arity is genuinely variadic (Call, Return, ControlState, ControlPhi).

**Proposed:** a small migration where the *known-fixed-arity* nodes (Load, Store, IntBinaryOp, IntCmpOp, etc. — every signature with no variadic suffix) get typed accessors:
```rust
impl Graph {
    pub fn load_inputs(&self, node: NodeId) -> Result<LoadInputs>;       // { mem, addr }
    pub fn store_inputs(&self, node: NodeId) -> Result<StoreInputs>;     // { mem, addr, value }
    pub fn int_binop_inputs(&self, node: NodeId) -> Result<BinopInputs>; // { lhs, rhs }
    // …
}
```
These are 1-liners over `node_inputs_exact`. Their value is the *named-field* destructuring at call sites: instead of `inputs[0]` (mem) and `inputs[1]` (addr), you write `let LoadInputs { mem, addr } = graph.load_inputs(node)?;`.

**Honest assessment:** taste-level. The current `let mem_input = *load_inputs.first()?; let addr_output = *load_inputs.get(1)?;` works; the typed-accessor form is just nicer. Skip unless we're already touching these files for G1-G3.

---

## Realistic impact of Section 2

If we ship G1, G2, G3, T1 (the four with concrete second consumers and clear gaps):

| Item | Pattern crate adds | Hand-rolled lines deleted | Risk |
|---|---|---|---|
| G1 — ignore_trivial_phis | ~30 LoC + a few tests | ~40 (`same_value`) + ~5 guard | Low — same machinery as existing cast-mask walk-through |
| G2 — `additive_terms` | ~80 LoC + tests | ~50 (`flatten_add_tree`) + ~30 (loop) | Medium — new Pat variant with recursive matching |
| G3 — `sp_relative` | ~40 LoC + tests | ~70 (`match_stack_array_shape` body) | Medium — requires plumbing SpExprMemo through Matcher |
| T1 — `data_op_preorder` | ~10 LoC | ~20 (skip-arms across 6 sites) | Trivial |
| **Total** | ~160 LoC | ~215 LoC | — |

Net: **~55 LoC reduction overall**, but the *quality* of the remaining code goes up substantially — three places that today have prose paragraphs explaining "this hand-rolled walker exists because pattern can't…" become declarative pattern queries. The `pattern` crate gains three reusable primitives that future passes will use whether we plan for them or not.

**What I am not proposing:**
- **G4 (`walk_back_control`)** — only one consumer today; ship if/when a second consumer materializes.
- **G5 (KnownBits)** — case-analysis dispatch is correctly structured as a big match; pattern would invert without simplifying.
- **G6 (memory-chain walk)** — single consumer, stateful, doesn't fit pattern's model.
- **An "alt(p1, p2, ...)" combinator for top-level dispatch** — the [classify_anchor](crates/opt/src/indirect_branch_resolve/classify.rs#L107) match is short enough that a pattern-based replacement would be net neutral on lines and worse on readability.
- **A general "pattern-driven optimization pass" framework** — `apply_rules_in_order` already exists for the rewrite case; the analysis-pass case (KnownBits, FunctionArgs) doesn't naturally fit.

## Phased plan for Section 2

If you approve this direction, I'd execute as separate PRs:

1. **PR S2-A — `MatcherOptions::ignore_trivial_phis` (G1).** Add the option, add tests in `pattern/src/matcher/walk_through.rs`, then migrate `bound_from_if_condition` to enable it and delete `same_value`. Smallest, lowest-risk; validates the approach.

2. **PR S2-B — `data_op_preorder()` (T1).** Independent of S2-A. Trivial mechanical change.

3. **PR S2-C — `sp_relative` Pat (G3).** Plumb `SpExprMemo` into `Matcher` (or per-call context); add the `SpRelativePat` variant; add tests; migrate one call site (`match_stack_array_shape` partial).

4. **PR S2-D — `additive_terms` Pat (G2).** Larger; depends on S2-C for the term-decompose composition. Add the recursive matcher; tests for nested Add/Sub trees with constants; complete `match_stack_array_shape` migration; delete `flatten_add_tree`.

5. **(Hold) PR S2-E — `walk_back_control`** if/when a second consumer appears.

Each PR is independent and reversible. S2-A is a 1-day job; S2-C and S2-D are each 2-3 days because the matcher state plumbing needs care.

## What success looks like (operationally)

After S2-A through S2-D:
- `match_stack_array_shape` should be readable in one sitting (currently it requires understanding `flatten_add_tree`, `decompose_sp`, and the per-term loop).
- `bound_from_if_condition` should not need a comment explaining why a separate `same_value` exists.
- A new pass author who wants "match Load[sp + K]" or "match a sum of these terms" finds them in `pattern::*` next to `add` and `mul`, not by reading the indirect-branch resolver.

If after building S2-A we find the `MatcherOptions::ignore_trivial_phis` machinery is awkward to extend (e.g., interacts badly with cast-mask walk-through), we stop and reassess before doing S2-C / S2-D. Each PR is gated on the previous one *feeling right*, not just compiling.

---

## Open questions for you

Before I touch any code, please indicate:

1. **PR 1 (CLAUDE.md):** approve as-is, or would you prefer to keep `ir-macros` as a *planned* crate and simply mark it so?
2. **PR 2 (delete back-compat wrappers):** approve, defer, or reject?
3. **PR 3 (test_support module + cargo feature):** approve the module-not-crate approach? Any naming preferences (`ir::test_support` vs `ir::test_utils` vs other)?
4. **PR 4 (move classifier tests; tighten cache):** approve both cleanups, or just one?
5. **PR 5 (cosmetic):** skip entirely?
6. Anything in the **"NOT proposing"** list that you'd actually like done — i.e., do you disagree with any of my "leave it alone" judgments?

### Section 2 (logic / pattern extensions)

7. **G1 (`ignore_trivial_phis` matcher option):** approve the smallest-PR-first approach? It's the cleanest "extend pattern, delete hand-rolled code" beat.
8. **G2 + G3 (`additive_terms` + `sp_relative` Pat variants):** approve plumbing `SpExprMemo` into `Matcher` to make these work? This is the single biggest design choice in Section 2.
9. **T1 (`data_op_preorder` + `NodeKind::is_structural`):** approve as standalone tiny PR?
10. **G4 (backward control walker):** ship now even though it has only one consumer, or hold?
11. **G5 / G6 (KnownBits big-switch / memory-chain walk):** agree they should *not* be pattern-ized, or push back?
12. Anything else hand-rolled in opt/strider you'd like me to look at as a candidate for pattern-ization that I missed?

Once you give the green light per item, I'll execute in PR order with full test runs between commits.
