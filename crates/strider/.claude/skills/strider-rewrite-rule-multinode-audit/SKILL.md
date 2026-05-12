---
name: strider-rewrite-rule-multinode-audit
description: Audit a `pattern::rewrite_rule` whose RHS produces multiple fresh nodes to ensure the engine's per-interior-node fingerprint propagation walk reached every freshly-built node, and document any direct `Graph::create_node_attributed` call you need for cases the engine doesn't cover.
---

# strider-rewrite-rule-multinode-audit

## When to use

Triggers:
- "this `pattern::rewrite_rule` builds a multi-node RHS and I want to verify the asm-fingerprint contract"
- "round 9 H-2 / EA1 Finding 1 — multi-node RHS attribution"
- a new `ConstantFold` / `KnownBits` / `FlagCmpCanonicalize` rule's RHS introduces fresh interior nodes (And/IntConst/Truncate/etc.)
- a fixture fails `validate_with_options(check_asm_fingerprints: true)` after running a custom rewrite rule

## When NOT to use

- The RHS is a single capture variable (`var(c)`) — single-node replacement, no interior nodes to attribute, the engine handles it.
- The pass uses hand-coded `Graph::create_node`/`create_node_attributed` directly (not through `pattern::rewrite_rule`) — the audit doesn't apply; use `strider-fingerprint-audit` instead.

## Background

Round 9 wave-2 fixed the multi-node attribution gap in
`pattern::rewrite_rule` (`crates/pattern/src/rewrite.rs`).  After
`BuildOutcome::Out(new_out)`, the engine:

1. Snapshots `pre_build_node_id = ctx.graph.next_node_id()` BEFORE the build closure runs.
2. Calls `extend_asm_fingerprint_from(new_node, contributor)` on the rewrite root (handles cache-hit and fresh-node alike).
3. Walks backward from `new_node` via `node_inputs`, propagating the contributor into every visited node whose id ≥ snapshot (i.e. freshly allocated during the build).  Pre-existing nodes (id < snapshot) bound the walk and stay untouched.

This means **most multi-node rules need no special handling** — the engine attributes every interior fresh node automatically, including freshly-built `IntConst` masks that the dedup cache didn't unify with pre-existing constants.

## Cases the engine does NOT cover

The engine attributes nodes *reachable from `new_node` via `node_inputs`*.  If your RHS produces a node that is NOT in the data-flow chain from the root, it stays unattributed.  Examples:

- A side-effecting node (e.g. a `Store` synthesised purely for its memory edge, not consumed by the root's data inputs).  Use `Graph::create_node_attributed(kind, inputs, outputs, &[contributor])` directly.
- A node that the dedup cache returns from an existing arena slot — the engine handles this for the rewrite root, but not for *interior* cache hits.  In practice this is rare; if you suspect it, add a targeted regression test.

## Procedure

1. Read your rule's RHS template.  Count the nodes the closure will create:
   - Each `add(...)`, `mul(...)`, `and(...)`, `or(...)`, `xor(...)`, `not(...)`, `int_const(...)`, etc. ctor produces a fresh node when invoked at build time.
   - Each `var(c)` is NOT a fresh node — it returns the captured node-output unchanged.
2. Confirm every fresh node is reachable from the rewrite root via the data-flow inputs.  Walk the RHS tree on paper from the root and verify each fresh node appears as an input or transitive input.
3. For any fresh node that is NOT reachable from the root via `node_inputs` (rare): add an explicit `extend_asm_fingerprint_from(new_node, contributor)` call after the build, or use `Graph::create_node_attributed`.
4. Add a regression test in your pass's `tests.rs`:
   - Build a graph that triggers the rule.  Set `lift_addr` on every input via the per-region driver pattern.
   - Run the pass.
   - Call `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` and assert `Ok(())`.
   - Optional: enumerate every reachable non-exempt node and assert each fingerprint contains at least one expected contributor address.
5. Verify the regression with `cargo test`.

## Reference: the round-9 H-2 test

`crates/opt/tests/asm_fingerprint_propagation.rs::constant_fold_rule_and_dist_attributes_inner_nodes` is the canonical reference.  It builds `((a&C1)|(b&C2))&C3` with non-cached masks (`C1=0xFFFF`, `C2=0xFFFF_0000`, `C3=0x00FF_FF00`), runs `ConstantFold`, and asserts:
- `validate_with_options(check_asm_fingerprints: true)` returns `Ok(())`.
- Every reachable `And` node has a non-empty fingerprint.

Pre-fix the test failed with `MissingAsmFingerprint` for two `IntBinaryOp::And` and two `IntConst` interior nodes.

## Anti-patterns

- Adding a single `extend_asm_fingerprint_from(root, contributor)` after a multi-node RHS build and assuming it's enough.  Pre-round-9 this was the silent failure mode.  The engine now walks; manual single calls are insufficient.
- Disabling `check_asm_fingerprints` on a fixture rather than fixing the gap.  Defeats the contract round 9 worked to make routinely verifiable.
- Constructing a fresh node outside `pattern::rewrite_rule` (e.g. via direct `graph.create_node`) and then routing it through a captured `var(c)`.  The engine's snapshot was taken before your direct creation; if the node id ends up in the walk, it'll be attributed; if not, it won't.  Don't mix the two construction styles.

## See also

- `crates/pattern/src/rewrite.rs` — the engine implementation.
- `crates/ir/src/graph/access.rs::next_node_id` — the snapshot API the engine uses.
- `strider-fingerprint-audit` skill — broader asm-fingerprint contract verification.
- `strider-opt-pass-author` skill — top-level pass-authoring procedure (the multi-node trap is documented there as a forward-reference).
