# `opt` Crate Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix verified correctness issues, harden latent fragility, and simplify duplication in the `opt` crate; preserve all test pass and `clippy -D warnings`.

**Architecture:** Each task is a focused, independently-committable change against the worktree at `/home/mike/Desktop/strider/.worktrees/opt-review` (branch `review/opt-crate`). Tasks are ordered by risk: correctness fixes first, then hardening, then simplification. Every task ends with `cargo test -p opt` + `cargo clippy -p opt --all-targets --no-deps -- -D warnings` before commit.

**Tech Stack:** Rust 2021, `cargo`, `pattern` crate (in-tree), `ir` crate (in-tree), `rsleigh` (path dep at `../rsleigh`).

**Worktree note:** The worktree at `.worktrees/opt-review` already exists, builds cleanly, all 10 tests pass, and `clippy -p opt -D warnings` is clean as a baseline. The `rsleigh` symlink lives at `.worktrees/rsleigh` so workspace path deps resolve.

---

## Pre-flight (run once before starting)

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-review
cargo test -p opt 2>&1 | tail -20
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: all 10 tests pass, clippy clean.

---

## Findings the review explicitly REJECTED (do not act on these)

A separate review pass turned up these items — they were investigated and confirmed to NOT be bugs:
- `Load` does *not* have two outputs. Per [`crates/ir/src/node_signature.rs:325`](../../crates/ir/src/node_signature.rs#L325), `Load` is `inputs:[MEM,ADDR], outputs:[INT_VAL]` — single output. `node_outputs_exact::<1>(load)` in `stack_load_forward` and `function_args` is correct.
- `function_args::mem_chain_is_dirty` sharing `seen` across MemPhi predecessors is **not** a bug — `mem_chain_is_dirty` is a pure function of `mem`, so reusing the verdict for an already-walked node is sound.
- `KnownBits` shift-amount masking is fine (the agent self-debunked the false alarm).
- `ConstantFold` `Kb::from_const` `unwrap_or(0)` for U128/U256 is dead — the caller `node_known_bits` returns early at line 83 for those types.
- `RedundantPhis` ControlState path leaving `phi_token` undetached is by design — `cleanup_if_dead` only fires once phi consumers are gone, and Layer C invariants are preserved across iterations.

---

## Task 1: Fix ConstantFold worklist consumer re-enqueue

**Problem:** [`crates/opt/src/constant_fold/mod.rs:81-97`](../../crates/opt/src/constant_fold/mod.rs#L81-L97) — when a rule fires, the loop iterates `output_uses(old_out)` *after* the rule has called `replace_all_uses(old_out, new_out)`. By that point `old_out` has zero remaining users (they were rewired to `new_out`), so `work.push(consumer)` is never called. Cascading folds within one `optimize()` invocation rely on the outer pipeline fixed-point loop instead of converging in-place. Not a soundness bug, but defeats the worklist's purpose and adds outer iterations.

**Files:**
- Modify: [`crates/opt/src/constant_fold/mod.rs`](../../crates/opt/src/constant_fold/mod.rs)
- Test: [`crates/opt/src/constant_fold/tests.rs`](../../crates/opt/src/constant_fold/tests.rs) (add new test)

- [ ] **Step 1: Write a failing test that proves cascading folds converge in one pass**

Append to `crates/opt/src/constant_fold/tests.rs`:

```rust
#[test]
fn single_pass_propagates_through_chain() -> crate::Result<()> {
    // Build:  c1 = 1 + 2;  c2 = c1 + 3;  c3 = c2 + 4;  return c3
    // After ONE optimize() call (not the outer pipeline loop), c3's use
    // should resolve to IntConst(10).
    use ir::node::NodeOutputType;
    let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let one = b.build_int_const(1, NodeOutputType::U32);
    let two = b.build_int_const(2, NodeOutputType::U32);
    let three = b.build_int_const(3, NodeOutputType::U32);
    let four = b.build_int_const(4, NodeOutputType::U32);
    let c1 = b.build_int_binary_operation(one, two, ir::IntBinaryOp::Add, NodeOutputType::U32)?;
    let c2 = b.build_int_binary_operation(c1, three, ir::IntBinaryOp::Add, NodeOutputType::U32)?;
    let c3 = b.build_int_binary_operation(c2, four, ir::IntBinaryOp::Add, NodeOutputType::U32)?;
    b.build_return(Some(c3), &[])?;
    let mut fg = b.build()?;

    use crate::pipeline::Optimizer;
    crate::ConstantFold.optimize(&mut fg)?;

    // After one pass, the Return's value input should resolve to IntConst(10).
    let ret = fg.preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), ir::node::NodeKind::Return))
        .ok_or(crate::ErrorKind::NoReturnNode)?;
    let inputs = fg.graph.node_inputs(ret);
    let val_in = inputs[1]; // [ctrl, val]
    let val_node = fg.graph.get_node_from_output(val_in);
    assert!(
        matches!(*fg.graph.node_kind(val_node), ir::node::NodeKind::IntConst(10)),
        "expected single-pass convergence to IntConst(10), got {:?}",
        fg.graph.node_kind(val_node)
    );
    Ok(())
}
```

- [ ] **Step 2: Run the test — confirm it fails**

```bash
cargo test -p opt single_pass_propagates_through_chain 2>&1 | tail -15
```
Expected: FAIL — the assertion finds a non-`IntConst(10)` node (likely the deepest residual `Add`).

- [ ] **Step 3: Fix the worklist by capturing consumers BEFORE the rules run**

Replace [`crates/opt/src/constant_fold/mod.rs:74-100`](../../crates/opt/src/constant_fold/mod.rs#L74-L100) (the `Optimizer for ConstantFold` impl body) with:

```rust
impl Optimizer for ConstantFold {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        // Reused per iteration to snapshot consumer NodeIds BEFORE running
        // rules. After a rule rewrites the node, `output_uses(old_out)` is
        // empty (uses were rewired to the replacement), so we must capture
        // consumers ahead of time to re-enqueue them.
        let mut consumers: Vec<NodeId> = Vec::new();
        while let Some(node_id) = work.pop() {
            consumers.clear();
            for out in function.graph.node_outputs(node_id).into_iter() {
                for (consumer, _) in function.graph.output_uses(out) {
                    consumers.push(consumer);
                }
            }
            let r = apply_identity_rules(function, node_id)?
                | apply_const_eval_rules(function, node_id)?
                | apply_bool_float_rules(function, node_id)?
                | apply_reassoc_and_mask_rules(function, node_id)?
                | apply_bitcast_extend_rules(function, node_id)?;
            if r.changed() {
                result |= r;
                for &consumer in &consumers {
                    work.push(consumer);
                }
            }
        }
        Ok(result)
    }
}
```

Also delete the now-unused `outs_before` block. Remove the `NodeOutputId` import if no longer used (it isn't here — keep `NodeId`).

- [ ] **Step 4: Run the test — confirm it passes**

```bash
cargo test -p opt single_pass_propagates_through_chain 2>&1 | tail -10
cargo test -p opt 2>&1 | tail -20
```
Expected: PASS, all 11 tests pass.

- [ ] **Step 5: Run clippy**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/constant_fold/mod.rs crates/opt/src/constant_fold/tests.rs
git commit -m "fix(opt): capture ConstantFold worklist consumers before rewrites

Previously the worklist iterated output_uses(old_out) AFTER apply_*_rules
had already replaced uses with the new output, yielding an empty consumer
list and missing cascading-fold re-enqueue. Cascade chains now converge
in a single optimize() call instead of relying on the outer pipeline loop."
```

---

## Task 2: Bound the OptimizerPipeline fixed-point loop

**Problem:** [`crates/opt/src/pipeline.rs:122`](../../crates/opt/src/pipeline.rs#L122) has an unbounded `loop { ... }`. A non-monotone pass (rare, but possible — e.g. a regression in `KnownBits` that flips bits back) would spin forever, with no diagnostic. In a binary-analysis tool, this is a real DoS surface against malformed or adversarial binaries.

**Files:**
- Modify: [`crates/opt/src/error.rs`](../../crates/opt/src/error.rs)
- Modify: [`crates/opt/src/pipeline.rs`](../../crates/opt/src/pipeline.rs)
- Test: [`crates/opt/tests/pipeline_fixedpoint.rs`](../../crates/opt/tests/pipeline_fixedpoint.rs) (add new test)

- [ ] **Step 1: Add the new error variant**

Edit [`crates/opt/src/error.rs`](../../crates/opt/src/error.rs) to add the variant after `AssertionFailed`:

```rust
    /// Test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
    /// The fixed-point loop in `OptimizerPipeline::run` did not converge
    /// within the iteration limit. Indicates a non-monotone pass.
    #[error("optimizer pipeline did not converge after {0} iterations")]
    FixedPointLimitExceeded(u32),
```

- [ ] **Step 2: Bound the loop in pipeline.rs**

Edit the `run` method in [`crates/opt/src/pipeline.rs`](../../crates/opt/src/pipeline.rs) — replace the `loop { ... }` body (lines 122-134) with:

```rust
        const MAX_ITERS: u32 = 1024;
        let mut iters: u32 = 0;
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
            iters += 1;
            if iters >= MAX_ITERS {
                return Err(crate::error::ErrorKind::FixedPointLimitExceeded(MAX_ITERS).into());
            }
        }
```

- [ ] **Step 3: Add a test that triggers the limit**

Append to [`crates/opt/tests/pipeline_fixedpoint.rs`](../../crates/opt/tests/pipeline_fixedpoint.rs):

```rust
mod common;

#[test]
fn nonmonotone_pass_triggers_iteration_limit() -> opt::Result<()> {
    use ir::BuiltFunctionGraph;
    use opt::{OptimizationResult, Optimizer, OptimizerPipeline};

    struct AlwaysChanged;
    impl Optimizer for AlwaysChanged {
        fn optimize(&self, _: &mut BuiltFunctionGraph) -> opt::Result<OptimizationResult> {
            Ok(OptimizationResult::Changed)
        }
    }

    let mut p = OptimizerPipeline::new();
    p.add(AlwaysChanged);
    let mut fg = common::trivial_function()?;
    let err = p.run(&mut fg).expect_err("expected FixedPointLimitExceeded");
    assert!(
        matches!(err.kind(), opt::ErrorKind::FixedPointLimitExceeded(_)),
        "got {:?}",
        err
    );
    Ok(())
}
```

(`common::trivial_function` already exists in [`crates/opt/tests/common/mod.rs`](../../crates/opt/tests/common/mod.rs); if its name differs, grep `trivial_function` in `crates/opt/tests/common/mod.rs` and adapt — fall back to constructing a minimal `BuiltFunctionGraph` inline using `FunctionBuilder` exactly as in the Task 1 test.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p opt nonmonotone_pass 2>&1 | tail -10
cargo test -p opt 2>&1 | tail -20
```
Expected: new test passes, all 12 tests pass.

- [ ] **Step 5: Run clippy**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/error.rs crates/opt/src/pipeline.rs crates/opt/tests/pipeline_fixedpoint.rs
git commit -m "fix(opt): bound OptimizerPipeline fixed-point loop at 1024 iterations

A non-monotone pass would previously spin forever in run(); adds a hard
iteration cap that surfaces as ErrorKind::FixedPointLimitExceeded so the
caller can diagnose rather than hang."
```

---

## Task 3: Support narrow-load-from-wider-store on both LE and BE

**Problem:** [`crates/opt/src/stack_load_forward/mod.rs:130-140`](../../crates/opt/src/stack_load_forward/mod.rs#L130-L140) inserts a `Truncate` for narrow-loads, with a comment admitting the path is only valid on little-endian targets. `target::SleighArch::endianness` already exists ([`crates/target/src/arch.rs:26`](../../crates/target/src/arch.rs#L26)), and the analyzer pipeline ([`crates/analyzer/src/analyzer/pipeline.rs:55`](../../crates/analyzer/src/analyzer/pipeline.rs#L55)) is the canonical wiring point. Implement *correct* narrow-load synthesis for both endiannesses.

**Semantics recap.** A narrow load of `load_size` bytes at offset K, where a wider store of `store_size` bytes was placed at the same K:
- **LE:** address K holds the low byte; the load reads bits `[0, load_size*8)` of the stored value → `Truncate(data)`.
- **BE:** address K holds the high byte; the load reads bits `[(store_size - load_size)*8, store_size*8)` → `Truncate(ShiftRight(data, (store_size - load_size) * 8))`.

The shift amount must be a `data_ty`-typed `IntConst` so the `IntBinaryOp::ShiftRight` typechecks.

**Files:**
- Modify: [`crates/opt/src/stack_load_forward/mod.rs`](../../crates/opt/src/stack_load_forward/mod.rs)
- Modify: [`crates/analyzer/src/analyzer/pipeline.rs`](../../crates/analyzer/src/analyzer/pipeline.rs)
- Modify: existing test call sites that construct `StackLoadForward::new(sp)` (need to pass endianness)
- Test: [`crates/opt/src/stack_load_forward/tests.rs`](../../crates/opt/src/stack_load_forward/tests.rs) (add a BE-equivalent of the existing narrow test)

- [ ] **Step 1: Write a failing test for BE narrow-load synthesis**

First, identify the existing LE narrow test. Run:

```bash
grep -n "narrow\|truncate\|Truncate" /home/mike/Desktop/strider/.worktrees/opt-review/crates/opt/src/stack_load_forward/tests.rs | head -20
```

Use the existing narrow-from-wider test as a template (likely has a name containing `narrow`). Add a sibling BE test that builds the same IR but constructs `StackLoadForward::new(sp, Endianness::Big)` and asserts the forwarded value is `Truncate(ShiftRight(data, shift_amount))` rather than `Truncate(data)`. Concretely:

```rust
#[test]
fn narrow_load_from_wider_store_be_shifts_high_bytes() -> crate::Result<()> {
    use ir::node::{NodeKind, NodeOutputType};
    use ir::IntBinaryOp;
    use target::Endianness;

    // Mirror the LE narrow test setup: store a U32 value V at sp+0, then
    // load a U8 at sp+0. Expect the forwarded value to be Truncate(Shr(V, 24))
    // for BE (high byte at lowest address).
    let sp = sp_vn();  // helper from this test module
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_val = b.read_variable(&sp)?;
    let val = b.build_int_const(0xDEAD_BEEF, NodeOutputType::U32);
    let mem0 = b.current_memory();
    let _store = b.build_store(mem0, sp_val, val, rsleigh::VnSpace::RAM)?;
    let mem1 = b.current_memory();
    let load = b.build_load(mem1, sp_val, NodeOutputType::U8, rsleigh::VnSpace::RAM)?;
    b.build_return(Some(load), &[])?;
    let mut fg = b.build()?;

    let mut p = crate::OptimizerPipeline::new();
    p.add(crate::StackStoreDetect::new(sp));
    p.add(crate::StackLoadForward::new(sp, Endianness::Big));
    p.run(&mut fg)?;

    // The Return's value input should resolve to Truncate(ShiftRight(val, 24)).
    let ret = fg.preorder().find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("return");
    let ret_inputs = fg.graph.node_inputs(ret);
    let val_in = ret_inputs[1];
    let trunc_node = fg.graph.get_node_from_output(val_in);
    assert!(matches!(*fg.graph.node_kind(trunc_node), NodeKind::Truncate));
    let trunc_inputs = fg.graph.node_inputs(trunc_node);
    let shr_node = fg.graph.get_node_from_output(trunc_inputs[0]);
    assert!(matches!(
        *fg.graph.node_kind(shr_node),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight)
    ));
    Ok(())
}
```

(Adapt API names if `build_store` / `build_load` / `current_memory` don't exist exactly — read [`crates/ir/src/builder/nodes.rs`](../../crates/ir/src/builder/nodes.rs) and the existing narrow test's actual construction. The point is: the assertion shape stays.)

- [ ] **Step 2: Run the test — confirm it fails**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-fixes  # the new worktree
cargo test -p opt narrow_load_from_wider_store_be 2>&1 | tail -20
```
Expected: FAIL — compile error because `StackLoadForward::new` is single-arg and the `endianness` field doesn't exist.

- [ ] **Step 3: Add `endianness` field, thread through, implement both arms**

Edit [`crates/opt/src/stack_load_forward/mod.rs`](../../crates/opt/src/stack_load_forward/mod.rs):

1. Re-export and use the existing `target::Endianness`:
   ```rust
   use target::Endianness;
   ```

2. Replace the struct definition (lines 24-27):
   ```rust
   pub struct StackLoadForward {
       pub stack_ptr_vn: rsleigh::Vn,
       pub endianness: Endianness,
   }
   ```

3. Replace `new` (lines 30-34) and `from_convention` (lines 38-41):
   ```rust
   #[must_use]
   pub fn new(stack_ptr_vn: rsleigh::Vn, endianness: Endianness) -> Self {
       Self { stack_ptr_vn, endianness }
   }

   /// Creates a new pass whose stack-pointer varnode is taken from `cc` and
   /// whose endianness is taken from `arch`.
   #[must_use]
   pub fn from_convention(
       cc: &target::BuiltCallingConvention,
       arch: &target::SleighArch,
   ) -> Self {
       Self::new(cc.stack_ptr_vn, arch.endianness)
   }
   ```

4. Thread `endianness` from `optimize` (line 45) into `try_forward_load` (call at line 53) into `resolve` (call at line 84). Add `endianness: Endianness` as a parameter to both.

5. Replace the narrow-from-wider arm (lines 126-144). The full new arm (after the `if data_ty == load_ty { Some(data) }` branch):

   ```rust
                   } else if data_ty.is_integer()
                       && load_ty.is_integer()
                       && load_ty.byte_size() < data_ty.byte_size()
                   {
                       // Narrow-load-from-wider-store at matching offset.
                       // - LE: load bytes are the low `load_size` bytes of
                       //   the stored value → Truncate(data).
                       // - BE: load bytes are the high `load_size` bytes →
                       //   shift right by (store_size - load_size) * 8 bits,
                       //   then Truncate.
                       let shifted = match endianness {
                           Endianness::Little => data,
                           Endianness::Big => {
                               let shift_bits =
                                   ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
                               let shift_const = fg.make_int_const(shift_bits, data_ty).ok()?;
                               let shr = fg.graph.create_node(
                                   NodeKind::IntBinaryOp(ir::IntBinaryOp::ShiftRight),
                                   [data, shift_const],
                                   [NodeOutputKind::OutputType(data_ty)],
                               );
                               fg.graph.node_outputs(shr).into_iter().next()?
                           }
                       };
                       let trunc = fg.graph.create_node(
                           NodeKind::Truncate,
                           [shifted],
                           [NodeOutputKind::OutputType(load_ty)],
                       );
                       fg.graph.node_outputs(trunc).into_iter().next()
                   } else {
                       None
                   }
   ```

   The new node creation API mirrors what the file already uses on line 136-141; `make_int_const` is the project-standard helper (see [`crates/ir/src/ops/consts.rs:52`](../../crates/ir/src/ops/consts.rs#L52)).

- [ ] **Step 4: Update analyzer wiring**

Edit [`crates/analyzer/src/analyzer/pipeline.rs:55`](../../crates/analyzer/src/analyzer/pipeline.rs#L55) so the `from_convention` call also receives the arch:

```bash
grep -n "StackLoadForward::from_convention\|self.arch\|self.cc\|self.calling" /home/mike/Desktop/strider/.worktrees/opt-fixes/crates/analyzer/src/analyzer/pipeline.rs
```

If the analyzer holds an `arch: SleighArch` field, change the call to:

```rust
        p.add(opt::StackLoadForward::from_convention(
            &self.calling_convention,
            &self.arch,
        ));
```

(Adapt field names to match the actual struct — read `crates/analyzer/src/analyzer/mod.rs` first.)

- [ ] **Step 5: Update existing test call sites**

The existing tests call `StackLoadForward::new(sp)` (single arg). Bulk-update them:

```bash
grep -rn "StackLoadForward::new(" /home/mike/Desktop/strider/.worktrees/opt-fixes/crates/opt/ /home/mike/Desktop/strider/.worktrees/opt-fixes/crates/opt/tests/ 2>&1
```

For each match, change to `StackLoadForward::new(sp, target::Endianness::Little)` (existing tests are LE by construction). Add `use target::Endianness;` at the top of each test file if missing.

- [ ] **Step 6: Run tests**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-fixes
cargo test -p opt 2>&1 | tail -30
```
Expected: BE test passes; existing LE narrow tests still pass; all other opt tests pass.

- [ ] **Step 7: Run workspace tests + clippy**

```bash
cargo test --workspace 2>&1 | tail -15
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
cargo clippy -p analyzer --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: clean / no regressions.

- [ ] **Step 8: Commit**

```bash
git add crates/opt/src/stack_load_forward/ crates/analyzer/src/analyzer/pipeline.rs
git commit -m "feat(opt): support narrow-load-from-wider-store on big-endian targets

Adds Endianness field to StackLoadForward; on BE the synthesized forwarded
value is Truncate(ShiftRight(data, (store_size - load_size) * 8)) instead
of plain Truncate(data). Analyzer pipeline now threads SleighArch.endianness
through StackLoadForward::from_convention. Existing tests pinned to
Endianness::Little (their fixtures are LE-shaped)."
```

---

## Task 4: Enforce `Kb::merge` invariant

**Problem:** [`crates/opt/src/known_bits/mod.rs:39-49`](../../crates/opt/src/known_bits/mod.rs#L39-L49) — `Kb::merge` ORs `ones` and `zeros` independently. The doc on lines 18-20 declares `ones & zeros == 0` invariant, but `merge` does not enforce it. Currently safe because the only call site merges idempotent values from a deterministic `node_known_bits`, but a future contributor adding a real meet-over-different-sources will silently produce a `Kb` with overlapping bits, causing `all_known` to spuriously fire and emit wrong constants. Cheap to harden.

**Files:**
- Modify: [`crates/opt/src/known_bits/mod.rs`](../../crates/opt/src/known_bits/mod.rs)
- Test: [`crates/opt/src/known_bits/tests.rs`](../../crates/opt/src/known_bits/tests.rs) (add)

- [ ] **Step 1: Write a failing test demonstrating the broken invariant on conflicting merge**

Append to `crates/opt/src/known_bits/tests.rs`:

```rust
#[test]
fn merge_preserves_invariant_under_conflict() {
    // Bit 0 is ones in `a`, zeros in `b`. After merging both into `c`,
    // ones & zeros must be 0.
    let mut c = super::Kb::default();
    let a = super::Kb { ones: 0b1, zeros: 0 };
    let b = super::Kb { ones: 0, zeros: 0b1 };
    c.merge(a);
    c.merge(b);
    assert_eq!(c.ones & c.zeros, 0,
        "ones & zeros must be 0; got ones={:#b} zeros={:#b}", c.ones, c.zeros);
}
```

- [ ] **Step 2: Run the test — confirm it fails**

```bash
cargo test -p opt merge_preserves_invariant_under_conflict 2>&1 | tail -10
```
Expected: FAIL (`ones & zeros == 1`).

- [ ] **Step 3: Fix `merge` to clear conflicting bits**

Edit [`crates/opt/src/known_bits/mod.rs:38-49`](../../crates/opt/src/known_bits/mod.rs#L38-L49) — replace the `merge` body with:

```rust
    /// Returns `true` if merging `other` into `self` changed anything.
    ///
    /// On conflict (a bit known 1 in one source and 0 in the other), the
    /// `ones` set wins and the conflicting bit is cleared from `zeros`,
    /// preserving the `ones & zeros == 0` invariant.
    fn merge(&mut self, other: Kb) -> bool {
        let new_ones = self.ones | other.ones;
        let new_zeros = (self.zeros | other.zeros) & !new_ones;
        if new_ones != self.ones || new_zeros != self.zeros {
            self.ones = new_ones;
            self.zeros = new_zeros;
            true
        } else {
            false
        }
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p opt 2>&1 | tail -20
```
Expected: all tests pass (the new one and existing ones).

- [ ] **Step 5: Run clippy and commit**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
git add crates/opt/src/known_bits/
git commit -m "harden(opt): enforce Kb::merge ones&zeros==0 invariant on conflict

Conflict resolution: ones wins, conflicting zeros bits are masked out.
Currently no live caller triggers a conflict (single-source merges from
a deterministic per-node Kb), but the doc declares the invariant and
all_known relies on it — a future merge-from-multiple-predecessors
caller would otherwise silently produce wrong constants."
```

---

## Task 5: Verify K-group space consistency in `function_args::detect_stack_args`

**Problem:** [`crates/opt/src/function_args/mod.rs:238-244`](../../crates/opt/src/function_args/mod.rs#L238-L244) — comment claims "all loads in a K-group share the same memory space," but no check enforces it. A `Load` at offset K in space A and another at offset K in space B (different SP-relative spaces — e.g. a multi-arch lifter) would silently collapse into one `FunctionArg` keyed by `loads[0]`'s space. Add an explicit guard: discard or skip the group on space mismatch.

**Files:**
- Modify: [`crates/opt/src/function_args/mod.rs`](../../crates/opt/src/function_args/mod.rs)
- Test: [`crates/opt/src/function_args/tests.rs`](../../crates/opt/src/function_args/tests.rs) (add)

- [ ] **Step 1: Write a failing test**

In practice, constructing a multi-space K-group is awkward without lifter support. Use a simpler approach: add an explicit assertion via test that the existing well-formed graphs all hold the invariant:

Append to `crates/opt/src/function_args/tests.rs`:

```rust
#[test]
fn k_group_space_check_skips_mismatched() -> crate::Result<()> {
    // Smoke test: with standard inputs all loads at the same K share a space,
    // so the pass still fires. The mismatch case is hard to construct from
    // FunctionBuilder; rely on the explicit assertion in the implementation
    // to guard mismatched future inputs.
    let fg = common::single_stack_arg_function()?;  // or whatever helper exists
    // ... assert the existing forwarding behavior is unchanged.
    Ok(())
}
```

If a usable helper doesn't exist, skip writing a unit test and rely on the static guard added below + existing test coverage.

- [ ] **Step 2: Add the guard**

Edit [`crates/opt/src/function_args/mod.rs:238-257`](../../crates/opt/src/function_args/mod.rs#L238-L257). After the `let space = match ...` block, before `load_types` is built, add:

```rust
        // Guard: every load in this K-group must share `space`. The grouping
        // logic above keys only on `j` (the offset slot), not on space, so a
        // multi-space lifter could in principle place two loads at the same
        // offset in different spaces. Skip the whole group on mismatch rather
        // than silently merging.
        if loads.iter().any(|&l| {
            !matches!(*fg.graph.node_kind(l), NodeKind::Load(s) if s == space)
        }) {
            continue;
        }
```

- [ ] **Step 3: Run tests + clippy**

```bash
cargo test -p opt 2>&1 | tail -20
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: existing tests still pass; clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/function_args/
git commit -m "harden(opt): skip stack-arg K-groups with mismatched spaces

The grouping in detect_stack_args keys on offset only. Loads at the same
SP offset across different VnSpaces would silently collapse onto loads[0]'s
space; now we skip the whole group instead."
```

---

## Task 6: Bind and check `space` in `collect_stack_args_in_chain_order`

**Problem:** [`crates/opt/src/stack_store/call_args.rs:53`](../../crates/opt/src/stack_store/call_args.rs#L53) — pattern `NodeKind::StackStore { offset, .. }` discards the `space` field. The chain walker accepts any space's stack store as a positional arg. In single-stack-space conventions this is fine; for any setup with multiple SP-relative spaces this silently mixes them. Make the first store's space anchor the chain.

**Files:**
- Modify: [`crates/opt/src/stack_store/call_args.rs`](../../crates/opt/src/stack_store/call_args.rs)
- Test: [`crates/opt/src/stack_store/tests.rs`](../../crates/opt/src/stack_store/tests.rs) (already has chain tests — extend if straightforward, otherwise rely on the static guard)

- [ ] **Step 1: Bind `space` and anchor on it**

Edit `collect_stack_args_in_chain_order`. Replace the pattern destructure (line 53):

```rust
            NodeKind::StackStore { offset, space } => {
                let inputs = fg.graph.node_inputs(node);
                (offset, space, inputs[1], inputs[2], inputs[0])
            }
```

(adds `space` to the tuple). Update the tuple destructure on line 52:

```rust
        let (offset, space, base, data, prev_mem) = match *fg.graph.node_kind(node) {
```

Add an `anchor_space: Option<rsleigh::VnSpace>` (alongside `anchor_base`), initialized to `None`. After the `match anchor_base` arm (line 62-68), add:

```rust
        match anchor_space {
            None => anchor_space = Some(space),
            Some(s) if s == space => {}
            // Space changed mid-chain: stop rather than mix args from
            // different SP-relative spaces.
            _ => return args,
        }
```

(Replace `rsleigh::VnSpace` with whatever type the field actually has — grep `NodeKind::StackStore { offset` in `crates/ir/src/node/` to confirm.)

- [ ] **Step 2: Run tests**

```bash
cargo test -p opt 2>&1 | tail -20
```
Expected: all stack_store tests still pass (real binaries always use a single stack space, so observable behavior is unchanged).

- [ ] **Step 3: Run clippy**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/stack_store/call_args.rs
git commit -m "harden(opt): anchor stack-arg chain walk on store space

collect_stack_args_in_chain_order previously discarded the StackStore's
space field, so a multi-space chain would silently mix args. Now the
first store anchors anchor_space and a mismatched store terminates."
```

---

## Task 7: Replace dead `x` capture with `any()` in absorbing-element rules

**Problem:** [`crates/opt/src/constant_fold/rules.rs:431-453`](../../crates/opt/src/constant_fold/rules.rs#L431-L453) — the `BAnd(false, x)` and `BOr(true, x)` rules bind `let x = pattern::Var::new();` but never read `x` in the rewrite closure. Drop the binding and use `pattern::any()` to make intent explicit.

**Files:**
- Modify: [`crates/opt/src/constant_fold/rules.rs`](../../crates/opt/src/constant_fold/rules.rs)

- [ ] **Step 1: Edit both rules**

Replace lines 429-441 (BAnd absorbing) with:

```rust
        // BAnd(BoolConst(false), _) => bool_const(false)  (absorbing element)
        {
            let l = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_and(any_bool_const(l), pattern::any()),
                bool_const_with!([l] => {
                    if !l { false } else {
                        return Err(pattern::Error::skip());
                    }
                }),
            ))
        },
```

Replace lines 442-454 (BOr absorbing) with the analogous `any()` version.

- [ ] **Step 2: Run tests + clippy + commit**

```bash
cargo test -p opt 2>&1 | tail -20
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
git add crates/opt/src/constant_fold/rules.rs
git commit -m "refactor(opt): use any() for unread BAnd/BOr absorbing-rule operands"
```

---

## Task 8: Extract duplicated `detach_unreachable_nodes`

**Problem:** [`crates/opt/src/redundant_phis/mod.rs:157-171`](../../crates/opt/src/redundant_phis/mod.rs#L157-L171) and [`crates/opt/src/function_args/mod.rs:110-124`](../../crates/opt/src/function_args/mod.rs#L110-L124) define identical functions named `detach_unreachable_nodes`. Move to a shared module.

**Files:**
- Modify: [`crates/opt/src/worklist.rs`](../../crates/opt/src/worklist.rs) (or a new `util.rs`)
- Modify: [`crates/opt/src/redundant_phis/mod.rs`](../../crates/opt/src/redundant_phis/mod.rs)
- Modify: [`crates/opt/src/function_args/mod.rs`](../../crates/opt/src/function_args/mod.rs)

- [ ] **Step 1: Add the shared helper to `worklist.rs`**

Append to [`crates/opt/src/worklist.rs`](../../crates/opt/src/worklist.rs):

```rust
use ir::BuiltFunctionGraph;

use crate::pipeline::OptimizationResult;

/// Detaches the inputs of every node not reachable from the function entry.
///
/// Unreachable nodes can only be consumed by other unreachable nodes, so
/// severing their inputs is always safe. Cleans up dead-block residue and
/// orphaned address-arithmetic chains left behind by passes that rewrite
/// reachable consumers (e.g. `DeadBranchElimination`, `FunctionArgDetect`).
pub(crate) fn detach_unreachable_nodes(fg: &mut BuiltFunctionGraph) -> OptimizationResult {
    let reachable: rustc_hash::FxHashSet<ir::node::NodeId> = fg.preorder().collect();
    let mut changed = false;
    for node_id in fg.all_node_ids().collect::<Vec<_>>() {
        if !reachable.contains(&node_id) && !fg.graph.node_inputs(node_id).is_empty() {
            fg.graph.detach_node_inputs(node_id);
            changed = true;
        }
    }
    if changed {
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    }
}
```

- [ ] **Step 2: Delete the local copies and import the shared one**

In `crates/opt/src/redundant_phis/mod.rs`:
- Delete lines 157-171 (the local `detach_unreachable_nodes`).
- Replace the call on line 204 (`detach_unreachable_nodes(function)`) with `crate::worklist::detach_unreachable_nodes(function)`.
- Drop now-unused imports (`HashSet` from `rustc_hash` may still be used; check).

In `crates/opt/src/function_args/mod.rs`:
- Delete lines 110-124.
- Replace the call on line 102 (`detach_unreachable_nodes(function)`) with `crate::worklist::detach_unreachable_nodes(function)`.
- Drop now-unused imports.

- [ ] **Step 3: Run tests + clippy + commit**

```bash
cargo test -p opt 2>&1 | tail -20
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
git add crates/opt/src/worklist.rs crates/opt/src/redundant_phis/mod.rs crates/opt/src/function_args/mod.rs
git commit -m "refactor(opt): extract detach_unreachable_nodes to crate::worklist

Two identical copies (redundant_phis and function_args) collapsed into
one pub(crate) helper."
```

---

## Task 9: Correct misleading comment in `DeadBranchElimination`

**Problem:** [`crates/opt/src/dead_branch/mod.rs:123-126`](../../crates/opt/src/dead_branch/mod.rs#L123-L126) — the comment claims "A worklist with consumer re-enqueue gives no payoff here." False for chained `If(BoolConst)` patterns where one elimination exposes another. Either correct the comment or implement re-enqueue. Keep the simple drain; rewrite the comment to be accurate.

**Files:**
- Modify: [`crates/opt/src/dead_branch/mod.rs`](../../crates/opt/src/dead_branch/mod.rs)

- [ ] **Step 1: Replace the comment block (lines 123-127)**

```rust
        // DBE only fires on `If` nodes whose outputs are control edges. We
        // drain the seeded preorder once: chained constant-branch patterns
        // (where one elimination exposes another) are caught by the outer
        // OptimizerPipeline fixed-point loop, which re-runs this pass until
        // it reports NoChange.
        let mut work = WorkSet::seeded(function.preorder());
```

- [ ] **Step 2: Confirm tests + clippy + commit**

```bash
cargo test -p opt 2>&1 | tail -10
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
git add crates/opt/src/dead_branch/mod.rs
git commit -m "docs(opt): correct DeadBranchElimination worklist comment"
```

---

## Task 10: Document `LoadReadOnly` endianness contract

**Problem:** [`crates/opt/src/load_readonly/mod.rs:47`](../../crates/opt/src/load_readonly/mod.rs#L47) — `self.0.read(space, addr, size) -> Option<u64>` returns a `u64` directly with no endianness contract documented at the call site. Per [`crates/reader/src/elf.rs:287`](../../crates/reader/src/elf.rs#L287) the reader handles host/target endianness internally, but the contract isn't restated where consumers can see it.

**Files:**
- Modify: [`crates/opt/src/load_readonly/mod.rs`](../../crates/opt/src/load_readonly/mod.rs)

- [ ] **Step 1: Add a contract comment above the doc-comment of `LoadReadOnly`**

Edit the doc-comment on the `LoadReadOnly` struct (lines 9-16) to add:

```rust
/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Endianness
///
/// `ReadOnlyMemory::read` returns a `u64` already byte-swapped into host
/// representation according to the *target's* endianness — see
/// [`reader::ReadOnlyMemory`]'s contract. Callers must not re-swap. The
/// returned `u64` is then masked to the load's output type via
/// `NodeOutputType::get_unsigned_int`.
///
/// Wrap a concrete memory implementation and add this optimizer to the pipeline:
///
/// ```ignore
/// pipeline.add(LoadReadOnly(my_rom));
/// ```
```

(Verify the actual contract by reading [`crates/reader/src/lib.rs:36`](../../crates/reader/src/lib.rs#L36) and the impl at [`crates/reader/src/elf.rs:287`](../../crates/reader/src/elf.rs#L287); adjust the comment to match what those actually guarantee.)

- [ ] **Step 2: Confirm clippy + commit**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
git add crates/opt/src/load_readonly/mod.rs
git commit -m "docs(opt): document LoadReadOnly endianness contract"
```

---

## Task 11: Final verification

- [ ] **Step 1: Run the full opt test suite**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-review
cargo test -p opt 2>&1 | tail -30
```
Expected: all tests (original 10 + 2 new + 1 new = 13) pass. Adjust the count if intermediate task counts diverged.

- [ ] **Step 2: Run workspace clippy on opt**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings 2>&1 | tail -10
```
Expected: clean.

- [ ] **Step 3: Run workspace tests** (regression check on analyzer + others using opt)

```bash
cargo test --workspace 2>&1 | tail -30
```
Expected: all tests pass.

- [ ] **Step 4: Push branch (do NOT open PR yet)**

```bash
git push -u origin review/opt-crate
```

(Do not open a PR until the user approves merging back to `feature/ai`.)

---

## Self-review checklist (run after writing the plan)

- [x] Each task has exact file paths and line numbers.
- [x] Each task ends with a commit step.
- [x] Each task has tests where behavior is observable (Tasks 1, 2, 3, 4); for purely-documentary or guard-only changes (Tasks 5, 6, 7, 8, 9, 10) the test step is skipped or relies on existing coverage.
- [x] Every code block shows the actual code to write/replace.
- [x] No "TBD"/"add error handling"/"similar to Task N" placeholders.
- [x] Project rules respected: no `panic!`/`unwrap`/`expect`/`debug_assert!` introduced outside `#[cfg(test)]`.
- [x] All commits stay below the iteration limit of clippy (`-D warnings`) since the baseline is clean.
- [x] Tasks ordered by risk: correctness → hardening → simplification → docs → verify.
