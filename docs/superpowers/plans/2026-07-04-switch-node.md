# Switch IR Node + Control-Node Patterns Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the switch/indirect-branch if-ladder lowering (the last production caller of eager `create_region`) with a real `Switch` IR node, add its collapse to `DeadBranchElimination`, add matcher patterns for `Switch`/`IndirectBranch`/`Unreachable`, and gate the now-production-unused eager `create_region` test-only.

**Architecture:** A new unit `NodeKind::Switch` with inputs `[ctrl, address]` and N variadic `Control` outputs; per-output target addresses in a `Function` side table (`switch_targets: SecondaryMap<NodeId, Vec<u64>>`). The lifter's `handle_switch` emits it directly (no dispatcher regions); `DeadBranchElimination` collapses it on a constant address. Patterns mirror the existing `RetPat` node-rooted builder.

**Tech Stack:** Rust workspace (`strider-ir`, `strider-lift`, `strider-opt`, `strider-pattern`, `strider-py`), cranelift-entity `SecondaryMap`, PyO3.

## Global Constraints

- Selector is the **dispatch address** (`Switch.inputs[1]`), same value `handle_switch` reads today; case addresses live in the `switch_targets` side table (positional: control output `i` ↔ `cases[i]`). No 0..N-1 index extraction.
- **No default output** — the resolved jump table is exhaustive (N outputs = N cases).
- `NodeKind::Switch` is a **unit variant** (payload ≤ 16 bytes guard in `node/kind.rs`) and **non-cacheable**.
- Every `match self`/`match kind` over `NodeKind` in `strider-ir` is exhaustive (no `_` arm) — adding the variant forces new arms; the compiler lists them.
- TDD: write the failing test first, watch it fail, implement, watch it pass, commit.
- Build strider-py via `uv run maturin develop --release` from the workspace ROOT (not `crates/strider-py`).
- Commit trailer on every commit:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01LaLKYVdEUjkhfQj14BBxpF`.
- All commits are squashed into ONE at the end (see Finalization), then constfold is rebased onto it — implementers still commit per task.

## File Structure

- `crates/strider-ir/src/node/kind.rs` — add `Switch` variant + `match` arms.
- `crates/strider-ir/src/node_signature.rs` — add a 4th `sig!` arm + the `Switch` signature.
- `crates/strider-ir/src/function/side_tables.rs` — `switch_targets` field, accessors, remap.
- `crates/strider-ir/src/builder/nodes.rs` — `build_switch` verb.
- `crates/strider-ir/src/validate/graph_invariants.rs` + `validate/mod.rs` — Switch invariant + errors.
- `crates/strider-ir/src/function/dot/label.rs` — Switch label with case addresses.
- `crates/strider-lift/src/lift/control.rs` — `handle_switch` emits `Switch`; delete `build_switch_if_ladder`.
- `crates/strider-opt/src/opt/dead_branch/mod.rs` + `tests.rs` — switch collapse.
- `crates/strider-pattern/src/node_builders/flow.rs`, `node_builders/mod.rs`, `lib.rs` — Rust patterns.
- `crates/strider-py/src/pattern.rs` — Python patterns.
- `crates/strider-ir/src/builder/vars.rs` — gate eager `create_region` test-only.

---

### Task 1: Add `NodeKind::Switch` variant + signature

**Files:**
- Modify: `crates/strider-ir/src/node/kind.rs` (variant after `If` @98; arms in `is_cacheable`, `asm_fingerprint_exempt`, `has_control_flow`; test list @476-484)
- Modify: `crates/strider-ir/src/node_signature.rs` (4th `sig!` arm @272-291; `Switch` arm near @315; coverage test vec @616-650)

**Interfaces:**
- Produces: `NodeKind::Switch` (unit variant); `expected_signature(&NodeKind::Switch)` = inputs `[CTRL, INT_VAL]`, outputs variadic `Control` (≥1).

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)]` module in `node_signature.rs`:

```rust
    #[test]
    fn expected_signature_switch_is_ctrl_val_in_variadic_ctrl_out() {
        let sig = expected_signature(&NodeKind::Switch);
        // inputs: fixed [CTRL, INT_VAL]
        assert_eq!(sig.inputs.head.len(), 2);
        assert!(sig.inputs.tail.is_none(), "switch inputs are fixed-arity");
        // outputs: variadic Control (head [CTRL] + out_tail CTRL)
        assert!(sig.outputs.tail.is_some(), "switch has variadic control outputs");
    }
```

(If `Signature`'s fields aren't named `inputs.head`/`inputs.tail`/`outputs.tail`, read `struct Signature`/`SlotList` at `node_signature.rs:85-125` and adjust the assertions to the real field names — the intent is "2 fixed inputs, variadic outputs".)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir expected_signature_switch -- --nocapture`
Expected: FAIL to compile — `no variant named Switch`.

- [ ] **Step 3a: Add the variant** in `node/kind.rs` right after `If,` (line 98):

```rust
    /// Multi-way branch (resolved jump table).  Consumes `(control, address)`
    /// and produces N `Control` outputs, one per target region in target order;
    /// output `i` is taken when `address == switch_targets[i]` (the per-output
    /// case addresses live in the `Function::switch_targets` side table).  The
    /// lifter emits this for a `RegionTerminator::Switch`; the table is
    /// exhaustive (no default arm).
    Switch,
```

- [ ] **Step 3b: Add the `match` arms** the compiler now demands in `node/kind.rs`:
  - In `is_cacheable` — add `Switch` to the **non-cacheable** group (the arm that lists `Return | IndirectBranch | Unreachable` → `false`): `NodeKind::Return | NodeKind::IndirectBranch | NodeKind::Unreachable | NodeKind::Switch => false,`.
  - In `asm_fingerprint_exempt` — add `Switch` to the **non-exempt** (`false`) group alongside `If`/`Return` (a switch comes from a real `jmp`, must carry a fingerprint).
  - In `has_control_flow` — add `NodeKind::Switch` to the **true** group alongside `If | Return | Call | IndirectBranch | Unreachable`.
  - In the `#[cfg(test)]` test `has_side_effects_is_control_flow_plus_memory_writes_and_opaque` (the control-flow-kinds array @476-484), add `NodeKind::Switch,`.

- [ ] **Step 3c: Add a 4th `sig!` macro arm** (fixed inputs, variadic outputs) in `node_signature.rs`, after the existing third arm (@285-290):

```rust
        // fixed inputs, variadic outputs (out_tail without in_tail)
        (inputs: [$($i:expr),* $(,)?], outputs: [$($o:expr),* $(,)?]; out_tail: $ot:expr $(,)?) => {
            Signature {
                inputs: SlotList::fixed(&[$($i),*]),
                outputs: SlotList::variadic(&[$($o),*], $ot),
            }
        };
```

(Match the exact `Signature`/`SlotList` constructor names used by the other three arms — read them at @272-291 and copy verbatim; the intent is `SlotList::fixed` for inputs and `SlotList::variadic` for outputs.)

- [ ] **Step 3d: Add the `Switch` signature arm** in `expected_signature` (near the `If` arm @315):

```rust
        NodeKind::Switch => sig!(inputs: [CTRL, INT_VAL], outputs: [CTRL]; out_tail: CTRL),
```

- [ ] **Step 3e:** Add `NodeKind::Switch,` to the `expected_signature_covers_every_node_kind` test `kinds` vec (@616-650).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p strider-ir 2>&1 | grep -E "test result|error"`
Expected: PASS, 0 failed, no compile errors.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-ir/src/node/kind.rs crates/strider-ir/src/node_signature.rs
git commit -m "feat(ir): add NodeKind::Switch variant + signature"
```

---

### Task 2: `switch_targets` side table

**Files:**
- Modify: `crates/strider-ir/src/function/side_tables.rs` (field @~108, accessors @~187-196, remap @~234)
- Test: `crates/strider-ir/src/function/func.rs` (mirror `compact_remaps_surviving_stack_offset_entry` @878, `retain_reachable_drops_side_table_entry_for_dropped_node` @992)

**Interfaces:**
- Consumes: `NodeKind::Switch` (Task 1).
- Produces: `SideTables::switch_targets(id: NodeId) -> &[u64]`, `SideTables::set_switch_targets(id: NodeId, targets: Vec<u64>)`; reached via `function.side_tables()` / `function.side_tables_mut()`.

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)]` module in `func.rs` (mirror the stack-offset compact test; adapt node creation to your test helpers):

```rust
    #[test]
    fn switch_targets_survive_compact() {
        // Build a minimal function with one node carrying switch targets.
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let r = b.create_region().unwrap();
        b.set_entry_region(r).unwrap();
        b.set_region(r);
        b.build_return(None, &[]).unwrap();
        let mut f = b.build().unwrap();
        let node = f.entry(); // any live NodeId
        f.side_tables_mut().set_switch_targets(node, vec![0x1000, 0x1020]);
        assert_eq!(f.side_tables().switch_targets(node), &[0x1000, 0x1020]);
        f.compact();
        // Entry survives compact; its targets must be remapped, not dropped.
        let new_node = f.entry();
        assert_eq!(f.side_tables().switch_targets(new_node), &[0x1000, 0x1020]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir switch_targets_survive_compact`
Expected: FAIL — `no method named set_switch_targets`.

- [ ] **Step 3a: Add the field** next to `stack_offsets` in `SideTables` (@~108):

```rust
    switch_targets: SecondaryMap<NodeId, Vec<u64>>,
```

- [ ] **Step 3b: Add accessors** next to `stack_offset`/`set_stack_offset` (@~187-196):

```rust
    #[inline]
    pub fn switch_targets(&self, id: NodeId) -> &[u64] {
        self.switch_targets[id].as_slice()
    }
    #[inline]
    pub fn set_switch_targets(&mut self, id: NodeId, targets: Vec<u64>) {
        self.switch_targets[id] = targets;
    }
```

- [ ] **Step 3c: Add the remap arm** in `SideTables::remap` (@~234, next to the `remap_node_keyed` calls for `call_other_names`/`asm_fingerprints`):

```rust
        remap_node_keyed(&mut self.switch_targets, remap);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-ir switch_targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-ir/src/function/side_tables.rs crates/strider-ir/src/function/func.rs
git commit -m "feat(ir): switch_targets side table (NodeId -> case addresses)"
```

---

### Task 3: `build_switch` builder verb + dot label

**Files:**
- Modify: `crates/strider-ir/src/builder/nodes.rs` (add `build_switch`, mirror `build_if` @160-180)
- Modify: `crates/strider-ir/src/function/dot/label.rs` (add `Switch` arm before the `_` catch-all @~235)
- Test: `crates/strider-ir/src/builder/tests.rs`

**Interfaces:**
- Consumes: `NodeKind::Switch` (T1), `set_switch_targets` (T2), `link_region` (existing @region.rs:296).
- Produces: `FunctionBuilder::build_switch(&mut self, address: ValueId, arms: &[(RegionId, u64)]) -> Result<()>`.

- [ ] **Step 1: Write the failing test** in `builder/tests.rs`:

```rust
    #[test]
    fn build_switch_makes_n_control_outputs_and_records_targets() -> crate::error::Result<()> {
        let mut b = strider_ir_test_utils::empty_builder()?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        let addr = b.build_int_const(0x1000u64, crate::node::ValueType::I64)?;
        b.build_switch(addr, &[(a, 0x1000), (c, 0x1020)])?;
        // terminate the arms so the function is valid
        b.set_region(a); b.build_return(None, &[])?;
        b.set_region(c); b.build_return(None, &[])?;
        let f = b.build()?;
        // Find the Switch node.
        let sw = f.graph().all_node_ids()
            .find(|&n| matches!(f.node_kind(n), crate::node::NodeKind::Switch))
            .expect("switch node exists");
        assert_eq!(f.node_inputs(sw).len(), 2, "[ctrl, address]");
        assert_eq!(f.node_outputs(sw).len(), 2, "one control output per arm");
        assert_eq!(f.side_tables().switch_targets(sw), &[0x1000, 0x1020]);
        Ok(())
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir build_switch_makes_n`
Expected: FAIL — `no method named build_switch`.

- [ ] **Step 3a: Add `build_switch`** in `builder/nodes.rs` (mirror `build_if`):

```rust
    /// Terminates the current region with a `Switch`: one `Control` output per
    /// arm, wired to that arm's target region in order, plus the per-output case
    /// addresses recorded in `switch_targets`.  `address` is the dispatch value
    /// (output `i` is taken when `address == arms[i].1`).
    ///
    /// # Errors
    /// `NoCurrentRegion` / `RegionTerminated` if no active region;
    /// `ExpectedValue` if `address` is not a value edge; `ExpectedControl` if the
    /// region's snapshotted control edge is mistyped.  Requires `arms` non-empty.
    pub fn build_switch(&mut self, address: ValueId, arms: &[(RegionId, u64)]) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_value_kind(address)?;
        self.require_control_kind(res.control)?;

        let sw = self.create_node(
            NodeKind::Switch,
            [res.control, address],
            std::iter::repeat(ValueKind::Control).take(arms.len()),
        );
        // Dynamic arity: snapshot outputs, cloning to end the immutable borrow.
        let out_ctrls: Vec<ValueId> = self.function().node_outputs(sw).to_vec();
        for (&(region, _addr), &ctrl) in arms.iter().zip(&out_ctrls) {
            self.link_region(region, ctrl, res.memory, res.region_id)?;
        }
        let targets: Vec<u64> = arms.iter().map(|&(_, a)| a).collect();
        self.function_mut().side_tables_mut().set_switch_targets(sw, targets);
        Ok(())
    }
```

(If `require_value_kind` doesn't exist, use the require-helper `build_if`/`build_return` use for value inputs — grep `require_` in `nodes.rs`. `link_region` is `pub(crate)` at `region.rs:296`.)

- [ ] **Step 3b: Add the dot label arm** in `function/dot/label.rs`, before the `_ => format!("{kind:?}")` catch-all (@~235):

```rust
            NodeKind::Switch => {
                let cases: String = self
                    .function
                    .side_tables()
                    .switch_targets(node)
                    .iter()
                    .map(|a| format!("\n0x{a:x}"))
                    .collect();
                format!("Switch{cases}")
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-ir build_switch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-ir/src/builder/nodes.rs crates/strider-ir/src/function/dot/label.rs crates/strider-ir/src/builder/tests.rs
git commit -m "feat(ir): build_switch verb + Switch dot label"
```

---

### Task 4: Switch validator invariant

**Files:**
- Modify: `crates/strider-ir/src/validate/graph_invariants.rs` (add `check_graph_invariants_switch`, mirror `check_graph_invariants_extend_truncate` @134)
- Modify: `crates/strider-ir/src/validate/mod.rs` (import @38-42, dispatch @86-94, `ValidationError` variants @~171)
- Test: `crates/strider-ir/tests/build_validate_roundtrip.rs` or the validate module tests

**Interfaces:**
- Consumes: `NodeKind::Switch` (T1), `switch_targets` (T2), `build_switch` (T3).
- Produces: `ValidationError::EmptySwitchTargets { node }`, `ValidationError::SwitchTargetArityMismatch { node, outputs, targets }`.

- [ ] **Step 1: Write the failing test** (a valid switch passes; an arity-broken one errors). In the validate tests:

```rust
    #[test]
    fn switch_target_arity_mismatch_is_rejected() {
        // Build a valid switch, then corrupt its side table to N-1 addresses.
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let entry = b.create_region().unwrap();
        let a = b.create_region().unwrap();
        let c = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();
        b.set_region(entry);
        let addr = b.build_int_const(0x1000u64, crate::node::ValueType::I64).unwrap();
        b.build_switch(addr, &[(a, 0x1000), (c, 0x1020)]).unwrap();
        b.set_region(a); b.build_return(None, &[]).unwrap();
        b.set_region(c); b.build_return(None, &[]).unwrap();
        let mut f = b.build().unwrap();
        assert!(crate::validate::validate(&f).is_ok(), "well-formed switch validates");
        let sw = f.graph().all_node_ids()
            .find(|&n| matches!(f.node_kind(n), crate::node::NodeKind::Switch)).unwrap();
        f.side_tables_mut().set_switch_targets(sw, vec![0x1000]); // now 1 addr, 2 outputs
        assert!(crate::validate::validate(&f).is_err(), "arity mismatch rejected");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir switch_target_arity_mismatch`
Expected: FAIL — validation currently doesn't check Switch (the mismatch is not rejected).

- [ ] **Step 3a: Add the check** in `graph_invariants.rs`:

```rust
pub(super) fn check_graph_invariants_switch(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in reachable.iter().map(|n| (n, graph.node_kind(n))) {
        if !matches!(kind, NodeKind::Switch) {
            continue;
        }
        let n_out = graph.node_outputs(node).len();
        let n_targets = function.side_tables().switch_targets(node).len();
        if n_out == 0 {
            errs.push(ValidationError::EmptySwitchTargets { node });
        } else if n_out != n_targets {
            errs.push(ValidationError::SwitchTargetArityMismatch {
                node,
                outputs: n_out,
                targets: n_targets,
            });
        }
    }
}
```

- [ ] **Step 3b: Add the error variants** in `validate/mod.rs` (`pub enum ValidationError`, near `EmptyRegionPredecessors`):

```rust
    #[error("Switch {node:?} has no control outputs")]
    EmptySwitchTargets { node: NodeId },
    #[error("Switch {node:?} has {outputs} control outputs but {targets} recorded target addresses")]
    SwitchTargetArityMismatch { node: NodeId, outputs: usize, targets: usize },
```

- [ ] **Step 3c: Import + dispatch** in `validate/mod.rs`: add `check_graph_invariants_switch,` to the `use super::graph_invariants::{...}` group (@38-42), and add the call in the `check_graph_invariants_*` block (@86-94):

```rust
    check_graph_invariants_switch(function, &reachable, &mut errs);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-ir switch_target_arity_mismatch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-ir/src/validate/graph_invariants.rs crates/strider-ir/src/validate/mod.rs crates/strider-ir/tests/build_validate_roundtrip.rs
git commit -m "feat(ir): validate Switch output/target-address arity"
```

---

### Task 5: Lifter emits `Switch`; delete `build_switch_if_ladder`

**Files:**
- Modify: `crates/strider-lift/src/lift/control.rs` (`handle_switch` @138 → call `build_switch`; **delete** `build_switch_if_ladder` @53-107 and its `#[cfg(test)]` tests @~372-472)

**Interfaces:**
- Consumes: `build_switch` (T3).
- Produces: `handle_switch` emits one `Switch` node (no dispatcher regions, no `If`-ladder).

- [ ] **Step 1: Rewrite the test** — replace the deleted `build_switch_if_ladder` unit tests with a `handle_switch` shape test in `control.rs`'s `#[cfg(test)]` module. Mirror the existing `handle_switch`/`make_builder_with_targets` fixtures (@398-472); assert the built function contains exactly one `NodeKind::Switch` with N control outputs and no `NodeKind::If` from the switch. (Read the existing test harness at @372-472 to reuse `make_builder_with_targets`.) Example assertion body:

```rust
    #[test]
    fn handle_switch_emits_single_switch_node() {
        // ... build via make_builder_with_targets with 3 targets ...
        let f = /* build_ir result */;
        let switches: Vec<_> = f.graph().all_node_ids()
            .filter(|&n| matches!(f.node_kind(n), strider_ir::node::NodeKind::Switch)).collect();
        assert_eq!(switches.len(), 1, "one Switch node");
        assert_eq!(f.node_outputs(switches[0]).len(), 3, "3 control outputs");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-lift handle_switch_emits_single_switch`
Expected: FAIL — `handle_switch` still emits an If-ladder (0 Switch nodes).

- [ ] **Step 3: Replace the `build_switch_if_ladder` call** in `handle_switch` (@166). Keep the `targets_and_regions` loop and `let idx = self.read_vn(target_vn)?;`, then:

```rust
        // n == 1 degenerates to a plain branch (unchanged behavior).
        if targets_and_regions.len() == 1 {
            return self.builder.build_branch(targets_and_regions[0].1);
        }
        let arms: Vec<(strider_ir::RegionId, u64)> = targets_and_regions
            .iter()
            .map(|&(addr, region)| (region, addr))
            .collect();
        self.builder.build_switch(idx, &arms)
```

Then **delete** `pub(crate) fn build_switch_if_ladder` (@53-107) and its unit tests (`handle_switch_with_one_target_...`, `..._two_targets_...`, `..._three_targets_chains_if_ladder_...` @~460-472 and the ladder tests @372-459 that reference `build_switch_if_ladder`). Update the `handle_switch` doc comment (@120-137) to say it emits a `Switch` node.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p strider-lift 2>&1 | grep -E "test result|error"`
Expected: PASS, 0 failed; `build_switch_if_ladder` no longer referenced (`grep -rn build_switch_if_ladder crates` returns nothing).

- [ ] **Step 5: Commit**

```bash
git add crates/strider-lift/src/lift/control.rs
git commit -m "feat(lift): emit Switch node; delete build_switch_if_ladder"
```

---

### Task 6: `DeadBranchElimination` switch collapse

**Files:**
- Modify: `crates/strider-opt/src/opt/dead_branch/mod.rs` (`matches_kind` @44, `try_rewrite` @48)
- Test: `crates/strider-opt/src/opt/dead_branch/tests.rs` (mirror `dead_branch_false` @137 + `make_if_fn` @39)

**Interfaces:**
- Consumes: `NodeKind::Switch` (T1), `switch_targets` (T2), `build_switch` (T3), `int_const_u128` (`IRViewer`).
- Produces: a constant-address `Switch` folds to its single matching arm and is killed.

- [ ] **Step 1: Write the failing test** in `dead_branch/tests.rs` — build a `Switch` whose address input is a constant equal to arm 1's case address, run `DeadBranchElimination`, assert it changed and the `Switch` is gone:

```rust
    #[test]
    fn dead_switch_const_address_keeps_matching_arm() -> Result<()> {
        let mut b = strider_ir_test_utils::empty_builder()?;
        let entry = b.create_region()?;
        let a0 = b.create_region()?;
        let a1 = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let addr = b.build_int_const(0x1020u64, strider_ir::ValueType::I64)?; // == arm 1's case
        b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
        b.set_region(a0); b.build_return(None, &[])?;
        b.set_region(a1); b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        let n_switch_before = fg.graph().all_node_ids()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch)).count();
        assert_eq!(n_switch_before, 1);
        let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OctCtxNew())?;
        assert!(result.changed(), "const-address switch must fold");
        let n_switch_after = fg.graph().all_node_ids()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch)).count();
        assert_eq!(n_switch_after, 0, "switch killed after fold");
        Ok(())
    }
```

(Use the same `OptCtx::new(None)` construction the existing tests use for the last arg — see `dead_branch_false` @137; replace `OctCtxNew()` with `&mut crate::OptCtx::new(None)` matching that test's signature.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-opt dead_switch_const_address`
Expected: FAIL — `matches_kind` returns false for `Switch`, so nothing folds (`changed` is false / switch survives).

- [ ] **Step 3a: Broaden `matches_kind`** (@44):

```rust
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If | NodeKind::Switch)
    }
```

- [ ] **Step 3b: Dispatch in `try_rewrite`** (@48) — keep the existing `If` body, add a `Switch` branch. Restructure the body as:

```rust
        match ctx.node_kind(root) {
            NodeKind::If => { /* ... existing If fold, unchanged ... */ }
            NodeKind::Switch => {
                // inputs: [ctrl, address]
                let inputs = ctx.node_inputs(root);
                let ctrl_value = inputs[0];
                let addr_value = inputs[1];
                let Some(k) = ctx.function().int_const_u128(addr_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };
                let targets: Vec<u64> = ctx.function().side_tables().switch_targets(root).to_vec();
                let Some(i) = targets.iter().position(|&t| u128::from(t) == k) else {
                    return Ok(PeepholeRewrite::NoChange); // exhaustive table => shouldn't happen
                };
                let live_ctrl = ctx.node_outputs(root)[i];
                ctx.absorb_fingerprint(ctrl_value, addr_value);
                ctx.replace_value(live_ctrl, ctrl_value)?;
                ctx.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            _ => Ok(PeepholeRewrite::NoChange),
        }
```

(Copy `ctrl_value`/`addr_value` out of the `node_inputs` slice before the mutable calls to end the borrow; `int_const_u128` is on `IRViewer` at `viewer.rs:189`.)

- [ ] **Step 4: Run test to verify it passes** + no regression on the existing If tests

Run: `cargo test -p strider-opt -- dead_branch dead_switch`
Expected: PASS, all existing `dead_branch_*` tests still green.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/opt/dead_branch/mod.rs crates/strider-opt/src/opt/dead_branch/tests.rs
git commit -m "feat(opt): DeadBranchElimination collapses constant-address Switch"
```

---

### Task 7: Rust pattern builders (`indirect_branch`, `unreachable`, `switch`)

**Files:**
- Modify: `crates/strider-pattern/src/node_builders/flow.rs` (add 3 builders after `RetPat` @257)
- Modify: `crates/strider-pattern/src/node_builders/mod.rs` (@45 re-export)
- Modify: `crates/strider-pattern/src/lib.rs` (@45-49 re-export)
- Test: `crates/strider-pattern/tests/control_build.rs` (mirror `ret_*` @266-352)

**Interfaces:**
- Consumes: `NodeKind::Switch` (T1); existing `IndirectBranch`/`Unreachable`.
- Produces: `strider_pattern::{indirect_branch, unreachable, switch}` + `{IndirectBranchPat, UnreachablePat, SwitchPat}`.

- [ ] **Step 1: Write the failing test** in `control_build.rs` (mirror `ret_captures_node`):

```rust
    #[test]
    fn indirect_branch_captures_node() {
        // Build a function whose region ends in an IndirectBranch placeholder.
        // (Reuse the existing control_build harness; see ret_captures_node @339.)
        let function = /* ... build with an IndirectBranch ... */;
        let n = Capture::new();
        let m = Matcher::new(&function)
            .find_all(&indirect_branch().capture(n).build()).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn unreachable_matches() {
        let function = /* ... build with an Unreachable ... */;
        let hits = Matcher::new(&function).find_all(&unreachable().build()).unwrap();
        assert_eq!(hits.len(), 1);
    }
```

(Read `control_build.rs:266-352` for the exact `Matcher`/`find_all` idiom and how to build a function containing these nodes — `IndirectBranch` via `build_indirect_branch`, `Unreachable` via `build_unreachable`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-pattern indirect_branch_captures`
Expected: FAIL — `indirect_branch` not found.

- [ ] **Step 3a: Add the builders** in `flow.rs` after `ret()` (@257):

```rust
// ── IndirectBranchPat ──  inputs [ctrl(0), mem(1), target(2)], no outputs.
pub struct IndirectBranchPat(NodePat);
impl IndirectBranchPat {
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self { Self(self.0.input(2, p)) }
    pub fn preceded_by<P: MatchPat + 'static>(self, p: P) -> Self { Self(self.0.input_control(0, p)) }
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self { Self(self.0.input_mem(1, p)) }
    pub fn capture(self, c: Capture) -> Self { Self(self.0.capture(c)) }
    pub fn build(self) -> Pattern { self.0.build() }
}
pub fn indirect_branch() -> IndirectBranchPat {
    IndirectBranchPat(NodePat::node(KindSpec::Exact(NodeKind::IndirectBranch)))
}

// ── UnreachablePat ──  inputs [ctrl(0)], no outputs.
pub struct UnreachablePat(NodePat);
impl UnreachablePat {
    pub fn preceded_by<P: MatchPat + 'static>(self, p: P) -> Self { Self(self.0.input_control(0, p)) }
    pub fn capture(self, c: Capture) -> Self { Self(self.0.capture(c)) }
    pub fn build(self) -> Pattern { self.0.build() }
}
pub fn unreachable() -> UnreachablePat {
    UnreachablePat(NodePat::node(KindSpec::Exact(NodeKind::Unreachable)))
}

// ── SwitchPat ──  inputs [ctrl(0), address(1)], N control outputs.
pub struct SwitchPat(NodePat);
impl SwitchPat {
    /// Constrain the dispatch address (`inputs[1]`).
    pub fn address<P: MatchPat + 'static>(self, p: P) -> Self { Self(self.0.input(1, p)) }
    pub fn preceded_by<P: MatchPat + 'static>(self, p: P) -> Self { Self(self.0.input_control(0, p)) }
    pub fn capture(self, c: Capture) -> Self { Self(self.0.capture(c)) }
    pub fn build(self) -> Pattern { self.0.build() }
}
pub fn switch() -> SwitchPat {
    SwitchPat(NodePat::node(KindSpec::Exact(NodeKind::Switch)))
}
```

- [ ] **Step 3b: Re-export** in `node_builders/mod.rs` (@45): add `IndirectBranchPat, SwitchPat, UnreachablePat` to the type list and `indirect_branch, switch, unreachable` to the fn list of the `pub use flow::{...}`.

- [ ] **Step 3c: Re-export** in `strider-pattern/src/lib.rs` (@45-49): add the same three types and three functions to the `pub use node_builders::{...}` group.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-pattern -- indirect_branch unreachable`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-pattern/src/node_builders/flow.rs crates/strider-pattern/src/node_builders/mod.rs crates/strider-pattern/src/lib.rs crates/strider-pattern/tests/control_build.rs
git commit -m "feat(pattern): switch / indirect_branch / unreachable builders"
```

---

### Task 8: Python pattern builders

**Files:**
- Modify: `crates/strider-py/src/pattern.rs` (`node_builder!` invocations after `PyRetPat` @2632; class registration @3033-3041; fn registration @3128-3133)
- Test: a Python test under the strider-py test dir (mirror an existing `strider.pattern.ret()` test)

**Interfaces:**
- Consumes: `strider_pattern::{switch, indirect_branch, unreachable}` (T7).
- Produces: `strider.pattern.{switch, indirect_branch, unreachable}()`.

- [ ] **Step 1: Write the failing test** — add a pytest (find the existing pattern test file via `grep -rl "pattern.ret\|pattern.load" crates/strider-py/tests`):

```python
def test_control_node_patterns_exist():
    import strider
    assert strider.pattern.indirect_branch() is not None
    assert strider.pattern.unreachable() is not None
    assert strider.pattern.switch() is not None
```

- [ ] **Step 2: Build + run to verify it fails**

Run: `uv run maturin develop --release && uv run pytest -k control_node_patterns -q`
Expected: FAIL — `module 'strider.pattern' has no attribute 'indirect_branch'`.

- [ ] **Step 3a: Add the `node_builder!` invocations** in `pattern.rs` after `PyRetPat` (@2632) — the `PyIndirectBranchPat`, `PyUnreachablePat`, and `PySwitchPat` blocks (root: node), each with a matching `#[pyfunction] pub fn <name>() -> Py...Pat { Py...Pat::new() }`. For `PySwitchPat`, fields `[ { pat address: address = "Constrain the dispatch address (inputs[1])." }, { pat preceded_by: preceded_by = "..." } ]` with `core: strider_pattern::switch, core_ty: strider_pattern::SwitchPat`. (Mirror the `PyRetPat` block verbatim for structure.)

- [ ] **Step 3b: Register classes** at @3033-3041:

```rust
    m.add_class::<PyIndirectBranchPat>()?;
    m.add_class::<PyUnreachablePat>()?;
    m.add_class::<PySwitchPat>()?;
```

- [ ] **Step 3c: Register constructor fns** at @3128-3133:

```rust
    add_fn!(indirect_branch);
    add_fn!(unreachable);
    add_fn!(switch);
```

- [ ] **Step 4: Build + run to verify it passes**

Run: `uv run maturin develop --release && uv run pytest -k control_node_patterns -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/pattern.rs crates/strider-py/tests
git commit -m "feat(py): switch / indirect_branch / unreachable pattern builders"
```

---

### Task 9: Gate eager `create_region` test-only

**Files:**
- Modify: `crates/strider-ir/src/builder/vars.rs` (`create_region` @~151; the `seed_current == true` behavior in `create_region_with` lives in `region.rs`)

**Interfaces:**
- Consumes: production no longer calls `create_region` (T5 deleted `build_switch_if_ladder`).
- Produces: `create_region` gated `#[cfg(any(test, feature = "test-util"))]`; production builds only via `create_region_pruned`.

- [ ] **Step 1: Prove production is clean** — this is a guard, not a behavior test. Add a check step:

Run: `grep -rn "\.create_region()" crates --include=*.rs | grep -v "create_region_pruned\|create_region_with\|create_region_skeleton" | grep -vE "test|tests|#\[cfg\(test\)\]"`
Expected: only lines inside `#[cfg(test)]` modules or test files (verify each hit's context). If any true production hit remains, STOP — it must be migrated first.

- [ ] **Step 2: Gate `create_region`** in `vars.rs` (@~147-154):

```rust
    /// Eager region constructor ... (test-only: production lifts via the pruned
    /// path `create_region_pruned`; the eager all-variables form is used only by
    /// tests and `strider-ir-test-utils`).
    #[cfg(any(test, feature = "test-util"))]
    pub fn create_region(&mut self) -> Result<RegionId> {
        let vn_ids: Vec<_> = self.function().vn_ids().collect();
        self.create_region_with(&vn_ids, true)
    }
```

- [ ] **Step 3: Build production + tests**

Run: `cargo build -p strider-lift && cargo test -p strider-ir 2>&1 | grep -E "test result|error"`
Expected: production `strider-lift` builds (doesn't reference `create_region`); strider-ir tests (which enable `cfg(test)`) still pass. Then `cargo build --workspace 2>&1 | grep -E "error|Finished"` — if any non-test crate fails to find `create_region`, it was a production caller; migrate or re-gate.

- [ ] **Step 4: Verify the full workspace test target** (test-util feature makes it available to `strider-ir-test-utils`):

Run: `cargo test --workspace --tests 2>&1 | grep -E "test result: FAILED|error\[" || echo clean`
Expected: `clean`.

(If `strider-ir-test-utils`' `RegisterSet` calls `create_region` outside `cfg(test)`, ensure that crate builds with the `test-util` feature — check its `Cargo.toml` `[features]` and how `record_register_arg_carriers`, already `#[cfg(any(test, feature = "test-util"))]`, is enabled. Mirror that exactly.)

- [ ] **Step 5: Commit**

```bash
git add crates/strider-ir/src/builder/vars.rs
git commit -m "refactor(ir): gate eager create_region test-only (production is 100% pruned SSA)"
```

---

### Task 10: End-to-end integration + full gate

**Files:**
- Test: `crates/strider-orchestrator/tests/` (a resolved-jump-table function analyzes to a `Switch`) — reuse an existing orchestrator integration fixture that resolves a `Multiple` indirect branch.

**Interfaces:**
- Consumes: all prior tasks.

- [ ] **Step 1: Write the integration test** — find an existing orchestrator/lift test that exercises a resolved jump table (`grep -rln "Switch\|Multiple\|jump.*table\|known_targets" crates/strider-orchestrator/tests crates/strider-lift/tests`). Assert the analyzed `Function` contains a `NodeKind::Switch` and NO if-ladder dispatcher artifacts, and that `analyze` returns `unresolved == []`. If no such fixture exists, add one that builds a small jump-table binary/CFG and drives `Strider::analyze`.

- [ ] **Step 2: Run it to verify it fails without the feature** (skip if writing on top of the implemented tasks — this is the acceptance test).

- [ ] **Step 3:** No new implementation — this task is the acceptance gate.

- [ ] **Step 4: Full gate**

```bash
# NOTE: the feature branch is based on pruned-ssa, which has a pre-existing
# example-build breakage (the 2-arg build_cfg fixed only on the constfold
# examples commit). Use --tests to skip examples here; the constfold rebase
# (Finalization step 2) restores a clean full-workspace build incl. examples.
cargo test --workspace --tests 2>&1 | grep -E "test result: FAILED|error\[|could not compile" || echo "workspace clean"
cargo clippy --workspace 2>&1 | grep -E "error:" || echo "clippy clean"
uv run maturin develop --release && uv run pytest crates/strider-py/tests/python -q 2>&1 | tail -3
# Kernel repros still analyze:
uv run python <the 6-kernel repro script> 2>&1 | grep -c "OK: analyzed"   # expect 6
```

Expected: workspace tests 0 failed, clippy clean, pytest all pass, 6 kernels OK.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-orchestrator/tests
git commit -m "test: switch-node end-to-end (resolved jump table lifts to Switch)"
```

---

## Finalization (controller, after all tasks pass review)

1. **Squash** all task commits into ONE feature commit on `feat/2026-07-04-switch-node`:
   `git reset --soft <feat-branch-base>` (the pruned-ssa tip the branch started from) then
   `git commit` with a full feature message (include the spec-doc changes).
2. **Rebase constfold** onto the squashed feature commit so the stack becomes
   `pruned-ssa → switch-feature → root-kind gate → examples`:
   `git rebase --onto feat/2026-07-04-switch-node <old-feat-base> perf/2026-07-04-constfold-flagcmp`.
3. Force-push both branches (`--force-with-lease`).

## Self-Review Notes (plan author)

- **Spec coverage:** §1 node → T1; §1 side table → T2; §2 builder → T3; §5 validation → T4; §3 lifter → T5; §4 collapse → T6; §5 patterns → T7 (Rust) + T8 (Python); §6 dot → T3; §7 create_region removal → T9; §6 testing + non-goals → T10. All covered.
- **Type consistency:** `switch_targets(node) -> &[u64]` / `set_switch_targets(node, Vec<u64>)`, `build_switch(address, &[(RegionId, u64)])`, `ValidationError::{EmptySwitchTargets, SwitchTargetArityMismatch}`, `NodeKind::Switch`, `sig!(inputs: [CTRL, INT_VAL], outputs: [CTRL]; out_tail: CTRL)` — used identically across tasks.
- **Known adaptation points** (flagged inline for implementers): exact `Signature`/`SlotList` field/constructor names (T1); `require_value_kind` name (T3); the `OptCtx::new(None)` arg form (T6); building fixtures containing `IndirectBranch`/`Unreachable` (T7); the `test-util` feature wiring for `strider-ir-test-utils` (T9).
</content>
