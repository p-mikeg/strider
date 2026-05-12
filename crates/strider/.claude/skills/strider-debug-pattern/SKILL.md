---
name: strider-debug-pattern
description: Diagnose why a strider pattern returns zero matches by inspecting the IR layer, lift-time canonicalisation, capture sharing, and commutativity assumptions.
---

# strider-debug-pattern

## When to use

User has an existing pattern that fails to match where they expect it to. Triggers include "my pattern returns zero matches", "`find_all` returns empty for what should be a `Call(at=...)`", "pattern works on one fixture but not another", "I expect this `If(x & 1 == 0)` to match but it doesn't".

## When NOT to use

- The pattern has not yet been authored — go via `strider-pattern-author`.
- The error is a `PatternError` panic / raise on construction — most are syntactic; if it is a Python wrapping issue, route to `strider-py-binding`.
- The user is debugging an opt pass that fails to rewrite — that is `strider-opt-pass-author`'s territory (or the systematic-debugging skill).

## Inputs the skill expects

- The failing pattern source (Rust or Python).
- The fixture binary or a `BuiltFunctionGraph` HTML dump (`graph-opt.html`).
- Optionally, an `asm_fingerprint` value identifying a node the user expected to match.

## Procedure

1. Dump the IR. Add a one-liner to the failing test: Rust uses `dot::write_html(&graph, "/tmp/dump.html")?;` (see `crates/dot/src/lib.rs`); Python uses `graph.to_html("/tmp/dump.html")`. Open the HTML and confirm the IR shape your pattern targets.
2. Identify which IR layer the pattern is querying. Pre-opt (raw lift) means none of `ConstantFold`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`, `FlagCmpCanonicalize` have run. Stable-default (`opt::stable_default_pipeline`) runs only the indirect-fixedpoint-stable subset (`ConstantFold` + `KnownBits` + `FlagCmpCanonicalize` + `IfCondInversion`). Default (post-orchestrator, via `strider::run`) is the full pipeline including destructive passes — this is the typical query target.
3. Re-check lift-time canonicalisation. Looking for `Sub`? IR has only `Add(_, Neg(_))`. Looking for `If(BoolNeg(C))`? `IfCondInversion` removed it; rewrite the pattern as `If(C)` with branches swapped. Looking for `LessEqual` / `NotEqual`? Use the `int_le` / `float_ne` aliases (compositions, not primitives). The full canonicalisation table is in `crates/strider/CLAUDE.md`.
4. Check `StackStoreDetect` classification. A bare `Store(SP+K, ...)` does NOT match `StackStorePat` until the pass runs. Either run the full default pipeline (or at minimum `StackStoreDetect` configured with the calling convention's stack-pointer Vn) before querying, or fall back to `StorePat` against the unclassified IR.
5. Use the asm-fingerprint to cross-reference. After a partial match, capture the rewritten root and call `m.asm_fingerprint(c, &graph)` (see `crates/ir/src/graph/store.rs` for the API). The returned slice should be a superset of every machine address contributing to the shape. An empty fingerprint on a non-exempt node means the lifter or a pass dropped the contract — open `strider-fingerprint-audit`.
6. Check capture sharing. With `find_all_requirements`, a shared `Capture` between patterns must bind to identical `(NodeId, NodeOutputId)`. If you see "matches individually but no joined match", that is a binding mismatch — most often because one pattern binds the value slot of a multi-output node and the other binds the control or memory slot.
7. Check commutativity. Typed builders default to commutative for the listed ops. Free `add` / `mul` / `and` / `or` / `xor` ctors are also commutative. To force ordering, switch to the typed `int_binary("Add", a, b).ordered()` dispatcher. `PyPat::ordered()` on a free-ctor result is a no-op.

## Verification

- Rerun the failing test with output captured: `cargo test --package <consumer> <test_name> -- --nocapture`.
- Or for Python: `uv run pytest crates/strider-py/tests/python/test_<file>.py::<test> -v`.
- After the fix, add a regression test pinning the shape so it cannot silently regress.

## Exit criteria

- The originally-failing pattern matches the expected node(s).
- A new regression test is in place.
- HTML dump and `asm_fingerprint` cross-checks confirm the pattern is querying the IR layer the consumer actually receives.

## Pitfalls

- "Just disable the optimiser" is usually wrong. Production consumers run on optimised IR; the pattern needs to target the post-optimisation shape.
- Skipping the HTML dump and reasoning about the graph in your head is slow and error-prone — rebuild the dump after every edit.
- Forgetting to apply the same arch / CC settings the production code uses. `StackStorePat` is configured by the CC's stack-pointer Vn; a mismatch silently fails.
- Commutativity surprises: confirming a pattern in mock graphs that only ever produce one operand order will hide commutative-vs-ordered bugs that surface against real lifts.

## Related skills

- `strider-pattern-author` — when the diagnosis is "rewrite the pattern from scratch".
- `strider-fingerprint-audit` — when an empty asm-fingerprint indicates a pass dropped the contract.
- `strider-opt-pass-author` — when the diagnosis is "an upstream pass produced an unexpected shape".
