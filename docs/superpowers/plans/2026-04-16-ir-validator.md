# IR Graph Validator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a structural validator that runs on the IR graph after `FunctionBuilder::build()` and after `OptimizerPipeline::run()`, detecting broken node signatures, corrupted use-lists, and malformed control flow. All failures propagate as `Result` errors — no panics.

**Architecture:** New module `crates/ir/src/validate.rs` exposing `validate(&Graph, NodeId) -> Result<(), ValidationErrors>`. Three check layers: (A) local node typing, (B) use-list consistency, (C) control-flow + Call structural invariants. Integrated by changing `FunctionBuilder::build()` to return `ir::Result<BuiltFunctionGraph>` and adding `validate?` to both `build()` and `OptimizerPipeline::run()`. Errors collected, not fail-fast.

**Tech Stack:** Rust, `cranelift-entity` PrimaryMaps, `thiserror`. Tests live inline in `#[cfg(test)] mod tests`.

**Spec:** [2026-04-16-ir-validator-design.md](../specs/2026-04-16-ir-validator-design.md)

---

## File Structure

- **Create:** `crates/ir/src/validate.rs` — validator, error types, unit tests.
- **Modify:** `crates/ir/src/node_view.rs` — extract `expected_signature()` helper; existing `node_view()` calls it.
- **Modify:** `crates/ir/src/error.rs` — add `ValidationFailed(ValidationErrors)` variant to `ir::Error`.
- **Modify:** `crates/ir/src/lib.rs` — add `pub mod validate;` and re-export `ValidationError`, `ValidationErrors`.
- **Modify:** `crates/ir/src/builder.rs` — `FunctionBuilder::build()` returns `ir::Result<BuiltFunctionGraph>`; calls `validate` before returning.
- **Modify:** `crates/opt/src/opt.rs` — `OptimizerPipeline::run()` calls `validate` after fixed-point.
- **Modify:** `crates/analyzer/src/analyzer.rs` — propagate the new `Result` from `build()` (the existing `analyze_cfg` already returns Result).
- **Modify:** `crates/analyzer/examples/analyzer.rs` and `crates/analyzer/tests/analyze_binary.rs` — call-site propagation if needed.

---

## Task 1: Extract `expected_signature` helper from `node_view.rs`

**Why:** Both `node_view()` and `validate()` need "what are this NodeKind's expected input/output kinds?" Currently this info is scattered across 40+ match arms with inline `verify_*` calls. Centralize it in one function that returns the expected signature as data.

**Files:**
- Modify: `crates/ir/src/node_view.rs`
- Test: `crates/ir/src/node_view.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Read `node_view.rs` end-to-end to understand all NodeKind signatures.**

Read the file and list every `NodeKind` variant and its expected inputs/outputs. You'll need this list for Step 3.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `node_view.rs` (create the block if it doesn't exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

    #[test]
    fn expected_signature_int_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::IntConst(42));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![NodeOutputKind::OutputType(NodeOutputType::U64)]);
    }

    #[test]
    fn expected_signature_entry() {
        let (inputs, outputs) = expected_signature(&NodeKind::Entry);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![NodeOutputKind::Control]);
    }

    #[test]
    fn expected_signature_if() {
        let (inputs, outputs) = expected_signature(&NodeKind::If);
        assert_eq!(inputs, vec![
            NodeOutputKind::Control,
            NodeOutputKind::OutputType(NodeOutputType::Bool),
        ]);
        assert_eq!(outputs, vec![NodeOutputKind::Control, NodeOutputKind::Control]);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --package ir --lib node_view::tests::expected_signature -- --nocapture`
Expected: compile error, `expected_signature` not found.

- [ ] **Step 4: Implement `expected_signature`**

Add this signature near the top of `node_view.rs` (after imports):

```rust
use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

/// Expected (input kinds, output kinds) for a given `NodeKind`.
///
/// This is the single source of truth used by both `node_view()` (which
/// reconstructs a high-level view) and `validate::validate()` (which
/// checks the graph). Variable-arity nodes (ControlState, MemPhi, ControlPhi,
/// Call, Return) return the **minimum fixed prefix** of inputs; callers
/// that need to validate the variadic tail must do it themselves.
pub(crate) fn expected_signature(kind: &NodeKind)
    -> (Vec<NodeOutputKind>, Vec<NodeOutputKind>)
{
    use NodeOutputKind::*;
    use NodeOutputType::*;

    match kind {
        NodeKind::Entry => (vec![], vec![Control]),
        NodeKind::InitialMemory => (vec![], vec![Memory]),
        NodeKind::InitialVar(_) => (vec![], vec![OutputType(U64)]),
        NodeKind::ControlState => (vec![], vec![Control, ControlPhi]), // variadic inputs
        NodeKind::MemPhi => (vec![ControlPhi], vec![Memory]),           // variadic tail
        NodeKind::ControlPhi(_) => (vec![ControlPhi], vec![OutputType(U64)]), // variadic tail
        NodeKind::If => (vec![Control, OutputType(Bool)], vec![Control, Control]),
        NodeKind::IfCase(_) => (vec![Control], vec![Control]),
        NodeKind::Call => (vec![Control, Memory, OutputType(U64)], vec![Control, Memory]), // variadic args + clobbered
        NodeKind::PostCallMemState => (vec![Control], vec![Memory]),
        NodeKind::PostCallVarState(_) => (vec![Control], vec![OutputType(U64)]),
        NodeKind::Return => (vec![Control, Memory], vec![]), // variadic ret values
        NodeKind::Load(_) => (vec![Memory, OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Store(_) => (vec![Memory, OutputType(U64), OutputType(U64)], vec![Memory]),
        NodeKind::StackStore { .. } => (vec![Memory, OutputType(U64)], vec![Memory]),
        NodeKind::StackStorePhi { .. } => (vec![ControlPhi], vec![Memory]),
        NodeKind::IntConst(_) => (vec![], vec![OutputType(U64)]),
        NodeKind::IntUnaryOp(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::IntBinaryOp(_) => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::IntCmpOp(_) => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(Bool)]),
        NodeKind::CastToInt => (vec![OutputType(Bool)], vec![OutputType(U64)]),
        NodeKind::Truncate => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Popcount => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Lzcount => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Piece => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Extract { .. } => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Insert { .. } => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Extend(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::BoolConst(_) => (vec![], vec![OutputType(Bool)]),
        NodeKind::BoolUnaryOp(_) => (vec![OutputType(Bool)], vec![OutputType(Bool)]),
        NodeKind::BoolBinaryOp(_) => (vec![OutputType(Bool), OutputType(Bool)], vec![OutputType(Bool)]),
        NodeKind::CastToBool => (vec![OutputType(U64)], vec![OutputType(Bool)]),
        NodeKind::FloatConst(_) => (vec![], vec![OutputType(U64)]),
        NodeKind::FloatBinaryOp(_) => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatUnaryOp(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatCmpOp(_) => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(Bool)]),
        NodeKind::FloatIsNan => (vec![OutputType(U64)], vec![OutputType(Bool)]),
        NodeKind::IntToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatToInt => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::IntBitsToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatBitsToInt => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::CastToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::CallOther { .. } => (vec![Control, Memory], vec![Control, Memory]), // variadic
        NodeKind::SegmentOp { .. } => (vec![OutputType(U64), OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::CPoolRef => (vec![], vec![OutputType(U64)]),
        NodeKind::New => (vec![Memory, OutputType(U64)], vec![Memory, OutputType(U64)]),
    }
}
```

**Important notes for the implementer:**
1. Integer ops are typed `OutputType(U64)` as a placeholder in this signature table — the actual graph may use U8/U16/U32/U64 per node. The validator treats all `OutputType(_)` variants as compatible with `OutputType(U64)` in this position (see Task 3 for how); **the signature function only distinguishes `Control` / `Memory` / `ControlPhi` / `OutputType(_)` at the kind level, not the specific integer width.** This is a deliberate simplification; width checks happen elsewhere.
2. The per-NodeKind expected signatures above are derived from `node_view.rs`'s existing match arms. If any arm in the actual `node_view()` function disagrees with the table above, **trust `node_view()` and update this table** — then fix the test.
3. Variadic nodes are flagged in comments. Task 3 handles them specially.

Walk through the existing `node_view()` match arms and cross-check each one against this table. Fix discrepancies.

- [ ] **Step 5: Refactor `node_view()` to use `expected_signature` where possible**

This is optional polish. `node_view()` needs to keep its per-kind `NodeView::*` construction, but its `verify_*` calls can be collapsed where the signature matches. Skip this step if it's not mechanical — the goal of Task 1 is just to expose `expected_signature` as a reusable helper.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --package ir --lib node_view::tests`
Expected: all three tests PASS.

Run: `cargo test --package ir`
Expected: all existing tests still PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ir/src/node_view.rs
git commit -m "ir: extract expected_signature helper from node_view"
```

---

## Task 2: Create `validate` module skeleton with error types

**Files:**
- Create: `crates/ir/src/validate.rs`
- Modify: `crates/ir/src/error.rs`
- Modify: `crates/ir/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ir/src/validate.rs` with just the test first:

```rust
// crates/ir/src/validate.rs

use crate::graph::Graph;
use crate::node::NodeId;

pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    let _ = (graph, entry);
    Ok(())
}

pub struct ValidationErrors(pub Vec<ValidationError>);

#[derive(Debug)]
pub enum ValidationError {
    // filled in by later tasks
}

impl std::fmt::Debug for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ValidationErrors").field(&self.0).finish()
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for err in &self.0 {
            writeln!(f, "{err:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

    #[test]
    fn empty_graph_with_entry_only() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        assert!(validate(&graph, entry).is_ok());
    }
}
```

- [ ] **Step 2: Wire the module into the crate**

Edit `crates/ir/src/lib.rs`, add near the other `pub mod`/`mod` lines:

```rust
pub mod validate;
pub use validate::{ValidationError, ValidationErrors};
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test --package ir --lib validate::tests::empty_graph_with_entry_only`
Expected: PASS.

- [ ] **Step 4: Add `ValidationFailed` variant to `ir::Error`**

Edit `crates/ir/src/error.rs`. Find the `Error` enum and add:

```rust
#[error("ir validation failed:\n{0}")]
ValidationFailed(#[from] crate::validate::ValidationErrors),
```

(Use `#[from]` if the crate uses `thiserror`; the `Cargo.toml` confirms it does.)

- [ ] **Step 5: Run full crate tests**

Run: `cargo test --package ir`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/validate.rs crates/ir/src/lib.rs crates/ir/src/error.rs
git commit -m "ir: add validate module skeleton and ValidationErrors type"
```

---

## Task 3: Layer A — local node typing checks

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add `ValidationError` variants**

Replace the empty `ValidationError` enum body with:

```rust
#[derive(Debug)]
pub enum ValidationError {
    // Layer A
    NodeInputCountMismatch   { node: NodeId, expected: usize, actual: usize },
    NodeInputKindMismatch    { node: NodeId, input_idx: usize, expected: NodeOutputKind, actual: NodeOutputKind },
    NodeOutputCountMismatch  { node: NodeId, expected: usize, actual: usize },
    NodeOutputKindMismatch   { node: NodeId, output_idx: usize, expected: NodeOutputKind, actual: NodeOutputKind },
}
```

Add imports at the top of the file:

```rust
use crate::node::{NodeId, NodeKind, NodeOutputKind, NodeOutputId};
use crate::node_view::expected_signature;
```

(`expected_signature` must be `pub(crate)` in Task 1 — verify and fix if needed.)

- [ ] **Step 2: Write the failing test — wrong input kind**

Add to `#[cfg(test)] mod tests`:

```rust
#[test]
fn layer_a_wrong_input_kind_on_int_unary_op() {
    use crate::ops::IntUnaryOp;
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    // IntUnaryOp expects an OutputType input, but we feed it a Control output.
    let control_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let _bad = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [control_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::NodeInputKindMismatch { input_idx: 0, .. }
    )), "expected a NodeInputKindMismatch, got: {errs:?}");
}
```

- [ ] **Step 3: Run test — expect FAIL**

Run: `cargo test --package ir --lib validate::tests::layer_a_wrong_input_kind_on_int_unary_op`
Expected: FAIL (validate currently returns Ok).

- [ ] **Step 4: Implement Layer A**

Replace the body of `validate` with:

```rust
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    let mut errs: Vec<ValidationError> = Vec::new();

    for node in graph.nodes.keys() {
        check_layer_a(graph, node, &mut errs);
    }

    if errs.is_empty() {
        let _ = entry;
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

fn check_layer_a(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = graph.node_kind(node).clone();
    let (expected_inputs, expected_outputs) = expected_signature(&kind);

    let actual_inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
    let actual_outputs: Vec<NodeOutputKind> = graph
        .node_outputs(node)
        .into_iter()
        .map(|oid| graph.output_kind(oid))
        .collect();

    // Variadic kinds: only the fixed prefix of inputs is checked; the rest is
    // checked by Layer C for the kinds that need it (ControlState, phis, Call, Return).
    let is_variadic_input = matches!(
        kind,
        NodeKind::ControlState
            | NodeKind::MemPhi
            | NodeKind::ControlPhi(_)
            | NodeKind::Call
            | NodeKind::Return
            | NodeKind::CallOther { .. }
    );
    let is_variadic_output = matches!(kind, NodeKind::Call | NodeKind::CallOther { .. });

    if !is_variadic_input && actual_inputs.len() != expected_inputs.len() {
        errs.push(ValidationError::NodeInputCountMismatch {
            node,
            expected: expected_inputs.len(),
            actual: actual_inputs.len(),
        });
    }
    if !is_variadic_output && actual_outputs.len() != expected_outputs.len() {
        errs.push(ValidationError::NodeOutputCountMismatch {
            node,
            expected: expected_outputs.len(),
            actual: actual_outputs.len(),
        });
    }

    // Check the fixed prefix of input kinds.
    let check_len = expected_inputs.len().min(actual_inputs.len());
    for idx in 0..check_len {
        let actual = graph.output_kind(actual_inputs[idx]);
        if !kinds_compatible(expected_inputs[idx], actual) {
            errs.push(ValidationError::NodeInputKindMismatch {
                node,
                input_idx: idx,
                expected: expected_inputs[idx],
                actual,
            });
        }
    }

    // Check the fixed prefix of output kinds.
    let check_len = expected_outputs.len().min(actual_outputs.len());
    for idx in 0..check_len {
        if !kinds_compatible(expected_outputs[idx], actual_outputs[idx]) {
            errs.push(ValidationError::NodeOutputKindMismatch {
                node,
                output_idx: idx,
                expected: expected_outputs[idx],
                actual: actual_outputs[idx],
            });
        }
    }
}

/// Two `NodeOutputKind`s are compatible if they are the same variant;
/// for `OutputType`, any integer width is compatible with any other.
fn kinds_compatible(expected: NodeOutputKind, actual: NodeOutputKind) -> bool {
    use NodeOutputKind::*;
    match (expected, actual) {
        (Control, Control) => true,
        (Memory, Memory) => true,
        (ControlPhi, ControlPhi) => true,
        (OutputType(_), OutputType(_)) => true,
        _ => false,
    }
}
```

- [ ] **Step 5: Add remaining Layer A tests**

```rust
#[test]
fn layer_a_wrong_output_kind() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Memory]); // should be Control
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::NodeOutputKindMismatch { output_idx: 0, .. }
    )), "got: {errs:?}");
}

#[test]
fn layer_a_wrong_input_count() {
    use crate::ops::IntBinaryOp;
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let c = graph.create_node(NodeKind::IntConst(5), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let c_out = graph.node_outputs(c).into_iter().next().unwrap();

    // IntBinaryOp expects 2 inputs, give it 1.
    let _bad = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::NodeInputCountMismatch { expected: 2, actual: 1, .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 6: Run tests — expect PASS**

Run: `cargo test --package ir --lib validate::tests`
Expected: all tests PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ir/src/validate.rs
git commit -m "ir: implement Layer A local node typing validation"
```

---

## Task 4: Layer B — use-list consistency (forward + backward)

**Files:**
- Modify: `crates/ir/src/validate.rs`
- Modify: `crates/ir/src/graph.rs` (add `#[cfg(test)]` corruption helper for testing)

- [ ] **Step 1: Add Layer B error variants**

Extend the `ValidationError` enum:

```rust
// Layer B
InputPointsToMissingOutput { node: NodeId, input_idx: usize, output: NodeOutputId },
InputMissingFromUseList    { node: NodeId, input_idx: usize, output: NodeOutputId },
UseListContainsStaleInput  { output: NodeOutputId, listed_input: NodeInputId },
```

Add import: `use crate::node::NodeInputId;`

- [ ] **Step 2: Add a test-only corruption helper to `Graph`**

Because corrupting a use-list via the public API isn't possible, add a `#[cfg(test)]` helper on `Graph` in `crates/ir/src/graph.rs`. This helper detaches one specific consumer from the doubly-linked list without touching the `inputs` PrimaryMap — exactly the bug we want Layer B to catch.

Add to `impl Graph` in `graph.rs`:

```rust
#[cfg(test)]
pub(crate) fn test_only_clear_consumers_of(&mut self, output: NodeOutputId) {
    // The validator's backward pass walks the consumer list; if we clear the
    // head, any NodeInputId still pointing at this output will now be
    // unreachable through the list -> InputMissingFromUseList.
    //
    // Field names below must match Graph's actual storage; adjust if the
    // implementer sees different field names.
    self.outputs[output].consumer_head = None;
}
```

**If the field name differs** (e.g. `first_use`, `use_list_head`), adjust. The implementer should grep `graph.rs` for the use-list head field and use that name.

- [ ] **Step 3: Write failing test — forward walk catches input pointing at freed/missing output**

This is tricky: the PrimaryMap doesn't expose "missing" keys. Skip constructing a truly missing NodeOutputId by hand; the forward-consistency check is about the use-list, not about dangling NodeOutputIds. Focus on the `InputMissingFromUseList` case instead:

```rust
#[test]
fn layer_b_input_missing_from_use_list() {
    use crate::ops::IntUnaryOp;
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let c = graph.create_node(NodeKind::IntConst(5), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let c_out = graph.node_outputs(c).into_iter().next().unwrap();
    let _n = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Corrupt: drop `c`'s consumer list head. The input on `_n` now has no
    // matching consumer-list entry.
    graph.test_only_clear_consumers_of(c_out);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::InputMissingFromUseList { .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 4: Run test — expect FAIL**

Run: `cargo test --package ir --lib validate::tests::layer_b_input_missing_from_use_list`
Expected: FAIL (no Layer B check yet).

- [ ] **Step 5: Implement Layer B forward check**

Add to `validate.rs`:

```rust
fn check_layer_b(graph: &Graph, errs: &mut Vec<ValidationError>) {
    // Forward: each input must be in its producer's consumer list.
    for node in graph.nodes.keys() {
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        for (idx, &target) in inputs.iter().enumerate() {
            let input_id_for_this_slot = graph.node_input_id_at(node, idx);
            let in_list = graph
                .output_uses(target)
                .into_iter()
                .any(|listed| listed == input_id_for_this_slot);
            if !in_list {
                errs.push(ValidationError::InputMissingFromUseList {
                    node,
                    input_idx: idx,
                    output: target,
                });
            }
        }
    }

    // Backward: every entry in a consumer list must point back at this output.
    for node in graph.nodes.keys() {
        for out in graph.node_outputs(node).into_iter() {
            for listed_input in graph.output_uses(out).into_iter() {
                let target = graph.input_target(listed_input);
                if target != out {
                    errs.push(ValidationError::UseListContainsStaleInput {
                        output: out,
                        listed_input,
                    });
                }
            }
        }
    }
}
```

Then call it from `validate()`:

```rust
for node in graph.nodes.keys() {
    check_layer_a(graph, node, &mut errs);
}
check_layer_b(graph, &mut errs);
```

**Graph API gaps.** The helpers `graph.node_input_id_at(node, idx)` and `graph.input_target(input_id)` may not exist on `Graph`. If they don't, add them in `graph.rs`:

```rust
pub fn node_input_id_at(&self, node: NodeId, idx: usize) -> NodeInputId {
    self.nodes[node].inputs[idx]
}
pub fn input_target(&self, input: NodeInputId) -> NodeOutputId {
    self.inputs[input].target
}
```

Adjust field names to the actual storage. Same-crate access means `pub(crate)` is fine; using `pub` is fine too.

- [ ] **Step 6: Add a second test-only helper and the backward-walk test**

Add to `impl Graph` in `graph.rs` (next to `test_only_clear_consumers_of`):

```rust
#[cfg(test)]
pub(crate) fn test_only_retarget_input(&mut self, input: NodeInputId, new_target: NodeOutputId) {
    // Overwrite the target without touching the doubly-linked use lists.
    // Old target's consumer list still contains this NodeInputId but the
    // input no longer points back -> UseListContainsStaleInput.
    //
    // Adjust field name if `target` isn't the stored field; grep `inputs:`
    // in graph.rs to find the actual field.
    self.inputs[input].target = new_target;
}
```

Then the test:

```rust
#[test]
fn layer_b_stale_input_in_use_list() {
    use crate::ops::IntUnaryOp;
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let a = graph.create_node(NodeKind::IntConst(1), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let b = graph.create_node(NodeKind::IntConst(2), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let a_out = graph.node_outputs(a).into_iter().next().unwrap();
    let b_out = graph.node_outputs(b).into_iter().next().unwrap();
    let op = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [a_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Retarget op's input from a_out to b_out without fixing use lists.
    let op_input_0 = graph.node_input_id_at(op, 0);
    graph.test_only_retarget_input(op_input_0, b_out);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::UseListContainsStaleInput { .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 7: Run all tests**

Run: `cargo test --package ir --lib validate::tests`
Expected: all tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/ir/src/validate.rs crates/ir/src/graph.rs
git commit -m "ir: implement Layer B use-list consistency validation"
```

---

## Task 5: Layer C — Entry / InitialMemory uniqueness

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add error variants**

```rust
// Layer C — whole graph
MultipleEntryNodes         { first: NodeId, second: NodeId },
MultipleInitialMemoryNodes { first: NodeId, second: NodeId },
MissingEntryNode,
MissingInitialMemoryNode,
```

- [ ] **Step 2: Write failing tests**

```rust
#[test]
fn layer_c_missing_initial_memory() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(e, ValidationError::MissingInitialMemoryNode)),
        "got: {errs:?}");
}

#[test]
fn layer_c_duplicate_entry() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _entry2 = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(e, ValidationError::MultipleEntryNodes { .. })),
        "got: {errs:?}");
}
```

- [ ] **Step 3: Run tests — expect FAIL**

Run: `cargo test --package ir --lib validate::tests::layer_c_missing_initial_memory validate::tests::layer_c_duplicate_entry`
Expected: FAIL.

- [ ] **Step 4: Implement**

Add to `validate.rs`:

```rust
fn check_layer_c_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
    let mut entries: Vec<NodeId> = vec![];
    let mut mems: Vec<NodeId> = vec![];
    for node in graph.nodes.keys() {
        match graph.node_kind(node) {
            NodeKind::Entry => entries.push(node),
            NodeKind::InitialMemory => mems.push(node),
            _ => {}
        }
    }
    match entries.as_slice() {
        [] => errs.push(ValidationError::MissingEntryNode),
        [_] => {}
        [a, b, ..] => errs.push(ValidationError::MultipleEntryNodes { first: *a, second: *b }),
    }
    match mems.as_slice() {
        [] => errs.push(ValidationError::MissingInitialMemoryNode),
        [_] => {}
        [a, b, ..] => errs.push(ValidationError::MultipleInitialMemoryNodes { first: *a, second: *b }),
    }
}
```

Call it from `validate()` after `check_layer_b`.

- [ ] **Step 5: Run tests — expect PASS**

Run: `cargo test --package ir --lib validate::tests`

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/validate.rs
git commit -m "ir: validate Entry/InitialMemory uniqueness"
```

---

## Task 6: Layer C — ControlState predecessor kinds

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add error variant**

```rust
ControlStateNonControlPredecessor {
    control_state: NodeId,
    input_idx: usize,
    producer: NodeId,
    producer_kind: NodeOutputKind,
},
```

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn layer_c_control_state_bad_predecessor() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();

    // ControlState with a Memory predecessor instead of Control.
    let _bad_cs = graph.create_node(
        NodeKind::ControlState,
        [mem_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::ControlStateNonControlPredecessor { input_idx: 0, .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 3: Run — expect FAIL**

- [ ] **Step 4: Implement**

```rust
fn check_layer_c_control_state(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        if !matches!(graph.node_kind(node), NodeKind::ControlState) {
            continue;
        }
        for (idx, target) in graph.node_inputs(node).into_iter().enumerate() {
            let kind = graph.output_kind(target);
            if kind != NodeOutputKind::Control {
                let (producer, _) = graph.output_definition(target);
                errs.push(ValidationError::ControlStateNonControlPredecessor {
                    control_state: node,
                    input_idx: idx,
                    producer,
                    producer_kind: kind,
                });
            }
        }
    }
}
```

Call from `validate()`.

- [ ] **Step 5: Run — expect PASS, commit**

```bash
cargo test --package ir --lib validate::tests
git add crates/ir/src/validate.rs
git commit -m "ir: validate ControlState predecessor kinds"
```

---

## Task 7: Layer C — phi ownership + value arity

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add error variants**

```rust
PhiTokenNotFromControlState { phi: NodeId, producer: NodeId, producer_kind: NodeOutputKind },
PhiValueArityMismatch       { phi: NodeId, owner_control_state: NodeId, expected_predecessors: usize, actual_values: usize },
```

- [ ] **Step 2: Write failing tests**

```rust
#[test]
fn layer_c_phi_token_from_wrong_node() {
    // Construct a ControlPhi whose input[0] is not a ControlPhi-kind output.
    // Easiest: feed it a ControlPhi-typed output from a synthetic non-ControlState
    // source — but nothing else produces ControlPhi. So emit a ControlPhi whose
    // input[0] is (somehow) a ControlState's *Control* output, not its ControlPhi.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_control_out = graph.node_outputs(cs).into_iter().next().unwrap(); // index 0 = Control
    let vn = test_vn(); // see helper below; grep existing tests for rsleigh::Vn construction
    let _phi = graph.create_node(
        NodeKind::ControlPhi(vn),
        [cs_control_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::PhiTokenNotFromControlState { .. }
    )), "got: {errs:?}");
}
```

Add a `fn test_vn() -> rsleigh::Vn` helper inside `#[cfg(test)] mod tests`. To find the right constructor: grep for `Vn::` in `crates/ir/` and `../rsleigh/` — existing ir or analyzer tests (e.g. `crates/analyzer/tests/analyze_binary.rs`) will show a working constructor. If none exists, check `../rsleigh/src/` for `impl Vn` — the public constructor is typically `Vn::register`, `Vn::new`, or similar. Use any valid varnode; the specific bytes don't matter for these tests.

- [ ] **Step 3: Run — expect FAIL**

- [ ] **Step 4: Implement**

```rust
fn check_layer_c_phis(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        let is_phi = matches!(
            graph.node_kind(node),
            NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. }
        );
        if !is_phi {
            continue;
        }

        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            // Layer A already flagged this; move on.
            continue;
        }
        let token = inputs[0];
        let token_kind = graph.output_kind(token);
        if token_kind != NodeOutputKind::ControlPhi {
            let (producer, _) = graph.output_definition(token);
            errs.push(ValidationError::PhiTokenNotFromControlState {
                phi: node,
                producer,
                producer_kind: token_kind,
            });
            continue;
        }

        // Token must come from a ControlState's ControlPhi output.
        let (owner, _idx) = graph.output_definition(token);
        if !matches!(graph.node_kind(owner), NodeKind::ControlState) {
            errs.push(ValidationError::PhiTokenNotFromControlState {
                phi: node,
                producer: owner,
                producer_kind: token_kind,
            });
            continue;
        }

        let expected_preds = graph.node_inputs(owner).into_iter().count();
        let actual_values = inputs.len() - 1;
        if expected_preds != actual_values {
            errs.push(ValidationError::PhiValueArityMismatch {
                phi: node,
                owner_control_state: owner,
                expected_predecessors: expected_preds,
                actual_values,
            });
        }
    }
}
```

Call from `validate()`.

- [ ] **Step 5: Add arity test**

```rust
#[test]
fn layer_c_phi_value_arity_mismatch() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();

    // ControlState with one predecessor.
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();

    // Phi with token + 2 values, but ControlState has only 1 predecessor.
    let c1 = graph.create_node(NodeKind::IntConst(1), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let c2 = graph.create_node(NodeKind::IntConst(2), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let c1_out = graph.node_outputs(c1).into_iter().next().unwrap();
    let c2_out = graph.node_outputs(c2).into_iter().next().unwrap();
    let vn = test_vn();
    let _phi = graph.create_node(
        NodeKind::ControlPhi(vn),
        [cs_phi_out, c1_out, c2_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::PhiValueArityMismatch { expected_predecessors: 1, actual_values: 2, .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 6: Run — expect PASS, commit**

```bash
cargo test --package ir --lib validate::tests
git add crates/ir/src/validate.rs
git commit -m "ir: validate phi ownership and value arity"
```

---

## Task 8: Layer C — PostCall producer structural check

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add error variants**

```rust
PostCallMemStateNotAfterCall { node: NodeId, producer: NodeId, producer_kind: NodeOutputKind },
PostCallVarStateNotAfterCall { node: NodeId, producer: NodeId, producer_kind: NodeOutputKind },
```

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn layer_c_postcall_mem_state_from_entry_is_error() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();

    // PostCallMemState's input must come from a Call. Feed it Entry's Control.
    let _bad = graph.create_node(
        NodeKind::PostCallMemState,
        [entry_ctrl],
        [NodeOutputKind::Memory],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::PostCallMemStateNotAfterCall { .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 3: Run — expect FAIL**

- [ ] **Step 4: Implement**

```rust
fn check_layer_c_postcall_producer(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        let kind = graph.node_kind(node).clone();
        let (mk_err, idx_in_call_must_be_zero): (fn(NodeId, NodeId, NodeOutputKind) -> ValidationError, bool) =
            match kind {
                NodeKind::PostCallMemState => (|n, p, k| ValidationError::PostCallMemStateNotAfterCall { node: n, producer: p, producer_kind: k }, true),
                NodeKind::PostCallVarState(_) => (|n, p, k| ValidationError::PostCallVarStateNotAfterCall { node: n, producer: p, producer_kind: k }, true),
                _ => continue,
            };

        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            continue; // Layer A will have flagged it
        }
        let target = inputs[0];
        let (producer, producer_out_idx) = graph.output_definition(target);

        let producer_kind = graph.output_kind(target);
        let is_call_control = matches!(graph.node_kind(producer), NodeKind::Call)
            && idx_in_call_must_be_zero
            && producer_out_idx == 0; // Call's output[0] is Control
        if !is_call_control {
            errs.push(mk_err(node, producer, producer_kind));
        }
    }
}
```

Call from `validate()`.

- [ ] **Step 5: Run — expect PASS, commit**

```bash
cargo test --package ir --lib validate::tests
git add crates/ir/src/validate.rs
git commit -m "ir: validate PostCall nodes consume Call control output"
```

---

## Task 9: Layer C — PostCall uniqueness per Call

**Files:**
- Modify: `crates/ir/src/validate.rs`

- [ ] **Step 1: Add error variants**

```rust
DuplicatePostCallMemState { call: NodeId, first: NodeId, second: NodeId },
DuplicatePostCallVarState { call: NodeId, vn: rsleigh::Vn, first: NodeId, second: NodeId },
```

Add import `use rsleigh::Vn;` if not already present.

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn layer_c_two_postcall_mem_states_on_same_call() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();
    let addr = graph.create_node(NodeKind::IntConst(0x1000), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let addr_out = graph.node_outputs(addr).into_iter().next().unwrap();

    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, mem_out, addr_out],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    let call_ctrl = graph.node_outputs(call).into_iter().next().unwrap();

    let _pcm1 = graph.create_node(NodeKind::PostCallMemState, [call_ctrl], [NodeOutputKind::Memory]);
    let _pcm2 = graph.create_node(NodeKind::PostCallMemState, [call_ctrl], [NodeOutputKind::Memory]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::DuplicatePostCallMemState { .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 3: Run — expect FAIL**

- [ ] **Step 4: Implement**

```rust
fn check_layer_c_postcall_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
    use std::collections::HashMap;

    let mut mem_states_by_call: HashMap<NodeId, NodeId> = HashMap::new();
    let mut var_states_by_call: HashMap<(NodeId, rsleigh::Vn), NodeId> = HashMap::new();

    for node in graph.nodes.keys() {
        let kind = graph.node_kind(node).clone();
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            continue;
        }
        let (producer, producer_out_idx) = graph.output_definition(inputs[0]);
        if producer_out_idx != 0 || !matches!(graph.node_kind(producer), NodeKind::Call) {
            continue; // Task 8 handles this
        }

        match kind {
            NodeKind::PostCallMemState => {
                if let Some(&first) = mem_states_by_call.get(&producer) {
                    errs.push(ValidationError::DuplicatePostCallMemState {
                        call: producer,
                        first,
                        second: node,
                    });
                } else {
                    mem_states_by_call.insert(producer, node);
                }
            }
            NodeKind::PostCallVarState(vn) => {
                let key = (producer, vn);
                if let Some(&first) = var_states_by_call.get(&key) {
                    errs.push(ValidationError::DuplicatePostCallVarState {
                        call: producer,
                        vn,
                        first,
                        second: node,
                    });
                } else {
                    var_states_by_call.insert(key, node);
                }
            }
            _ => {}
        }
    }
}
```

Call from `validate()`.

- [ ] **Step 5: Add the varstate duplicate test**

```rust
#[test]
fn layer_c_two_postcall_var_states_same_vn() {
    // Similar to layer_c_two_postcall_mem_states_on_same_call but create two
    // PostCallVarState(vn) nodes with the same Vn.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();
    let addr = graph.create_node(NodeKind::IntConst(0x1000), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let addr_out = graph.node_outputs(addr).into_iter().next().unwrap();
    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, mem_out, addr_out],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    let call_ctrl = graph.node_outputs(call).into_iter().next().unwrap();

    let vn = test_vn();
    let _v1 = graph.create_node(NodeKind::PostCallVarState(vn), [call_ctrl], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let _v2 = graph.create_node(NodeKind::PostCallVarState(vn), [call_ctrl], [NodeOutputKind::OutputType(NodeOutputType::U64)]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(errs.0.iter().any(|e| matches!(
        e,
        ValidationError::DuplicatePostCallVarState { .. }
    )), "got: {errs:?}");
}
```

- [ ] **Step 6: Run — expect PASS, commit**

```bash
cargo test --package ir --lib validate::tests
git add crates/ir/src/validate.rs
git commit -m "ir: validate PostCall nodes are unique per Call"
```

---

## Task 10: Wire validation into `FunctionBuilder::build()`

**Files:**
- Modify: `crates/ir/src/builder.rs`
- Modify: `crates/ir/src/function.rs` (if needed to access the graph from entry)
- Modify: `crates/analyzer/src/analyzer.rs` (call-site update)
- Modify: `crates/analyzer/examples/analyzer.rs` (may need no change if already `?`)
- Modify: `crates/analyzer/tests/analyze_binary.rs` (if affected)

- [ ] **Step 1: Change `build()` signature**

In `crates/ir/src/builder.rs`, change:

```rust
pub fn build(self) -> crate::function::BuiltFunctionGraph {
    BuiltFunctionGraph { /* ... */ }
}
```

to:

```rust
pub fn build(self) -> crate::Result<crate::function::BuiltFunctionGraph> {
    let built = crate::function::BuiltFunctionGraph {
        graph: self.function.graph,
        entry: self.function.entry,
        variables: self.variables,
        call_clobbered: self.call_cloberred_variables.into_boxed_slice(),
    };
    crate::validate::validate(&built.graph, built.entry)?;
    Ok(built)
}
```

The `?` relies on `ir::Error::ValidationFailed(#[from] ValidationErrors)` from Task 2 Step 4.

- [ ] **Step 2: Find every caller of `build()` and add `?`**

Run: `cargo build --workspace 2>&1 | head -80`

The compile errors list every caller. For each one, add `?`. Typical pattern: `let bfg = builder.build();` → `let bfg = builder.build()?;`.

Known call sites (from the spec):
- `crates/analyzer/src/analyzer.rs:790` — inside `analyze_cfg` which already returns `Result`, so add `?`.
- Any test helpers in the ir / opt / analyzer crates that call `build()` — update signatures to return `Result` or `.unwrap()` in a test-helper context (tests may panic on validation failure; that's fine since tests can `.unwrap()` trivially).

- [ ] **Step 3: Run full workspace build**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass. If the validator catches a real bug in the analyzer's output, **stop and report the error to the user** — this means we've found a real IR construction bug that must be fixed before proceeding.

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder.rs crates/analyzer/src/analyzer.rs crates/analyzer/examples/analyzer.rs crates/analyzer/tests/analyze_binary.rs
git commit -m "ir: validate graph at the end of FunctionBuilder::build"
```

---

## Task 11: Wire validation into `OptimizerPipeline::run()`

**Files:**
- Modify: `crates/opt/src/opt.rs`

- [ ] **Step 1: Add validate call**

In `crates/opt/src/opt.rs`, change the end of `OptimizerPipeline::run()`:

```rust
pub fn run(&self, graph: &mut ir::BuiltFunctionGraph) -> crate::Result<()> {
    loop {
        let mut changed = false;
        for opt in &self.optimizers {
            if opt.optimize(graph)?.changed() {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for opt in &self.post_passes {
        opt.optimize(graph)?;
    }
    ir::validate::validate(&graph.graph, graph.entry)?;
    Ok(())
}
```

Because `opt::Error` has `#[from] ir::Error`, and `ir::Error` has `From<ValidationErrors>`, the `?` chains correctly. If the implementer finds that `BuiltFunctionGraph`'s `graph` and `entry` fields are not accessible from `opt`, add accessor methods on `BuiltFunctionGraph`:

```rust
// in crates/ir/src/function.rs
impl BuiltFunctionGraph {
    pub fn graph(&self) -> &crate::graph::Graph { &self.graph }
    pub fn entry(&self) -> crate::node::NodeId { self.entry }
}
```

- [ ] **Step 2: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass. Same caveat as Task 10 Step 4 — if the validator catches a real bug introduced by an opt pass, stop and report.

- [ ] **Step 3: Commit**

```bash
git add crates/opt/src/opt.rs crates/ir/src/function.rs
git commit -m "opt: validate graph at end of OptimizerPipeline::run"
```

---

## Task 12: End-to-end smoke test via the analyzer example

**Files:**
- Modify: `crates/analyzer/tests/analyze_binary.rs` (or create a new test there)

- [ ] **Step 1: Write the test**

The existing `crates/analyzer/tests/analyze_binary.rs` already runs `analyze_cfg` on a real binary. Extend it (or add a new `#[test]`) that asserts the full pipeline succeeds:

```rust
#[test]
fn validator_accepts_analyzer_output_on_binary_test() {
    // mirror the setup of the existing test in this file; call
    // analyze_cfg(...) and then build_optimizer_pipeline().run(...).
    // Both are already `?`-propagating; a Err from validation will fail the
    // test naturally. No assertion beyond "both calls returned Ok" needed.

    // Copy the setup block from the existing test, change only the asserts:
    let cfg = /* ... as in existing test ... */;
    let mut function = analyzer.analyze_cfg(&cfg).expect("analyze_cfg should produce a valid graph");
    analyzer.build_optimizer_pipeline()
        .run(&mut function)
        .expect("optimizer should leave the graph valid");
}
```

Use `.expect(...)` here (not `?`) so failures surface with a clear message.

- [ ] **Step 2: Run**

Run: `cargo test --package analyzer --test analyze_binary`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/analyze_binary.rs
git commit -m "analyzer: smoke test that validator accepts real binary output"
```

---

## Final verification

- [ ] Run: `cargo clippy --workspace -- -D warnings`
- [ ] Run: `cargo test --workspace`
- [ ] Run: `cargo run --example analyzer` — confirm it still produces cfg.html / graph.html without error.

If any of the above fails, do **not** mark complete. Diagnose and fix before reporting done.
