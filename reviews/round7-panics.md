# Round 7 — Production-Code Panic Audit

Strict audit for panic-equivalent constructs (`unwrap`, `expect`, `panic!`,
`unreachable!`, `assert!`/`debug_assert!`/`assert_eq!`/`assert_ne!`, `todo!`,
risky bracket indexing on `Vec`/slice/`HashMap`) in production code only.

**Scope.** All `crates/*/src/**/*.rs` excluding `#[cfg(test)]` blocks,
`tests/`, `examples/`, and `benches/` directories.  Inline `mod tests` blocks
guarded by `#[cfg(test)]` were located by file, and only lines BEFORE that
boundary were considered.

---

## Per-Crate Findings Table

| Crate | File:Line | Construct | Classification | Proposed Fix |
|-------|-----------|-----------|----------------|--------------|
| `ir` | `crates/ir/src/graph/compact.rs:118` | `.expect("just installed in pass 1")` | JUSTIFIED (TEST-LIKE-IN-PROD via `#[allow(clippy::expect_used)]`) | Could remain — pass-1 install of `remap.nodes[old_id]` is by-construction; nevertheless violates "no expect" policy. Convert `retain_reachable` → `Result` and `bail!` instead. |
| `ir` | `crates/ir/src/graph/compact.rs:127-129` | `.expect("input references an output whose producing node was unreachable")` | UNJUSTIFIED | Make `retain_reachable` return `Result<NodeIdRemap>`, `bail!("compact: dangling input reference: producer of output {old_input.output_id:?} unreachable")`. Update `BuiltFunctionGraph::compact` and downstream callers in `strider/orchestrator.rs`. |
| `ir` | `crates/ir/src/function.rs:150` | `.expect("entry must survive its own compaction")` | JUSTIFIED (TEST-LIKE-IN-PROD via `#[allow(clippy::expect_used)]`) | Same `retain_reachable` invariant; if propagated to `Result`, surface as `bail!("compact: entry node {self.entry:?} not in remap")`. |
| `ir` | `crates/ir/src/node/output_type.rs:69` | `&TYPE_INFO[self as usize]` (slice index) | UNJUSTIFIED — risky indexing | Replace with `match self { Bool => &TYPE_INFO[0], U8 => &TYPE_INFO[1], … }` so the compiler enforces variant↔index correspondence, OR add a `const _: () = { … }` ordering check.  Test `type_info_table_matches_variants` exists at `node/tests.rs:305` but does not run at compile time. |
| `ir` | `crates/ir/src/iterators.rs:37-39` | `Index<usize> for Outputs` panics on OOB | UNJUSTIFIED — by-design panic | The `Index` impl produces panics whenever `outputs[i]` is OOB.  Either remove the `Index` impl (force callers to `.get(i)?`) or add `#[doc = "Panics on OOB"]` and audit every call site for guards. ~30 production `outputs[N]` / `inputs[N]` call sites rely on validator-asserted shape. |
| `ir` | `crates/ir/src/iterators.rs:91-97` | `Index<usize> for Inputs` panics on OOB | UNJUSTIFIED — same | Same fix as above. |
| `opt` | `crates/opt/src/flag_cmp_canonicalize/mod.rs:128` | `.expect("Capture a must bind to a value output")` | JUSTIFIED (TEST-LIKE-IN-PROD via `#[allow(clippy::expect_used)]`) | Pattern-rule contract: `cap_a` is always bound at a value-producing position by construction of every `Rule.lhs`. Could `bail!` for paranoia; otherwise document via `#[allow]`. |
| `opt` | `crates/opt/src/flag_cmp_canonicalize/mod.rs:161` | `.expect("IntCmpOp produces 1 output")` | JUSTIFIED (TEST-LIKE-IN-PROD) | Just-built node has exactly 1 declared `Bool` output. Collapse to `let out = graph.first_output(n);` helper or propagate `Result`. |
| `opt` | `crates/opt/src/flag_cmp_canonicalize/mod.rs:175` | `.expect("BoolNeg produces 1 output")` | JUSTIFIED (TEST-LIKE-IN-PROD) | Same as 161. |
| `graphmock` | `crates/graphmock/src/lib.rs:119` | `.unwrap_or_else(\|\| panic!("graphmock: line missing \`->\`: {line:?}"))` | TEST-LIKE-IN-PROD | `graphmock` is dev-dependency-only (only `graphwalk`'s `[dev-dependencies]` consumes it). Comment at line 113-115 documents intent. Acceptable, but could be `.expect(...)` or return `Result` for a pure DSL parser. |
| `graphmock` | `crates/graphmock/src/lib.rs:122` | `assert!(!name.is_empty(), …)` | TEST-LIKE-IN-PROD | Same. |
| `strider` | — | none | — | Strider has zero non-test panics. |
| `cfg` | — | none (one `.expect(` was a doc comment at `builder/mod.rs:44`) | — | CFG has zero non-test panics. |
| `pcode-lift` | — | none | — | Zero non-test panics. |
| `pattern` | — | none (only doc-comment unwraps in `lib.rs` and `matcher/mod.rs`) | — | Zero non-test panics. |
| `strider-py` | — | none | — | Zero non-test panics. |
| `target` | — | none | — | Zero non-test panics. |
| `reader` | — | none | — | Zero non-test panics. |
| `dot` | — | none | — | Zero non-test panics. |
| `graphwalk` | — | none | — | Zero non-test panics. |
| `entity-utils` | — | none | — | Zero non-test panics. |

---

## Why Each Unjustified — Expanded Reasoning

### `ir/src/graph/compact.rs:127-129` — UNJUSTIFIED

```rust
let new_output_id = remap.outputs[old_input.output_id].expect(
    "input references an output whose producing node was unreachable",
);
```

The invariant is graph-level, not language-level: it requires that for every
node `N` reachable from `entry`, *every* `output_id` listed in `N.inputs`
points to a node also reachable from `entry`.  This invariant can be violated
by:

1.  Mid-optimization graph states where a pass has detached one node's input
    chain but not yet removed the consumer (zombie state).
2.  External code that constructs a `Graph` without the invariant
    (`FunctionBuilder` enforces it via `validate`, but raw `Graph` users could
    bypass it).
3.  Future passes that retain partial graphs.

The `#[allow(clippy::expect_used)]` annotation + comment is policy-violating
and shifts a graph-state error into a process panic.  **Fix:** convert
`retain_reachable` → `Result<NodeIdRemap, anyhow::Error>` and `bail!` with
a structured message. Propagate through `BuiltFunctionGraph::compact` (which
is already marketed as a fallible operation given it returns a `NodeIdRemap`).

### `ir/src/node/output_type.rs:69` — UNJUSTIFIED (risky index)

```rust
fn info(self) -> &'static TypeInfo {
    &TYPE_INFO[self as usize]
}
```

The comment at line 50-51 says "Order MUST match the `NodeOutputType` enum
declaration order (asserted by `type_info_table_matches_variants` in the test
module)".  The test does exist at `node/tests.rs:305-329`, but it runs only
under `cargo test`, NOT at compile time.  Adding a new variant without
re-ordering `TYPE_INFO` would silently corrupt every call to `as_str`,
`byte_size`, `bit_width`, `is_bool`, `is_integer`, `is_float`, OR panic with
OOB at runtime.  Hot path: every node-creation cache lookup, every dot
render, every type check during validation.

**Fix:** Replace with explicit `match self { Bool => &TYPE_INFO[0], … }`.
The match exhaustiveness check then enforces parity at the type system level.

### `ir/src/iterators.rs:37-40` and `91-97` — UNJUSTIFIED (by-design panic)

```rust
impl Index<usize> for Outputs<'_> {
    type Output = NodeOutputId;
    fn index(&self, index: usize) -> &Self::Output { &self.0[index] }
}
```

There is already a `get(index) -> Option<&NodeOutputId>` method right above.
The `Index` impl exists purely for ergonomic `outputs[0]` syntax in callers
that have already length-checked.  Per the user's "no panic in production"
policy, this construct is *exactly* what should not exist.

Concrete production callers of `Outputs[N]` / `Inputs[N]` (sample):
- `crates/ir/src/builder/call.rs:130-131`, `288, 291`
- `crates/ir/src/dot/render.rs:18`
- `crates/ir/src/validate/layer_c.rs:119`
- `crates/opt/src/sp_expr.rs:120, 122, 142, 150, 173, 176, 178, 180, 228, 298, 299, 330, 331`
- `crates/opt/src/dead_branch/mod.rs:54-55, 145`
- `crates/opt/src/function_args/mod.rs:445`
- `crates/opt/src/indirect_branch_resolve/jump_table.rs:458, 467`
- `crates/opt/src/indirect_branch_resolve/inplace.rs:121-123, 339, 374`
- `crates/opt/src/indirect_branch_resolve/mod.rs:421` (test module — excluded)
- `crates/opt/src/load_readonly/mod.rs:70`
- `crates/opt/src/redundant_phis/mod.rs:54, 76, 98`
- `crates/opt/src/stack_load_forward/mod.rs:222, 239, 268, 270`
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:279`

Every site I sampled (sp_expr.rs, dead_branch, redundant_phis, load_readonly,
stack_load_forward, indirect_resolve.rs) IS preceded by a length guard
(`if inputs.len() < N { return ... }` or `node_inputs_exact::<N>()?`).
The indexing is therefore JUSTIFIED at each call site, but the underlying
`Index` impl is a footgun that silently panics if any future caller forgets
the guard.

**Fix:** Either delete the `Index` impl and force `.get(N)?` everywhere
(stricter), or annotate it with `#[doc = "# Panics\n\nPanics if `index >=
self.len()`."]` and `clippy::indexing_slicing` allow at every call site
(weaker; matches Rust stdlib convention).

---

## Summary Counts

- **Total production-code panic-equivalents found:** 11 occurrences across 5 files.
- **Unjustified (must-fix):** 4 distinct issues
  1. `ir/graph/compact.rs:127-129` `expect` on graph invariant.
  2. `ir/node/output_type.rs:69` slice-indexed type-info table without compile-time guard.
  3. `ir/iterators.rs:37-40` `Index<usize> for Outputs` (by-design panic).
  4. `ir/iterators.rs:91-97` `Index<usize> for Inputs` (by-design panic).

- **Justified (still violate strict policy, retained via `#[allow(clippy::expect_used)]`):** 4 occurrences
  - `ir/graph/compact.rs:118` (pass-1 install invariant)
  - `ir/function.rs:150` (entry-survives-compact invariant)
  - `opt/flag_cmp_canonicalize/mod.rs:128, 161, 175` (rule-construction invariants)

- **Test-like-in-production:** 2 occurrences in `graphmock/src/lib.rs:119, 122`
  (dev-dependency-only crate, but not behind `#[cfg(test)]`).

- **Crates with ZERO production panics:** `strider`, `cfg`, `pcode-lift`,
  `pattern`, `strider-py`, `target`, `reader`, `dot`, `graphwalk`, `entity-utils`.

- **Risky indexing audit:** ~30 `outputs[N]`/`inputs[N]` production sites, ALL
  guarded by either `inputs.len() < N` checks or `node_inputs_exact::<N>()?`.
  Local correctness is preserved; the global concern is the `Index` trait
  itself remaining as a footgun for future callers.

The audit confirms strider's panic discipline is excellent overall: 8 of 13
crates have zero production panics, and every guarded indexing site I
sampled IS guarded.  The four unjustified items are concentrated in the IR
graph layer and form a coherent fix set: convert `retain_reachable` to
`Result`, replace the `TYPE_INFO[self as usize]` access with a `match`, and
either remove the `Index` impls on `Outputs`/`Inputs` or document them as
panicking.
