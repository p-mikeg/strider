# Optimizer Rework — Workstream 3: CFG-detach split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Extract the CFG-edge surgery (removing dead `Region` predecessor slots + matching `Phi`/`MemPhi` value slots) out of `DeadBranchElimination` into a standalone `CfgDetach` pass that uses `cfg_reachable` from `walk.rs`, behind a single `Function::remove_region_predecessor` primitive.

**Architecture:** Today `DeadBranchElimination` does three things at once: recognise `If(const)`, redirect live control, and perform the dead-`Region`-predecessor surgery (`strip_dead_region_inputs`). WS3 splits the surgery out. After the split:
- `DeadBranchElimination` only redirects live control (`replace_value`) and, when the dead subgraph does NOT escape, detaches the folded `If` (severs its inputs). It performs no `Region`/`Phi` slot removal.
- `CfgDetach` (new) walks `cfg_reachable(entry)` and, for every reachable `Region`, removes each predecessor slot whose control-input producer is unreachable — plus the matching `Phi`/`MemPhi` value slot — via the new `Function::remove_region_predecessor`. The escape case is handled for free: when DBE leaves the `If` attached (escaping subgraph), the dead edge's producer stays reachable, so `CfgDetach` correctly skips it.

This is the trickiest workstream — the existing `dead_branch` unit tests assert that `DeadBranchElimination` *alone* strips the dead `Region`; those move to a DBE+CfgDetach mini-pipeline.

**Tech Stack:** Rust; `strider-ir` (`Function`, `Graph::remove_node_input`, `walk::cfg_reachable`), `strider-analyze` opt passes/pipeline.

**Spec:** `docs/superpowers/specs/2026-06-01-optimizer-rework-design.md` (D5, item 2).

**Scope boundary:** WS3 does NOT touch `if_cond_inversion` (its `update_input` branch-swap is a separate WS), `RedundantPhis` (keeps its single-pred collapse), or the still-remaining manual-fingerprint sites in `indirect_branch_resolve`/`worklist`/`mem_walk`. The global "no manual fingerprint mutation in opt/" enforcement test is the FINAL task of WS3 (Task 6), now that value-replacement (WS2) and region-pred removal (WS3) both have SSoT homes — but it ALLOWLISTS the not-yet-migrated sites explicitly so it documents the remaining debt instead of failing.

---

## File structure

- `crates/strider-ir/src/function.rs` — add `remove_region_predecessor` (next to `replace_value`).
- `crates/strider-analyze/src/opt/cfg_detach/mod.rs` — NEW pass + `#[cfg(test)] mod tests`.
- `crates/strider-analyze/src/opt/cfg_detach/tests.rs` — NEW.
- `crates/strider-analyze/src/opt/dead_branch/mod.rs` — drop `strip_dead_region_inputs`; DBE redirect + escape-gated detach only.
- `crates/strider-analyze/src/opt/dead_branch/tests.rs` — migrate strip-asserting tests to a DBE+CfgDetach helper.
- `crates/strider-analyze/src/opt/mod.rs` — declare `mod cfg_detach`, `pub use`, add `CfgDetach` to both destructive pipelines.

---

### Task 1: `Function::remove_region_predecessor` SSoT

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (after `replace_value`)
- Test: a Function unit test near `replace_value`'s test

The single primitive for removing a dead `Region` predecessor. Encapsulates the index-juggling that currently lives inline in `dead_branch::strip_dead_region_inputs`.

- [ ] **Step 1: Write the failing test**

Build a `Region` with 2 control predecessors and a `Phi` (or `MemPhi`) consuming the region's phi-token output with 2 value inputs. Call `remove_region_predecessor(region, 0)`; assert the Region drops to 1 control input AND the Phi drops to `[phi_token, val_pred1]` (the slot for pred 0, at Phi input index 1, is gone; pred 1's value remains). Copy the construction idiom from `dead_branch/tests.rs::var_phi_loses_dead_slot` (it builds exactly this shape via `FunctionBuilder`), but call the new primitive directly instead of running DBE.

```rust
#[test]
fn remove_region_predecessor_drops_region_slot_and_matching_phi_slot() -> crate::error::Result<()> {
    // Build: Region with 2 ctrl preds; a value Phi over it with 2 value inputs.
    // ... (mirror var_phi_loses_dead_slot's builder; capture region NodeId + phi NodeId) ...
    let pred1_val = /* the Phi's input at index 2 (pred 1's value), captured pre-removal */;
    f.remove_region_predecessor(region, 0)?;
    assert_eq!(f.node_inputs(region).len(), 1, "region drops to 1 ctrl input");
    let phi_inputs: Vec<_> = f.node_inputs(phi).into_iter().collect();
    assert_eq!(phi_inputs.len(), 2, "phi: [token, surviving value]");
    assert_eq!(phi_inputs[1], pred1_val, "surviving slot is pred 1's value");
    Ok(())
}
```

- [ ] **Step 2: Run — verify FAIL** (`no method named remove_region_predecessor`).
`cargo test -p strider-ir remove_region_predecessor -- --nocapture`

- [ ] **Step 3: Implement**

After `replace_value` in `function.rs`:

```rust
/// Removes predecessor slot `pred_index` from a `Region` and the matching
/// value slot from every `Phi`/`MemPhi` that consumes the Region's
/// phi-token output — the single structural primitive for dropping a dead
/// control edge into a join.
///
/// A `Region` produces `[control, phi_token]`; a `Phi`/`MemPhi` over it has
/// inputs `[phi_token, val_pred0, val_pred1, …]`, so the value for Region
/// predecessor `i` lives at phi input `i + 1`. Region/Phi nodes are exempt
/// from the asm-fingerprint non-empty check, so no fingerprint work is needed.
///
/// No-op-safe: out-of-range `pred_index` (already shifted by a prior removal)
/// is skipped per-node via bounds checks.
///
/// # Errors
/// Propagates [`Graph::remove_node_input`]'s error arm.
pub fn remove_region_predecessor(
    &mut self,
    region: crate::node::NodeId,
    pred_index: u32,
) -> crate::error::Result<()> {
    debug_assert!(
        matches!(self.node_kind(region), crate::node::NodeKind::Region),
        "remove_region_predecessor: node is not a Region",
    );
    // Region outputs: [ctrl_out, phi_out]. Collect phi consumers BEFORE
    // mutating (output_uses borrows the use-list).
    let outputs = self.node_outputs(region);
    if outputs.len() >= 2 {
        let phi_out = outputs[1];
        let phi_nodes: Vec<crate::node::NodeId> =
            self.output_uses(phi_out).map(|(n, _)| n).collect();
        let phi_input_idx = pred_index + 1;
        for phi in phi_nodes {
            if phi_input_idx < self.node_inputs(phi).len() as u32 {
                self.remove_node_input(phi, phi_input_idx)?;
            }
        }
    }
    if pred_index < self.node_inputs(region).len() as u32 {
        self.remove_node_input(region, pred_index)?;
    }
    Ok(())
}
```

Confirm `node_outputs`, `output_uses`, `node_inputs`, `remove_node_input` are reachable on `Function` (via Deref to Graph — they are, `dead_branch` calls them on a `RewriteCtx`). Adjust the `Result` alias to match `remove_node_input`'s (`crate::error::Result`).

- [ ] **Step 4: Run — verify PASS.** Then `cargo test -p strider-ir` + `cargo clippy -p strider-ir --no-deps`.

- [ ] **Step 5: Commit**
```bash
git add crates/strider-ir/src/function.rs
git commit -m "feat(strider-ir): add Function::remove_region_predecessor structural SSoT"
```

---

### Task 2: `CfgDetach` pass

**Files:**
- Create: `crates/strider-analyze/src/opt/cfg_detach/mod.rs`
- Create: `crates/strider-analyze/src/opt/cfg_detach/tests.rs`
- Modify: `crates/strider-analyze/src/opt/mod.rs` (declare module + `pub use`)

- [ ] **Step 1: Write the failing test** (`cfg_detach/tests.rs`)

Build the `make_if_fn(false)` shape from `dead_branch/tests.rs` (copy the helper). Run `DeadBranchElimination` ONCE (it redirects live control + detaches the folded If, leaving the dead Region's control input pointing at the now-unreachable If). Then run `CfgDetach`; assert the dead Region drops to 0 control inputs:

```rust
#[test]
fn cfg_detach_removes_dead_region_pred_after_dbe() -> Result<()> {
    let mut fg = make_if_fn(false)?;
    DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed(), "CfgDetach removes the dead predecessor");
    assert_eq!(count_regions_with_n_inputs(&fg, 0), 1, "dead Region now 0 inputs");
    Ok(())
}
```
(Bring `count_regions_with_n_inputs` + `make_if_fn` into this test module — copy them, or `pub(crate)` them in `dead_branch::tests` and re-import. Copying the two tiny helpers is simplest.)

- [ ] **Step 2: Run — verify FAIL** (`CfgDetach` undefined).

- [ ] **Step 3: Implement the pass** (`cfg_detach/mod.rs`)

```rust
//! `CfgDetach` — removes dead control-flow edges into `Region` joins.
//!
//! After `DeadBranchElimination` redirects a constant `If`'s live branch and
//! detaches the folded `If`, the dead branch's control producer becomes
//! unreachable from the entry. This pass walks `cfg_reachable(entry)` and, for
//! every reachable `Region`, drops each predecessor slot whose control producer
//! is unreachable (plus the matching `Phi`/`MemPhi` value slot) via
//! `Function::remove_region_predecessor`.
//!
//! It is the single home for dead-`Region`-predecessor surgery: no other pass
//! mutates `Region`/`Phi` predecessor structure. When a dead subgraph still
//! escapes to live data (so DBE left the `If` attached), the dead edge's
//! producer is still reachable and this pass correctly leaves it alone.

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::walk::cfg_reachable;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Removes `Region` predecessor slots whose control producer is unreachable.
#[derive(Clone, Copy)]
pub struct CfgDetach;

impl Optimizer for CfgDetach {
    fn optimize(
        &self,
        function: &mut strider_ir::Function,
        _ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let entry = function
            .entry()
            .expect("Optimizer::optimize: function must be built");
        let reachable = cfg_reachable(function.graph(), entry);

        // Collect (region, dead_pred_index) for every reachable Region whose
        // control input at that index has an unreachable producer. Gather all
        // before mutating, then remove per region in DESCENDING index order so
        // earlier removals don't shift the indices of later ones.
        let mut dead: Vec<(NodeId, u32)> = Vec::new();
        for region in function.graph().all_node_ids() {
            if !matches!(function.node_kind(region), NodeKind::Region) {
                continue;
            }
            if !reachable.contains(region) {
                continue; // whole region is dead; orphan cleanup handles it
            }
            for (idx, input) in function.node_inputs(region).into_iter().enumerate() {
                let producer = function.output_definition(input).0;
                if !reachable.contains(producer) {
                    dead.push((region, idx as u32));
                }
            }
        }

        if dead.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }
        // Descending index within each region keeps removals index-stable.
        dead.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        for (region, idx) in dead {
            function.remove_region_predecessor(region, idx)?;
        }
        Ok(OptimizationResult::Changed)
    }
}
```

Confirm: `function.graph()` (added in WS1), `cfg_reachable(graph, entry) -> DenseEntitySet<NodeId>` with `.contains` (walk/mod.rs:30), `output_definition`, `all_node_ids`, `node_inputs` iteration yielding `NodeOutputId`. If `node_inputs().into_iter().enumerate()` yields the input's `NodeOutputId`, good; otherwise use `nth_input`. Verify the `Optimizer`/`OptCtx`/`OptimizationResult` import paths match `dead_branch/mod.rs`.

In `crates/strider-analyze/src/opt/mod.rs`: add `mod cfg_detach;` and `pub use cfg_detach::CfgDetach;` next to the `dead_branch` declarations (~lines 60-64, 80-81).

- [ ] **Step 4: Run — verify PASS.** `cargo test -p strider-analyze --lib cfg_detach`.

- [ ] **Step 5: Commit**
```bash
git add crates/strider-analyze/src/opt/cfg_detach/ crates/strider-analyze/src/opt/mod.rs
git commit -m "feat(strider-analyze): add CfgDetach pass (dead Region-predecessor removal)"
```

---

### Task 3: Simplify `DeadBranchElimination` to redirect + escape-gated detach

**Files:**
- Modify: `crates/strider-analyze/src/opt/dead_branch/mod.rs`
- Modify: `crates/strider-analyze/src/opt/dead_branch/tests.rs`

- [ ] **Step 1: Remove the surgery from DBE**

In `dead_branch/mod.rs`:
- DELETE `strip_dead_region_inputs` entirely.
- In `try_eliminate_dead_branch`, after `ctx.replace_value(live_ctrl, ctrl_in)?`, replace the `if !dead_subgraph_escapes { strip_dead_region_inputs(ctx, &dead_uses, node_id)?; }` block with:
  ```rust
  // Detach the folded If when its dead subgraph is self-contained, so its
  // dead control output becomes unreachable from entry. CfgDetach then
  // removes the now-dead Region predecessor slot(s) it fed. When the
  // subgraph escapes, leave the If attached (RedundantPhis tears the
  // live↔dead data edges apart on later iterations; a future pass detaches).
  if !dead_subgraph_escapes {
      ctx.detach_node_inputs(node_id);
  }
  ```
- KEEP: `collect_dead_subgraph`, `dead_subgraph_has_live_data_consumer`, the idempotency logic, `dead_uses_all_zero_input`. The `dead_uses` vector is still needed for the escape analysis (`collect_dead_subgraph` consumes it).
- Update the DBE doc comment to say it no longer strips Region predecessors — that is `CfgDetach`'s job.

- [ ] **Step 2: Migrate the strip-asserting tests**

In `dead_branch/tests.rs`, add a helper that runs DBE then CfgDetach to fixpoint over a function:
```rust
use crate::opt::CfgDetach;

/// Run DBE + CfgDetach to convergence (the split passes that together do what
/// DBE alone used to do).
fn dbe_then_detach(fg: &mut strider_ir::Function) -> Result<()> {
    let mut p = OptimizerPipeline::new();
    p.add(DeadBranchElimination);
    p.add(CfgDetach);
    p.run(fg, &crate::opt::OptCtx::empty())?;
    Ok(())
}
```
Then update the tests whose assertions depend on the Region/Phi slot removal:
- `dead_branch_false`, `dead_branch_true`: replace the single `DeadBranchElimination.optimize(...)` call with `dbe_then_detach(&mut fg)?;` and keep the `count_regions_with_n_inputs(&fg, 0) == 1` / `== 1`(... 2 with 1 input) assertions. (Drop the `result.changed()` assert, or assert the pipeline ran; the count assertions are the real check.)
- `dead_branch_handles_dead_ctrl_wired_at_multiple_slots`: this exercises the descending-index removal that now lives in `remove_region_predecessor`/`CfgDetach`. It builds a deliberately-invalid duplicate-slot shape and calls DBE directly (no validation). After the split, the duplicate-slot removal happens in CfgDetach. Run `DeadBranchElimination.optimize` (redirect+detach) then `CfgDetach.optimize` directly (NOT the validating pipeline, since the shape is intentionally odd), and assert `node_inputs(false_region).len() == 0`. If the duplicate-slot shape makes `cfg_reachable` behave unexpectedly, MOVE this test to `cfg_detach/tests.rs` as a `remove_region_predecessor` duplicate-slot test instead (it is fundamentally testing the index-stable removal, which is now `remove_region_predecessor`'s contract — a direct `remove_region_predecessor` test is the cleaner home). Use judgment; the behaviour under test (both duplicate dead slots removed, 0 remaining) must be pinned SOMEWHERE.
- `var_phi_loses_dead_slot`: replace the `DeadBranchElimination.optimize(...)` with `dbe_then_detach(&mut fg)?;`, keep the phi-input-count assertion.
- `dead_branch_with_non_region_dead_consumer`: KEEP AS-IS calling `DeadBranchElimination.optimize` alone — it asserts the ESCAPE behaviour (If retains 2 inputs, no detach), which is entirely DBE's responsibility and unchanged. Verify it still passes (DBE must NOT detach when escaping).
- `nested_if_true_eliminated`: it runs a pipeline (ConstantFold + DBE + RedundantPhis). ADD `pipeline.add(CfgDetach);` after `DeadBranchElimination` so the dead Region preds get removed; the `if_count == 0` assertion must still hold.
- `dead_branch_non_const_no_change`: unchanged (DBE alone, no const → NoChange).

- [ ] **Step 3: Run the dead_branch + cfg_detach tests**
```
cargo test -p strider-analyze --lib dead_branch cfg_detach
```
All green. If `dead_branch_with_non_region_dead_consumer` fails, the escape gate in DBE is wrong — fix DBE, not the test (the test pins a real soundness property).

- [ ] **Step 4: Commit**
```bash
git add crates/strider-analyze/src/opt/dead_branch/
git commit -m "refactor(strider-analyze): DeadBranchElimination redirects + escape-detaches only; surgery moves to CfgDetach"
```

---

### Task 4: Wire `CfgDetach` into the destructive pipelines

**Files:**
- Modify: `crates/strider-analyze/src/opt/mod.rs` (`destructive_default_pipeline` ~166, the stable+destructive builder ~197)

- [ ] **Step 1: Add CfgDetach after DeadBranchElimination**

In `destructive_default_pipeline()` (and the stable+destructive builder around line 197), add `p.add(CfgDetach);` immediately after `p.add(DeadBranchElimination);`. Update the doc-comment pass lists in those functions to mention `CfgDetach` (the "Removes If(const) branches and strips dead control edges" line for `DeadBranchElimination` at mod.rs:22 should split: DBE redirects/detaches; CfgDetach strips dead control edges).

Also check `crates/strider-analyze/src/indirect_resolver.rs` (the cfg-time mini-IR resolver at ~line 320 adds `RedundantPhis`): it builds `ConstantFold + KnownBits + LoadReadOnly + RedundantPhis`. Determine whether the mini-resolver produces `If(const)` shapes needing CfgDetach. If it does NOT add `DeadBranchElimination` today, it does not need `CfgDetach` either — leave it. Only add `CfgDetach` where `DeadBranchElimination` is present. Document the decision in the commit message.

- [ ] **Step 2: Full analyze crate tests**
`cargo test -p strider-analyze` → all green (lib + integration; the orchestrator drives the destructive pipeline over real fixtures, so this exercises DBE+CfgDetach end-to-end).

- [ ] **Step 3: Commit**
```bash
git add crates/strider-analyze/src/opt/mod.rs
git commit -m "feat(strider-analyze): run CfgDetach after DeadBranchElimination in destructive pipelines"
```

---

### Task 5: Regression backstop

- [ ] **Step 1:** `cargo test --workspace` → 0 failures.
- [ ] **Step 2:** `cargo run -p strider-analyze --example orchestrator_demo` → exit 0, three HTML dumps regenerate.
- [ ] **Step 3:** Commit any incidental fix:
```bash
git add -A && git commit -m "test: WS3 regression backstop for CfgDetach split" || echo "nothing to commit"
```

---

### Task 6: Enforcement guard — no manual fingerprint mutation in migrated passes

Now that value-replacement (`replace_value`, WS2) and region-pred removal (`remove_region_predecessor`, WS3) have SSoT homes, pin the progress with a source-level test so regressions can't re-introduce hand-written pairs.

**Files:**
- Create or extend a test in `crates/strider-analyze/tests/` (an integration test that reads source files) — OR a `#[test]` in `opt/mod.rs`'s test module.

- [ ] **Step 1: Write the guard test**

```rust
/// Pins WS2/WS3 progress: the value-rewrite + CFG-detach passes must route all
/// fingerprint-bearing mutations through the SSoT primitives (`replace_value`,
/// `remove_region_predecessor`), never hand-written `extend_asm_fingerprint_from`.
/// The still-unmigrated sites are listed explicitly so this test documents the
/// remaining debt instead of silently passing.
#[test]
fn migrated_passes_have_no_manual_fingerprint_mutation() {
    let migrated = [
        "src/opt/load_readonly/mod.rs",
        "src/opt/redundant_phis/mod.rs",
        "src/opt/dead_branch/mod.rs",
        "src/opt/load_forward/mod.rs",
        "src/opt/cfg_detach/mod.rs",
    ];
    for rel in migrated {
        let path = concat_crate_root(rel); // CARGO_MANIFEST_DIR + rel
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(
            !src.contains("extend_asm_fingerprint_from"),
            "{rel} must route fingerprint propagation through replace_value / \
             remove_region_predecessor, not hand-written extend_asm_fingerprint_from",
        );
    }
    // NOT YET migrated (tracked debt, allowed to still contain manual sites):
    //   if_cond_inversion, indirect_branch_resolve/*, worklist, mem_walk.
}
```
Implement `concat_crate_root` via `env!("CARGO_MANIFEST_DIR")`. If a test reading source files doesn't fit the crate's conventions, put it under `crates/strider-analyze/tests/fingerprint_ssot.rs` as an integration test. Confirm the five files genuinely contain no `extend_asm_fingerprint_from` after Tasks 1-4 (run the grep from WS2 Task 4 Step 3 plus `grep -n extend_asm_fingerprint_from crates/strider-analyze/src/opt/cfg_detach/mod.rs`).

- [ ] **Step 2: Run** `cargo test -p strider-analyze migrated_passes_have_no_manual_fingerprint_mutation` → PASS.

- [ ] **Step 3: Commit**
```bash
git add -A
git commit -m "test(strider-analyze): guard migrated passes against manual fingerprint mutation"
```

---

## Self-review

**Spec coverage (D5 / item 2):**
- Standalone CFG-detach pass using `walk.rs` — Task 2 (`CfgDetach` + `cfg_reachable`).
- Surgery removed from DBE/phi-elimination — Task 3 (DBE), `RedundantPhis` untouched (its single-pred collapse is a different concern, in scope nowhere).
- Structural SSoT primitive — Task 1 (`remove_region_predecessor`).
- Pipeline wiring — Task 4.
- Enforcement of the no-manual-fingerprint intent (item 3 tail) — Task 6.

**Placeholder scan:** Task 1's and Task 2's tests have `// ...` construction sketches that the implementer fills by copying `dead_branch/tests.rs::var_phi_loses_dead_slot` / `make_if_fn`. The required assertions are explicit. Every implementation step has complete code.

**Type consistency:** `remove_region_predecessor(region: NodeId, pred_index: u32) -> Result<()>` (Task 1) is the only API `CfgDetach` (Task 2) calls. `CfgDetach` unit struct + `Optimizer` impl mirror `DeadBranchElimination`'s shape exactly.

**Risk notes:**
- The escape soundness property (`dead_branch_with_non_region_dead_consumer`) is preserved by KEEPING the escape analysis in DBE and only detaching the If when non-escaping — CfgDetach then naturally skips still-reachable dead edges. This is the load-bearing correctness argument; that test must pass calling DBE alone.
- `cfg_reachable` is control-edges-only (walk/mod.rs:30) — exactly the right reachability notion for "is this Region predecessor's producer still live control."
- The duplicate-slot test may relocate to a direct `remove_region_predecessor` test (Task 3 Step 2) — its real subject is the index-stable removal contract, which now lives in the primitive.
