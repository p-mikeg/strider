# IR Graph Validator

**Status:** design
**Date:** 2026-04-16

## Goal

Detect bugs in IR graph construction and in optimization passes by running a
structural validator on the graph immediately after `FunctionBuilder::build()`
and again after `OptimizerPipeline::run()`. A failing validation returns an
error that the caller propagates; nothing panics.

The validator is a pure library function — it takes a `&Graph` and an entry
`NodeId`, returns every invariant violation it finds, and is trivial to unit
test with hand-built broken graphs.

## Non-goals

- Reachability / orphan detection (dead nodes are expected before a GC pass).
- SSA dominance (every data use dominated by its def) — valuable but deferred.
- Performance. This is a correctness tool. Expect O(nodes + edges + use-list
  length) work per call.

## Public API

New module `crates/ir/src/validate.rs`, re-exported from the `ir` crate.

```rust
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors>;

pub struct ValidationErrors(pub Vec<ValidationError>);

impl std::fmt::Display for ValidationErrors { /* one error per line */ }
impl std::error::Error for ValidationErrors {}

pub enum ValidationError {
    // --- Layer A: local node typing ---
    NodeInputCountMismatch   { node: NodeId, expected: usize, actual: usize },
    NodeInputKindMismatch    { node: NodeId, input_idx: usize, expected: NodeOutputKind, actual: NodeOutputKind },
    NodeOutputCountMismatch  { node: NodeId, expected: usize, actual: usize },
    NodeOutputKindMismatch   { node: NodeId, output_idx: usize, expected: NodeOutputKind, actual: NodeOutputKind },

    // --- Layer B: use-list consistency ---
    InputPointsToMissingOutput { node: NodeId, input: NodeInputId, output: NodeOutputId },
    InputMissingFromUseList    { node: NodeId, input: NodeInputId, output: NodeOutputId },
    UseListContainsStaleInput  { output: NodeOutputId, input: NodeInputId },

    // --- Layer C: control-flow structural ---
    ControlStateNonControlPredecessor { control_state: NodeId, input_idx: usize, producer: NodeId, producer_kind: NodeOutputKind },
    PhiValueArityMismatch       { phi: NodeId, expected_predecessors: usize, actual_values: usize },
    PhiTokenNotFromControlState { phi: NodeId, producer: NodeId },
    MultipleEntryNodes         { first: NodeId, second: NodeId },
    MultipleInitialMemoryNodes { first: NodeId, second: NodeId },
    MissingEntryNode,
    MissingInitialMemoryNode,

    // --- Layer C: call-related structural ---
    PostCallMemStateNotAfterCall { node: NodeId, producer: NodeId, producer_kind: NodeOutputKind },
    PostCallVarStateNotAfterCall { node: NodeId, producer: NodeId, producer_kind: NodeOutputKind },
    DuplicatePostCallMemState    { call: NodeId, first: NodeId, second: NodeId },
    DuplicatePostCallVarState    { call: NodeId, vn: rsleigh::Vn, first: NodeId, second: NodeId },
}
```

`validate` collects every violation it finds and returns them all, rather than
stopping at the first. This makes debugging a pass that corrupts many nodes
much easier.

## Checks, by layer

### Layer A — local node typing

For every node in the graph:

- The actual number of inputs matches the expected count for its `NodeKind`.
- Each input's `NodeOutputKind` matches the expected kind at that position.
- The actual number of outputs matches.
- Each output's `NodeOutputKind` matches the expected kind.

Implementation: extract the per-`NodeKind` expected input/output kinds from
`node_view.rs` into a shared helper (e.g. `expected_signature(&NodeKind) ->
(Vec<NodeOutputKind>, Vec<NodeOutputKind>)`). The existing `verify_*` helpers
and `node_view()` become thin wrappers around it. The validator iterates every
node and diffs against the expected signature.

### Layer B — use-list consistency

The `Graph` maintains a doubly-linked use list per `NodeOutputId`. Two passes:

1. **Forward.** For each node `n` and each of its inputs `i` pointing at
   `NodeOutputId o`:
   - `o` must exist and belong to a live node.
   - `o`'s consumer list must contain `i`.
2. **Backward.** For each live `NodeOutputId o`, walk its consumer list; each
   listed `NodeInputId i` must still be a live input of some node and must
   point back at `o`.

Emits `InputPointsToMissingOutput`, `InputMissingFromUseList`, and
`UseListContainsStaleInput` respectively.

### Layer C — control-flow structural

Whole-graph shape:

- Exactly one `Entry` node and exactly one `InitialMemory` node.

Per-node-kind checks:

- **`ControlState`:** every input's producer kind must be `Control`.
- **`ControlPhi(_)` and `MemPhi`:** input[0] must be the `ControlPhi`-kind
  output of some `ControlState`; call that the phi's "owner." Record the
  ownership map during the walk.
- **Phi arity:** for each phi, `inputs.len() - 1` (the value inputs) must equal
  the number of inputs on the owning `ControlState`. This catches the bug
  where a pass drops a predecessor edge from a `ControlState` but forgets to
  drop the matching value from every joining phi.

Call-related checks:

- **`PostCallMemState`:** its single input must come from a `Call` node's
  **control** output. Not from `Entry`, `If`, `ControlState`, or any other
  control producer.
- **`PostCallVarState(Vn)`:** same — input must come from a `Call`'s control
  output.
- **Uniqueness:** at most one `PostCallMemState` may consume a given Call's
  control output; at most one `PostCallVarState(Vn)` per distinct `Vn` may
  consume it. Implementation: during the walk, group `PostCall*` consumers by
  their producing Call; emit a `Duplicate*` error if a group has two or more
  entries for the same role/Vn.

Not checked: "`PostCallVarState(Vn)` is for an actually-clobbered `Vn`." The
clobbered set lives on `FunctionBuilder`, not on the `Call` node, and threading
it through `validate` would widen the API. This is a known gap; revisit if a
real bug slips through it.

Call input/output shape (input[0] Control, input[1] Memory, input[2] Int
address, inputs[3..] values; output[0] Control, output[1] Memory, outputs[2..]
value kinds) is covered by Layer A via the `NodeKind::Call` signature.

## Integration

`validate()` is a pure function returning `Result`. It is wired into the two
points where the graph is finalized, and the error propagates up the normal
`Result` chain. **No panics, no `unwrap`s, no `cfg(debug_assertions)` gating.**

### 1. `FunctionBuilder::build()`

Return type changes from `BuiltFunctionGraph` to
`Result<BuiltFunctionGraph, BuildError>`.

`BuildError` is a new error enum in the `ir` crate with at least a
`Validation(ValidationErrors)` variant (and room for any existing / future
build-time failure modes). `From<ValidationErrors> for BuildError` is provided
so `validate(...)?` at the end of `build()` threads the error out cleanly.

All callers of `build()` are updated to propagate with `?`.

### 2. `OptimizerPipeline::run()`

Return type changes from `()` to `Result<(), OptError>`.

`OptError` is a new error enum in the `opt` crate; the initial variant is
`Validation(ir::ValidationErrors)`. Validation runs once after the fixed-point
loop and post-passes complete. (Checking between individual opt passes to
bisect which pass corrupted the graph is easy to add later; not part of this
spec.)

All callers of `run()` are updated to propagate with `?`.

### 3. `Analyzer::analyze_cfg()` / analyzer plumbing

`Analyzer::analyze_cfg()` (or the closest existing Result-returning wrapper)
gets its error type extended with `From<BuildError>` and `From<OptError>`, so
`?` propagates validation failures through the analyzer to the top-level
caller.

`examples/analyzer.rs` is updated (if not already) so `main` returns
`Result<(), Box<dyn Error>>`. A validation failure prints a clean error and
exits non-zero.

### 4. `cfg(debug_assertions)` gating

**Not used.** Validation runs unconditionally. If profiling shows this is too
expensive in release builds, gate it behind a `validate-ir` Cargo feature
flag that release-critical consumers can disable.

## Testing

Unit tests in `validate.rs` that hand-construct broken graphs and assert the
expected `ValidationError` variant fires:

- Wrong input kind on an arithmetic op (Layer A).
- Mismatched input count (Layer A).
- An input pointing at a non-existent `NodeOutputId` (Layer B).
- A consumer list that still contains a detached `NodeInputId` (Layer B).
- A `ControlState` with a non-`Control` predecessor (Layer C).
- A `ControlPhi` whose value count disagrees with its `ControlState`'s
  predecessor count (Layer C).
- A `PostCallMemState` whose input is the `Entry` node's control (Layer C).
- Two `PostCallMemState` nodes on the same Call (Layer C).
- Two `PostCallVarState(Vn)` for the same `Vn` on the same Call (Layer C).

One integration smoke test that runs the analyzer on
`binary_tests/binary_test` end-to-end and asserts validation returns `Ok(())`
at both call sites.

## Files touched

- **New:** `crates/ir/src/validate.rs`
- **Modified:** `crates/ir/src/lib.rs` (export), `crates/ir/src/node_view.rs`
  (extract shared `expected_signature` helper), `crates/ir/src/builder.rs`
  (`build()` returns `Result`, new `BuildError`), `crates/opt/src/opt.rs`
  (`OptimizerPipeline::run()` returns `Result`, new `OptError`),
  `crates/analyzer/src/analyzer.rs` and its error type, `examples/analyzer.rs`
  (propagate).
