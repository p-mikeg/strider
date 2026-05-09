---
name: strider-validation-invariant-extend
description: Add a new Layer-A / B / C check to ir::validate — covers reachability scoping, opt-in ValidateOptions, the ValidationErrors aggregating bundle, and the round-8 zombie-node lesson.
---

# strider-validation-invariant-extend

## When to invoke

User wants to add a new structural invariant to the IR validator. Triggers include:

- "Add a new validate Layer-C check."
- "Detect zombie `InitialVar` nodes wired into post-stable-pipeline reads."
- "Strengthen `validate_with_options`."
- "How do I assert <new IR invariant> at validate time?"
- A round-8-style invariant violation (`round8-correctness-invariants.md` M-1: zombie-`InitialVar` resurrection in `apply_in_place_edits`) deserves a static check rather than per-test ad-hoc assertions.
- Adding an attribution / propagation invariant beyond fingerprints.

## When NOT to invoke

- The check is per-pass-internal — keep it inside the pass, not in `validate`. (E.g. `RedundantPhis` confirming it didn't drop a live phi.)
- The check is specific to one fixture — write a unit test, not a global validator.
- The check is dynamic (e.g. "value at runtime is in range") — that's not a structural invariant.
- The check would force a behavioural change on existing graphs — make it opt-in via `ValidateOptions` instead.

## Files this skill operates on

- `crates/ir/src/validate/mod.rs` — entry points `validate(graph, entry)` and `validate_with_options(graph, entry, options)`, plus the `ValidateOptions` struct.
- `crates/ir/src/validate/layer_a.rs` — local per-node typing checks (`expected_signature`).
- `crates/ir/src/validate/layer_b.rs` — bidirectional use-list consistency.
- `crates/ir/src/validate/layer_c.rs` — graph-level invariants (uniqueness, predecessor kinds, phi token ownership, function-arg uniqueness, wide-const interning, opt-in asm-fingerprint coverage).
- `crates/ir/src/validate/tests.rs` — graphmock-driven unit tests.
- `crates/graphmock/src/lib.rs` — for constructing the deliberately-malformed test graphs.

## Procedure

1. **Decide which layer the new check belongs in.** The three layers are:

   - **Layer A** — local per-node typing. Each node is checked in isolation against `expected_signature` (the single source of truth in `crates/ir/src/node_signature.rs`). Checks input slot kinds and output slot kinds. Add here only if the invariant is per-node-local AND the relevant kind isn't already covered by `expected_signature`.

   - **Layer B** — bidirectional use-list consistency. For every edge `producer.output → consumer.input`, the producer's use-list must contain the consumer and vice versa. Rare to extend.

   - **Layer C** — graph-level invariants. Most new checks land here. Existing Layer-C checks: `check_layer_c_uniqueness` (Entry/InitialMemory uniqueness — globally scoped, NOT reachability-gated), `check_layer_c_control_state` (predecessor kinds), `check_layer_c_phis` (phi token ownership + per-predecessor arity), `check_layer_c_function_arg_uniqueness` (one node per arg index), `check_layer_c_wide_consts` (`IntConstWide(id)` references a live wide-const entry), and the opt-in `check_layer_c_asm_fingerprints` (round-7 attribution invariant).

2. **Decide reachability scoping.** This is the round-8 lesson (`round8-correctness-invariants.md` M-1). Most Layer-C checks must be reachability-scoped:

   ```rust
   for node in graph.nodes.keys() {
       if !reachable.contains(node) {
           continue;
       }
       ...
   }
   ```

   Why: optimization passes (notably `RedundantPhis::detach_unreachable_nodes`) leave **zombie nodes** in the arena. They have no inputs, no live use-list entries, and are unreachable from `entry`, but they still occupy `NodeId`s in `graph.nodes.keys()`. Without scoping, a Layer-C check would false-positive on a perfectly-valid post-`RedundantPhis` graph because (e.g.) two `FunctionArg` nodes with the same index exist — one zombie + one live.

   The exception is `check_layer_c_uniqueness` (Entry/InitialMemory uniqueness), which deliberately scans the entire arena to catch a duplicate created and orphaned by a buggy pass. Anything else (`check_layer_c_function_arg_uniqueness` was changed in round 8 to be reachability-scoped, see `crates/ir/src/validate/layer_c.rs:228-253` and the comment at lines 222-227) should pass `reachable: &NodeIdSet` and gate on it.

   Layer A is already reachability-scoped at the entry (`mod.rs:90`). Layer B uses `reachable` to filter what it walks.

3. **Add an opt-in flag on `ValidateOptions`** if the new check would break existing graphmock tests that don't set up the invariant. Pattern:

   ```rust
   #[derive(Debug, Clone, Copy, Default)]
   pub struct ValidateOptions {
       pub check_asm_fingerprints: bool,
       pub check_<your_invariant>: bool,  // add here
   }
   ```

   Then gate the call in `validate_with_options`:

   ```rust
   if options.check_<your_invariant> {
       check_layer_c_<your_invariant>(graph, &reachable, &mut errs);
   }
   ```

   Default is `false`. Existing call-sites using the bare `validate(graph, entry)` see no behaviour change. Tests that exercise the invariant explicitly opt in via `validate_with_options(graph, entry, ValidateOptions { check_<your_invariant>: true })`.

   Make a check non-opt-in (always-on) only if you've audited every existing test in the workspace and confirmed no test trips it accidentally. The default is opt-in.

4. **Add a `ValidationError` variant** in `crates/ir/src/validate/mod.rs` (the `#[derive(thiserror::Error)] pub enum ValidationError { ... }` block, around line 148). Use a clear `#[error("...")]` message that names the offending `NodeId` and (where helpful) the offending kind / index / slot.

5. **Aggregate, do not fail-fast.** Push to `&mut errs: Vec<ValidationError>` and let the caller see the full set. The contract is "every layer runs to completion so the caller sees every problem at once." Do NOT `return Err(...)` early from a check.

6. **Walk reachable nodes via `walk::walk_graph(graph, entry)`.** The driver in `validate_with_options` already builds the `reachable: NodeIdSet`; pass it to the new check. Do not re-walk; reuse the existing set.

7. **Test with deliberately-malformed graphmock cases AND at least one passing fixture-derived case.** Pattern from existing tests:

   - Negative test: build a graph that violates the invariant, call `validate_with_options(...)` with the new flag set, assert the bundle contains the expected error variant.
   - Positive test: lift a real fixture (or construct a known-good graphmock), call `validate_with_options(...)` with the flag set, assert `Ok(())`.
   - Default-options test: build the same malformed graph, call `validate(graph, entry)` (no opt-in), assert the new error is NOT raised — confirms backwards compatibility.

8. **Document the invariant in the doc-comment on the check function.** Include: what the invariant says, why it holds (which lift / pass establishes it), what kind of bug would violate it, and whether reachability scoping was a deliberate choice.

9. **Run the full test suite to confirm no existing test trips the new check unexpectedly.** This is the round-8 backward-compatibility audit. Even with opt-in, downstream tests using `validate_with_options` may have been written assuming a specific superset of checks; new variants may surprise them.

## Verification

- `cargo test --package ir validate` — local validate tests, including the new positive / negative / default-options trio.
- `cargo test --workspace` — full suite. Confirms no existing test trips the new check unexpectedly.
- Run `validate_with_options(graph, entry, ValidateOptions { check_<new>: true })` on one of the canonical orchestrator-output fixtures (via a small helper test that lifts e.g. `fixtures/out/x86/arithmetic.elf::add` and runs the full pipeline before validating).
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- New check is opt-in via a `ValidateOptions` flag (or always-on if and only if no existing test trips it).
- Reachability-scoped (gates on `&reachable: &NodeIdSet`) unless the invariant explicitly needs to catch unreachable corruption (e.g. uniqueness of Entry / InitialMemory).
- A new `ValidationError` variant is wired through the `ValidationErrors` bundle with a clear `#[error(...)]` message.
- Aggregating semantics preserved (no fail-fast).
- Negative + positive + default-options test trio in place.
- The check covers at least one round-8-class regression (zombie resurrection, attribution-superset violation, etc.).
- `cargo clippy --workspace -- -D warnings` clean.
- Full workspace tests pass.

## Pitfalls

- **Skipping reachability scoping.** Round 8 caught this on `check_layer_c_function_arg_uniqueness`: pre-fix, the check scanned the entire arena and false-positived on graphs where `RedundantPhis` had detached an old `FunctionArg` zombie. Post-fix it gates on `reachable` (see `crates/ir/src/validate/layer_c.rs:228`). For any new per-node invariant, ask: "does an optimization pass detach but not delete this kind of node?" If yes, scope on `reachable`.
- **Making a check non-opt-in.** It will trip existing graphmock tests that didn't set up the invariant. Default to opt-in via `ValidateOptions`. Promote to always-on only after auditing every workspace test.
- **Failing fast.** Do not `return Err(...)` from a check. The `ValidationErrors` bundle aggregates every violation so the caller sees the full picture in one invocation. Returning early hides downstream problems.
- **Walking the graph twice.** The driver already builds `reachable: NodeIdSet` and passes it to every Layer-B and Layer-C check. Re-walking from each check wastes time and risks divergence (e.g. one walk skipping a kind the other includes).
- **Forgetting the doc-comment.** The check function is the only place the invariant is documented. Future maintainers walking past will not know whether the reachability gate is load-bearing or accidental. Spell out the choice.
- **Not adding a `#[error(...)]` message that names the offending NodeId.** "Layer-C check failed" is useless in CI logs. "FunctionArg index N: first NodeId={first:?} second NodeId={second:?}" is actionable.

## Related skills

- `strider-fingerprint-audit` — the existing opt-in `check_asm_fingerprints` is the canonical example of an opt-in Layer-C check.
- `strider-opt-pass-author` — when the new invariant is established by a new pass, the pass-author skill cross-references the validator.
- `strider-cli-runner` — for running `validate_with_options(...)` on a real fixture during development.
- `strider-fixture-author` — for the real-ELF positive test case that confirms a clean run with the new flag.
