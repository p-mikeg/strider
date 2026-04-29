# R4 — readability + doc/code drift + file organisation

## Executive summary

- **Files audited**: 46 modified across `ir/`, `opt/`, `pattern/`, `strider/`,
  `cfg/` (whole-workspace sweep for two-step `Graph` accessor calls and
  stale F-/W-numbered codenames; targeted reads for file-organisation
  candidates).
- **Renames / restructures**: 4 (new `Graph::kind_of_output` accessor +
  ~50 callsite migration, two `insn/` sub-files merged into parent,
  one test file rename, ten copies of an identical "F2 bridge" comment
  block deleted). Considered + rejected: 4 (broad `pub(crate)`
  demotion of `ir::node_signature` — internal-only but not a readability
  win; further `unreachable_pub` cleanup in pattern's user-facing DSL
  surface — those `pub` items are intentional; merging
  `cache/{lift,invalidate}.rs` back into `cache/mod.rs` — those still
  hold meaningful chunks of distinct logic; renaming `handle_*` /
  `process_*` methods — current names match the dispatch pattern).
- **LOC delta**: -111 lines net (`46 files changed, 268 insertions(+),
  379 deletions(-)`).
- **Final test count**: pre 2860 → post 2861 (one new accessor unit
  test).

## Methodology

Each candidate from the R4 brief was first verified by direct grep + a
representative read before any change. For the high-volume migration
(graph accessor) the work was split into two commits — one to
introduce + pin the new accessor, one for the mechanical migration —
so a regression in the migration would bisect cleanly to the second
commit. Verification per commit: `cargo build --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`. The full clippy sweep was clean at every
checkpoint.

## Changes landed

### R4-1 `Graph::kind_of_output` accessor — `b6312bd`, `187709a`

- **Files**: `crates/ir/src/graph/access.rs`, `crates/ir/src/graph/tests.rs`
  + 19 callsite files across `ir/`, `opt/`, `pattern/`, `strider/`.
- **Before**: `*fg.graph.node_kind(fg.graph.get_node_from_output(out))`
  was the most-repeated boilerplate in the codebase. R3-extension
  flagged the pattern at ~100 callsites.
- **After**: a single `kind_of_output(out)` field-access through the
  same arenas. Equivalence pinned by a unit test
  (`kind_of_output_matches_two_step_lookup`) that asserts the new and
  two-step forms agree.
- **Why this is clearer**: replaces an 80-character expression with a
  20-character one at every callsite where the intermediate `NodeId`
  was discarded. Where the same `NodeId` is also passed to
  `node_inputs`/`node_outputs`, the original two-step form was left
  in place.
- **Behavior-equivalence evidence**: pinning unit test + 2861/2861
  workspace tests pass.

### R4-2 Drop F5/relocation provenance prose from `opt::indirect_branch_resolve` — `45f6675`

- **Files**: `crates/opt/src/indirect_branch_resolve/{mod,classify,inplace}.rs`.
- **Before**: module headers spent two paragraphs describing the F5
  refactor that moved code from strider into opt — historical only,
  no reader benefit.
- **After**: each header is a one-line summary of what the module
  *does* now ("Producer-shape classifier..." / "In-place IR edits...").
- **Why this is clearer**: removes 51 lines of provenance prose; what
  survives is task-relevant and grounded in current behavior.

### R4-3 Rename `manual_rewrite.rs` → `graph_rewriter.rs` — `638d74c`

- **Files**: `crates/strider/tests/manual_rewrite.rs` (renamed +
  module-doc rewrite).
- **Before**: filename suggested "manual" rewriting, but every test
  uses `pattern::rewrite_rule` + `strider::GraphRewriter`. The name
  predated the F6 `GraphRewriter` API.
- **After**: filename matches what's exercised; module doc loses two
  stale "F6+F7" tags.
- **Behavior-equivalence evidence**: `cargo test --test graph_rewriter`
  → 6/6 ok.

### R4-4 Merge `strider::insn::{memory,misc}` into parent — `9187fd3`

- **Files**: `crates/strider/src/strider/insn/{memory.rs,misc.rs,mod.rs}`.
- **Before**: two ~40-line orphan files holding one method each
  (`handle_store`, `handle_call_other`); the split predated
  `pcode-lift` absorbing every value-producing opcode.
- **After**: both methods live next to the dispatch table in `mod.rs`;
  the file `insn/control.rs` (530 lines, distinct topic) stays
  separate.
- **Why this is clearer**: cuts two files; readers no longer chase
  `handle_store` to a 34-line file with one private helper and one
  method.

### R4-5 Drop dead `#[allow(dead_code)]` cruft — `40dd525`

- **Files**: `crates/opt/src/indirect_branch_resolve/jump_table.rs`,
  `crates/opt/tests/common/mod.rs`,
  `crates/strider/src/indirect_resolve_tier2/classify.rs`.
- **Before**: three unused suppressions, each justified by a comment
  that no longer matched the code (a "preserved verbatim" struct
  field that was set but never read; a `_unused()` no-op marker;
  a `_unused_to_keep_imports` whose import was actually used four
  times).
- **After**: the dead field is removed (and the surrounding match
  arm binds `Load(_space)`); the no-ops are deleted.
- **Why this is clearer**: every `#[allow(...)]` that survives the
  cleanup now reflects a real constraint.

### R4-6 Strip stale F-/W-numbered project codenames from comments — `8ebc40f`

- **Files**: 19 — `crates/cfg/src/cfg/query.rs`,
  `crates/ir/tests/builder_extended_use.rs`,
  `crates/opt/src/{call_other_elide,constant_fold,dead_branch,
  function_args,known_bits,load_readonly,redundant_phis,
  stack_load_forward,stack_store/{call_args,detect}}/...`,
  `crates/opt/src/pipeline.rs`,
  `crates/opt/src/indirect_branch_resolve/classify.rs`,
  `crates/opt/src/stack_load_forward/tests.rs`,
  `crates/opt/tests/indirect_branch_resolve.rs`,
  `crates/strider/src/{lib,strider/pipeline,
  indirect_resolve_tier2/orchestrator}.rs`.
- **Before**: ten identical 4-line "F2 bridge:" comment blocks above
  every `crate::pipeline::with_built(...)` call; F3/F5/W2/W5/W10
  tags scattered across pass-trait docstrings, test names, and
  module headers.
- **After**: the duplicated `with_built` comment block is gone (the
  helper has its own doc); the surviving prose summarises the
  *current* shape rather than historical step numbers.
- **Why this is clearer**: ~50 lines of provenance noise removed; a
  reader new to the codebase no longer has to interpret "F2
  contract" / "W10 — single-line summary" tags whose meaning lives
  in deleted planning docs.

## Rejected

- **Broad `pub(crate)` demotion of `ir::node_signature`** — `cargo
  clippy -- -W unreachable_pub` flags `Slot`, `SlotRole`, `SlotList`
  etc. as unreachable. They are intentionally `pub` so they can be
  `pub use`-d at module roots if needed; demoting them is mechanical
  visibility hygiene, not a readability gain.
- **`pattern` user-facing DSL `pub` items** — clippy flags many of
  the `IntBinaryOpPat` etc. builders as unreachable inside individual
  test crates. They are part of the library's user API. Skip.
- **Re-merging `cache/{lift,invalidate,extend}.rs`** — each is
  60–200 LOC with cohesive responsibilities (extend = predecessors,
  invalidate = post-resolve deltas, lift = entry construction). The
  W6 split is doing real navigation work. Leave.
- **Renaming `handle_X` / `process_X`** — the dispatch table in
  `insn/mod.rs` already documents what each `handle_*` arm does;
  renaming to `lift_*` would just be taste.

## Open questions for R5

- The `strider::ir_cache` `pub use` alias is `#[doc(hidden)]` and
  has one external user (`tests/tier2_cache.rs`). Worth dropping
  the alias in R5 if the test file gets renamed?
- `crates/opt/src/indirect_branch_resolve/classify.rs:5` still
  comments "Each arm is a soundness-checked shape ... — the
  comments on each arm spell out why the runtime target set is
  constrained." That's accurate today; if R5 finds an arm whose
  rationale comment has drifted, the module-level claim will need
  to follow.
- `JumpTableShape.entry_size` is now the only multi-line field;
  if a future round wants to factor the "padding-aware stride"
  comment out of the struct, fine, but it's load-bearing right now.

