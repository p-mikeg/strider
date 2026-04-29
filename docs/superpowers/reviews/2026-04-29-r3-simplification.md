# R3 — simplification + unification

Round 3 of 5.  R3's job is duplication / parallel abstractions / shim
layers across crates — consolidate without changing behavior.

## Executive summary

- **Files audited:** ~95 production .rs files across `crates/` (full
  workspace), driven from R3's six numbered candidates plus a
  workspace-wide grep for cross-crate duplication.
- **Consolidations landed:** 5.
- **Considered + rejected:** 4 (one-liners below).
- **Dead code removed:** 5 files deleted (`crates/ir/src/node/pcode_addr.rs`;
  `crates/strider/src/indirect_resolve_tier2/{jump_table,stack_array}.rs`;
  `crates/strider/tests/common/tier2_helpers/{cache,inplace}.rs`).
  Net diff: 18 files changed, +71 / −241 = **−170 LOC**.
- **Final test count:** pre 2860 / 0 / 26 → post 2860 / 0 / 26 (Δ=0).
  Clippy clean both ends.

## Methodology

Candidates were sourced from the R3 brief's six lettered items
(A–F).  For each:

1. Verified end-to-end with `grep` over `crates/` (excluding the
   `.claude/worktrees/` shadow tree).
2. Checked the workspace dep graph (`Cargo.toml`) before moving any
   type across crates.
3. After every commit ran `cargo test --workspace` (full 2860/0/26)
   and `cargo clippy --workspace --all-targets -- -D warnings` (clean)
   before staging the next change.
4. Behaviour-equivalence pinned by the existing test suite — every
   consolidation was a name change or a delegation collapse with no
   match-arm logic touched.

No characterization tests needed; the behaviour each refactor
preserves is already pinned by `tests/tier2_*` and
`opt::indirect_branch_resolve::*::tests`.

## Consolidations landed

### R3-1 — drop dead `ir::PcodeInsnAddr`

- **Files:** `crates/ir/src/node/pcode_addr.rs` (deleted),
  `crates/ir/src/node/mod.rs`, `crates/ir/src/lib.rs`.
- **Commit:** `908b973`.
- **Before:** `ir` exposed `PcodeInsnAddr` (a 2-field struct
  structurally identical to `cfg::PcodeInsnAddr`) with a doc-comment
  claiming `cfg`, `pcode-lift`, and `strider` would build one via
  `PcodeInsnAddr::new`.  Workspace grep confirmed the type's only
  references were its own `pub use` re-exports — zero callers.
- **After:** type and re-exports gone.  `cfg::PcodeInsnAddr` remains
  the single source of truth.
- **Lines:** −39 / +0.
- **Behaviour-equivalence:** delete-only; unused symbol.  Confirmed
  by full-workspace grep and successful build.
- **Confidence preserved:** high — dead-code deletion.

### R3-2 — collapse `BranchResolution` / `ResolvedTargets` duplicate enum

- **Files:** `crates/opt/src/indirect_branch_resolve/{mod,classify,
  jump_table,stack_array}.rs`, `crates/opt/src/indirect_branch_resolve/
  jump_table_tests.rs`, `crates/opt/src/lib.rs`,
  `crates/cfg/src/cfg/builder/indirect_resolve.rs`,
  `crates/strider/src/indirect_resolve_tier2/{classify,jump_table,
  stack_array}.rs`.
- **Commit:** `2a7a154`.
- **Before:** `opt` defined `BranchResolution { LinkRegister,
  Single(u64), Multiple(Vec<u64>) }`; `cfg` defined
  `ResolvedTargets { LinkRegister, Single(u64), Multiple(Vec<u64>) }`.
  Two structurally identical enums with a doc-comment in
  `opt::indirect_branch_resolve::mod.rs` justifying the split via
  "opt → cfg would be a dep cycle."  Strider's tier-2 shims spent
  four `match` blocks (one in each of `classify.rs`, `jump_table.rs`,
  `stack_array.rs`) re-packing each variant from one enum to the
  other.
- **After:** `opt::BranchResolution` renamed to `opt::ResolvedTargets`.
  `cfg`'s enum declaration replaced with `pub use opt::ResolvedTargets;`.
  All four conversion `match` blocks become straight `opt::classify_*`
  delegations.  The `from_opt` helper in
  `strider::indirect_resolve_tier2::classify` deleted.
- **Lines:** ~−45 / +20.
- **Behaviour-equivalence:** the two enums had identical variants and
  identical semantics; `from_opt` mapped each variant 1:1.  Type
  identity now eliminates the possibility of a future drift between
  the two enums.
- **Confidence preserved:** high — type-level rename + re-export, no
  match-arm logic touched.  Tests pass unchanged.

### R3-3 — drop empty `tier2_helpers` placeholder sub-modules

- **Files:** `crates/strider/tests/common/tier2_helpers/cache.rs`
  (deleted), `crates/strider/tests/common/tier2_helpers/inplace.rs`
  (deleted), `crates/strider/tests/common/tier2_helpers/mod.rs`.
- **Commit:** `fd3b07d`.
- **Before:** Both sub-modules were 13 lines each — one `pub use
  super::classify::build_initial_var_target_scenario_x86_64;` line
  plus a doc-comment promising "future cache-only / inplace-only
  fixtures."  No external caller imported either path; every
  integration test reached the helper through the flat re-export in
  `mod.rs`.
- **After:** both files deleted; `mod.rs`'s table of sub-modules
  shortened from 4 to 2.  The doc-comment now explains why R3
  removed the placeholders.
- **Lines:** −33 / +13.
- **Behaviour-equivalence:** the two files re-exported one helper;
  the same helper was already re-exported flat from `mod.rs`.  Tests
  unchanged.
- **Confidence preserved:** high — both files were unused
  pass-through re-exports.

### R3-4 — drop unused strider-side `classify_jump_table` /
`classify_stack_array` shims

- **Files:** `crates/strider/src/indirect_resolve_tier2/jump_table.rs`
  (deleted), `crates/strider/src/indirect_resolve_tier2/stack_array.rs`
  (deleted), `crates/strider/src/indirect_resolve_tier2/mod.rs`.
- **Commit:** `e180413`.
- **Before:** Two ~14-line files in `strider/src/indirect_resolve_tier2/`
  each delegated to `opt::classify_*` with the only logic being a
  variant-by-variant remap from `opt::BranchResolution` to
  `cfg::ResolvedTargets`.  R3-2 collapsed the two enums into one,
  reducing each shim to a one-line forward.  Workspace grep showed no
  external caller (orchestrator, integration tests, examples)
  imported either shim — they were dead public API surface.
- **After:** both files deleted, `mod.rs`'s `pub use` list shortened.
  Callers needing the rodata-jump-table or stack-array arms call
  `opt::classify_jump_table` / `opt::classify_stack_array` directly,
  or the umbrella `classify_anchor_with_rom_and_sp` which routes
  into both arms.
- **Lines:** −46 / +0.
- **Behaviour-equivalence:** unused public shims.
- **Confidence preserved:** high — dead-public-API deletion.

### R3-5 — inline `find_placeholder_return_for_anchor` wrapper

- **Files:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs`.
- **Commit:** `b9fe125`.
- **Before:** A 13-line file-private wrapper in `orchestrator.rs`
  whose entire body was unwrapping `&BuiltFunctionGraph` into the
  `&Graph` that `opt::find_placeholder_return_for_anchor` already
  accepts.  Two call sites in the same file.
- **After:** wrapper deleted.  Both call sites read
  `opt::find_placeholder_return_for_anchor(&graph.graph,
  *anchor_output)`.
- **Lines:** −24 / +2.
- **Behaviour-equivalence:** trivial — wrapper was pure delegation.
- **Confidence preserved:** high.

## Considered + rejected

- **Candidate D — per-arch CC files repeating boilerplate.**  The
  brief flagged "15 calling conventions, each with ... 15 modules of
  fn-returning-Vec".  Verified: there is no `crates/strider/src/strider/cc/`
  directory.  All 11 calling conventions live in a single file
  (`crates/target/src/calling_convention.rs`) sharing one
  `CallingConvention` data struct + a `build()` method that resolves
  register names to varnodes.  Already consolidated.  **No change
  needed.**

- **Candidate E — repeated `match graph.node_kind(node) {
  NodeKind::X(...) => ... else None }` boilerplate.**  Audited
  `crates/opt/src/**/*.rs` and `crates/strider/src/**/*.rs`.  The
  repetitions are all `matches!(...,  NodeKind::X)` filters in
  preorder walks (~25 sites).  Each one pairs a different
  `NodeKind` variant with its own follow-up logic — they look
  similar but the bodies diverge.  Adding a `match_node_kind!`
  macro or a typed accessor would replace one idiom with another
  without reducing reading effort.  **Keep as-is.**

- **Candidate F — collapse remaining `tier2_helpers` split.**  After
  R3-3 the dir holds two real sub-modules: `classify.rs` (772 LOC,
  one fixture per producer-shape arm) and `orchestrator.rs` (124 LOC,
  pipeline-runner helpers).  The split is real; recombining them
  would put 900 lines back in one file with two unrelated concerns.
  **No further change.**

- **Strider `inplace.rs` shim layer.**  The shim's
  `opt_to_strider_err` re-routes `opt::ErrorKind::IrError` → strider's
  `ErrorKind::IrError` (skipping the `OptError` wrapper layer the
  blanket `bridge_error!` would impose) and
  `opt::ErrorKind::ExpectedNodeNotFound` → `WrongNodeKind`.  No
  test asserts on the specific kind, so we *could* drop the routing
  and let the blanket `bridge_error!(opt::Error => Error,
  ErrorKind::OptError)` handle everything.  But that's a public
  error-shape change — Python bindings or downstream consumers may
  pattern-match on the strider-side variant — and R3's brief is
  "preserve behavior."  **Leave for an explicit API decision in a
  future round.**

## Open questions for R4 / R5

- **R4 (readability).**  After R3-2 the rename
  `BranchResolution` → `ResolvedTargets` propagated through opt's
  classify / jump_table / stack_array; doc-comments inside those
  files still talk about "the canonical implementation" / "F5 shim"
  in places where the F5 split is no longer interesting.  Worth
  scrubbing for stale provenance prose.
- **R4 (file organisation).**  `crates/strider/src/indirect_resolve_tier2/`
  now holds 4 files (was 6).  Of these, `classify.rs` is a 477-line
  shim where the public surface is 3 fns and the rest is unit tests.
  R4 may want to either move the unit tests under `#[cfg(test)] mod tests`
  in a sub-module of the actual `opt`-side classifier (since
  the shim has no logic to test independently) or accept the current
  split.
- **R5 (test gaps).**  Each strider-side shim has its own unit-test
  block that overlaps with the opt-side tests.  R5 might prune the
  duplicated tests once it confirms the opt-side coverage is
  sufficient — a separate concern from R3's "no behavior change."
- **Error-shape decision for `strider::indirect_resolve_tier2::inplace`.**
  See "Considered + rejected" above.  Future round should decide
  whether the custom `opt_to_strider_err` routing is part of the
  public API contract (preserve) or an accident of history (drop in
  favour of the blanket `bridge_error!`).
