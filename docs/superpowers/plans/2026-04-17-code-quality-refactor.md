# Code-Quality Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce duplication and boilerplate across the workspace so that adding new IR features, optimization passes, and pattern queries requires less code and reads more clearly. Preserves all existing functionality; every existing test keeps passing unchanged.

**Architecture:** Five phases landed in order, each independently reviewable. Phase 0 introduces an `ir::ops` module with shared graph helpers (consts, rewrite, builder) and error-assertion methods on `NodeOutputKind`. Phase 1 table-drives `NodeOutputType`. Phase 2 introduces a `rewrite_rules!` proc-macro DSL and rewrites `opt/constant_fold.rs` to use it. Phase 3 adds an `IntoPat` blanket trait and matcher helpers to the `pattern` crate. Phase 4 mechanically applies the new helpers to the remaining `opt` passes.

**Tech Stack:** Rust (edition 2024), `proc-macro2`/`syn`/`quote` for the DSL, `trybuild` for macro tests, existing `strider_error::define_error!` macro for error variants, `cranelift-entity` primary maps, `petgraph`, `rsleigh`.

---

## File Structure

**New files:**
- `crates/ir/src/ops/mod.rs` — module root; re-exports op enums, mounts new submodules.
- `crates/ir/src/ops/op_kinds.rs` — target for the contents of the current `crates/ir/src/ops.rs` (op enums `IntBinaryOp`, `BoolBinaryOp`, `FloatBinaryOp`, etc.).
- `crates/ir/src/ops/consts.rs` — `int_const_val`, `bool_const_val`, `float_const_val`, `make_int_const`, `make_bool_const`, `make_float_const` as methods on `BuiltFunctionGraph`.
- `crates/ir/src/ops/rewrite.rs` — `replace_all_uses` method.
- `crates/ir/src/ops/builder.rs` — `make_value_node` method + the two specialised `make_int_bits_to_float_node` / `make_float_to_float_node` helpers migrated from `opt/utils.rs`.
- `crates/ir-macros/tests/rewrite_rules_ok.rs` — ok-path trybuild tests.
- `crates/ir-macros/tests/rewrite_rules_err.rs` — err-path trybuild tests.
- `crates/ir-macros/tests/rewrite_rules_expand.rs` — runtime expansion tests (rule firing on a graph).
- `crates/ir-macros/src/rewrite_rules/mod.rs` — new proc-macro implementation (entry-point, spans, codegen).
- `crates/ir-macros/src/rewrite_rules/parse.rs` — LHS/RHS parser.
- `crates/ir-macros/src/rewrite_rules/lhs.rs` — LHS match codegen.
- `crates/ir-macros/src/rewrite_rules/rhs.rs` — RHS expression codegen.

**Deleted files:**
- `crates/ir/src/ops.rs` (replaced by `ops/op_kinds.rs` through the `mod.rs` re-export).
- `crates/opt/src/utils.rs`.

**Modified files (by phase):**
- Phase 0: `crates/ir/src/lib.rs`, `crates/ir/src/node.rs`, `crates/ir/src/error.rs`, `crates/opt/src/opt.rs`, and one-site migrations of `opt/src/{known_bits,dead_branch,redundant_phis,load_readonly,stack_store}.rs`.
- Phase 1: `crates/ir/src/node.rs`.
- Phase 2: `crates/ir-macros/src/lib.rs`, `crates/opt/src/constant_fold.rs`.
- Phase 3: `crates/pattern/src/pat.rs`, `crates/pattern/src/matcher.rs`, `crates/pattern/src/lib.rs`.
- Phase 4: all remaining `as_value().ok_or(...)` and phi-kind match-arm call sites in `crates/opt/src/`.

---

## Pre-flight: baseline capture

### Task P0: Capture analyzer-output baseline

**Files:**
- Create: `scripts/capture-baseline.sh`
- Create: `scripts/compare-analyzer-output.sh`

These scripts serve as the semantic-equivalence gate called out in the spec. They are deleted at the end of the refactor.

- [ ] **Step P0.1: Create the baseline-capture script**

Write `scripts/capture-baseline.sh`:

```bash
#!/usr/bin/env bash
# Captures analyzer-output baseline for the refactor. Run once before Phase 0.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT_DIR="scripts/baseline"
mkdir -p "$OUT_DIR"
cargo run --quiet --example analyzer
cp cfg.html  "$OUT_DIR/cfg.html"
cp graph.html "$OUT_DIR/graph.html"
cp cfg.dot    "$OUT_DIR/cfg.dot"
cp graph.dot  "$OUT_DIR/graph.dot"
echo "baseline captured in $OUT_DIR"
```

Make it executable: `chmod +x scripts/capture-baseline.sh`.

- [ ] **Step P0.2: Create the compare script**

Write `scripts/compare-analyzer-output.sh`:

```bash
#!/usr/bin/env bash
# Compares current analyzer output against baseline. Semantic check only:
# same node-kind counts and same edge count. Node IDs are allowed to shift.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run --quiet --example analyzer

# Extract node-kind label frequencies (from the `[label="…"]` attribute, first line).
kind_counts() {
    grep -oE 'label="[^"\\]*' "$1" | sort | uniq -c | sort -k2
}

edge_count() {
    grep -cE '^\s*"[0-9]+"\s*->\s*"[0-9]+"' "$1"
}

diff <(kind_counts scripts/baseline/graph.dot) <(kind_counts graph.dot) \
    || { echo "graph.dot node-kind counts differ"; exit 1; }

b_edges=$(edge_count scripts/baseline/graph.dot)
c_edges=$(edge_count graph.dot)
[ "$b_edges" = "$c_edges" ] || { echo "graph.dot edge count differs: $b_edges vs $c_edges"; exit 1; }

echo "analyzer output semantically equivalent to baseline"
```

Make it executable: `chmod +x scripts/compare-analyzer-output.sh`.

- [ ] **Step P0.3: Capture baseline**

Run: `scripts/capture-baseline.sh`
Expected: prints `baseline captured in scripts/baseline`.

- [ ] **Step P0.4: Verify comparison script passes on unchanged tree**

Run: `scripts/compare-analyzer-output.sh`
Expected: prints `analyzer output semantically equivalent to baseline`.

- [ ] **Step P0.5: Commit**

```bash
git add scripts/capture-baseline.sh scripts/compare-analyzer-output.sh scripts/baseline/
git commit -m "scripts: add analyzer-output baseline for refactor"
```

---

## Phase 0 — `ir::ops` foundation

### Task 0.1: Split `ir::ops` into module with submodules

**Files:**
- Create: `crates/ir/src/ops/mod.rs`
- Create: `crates/ir/src/ops/op_kinds.rs`
- Delete: `crates/ir/src/ops.rs`

- [ ] **Step 0.1.1: Create `ops/op_kinds.rs` with the current `ops.rs` contents**

Copy the contents of `crates/ir/src/ops.rs` verbatim into `crates/ir/src/ops/op_kinds.rs`. (Use the file system: `git mv crates/ir/src/ops.rs crates/ir/src/ops/op_kinds.rs` after creating the directory; git will track the rename.)

- [ ] **Step 0.1.2: Create `ops/mod.rs` that re-exports op enums**

Write `crates/ir/src/ops/mod.rs`:

```rust
//! Operation kinds used by IR nodes, plus helper methods on
//! [`crate::function::BuiltFunctionGraph`] grouped by purpose:
//!
//! - [`consts`] — constant-output inspection & creation.
//! - [`rewrite`] — graph-mutation helpers like `replace_all_uses`.
//! - [`builder`] — ergonomic node constructors.

mod op_kinds;

pub use op_kinds::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

pub mod builder;
pub mod consts;
pub mod rewrite;
```

- [ ] **Step 0.1.3: Create empty submodule files**

Create `crates/ir/src/ops/consts.rs`, `crates/ir/src/ops/rewrite.rs`, `crates/ir/src/ops/builder.rs` each with only:

```rust
//! (documented below in subsequent tasks)
```

- [ ] **Step 0.1.4: Verify build**

Run: `cargo build -p ir`
Expected: PASS.

- [ ] **Step 0.1.5: Run all tests**

Run: `cargo test --workspace`
Expected: every existing test passes (this is a file reshuffle only).

- [ ] **Step 0.1.6: Commit**

```bash
git add crates/ir/src/ops/ crates/ir/src/ops.rs
git commit -m "ir: split ops into module with submodules for upcoming helpers"
```

---

### Task 0.2: Add `ir::ErrorKind::ExpectedValueOutput` variant

**Files:**
- Modify: `crates/ir/src/error.rs`

- [ ] **Step 0.2.1: Add the variant**

Edit `crates/ir/src/error.rs`, inserting this variant immediately after the existing `ExpectedValue` variant (around line 35):

```rust
        /// An output was expected to carry a concrete value type but is a
        /// control/memory/control-phi edge instead. Unlike [`Self::ExpectedValue`],
        /// this variant carries only the mismatched kind (no output id), used
        /// by [`crate::node::NodeOutputKind::as_value_or_err`].
        #[error("expected value output, got {0:?}")]
        ExpectedValueOutput(NodeOutputKind),
```

- [ ] **Step 0.2.2: Verify build**

Run: `cargo build -p ir`
Expected: PASS.

- [ ] **Step 0.2.3: Commit**

```bash
git add crates/ir/src/error.rs
git commit -m "ir: add ExpectedValueOutput error variant"
```

---

### Task 0.3: Add error-assertion helpers on `NodeOutputKind`

**Files:**
- Modify: `crates/ir/src/node.rs`
- Test: `crates/ir/src/node.rs` (add to existing `#[cfg(test)] mod tests`)

- [ ] **Step 0.3.1: Write failing tests**

Locate the `#[cfg(test)] mod tests` block in `crates/ir/src/node.rs` (or append one if absent) and add:

```rust
    #[test]
    fn as_value_or_err_value_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U32);
        assert_eq!(kind.as_value_or_err().unwrap(), NodeOutputType::U32);
    }

    #[test]
    fn as_value_or_err_control_case() {
        let kind = NodeOutputKind::Control;
        let err = kind.as_value_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedValueOutput(_)));
    }

    #[test]
    fn as_integer_or_err_int_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U64);
        assert_eq!(kind.as_integer_or_err().unwrap(), NodeOutputType::U64);
    }

    #[test]
    fn as_integer_or_err_float_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::F32);
        let err = kind.as_integer_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedIntegerType(_)));
    }

    #[test]
    fn as_float_or_err_float_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::F64);
        assert_eq!(kind.as_float_or_err().unwrap(), NodeOutputType::F64);
    }

    #[test]
    fn as_float_or_err_int_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U32);
        let err = kind.as_float_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedFloatType(_)));
    }
```

- [ ] **Step 0.3.2: Run tests to verify they fail**

Run: `cargo test -p ir node::tests::as_value_or_err_value_case`
Expected: FAIL with "no method named `as_value_or_err` found".

- [ ] **Step 0.3.3: Implement helpers**

Locate the `impl NodeOutputKind` block in `crates/ir/src/node.rs`. Immediately after the existing `pub fn as_value(self) -> Option<NodeOutputType>` method, add:

```rust
    /// Returns the value type, or an error whose payload is `self` if this
    /// kind is not a value edge.
    #[track_caller]
    pub fn as_value_or_err(self) -> crate::Result<NodeOutputType> {
        self.as_value()
            .ok_or_else(|| crate::ErrorKind::ExpectedValueOutput(self).into())
    }

    /// Returns the value type, asserting it is integer. Errors as
    /// [`crate::ErrorKind::ExpectedValueOutput`] for non-value kinds and as
    /// [`crate::ErrorKind::ExpectedIntegerType`] for bool/float value kinds.
    #[track_caller]
    pub fn as_integer_or_err(self) -> crate::Result<NodeOutputType> {
        let ty = self.as_value_or_err()?;
        if ty.is_integer() {
            Ok(ty)
        } else {
            Err(crate::ErrorKind::ExpectedIntegerType(ty).into())
        }
    }

    /// Returns the value type, asserting it is float. Errors as
    /// [`crate::ErrorKind::ExpectedValueOutput`] for non-value kinds and as
    /// [`crate::ErrorKind::ExpectedFloatType`] for bool/int value kinds.
    #[track_caller]
    pub fn as_float_or_err(self) -> crate::Result<NodeOutputType> {
        let ty = self.as_value_or_err()?;
        if ty.is_float() {
            Ok(ty)
        } else {
            Err(crate::ErrorKind::ExpectedFloatType(ty).into())
        }
    }
```

- [ ] **Step 0.3.4: Run tests to verify they pass**

Run: `cargo test -p ir node::tests::as_value_or_err_ node::tests::as_integer_or_err_ node::tests::as_float_or_err_`
Expected: 6 tests PASS.

- [ ] **Step 0.3.5: Commit**

```bash
git add crates/ir/src/node.rs
git commit -m "ir: add as_value_or_err / as_integer_or_err / as_float_or_err on NodeOutputKind"
```

---

### Task 0.4: Add `NodeKind::is_phi`

**Files:**
- Modify: `crates/ir/src/node.rs`

- [ ] **Step 0.4.1: Write failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/ir/src/node.rs`:

```rust
    #[test]
    fn is_phi_true_cases() {
        assert!(NodeKind::ControlPhi(rsleigh::Vn::constant(0, 8)).is_phi());
        assert!(NodeKind::MemPhi.is_phi());
        assert!(NodeKind::StackStorePhi { space: rsleigh::VnSpace::Ram }.is_phi());
    }

    #[test]
    fn is_phi_false_cases() {
        assert!(!NodeKind::Entry.is_phi());
        assert!(!NodeKind::InitialMemory.is_phi());
        assert!(!NodeKind::IntConst(0).is_phi());
    }
```

- [ ] **Step 0.4.2: Run tests to verify they fail**

Run: `cargo test -p ir node::tests::is_phi`
Expected: FAIL with "no method named `is_phi`".

- [ ] **Step 0.4.3: Implement `is_phi`**

Find the `impl NodeKind` block in `crates/ir/src/node.rs`. Add this method at the end of it:

```rust
    /// Returns `true` for any phi node kind: [`NodeKind::ControlPhi`],
    /// [`NodeKind::MemPhi`], or [`NodeKind::StackStorePhi`].
    pub fn is_phi(&self) -> bool {
        matches!(
            self,
            NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. }
        )
    }
```

- [ ] **Step 0.4.4: Run tests to verify they pass**

Run: `cargo test -p ir node::tests::is_phi`
Expected: 2 tests PASS.

- [ ] **Step 0.4.5: Commit**

```bash
git add crates/ir/src/node.rs
git commit -m "ir: add NodeKind::is_phi helper"
```

---

### Task 0.5: `ops::consts` — const inspection helpers on `BuiltFunctionGraph`

**Files:**
- Modify: `crates/ir/src/ops/consts.rs`
- Test: `crates/ir/src/ops/consts.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 0.5.1: Write failing tests first**

Write `crates/ir/src/ops/consts.rs`:

```rust
//! Constant-output inspection and creation helpers on
//! [`crate::function::BuiltFunctionGraph`].

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl BuiltFunctionGraph {
    /// Returns the integer constant value of `out` (masked to its declared
    /// type), or `None` if the output is not an integer constant.
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.graph.output_kind(out).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::IntConst(v) => ty.get_unsigned_int(v),
            _ => None,
        }
    }

    /// Returns the boolean constant value of `out`, or `None` if it is not a
    /// `BoolConst` node.
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool> {
        if !self.graph.output_kind(out).is_bool() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::BoolConst(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the raw bits of a float constant, or `None` if the output is
    /// not a `FloatConst` node.
    pub fn float_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.graph.output_kind(out).as_value()?;
        if !ty.is_float() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::FloatConst(bits) => Some(bits),
            _ => None,
        }
    }

    /// Creates (or retrieves from the dedup cache) an `IntConst(val)` node of
    /// type `ty` and returns its single output.
    pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::IntConst(val),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `BoolConst(val)` node.
    pub fn make_bool_const(&mut self, val: bool) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::BoolConst(val),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `FloatConst(bits)` node of float type `ty`.
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::FloatConst(bits),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::FunctionBuilder;

    fn empty_built() -> BuiltFunctionGraph {
        FunctionBuilder::new_standalone().build().unwrap()
    }

    #[test]
    fn make_and_read_int_const() {
        let mut fg = empty_built();
        let out = fg.make_int_const(0x1234, NodeOutputType::U32).unwrap();
        assert_eq!(fg.int_const_val(out), Some(0x1234));
        assert_eq!(fg.bool_const_val(out), None);
        assert_eq!(fg.float_const_val(out), None);
    }

    #[test]
    fn make_and_read_bool_const() {
        let mut fg = empty_built();
        let out = fg.make_bool_const(true).unwrap();
        assert_eq!(fg.bool_const_val(out), Some(true));
        assert_eq!(fg.int_const_val(out), None);
    }

    #[test]
    fn make_and_read_float_const() {
        let mut fg = empty_built();
        let bits = 0x4049_0fdbu64; // pi as f32 bits
        let out = fg.make_float_const(bits, NodeOutputType::F32).unwrap();
        assert_eq!(fg.float_const_val(out), Some(bits));
    }
}
```

If `FunctionBuilder::new_standalone()` does not exist in your branch, replace the helper with whatever the existing test-setup idiom is (look at existing tests in `crates/ir/src/function.rs` — the builder of the mock/smoke graph used elsewhere). The exact constructor name does not affect the plan.

- [ ] **Step 0.5.2: Run tests to verify they fail / compile-check**

Run: `cargo test -p ir ops::consts::tests`
Expected: compiles (the methods are implemented in the same step) and tests PASS. If the test helper name is wrong, the test file will fail to build — fix the helper to match the crate's actual test idiom, then rerun.

- [ ] **Step 0.5.3: Commit**

```bash
git add crates/ir/src/ops/consts.rs
git commit -m "ir: ops::consts helpers on BuiltFunctionGraph"
```

---

### Task 0.6: `ops::rewrite` — `replace_all_uses`

**Files:**
- Modify: `crates/ir/src/ops/rewrite.rs`
- Test: `crates/ir/src/ops/rewrite.rs` (inline tests)

- [ ] **Step 0.6.1: Implement with failing test first**

Write `crates/ir/src/ops/rewrite.rs`:

```rust
//! Graph-mutation helpers on [`crate::function::BuiltFunctionGraph`].

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::node::NodeOutputId;

impl BuiltFunctionGraph {
    /// Redirects every consumer of `old` to `new_val`.
    ///
    /// Returns `true` if at least one use was replaced, `false` if `old` had
    /// no uses.
    pub fn replace_all_uses(
        &mut self,
        old: NodeOutputId,
        new_val: NodeOutputId,
    ) -> Result<bool> {
        let mut cursor = self.graph.output_use_cursor(old);
        if cursor.current().is_none() {
            return Ok(false);
        }
        while cursor.current().is_some() {
            cursor.replace_current_with(new_val)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    // Exhaustively covered by `opt/` integration tests (every pass that
    // rewrites the graph exercises this path). A smoke test here would
    // require a full builder setup that's redundant with those.
}
```

- [ ] **Step 0.6.2: Verify build**

Run: `cargo build -p ir`
Expected: PASS.

- [ ] **Step 0.6.3: Commit**

```bash
git add crates/ir/src/ops/rewrite.rs
git commit -m "ir: ops::rewrite::replace_all_uses helper"
```

---

### Task 0.7: `ops::builder` — `make_value_node` + migrated conversion helpers

**Files:**
- Modify: `crates/ir/src/ops/builder.rs`

- [ ] **Step 0.7.1: Implement**

Write `crates/ir/src/ops/builder.rs`:

```rust
//! Ergonomic node-construction helpers on
//! [`crate::function::BuiltFunctionGraph`].

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl BuiltFunctionGraph {
    /// Creates (or retrieves from the dedup cache) a node with a single value
    /// output of `ty` and returns the output id directly. Shortcut for
    ///
    /// ```ignore
    /// let n = g.create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
    /// let [out] = g.node_outputs_exact::<1>(n)?;
    /// ```
    pub fn make_value_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let inputs: Vec<NodeOutputId> = inputs.into_iter().collect();
        let node = self
            .graph
            .create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }

    /// Convenience: create an `IntBitsToFloat` node with the given result type.
    pub fn make_int_bits_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.make_value_node(NodeKind::IntBitsToFloat, [input], ty)
    }

    /// Convenience: create a `FloatToFloat` node with the given result type.
    pub fn make_float_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.make_value_node(NodeKind::FloatToFloat, [input], ty)
    }
}
```

- [ ] **Step 0.7.2: Verify build**

Run: `cargo build -p ir`
Expected: PASS.

- [ ] **Step 0.7.3: Commit**

```bash
git add crates/ir/src/ops/builder.rs
git commit -m "ir: ops::builder::make_value_node + conversion helpers"
```

---

### Task 0.8: `opt::OptimizationResult::from_changed`

**Files:**
- Modify: `crates/opt/src/opt.rs`

- [ ] **Step 0.8.1: Add helper**

In `crates/opt/src/opt.rs`, locate the `impl OptimizationResult` block (or add one if absent). Add:

```rust
impl OptimizationResult {
    /// Maps the boolean return of [`BuiltFunctionGraph::replace_all_uses`] to
    /// an `OptimizationResult`: `true` → `Changed`, `false` → `NoChange`.
    pub fn from_changed(changed: bool) -> Self {
        if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }
}
```

- [ ] **Step 0.8.2: Verify build**

Run: `cargo build -p opt`
Expected: PASS.

- [ ] **Step 0.8.3: Commit**

```bash
git add crates/opt/src/opt.rs
git commit -m "opt: OptimizationResult::from_changed helper"
```

---

### Task 0.9: Migrate call-sites in `opt/src/known_bits.rs` and `opt/src/dead_branch.rs`

**Files:**
- Modify: `crates/opt/src/known_bits.rs`
- Modify: `crates/opt/src/dead_branch.rs`

- [ ] **Step 0.9.1: Replace `utils::` imports and usages in `known_bits.rs`**

In `crates/opt/src/known_bits.rs`:
- Remove any `use crate::utils::{…};` lines.
- Replace every free-function call:
  - `int_const_val(fg, x)` → `fg.int_const_val(x)`
  - `bool_const_val(fg, x)` → `fg.bool_const_val(x)`
  - `float_const_val(fg, x)` → `fg.float_const_val(x)`
  - `make_int_const(fg, v, ty)?` → `fg.make_int_const(v, ty)?`
  - `make_bool_const(fg, v)?` → `fg.make_bool_const(v)?`
  - `make_float_const(fg, bits, ty)?` → `fg.make_float_const(bits, ty)?`
  - `replace_all_uses(fg, old, new)?` → `OptimizationResult::from_changed(fg.replace_all_uses(old, new)?)`
  - `make_int_bits_to_float_node(fg, x, ty)?` → `fg.make_int_bits_to_float_node(x, ty)?`
  - `make_float_to_float_node(fg, x, ty)?` → `fg.make_float_to_float_node(x, ty)?`

Use an editor search within the file to find each occurrence.

- [ ] **Step 0.9.2: Repeat for `dead_branch.rs`**

Same mechanical substitutions in `crates/opt/src/dead_branch.rs`.

- [ ] **Step 0.9.3: Verify build**

Run: `cargo build -p opt`
Expected: PASS.

- [ ] **Step 0.9.4: Run affected tests**

Run: `cargo test -p opt`
Expected: every opt test PASS.

- [ ] **Step 0.9.5: Commit**

```bash
git add crates/opt/src/known_bits.rs crates/opt/src/dead_branch.rs
git commit -m "opt: migrate known_bits and dead_branch to ir::ops helpers"
```

---

### Task 0.10: Migrate remaining opt passes

**Files:**
- Modify: `crates/opt/src/redundant_phis.rs`
- Modify: `crates/opt/src/load_readonly.rs`
- Modify: `crates/opt/src/stack_store.rs`

- [ ] **Step 0.10.1: Mechanical substitution in each file**

Apply the same substitutions from Task 0.9.1 to each of the three files.

- [ ] **Step 0.10.2: Verify build**

Run: `cargo build -p opt`
Expected: PASS.

- [ ] **Step 0.10.3: Run tests**

Run: `cargo test -p opt`
Expected: all PASS.

- [ ] **Step 0.10.4: Commit**

```bash
git add crates/opt/src/redundant_phis.rs crates/opt/src/load_readonly.rs crates/opt/src/stack_store.rs
git commit -m "opt: migrate redundant_phis, load_readonly, stack_store to ir::ops helpers"
```

---

### Task 0.11: Delete `opt/src/utils.rs`

**Files:**
- Delete: `crates/opt/src/utils.rs`
- Modify: `crates/opt/src/lib.rs` (remove `mod utils;`)

- [ ] **Step 0.11.1: Verify `utils` has no remaining importers**

Run: `cargo build -p opt`. If the build passes without the utils usages, proceed. Also sanity-check with Grep: search `crates/opt/src/` for `utils::` — there must be zero matches.

- [ ] **Step 0.11.2: Delete the file and its `mod` declaration**

- `git rm crates/opt/src/utils.rs`
- In `crates/opt/src/lib.rs`, remove the line `mod utils;`.

- [ ] **Step 0.11.3: Build & test**

Run: `cargo build -p opt && cargo test -p opt`
Expected: all PASS.

- [ ] **Step 0.11.4: Semantic baseline check**

Run: `scripts/compare-analyzer-output.sh`
Expected: `analyzer output semantically equivalent to baseline`.

- [ ] **Step 0.11.5: Commit**

```bash
git add crates/opt/src/lib.rs crates/opt/src/utils.rs
git commit -m "opt: delete utils.rs; contents live in ir::ops"
```

---

### Task 0.12: Phase 0 final sanity pass

- [ ] **Step 0.12.1: Run the full workspace tests**

Run: `cargo test --workspace`
Expected: all PASS.

- [ ] **Step 0.12.2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 0.12.3: Run analyzer-output semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: `analyzer output semantically equivalent to baseline`.

No commit — this is a gate task.

---

## Phase 1 — `NodeOutputType` reflection table

### Task 1.1: Reflection table with coverage test

**Files:**
- Modify: `crates/ir/src/node.rs`

- [ ] **Step 1.1.1: Add variant-order coverage test**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn type_info_table_matches_variants() {
        // Table indices must match discriminant order. Enumerate every variant
        // explicitly and check `info().name` / category.
        let cases: &[(NodeOutputType, &str, usize, bool, bool, bool)] = &[
            (NodeOutputType::Bool, "bool", 1, false, true, false),
            (NodeOutputType::U8,   "u8",   1, true,  false, false),
            (NodeOutputType::U16,  "u16",  2, true,  false, false),
            (NodeOutputType::U32,  "u32",  4, true,  false, false),
            (NodeOutputType::U64,  "u64",  8, true,  false, false),
            (NodeOutputType::U128, "u128", 16, true, false, false),
            (NodeOutputType::U256, "u256", 32, true, false, false),
            (NodeOutputType::F32,  "f32",  4, false, false, true),
            (NodeOutputType::F64,  "f64",  8, false, false, true),
        ];
        for (ty, name, size, is_int, is_bool, is_float) in cases {
            assert_eq!(ty.as_str(), *name);
            assert_eq!(ty.byte_size(), *size);
            assert_eq!(ty.bit_width(), *size * 8);
            assert_eq!(ty.is_integer(), *is_int);
            assert_eq!(ty.is_bool(), *is_bool);
            assert_eq!(ty.is_float(), *is_float);
        }
    }
```

- [ ] **Step 1.1.2: Run test to verify it currently passes against the hand-written matches**

Run: `cargo test -p ir node::tests::type_info_table_matches_variants`
Expected: PASS (the test is an oracle — it should pass both before and after the refactor).

- [ ] **Step 1.1.3: Replace the six matches with a table**

In `crates/ir/src/node.rs`, locate the `NodeOutputType` enum. Immediately below it, **replace** the existing `byte_size`, `bit_width`, `as_str`, `is_integer`, `is_bool`, `is_float` bodies with a table-driven implementation. The exact replacement block:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeOutputTypeCategory {
    Bool,
    Int,
    Float,
}

struct TypeInfo {
    name: &'static str,
    byte_size: u8,
    category: NodeOutputTypeCategory,
}

// Order MUST match the `NodeOutputType` enum declaration order
// (asserted by `type_info_table_matches_variants`).
const TYPE_INFO: &[TypeInfo] = &[
    TypeInfo { name: "bool", byte_size: 1,  category: NodeOutputTypeCategory::Bool  },
    TypeInfo { name: "u8",   byte_size: 1,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u16",  byte_size: 2,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u32",  byte_size: 4,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u64",  byte_size: 8,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u128", byte_size: 16, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u256", byte_size: 32, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "f32",  byte_size: 4,  category: NodeOutputTypeCategory::Float },
    TypeInfo { name: "f64",  byte_size: 8,  category: NodeOutputTypeCategory::Float },
];

impl NodeOutputType {
    #[inline]
    fn info(self) -> &'static TypeInfo {
        &TYPE_INFO[self as usize]
    }

    #[inline]
    pub fn as_str(self) -> &'static str {
        self.info().name
    }

    #[inline]
    pub fn byte_size(self) -> usize {
        self.info().byte_size as usize
    }

    #[inline]
    pub fn bit_width(self) -> usize {
        self.byte_size() * 8
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Bool)
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Int)
    }

    #[inline]
    pub fn is_float(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Float)
    }
}
```

Delete the previous hand-written bodies for these six methods. Leave `get_unsigned_int`, `get_signed_int`, and `to_natural_int_type` untouched.

- [ ] **Step 1.1.4: Verify both build and coverage test**

Run: `cargo test -p ir node::tests::type_info_table_matches_variants`
Expected: PASS.

Run: `cargo test -p ir`
Expected: every existing ir test PASS.

- [ ] **Step 1.1.5: Semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: `analyzer output semantically equivalent to baseline`.

- [ ] **Step 1.1.6: Commit**

```bash
git add crates/ir/src/node.rs
git commit -m "ir: table-drive NodeOutputType type info"
```

---

## Phase 2 — `rewrite_rules!` proc-macro and `constant_fold` rewrite

Phase 2 is the marquee phase. It is split into three sub-phases:
- **2a**: de-risk spike (three rules end-to-end) on a branch of the macro.
- **2b**: grammar expansion, feature by feature, each with a trybuild test.
- **2c**: rewrite `constant_fold.rs` rule by rule.

### Task 2.0: Spike — three-rule proof of concept

**Files:**
- Create: `crates/ir-macros/src/rewrite_rules/mod.rs`
- Create: `crates/ir-macros/src/rewrite_rules/parse.rs`
- Create: `crates/ir-macros/src/rewrite_rules/lhs.rs`
- Create: `crates/ir-macros/src/rewrite_rules/rhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_spike.rs`
- Modify: `crates/ir-macros/src/lib.rs`

The spike implements three representative rules end-to-end to prove the design:
1. `(x + IntConst(0)) => x` — bare output capture, int-const literal, RHS output passthrough.
2. `((a & IntConst(c1)) & IntConst(c2)) => a & int_const(c1 & c2, ty)` — nested match, multiple int-const captures, RHS operator-overload node build.
3. `Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty)` — parametrised kind, input-type binding.

If the spike succeeds, Phase 2 continues with broader grammar. If any of the three hit a wall, re-scope before continuing (the spec calls this out explicitly).

- [ ] **Step 2.0.1: Scaffold the `rewrite_rules` submodule**

Edit `crates/ir-macros/src/lib.rs` — near the top, below the existing `match_value!` entry point, add:

```rust
mod rewrite_rules;

#[proc_macro]
pub fn rewrite_rules(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    rewrite_rules::expand(input.into()).into()
}
```

Create `crates/ir-macros/src/rewrite_rules/mod.rs`:

```rust
//! `rewrite_rules!` proc-macro DSL.
//!
//! Grammar and semantics: see `docs/superpowers/specs/2026-04-17-code-quality-refactor-design.md`.

use proc_macro2::TokenStream;
use quote::quote;

mod lhs;
mod parse;
mod rhs;

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    match try_expand(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error(),
    }
}

fn try_expand(input: TokenStream) -> syn::Result<TokenStream> {
    let rules: parse::Rules = syn::parse2(input)?;
    let rule_fns = rules.rules.iter().enumerate().map(|(i, r)| {
        let name = quote::format_ident!("__rewrite_rule_{}", i);
        r.codegen(&name)
    }).collect::<syn::Result<Vec<_>>>()?;

    let dispatch_calls = rules.rules.iter().enumerate().map(|(i, _)| {
        let name = quote::format_ident!("__rewrite_rule_{}", i);
        quote! { if let ir::opt::OptimizationResult::Changed = #name(fg, node)? { changed = true; } }
    });

    Ok(quote! {
        {
            // Emitted by `rewrite_rules!`. Defines per-rule functions and a
            // single dispatcher `__rewrite_rules_apply` callable by the pass.
            #( #rule_fns )*
            #[allow(non_snake_case)]
            fn __rewrite_rules_apply(
                fg: &mut ir::function::BuiltFunctionGraph,
                node: ir::node::NodeId,
            ) -> ::core::result::Result<opt::OptimizationResult, opt::Error> {
                let mut changed = false;
                #( #dispatch_calls )*
                Ok(opt::OptimizationResult::from_changed(changed))
            }
            __rewrite_rules_apply
        }
    })
}
```

Create empty stubs in `parse.rs`, `lhs.rs`, `rhs.rs` — they will be filled in as the macro grows:

```rust
// crates/ir-macros/src/rewrite_rules/parse.rs
use syn::parse::{Parse, ParseStream};

pub(super) struct Rules { pub rules: Vec<Rule> }
pub(super) struct Rule { /* LHS, RHS, where-guard, escape-hatch */ }

impl Parse for Rules {
    fn parse(_input: ParseStream) -> syn::Result<Self> { todo!("implement in 2.0.2") }
}

impl Rule {
    pub fn codegen(&self, _name: &proc_macro2::Ident) -> syn::Result<proc_macro2::TokenStream> {
        todo!("implement in 2.0.3")
    }
}
```

```rust
// crates/ir-macros/src/rewrite_rules/lhs.rs
// intentionally empty; filled in 2.0.3
```

```rust
// crates/ir-macros/src/rewrite_rules/rhs.rs
// intentionally empty; filled in 2.0.3
```

- [ ] **Step 2.0.2: Write failing spike test for rule 1**

Write `crates/ir-macros/tests/rewrite_rules_spike.rs`:

```rust
//! End-to-end spike: three rules through the `rewrite_rules!` macro.
//!
//! These prove the design end-to-end. If any fails, Phase 2 is re-scoped.

use ir::function::FunctionBuilder;
use ir::node::NodeOutputType;
use ir::ops::{IntBinaryOp, ExtendOp};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn rule_1_add_zero_identity() {
    let mut fb = FunctionBuilder::new_standalone();
    let a = fb.graph.build_initial_var_for_test(NodeOutputType::U32);
    let zero = fb.graph.build_int_const_for_test(0, NodeOutputType::U32);
    let add = fb.graph.build_int_add_for_test(a, zero, NodeOutputType::U32);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        (x + IntConst(0)) => x,
    };
    let node_of_add = fg.graph.get_node_from_output(add);
    let res = apply(&mut fg, node_of_add).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
    // After the rewrite, `add`'s use should have been replaced by `a`.
    // (The node itself stays in the arena; its users have been redirected.)
    // Concretely: `add` should have no users after the rewrite.
    assert!(fg.graph.output_use_cursor(add).current().is_none());
}
```

(The exact test-setup helpers — `build_initial_var_for_test`, `build_int_const_for_test`, `build_int_add_for_test` — may not all exist. Before running, grep existing ir tests for whatever is used there and substitute. The point of the test is: construct an `Add(a, IntConst(0))`, apply the rule, assert the user list of the `Add` output is now empty.)

- [ ] **Step 2.0.3: Implement parser + LHS + RHS to make rule 1 pass**

This is the bulk of the spike. Implement in `parse.rs` / `lhs.rs` / `rhs.rs`:

- `Rules::parse`: read a comma-separated list of `Rule`s until end-of-stream.
- `Rule::parse`: parse `<LhsPattern> => <RhsExpr>`, optionally preceded by `where <Expr>`. Emit spans for each sub-element.
- `LhsPattern` variants needed for rule 1:
  - `OutputCapture(Ident)` — bare identifier.
  - `IntConstLiteral(u64)` — `IntConst(<literal>)`.
  - `IntBinaryOp { op: IntBinaryOp, lhs: Box<LhsPattern>, rhs: Box<LhsPattern>, commutative: bool }`.
- `RhsExpr` variants needed: bare output reference (forwards a captured `NodeOutputId`).
- LHS codegen for a rule produces:
  ```rust
  fn __rewrite_rule_0(fg: &mut BuiltFunctionGraph, node: NodeId) -> Result<OptimizationResult> {
      let kind = fg.graph.node_kind(node);
      let NodeKind::IntBinaryOp(IntBinaryOp::Add) = *kind else { return Ok(OptimizationResult::NoChange); };
      let inputs = fg.graph.node_inputs_exact::<2>(node)?;
      // Commutative: try both orderings.
      for (l_idx, r_idx) in [(0usize, 1usize), (1, 0)] {
          let l = inputs[l_idx];
          let r = inputs[r_idx];
          // LHS-left: OutputCapture x → bind l as x.
          let x = l;
          // LHS-right: IntConstLiteral(0) → read the int-const value of r.
          let Some(rhs_const) = fg.int_const_val(r) else { continue; };
          if rhs_const != 0 { continue; }
          // Where clause: (none)
          // RHS: x
          let [root_out] = fg.graph.node_outputs_exact::<1>(node)?;
          let new_out = x;
          let changed = fg.replace_all_uses(root_out, new_out)?;
          return Ok(OptimizationResult::from_changed(changed));
      }
      Ok(OptimizationResult::NoChange)
  }
  ```
  (Hand-verified against the rule syntax; macro emits this shape.)
- RHS codegen: for rule 1 it's just the identifier `x`, emits `x`.

Keep the per-sub-element spans so errors like "undeclared identifier on RHS" point to the offending identifier.

- [ ] **Step 2.0.4: Run rule 1 test**

Run: `cargo test -p ir-macros --test rewrite_rules_spike rule_1_add_zero_identity`
Expected: PASS.

- [ ] **Step 2.0.5: Extend LHS/RHS to support rule 2**

Add LHS features:
- Nested `IntBinaryOp` (recursive — already supported).
- Capture of the `IntConst`'s value: `IntConst(c1)` binds `c1: u64`.

Add RHS features:
- `int_const(<expr>, ty)` builder form. The macro must emit a call to `fg.make_int_const(<expr>, ty)?`. `ty` is an in-scope identifier bound by the outer match to the matched node's output type.
- `&` operator between a `NodeOutputId` and another `NodeOutputId`: emits `fg.make_value_node(NodeKind::IntBinaryOp(IntBinaryOp::And), [l, r], ty)?`.
- `&` operator between two `u64`-typed captures: emits a plain `u64` `&` expression.

The macro decides per-subexpression which form to emit by classifying each identifier as "value-bound" (`u64` / `bool` from `IntConst`/`BoolConst`) or "output-bound" (`NodeOutputId` from a bare capture). An `&` whose operands are all value-bound lowers to plain Rust; any output-bound operand forces the `make_value_node` lowering.

Add test:

```rust
#[test]
fn rule_2_nested_and_mask_merge() {
    let mut fb = FunctionBuilder::new_standalone();
    let a = fb.graph.build_initial_var_for_test(NodeOutputType::U32);
    let c1 = fb.graph.build_int_const_for_test(0xF0, NodeOutputType::U32);
    let c2 = fb.graph.build_int_const_for_test(0x0F, NodeOutputType::U32);
    let inner = fb.graph.build_int_and_for_test(a, c1, NodeOutputType::U32);
    let outer = fb.graph.build_int_and_for_test(inner, c2, NodeOutputType::U32);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        ((a & IntConst(c1)) & IntConst(c2)) => a & int_const(c1 & c2, ty),
    };
    let outer_node = fg.graph.get_node_from_output(outer);
    let res = apply(&mut fg, outer_node).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
    // The outer output now has no uses (it's been replaced). There should
    // exist a new `And(a, IntConst(0))` elsewhere that is the replacement.
}
```

- [ ] **Step 2.0.6: Run rule 2 test**

Run: `cargo test -p ir-macros --test rewrite_rules_spike rule_2_nested_and_mask_merge`
Expected: PASS.

- [ ] **Step 2.0.7: Extend LHS/RHS to support rule 3**

Add LHS features:
- `Extend::<SignExtend>(<inner>)` — kind with generic-style parameter; matches `NodeKind::Extend(ExtendOp::SignExtend)`. Also `Extend::<ZeroExtend>`.
- `: in_ty` input-type binding suffix: after matching an LHS subpattern, bind the subpattern's output's type as `in_ty: NodeOutputType`.

Add RHS features: none new — uses `int_const(<u64-expr>, ty)` already supported.

Test:

```rust
#[test]
fn rule_3_sign_extend_constant() {
    let mut fb = FunctionBuilder::new_standalone();
    let c = fb.graph.build_int_const_for_test(0xFF, NodeOutputType::U8);
    let ext = fb.graph.build_extend_for_test(ExtendOp::SignExtend, c, NodeOutputType::U32);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty),
    };
    let ext_node = fg.graph.get_node_from_output(ext);
    let res = apply(&mut fg, ext_node).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
    // The ext output was replaced by an IntConst(0xFFFF_FFFF, U32).
}
```

You will need `NodeOutputType::sign_extend(self, val: u64) -> u64` — check if it exists on the type; if not, add it (likely already present, given `get_signed_int` and friends are there).

- [ ] **Step 2.0.8: Run rule 3 test**

Run: `cargo test -p ir-macros --test rewrite_rules_spike rule_3_sign_extend_constant`
Expected: PASS.

- [ ] **Step 2.0.9: Spike gate**

Run: `cargo test -p ir-macros --test rewrite_rules_spike`
Expected: three tests PASS.

If any test is still red at this point, **stop and re-plan** per the spec's Phase 2 Risks section.

- [ ] **Step 2.0.10: Commit the spike**

```bash
git add crates/ir-macros/src/lib.rs crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_spike.rs
git commit -m "ir-macros: rewrite_rules spike (three rules end-to-end)"
```

---

### Task 2.1: Grammar — bool-const and float-const captures

**Files:**
- Modify: `crates/ir-macros/src/rewrite_rules/parse.rs`
- Modify: `crates/ir-macros/src/rewrite_rules/lhs.rs`
- Modify: `crates/ir-macros/src/rewrite_rules/rhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_const_captures.rs`

- [ ] **Step 2.1.1: Write failing test for `BoolConst(b)`**

`crates/ir-macros/tests/rewrite_rules_const_captures.rs`:

```rust
use ir::function::FunctionBuilder;
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn bool_const_capture() {
    let mut fb = FunctionBuilder::new_standalone();
    let t = fb.graph.build_bool_const_for_test(true);
    let b = fb.graph.build_bool_and_for_test(t, t);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        BAnd(BoolConst(true), x) => x,
    };
    let n = fg.graph.get_node_from_output(b);
    let res = apply(&mut fg, n).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
}
```

- [ ] **Step 2.1.2: Run: expected FAIL (`BoolConst` / `BAnd` not yet parsed)**

Run: `cargo test -p ir-macros --test rewrite_rules_const_captures`
Expected: FAIL with parse error.

- [ ] **Step 2.1.3: Extend LHS parser**

In `parse.rs`: add `BoolConstLiteral(bool)`, `BoolConstCapture(Ident)`, `FloatConstLiteral(u64)`, `FloatConstCapture(Ident)`. Also add `BoolBinaryOp { op: BoolBinaryOp, l, r, commutative: true }` for `BAnd`/`BOr`/`BXor` function-form.

In `lhs.rs`: emit matching code for each — for `BoolConst`, check `fg.bool_const_val(out)`; for `FloatConst`, check `fg.float_const_val(out)`. For `BoolBinaryOp` function form, match `NodeKind::BoolBinaryOp(BoolBinaryOp::<Variant>)` and recurse.

- [ ] **Step 2.1.4: Run test**

Run: `cargo test -p ir-macros --test rewrite_rules_const_captures`
Expected: PASS.

- [ ] **Step 2.1.5: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_const_captures.rs
git commit -m "ir-macros: rewrite_rules support BoolConst / FloatConst / BAnd/BOr/BXor"
```

---

### Task 2.2: Grammar — float operations (FAdd/FSub/FMul/FDiv, FEq/FNe/FLt/FLe)

**Files:**
- Modify: `crates/ir-macros/src/rewrite_rules/parse.rs`
- Modify: `crates/ir-macros/src/rewrite_rules/lhs.rs`
- Modify: `crates/ir-macros/src/rewrite_rules/rhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_float.rs`

- [ ] **Step 2.2.1: Failing test**

```rust
// crates/ir-macros/tests/rewrite_rules_float.rs
use ir::function::FunctionBuilder;
use ir::node::NodeOutputType;
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn float_add_zero_identity() {
    let mut fb = FunctionBuilder::new_standalone();
    let x = fb.graph.build_initial_var_for_test(NodeOutputType::F32);
    let zero = fb.graph.build_float_const_for_test(0, NodeOutputType::F32);
    let add = fb.graph.build_float_add_for_test(x, zero, NodeOutputType::F32);
    let mut fg = fb.build().unwrap();

    // Note: f_add(x, 0) is NOT an identity in IEEE-754 (−0 vs +0), so the
    // rule is deliberately narrow. This test just proves parser+codegen.
    let apply = rewrite_rules! {
        FAdd(x, FloatConst(0)) => x,
    };
    let n = fg.graph.get_node_from_output(add);
    let res = apply(&mut fg, n).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
}
```

- [ ] **Step 2.2.2: Run: expected FAIL**

Run: `cargo test -p ir-macros --test rewrite_rules_float`
Expected: FAIL.

- [ ] **Step 2.2.3: Extend parser and codegen**

Add `FloatBinaryOp(op, l, r, commutative: op ∈ {Add, Mul})` and `FloatCmpOp(op, l, r, commutative: op ∈ {Equal, NotEqual})` to the LHS enum and its codegen. Also support RHS builder `float_const(<bits-expr>, ty)` lowering to `fg.make_float_const(bits, ty)?`.

- [ ] **Step 2.2.4: Run test**

Run: `cargo test -p ir-macros --test rewrite_rules_float`
Expected: PASS.

- [ ] **Step 2.2.5: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_float.rs
git commit -m "ir-macros: rewrite_rules float ops (FAdd/FSub/FMul/FDiv, FEq/…)"
```

---

### Task 2.3: Grammar — integer comparisons (IntEq/IntLt/IntLe/IntSlt/IntSle/IntCarry/IntBorrow/IntScarry/IntSborrow)

**Files:**
- Modify: `parse.rs`, `lhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_intcmp.rs`

- [ ] **Step 2.3.1: Failing test**

```rust
// crates/ir-macros/tests/rewrite_rules_intcmp.rs
use ir::function::FunctionBuilder;
use ir::node::NodeOutputType;
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn int_eq_constant_equality() {
    let mut fb = FunctionBuilder::new_standalone();
    let c1 = fb.graph.build_int_const_for_test(5, NodeOutputType::U32);
    let c2 = fb.graph.build_int_const_for_test(5, NodeOutputType::U32);
    let eq = fb.graph.build_int_eq_for_test(c1, c2);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        IntEq(IntConst(l), IntConst(r)) => bool_const(l == r),
    };
    let n = fg.graph.get_node_from_output(eq);
    let res = apply(&mut fg, n).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
}
```

- [ ] **Step 2.3.2: Extend parser/codegen**

Each cmp name maps to `NodeKind::IntCmpOp(IntCmpOp::<Variant>)`. Commutativity: `IntEq` commutative, others ordered.

- [ ] **Step 2.3.3: Run test**

Run: `cargo test -p ir-macros --test rewrite_rules_intcmp`
Expected: PASS.

- [ ] **Step 2.3.4: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_intcmp.rs
git commit -m "ir-macros: rewrite_rules int comparisons"
```

---

### Task 2.4: Grammar — `where` clause post-match guard

**Files:**
- Modify: `parse.rs`, `lhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_where.rs`

- [ ] **Step 2.4.1: Failing test**

```rust
// crates/ir-macros/tests/rewrite_rules_where.rs
use ir::function::FunctionBuilder;
use ir::node::NodeOutputType;
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn where_clause_rejects_mismatch() {
    let mut fb = FunctionBuilder::new_standalone();
    let c1 = fb.graph.build_int_const_for_test(3, NodeOutputType::U32);
    let c2 = fb.graph.build_int_const_for_test(5, NodeOutputType::U32);
    let add = fb.graph.build_int_add_for_test(c1, c2, NodeOutputType::U32);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        (IntConst(l) + IntConst(r)) where ty.fits_u64()
            => int_const(l.wrapping_add(r), ty),
    };
    let n = fg.graph.get_node_from_output(add);
    let res = apply(&mut fg, n).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
}
```

(If `NodeOutputType::fits_u64` doesn't exist, add it: `self.byte_size() <= 8`.)

- [ ] **Step 2.4.2: Parse `where <Expr>`**

After the RHS arrow, allow optional `where <Expr>`. Emit the expr after the captures are bound and before the RHS is built; if it evaluates `false`, skip the current candidate (for commutative ops, this means "try the other ordering").

- [ ] **Step 2.4.3: Run test**

Run: `cargo test -p ir-macros --test rewrite_rules_where`
Expected: PASS.

- [ ] **Step 2.4.4: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_where.rs
git commit -m "ir-macros: rewrite_rules where-clause"
```

---

### Task 2.5: Grammar — escape hatch `name @ fn_name`

**Files:**
- Modify: `parse.rs`, `lhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_escape.rs`

- [ ] **Step 2.5.1: Failing test**

```rust
// crates/ir-macros/tests/rewrite_rules_escape.rs
use ir::function::{BuiltFunctionGraph, FunctionBuilder};
use ir::node::NodeId;
use ir_macros::rewrite_rules;
use opt::{Error, OptimizationResult, Result};

fn always_nochange(_fg: &mut BuiltFunctionGraph, _node: NodeId) -> Result<OptimizationResult> {
    Ok(OptimizationResult::NoChange)
}

#[test]
fn escape_hatch_is_called() {
    let mut fg = FunctionBuilder::new_standalone().build().unwrap();
    let apply = rewrite_rules! {
        nop @ always_nochange,
    };
    // Any node id works; the helper ignores it.
    let entry = fg.graph.entry_node();
    let res = apply(&mut fg, entry).unwrap();
    assert_eq!(res, OptimizationResult::NoChange);
}
```

- [ ] **Step 2.5.2: Parse `<Ident> @ <Ident>`**

A Rule is either a pattern-rule (`LHS => RHS [where …]`) or an escape-rule (`<label> @ <fn_name>`). Codegen for escape: the per-rule function simply calls `fn_name(fg, node)`.

- [ ] **Step 2.5.3: Run test**

Run: `cargo test -p ir-macros --test rewrite_rules_escape`
Expected: PASS.

- [ ] **Step 2.5.4: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_escape.rs
git commit -m "ir-macros: rewrite_rules escape-hatch syntax"
```

---

### Task 2.6: Grammar — remaining unary kinds (Truncate, Popcount, Lzcount, CastToBool, CastToInt)

**Files:**
- Modify: `parse.rs`, `lhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_unary.rs`

- [ ] **Step 2.6.1: Failing test covering all five**

```rust
// crates/ir-macros/tests/rewrite_rules_unary.rs
use ir::function::FunctionBuilder;
use ir::node::NodeOutputType;
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

#[test]
fn popcount_on_constant() {
    let mut fb = FunctionBuilder::new_standalone();
    let c = fb.graph.build_int_const_for_test(0b1101, NodeOutputType::U32);
    let pc = fb.graph.build_popcount_for_test(c, NodeOutputType::U32);
    let mut fg = fb.build().unwrap();

    let apply = rewrite_rules! {
        Popcount(IntConst(v)) => int_const(v.count_ones() as u64, ty),
    };
    let n = fg.graph.get_node_from_output(pc);
    assert_eq!(apply(&mut fg, n).unwrap(), OptimizationResult::Changed);
}
```

Repeat the same shape for `Truncate`, `Lzcount`, `CastToBool`, `CastToInt` (each as a separate test function) — write them all in this file before implementation.

- [ ] **Step 2.6.2: Extend parser**

Each kind name parses to an `LhsPattern::Unary { kind: UnaryKind, operand }` where `UnaryKind` has variants `Truncate`/`Popcount`/`Lzcount`/`CastToBool`/`CastToInt`. Codegen matches `NodeKind::<Variant>` and recurses into the operand.

- [ ] **Step 2.6.3: Run tests**

Run: `cargo test -p ir-macros --test rewrite_rules_unary`
Expected: all PASS.

- [ ] **Step 2.6.4: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_unary.rs
git commit -m "ir-macros: rewrite_rules unary kinds (Truncate/Popcount/Lzcount/CastToBool/CastToInt)"
```

---

### Task 2.7: Grammar — Extend with both sign variants, Piece, Extract, Insert, Int/Float conversions

**Files:**
- Modify: `parse.rs`, `lhs.rs`
- Create: `crates/ir-macros/tests/rewrite_rules_misc_kinds.rs`

- [ ] **Step 2.7.1: Failing tests**

Cover in order: `Extend::<ZeroExtend>`, `Piece(hi, lo)`, `Extract::<lsb, len>(x)`, `Insert::<lsb, len>(base, v)`, `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`. One rule per test, each rule does a trivial rewrite (e.g. identity-replacement on a constant).

- [ ] **Step 2.7.2: Extend parser**

- `Extend::<Ident>(x)` — two variants: `ZeroExtend`/`SignExtend`. Already handled in spike; generalise.
- `Extract::<<lit>, <lit>>(x)` — matches `NodeKind::Extract { lsb, len }`, takes two integer literals.
- `Insert::<<lit>, <lit>>(base, v)`.
- `Piece(hi, lo)` — binary, non-commutative.
- Conversions (`IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`) — unary.

- [ ] **Step 2.7.3: Run tests**

Run: `cargo test -p ir-macros --test rewrite_rules_misc_kinds`
Expected: all PASS.

- [ ] **Step 2.7.4: Commit**

```bash
git add crates/ir-macros/src/rewrite_rules/ crates/ir-macros/tests/rewrite_rules_misc_kinds.rs
git commit -m "ir-macros: rewrite_rules Extend/Piece/Extract/Insert/conversion kinds"
```

---

### Task 2.8: `trybuild` compile-fail coverage for grammar errors

**Files:**
- Modify: `crates/ir-macros/Cargo.toml` (dev-dep `trybuild`, if absent)
- Create: `crates/ir-macros/tests/trybuild.rs`
- Create: `crates/ir-macros/tests/ui/rewrite_rules/undeclared_rhs_ident.rs` and `.stderr`
- Create: `crates/ir-macros/tests/ui/rewrite_rules/value_vs_output_misuse.rs` and `.stderr`
- Create: `crates/ir-macros/tests/ui/rewrite_rules/unknown_kind_name.rs` and `.stderr`

- [ ] **Step 2.8.1: Add trybuild dev-dep**

In `crates/ir-macros/Cargo.toml` under `[dev-dependencies]`:

```toml
trybuild = "1"
```

- [ ] **Step 2.8.2: Add trybuild harness**

```rust
// crates/ir-macros/tests/trybuild.rs
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/rewrite_rules/*.rs");
}
```

- [ ] **Step 2.8.3: Write failing-expansion cases**

`tests/ui/rewrite_rules/undeclared_rhs_ident.rs`:

```rust
use ir_macros::rewrite_rules;
fn main() {
    let _ = rewrite_rules! {
        (x + IntConst(0)) => y,
    };
}
```

Expected `.stderr` (copy from the first failing run; trybuild prints the canonical error).

Similar files for:
- `value_vs_output_misuse.rs`: uses an `IntConst`-bound value identifier as if it were a `NodeOutputId` (e.g. `c1 & x` where `c1` is from `IntConst(c1)` and `x` is an output).
- `unknown_kind_name.rs`: uses a kind name the DSL doesn't know (`FooBar(x)`).

- [ ] **Step 2.8.4: First trybuild run to generate .stderr**

Run: `TRYBUILD=overwrite cargo test -p ir-macros --test trybuild`
Expected: writes the `.stderr` files. Verify each message is readable and points at the offending span. If a message is cryptic, improve it in the macro code before locking in.

- [ ] **Step 2.8.5: Freeze trybuild output**

Run (without TRYBUILD=overwrite): `cargo test -p ir-macros --test trybuild`
Expected: PASS.

- [ ] **Step 2.8.6: Commit**

```bash
git add crates/ir-macros/Cargo.toml crates/ir-macros/tests/trybuild.rs crates/ir-macros/tests/ui/
git commit -m "ir-macros: trybuild cases for rewrite_rules grammar errors"
```

---

### Task 2.9: `constant_fold.rs` rewrite — part 1, algebraic identities

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.9.1: Identify the identity rules**

Read `crates/opt/src/constant_fold.rs` and list every "X op 0 → X" / "X op X → 0" / absorption rule. Expected set (from the spec):

- `x + 0 → x`
- `x - 0 → x`
- `x - x → 0`
- `x ^ x → 0`
- `x ^ 0 → x`
- `x * 0 → 0`
- `x * 1 → x`
- `x & 0 → 0`
- `x & x → x`
- `x | 0 → x`
- `x | x → x`

Verify this matches the code; include any other identities present.

- [ ] **Step 2.9.2: Replace with `rewrite_rules!`**

In `constant_fold.rs`, replace the corresponding hand-written `try_fold_*` functions with a single `rewrite_rules!` invocation building an `apply` function. Structure:

```rust
use ir::ops::IntBinaryOp::*;
use ir_macros::rewrite_rules;

fn apply_identity_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    let rules = rewrite_rules! {
        (x + IntConst(0)) => x,
        (x - IntConst(0)) => x,
        (x - x)           => int_const(0, ty),
        (x ^ x)           => int_const(0, ty),
        (x ^ IntConst(0)) => x,
        (x * IntConst(0)) => int_const(0, ty),
        (x * IntConst(1)) => x,
        (x & IntConst(0)) => int_const(0, ty),
        (x & x)           => x,
        (x | IntConst(0)) => x,
        (x | x)           => x,
    };
    rules(fg, node)
}
```

(The macro emits a closure/function value captured in `rules`; rename as the spec's `__rewrite_rules_apply` convention.)

In the main `ConstantFold::run` loop, dispatch to `apply_identity_rules` as one of the per-node steps alongside the remaining hand-written `try_fold_*` helpers (which will be rewritten in later tasks).

- [ ] **Step 2.9.3: Run existing constant_fold tests**

Run: `cargo test -p opt constant_fold`
Expected: every existing identity test PASSES unchanged.

- [ ] **Step 2.9.4: Semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: `analyzer output semantically equivalent to baseline`.

- [ ] **Step 2.9.5: Commit**

```bash
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: convert algebraic identities to rewrite_rules!"
```

---

### Task 2.10: `constant_fold.rs` — part 2, full int constant evaluation

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.10.1: Enumerate fold sites**

List every `try_fold_*` in the current `constant_fold.rs` that does full int constant evaluation — every arithmetic, bitwise, shift, comparison. Expected set:

Arithmetic: `Add`, `Sub`, `Mul`, `Div`, `Sdiv`, `Rem`, `Srem`.
Bitwise: `And`, `Or`, `Xor`.
Shifts: `ShiftLeft`, `ShiftRight`, `SShiftRight`.
Comparisons: `IntEq`, `IntLt`, `IntLe`, `IntSlt`, `IntSle`, `IntCarry`, `IntBorrow`, `IntScarry`, `IntSborrow`.
Unary: `Neg`, `Not`, `Popcount`, `Lzcount`.

- [ ] **Step 2.10.2: Add rules for each**

Extend the `rewrite_rules!` block (or split into a second block — the macro produces a function value so two blocks are fine). Example rule shapes:

```rust
// Arithmetic
(IntConst(l) + IntConst(r))      => int_const(l.wrapping_add(r), ty),
(IntConst(l) - IntConst(r))      => int_const(l.wrapping_sub(r), ty),
(IntConst(l) * IntConst(r))      => int_const(l.wrapping_mul(r), ty),
// Division guards against r == 0
(IntConst(l) / IntConst(r)) where r != 0
    => int_const(l / r, ty),
// Bitwise
(IntConst(l) & IntConst(r))      => int_const(l & r, ty),
(IntConst(l) | IntConst(r))      => int_const(l | r, ty),
(IntConst(l) ^ IntConst(r))      => int_const(l ^ r, ty),
// Shifts (right operand width is the shift amount, capped by ty.bit_width())
(IntConst(l) << IntConst(r)) where (r as u32) < ty.bit_width() as u32
    => int_const(l.wrapping_shl(r as u32), ty),
// Comparisons
IntEq(IntConst(l), IntConst(r))  => bool_const(l == r),
IntLt(IntConst(l), IntConst(r))  => bool_const(l < r),
// Unary
Popcount(IntConst(v))            => int_const(v.count_ones() as u64, ty),
Lzcount(IntConst(v) : in_ty)
    => int_const(v.leading_zeros() as u64 - (64 - in_ty.bit_width() as u32) as u64, ty),
```

(Copy the masking / extension semantics from the current hand-written bodies exactly. Division-by-zero: match the current behaviour — some call sites return `NoChange`, others propagate; test expectations in the current test suite are the oracle.)

- [ ] **Step 2.10.3: Delete the hand-written `try_fold_*` functions replaced by the rules**

For each rule added, delete the corresponding `try_fold_*` function and its dispatch call in `ConstantFold::run`.

- [ ] **Step 2.10.4: Run tests**

Run: `cargo test -p opt constant_fold`
Expected: every existing test PASSES. If any fail, the rule body's semantics differ — compare to the hand-written body, fix.

- [ ] **Step 2.10.5: Semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: PASS.

- [ ] **Step 2.10.6: Commit**

```bash
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: convert full int constant evaluation to rewrite_rules!"
```

---

### Task 2.11: `constant_fold.rs` — part 3, bool and float constant evaluation

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.11.1: Add rules for bool and float**

Bool:
```rust
BAnd(BoolConst(l), BoolConst(r)) => bool_const(l && r),
BOr(BoolConst(l), BoolConst(r))  => bool_const(l || r),
BXor(BoolConst(l), BoolConst(r)) => bool_const(l ^ r),
BAnd(BoolConst(false), _)        => bool_const(false),
BOr(BoolConst(true), _)          => bool_const(true),
```

Float (only bit-exact rules — no algebraic identities because IEEE-754):
```rust
FAdd(FloatConst(l), FloatConst(r)) => float_const(
    f64::from_bits(l).wrapping_add_bits(r, ty).to_bits(), ty),
// …etc
```

Use whatever the current implementation does for arithmetic — it should already guard NaN handling correctly.

- [ ] **Step 2.11.2: Delete replaced helpers and dispatch**

Remove the corresponding `try_fold_*` functions.

- [ ] **Step 2.11.3: Run tests, semantic check, commit**

```bash
cargo test -p opt constant_fold
scripts/compare-analyzer-output.sh
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: convert bool & float const evaluation to rewrite_rules!"
```

---

### Task 2.12: `constant_fold.rs` — part 4, reassociation and mask merging

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.12.1: Add rules**

```rust
// add/sub reassociation
((x + IntConst(c1)) + IntConst(c2))
    => x + int_const(c1.wrapping_add(c2), ty),
((x - IntConst(c1)) - IntConst(c2))
    => x - int_const(c1.wrapping_add(c2), ty),
((x + IntConst(c1)) - IntConst(c2))
    => x + int_const(c1.wrapping_sub(c2), ty),
// mask merging
((a & IntConst(c1)) & IntConst(c2))
    => a & int_const(c1 & c2, ty),
// the distribution example explicitly called out in the design
((a & IntConst(c1)) | (b & IntConst(c2))) & IntConst(c3)
    => (a & int_const(c1 & c3, ty)) | (b & int_const(c2 & c3, ty)),
```

- [ ] **Step 2.12.2: Run existing + any new tests**

Run: `cargo test -p opt constant_fold`
Expected: PASS. If no existing test exercises the distribution rule, add one:

```rust
#[test]
fn distribution_rewrite() {
    // Build ((a & 0xF0) | (b & 0x0F)) & 0xFF, assert the result is
    // semantically ((a & 0xF0) | (b & 0x0F)), i.e. c3=0xFF becomes a no-op
    // after per-branch AND — in practice the rule fires and then other
    // rules simplify further. Assert the rule fires (changed=true).
    // …fill in using existing test helpers…
}
```

- [ ] **Step 2.12.3: Semantic check + commit**

```bash
scripts/compare-analyzer-output.sh
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: reassociation and AND-mask merging rules"
```

---

### Task 2.13: `constant_fold.rs` — part 5, bitcast identity and extend/truncate

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.13.1: Add rules**

```rust
// bitcast identity
IntBitsToFloat(FloatBitsToInt(x)) => x,
FloatBitsToInt(IntBitsToFloat(x)) => x,

// sign/zero-extend of constant
Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty),
Extend::<ZeroExtend>(IntConst(v) : in_ty) => int_const(v, ty),

// truncate of constant
Truncate(IntConst(v))              => int_const(v, ty),

// popcount / lzcount of constant (may already be in 2.6 tests — in that case, re-use)
Popcount(IntConst(v))              => int_const(v.count_ones() as u64, ty),
```

- [ ] **Step 2.13.2: Delete replaced helpers**

- [ ] **Step 2.13.3: Run tests, semantic check, commit**

```bash
cargo test -p opt constant_fold
scripts/compare-analyzer-output.sh
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: bitcast identity + extend/truncate rules"
```

---

### Task 2.14: Wire `cast_to_float` escape hatch

**Files:**
- Modify: `crates/opt/src/constant_fold.rs`

- [ ] **Step 2.14.1: Keep `try_lower_cast_to_float` as-is**

The existing hand-written `try_lower_cast_to_float(fg, node)` function remains in `constant_fold.rs` unchanged. It's a four-way state machine producing different node kinds.

- [ ] **Step 2.14.2: Dispatch via escape hatch syntax**

Add to the main `rewrite_rules!` block a last entry:

```rust
cast_to_float @ try_lower_cast_to_float,
```

This replaces any explicit call-site that previously invoked it from `ConstantFold::run`.

- [ ] **Step 2.14.3: Run tests**

Run: `cargo test -p opt constant_fold::tests::cast_to_float`
Expected: PASS.

- [ ] **Step 2.14.4: Semantic check + commit**

```bash
scripts/compare-analyzer-output.sh
git add crates/opt/src/constant_fold.rs
git commit -m "opt/constant_fold: wire cast_to_float via rewrite_rules escape hatch"
```

---

### Task 2.15: Phase 2 final sweep and gate

- [ ] **Step 2.15.1: Verify the file shrank**

Run: `wc -l crates/opt/src/constant_fold.rs`
Expected: approximately 100-200 lines of non-test code (was ~1000). If the file is still large, identify the leftover hand-written helpers — every rewrite except the cast_to_float state machine should be a rule by now.

- [ ] **Step 2.15.2: Full workspace build and test**

Run: `cargo test --workspace`
Expected: all PASS.

- [ ] **Step 2.15.3: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 2.15.4: Semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: PASS.

No commit — gate task.

---

## Phase 3 — `pattern` crate unification

### Task 3.1: `IntoPat` blanket trait

**Files:**
- Modify: `crates/pattern/src/pat.rs`
- Modify: `crates/pattern/src/lib.rs`

- [ ] **Step 3.1.1: Add the trait**

In `crates/pattern/src/pat.rs`, near the top (after the `Pat` type definition), add:

```rust
/// Blanket trait that lets every builder struct (`IntBinaryOpPat`, `LoadPat`, …)
/// participate in the common `capture` / `when` suffix operations without
/// each builder re-implementing them.
///
/// The implementation forwards to [`Pat::capture`] / [`Pat::when`] via the
/// builder's `Into<Pat>` impl.
pub trait IntoPat: Into<Pat> + Sized {
    /// Attach a [`Var`] capture to this pattern.
    fn capture(self, v: Var) -> Pat {
        self.into().capture(v)
    }

    /// Attach a post-match predicate guard.
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        self.into().when(f)
    }
}

impl<T: Into<Pat>> IntoPat for T {}
```

Ensure `Pat` itself also implements `Into<Pat>` (it does trivially via the blanket `impl<T> Into<T> for T`). Both `.capture()` and `.when()` calls on `Pat` now go through the trait's default body, so `Pat::capture` and `Pat::when` methods (the concrete ones producing a new `Pat`) must stay but can be renamed to avoid recursion — rename them on `Pat` to `capture_impl` / `when_impl` (private), and have the blanket trait call the impl methods.

Concretely: in the `impl Pat { … }` block, rename the existing `pub fn capture` / `pub fn when` to private `fn capture_impl` / `fn when_impl`; have the trait call the private methods on the `Pat`.

Amend the trait body:

```rust
impl<T: Into<Pat>> IntoPat for T {
    fn capture(self, v: Var) -> Pat { self.into().capture_impl(v) }
    fn when<F>(self, f: F) -> Pat
    where F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    { self.into().when_impl(f) }
}
```

- [ ] **Step 3.1.2: Delete duplicate methods from each builder struct**

For each of the twelve builder structs, remove their `pub fn capture` and `pub fn when` methods. They now come from the blanket trait.

Struct list (confirm before deleting): `IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`, `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat`, `CallPat`, `CallOtherPat`, `RetPat`, `IfPat`.

- [ ] **Step 3.1.3: Re-export `IntoPat`**

In `crates/pattern/src/lib.rs`, add `IntoPat` to the `pub use pat::{…};` re-export list.

- [ ] **Step 3.1.4: Build**

Run: `cargo build -p pattern`
Expected: PASS. If a user site calls `.capture(v)` / `.when(f)` and fails to compile, import `IntoPat` in scope (`use pattern::IntoPat;`). The blanket impl makes it always applicable.

- [ ] **Step 3.1.5: Run pattern tests**

Run: `cargo test -p pattern`
Expected: all existing tests PASS.

- [ ] **Step 3.1.6: Commit**

```bash
git add crates/pattern/src/pat.rs crates/pattern/src/lib.rs
git commit -m "pattern: IntoPat blanket trait removes duplicate capture/when methods"
```

---

### Task 3.2: Matcher unary and binary-op helpers

**Files:**
- Modify: `crates/pattern/src/matcher.rs`

- [ ] **Step 3.2.1: Add the two helper methods**

Inside `impl<'g> Matcher<'g> { … }` in `crates/pattern/src/matcher.rs`, add:

```rust
    /// Dispatcher for unary patterns whose only shape variation is the
    /// expected `NodeKind`. `kind_ok` returns `true` if the node's kind
    /// matches the pattern's kind; on success, the single input is matched
    /// against `operand`.
    fn match_unary_op<F>(
        &self,
        node: NodeId,
        operand: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where
        F: FnOnce(&NodeKind) -> bool,
    {
        if !kind_ok(self.fg.graph.node_kind(node)) {
            return false;
        }
        let Ok(inputs) = self.fg.graph.node_inputs_exact::<1>(node) else {
            return false;
        };
        self.match_output(inputs[0], operand, bindings)
    }

    /// Dispatcher for two-input, non-commutative patterns whose only shape
    /// variation is the expected `NodeKind`.
    fn match_binary_op<F>(
        &self,
        node: NodeId,
        lhs: &Pat,
        rhs: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where
        F: FnOnce(&NodeKind) -> bool,
    {
        if !kind_ok(self.fg.graph.node_kind(node)) {
            return false;
        }
        let Ok(inputs) = self.fg.graph.node_inputs_exact::<2>(node) else {
            return false;
        };
        let saved = bindings.snapshot();
        if self.match_output(inputs[0], lhs, bindings)
            && self.match_output(inputs[1], rhs, bindings)
        {
            true
        } else {
            bindings.restore(saved);
            false
        }
    }
```

(Confirm `match_output`, `Bindings::snapshot`, `Bindings::restore`, and `self.fg` naming by reading the existing file — adjust names to match. If `Bindings` doesn't currently have `snapshot`/`restore`, check whether the matching code does manual backtracking and use that idiom instead.)

- [ ] **Step 3.2.2: Collapse unary arms**

Find the `match pattern.kind()` block inside `Matcher::match_pattern_against_node` (or similarly named method). Rewrite the six unary arms:

```rust
PatKind::CastToBool { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToBool)),
PatKind::CastToInt { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToInt)),
PatKind::Truncate { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Truncate)),
PatKind::Popcount { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Popcount)),
PatKind::Lzcount { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Lzcount)),
PatKind::Extend { ext_op, operand } =>
    self.match_unary_op(node, operand, bindings,
        |k| matches!(k, NodeKind::Extend(op) if op == ext_op)),
```

- [ ] **Step 3.2.3: Collapse non-commutative binary arms**

```rust
PatKind::Piece { hi, lo } =>
    self.match_binary_op(node, hi, lo, bindings, |k| matches!(k, NodeKind::Piece)),
PatKind::Insert { lsb, len, base, val } => {
    // Slight variant: Insert has extra fields; inline it or pass them
    // through a closure that also reads node_kind fields. If the kind
    // check is only a shape match, match_binary_op still works:
    self.match_binary_op(node, base, val, bindings,
        |k| matches!(k, NodeKind::Insert { lsb: l, len: n } if l == lsb && n == len))
}
PatKind::IntCmpOp { op, lhs, rhs } =>
    self.match_binary_op(node, lhs, rhs, bindings,
        |k| matches!(k, NodeKind::IntCmpOp(kop) if kop == op)),
PatKind::FloatCmpOp { op, lhs, rhs } =>
    self.match_binary_op(node, lhs, rhs, bindings,
        |k| matches!(k, NodeKind::FloatCmpOp(kop) if kop == op)),
```

(Exact field names — `hi`/`lo`, `lhs`/`rhs`, `op`, etc. — come from `PatKind`. Confirm by reading `pat.rs`.)

- [ ] **Step 3.2.4: Build and test**

Run: `cargo build -p pattern && cargo test -p pattern`
Expected: all existing tests PASS. The behavioural oracle is `crates/pattern/tests/matching.rs`; if anything fails, revert to the hand-written arm and compare.

- [ ] **Step 3.2.5: Semantic check**

Run: `scripts/compare-analyzer-output.sh`
Expected: PASS (pattern crate isn't in the analyzer pipeline, but the end-to-end check is cheap).

- [ ] **Step 3.2.6: Commit**

```bash
git add crates/pattern/src/matcher.rs
git commit -m "pattern: match_unary_op / match_binary_op helpers"
```

---

### Task 3.3: Phase 3 gate

- [ ] **Step 3.3.1: Full workspace test + clippy**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all PASS, no warnings.

No commit.

---

## Phase 4 — remaining passes clean-up

### Task 4.1: Sweep `as_value().ok_or(...)` across `opt/` passes

**Files:**
- Modify: `crates/opt/src/known_bits.rs`
- Modify: `crates/opt/src/dead_branch.rs`
- Modify: `crates/opt/src/redundant_phis.rs`
- Modify: `crates/opt/src/load_readonly.rs`
- Modify: `crates/opt/src/stack_store.rs`

- [ ] **Step 4.1.1: Identify call-sites**

Using Grep, find every occurrence of `as_value().ok_or(` and `as_integer().ok_or(` and `as_float().ok_or(` in `crates/opt/src/`.

- [ ] **Step 4.1.2: Replace each**

For each hit:

```rust
// before
let ty = out_kind.as_value().ok_or(ErrorKind::ExpectedValueOutput(out_kind))?;
// after
let ty = out_kind.as_value_or_err()?;
```

Same for the integer and float variants (→ `.as_integer_or_err()?` / `.as_float_or_err()?`).

Remove now-unused `use crate::error::ErrorKind;` if the file no longer references `ErrorKind`.

- [ ] **Step 4.1.3: Build and test**

Run: `cargo test -p opt`
Expected: all PASS.

- [ ] **Step 4.1.4: Commit**

```bash
git add crates/opt/src/
git commit -m "opt: sweep as_value().ok_or(...) to as_value_or_err() etc."
```

---

### Task 4.2: Sweep phi-kind match arms to `NodeKind::is_phi()`

**Files:**
- Modify: `crates/opt/src/redundant_phis.rs`
- Modify: any other file with `NodeKind::ControlPhi(..) | NodeKind::MemPhi` style matches where the code only asks "is this a phi?"

- [ ] **Step 4.2.1: Find call-sites**

Grep: `NodeKind::ControlPhi.*NodeKind::MemPhi` (multiline). Hits in `opt/src/redundant_phis.rs` (line 60 — the precedence check) and possibly `validate.rs`/`walk.rs`.

- [ ] **Step 4.2.2: Replace where semantics match**

Replace bare `matches!(k, NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. })` with `k.is_phi()`. Do not replace arms that discriminate between phi variants (those are genuinely per-variant).

- [ ] **Step 4.2.3: Build and test**

Run: `cargo test --workspace`
Expected: all PASS.

- [ ] **Step 4.2.4: Commit**

```bash
git add crates/opt/src/redundant_phis.rs
git commit -m "opt: use NodeKind::is_phi() where semantics match"
```

---

### Task 4.3: Phase 4 gate

- [ ] **Step 4.3.1: Full workspace sweep**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/compare-analyzer-output.sh
```
Expected: all PASS.

---

## Wrap-up: remove baseline scripts

### Task W1: Remove the baseline-comparison scripts

**Files:**
- Delete: `scripts/capture-baseline.sh`
- Delete: `scripts/compare-analyzer-output.sh`
- Delete: `scripts/baseline/`

- [ ] **Step W1.1: Delete**

```bash
git rm -r scripts/baseline scripts/capture-baseline.sh scripts/compare-analyzer-output.sh
```

- [ ] **Step W1.2: Commit**

```bash
git commit -m "scripts: remove refactor baseline-comparison scaffolding"
```

---

## Self-review

### Spec coverage

- Phase 0 — `ir::ops` foundation: ✅ Tasks 0.1–0.12 cover module split, `ExpectedValueOutput` variant, `as_value_or_err/as_integer_or_err/as_float_or_err`, `is_phi`, `consts`/`rewrite`/`builder` submodules with tests, `OptimizationResult::from_changed`, migration of all five opt passes, deletion of `opt/utils.rs`.
- Phase 1 — `NodeOutputType` table: ✅ Task 1.1 replaces the six repeated matches with `TYPE_INFO` and `Category`, keeps `get_unsigned_int`/`get_signed_int`/`to_natural_int_type` untouched, adds the variant-order coverage test.
- Phase 2 — `rewrite_rules!`: ✅ Task 2.0 spike proves three representative rules before the full grammar. Tasks 2.1–2.7 extend the grammar feature-by-feature with tests each. Task 2.8 adds trybuild err coverage. Tasks 2.9–2.14 rewrite `constant_fold.rs` group by group (identities → full eval → bool/float → reassociation → bitcast/extend → cast_to_float escape hatch).
- Phase 3 — pattern unification: ✅ Task 3.1 adds `IntoPat` blanket trait and removes duplicate methods from twelve builder structs. Task 3.2 consolidates unary and binary-op matcher arms.
- Phase 4 — clean-up sweep: ✅ Task 4.1 replaces `as_value().ok_or(...)` patterns; Task 4.2 replaces phi-kind matches with `is_phi()`.

Baseline + wrap-up: ✅ Pre-flight captures the analyzer output; Task W1 removes the scaffolding afterwards.

### Placeholder scan

The plan does contain TODO-like text in a few places — each is a *legitimate pointer to real code in the existing tree*, not a placeholder requiring content invention:
- Task 2.9.1 asks the engineer to read `constant_fold.rs` and list identity rules. The expected list is given; the read is confirmation.
- Task 2.10.1/2.11.1 same shape — a list of kinds/variants the engineer verifies against the current file.
- Task 3.2.1 asks the engineer to confirm `match_output` / `Bindings::snapshot` naming. Substituting an unknown name in the plan would be worse than pointing at the file.
- Task 0.5.1 notes the test helper name (`FunctionBuilder::new_standalone`) may not exist in the branch; gives concrete fallback instructions.

No "TBD", no "implement later", no "similar to Task N", no bare "handle edge cases" instructions.

### Type consistency

- `BuiltFunctionGraph` helper naming stays consistent: `int_const_val`, `bool_const_val`, `float_const_val`, `make_int_const`, `make_bool_const`, `make_float_const`, `make_value_node`, `replace_all_uses`. Used under the same names across Phase 0 migrations, Phase 2 macro codegen, and Phase 4 sweeps.
- Error variant names: `ir::ErrorKind::ExpectedValueOutput(NodeOutputKind)` (new in Task 0.2), `ExpectedIntegerType(NodeOutputType)` and `ExpectedFloatType(NodeOutputType)` (pre-existing in `ir::error.rs`, confirmed). `as_value_or_err` uses the new one; `as_integer_or_err` / `as_float_or_err` use the pre-existing ones. Consistent.
- `OptimizationResult::from_changed(bool)` in Phase 0 Task 0.8 is called from the macro codegen in Task 2.0 and from every migration from Task 0.9 onwards. Same signature throughout.
- Macro grammar element names (`IntConst`, `BoolConst`, `FloatConst`, `BAnd`/`BOr`/`BXor`, `FAdd`/`FSub`/`FMul`/`FDiv`, `IntEq`/`IntLt`/…, `FEq`/`FNe`/…, `Extend::<SignExtend>`, `: in_ty`) match the spec's Grammar table 1:1.
- Phase 3 trait name is `IntoPat` in both Task 3.1 code and references.

No drift found.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-17-code-quality-refactor.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
