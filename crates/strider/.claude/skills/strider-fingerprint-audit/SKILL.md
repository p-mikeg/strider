---
name: strider-fingerprint-audit
description: Verify asm-fingerprint propagation through a strider opt pass or rewrite using validate_with_options Layer C and per-node extend_asm_fingerprint_from checks.
---

# strider-fingerprint-audit

## When to use

User wants to verify that asm-fingerprint propagation is correct after a new pass or rewrite. Triggers include "verify asm-fingerprint propagation through this new pass", "did my rewrite preserve the fingerprint contract?", "run validate with `check_asm_fingerprints` on this fixture", "an `asm_fingerprint(c, &graph)` call returned an empty slice for a non-exempt node".

## When NOT to use

- The user is authoring a brand-new pass — that flow is `strider-opt-pass-author`, which already invokes this skill at the end.
- The fingerprint check is failing because the lifter never set one — escalate to lift-side investigation (`crates/strider/src/strider/insn/`), not pass-side.

## Inputs the skill expects

- A failing test or fixture binary path.
- The pass(es) under audit.

## Procedure

1. Read the contract. Public API on `Graph` (`crates/ir/src/graph/store.rs:132-184`): `asm_fingerprint(id) -> &[u64]`, `set_asm_fingerprint(id, Vec<u64>)`, `extend_asm_fingerprint(id, &[u64])`, `extend_asm_fingerprint_from(dst, src)`. Superset-only contract; structurally identical (cacheable) nodes share a single side-table entry that is the union of every contributor.
2. Enable opt-in validation in your test. Replace `validate(graph, entry)` with `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` (`crates/ir/src/validate/mod.rs:83`). Layer C will flag every reachable non-exempt node with an empty fingerprint. Exempt kinds are listed in `crates/ir/src/validate/layer_c.rs::asm_fingerprint_exempt` (`Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`).
3. For each newly-created node in the pass, confirm there is a matching `extend_asm_fingerprint_from(new, contributor)` call. The contributor is the node whose semantics the new node preserves — typically the matched root, or the multi-input nodes whose values are unioned.
4. Spot-check from a pattern match. In a unit test, capture the rewritten root, then assert that `m.asm_fingerprint(c, &graph)` is a superset of every machine address in the input shape.
5. For lift-time changes: the per-region driver wraps each `process_insn` call (and each special-terminator handler) in `set_lift_addr(Some(addr)) ... set_lift_addr(None)`, so every node `create_node` produces during the insn picks up `addr` automatically. If you bypass this funnel (e.g. constructing nodes outside the per-insn block), call `extend_asm_fingerprint` explicitly.
6. Cache-hit case: when `Graph::create_node` deduplicates, the side-table entry must be the union of every contributor. The `asm_fingerprint_dedup_cache_hit_unions_via_extend` test pins this contract. Audit any pass that creates nodes inside a hot loop to ensure no contributor is dropped on cache hits.

## Verification

- `cargo test --package ir asm_fingerprint` — round-trips the side-table contract.
- `cargo test --package opt -- --include-ignored` — ignored slot for the heavy fingerprint-validation tests.
- For a real binary: enable `check_asm_fingerprints: true` in a fixture-driven test and run `cargo test --package strider <fixture_test>`.

## Exit criteria

- `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` passes on the optimised graph for the audit fixture.
- A test asserts `m.asm_fingerprint(c, &graph)` is non-empty and contains the expected machine addresses.
- No regression in `cargo test --package ir` or `cargo test --package opt`.

## Pitfalls

- Layer C is opt-in. Default `validate` does not check fingerprints, so legacy mock-graph tests stay green even with empty fingerprints. The audit must explicitly use `validate_with_options`.
- Calling `set_asm_fingerprint(id, ...)` (overwrite) inside a pass can shrink the set. Use `extend_asm_fingerprint` / `extend_asm_fingerprint_from` exclusively from passes; reserve `set_asm_fingerprint` for the lifter where it owns the initial population.
- "Empty fingerprint is fine because of dedup" is wrong. Two structurally-identical nodes share one entry that must be the union; the union must be non-empty.
- A test that exercises `validate` but not `validate_with_options` will not detect the regression. Always switch the audit test to the opt-in form.

## Related skills

- `strider-opt-pass-author` — invokes this skill as the mandatory final step.
- `strider-indirect-shape-author` — same; the new resolver shape must pass fingerprint audit.
- `strider-pattern-author` — patterns can be constructed to assert fingerprint coverage.
