# Per-arch re-CR — cefa0db..b5419b6

## Executive summary

- **2 bugs found total, 1 fixed in this pass, 1 deferred to follow-up.**
  - Plus several lower-confidence concerns and provenance-contract
    observations flagged but not fixed.
- **Highest-confidence finding (FIXED): the in-place tail-call edit's
  `clobbered_kinds` came from `cc.callee_saved_regs` (the OPPOSITE of
  caller-saved/clobbered), producing Calls whose output-slot ↔
  `call_clobbered[i]` mapping diverged from
  `FunctionBuilder::build_call`. Pattern queries via `Match::get_vn`
  over those Calls returned wrong varnodes.** Fix commit `92d2cee`.
- **Most surprising:** the bug was hidden because none of the
  orchestrator-driven tier-2 in-place-tail-call tests reliably FIRE
  the in-place edit on the existing fixtures (`stats.tail_call_edits`
  is conditionally `>= 1` — when it's `0`, the test bails). The fix's
  test bypasses the orchestrator's outer flow and unit-tests
  `build_anchor_calling_context` directly to expose the shape
  inconsistency.

## Methodology

### Files audited (load-bearing changes in cefa0db..b5419b6)

- `crates/opt/src/indirect_branch_resolve/{mod, classify, jump_table, stack_array, inplace}.rs` — F5 + F3 + H0 + Phase-5
- `crates/strider/src/indirect_resolve_tier2/{orchestrator, classify, inplace, jump_table, stack_array, mod}.rs` — orchestrator + shims
- `crates/strider/src/strider/insn/{control, mod}.rs` — F7 If-ladder + F1 lift seeding
- `crates/strider/src/strider/pipeline.rs` — analyze_cfg threading + Switch terminator dispatch
- `crates/strider/src/cache/*.rs` — RegionIrCache lifecycle
- `crates/cfg/src/cfg/{types, builder/region_builder, builder/indirect_resolve, query}.rs` — W3 + W9 + Switch
- `crates/ir/src/builder/{mod, call, nodes}.rs` — FunctionBuilder shape (build_call + new_raw)
- `crates/ir/src/graph/store.rs` + `crates/ir/src/node/fingerprint.rs` — F1 dedup + auto-merge
- `crates/ir/src/ops/rewrite.rs` — replace_all_uses
- `crates/opt/src/sp_expr.rs` — decompose_sp soundness
- `crates/opt/src/stack_load_forward/mod.rs` — narrow-load BE handling
- `crates/opt/src/{constant_fold, known_bits, load_readonly, redundant_phis, dead_branch, stack_store, call_other_elide}/mod.rs` — fold-rule fingerprint propagation
- `crates/opt/src/pipeline.rs` — final-validate, pipeline composition
- `crates/pattern/src/rewrite.rs` — root_fp merge into producer_fp
- `crates/target/src/calling_convention.rs` — every preset (15 cases)

### Archs covered (all 15)

x86, x86_64, aarch64 (LE), aarch64be, arm (LE), arm_be, arm_thumb,
mips32le, mips32be, mips64le (ignored for BUG-30 only), mips64be
(ignored for BUG-30 only), ppc32be (ignored for BUG-30 only), ppc32le
(ignored for BUG-30 only), ppc64be (ignored for BUG-30 only), ppc64le
(ignored for BUG-30 only).

The clobbered-kinds bug is per-arch in the sense that EVERY
calling-convention preset is affected (each has a different
callee-saved set vs. all_used_variables-derived call_clobbered). The
visible mismatch sizes per arch:

- x86 cdecl: cc.callee_saved=4, canonical varies.
- x86_64 SysV: cc.callee_saved=6, canonical varies.
- aarch64 AAPCS64 (LE+BE): cc.callee_saved=12, canonical varies.
- arm AAPCS (LE+BE+Thumb): cc.callee_saved=9, canonical varies.
- mips O32 (LE+BE): cc.callee_saved=11, canonical varies.
- mips N64 (LE+BE): cc.callee_saved=11, canonical varies.
- ppc SysV32 (LE+BE): cc.callee_saved=19, canonical varies.
- ppc ELFv1/v2 (LE+BE): cc.callee_saved=19, canonical varies.

### TDD discipline

- Bug F-1 fixed under TDD: failing test added FIRST
  (`build_anchor_calling_context_clobbered_matches_call_clobbered` in
  `crates/strider/src/indirect_resolve_tier2/orchestrator.rs`).
  Watched it fail with `6 != 4`. Wrote minimal fix. Watched it pass.
  Re-ran full suite + clippy. Single commit per the mission's
  "one bug per commit" rule.
- No deviations.

### Verification

- Pre-mission baseline: **2895 passed / 0 failed / 26 ignored**.
- Post-fix baseline: **2896 passed / 0 failed / 26 ignored** (+1
  from the new TDD test).
- `cargo clippy --workspace --all-targets`: clean (no new warnings).

## Findings (by severity)

### Critical (correctness — wrong IR for some valid input)

#### F-1 (FIXED) `apply_in_place_edit`'s tail-call Call has wrong clobbered-output shape

- **Location**: `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:981-990` (pre-fix); same range post-fix.
- **Symptom**: When the orchestrator's tier-2 fixed-point loop
  classifies an `UnresolvedIndirectBranch` as `Single(K)` with K out
  of function range, `apply_in_place_edit` splices a fresh
  `Call(IntConst(target)) → Return(ret_vals)` chain. The Call's
  output kinds were `[Control, Memory] + cc.callee_saved_regs`-sized
  slots, NOT `[Control, Memory] + BuiltFunctionGraph::call_clobbered`-
  sized slots. Pattern queries via
  `pattern::Match::get_vn(call_out_slot)` index `slot 2 + i ↔
  call_clobbered[i]` (per `crates/pattern/src/matcher/match_result.rs`),
  so the in-place-edit Call's slot mapping was OFF — wrong varnodes,
  wrong widths, mismatched between the in-place vs CFG-rebuild paths
  of the same orchestrator.
- **Root cause**: The pre-fix loop iterated `cc.callee_saved_regs`
  (the SET of registers the callee MUST PRESERVE) and emitted one
  output kind per varnode. The comment said "Clobbered = caller-saved"
  but the implementation iterated callee-saved (the OPPOSITE), which
  is internally contradictory. The canonical shape — what
  `FunctionBuilder::build_call` produces — derives clobbered_kinds
  from `self.call_cloberred_variables` (caller-saved + ret-val
  prefix), which is the same list as
  `BuiltFunctionGraph::call_clobbered`.
- **Test that pins it**:
  `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:1601-1690`
  (`build_anchor_calling_context_clobbered_matches_call_clobbered`).
- **Fix commit**: `92d2cee`.
- **Confidence**: **High**. The semantic mismatch is unambiguous (the
  comment said one thing, the code did the opposite); the test pin
  produces deterministic 6-vs-4 disagreement on x86_64; the fix uses
  the same field (`graph.call_clobbered`) that pattern queries
  already index against, eliminating the mismatch by construction.
- **Archs affected**: ALL 15 — every CC preset has a different
  callee_saved set vs. all_used_variables-derived
  call_clobbered set. The bug is symmetric across every arch the
  orchestrator's tail-call in-place edit ever fires on.

### Major (correctness only under edge case)

(None found. The provenance / fingerprint observations below are
flagged as Found-but-not-fixed because they're contract violations
without an arch-correctness consequence on the IR graph itself.)

### Minor (cleanup found on the way)

(Pure cleanup nits skipped per mission scope.)

### Found but not fixed

#### F-2 F1 fingerprint contract: cache-hit at `Graph::create_node` doesn't add new pcode addrs

- **Location**: `crates/ir/src/graph/store.rs:72-144`,
  `crates/pcode-lift/src/lib.rs:109-134`,
  `crates/strider/src/strider/insn/mod.rs:104-121`.
- **Symptom**: When pcode insn A creates `IntConst(0x42) U64`, its
  fingerprint becomes `{A}`. When pcode insn B at a different machine
  address ALSO creates `IntConst(0x42) U64`, the dedup cache hits and
  returns the existing node. The lift-time seed loop (in
  `lift_with_addr` and `IrStrider::process_insn`) iterates only
  `pre_count..post_count` of newly-allocated node ids — the cached
  node's id is NOT in that range, so insn B's address is never
  merged into the cached node's fingerprint. Result: pattern matches
  whose proof-of-work runs through that cached `IntConst` will not
  surface insn B as a contributor.
- **Root cause**: F1's design choice (per commit `28f6f10`'s message:
  "Cache hits in create_node return existing nodes that already
  carry their original addrs, so the merge is idempotent"). The
  comment in `Graph::create_node` argues the cached node's
  fingerprint is "necessarily a superset of (or equal to) the merge"
  — true RELATIVE to the auto-merge from inputs, but FALSE relative
  to the additional lift-time seed the caller would otherwise have
  added.
- **Why I didn't fix**: Two reasons.
  1. The contract violation is for "proof-of-work pattern queries"
     accuracy — it doesn't affect IR graph CORRECTNESS on any arch.
     Per the mission's "Correctness of the IR graph is the most
     crucial thing," this isn't in primary scope.
  2. Fixing it requires reaching INTO `create_node` to make the seed
     merge happen on cache hits too, which would change the F1 design
     contract that the F1 commit message explicitly anchors as
     "idempotent."
- **Suggested remediation**: Either (a) update the F1 contract docs
  to say "cached nodes' fingerprints reflect their FIRST construction
  context only, NOT the union of every pcode addr that produces them
  via dedup," explicitly accepting the relaxed semantic, or (b) move
  the lift seed from a `pre_count..post_count` loop into
  `lift_with_addr` returning a list of touched node-ids (including
  cache hits) that the caller can seed. Option (a) is cheaper and
  matches the existing test coverage; option (b) would require the
  pcode-lift / strider lift sites to track touched-via-cache nodes.
- **Confidence**: **Medium-high** that this is a real contract
  violation. **Low** that it has observable consequences for any
  current pattern user (no test pins it; no downstream consumer is
  known to depend on the strict contract).

#### F-3 RedundantPhis collapse loses the phi node's lift-time fingerprint

- **Location**: `crates/opt/src/redundant_phis/mod.rs:60-119`.
- **Symptom**: When `remove_phis` collapses a single-input ControlPhi
  / MemPhi, it `replace_output_uses(phi_output, value)`. Consumers
  now flow from `value` directly. The phi node's own fingerprint
  (which may have included lift-site seeds from
  `process_insn`'s post-loop seeding) is detached from the chain;
  the consumer's fingerprint, computed at consumer-creation time,
  doesn't get retroactively updated.
- **Why I didn't fix**: Same shape as F-2 — provenance contract
  violation, not arch-correctness. Plus, the phi nodes are scaffolding
  that exist independently of any pcode insn (per
  `fingerprint_e2e.rs`'s test, ControlPhi / MemPhi explicitly start
  empty), so in practice the phi's fingerprint is empty and the
  "loss" is vacuous. But on phis that DID gain a lift-time seed
  (e.g. from process_insn's post-loop seeding when a control-flow
  handler created the phi), the seed is lost.
- **Suggested remediation**: Have `replace_output_uses` (or the
  caller) merge the old producer's fingerprint into each consumer's
  fingerprint. This would be a wider change touching every pass that
  uses `replace_all_uses`.
- **Confidence**: **Medium**. I traced the code but did not
  construct a failing test demonstrating provenance loss. The
  vacuity-on-empty-phi-fingerprints argument may make this a
  non-issue in practice.

#### F-4 `apply_link_register` doesn't merge ret_val_outputs' fingerprints into the modified Return

- **Location**: `crates/opt/src/indirect_branch_resolve/inplace.rs:46-59`.
- **Symptom**: `apply_link_register` calls
  `graph.add_node_input(placeholder_return, ret)` for each ret-val
  output. `add_node_input` doesn't touch the Return's fingerprint.
  Post-edit, the Return's fingerprint is whatever it was pre-edit
  (typically `{lift-site-of-the-original-bx-lr-insn}`), without
  merging in the ret-val outputs' fingerprints.
- **Why I didn't fix**: Same as F-2/F-3 — provenance contract
  violation, not arch-correctness. Pattern queries that use the
  Return's fingerprint specifically (rare today) would see incomplete
  provenance.
- **Suggested remediation**: After every `add_node_input`, merge the
  added input's producer's fingerprint into the Return's fingerprint.
- **Confidence**: **Medium**.

#### F-5 LSB-strip `for _ in 0..4` cap may miss real ARM Thumb dispatches

- **Location**: `crates/opt/src/indirect_branch_resolve/stack_array.rs:151-205`.
- **Symptom**: `strip_target_mask` walks at most 4 layers of And /
  Or wrappers. The comment says "ARM-Thumb commonly nests
  `And(Or(load, 1), 0xFFFFFFFE)` — that's 2 layers." But what about
  Sleigh-emitted intermediate nodes (Truncate / Extend) between the
  And and the Load? If the lifter places `And(Truncate(Or(load, 1)),
  0xFFFFFFFE)`, the inner producer of the And is a Truncate, NOT an
  Or, and the strip-Or arm doesn't fire. This is unrelated to the
  layer cap but is reachable on archs where Sleigh emits Truncate
  wrappers.
- **Why I didn't fix**: I have no test reproducing this pattern from
  a real arm/arm_be lift, so the hypothesis is speculative. The 8
  archs that DO claim BUG-30 support pass their tests, so this
  potential issue isn't observed today.
- **Suggested remediation**: Add `Truncate` and
  `Extend(ZeroExtend)` arms to the strip loop alongside the And /
  Or arms. Run the existing
  `indirect_branch_resolved_arm{,_be,_thumb}` tests to confirm no
  regression.
- **Confidence**: **Low**. Pure code-reading speculation; couldn't
  construct a failing test from existing fixtures.

#### F-6 `region_id_at_start` linear scan is order-dependent on duplicate machine_addrs

- **Location**: `crates/cfg/src/cfg/query.rs:119-128`.
- **Symptom**: If two regions share `start_addr.machine_addr` (one
  with insn_index=0, another with insn_index>0 — possible if a
  machine instruction lifts to multiple pcode insns and the lifter
  splits mid-instruction), the linear scan returns the FIRST match
  in `node_indices()` order. `node_indices()` is
  insertion-order-stable for `petgraph::StableDiGraph`, so this is
  deterministic in practice but isn't documented.
- **Why I didn't fix**: I couldn't construct a real fixture where
  two regions share machine_addr — F7 jump-table targets are
  always machine-aligned (each lifts at insn_index=0), and other
  region-creation sites also use insn_index=0 for region starts.
  The hypothesis remains theoretical.
- **Suggested remediation**: Document the order-dependence
  explicitly OR change the lookup to fail when multiple regions
  share `machine_addr` (return None or Err). Or change the key to
  include insn_index.
- **Confidence**: **Low**. No failing test; theoretical.

#### F-7 W9 `target_value: Some(_)` path is plumbed but never exercised

- **Location**: `crates/cfg/src/cfg/builder/region_builder.rs:444-460`,
  `crates/strider/src/strider/insn/control.rs:201-206`.
- **Symptom**: The cfg builder always constructs `Switch { ...,
  target_value: None }`. F7's `handle_switch` falls back to
  `read_vn(target_vn)` in that case. The `Some(v)` arm is dead
  code. This is plumbing for a future feedback path but the feedback
  path is never wired up.
- **Why I didn't fix**: Not a bug — dead-but-correct code. Mission
  specified to flag found-and-noted code paths.
- **Suggested remediation**: Either wire up the orchestrator's
  feedback path (W9 was supposed to do this but the cfg builder's
  `with_known_targets` doesn't construct Switch with target_value
  populated) or remove the dead Some(v) arm and the Option-typed
  field.
- **Confidence**: **High** that this is dead code. **No correctness
  consequence today** — dead code is harmless.

## Confidence calibration

### Where I was confident and right

- F-1 (clobbered-kinds shape mismatch): the comment said "caller-saved"
  while the loop iterated callee-saved — this is unambiguously
  contradictory. The fix's failing test produced a clean `6 != 4`
  comparison, and the fix produces 4 (matching the canonical). Full
  workspace test count went 2895 → 2896 (the new test); no other tests
  flipped.

### Where I was wrong and learned

- I initially tried to pin F-1 via the orchestrator's public surface
  using fixtures that DO have a tail-call resolution. I burned ~20
  minutes trying both `push K; pop rax; jmp rax` and `mov rax, K;
  jmp rax` fixtures. The push/pop fixture's orchestrator returned
  `UnresolvedIndirectBranch` because the orchestrator's STABLE
  optimizer subset (which excludes `RedundantPhis` /
  `DeadBranchElimination`) doesn't fully fold push/pop into IntConst
  during intermediate iterations — the trivial ControlPhi over rax
  doesn't collapse without RedundantPhis. The mov fixture was
  caught by the cfg builder's tier-1 mini-IR resolver before the
  orchestrator even saw it.
  - **Lesson**: existing tests' tolerance pattern (`if
    stats.tail_call_edits >= 1 { assert ... }`) is a tell that the
    fixtures don't reliably fire the in-place edit. Going through
    the **private helper directly** in a unit test was the right
    move.

### Open questions for the user

1. **F-2 / F-3 / F-4** (provenance contract violations): the F1
   commit message says cache hits are "idempotent." Should this be
   formalised as the contract (i.e., update docs to say cached
   nodes' fingerprints are first-construction-only), or is the
   intended contract truly "union of every pcode addr that flowed
   in" (i.e., should fix the lift seed loop to also touch cache
   hits)? My recommendation: pick (a) or (b) explicitly and update
   docs to match. The current state has the comment claim one
   contract and the code implement another.

2. **F-5** (LSB-strip with intermediate Truncate / Extend): worth a
   targeted fixture to confirm? The 8 archs that claim BUG-30
   support pass their tests today, but I couldn't tell from
   code-reading alone whether arm_be Sleigh emits Truncate wrappers
   that might surface the issue when its BUG-30 ignore is lifted.

3. **F-7** (W9 dead code): is the feedback path planned for a
   future round, or should the dead `Some(v)` arm + `target_value`
   field be removed?
