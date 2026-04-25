# `ir` crate review, simplification, restructure, and testing

**Date:** 2026-04-25
**Branch:** `review/ir-crate` (off `feature/ai`)
**Worktree:** `/home/mike/Desktop/strider/.worktrees/ir-review`
**Final integration:** non-fast-forward merge into `feature/ai` after acceptance gate passes.

## Goals (priority-ordered)

1. **Correctness above all.** Find and fix every real bug. Never trade correctness for cosmetics. When in doubt, write a regression test that pins the behavior.
2. **Zero clippy warnings** under `cargo clippy -p ir --all-targets --no-deps -- -D warnings`.
3. **Restructure** `node.rs` (1007 lines) and `graph.rs` (1246 lines) into responsibility-scoped submodules. Public surface unchanged via `pub use` re-exports — no callsite outside `crates/ir/` changes.
4. **Comprehensive tests** — white-box per module + black-box integration tests in `crates/ir/tests/` + property-based proptest invariants.
5. **Simplification** wherever it makes correctness reasoning easier — never as stylistic noise.

## Non-goals

- No public API path changes. Promoting `ir::node::Foo` to `ir::Foo` is a deferred follow-up ("B2"), not part of this review.
- No new IR features, node kinds, or builder methods.
- No changes outside `crates/ir/` other than the spec/plan documents themselves.
- No structural reorganization of `validate/`, `dot/`, `ops/`, `builder/` — these are already responsibility-scoped. Correctness fixes, clippy fixes, and test additions only.
- No benchmarks. (Can be added later if perf becomes a concern.)

## Baseline (measured 2026-04-25 on `feature/ai @ b3da20d`)

- `cargo test -p ir`: **165 tests pass**, 1 doctest ignored, 0 failures.
- `cargo clippy -p ir --all-targets --no-deps -- -D warnings`: **132 errors**, bucketed:
  - 57 × `missing_errors_doc` on `pub fn` returning `Result`
  - 32 × `must_use_candidate` on getters
  - 29 × `map(...).unwrap_or(...)` (mechanical → `map_or`)
  - 12 × `match_same_arms`
  - 2 × function-level `must_use_candidate`
- File sizes (lines, src only):
  - `graph.rs` 1246, `node.rs` 1007, `node_signature.rs` 631
  - `builder/nodes.rs` 538, `builder/tests.rs` 737
  - `validate/{mod,layer_a,layer_b,layer_c,tests}.rs` 263+84+55+244+575
  - `dot/{mod,label,render,tests}.rs` 172+356+229+379
- `cargo run --example analyzer` produces deterministic `cfg.html` and `graph.html` (golden artifacts captured at start of work for before/after comparison).

## Final module structure

```
crates/ir/
├── src/
│   ├── lib.rs                    # public exports unchanged
│   ├── error.rs
│   ├── function.rs
│   ├── iterators.rs
│   ├── region.rs
│   ├── walk.rs
│   ├── node_signature.rs
│   ├── node/                     # split from node.rs
│   │   ├── mod.rs                # `pub use` everything; preserves ir::node::*
│   │   ├── ids.rs                # NodeId, NodeOutputId, NodeInputId, EntityList aliases
│   │   ├── output_type.rs        # NodeOutputType + Display + TryFrom<u32>
│   │   ├── output_kind.rs        # NodeOutputKind + helpers
│   │   ├── kind.rs               # FunctionArgSource, NodeKind + impl
│   │   ├── data.rs               # NodeOutput, NodeInput, Node
│   │   └── tests.rs              # white-box tests for the split modules
│   ├── graph/                    # split from graph.rs
│   │   ├── mod.rs                # struct Graph + Default + pub use; preserves ir::graph::*
│   │   ├── store.rs              # arena (PrimaryMaps), create_node, dedup cache, side-tables
│   │   ├── uses.rs               # use-list bookkeeping: add/remove/update input, detach, replace
│   │   ├── access.rs             # node_kind, node_inputs[_exact], node_outputs[_exact], get_node_from_output
│   │   └── tests.rs              # white-box tests for the split modules
│   ├── builder/                  # structure unchanged; tests expanded
│   ├── ops/                      # structure unchanged
│   ├── dot/                      # structure unchanged; tests expanded
│   └── validate/                 # structure unchanged; tests expanded
└── tests/                        # NEW: black-box integration tests
    ├── common/mod.rs                 # shared FunctionBuilder helpers
    ├── build_validate_roundtrip.rs   # every public-API construction → validate() succeeds
    ├── walk_reachability.rs          # walk_graph completeness on hand-crafted graphs
    ├── dedup_cache.rs                # cacheable kinds always dedup; non-cacheable never do
    ├── use_list_consistency.rs       # forward/backward list invariants under all mutations
    ├── validate_layer_a.rs           # local typing failures detected, zombies skipped
    ├── validate_layer_b.rs           # use-list bidirectional consistency violations detected
    ├── validate_layer_c.rs           # graph-level invariants (Entry/InitialMemory uniqueness, ControlState predecessor kinds, phi token ownership/arity, StackStorePhi arity 3, PostCall producer/uniqueness)
    └── proptest_graph_invariants.rs  # property-based invariants
```

**Why this `graph/` split:** the three submodules align with the three contracts the validator's three layers each protect. `store` owns the arena and the dedup cache; `uses` owns the bidirectional use-list (Layer B's contract); `access` owns the typed reads Layer A enforces against. When an invariant lives in one file, it's auditable.

**Why this `node/` split:** `node.rs` already has internal section boundaries — the split is mechanical and lossless. Each submodule holds one type family.

## Review approach

A two-pass read of every file in the crate.

### Pass 1 — Correctness read

Read each module top-to-bottom while carrying these invariants in mind:
- **Use-list bidirectionality.** Every mutation that touches a `NodeInput → NodeOutput` edge must update both directions of the list as a pair.
- **Dedup cache equivalence.** Two nodes with identical `(kind, inputs, output_kinds)` on a cacheable kind must return the same `NodeId`. Non-cacheable kinds (notably `StackStorePhi`, `Call`-likes, anything with side-tables) must never be cached.
- **Side-table coherence.** When a node is detached, its side-table entries (`stack_phi_offsets`) must not strand.
- **`validate`'s three layers.**
  - Layer A: local typing of every reachable node against `expected_signature`.
  - Layer B: use-list bidirectionality (forward inputs ↔ backward `OutputUsageIter`).
  - Layer C: graph-level invariants — Entry/InitialMemory uniqueness, ControlState predecessor kinds, ControlPhi/MemPhi token ownership and per-predecessor arity, StackStorePhi fixed arity 3, PostCall producer & uniqueness.
- **Walk reachability.** `walk_graph` follows backward-data and forward-control. Every node with a path-to-entry under that definition must be visited.
- **No panic in normal paths.** `unwrap`/`expect`/`panic!` are bugs in any code path that handles user-controlled or builder-driven input. They are acceptable only for genuine "this can never happen" cases backed by a structural invariant; even then, prefer a runtime check that returns `Result`.
- **No `debug_assert!`.** Either a real runtime check or nothing.
- **`_ =>` catch-alls.** A wildcard arm in a `match` over a closed enum (`NodeKind`, `NodeOutputType`, `IntBinaryOp`, etc.) silently swallows new variants. Each one is a future-bug surface.

When something looks off, **act**:
- **Bug confirmed** → write a failing regression test, fix the bug, commit (one logical change per commit).
- **Not a bug, but the reasoning was hard** → write a test that pins the behavior + a one-line comment explaining the *why* if non-obvious; commit.
- **Not a bug** → move on; no commit.

### Pass 2 — Simplification read

Same files, second pass, looking for:
- Dead code, dead branches, unreachable matches.
- Redundant `clone()` / `to_vec()` / collect-then-iter chains.
- Manual loops clearer as iterator chains, *and* iterator chains clearer as loops — whichever serves the reader better.
- Tangled control flow that can be flattened with early returns.
- Names that don't say what the value *is*. Variables named after their type or after a piece of context that's already obvious.
- Unhelpful abstractions — wrapper types or helper functions used in exactly one place that would read clearer inlined.
- Code duplicated across modules — factor into a shared helper *only* when there are at least two real callers and the helper has a clear name.
- Comments that lie or paraphrase the code. Either rewrite for *why*, or delete.
- `pub` that should be `pub(crate)`.

A simplification ships only when it makes the code easier to reason about *for correctness*. Stylistic-only changes (e.g. shuffling whitespace, renaming for taste) are out of scope.

## Order of work

Risk lands first; restructure last; tests against the final shape.

1. **Worktree baseline.** Build, test, clippy snapshot. Run `cargo run --example analyzer` and capture `cfg.html`/`graph.html` for the before/after golden diff.
2. **Mechanical clippy fixes.** Small commits, low risk. Bucketed by lint:
   - `map_or` rewrites (29).
   - `#[must_use]` on getters and pure functions (32 + 2).
   - `# Errors` doc sections on `pub fn` returning `Result` (57).
   - `match_same_arms` resolutions (12) — case-by-case, may merge with `|` or refactor depending on intent.
   Each bucket is one or more commits.
3. **Pass 1 correctness read** in this order:
   - `validate/` (its invariants ground every other module).
   - `graph.rs` (use-list, dedup cache, arena).
   - `node.rs` + `node_signature.rs` (signatures must agree with builder + validator).
   - `walk.rs` (reachability used by validate Layer A and several passes).
   - `builder/` (every public method must produce a graph that validates).
   - `error.rs`, `function.rs`, `region.rs`, `iterators.rs`, `ops/`, `dot/`.
   Bug fixes and pinning tests committed inline.
4. **Pass 2 simplification read,** same order. Each simplification a separate commit.
5. **C-restructure.** Split `node.rs` and `graph.rs` per the final-structure layout. Done last, on known-good code.
   - `pub use` everything in `node/mod.rs` and `graph/mod.rs` so `ir::node::*` and `ir::graph::*` paths still resolve identically.
   - After each split, run `cargo build --workspace` and `cargo test --workspace` to verify no downstream breakage.
6. **Test architecture build-out.**
   - White-box: each restructured submodule gets its own `tests.rs`. Existing inline tests for unchanged modules (`validate/tests.rs`, `builder/tests.rs`, `dot/tests.rs`) audited for gaps and expanded.
   - Black-box: `crates/ir/tests/*` files per the final-structure layout. Each file exercises one invariant against the public API only.
   - Property-based: `tests/proptest_graph_invariants.rs` adds a `proptest` dev-dependency and runs the five properties below.
7. **Acceptance gate** (Section "Acceptance criteria") run end-to-end. If green, request user approval and merge `--no-ff` into `feature/ai`.

## Test architecture

### White-box (inline `#[cfg(test)] mod tests`)

Every restructured submodule and every existing module that holds non-trivial logic gets at least:
- One happy-path test per public method.
- One edge-case test per documented invariant the method is responsible for.
- Negative tests for every documented error condition.

### Black-box (`crates/ir/tests/`)

Each file targets one invariant against the public API only. No `pub(crate)` access.

| File | What it asserts |
|---|---|
| `build_validate_roundtrip.rs` | `FunctionBuilder::build()` produces a graph that `validate()` accepts, for every kind of construction (consts, ops, regions, phis, calls, returns, ifs, loads, stores). |
| `walk_reachability.rs` | `walk_graph` visits every node reachable under the documented backward-data + forward-control rule, and no others. Hand-crafted graphs cover phi joins, post-call states, multi-entry impossibilities. |
| `dedup_cache.rs` | Cacheable kinds with identical signatures dedup to one `NodeId`; non-cacheable kinds (StackStorePhi, Call, kinds with side-tables) never dedup; mutating a node evicts it from the cache. |
| `use_list_consistency.rs` | After every sequence of `add_node_input` / `remove_node_input` / `update_input` / `detach_node_inputs` / `replace_all`, forward and backward lists agree on every edge. |
| `validate_layer_a.rs` | Each `expected_signature` mismatch surfaces as a Layer A error. Layer A skips unreachable zombie nodes (per documented scoping). |
| `validate_layer_b.rs` | Manually corrupted use-lists are caught by Layer B in both directions. |
| `validate_layer_c.rs` | One negative test per Layer C invariant: duplicate Entry, duplicate InitialMemory, ControlState with non-Control predecessor, ControlPhi with phi-token from another ControlState, ControlPhi with wrong per-predecessor arity, StackStorePhi with arity != 3, PostCall without proper producer, duplicate PostCall on one Call. |

### Property-based (`tests/proptest_graph_invariants.rs`)

Generator: random sequences of public builder calls (`int_const`, `bool_const`, integer/bool/float ops, `region`, `phi`, `if`, `call`, `load`/`store`, `return`) bounded by node count (e.g. ≤ 50 nodes per case). Output: a `BuiltFunctionGraph`.

Properties (256 cases each by default; CI-tunable via env):

1. **Build-validate.** Any graph the generator produces must be accepted by `validate`.
2. **Walk completeness.** A hand-rolled BFS over backward-data + forward-control reaches the same set of nodes as `walk_graph`.
3. **Dedup determinism.** For any cacheable kind, two `create_node` calls with identical `(kind, inputs, output_kinds)` return the same `NodeId`.
4. **Use-list consistency.** After any sequence of input mutations applied to the generated graph, every forward edge has a matching backward edge and vice versa.
5. **Detach soundness.** After `detach_node_inputs(n)`, no other node's use-list contains an input that points at any of `n`'s outputs *for the inputs `n` previously held*; conversely, the outputs `n` produced retain their consumers.

Shrinking enabled. Failures print a minimized failing case for diagnosis.

### Test helpers

A single `crates/ir/tests/common/mod.rs` exposes shared builder helpers used across the integration tests (`make_fn`, `int_const`, `add_const`, `phi_at`, `call_at`, `return_kind`, etc.). Replaces ad-hoc duplication.

## Clippy strategy

Fix every warning. No blanket `#[allow]`. Per-site `#[allow]` with a one-line comment is permitted only when the lint is genuinely wrong for that site (rare).

By bucket:
- **`map_or` rewrites (29).** Mechanical: `x.map(f).unwrap_or(d)` → `x.map_or(d, f)`. One commit per logical group.
- **`#[must_use]` on getters and pure functions (34 total).** Mechanical: add the attribute. One commit per module.
- **`# Errors` doc sections (57).** Each `pub fn` returning `Result` gets a doc comment with an `# Errors` section listing the error variants it can return. This forces a real read of every public fallible function and often surfaces unused error variants and undocumented preconditions — that's value, not noise. One commit per module.
- **`match_same_arms` (12).** Each one reviewed individually. The validate Layer A subtype check (`output_kind_matches_expected`) is a prime example: the arms look identical syntactically but each documents a *different* type relationship. Solution there is to keep the arms separate but extract the dispatch into a small `is_compatible` helper that pattern-matches once and returns `bool` — preserving intent without tripping clippy. Other sites may genuinely merge with `|`.

Clippy runs after every commit during the work; any new warning blocks the next commit.

## Acceptance criteria

A merge into `feature/ai` is permitted only when **all** of the following are true on the worktree's HEAD:

1. `cargo build -p ir` clean.
2. `cargo build --workspace` clean (proves Option A path-preservation worked).
3. `cargo test -p ir` — all baseline 165 tests pass + every new test passes.
4. `cargo test --workspace` — all baseline workspace tests pass (proves no downstream regression).
5. `cargo clippy -p ir --all-targets --no-deps -- -D warnings` produces zero output.
6. `cargo run --example analyzer` produces `cfg.html` and `graph.html` byte-identical to the baseline captured at start of work. (Modulo any deliberate dot-rendering improvement, which would be called out in its own commit with the golden updated.)
7. Every bug found during Pass 1 has a regression test that fails on the pre-fix code.
8. Every property in `proptest_graph_invariants.rs` runs at least 256 cases without failure on `cargo test -p ir --release`.

**Merge style:** `git merge --no-ff review/ir-crate` into `feature/ai`. The merge commit message references this spec and lists the bugs found and the test-count delta.

## What this spec does *not* commit to

- The exact bugs that will be found. Pass 1 is discovery; the plan will record findings as they surface, but the spec cannot pre-enumerate them.
- The exact set of simplifications. Pass 2 is judgment-driven; only simplifications that pass the "easier to reason about for correctness" bar are made.
- The exact line-by-line shape of the split files. The structure is fixed (Section "Final module structure"); the assignment of borderline functions to one submodule vs another (e.g., a helper that touches both store and uses) is decided during the restructure step based on what reads cleanest.

These are deliberately under-specified — over-specifying them in the spec would either lock in wrong answers or force the spec to lie about what was done.
