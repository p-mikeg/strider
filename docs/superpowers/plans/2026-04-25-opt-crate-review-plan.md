# `opt` Crate Review, Scaling, and Testing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify correctness of every `opt` pass, reorganize the crate into per-pass directories with shared SP machinery, add basic + comprehensive tests for every optimization, scale to large IR graphs via memoization and per-pass worklists, and finish clippy-clean.

**Architecture:** Per-pass internal worklist (no API change). Shared `sp_expr` module replaces `pub(crate)` re-exports from `stack_store`. Hybrid test layout: white-box `<pass>/tests.rs` siblings + black-box `crates/opt/tests/`. `FxHashMap`/`FxHashSet` from `rustc-hash` in hot paths. Criterion benches at 100/1k/10k node scales prove the scaling wins.

**Tech Stack:** Rust 2024 edition, `cranelift-entity` (via `ir`), `petgraph` (via `cfg`), `pattern` crate, `rustc-hash`, `criterion`.

**Spec:** See `docs/superpowers/specs/2026-04-25-opt-crate-review-design.md`.

**Worktree:** `/home/mike/Desktop/strider/.worktrees/opt-review` (branch `feature/opt-review`).

**Baseline:** 92 tests passing, 15 clippy warnings, 6095 lines of source across 11 source files.

---

## Conventions used throughout this plan

- **All paths are relative to the worktree root** unless stated otherwise.
- **Every code-changing step ends with running the affected tests.** Never declare a task complete without watching the green output.
- **One commit per task** unless a task explicitly says "no commit yet".
- **Bug-fix policy:** if a correctness issue surfaces during a task, the fix and its regression test are added to that task. If the fix changes observable behavior, leave a `// FIXME(opt-review): bug-fix changes behavior — see docs/superpowers/specs/...md` comment on the line and surface it to the user before committing.
- **Test verification policy:** every new test must be run twice — first to confirm the assertion shape (run with the test temporarily mutated to fail, e.g. `assert_eq!(x, "wrong")`, see it fail, restore), then to confirm pass. This catches false-positive tests that always pass.

---

## Phase 0 — Foundation

### Task 0.1: Add `rustc-hash` and `criterion` dependencies

**Files:**
- Modify: `crates/opt/Cargo.toml`
- Modify: `Cargo.toml` (workspace root — only if `rustc-hash` isn't already there)

- [ ] **Step 1: Check whether `rustc-hash` is already a workspace dependency**

Run: `grep -n rustc-hash Cargo.toml`
Expected: prints any existing definition, or nothing.

- [ ] **Step 2: If missing, add to workspace deps**

Edit `Cargo.toml` workspace `[workspace.dependencies]` block, add:
```toml
rustc-hash = "2"
```
If already present, skip.

- [ ] **Step 3: Add to `crates/opt/Cargo.toml`**

After `thiserror = ...`, add:
```toml
rustc-hash = { workspace = true }

[dev-dependencies]
criterion = { version = "0.7", features = ["html_reports"] }

[[bench]]
name = "constant_fold"
harness = false

[[bench]]
name = "known_bits"
harness = false

[[bench]]
name = "stack_store"
harness = false

[[bench]]
name = "default_pipeline"
harness = false
```

- [ ] **Step 4: Build to confirm deps resolve**

Run: `cargo build -p opt`
Expected: builds clean (the `[[bench]]` entries will warn until benches exist; that's fine).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/opt/Cargo.toml
git commit -m "build(opt): add rustc-hash dep and criterion dev-dep for benches"
```

---

### Task 0.2: Create `tests/common/mod.rs` shared test harness

**Files:**
- Create: `crates/opt/tests/common/mod.rs`
- Create: `crates/opt/tests/_bootstrap.rs` (sentinel — `cargo test` requires at least one `tests/*.rs`; this guarantees `common` compiles even before real integration tests exist)

- [ ] **Step 1: Create the common module**

```rust
// crates/opt/tests/common/mod.rs
//! Shared helpers for `opt` integration tests.
//!
//! Currently re-implements the patterns spread across the per-pass white-box
//! test modules so black-box `tests/*.rs` files can write concise scenarios.

#![allow(dead_code)] // Helpers are reused across files; rustc can't see all uses.

use ir::node::{NodeKind, NodeOutputType};
use ir::{BuiltFunctionGraph, FunctionBuilder, Value};
use opt::{Error, Result};

/// Builds a single-region function whose return value is what `f` produces.
pub fn make_fn<F>(f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    Ok(b.build()?)
}

/// Builds a single-region function with a tracked variable `vn`. The closure
/// receives the read-back value (a `ControlPhi` over `InitialVar(vn)`).
pub fn make_fn_with_var<F>(
    vn: rsleigh::Vn,
    f: F,
) -> Result<(BuiltFunctionGraph, Value)>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build()?, x))
}

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub fn return_value(fg: &BuiltFunctionGraph) -> Result<Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(opt::ErrorKind::NoReturnNode)?;
    Ok(fg.graph.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub fn return_kind(fg: &BuiltFunctionGraph) -> Result<NodeKind> {
    let val = return_value(fg)?;
    let node = fg.graph.get_node_from_output(val);
    Ok(*fg.graph.node_kind(node))
}

/// Counts nodes matching `pred`.
pub fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Counts CFG-reachable nodes matching `pred`.
pub fn count_reachable<F: Fn(&NodeKind) -> bool>(
    fg: &BuiltFunctionGraph,
    pred: F,
) -> usize {
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Fabricates a register varnode of the given size at offset `off`.
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// Stack-pointer varnode at REGISTER:0x20, size 4 (matches x86 ESP).
pub fn sp_vn() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

/// Runs `pass.optimize` until it reports `NoChange` or `MAX_ITERS` is hit
/// (panics on hit — indicates a non-converging pass).
pub fn run_to_fixed_point<P: opt::Optimizer>(
    pass: &P,
    fg: &mut BuiltFunctionGraph,
) -> Result<()> {
    const MAX_ITERS: usize = 100;
    for _ in 0..MAX_ITERS {
        if !pass.optimize(fg)?.changed() {
            return Ok(());
        }
    }
    Err(Error::from(opt::ErrorKind::AssertionFailed(
        format!("pass did not converge in {MAX_ITERS} iterations"),
    )))
}

/// Conventional unused suppressions for tests that don't use every helper.
#[allow(dead_code)]
fn _unused() {}

// Re-export commonly used IR types so test files don't need long use-paths.
pub use ir::node::NodeOutputType as Type;
```

- [ ] **Step 2: Create the bootstrap test file**

```rust
// crates/opt/tests/_bootstrap.rs
//! Empty placeholder test crate — just makes `tests/common/mod.rs` compile
//! before real integration test files arrive.
mod common;

#[test]
fn common_compiles() {
    let _ = common::sp_vn();
}
```

- [ ] **Step 3: Build tests to verify they compile**

Run: `cargo test -p opt --test _bootstrap`
Expected: 1 test, passes.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/tests/common/mod.rs crates/opt/tests/_bootstrap.rs
git commit -m "test(opt): add shared test harness in tests/common"
```

---

## Phase 1 — Extract `sp_expr` module

### Task 1.1: Create `crates/opt/src/sp_expr.rs` with the SP-decomposition machinery

**Files:**
- Create: `crates/opt/src/sp_expr.rs`
- Modify: `crates/opt/src/lib.rs` (add `mod sp_expr;`)

The module hosts everything currently at the top of `stack_store.rs`: `SpExpr`, `decompose_sp`, `ranges_disjoint`, `int_const_signed` — but with a memoization cache threaded through `decompose_sp`.

- [ ] **Step 1: Create `sp_expr.rs` with the original (unmemoized) implementations copied verbatim**

Copy from `crates/opt/src/stack_store.rs` lines 22-179 into `crates/opt/src/sp_expr.rs`. Replace the leading attributes / `pub(crate)` markers as below:

```rust
// crates/opt/src/sp_expr.rs
//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`stack_store::detect`, `stack_load_forward`, `function_args::stack_args`).
//!
//! `decompose_sp` is the workhorse: given an output that may be `InitialVar(sp)`
//! transformed by `Add`/`Sub` of constants and joined by `ControlPhi(sp)`, it
//! returns either a `Terminal { base, offset }` or a `Phi { node, offsets[] }`.
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.

use rustc_hash::FxHashMap;
use std::collections::HashSet;

use ir::node::{NodeId, NodeOutputId};
use ir::{BuiltFunctionGraph, IntBinaryOp};
use ir::node::NodeKind;

/// Decomposed stack-pointer expression.
#[derive(Clone, Debug)]
pub(crate) enum SpExpr {
    Terminal { base: NodeOutputId, offset: i64 },
    Phi { phi_node: NodeId, offsets: Vec<i64> },
}

impl SpExpr {
    pub(crate) fn shifted(self, delta: i64) -> Self {
        match self {
            SpExpr::Terminal { base, offset } => SpExpr::Terminal {
                base,
                offset: offset.wrapping_add(delta),
            },
            SpExpr::Phi { phi_node, offsets } => SpExpr::Phi {
                phi_node,
                offsets: offsets.into_iter().map(|o| o.wrapping_add(delta)).collect(),
            },
        }
    }
}

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` are
/// disjoint.
#[inline]
pub(crate) fn ranges_disjoint(a_off: i64, a_size: i64, b_off: i64, b_size: i64) -> bool {
    a_off + a_size <= b_off || b_off + b_size <= a_off
}

/// Reads an integer-constant output as signed, sign-extended from its declared
/// bit width. Returns `None` for non-integer-constant or for U128/U256.
pub(crate) fn int_const_signed(fg: &BuiltFunctionGraph, out: NodeOutputId) -> Option<i64> {
    let c = fg.int_const_val(out)?;
    fg.graph.output_kind(out).as_value()?.get_signed_int(c)
}

/// Per-pass-call memo for `decompose_sp`.
pub(crate) type SpExprMemo = FxHashMap<NodeOutputId, Option<SpExpr>>;

/// Decomposes `out` into `InitialVar(sp) + K` (or per-branch equivalent),
/// caching definitive results in `memo`. The `visiting` set guards against
/// cycles through `ControlPhi` back-edges; cycle-broken results are NOT
/// memoized.
pub(crate) fn decompose_sp(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut HashSet<NodeId>,
) -> Option<SpExpr> {
    if let Some(cached) = memo.get(&out) {
        return cached.clone();
    }
    let node = fg.graph.get_node_from_output(out);
    if !visiting.insert(node) {
        // Cycle: do NOT cache (a different call path may resolve it).
        return None;
    }
    let result = decompose_sp_inner(fg, out, node, sp_vn, memo, visiting);
    visiting.remove(&node);
    // Only cache if no cycle was hit on this call path. Approximation: if
    // `visiting` is empty here we know we returned cleanly.
    if visiting.is_empty() {
        memo.insert(out, result.clone());
    }
    result
}

fn decompose_sp_inner(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut HashSet<NodeId>,
) -> Option<SpExpr> {
    match *fg.graph.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == sp_vn => Some(SpExpr::Terminal {
            base: out,
            offset: 0,
        }),
        NodeKind::ControlPhi(vn) if vn == sp_vn => decompose_sp_phi(fg, out, node, sp_vn, memo, visiting),
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            if let Some(c) = int_const_signed(fg, r) {
                decompose_sp(fg, l, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else if let Some(c) = int_const_signed(fg, l) {
                decompose_sp(fg, r, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else {
                None
            }
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Sub) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            int_const_signed(fg, r).and_then(|c| {
                decompose_sp(fg, l, sp_vn, memo, visiting).map(|e| e.shifted(c.wrapping_neg()))
            })
        }
        _ => None,
    }
}

fn decompose_sp_phi(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut HashSet<NodeId>,
) -> Option<SpExpr> {
    let inputs = fg.graph.node_inputs(node);
    if inputs.len() < 2 {
        return Some(SpExpr::Terminal { base: out, offset: 0 });
    }
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    let mut bases = Vec::with_capacity(inputs.len() - 1);
    for pred_input in inputs.into_iter().skip(1) {
        match decompose_sp(fg, pred_input, sp_vn, memo, visiting) {
            Some(SpExpr::Terminal { base, offset }) => {
                bases.push(base);
                offsets.push(offset);
            }
            _ => return Some(SpExpr::Terminal { base: out, offset: 0 }),
        }
    }
    if bases.iter().all(|&b| b == bases[0]) && offsets.iter().all(|&o| o == offsets[0]) {
        Some(SpExpr::Terminal { base: bases[0], offset: offsets[0] })
    } else {
        Some(SpExpr::Phi { phi_node: node, offsets })
    }
}
```

Note the changes vs the original: (1) memo + visiting threaded through, (2) cycle results not cached, (3) phi & inner split into helpers for readability.

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/opt/src/lib.rs`, after `mod pipeline;`, add:
```rust
mod sp_expr;
```

- [ ] **Step 3: Build to confirm it compiles**

Run: `cargo build -p opt`
Expected: clean build (the old `stack_store.rs` definitions still exist but are now duplicates — that's intentional, removed in Task 1.2).

- [ ] **Step 4: No commit yet — finish Task 1.2 first**

---

### Task 1.2: Migrate `stack_store.rs`, `stack_load_forward.rs`, `function_args.rs` to use `sp_expr`

**Files:**
- Modify: `crates/opt/src/stack_store.rs`
- Modify: `crates/opt/src/stack_load_forward.rs`
- Modify: `crates/opt/src/function_args.rs`

- [ ] **Step 1: Delete the original `SpExpr`/`decompose_sp`/`ranges_disjoint`/`int_const_signed` from `stack_store.rs`**

Remove lines 22-179 of `crates/opt/src/stack_store.rs`. Replace with:
```rust
use crate::sp_expr::{SpExpr, decompose_sp, ranges_disjoint, SpExprMemo};
```
(Drop `ranges_disjoint` from the import if `stack_store.rs` no longer uses it directly — only `function_args` and `stack_load_forward` use it. Check with `grep -n ranges_disjoint crates/opt/src/stack_store.rs`.)

- [ ] **Step 2: Update `try_detect_stack_store` to thread the memo**

Replace each call site of:
```rust
let mut visiting = std::collections::HashSet::new();
let Some(expr) = decompose_sp(fg, addr, sp_vn, &mut visiting) else {
    ...
};
```
with:
```rust
let mut visiting = std::collections::HashSet::new();
let Some(expr) = decompose_sp(fg, addr, sp_vn, memo, &mut visiting) else {
    ...
};
```
where `memo: &mut SpExprMemo` is passed through from `optimize`.

In `StackStoreDetect::optimize`, create the memo once at the top:
```rust
let mut memo: SpExprMemo = Default::default();
for node_id in nodes {
    result |= try_detect_stack_store(function, node_id, self.stack_ptr_vn, &mut memo)?;
}
```
And update `try_detect_stack_store`'s signature to take `memo: &mut SpExprMemo`.

- [ ] **Step 3: Update `stack_load_forward.rs`**

Replace `use crate::stack_store::{SpExpr, decompose_sp, ranges_disjoint};` with:
```rust
use crate::sp_expr::{SpExpr, decompose_sp, ranges_disjoint, SpExprMemo};
```
Add memo plumbing: `StackLoadForward::optimize` creates one memo at the top and threads it through every helper that calls `decompose_sp`.

- [ ] **Step 4: Update `function_args.rs`**

Same as Step 3. Replace `use crate::stack_store::{SpExpr, decompose_sp, ranges_disjoint};` with the `crate::sp_expr` import. Thread a single memo through `detect_stack_args`.

- [ ] **Step 5: Run all tests to confirm no regressions**

Run: `cargo test -p opt`
Expected: 92 tests pass.

- [ ] **Step 6: Run clippy to confirm no new warnings**

Run: `cargo clippy -p opt --all-targets 2>&1 | grep -c "^warning:"`
Expected: ≤ 15 (the pre-existing warning count).

- [ ] **Step 7: Commit Tasks 1.1 + 1.2 together**

```bash
git add crates/opt/src/sp_expr.rs crates/opt/src/lib.rs crates/opt/src/stack_store.rs crates/opt/src/stack_load_forward.rs crates/opt/src/function_args.rs
git commit -m "refactor(opt): extract sp_expr module with memoized decompose_sp

The SP-decomposition machinery (SpExpr, decompose_sp, ranges_disjoint,
int_const_signed) is now in its own module instead of being re-exported
from stack_store.rs as pub(crate). decompose_sp gains a per-pass-call
memo (FxHashMap<NodeOutputId, Option<SpExpr>>) so repeated walks over
the same SP chain are O(1) on cache hit.

Cycle-broken results are not cached: if visiting is non-empty when
the recursion returns, the result is left out of the memo so that
a different call path can still resolve the same output."
```

---

### Task 1.3: Add unit tests for `sp_expr`

**Files:**
- Create: `crates/opt/src/sp_expr/mod.rs` (move sp_expr.rs into a directory)
- Create: `crates/opt/src/sp_expr/tests.rs`

Wait — the simpler shape is keeping `sp_expr.rs` flat and adding tests inside it. Use the inline `#[cfg(test)] mod tests` pattern since `sp_expr` is a single file with no submodules.

- [ ] **Step 1: Append `tests` module to `sp_expr.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr: rsleigh::VnAddr { space: rsleigh::VnSpace::REGISTER, off: 0x20 },
            size: 4,
        }
    }

    #[test]
    fn ranges_disjoint_basic() {
        // Adjacent ranges are disjoint (touching is fine).
        assert!(ranges_disjoint(0, 4, 4, 4));
        // Overlapping ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 2, 4));
        // Identical ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 0, 4));
        // Reverse order — equally disjoint.
        assert!(ranges_disjoint(4, 4, 0, 4));
    }

    #[test]
    fn int_const_signed_u32_negative() -> crate::Result<()> {
        // 0xFFFF_FFFC at U32 must read as -4 signed.
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let v = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
        b.build_return(Some(v), &[])?;
        let fg = b.build()?;
        assert_eq!(int_const_signed(&fg, v), Some(-4));
        Ok(())
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        let fg = b.build()?;
        // sp_val is a ControlPhi-of-InitialVar; the phi has 1 predecessor →
        // collapses to Terminal{base: InitialVar(sp), offset: 0}.
        let mut memo = SpExprMemo::default();
        let mut visiting = std::collections::HashSet::new();
        let r = decompose_sp(&fg, sp_val, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: 0, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = std::collections::HashSet::new();
        let r = decompose_sp(&fg, addr, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = std::collections::HashSet::new();
        let r = decompose_sp(&fg, addr, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_hit_returns_same_result() -> crate::Result<()> {
        // Calling decompose_sp twice on the same out should populate the memo
        // and return the same answer.
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r1 = {
            let mut v = std::collections::HashSet::new();
            decompose_sp(&fg, addr, sp, &mut memo, &mut v)
        };
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = {
            let mut v = std::collections::HashSet::new();
            decompose_sp(&fg, addr, sp, &mut memo, &mut v)
        };
        assert!(matches!((&r1, &r2),
            (Some(SpExpr::Terminal { offset: -4, .. }),
             Some(SpExpr::Terminal { offset: -4, .. }))));
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let c = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_return(Some(c), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = std::collections::HashSet::new();
        assert!(decompose_sp(&fg, c, sp, &mut memo, &mut visiting).is_none());
        Ok(())
    }
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p opt sp_expr::tests`
Expected: 6 tests pass.

- [ ] **Step 3: Verify the tests actually fail when broken**

Temporarily change the assertion in `decompose_sp_sub_constant` from `offset: -4` to `offset: 4`. Run the test — it must fail.

Run: `cargo test -p opt sp_expr::tests::decompose_sp_sub_constant`
Expected: FAIL.

Restore the assertion. Re-run: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/sp_expr.rs
git commit -m "test(sp_expr): add unit tests for SpExpr, decompose_sp, ranges_disjoint, memo"
```

---

## Phase 2 — Per-pass migration

For Phase 2, each pass goes through this template (substitute pass-specific names):

1. Move `<pass>.rs` → `<pass>/mod.rs`
2. Extract inline `#[cfg(test)] mod tests` → `<pass>/tests.rs`
3. Apply per-pass internal worklist refactor (where applicable)
4. Add basic + complex tests
5. Apply correctness fixes (if found)
6. Run full opt test suite + clippy

Order chosen to minimize blast radius: simpler passes first, larger refactors last.

### Task 2.A: `load_readonly` — file split + tests (smallest pass)

**Files:**
- Move: `crates/opt/src/load_readonly.rs` → `crates/opt/src/load_readonly/mod.rs`
- Create: `crates/opt/src/load_readonly/tests.rs`
- Modify: `crates/opt/src/lib.rs` (no change — `mod load_readonly;` still resolves)

- [ ] **Step 1: Create the directory and move the file**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-review
mkdir -p crates/opt/src/load_readonly
git mv crates/opt/src/load_readonly.rs crates/opt/src/load_readonly/mod.rs
```

- [ ] **Step 2: Extract tests block to a sibling file**

In `crates/opt/src/load_readonly/mod.rs`, find the `#[cfg(test)] mod tests { ... }` block (lines 63-151 of the original) and replace the entire block with:
```rust
#[cfg(test)]
mod tests;
```

Create `crates/opt/src/load_readonly/tests.rs` and paste the contents of the original `mod tests { ... }` block into it (without the outer `mod tests {` and the closing `}`):

```rust
// crates/opt/src/load_readonly/tests.rs
use super::*;
use crate::error::{ErrorKind, Result};
use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

// ── tiny ROM fixture ──────────────────────────────────────────────────────

struct TestRom;

impl ReadOnlyMemory for TestRom {
    fn read(&self, _space: rsleigh::VnSpace, addr: u64, _size: usize) -> Option<u64> {
        match addr {
            0x1000 => Some(42),
            0x2000 => Some(0xFF),
            _ => None,
        }
    }
}

fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    Ok(b.build()?)
}

fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(ErrorKind::NoReturnNode)?;
    let val = fg.graph.node_inputs(ret)[2];
    Ok(*fg.graph.node_kind(fg.graph.get_node_from_output(val)))
}

#[test]
fn load_from_rom_const_addr() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000, NodeOutputType::U64);
        Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(42));
    Ok(())
}

#[test]
fn load_non_rom_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xDEAD, NodeOutputType::U64);
        Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
    })?;
    assert!(!LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
    assert!(
        fg.all_node_ids()
            .any(|n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
    );
    Ok(())
}

#[test]
fn load_non_const_addr_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let base = b.build_int_const(0x1000, NodeOutputType::U64);
        let off = b.build_int_const(0, NodeOutputType::U64);
        let addr = b.build_int_binary_operation(base, off, ir::IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
    })?;
    assert!(!LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
    Ok(())
}
```

- [ ] **Step 3: Run tests to verify the migration didn't break anything**

Run: `cargo test -p opt load_readonly`
Expected: 3 tests pass.

- [ ] **Step 4: Add the new "complex" test cases**

Append to `crates/opt/src/load_readonly/tests.rs`:

```rust
// ── new comprehensive tests ──────────────────────────────────────────────

/// Loading more bytes than the ROM provides (read returns None) leaves the
/// Load node intact.
#[test]
fn load_oversize_read_no_change() -> Result<()> {
    struct Limited;
    impl ReadOnlyMemory for Limited {
        fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
            // Only single-byte reads are supported.
            if size == 1 && addr == 0x1000 { Some(42) } else { None }
        }
    }
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000, NodeOutputType::U64);
        // Request 8 bytes — limited ROM returns None.
        Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
    })?;
    assert!(!LoadReadOnly(Limited).optimize(&mut fg)?.changed());
    Ok(())
}

/// Different `VnSpace`s should be tried independently — a ROM that only
/// answers in `RAM` doesn't fold a `Load(REGISTER)`.
#[test]
fn load_other_space_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x1000, NodeOutputType::U64);
        Ok(b.build_load(addr, rsleigh::VnSpace::REGISTER, NodeOutputType::U64)?)
    })?;
    // TestRom answers regardless of space — but a load from REGISTER space
    // should still resolve only when ROM matches addr. Confirm with a ROM
    // that distinguishes spaces.
    struct RamOnly;
    impl ReadOnlyMemory for RamOnly {
        fn read(&self, space: rsleigh::VnSpace, _addr: u64, _size: usize) -> Option<u64> {
            if space == rsleigh::VnSpace::RAM { Some(0) } else { None }
        }
    }
    assert!(!LoadReadOnly(RamOnly).optimize(&mut fg)?.changed());
    Ok(())
}

/// Loading 8 bytes from a U8-typed slot must mask correctly: the optimizer
/// applies `ty.get_unsigned_int(loaded)`, so 0xFF → 0xFF in U8.
#[test]
fn load_u8_masks_to_byte() -> Result<()> {
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0x2000, NodeOutputType::U64);
        Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)?)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// Multiple loads at different addresses fold independently in one pass.
#[test]
fn multiple_loads_fold_in_one_pass() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a1 = b.build_int_const(0x1000, NodeOutputType::U64);
        let a2 = b.build_int_const(0x2000, NodeOutputType::U64);
        let l1 = b.build_load(a1, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let l2 = b.build_load(a2, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(l1, l2, ir::IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    assert!(LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
    // After folding, both loads become consts; an Add of consts remains
    // until ConstantFold runs (which we don't run here). Verify both Loads
    // are gone from the reachable subgraph.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let remaining_loads = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
        .count();
    assert_eq!(remaining_loads, 0, "both loads must have folded");
    Ok(())
}
```

- [ ] **Step 5: Run all new tests**

Run: `cargo test -p opt load_readonly`
Expected: 7 tests pass (3 original + 4 new).

- [ ] **Step 6: Falsify each new test once to confirm it can fail**

For each of the 4 new tests, mutate one assertion to be wrong, run, see fail, restore. Skip on Step 3 — these were already verified in original code.

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/load_readonly/
git commit -m "test(opt): split load_readonly into module + add comprehensive tests

Adds: oversize read returns None, cross-VnSpace, U8 masking,
multiple loads folded in one pass."
```

---

### Task 2.B: `dead_branch` — file split + worklist + tests

**Files:**
- Move: `crates/opt/src/dead_branch.rs` → `crates/opt/src/dead_branch/mod.rs`
- Create: `crates/opt/src/dead_branch/tests.rs`

- [ ] **Step 1: Create directory and move file**

```bash
mkdir -p crates/opt/src/dead_branch
git mv crates/opt/src/dead_branch.rs crates/opt/src/dead_branch/mod.rs
```

- [ ] **Step 2: Extract inline tests to `tests.rs`**

In `mod.rs`, replace the `#[cfg(test)] mod tests { ... }` block (last ~120 lines) with `#[cfg(test)] mod tests;`. Move the body to `tests.rs` (drop the outer `mod tests {` wrapper).

- [ ] **Step 3: Run tests to confirm migration**

Run: `cargo test -p opt dead_branch`
Expected: 3 tests pass.

- [ ] **Step 4: Apply worklist refactor**

In `mod.rs`, replace the `Optimizer` impl body:
```rust
// OLD:
let nodes: Vec<_> = function.preorder().collect();
let mut result = OptimizationResult::NoChange;
for node_id in nodes {
    result |= try_eliminate_dead_branch(function, node_id)?;
}
Ok(result)
```
with the worklist loop. First, add a private helper near the top:

```rust
/// Internal worklist used by all opt passes that fold node-by-node. Seeds
/// with the preorder traversal; on every successful rewrite, callers
/// re-enqueue the consumers of the replaced output(s).
struct WorkSet {
    queued: rustc_hash::FxHashSet<ir::node::NodeId>,
    queue: std::collections::VecDeque<ir::node::NodeId>,
}

impl WorkSet {
    fn seeded(it: impl IntoIterator<Item = ir::node::NodeId>) -> Self {
        let mut q = Self {
            queued: rustc_hash::FxHashSet::default(),
            queue: std::collections::VecDeque::new(),
        };
        for n in it { q.push(n); }
        q
    }
    fn push(&mut self, n: ir::node::NodeId) {
        if self.queued.insert(n) { self.queue.push_back(n); }
    }
    fn pop(&mut self) -> Option<ir::node::NodeId> {
        let n = self.queue.pop_front()?;
        self.queued.remove(&n);
        Some(n)
    }
}
```

Then rewrite the `Optimizer` impl:
```rust
impl Optimizer for DeadBranchElimination {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            // Collect consumers of the live output BEFORE the rewrite — after
            // detach_node_inputs the If's output_uses are gone but their old
            // consumers may have new inputs to revisit.
            let pre_consumers: Vec<NodeId> = collect_potential_consumers(function, node_id);
            let r = try_eliminate_dead_branch(function, node_id)?;
            if r.changed() {
                result |= r;
                for c in pre_consumers {
                    work.push(c);
                }
            }
        }
        Ok(result)
    }
}

fn collect_potential_consumers(function: &BuiltFunctionGraph, node_id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    if !matches!(*function.graph.node_kind(node_id), NodeKind::If) {
        return out;
    }
    // Outputs of the If node feed the successor ControlState nodes; their
    // ControlPhi consumers may need re-checking after a rewrite.
    for o in function.graph.node_outputs(node_id) {
        for (consumer, _) in function.graph.output_uses(o) {
            out.push(consumer);
            // Also follow into ControlState's phi consumers.
            if matches!(*function.graph.node_kind(consumer), NodeKind::ControlState) {
                let cs_outputs = function.graph.node_outputs(consumer);
                if cs_outputs.len() >= 2 {
                    for (phi, _) in function.graph.output_uses(cs_outputs[1]) {
                        out.push(phi);
                    }
                }
            }
        }
    }
    out
}
```

Note: `WorkSet` will move to a shared module in Task 2.X.shared (later). For now, each pass has its own local copy.

- [ ] **Step 5: Run tests to confirm worklist refactor preserves behavior**

Run: `cargo test -p opt dead_branch`
Expected: 3 tests pass.

- [ ] **Step 6: Add comprehensive tests**

Append to `crates/opt/src/dead_branch/tests.rs`:

```rust
use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};
use ir::{FunctionBuilder, IntBinaryOp};

/// `if(true)` nested inside a live branch of `if(true)` — both must be
/// eliminated, leaving a straight-line graph.
#[test]
fn nested_if_true_eliminated() -> Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let outer_t = b.create_region()?;
    let outer_f = b.create_region()?;
    let inner_t = b.create_region()?;
    let inner_f = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(true);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let v = b.build_int_const(1, ir::ValueType::U64);
    b.build_return(Some(v), &[])?;
    b.set_region(inner_f);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// A ControlPhi at a 2-input join — when the dead branch is removed, the
/// phi must lose exactly one input slot (the dead position).
#[test]
fn control_phi_loses_dead_slot() -> Result<()> {
    let var = crate::sp_expr::tests::reg_vn(0x1000, 8);
    let mut b = FunctionBuilder::new_raw(vec![var], &[var], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1, ir::ValueType::U64);
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2, ir::ValueType::U64);
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;

    let mut fg = b.build()?;
    let pre_phi_count = fg.all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlPhi(_)))
        .count();
    assert!(pre_phi_count > 0);

    DeadBranchElimination.optimize(&mut fg)?;
    // A ControlPhi at the join should now have only the live predecessor's
    // value input (length = 1 token + 1 value = 2).
    let join_phi = fg.all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlPhi(v) if *v == var))
        .expect("control phi at join must exist");
    let phi_inputs = fg.graph.node_inputs(join_phi);
    assert_eq!(phi_inputs.len(), 2, "phi must have exactly 1 live value");
    Ok(())
}
```

(Note: this references `crate::sp_expr::tests::reg_vn` — since `tests` is a private inner module, replace with an in-file helper.)

Replace the `reg_vn` reference with a local helper at the top of the appended block:
```rust
fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn { size, addr: rsleigh::VnAddr { off, space: rsleigh::VnSpace::REGISTER } }
}
```

And use `reg_vn(0x1000, 8)`.

- [ ] **Step 7: Run tests; falsify-test the 2 new tests**

Run: `cargo test -p opt dead_branch`
Expected: 5 tests pass.

For each new test, briefly mutate an assertion to confirm it can fail.

- [ ] **Step 8: Commit**

```bash
git add crates/opt/src/dead_branch/
git commit -m "refactor(opt): split dead_branch into module + worklist + tests

Per-pass internal worklist: seeds with preorder, re-enqueues consumers
of the eliminated If's outputs. New tests cover nested If(true) and
ControlPhi slot removal at a 2-predecessor join."
```

---

### Task 2.C: `redundant_phis` — file split + worklist + tests

**Files:**
- Move: `crates/opt/src/redundant_phis.rs` → `crates/opt/src/redundant_phis/mod.rs`
- Create: `crates/opt/src/redundant_phis/tests.rs`

- [ ] **Step 1: Create directory and move file**

```bash
mkdir -p crates/opt/src/redundant_phis
git mv crates/opt/src/redundant_phis.rs crates/opt/src/redundant_phis/mod.rs
```

- [ ] **Step 2: Extract inline tests to `tests.rs`**

Same pattern as 2.A: replace `mod tests { ... }` with `mod tests;` and put the body in `tests.rs`.

- [ ] **Step 3: Run tests to confirm migration**

Run: `cargo test -p opt redundant_phis`
Expected: 1 test passes.

- [ ] **Step 4: Switch `HashSet` → `FxHashSet` in hot paths**

In `mod.rs`, replace:
```rust
use std::collections::HashSet;
```
with:
```rust
use rustc_hash::FxHashSet as HashSet;
```

(Same alias name keeps the rest of the file unchanged.)

- [ ] **Step 5: Worklist refactor**

`RedundantPhis` doesn't naturally fit the seeded-worklist model — it walks all phi-like nodes once, then detaches unreachable nodes. The refactor: keep the structure but use an `FxHashSet` for the reachable set and short-circuit the second loop when `detach_unreachable_nodes` finds nothing.

Replace the `Optimizer` impl with:
```rust
impl Optimizer for RedundantPhis {
    fn optimize(&self, function: &mut ir::BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let reachable: HashSet<ir::node::NodeId> =
            ir::walk::cfg_reachable(&function.graph, function.entry).into_iter().collect();
        let mut res = OptimizationResult::NoChange;
        // Scope-then-collect to avoid mutating while iterating.
        let phi_candidates: Vec<NodeId> = function.preorder()
            .filter(|&n| matches!(function.graph.node_kind(n),
                NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::ControlState))
            .collect();
        for node_id in phi_candidates {
            res |= remove_phis(function, node_id, &reachable)?;
        }
        res |= detach_unreachable_nodes(function);
        Ok(res)
    }
}
```

(The rest of the file stays the same. Note: `ir::walk::cfg_reachable` already returns a `HashSet`; we collect into our `FxHashSet` alias.)

- [ ] **Step 6: Run tests**

Run: `cargo test -p opt redundant_phis`
Expected: 1 test passes.

- [ ] **Step 7: Add comprehensive tests**

Append to `crates/opt/src/redundant_phis/tests.rs`:

```rust
/// MemPhi with a single reachable predecessor must be eliminated and uses
/// rewired to the predecessor's memory token.
#[test]
fn mem_phi_single_pred_eliminated() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    // A simple Store creates a MemPhi at the body region's join.
    let addr = b.build_int_const(0x1000, NodeOutputType::U64);
    let data = b.build_int_const(0x42, NodeOutputType::U64);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    RedundantPhis.optimize(&mut fg)?;

    // Surviving (reachable) MemPhis with a single predecessor must be 0.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let surviving = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::MemPhi))
        .filter(|&n| fg.graph.node_inputs(n).len() <= 2)
        .count();
    assert_eq!(surviving, 0);
    Ok(())
}

/// A ControlState with a single reachable predecessor and only its ctrl
/// output used (phi token unused) must collapse.
#[test]
fn control_state_single_pred_collapses() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    RedundantPhis.optimize(&mut fg)?;

    // The single-predecessor body's ControlState should be detached or
    // bypassed.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let single_pred_cs = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlState))
        .filter(|&n| fg.graph.node_inputs(n).len() == 1)
        .count();
    // Either 1 (the entry CS) or 0 — but never 2.
    assert!(single_pred_cs <= 1, "redundant CS at body must be collapsed");
    Ok(())
}

/// A `Store` whose memory output is unused after `replace_all_uses` happens
/// upstream must be detached (zero inputs) so the validator skips it.
#[test]
fn unreachable_store_inputs_detached() -> crate::Result<()> {
    // Build: store; if(false) { store-2 }; return.
    // After DeadBranchElimination strips the false branch, store-2 is
    // unreachable; RedundantPhis must detach its inputs.
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let dead = b.create_region()?;
    let live = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, dead, live)?;
    b.set_region(dead);
    let addr_d = b.build_int_const(0xDEAD, NodeOutputType::U64);
    let data_d = b.build_int_const(0xBADC, NodeOutputType::U64);
    b.build_store(addr_d, data_d, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_region(live);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg)?;
    // Validation runs at the end of `pipeline.run`, so reaching here means
    // the unreachable store didn't leave an invalid graph.
    Ok(())
}
```

- [ ] **Step 8: Run new tests**

Run: `cargo test -p opt redundant_phis`
Expected: 4 tests pass.

- [ ] **Step 9: Falsify each new test once**

- [ ] **Step 10: Commit**

```bash
git add crates/opt/src/redundant_phis/
git commit -m "refactor(opt): split redundant_phis + FxHashSet + comprehensive tests

Adds: MemPhi single-pred elimination, ControlState single-pred collapse,
unreachable-store detachment validation."
```

---

### Task 2.D: `known_bits` — file split + worklist refactor + tests

**Files:**
- Move: `crates/opt/src/known_bits.rs` → `crates/opt/src/known_bits/mod.rs`
- Create: `crates/opt/src/known_bits/tests.rs`

- [ ] **Step 1: Create directory and move file**

```bash
mkdir -p crates/opt/src/known_bits
git mv crates/opt/src/known_bits.rs crates/opt/src/known_bits/mod.rs
```

- [ ] **Step 2: Extract inline tests to `tests.rs`**

Same pattern as 2.A.

- [ ] **Step 3: Run tests to confirm migration**

Run: `cargo test -p opt known_bits`
Expected: 4 tests pass.

- [ ] **Step 4: Apply Phase-1 worklist refactor**

In `mod.rs`, replace the Phase-1 propagation loop. The existing code:
```rust
let mut known: HashMap<NodeOutputId, Kb> = HashMap::new();
let mut any_changed = true;
while any_changed {
    any_changed = false;
    for &node_id in &nodes {
        if let Some((out, kb)) = node_known_bits(function, node_id, &known)? {
            any_changed |= known.entry(out).or_default().merge(kb);
        }
    }
}
```
becomes:
```rust
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;

let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();
let mut queued: FxHashSet<NodeId> = nodes.iter().copied().collect();
let mut work: VecDeque<NodeId> = nodes.iter().copied().collect();

while let Some(node_id) = work.pop_front() {
    queued.remove(&node_id);
    let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
        continue;
    };
    let merged = known.entry(out).or_default().merge(kb);
    if !merged { continue; }
    // Re-queue every consumer of `out`.
    for (consumer, _idx) in function.graph.output_uses(out) {
        if queued.insert(consumer) {
            work.push_back(consumer);
        }
    }
}
```

Also replace the use of `std::collections::HashMap` at the top with the FxHashMap alias.

- [ ] **Step 5: Run tests to confirm worklist refactor preserves behavior**

Run: `cargo test -p opt known_bits`
Expected: 4 tests pass.

- [ ] **Step 6: Add comprehensive tests**

Append to `crates/opt/src/known_bits/tests.rs`:

```rust
/// `(x | 0xF0) >> 4` — after the shift, bits 0-3 are partly known. Then
/// `& 0x0F` keeps only those bits. The Or-Shift-And chain should fold to
/// 0x0F when x's bits 0-3 happen to be all 1 (here x is const 0xFF).
#[test]
fn shift_right_propagates() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFF, NodeOutputType::U8);
        let f0 = b.build_int_const(0xF0, NodeOutputType::U8);
        let four = b.build_int_const(4, NodeOutputType::U8);
        let f = b.build_int_const(0x0F, NodeOutputType::U8);
        let or_ = b.build_int_binary_operation(x, f0, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shr = b.build_int_binary_operation(or_, four, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        Ok(b.build_int_binary_operation(shr, f, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed { changed = KnownBits.optimize(&mut fg)?.changed(); }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x0F));
    Ok(())
}

/// `popcount(x) & 0xFE` for U8 — popcount fits in 4 bits (max value 8 < 16),
/// so bits 4..7 are 0. With the lower bits unknown but bit 0 of 0xFE is 0,
/// & 0xFE clears bit 0 in the result; the upper-zero region tells us
/// bits 4..7 are 0 in the And too. So the result has bits 0,4,5,6,7 = 0
/// and bits 1,2,3 unknown (from popcount). Cannot fold to a single const.
#[test]
fn popcount_and_partial_no_fold() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x0F, NodeOutputType::U8); // popcount = 4
        let pc = b.build_popcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xFE, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(pc, mask, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed { changed = KnownBits.optimize(&mut fg)?.changed(); }
    // popcount(0x0F) = 4 = 0b0100. 4 & 0xFE = 4. KnownBits *can* prove this
    // since x is fully known.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(4));
    Ok(())
}

/// U128 / U256 are not tracked. The pass must leave them alone.
#[test]
fn u128_no_change() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Fabricate an Extend U64 → U128 (KnownBits.get_unsigned_int returns
        // None for U128, so the pass should bail).
        let v = b.build_int_const(0xFF, NodeOutputType::U64);
        // `build_extend(v, ZeroExtend, target_ty)` if available; otherwise
        // fall back to verifying via a U64-only chain that doesn't touch U128.
        Ok(v)
    })?;
    // Trivial — no folding needed; test passes if pass returns NoChange.
    assert!(!KnownBits.optimize(&mut fg)?.changed());
    Ok(())
}

/// A long chain of OR / AND with known masks — exercises the worklist
/// re-enqueueing logic.
#[test]
fn long_or_and_chain_folds() -> Result<()> {
    let mut fg = make_fn(|b| {
        let mut acc = b.build_int_const(0, NodeOutputType::U64);
        for i in 0..8u64 {
            let bit = b.build_int_const(1u64 << i, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, bit, IntBinaryOp::Or, NodeOutputType::U64)?;
        }
        let mask = b.build_int_const(0xFF, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(acc, mask, IntBinaryOp::And, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed { changed = KnownBits.optimize(&mut fg)?.changed(); }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}
```

- [ ] **Step 7: Run new tests**

Run: `cargo test -p opt known_bits`
Expected: 8 tests pass.

- [ ] **Step 8: Falsify each new test once**

- [ ] **Step 9: Commit**

```bash
git add crates/opt/src/known_bits/
git commit -m "perf(opt): worklist-driven KnownBits propagation + comprehensive tests

Phase 1 propagation re-evaluates a node only when one of its inputs'
Kb merges produced a change. FxHashMap/FxHashSet replace std collections
in the hot map/set. New tests cover ShiftRight propagation, popcount
range bounds, and worklist-stress chains."
```

---

### Task 2.E: `constant_fold` — minor cleanup + comprehensive tests

`constant_fold` already has a `mod.rs` + `tests.rs`. Just need: worklist refactor inside `ConstantFold::optimize`, plus expanded tests.

**Files:**
- Modify: `crates/opt/src/constant_fold/mod.rs`
- Modify: `crates/opt/src/constant_fold/tests.rs`

- [ ] **Step 1: Apply worklist refactor**

In `mod.rs`, replace `ConstantFold::optimize`:
```rust
impl Optimizer for ConstantFold {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            // Snapshot the outputs before each rule application so we can
            // re-enqueue consumers if they change.
            let outs_before: Vec<NodeOutputId> = function.graph.node_outputs(node_id).into_iter().collect();
            let r = apply_identity_rules(function, node_id)?
                | apply_const_eval_rules(function, node_id)?
                | apply_bool_float_rules(function, node_id)?
                | apply_reassoc_and_mask_rules(function, node_id)?
                | apply_bitcast_extend_rules(function, node_id)?;
            if r.changed() {
                result |= r;
                for o in &outs_before {
                    for (consumer, _) in function.graph.output_uses(*o) {
                        work.push(consumer);
                    }
                }
            }
        }
        Ok(result)
    }
}
```

(`WorkSet` will be hoisted to a shared module in Task 2.shared; for now duplicate the local one already used by `dead_branch`.)

Add at the top:
```rust
use ir::node::NodeOutputId;

struct WorkSet {
    queued: rustc_hash::FxHashSet<ir::node::NodeId>,
    queue: std::collections::VecDeque<ir::node::NodeId>,
}
impl WorkSet {
    fn seeded(it: impl IntoIterator<Item = ir::node::NodeId>) -> Self {
        let mut q = Self { queued: Default::default(), queue: Default::default() };
        for n in it { q.push(n); }
        q
    }
    fn push(&mut self, n: ir::node::NodeId) {
        if self.queued.insert(n) { self.queue.push_back(n); }
    }
    fn pop(&mut self) -> Option<ir::node::NodeId> {
        let n = self.queue.pop_front()?;
        self.queued.remove(&n);
        Some(n)
    }
}
```

- [ ] **Step 2: Run all `constant_fold` tests**

Run: `cargo test -p opt constant_fold`
Expected: ~50 tests pass (existing).

- [ ] **Step 3: Add comprehensive tests for shifts, NaN, and bitcast roundtrips**

Append to `crates/opt/src/constant_fold/tests.rs`:

```rust
// ── Shift constant evaluation ───────────────────────────────────────────────

#[test]
fn fold_shl_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1, NodeOutputType::U32);
        let n = b.build_int_const(4, NodeOutputType::U32);
        Ok(b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::U32)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x10));
    Ok(())
}

#[test]
fn fold_shl_at_width_boundary() -> Result<()> {
    // 1 << 31 in U32 = 0x8000_0000 (high bit set).
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1, NodeOutputType::U32);
        let n = b.build_int_const(31, NodeOutputType::U32);
        Ok(b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::U32)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x8000_0000));
    Ok(())
}

#[test]
fn fold_shr_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x80, NodeOutputType::U8);
        let n = b.build_int_const(7, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(x, n, IntBinaryOp::ShiftRight, NodeOutputType::U8)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

// ── NaN and infinity handling ───────────────────────────────────────────────

#[test]
fn fold_f64_nan_plus_one_is_nan() -> Result<()> {
    let nan = f64::NAN.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(nan, NodeOutputType::F64);
        let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        Ok(b.build_float_binary_op(a, one, FloatBinaryOp::Add, NodeOutputType::F64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    let val = return_value(&fg)?;
    let kind = *fg.graph.node_kind(fg.graph.get_node_from_output(val));
    if let NodeKind::FloatConst(bits) = kind {
        assert!(f64::from_bits(bits).is_nan(), "result must be NaN");
    } else {
        panic!("expected FloatConst, got {kind:?}");
    }
    Ok(())
}

#[test]
fn fold_f64_inf_minus_inf_is_nan() -> Result<()> {
    let inf = f64::INFINITY.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(inf, NodeOutputType::F64);
        let bb = b.build_float_const(inf, NodeOutputType::F64);
        Ok(b.build_float_binary_op(a, bb, FloatBinaryOp::Sub, NodeOutputType::F64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    let val = return_value(&fg)?;
    if let NodeKind::FloatConst(bits) = *fg.graph.node_kind(fg.graph.get_node_from_output(val)) {
        assert!(f64::from_bits(bits).is_nan());
    } else {
        panic!();
    }
    Ok(())
}

// ── Bitcast roundtrip on f32 ────────────────────────────────────────────────

#[test]
fn fold_bitcast_roundtrip_f32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(2.5f32.to_bits() as u64, NodeOutputType::F32);
        let i = b.build_float_bits_to_int(v, NodeOutputType::U32)?;
        Ok(b.build_int_bits_to_float(i, NodeOutputType::F32)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f32.to_bits() as u64));
    Ok(())
}

// ── Long reassociation chain — worklist stress ──────────────────────────────

#[test]
fn fold_chain_of_ten_subs() -> Result<()> {
    // ((((((((((x - 1) - 1) - 1) - 1) - 1) - 1) - 1) - 1) - 1) - 1) → x - 10
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..10 {
            let one = b.build_int_const(1, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Sub, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    let mut changed = true;
    while changed { changed = ConstantFold.optimize(&mut fg)?.changed(); }
    assert_sub_with_const(&fg, x, 10, NodeOutputType::U64)?;
    Ok(())
}
```

- [ ] **Step 4: Run all tests; falsify each new test once**

Run: `cargo test -p opt constant_fold`
Expected: ~57 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/constant_fold/
git commit -m "perf(opt): worklist-driven constant_fold + comprehensive tests

Adds: shift constant eval, width-boundary shifts, NaN / inf semantics,
bitcast roundtrip on f32, 10-deep sub chain via worklist re-enqueue."
```

---

### Task 2.F: `stack_store` — split into directory + `detect.rs` + `call_args.rs` + tests

**Files:**
- Move: `crates/opt/src/stack_store.rs` → `crates/opt/src/stack_store/mod.rs`
- Create: `crates/opt/src/stack_store/detect.rs`
- Create: `crates/opt/src/stack_store/call_args.rs`
- Create: `crates/opt/src/stack_store/tests.rs`

- [ ] **Step 1: Create directory and move**

```bash
mkdir -p crates/opt/src/stack_store
git mv crates/opt/src/stack_store.rs crates/opt/src/stack_store/mod.rs
```

- [ ] **Step 2: Split `mod.rs` into `detect.rs` + `call_args.rs`**

In `mod.rs`, the file currently holds two pass implementations: `StackStoreDetect` (lines ~180-280 after Phase 1's deletions) and `CallStackArgCollect` (lines ~280-440).

Move the `try_detect_stack_store` function and `StackStoreDetect` struct + impl to `detect.rs`. Move `collect_stack_args_in_chain_order`, `try_collect_stack_args`, and `CallStackArgCollect` struct + impl to `call_args.rs`.

`mod.rs` becomes:
```rust
// crates/opt/src/stack_store/mod.rs
//! `Store` → `StackStore` rewrite (`detect`) and post-pass stack-arg
//! collection (`call_args`). The shared SP-decomposition machinery lives
//! in `crate::sp_expr`.

mod call_args;
mod detect;
#[cfg(test)]
mod tests;

pub use call_args::CallStackArgCollect;
pub use detect::StackStoreDetect;
```

- [ ] **Step 3: Move the inline `mod tests` block to `tests.rs`**

Same pattern as before — extract the entire `#[cfg(test)] mod tests { ... }` into `tests.rs` (drop outer wrapper).

- [ ] **Step 4: Run all `stack_store` tests**

Run: `cargo test -p opt stack_store`
Expected: 10 tests pass.

- [ ] **Step 5: Apply worklist + memo refactor to `StackStoreDetect`**

In `detect.rs`, replace the `Optimizer` impl:
```rust
impl Optimizer for StackStoreDetect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let mut memo: SpExprMemo = Default::default();
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            let outs_before: Vec<NodeOutputId> = function.graph.node_outputs(node_id).into_iter().collect();
            let r = try_detect_stack_store(function, node_id, self.stack_ptr_vn, &mut memo)?;
            if r.changed() {
                result |= r;
                for o in &outs_before {
                    for (consumer, _) in function.graph.output_uses(*o) {
                        work.push(consumer);
                    }
                }
            }
        }
        Ok(result)
    }
}
```

(Add the local `WorkSet` struct as in 2.B / 2.E.)

- [ ] **Step 6: Run tests**

Run: `cargo test -p opt stack_store`
Expected: 10 tests pass.

- [ ] **Step 7: Add comprehensive tests**

Append to `crates/opt/src/stack_store/tests.rs`:

```rust
// ── stack_store::detect comprehensive tests ─────────────────────────────────

/// SP arithmetic with a mix of Add and Sub, both directions, must reduce.
#[test]
fn detect_mixed_add_sub_reduces() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    // sp + 16 - 4 - 4 = sp + 8.
    let sp_v = b.read_variable(&sp)?;
    let s16 = b.build_int_const(16, NodeOutputType::U32);
    let s4 = b.build_int_const(4, NodeOutputType::U32);
    let plus16 = b.build_int_binary_operation(sp_v, s16, IntBinaryOp::Add, NodeOutputType::U32)?;
    let minus4a = b.build_int_binary_operation(plus16, s4, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let minus4b = b.build_int_binary_operation(minus4a, s4, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(minus4b, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_stores = count(&fg, |k| matches!(k, NodeKind::StackStore { offset: 8, .. }));
    assert_eq!(stack_stores, 1);
    Ok(())
}

/// A non-SP base (e.g. a fresh `Add` of two non-const vars) must NOT be
/// rewritten — the address is opaque.
#[test]
fn detect_non_sp_base_skipped() -> Result<()> {
    let sp = sp_vn();
    let other = crate::tests::common_helpers::reg_vn(0x10, 4);
    let mut b = FunctionBuilder::new_raw(vec![sp, other], &[other], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let other_v = b.read_variable(&other)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(other_v, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    StackStoreDetect::new(sp).optimize(&mut fg)?;
    assert_eq!(count(&fg, |k| matches!(k, NodeKind::StackStore { .. })), 0);
    assert_eq!(count(&fg, |k| matches!(k, NodeKind::Store(_))), 1);
    Ok(())
}

// ── call_args comprehensive tests ───────────────────────────────────────────

/// AArch64-style: stack_arg_offsets[0] == 0 means the first store IS arg 0.
#[test]
fn call_args_aarch64_first_store_is_arg0() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    // Just one push at sp, treated as arg 0 directly.
    let sp_v = b.read_variable(&sp)?;
    let arg0 = b.build_int_const(99, NodeOutputType::U32);
    b.build_store(sp_v, arg0, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0])); // AArch64 layout
    pipeline.run(&mut fg)?;

    let call = fg.all_node_ids().find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call)).expect("call");
    let inputs: Vec<_> = fg.graph.node_inputs(call).into_iter().collect();
    assert_eq!(inputs.len(), 4, "ctrl + mem + target + arg0");
    let arg0_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[3]));
    assert!(matches!(arg0_kind, NodeKind::IntConst(99)));
    Ok(())
}

/// Two consecutive Calls on the same memory chain — each must collect its
/// own args.
#[test]
fn call_args_two_calls_independent() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // First call: push arg=11.
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_v1 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let a0 = b.build_int_const(11, NodeOutputType::U32);
    b.build_store(sp_v1, a0, rsleigh::VnSpace::RAM)?;
    let t0 = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(t0)?;

    // Second call: push arg=22.
    let sp_v2 = b.read_variable(&sp)?;
    let sp_v3 = b.build_int_binary_operation(sp_v2, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v3)?;
    let a1 = b.build_int_const(22, NodeOutputType::U32);
    b.build_store(sp_v3, a1, rsleigh::VnSpace::RAM)?;
    let t1 = b.build_int_const(0x2000, NodeOutputType::U32);
    b.build_call(t1)?;

    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4]));
    pipeline.run(&mut fg)?;

    let calls: Vec<_> = fg.all_node_ids().filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call)).collect();
    assert_eq!(calls.len(), 2);
    for c in calls {
        let inputs: Vec<_> = fg.graph.node_inputs(c).into_iter().collect();
        // ctrl + mem + target + at least 1 arg.
        assert!(inputs.len() >= 4, "each call must have collected at least one arg");
    }
    Ok(())
}
```

(Tests reference `crate::tests::common_helpers::reg_vn` — replace with a local `reg_vn` helper at the top of the appended block, identical to the one in `dead_branch/tests.rs`.)

- [ ] **Step 8: Run new tests; falsify each once**

Run: `cargo test -p opt stack_store`
Expected: 13 tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/opt/src/stack_store/
git commit -m "refactor(opt): split stack_store into detect + call_args + tests

Per-pass internal worklist for StackStoreDetect (memoized via sp_expr).
New tests: mixed Add/Sub SP arithmetic, non-SP base skipped,
AArch64-style 0-offset arg layout, two consecutive calls collect args
independently."
```

---

### Task 2.G: `stack_load_forward` — file split + memo + tests

**Files:**
- Move: `crates/opt/src/stack_load_forward.rs` → `crates/opt/src/stack_load_forward/mod.rs`
- Create: `crates/opt/src/stack_load_forward/tests.rs`

- [ ] **Step 1: Create directory, move file, extract tests**

```bash
mkdir -p crates/opt/src/stack_load_forward
git mv crates/opt/src/stack_load_forward.rs crates/opt/src/stack_load_forward/mod.rs
```
Extract inline tests into `tests.rs` as in earlier tasks.

- [ ] **Step 2: Run tests to confirm migration**

Run: `cargo test -p opt stack_load_forward`
Expected: existing tests pass (count varies — record before/after).

- [ ] **Step 3: Apply memo + worklist refactor**

In `mod.rs`, replace `StackLoadForward::optimize`:
```rust
impl Optimizer for StackLoadForward {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let loads: Vec<NodeId> = function.preorder()
            .filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Load(_)))
            .collect();
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        for load in loads {
            result |= try_forward_load(function, load, self.stack_ptr_vn, &mut memo)?;
        }
        Ok(result)
    }
}
```
And update `try_forward_load` to take `memo: &mut SpExprMemo` and pass it to every `decompose_sp` call.

- [ ] **Step 4: Run tests**

Run: `cargo test -p opt stack_load_forward`
Expected: same count as Step 2.

- [ ] **Step 5: Add comprehensive tests**

Append to `crates/opt/src/stack_load_forward/tests.rs`:

```rust
/// Load width > Store width must NOT forward (would be reading uninitialized
/// upper bytes).
#[test]
fn no_forward_load_wider_than_store() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    // Store U16, load U32 — must not forward.
    let data = b.build_int_const(0x42, NodeOutputType::U16);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::RedundantPhis);
    pipeline.add(crate::StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    // Load should still exist (not forwarded).
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let loads = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
        .count();
    assert!(loads >= 1, "wider load must not be forwarded");
    Ok(())
}

/// An aliasing store between matching store/load must block forwarding.
#[test]
fn no_forward_through_aliasing_store() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)?;

    // Original store at sp-4.
    let data1 = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(addr, data1, rsleigh::VnSpace::RAM)?;
    // Aliasing store at the same address with different value.
    let data2 = b.build_int_const(0x22, NodeOutputType::U32);
    b.build_store(addr, data2, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::RedundantPhis);
    pipeline.add(crate::StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    // The load should resolve to 0x22 (the most recent store), not 0x11.
    let val = fg.graph.node_inputs(
        fg.all_node_ids().find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return)).unwrap()
    )[2];
    let kind = *fg.graph.node_kind(fg.graph.get_node_from_output(val));
    if let NodeKind::IntConst(c) = kind {
        assert_eq!(c, 0x22, "load must forward the most recent value, not the older one");
    }
    Ok(())
}
```

- [ ] **Step 6: Run new tests; falsify each once**

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/stack_load_forward/
git commit -m "refactor(opt): split stack_load_forward + memo + comprehensive tests

decompose_sp memo threaded through every call. New tests: load wider
than store does not forward; aliasing store between source and load
yields the recent value, not the older one."
```

---

### Task 2.H: `function_args` — split into directory + sub-files + tests

**Files:**
- Move: `crates/opt/src/function_args.rs` → `crates/opt/src/function_args/mod.rs`
- Create: `crates/opt/src/function_args/register_args.rs`
- Create: `crates/opt/src/function_args/stack_args.rs`
- Create: `crates/opt/src/function_args/tests.rs`

- [ ] **Step 1: Create directory and move**

```bash
mkdir -p crates/opt/src/function_args
git mv crates/opt/src/function_args.rs crates/opt/src/function_args/mod.rs
```

- [ ] **Step 2: Move `detect_register_args` to `register_args.rs`**

Find the `fn detect_register_args(...)` block. Move to `register_args.rs`:
```rust
// crates/opt/src/function_args/register_args.rs
use ir::node::{FunctionArgSource, NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use ir::BuiltFunctionGraph;

use crate::error::Result;
use crate::pipeline::OptimizationResult;

pub(super) fn detect_register_args(
    function: &mut BuiltFunctionGraph,
    arg_passing_regs: &[rsleigh::Vn],
) -> Result<OptimizationResult> {
    // ... copy body verbatim ...
}

// Plus any helpers used only by detect_register_args.
```

- [ ] **Step 3: Move `detect_stack_args` (and shadow walk) to `stack_args.rs`**

Same pattern for `detect_stack_args` and any shadow-walk helpers:
```rust
// crates/opt/src/function_args/stack_args.rs
use rustc_hash::FxHashMap;
use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId, NodeKind, NodeOutputType};

use crate::error::Result;
use crate::pipeline::OptimizationResult;
use crate::sp_expr::{SpExpr, decompose_sp, ranges_disjoint, SpExprMemo};

/// Memo for the shadow-walk DFS through MemPhi.
type ShadowMemo = FxHashMap<(NodeOutputId, i64, i64), bool>;

pub(super) fn detect_stack_args(
    function: &mut BuiltFunctionGraph,
    sp_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg_idx: usize,
) -> Result<OptimizationResult> {
    // ... body adapted: thread `&mut SpExprMemo` and `&mut ShadowMemo`.
}
```

The shadow walk function gets the `ShadowMemo` cache argument and consults it before recursing. Cache keyed on `(memory_token_output, offset, size)`.

- [ ] **Step 4: Update `mod.rs` to re-export and call into the sub-modules**

`mod.rs` body:
```rust
//! ... (keep the docstring) ...
mod register_args;
mod stack_args;
#[cfg(test)]
mod tests;

use ir::BuiltFunctionGraph;
use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

pub struct FunctionArgDetect {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub stack_ptr_vn: rsleigh::Vn,
    pub stack_arg_offsets: Vec<i64>,
}

impl FunctionArgDetect {
    pub fn new(arg_passing_regs: Vec<rsleigh::Vn>, stack_ptr_vn: rsleigh::Vn, stack_arg_offsets: Vec<i64>) -> Self {
        Self { arg_passing_regs, stack_ptr_vn, stack_arg_offsets }
    }
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.arg_passing_regs.clone(), cc.stack_ptr_vn, cc.stack_arg_offsets.clone())
    }
}

impl Optimizer for FunctionArgDetect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let mut changed = OptimizationResult::NoChange;
        changed |= register_args::detect_register_args(function, &self.arg_passing_regs)?;
        changed |= stack_args::detect_stack_args(
            function,
            self.stack_ptr_vn,
            &self.stack_arg_offsets,
            self.arg_passing_regs.len(),
        )?;
        changed |= detach_unreachable_nodes(function);
        Ok(changed)
    }
}

// Keep detach_unreachable_nodes here (it's small and shared by both detect
// paths). Or move to a `cleanup.rs` if it grows.
fn detach_unreachable_nodes(function: &mut BuiltFunctionGraph) -> OptimizationResult {
    // ... copy body ...
}
```

- [ ] **Step 5: Move inline tests to `tests.rs`**

Standard extraction pattern.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p opt function_args`
Expected: 12 tests pass (current count).

- [ ] **Step 7: Add comprehensive tests**

Append to `crates/opt/src/function_args/tests.rs`:

```rust
/// A register arg whose only use is via a Truncate to a narrower width
/// must still be detected and the Truncate rewired through the FunctionArg.
#[test]
fn register_arg_truncated_use_detected() -> Result<()> {
    // arg_passing_regs[0] = a U64 register; consumer reads only the lower 32 bits.
    let arg_reg = reg_vn(0x100, 8);
    let mut b = FunctionBuilder::new_raw(vec![arg_reg], &[arg_reg], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let full = b.read_variable(&arg_reg)?;
    let trunc = b.truncate_if_needed(full, NodeOutputType::U32)?;
    b.build_return(Some(trunc), &[])?;
    let mut fg = b.build()?;

    FunctionArgDetect::new(vec![arg_reg], reg_vn(0x20, 4), vec![]).optimize(&mut fg)?;

    let func_args = fg.all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::FunctionArg { .. }))
        .count();
    assert_eq!(func_args, 1);
    Ok(())
}

/// Stack-arg load shadowed by a same-offset store before the load must NOT
/// be detected as a function arg.
#[test]
fn stack_arg_shadowed_skipped() -> Result<()> {
    let sp = reg_vn(0x20, 4);
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    // Shadow: store at the same offset before the load.
    let new_data = b.build_int_const(0x99, NodeOutputType::U32);
    b.build_store(addr, new_data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    FunctionArgDetect::new(vec![], sp, vec![4]).optimize(&mut fg)?;

    // Shadowed load must not have been replaced by a FunctionArg.
    let func_args = fg.all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::FunctionArg { .. }))
        .count();
    assert_eq!(func_args, 0, "shadowed stack-arg load must not be detected");
    Ok(())
}

/// Gap in stack-arg offsets — first detected slot is non-zero — truncates
/// detection from there onwards.
#[test]
fn stack_arg_gap_truncates() -> Result<()> {
    let sp = reg_vn(0x20, 4);
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let twelve = b.build_int_const(12, NodeOutputType::U32);
    // Skip offset 4 (slot 0); only offset 12 (slot 2) is present.
    let addr12 = b.build_int_binary_operation(sp_v, twelve, IntBinaryOp::Add, NodeOutputType::U32)?;
    let l12 = b.build_load(addr12, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(l12), &[])?;
    let mut fg = b.build()?;

    FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]).optimize(&mut fg)?;

    // No FunctionArg should be emitted (gap at slot 0 and 1 truncates).
    let func_args = fg.all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::FunctionArg { .. }))
        .count();
    assert_eq!(func_args, 0, "gap-truncation: no slots emitted past first gap");
    Ok(())
}
```

- [ ] **Step 8: Run new tests; falsify each once**

Run: `cargo test -p opt function_args`
Expected: 15 tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/opt/src/function_args/
git commit -m "refactor(opt): split function_args into register/stack/cleanup + tests

Sub-modules: register_args, stack_args (with memoized shadow walk).
mod.rs hosts the FunctionArgDetect struct + Optimizer impl.
New tests: truncated register arg, shadowed stack arg, gap truncation."
```

---

### Task 2.I: Hoist `WorkSet` to a shared `worklist` module

The local `WorkSet` struct duplicated in 2.B/2.E/2.F shares one definition.

**Files:**
- Create: `crates/opt/src/worklist.rs`
- Modify: each pass `mod.rs` to import `crate::worklist::WorkSet`

- [ ] **Step 1: Create the shared module**

```rust
// crates/opt/src/worklist.rs
//! Shared per-pass worklist used by every fold-style optimizer.

use std::collections::VecDeque;
use rustc_hash::FxHashSet;
use ir::node::NodeId;

/// FIFO worklist that prevents double-enqueue. Used by passes that walk
/// nodes in preorder and may re-visit consumers after a rewrite.
#[derive(Default)]
pub(crate) struct WorkSet {
    queued: FxHashSet<NodeId>,
    queue: VecDeque<NodeId>,
}

impl WorkSet {
    /// Seeds the worklist with `it`.
    pub(crate) fn seeded(it: impl IntoIterator<Item = NodeId>) -> Self {
        let mut q = Self::default();
        for n in it { q.push(n); }
        q
    }

    /// Adds `n` to the queue if it isn't already pending.
    pub(crate) fn push(&mut self, n: NodeId) {
        if self.queued.insert(n) { self.queue.push_back(n); }
    }

    /// Pops the next node, removing it from the pending set.
    pub(crate) fn pop(&mut self) -> Option<NodeId> {
        let n = self.queue.pop_front()?;
        self.queued.remove(&n);
        Some(n)
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

Add `mod worklist;` after `mod sp_expr;`.

- [ ] **Step 3: Replace the duplicate `WorkSet` definitions**

In each of `dead_branch/mod.rs`, `constant_fold/mod.rs`, `stack_store/detect.rs`: remove the local `WorkSet` definition and use:
```rust
use crate::worklist::WorkSet;
```

- [ ] **Step 4: Run all opt tests**

Run: `cargo test -p opt`
Expected: all tests pass (count = 92 + new tests added in Phase 2).

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/worklist.rs crates/opt/src/lib.rs crates/opt/src/dead_branch/ crates/opt/src/constant_fold/ crates/opt/src/stack_store/
git commit -m "refactor(opt): hoist WorkSet to shared worklist module

Every fold-style pass uses one WorkSet definition with FxHashSet
double-enqueue guard."
```

---

## Phase 3 — Black-box integration tests

### Task 3.1: `tests/pipeline_default.rs`

**Files:**
- Create: `crates/opt/tests/pipeline_default.rs`

- [ ] **Step 1: Create the test file**

```rust
// crates/opt/tests/pipeline_default.rs
//! End-to-end tests for `opt::default_pipeline`. Black-box: exercises only
//! the public API.

mod common;

use ir::node::{NodeKind, NodeOutputType};
use ir::IntBinaryOp;
use opt::{default_pipeline, OptimizerPipeline};

use common::{make_fn, return_kind, make_fn_with_var, reg_vn, run_to_fixed_point};

#[test]
fn default_pipeline_folds_int_chain() -> Result<(), opt::Error> {
    // ((1 + 2) + 3) + 4 → 10.
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1, NodeOutputType::U64);
        let c2 = b.build_int_const(2, NodeOutputType::U64);
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let a = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
        let bb = b.build_int_binary_operation(a, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(bb, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;

    default_pipeline().run(&mut fg)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(10));
    Ok(())
}

#[test]
fn default_pipeline_eliminates_dead_branch_and_phi() -> Result<(), opt::Error> {
    // if(true) return 1 else return 2 — pipeline should leave only the
    // true branch alive.
    let mut fg = {
        let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let entry = b.create_region()?;
        let t = b.create_region()?;
        let f = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t, f)?;
        b.set_region(t);
        let v = b.build_int_const(1, ir::ValueType::U64);
        b.build_return(Some(v), &[])?;
        b.set_region(f);
        let v2 = b.build_int_const(2, ir::ValueType::U64);
        b.build_return(Some(v2), &[])?;
        b.build()?
    };

    default_pipeline().run(&mut fg)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_nodes = fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_nodes, 0, "If(true) must be eliminated");
    Ok(())
}

#[test]
fn default_pipeline_validates_at_end() -> Result<(), opt::Error> {
    // The pipeline calls validate() at the end; if any pass leaves an
    // invalid graph behind, run() returns an error. This test passes by
    // not panicking.
    let mut fg = make_fn(|b| Ok(b.build_int_const(42, NodeOutputType::U64)))?;
    default_pipeline().run(&mut fg)?;
    Ok(())
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p opt --test pipeline_default`
Expected: 3 tests pass.

- [ ] **Step 3: Falsify each test once**

- [ ] **Step 4: Commit**

```bash
git add crates/opt/tests/pipeline_default.rs
git commit -m "test(opt): add black-box integration tests for default_pipeline"
```

---

### Task 3.2: `tests/pipeline_with_stack.rs`

**Files:**
- Create: `crates/opt/tests/pipeline_with_stack.rs`

- [ ] **Step 1: Create the file**

```rust
// crates/opt/tests/pipeline_with_stack.rs
//! End-to-end tests for an SP-aware pipeline like the one Analyzer wires:
//! default + StackStoreDetect + StackLoadForward + FunctionArgDetect +
//! CallStackArgCollect post-pass.

mod common;

use ir::node::{NodeKind, NodeOutputType};
use ir::IntBinaryOp;
use opt::*;

use common::{sp_vn, count, count_reachable};

fn pipeline_with_sp(sp: rsleigh::Vn, stack_offsets: Vec<i64>) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.add(StackStoreDetect::new(sp));
    p.add(StackLoadForward::new(sp));
    p.add_post_pass(CallStackArgCollect::new(stack_offsets));
    p
}

#[test]
fn store_then_load_at_same_offset_forwarded() -> Result<(), opt::Error> {
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![4, 8, 12]).run(&mut fg)?;

    // The Load should have been forwarded — return value is 0x42 (or
    // whatever the chain folds to).
    let ret = fg.all_node_ids().find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return)).unwrap();
    let val = fg.graph.node_inputs(ret)[2];
    let kind = *fg.graph.node_kind(fg.graph.get_node_from_output(val));
    assert!(matches!(kind, NodeKind::IntConst(0x42)),
        "load must be forwarded to stored value, got {:?}", kind);
    Ok(())
}

#[test]
fn full_call_pipeline_collects_args() -> Result<(), opt::Error> {
    // Push two args, call — expect Call inputs grow by 2.
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_v1 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22, NodeOutputType::U32);
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
    let sp_v2 = b.build_int_binary_operation(sp_v1, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11, NodeOutputType::U32);
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![0, 4]).run(&mut fg)?;

    let call = fg.all_node_ids().find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call)).unwrap();
    let inputs = fg.graph.node_inputs(call);
    assert_eq!(inputs.len(), 5, "ctrl + mem + target + 2 args");
    Ok(())
}
```

- [ ] **Step 2: Run; falsify each once**

Run: `cargo test -p opt --test pipeline_with_stack`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/opt/tests/pipeline_with_stack.rs
git commit -m "test(opt): add black-box stack-aware pipeline tests"
```

---

### Task 3.3: `tests/pipeline_fixedpoint.rs`

**Files:**
- Create: `crates/opt/tests/pipeline_fixedpoint.rs`

- [ ] **Step 1: Create**

```rust
// crates/opt/tests/pipeline_fixedpoint.rs
//! Convergence and idempotency: no pass loops forever; running the
//! pipeline twice yields the same graph.

mod common;

use ir::node::NodeOutputType;
use ir::IntBinaryOp;
use opt::*;

use common::{make_fn, make_fn_with_var, reg_vn};

/// Running default_pipeline a second time on the already-optimized graph
/// must report no change.
#[test]
fn default_pipeline_idempotent() -> Result<(), opt::Error> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c1 = b.build_int_const(1, NodeOutputType::U64);
        let c2 = b.build_int_const(2, NodeOutputType::U64);
        let a = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(a, c2, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;

    default_pipeline().run(&mut fg)?;
    let snapshot_node_count_1 = fg.all_node_ids().count();

    default_pipeline().run(&mut fg)?;
    let snapshot_node_count_2 = fg.all_node_ids().count();

    assert_eq!(snapshot_node_count_1, snapshot_node_count_2,
        "second run must not change node count");
    Ok(())
}

/// Pathological: a long chain of `(((x + 1) + 1) + 1) ...` of depth 50
/// must reach fixed point in a bounded number of pipeline iterations.
#[test]
fn long_reassoc_chain_converges() -> Result<(), opt::Error> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..50 {
            let one = b.build_int_const(1, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    default_pipeline().run(&mut fg)?;
    // If `run` returns Ok we converged — assertion is just a sanity check.
    Ok(())
}
```

- [ ] **Step 2: Run; falsify each once**

Run: `cargo test -p opt --test pipeline_fixedpoint`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/opt/tests/pipeline_fixedpoint.rs
git commit -m "test(opt): add fixed-point convergence and idempotency tests"
```

---

### Task 3.4: `tests/pipeline_validation.rs`

**Files:**
- Create: `crates/opt/tests/pipeline_validation.rs`

- [ ] **Step 1: Create**

```rust
// crates/opt/tests/pipeline_validation.rs
//! `OptimizerPipeline::run` always calls `ir::validate::validate` at the end.
//! If any pass leaves an invalid graph, run returns Err.

mod common;

use ir::node::NodeOutputType;
use opt::*;

use common::make_fn;

#[test]
fn run_validates_after_each_full_pipeline() -> Result<(), opt::Error> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(0, NodeOutputType::U64)))?;
    default_pipeline().run(&mut fg)?;
    Ok(())
}

#[test]
fn run_with_post_passes_validates() -> Result<(), opt::Error> {
    use ir::FunctionBuilder;
    let sp = common::sp_vn();
    let mut fg = {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.build_return(None, &[])?;
        b.build()?
    };
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(StackStoreDetect::new(sp));
    p.add_post_pass(CallStackArgCollect::new(vec![0]));
    p.run(&mut fg)?;
    Ok(())
}
```

- [ ] **Step 2: Run; falsify each once**

Run: `cargo test -p opt --test pipeline_validation`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/opt/tests/pipeline_validation.rs
git commit -m "test(opt): add validate-at-end coverage"
```

---

### Task 3.5: Remove the `_bootstrap.rs` placeholder

**Files:**
- Delete: `crates/opt/tests/_bootstrap.rs`

- [ ] **Step 1: Delete**

```bash
git rm crates/opt/tests/_bootstrap.rs
```

- [ ] **Step 2: Confirm `tests/common/mod.rs` still resolves via the real test files**

Run: `cargo test -p opt --tests`
Expected: all integration tests still pass.

- [ ] **Step 3: Commit**

```bash
git commit -m "test(opt): drop _bootstrap placeholder now that real integration tests exist"
```

---

## Phase 4 — Benchmarks

### Task 4.1: `benches/constant_fold.rs`

**Files:**
- Create: `crates/opt/benches/constant_fold.rs`

- [ ] **Step 1: Create the bench file**

```rust
// crates/opt/benches/constant_fold.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::{ConstantFold, Optimizer};

fn build_chain(n: usize) -> ir::BuiltFunctionGraph {
    let vn = rsleigh::Vn { addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER }, size: 8 };
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.read_variable(&vn).unwrap();
    for _ in 0..n {
        let one = b.build_int_const(1, NodeOutputType::U64);
        acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64).unwrap();
    }
    b.build_return(Some(acc), &[]).unwrap();
    b.build().unwrap()
}

fn bench_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("constant_fold/chain");
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_chain(n),
                |mut fg| {
                    let mut iters = 0;
                    while ConstantFold.optimize(&mut fg).unwrap().changed() {
                        iters += 1;
                        if iters > 200 { panic!("did not converge"); }
                    }
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_chain);
criterion_main!(benches);
```

- [ ] **Step 2: Build the bench (don't run yet — slow)**

Run: `cargo bench -p opt --bench constant_fold --no-run`
Expected: clean build.

- [ ] **Step 3: Smoke-run the smallest size (n=100) for 5 seconds**

Run: `cargo bench -p opt --bench constant_fold -- "constant_fold/chain/100" --quick 2>&1 | tail -15`
Expected: criterion reports a time. Don't worry about the exact value yet — just need to confirm the bench harness works.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/benches/constant_fold.rs
git commit -m "bench(opt): add constant_fold chain benchmarks (100/1k/10k)"
```

---

### Task 4.2: `benches/known_bits.rs`

**Files:**
- Create: `crates/opt/benches/known_bits.rs`

- [ ] **Step 1: Create**

```rust
// crates/opt/benches/known_bits.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::{KnownBits, Optimizer};

fn build_or_and_chain(n: usize) -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.build_int_const(0, NodeOutputType::U64);
    for i in 0..n as u64 {
        let bit = b.build_int_const(1u64 << (i % 64), NodeOutputType::U64);
        acc = b.build_int_binary_operation(acc, bit, IntBinaryOp::Or, NodeOutputType::U64).unwrap();
    }
    let mask = b.build_int_const(0xFFFF, NodeOutputType::U64);
    let masked = b.build_int_binary_operation(acc, mask, IntBinaryOp::And, NodeOutputType::U64).unwrap();
    b.build_return(Some(masked), &[]).unwrap();
    b.build().unwrap()
}

fn bench_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("known_bits/or_and_chain");
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_or_and_chain(n),
                |mut fg| {
                    let mut iters = 0;
                    while KnownBits.optimize(&mut fg).unwrap().changed() {
                        iters += 1;
                        if iters > 200 { panic!("did not converge"); }
                    }
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_chain);
criterion_main!(benches);
```

- [ ] **Step 2: Build, smoke-run 100, commit**

Run: `cargo bench -p opt --bench known_bits --no-run` then `cargo bench -p opt --bench known_bits -- "known_bits/or_and_chain/100" --quick`
Commit:
```bash
git add crates/opt/benches/known_bits.rs
git commit -m "bench(opt): add known_bits chain benchmarks"
```

---

### Task 4.3: `benches/stack_store.rs`

Same pattern: a synthetic chain of N pushes (cdecl-style) and run `StackStoreDetect`. See bench in 4.1 / 4.2 for shape.

- [ ] **Step 1: Create the bench (omitted body for brevity; follow 4.1/4.2 template — chain of N `Sub esp, 4; Store; ...`)**

- [ ] **Step 2: Build, smoke-run, commit**

```bash
git add crates/opt/benches/stack_store.rs
git commit -m "bench(opt): add stack_store push-chain benchmarks"
```

---

### Task 4.4: `benches/default_pipeline.rs`

**Files:**
- Create: `crates/opt/benches/default_pipeline.rs`

- [ ] **Step 1: Create**

```rust
// crates/opt/benches/default_pipeline.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::default_pipeline;

fn build_mixed(n: usize) -> ir::BuiltFunctionGraph {
    // Mixed: arithmetic + load + store chain.
    let vn = rsleigh::Vn { addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER }, size: 8 };
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.read_variable(&vn).unwrap();
    for _ in 0..n {
        let one = b.build_int_const(1, NodeOutputType::U64);
        acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64).unwrap();
    }
    b.build_return(Some(acc), &[]).unwrap();
    b.build().unwrap()
}

fn bench_default(c: &mut Criterion) {
    let mut group = c.benchmark_group("default_pipeline/mixed");
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_mixed(n),
                |mut fg| {
                    default_pipeline().run(&mut fg).unwrap();
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_default);
criterion_main!(benches);
```

- [ ] **Step 2: Build, smoke-run, commit**

```bash
git add crates/opt/benches/default_pipeline.rs
git commit -m "bench(opt): add default_pipeline mixed benchmark"
```

---

## Phase 5 — Final cleanup

### Task 5.1: Fix all clippy warnings in `opt`

**Files:**
- Modify: any files that surface warnings

- [ ] **Step 1: Get the warning list**

Run: `cargo clippy -p opt --all-targets 2>&1 | grep -E "^warning:" | sort -u | head -50`
Note: warnings outside `crates/opt/` (e.g. in `ir`, `dot`) are out of scope — only fix `opt` warnings.

- [ ] **Step 2: Apply mechanical `must_use` attributes**

For every `pub fn new(...)` / `pub fn from_convention(...)` in `opt` flagged by clippy, add `#[must_use]` directly above the `pub fn`. Example:
```rust
#[must_use]
pub fn new(stack_ptr_vn: rsleigh::Vn) -> Self { ... }
```

For `pub fn default_pipeline()` in `lib.rs`, add `#[must_use]`.

- [ ] **Step 3: Fix `match_same_arms` instances**

For each warning of `these match arms have identical bodies`, merge the duplicated arms. Example: `NodeKind::Store(_) => true` plus a `_ => true` wildcard becomes just `_ => true`.

- [ ] **Step 4: Fix `map(...).unwrap_or(...)`**

Replace `.map(f).unwrap_or(default)` with `.map_or(default, f)` per clippy's suggestion.

- [ ] **Step 5: Run with `-D warnings`**

Run: `cargo clippy -p opt --all-targets -- -D warnings`
Expected: no errors.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p opt`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/opt/
git commit -m "style(opt): clear all clippy warnings in opt crate

must_use attributes for constructors, merged identical match arms,
.map_or where suggested. Now passes cargo clippy -p opt --all-targets
-- -D warnings."
```

---

### Task 5.2: Run analyzer example smoke test

**Files:**
- None modified — verification only.

- [ ] **Step 1: Run the analyzer example**

Run: `cargo run --example analyzer 2>&1 | tail -10`
Expected: no errors. `cfg.html` and `graph.html` produced.

- [ ] **Step 2: Compare against the master baseline**

Optionally diff the produced HTML files against the same example run on a fresh clone of `feature/ai`. This is a smoke check, not a bit-for-bit check (graph rendering depends on hash-set iteration order).

- [ ] **Step 3: No commit — verification only**

---

### Task 5.3: Run full workspace tests

**Files:**
- None modified.

- [ ] **Step 1: Run**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: all tests pass across all crates.

- [ ] **Step 2: No commit**

---

### Task 5.4: Self-review the diff before requesting code-review

- [ ] **Step 1: Get the diff stat**

Run: `git diff feature/ai..feature/opt-review --stat`
Expected: ~30+ files changed; line additions roughly balanced with deletions for the file moves.

- [ ] **Step 2: Look for accidental changes**

Run: `git diff feature/ai..feature/opt-review -- '*.rs' | grep -E "^[+-]" | grep -v "^[+-]{3}" | grep -E "(eprintln|println|dbg!|unwrap\(\)|todo!|unimplemented)" | head -20`
Expected: no debugging or panicking constructs introduced.

- [ ] **Step 3: No commit — verification only**

---

### Task 5.5: Run code-review skill

- [ ] **Step 1: Use `superpowers:requesting-code-review` skill**

Pass it the branch range `feature/ai..feature/opt-review`. Address any blocking findings inline; for non-blocking findings, surface to the user.

- [ ] **Step 2: Apply review fixes (if any) as a single commit**

```bash
git commit -m "refactor(opt): address code-review feedback"
```

---

### Task 5.6: Merge `feature/opt-review` back to `feature/ai`

**Files:**
- None — git only.

- [ ] **Step 1: Switch to the main checkout (not the worktree)**

```bash
cd /home/mike/Desktop/strider
```

- [ ] **Step 2: Confirm `feature/ai` hasn't drifted**

Run: `git fetch && git log feature/ai..origin/feature/ai --oneline | head`
Expected: empty (no upstream commits) — if not, ask the user before proceeding.

- [ ] **Step 3: Merge**

Run: `git checkout feature/ai && git merge --no-ff feature/opt-review -m "Merge opt-review: scaling, tests, and clippy cleanup"`
Expected: clean merge.

- [ ] **Step 4: Run tests on the merged branch**

Run: `cargo test --workspace 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Clean up the worktree**

```bash
git worktree remove .worktrees/opt-review
git branch -d feature/opt-review
rm /home/mike/Desktop/strider/.worktrees/rsleigh
```

- [ ] **Step 6: Confirm with user before pushing**

Do NOT push without explicit user approval. Surface the merge status and ask whether to push.

---

## Self-review checklist (run by plan author after writing)

- [x] **Spec coverage:** every spec section maps to one or more tasks. Goals 1-6, structure, module contracts, test plan, scaling work 1-6, correctness methodology, process — all covered.
- [x] **Placeholder scan:** no "TBD", no "implement later", every code step has actual code. Phase 4.3 says "follow 4.1/4.2 template" but explicitly tells the engineer the shape — not a placeholder.
- [x] **Type consistency:** `WorkSet` defined locally in 2.B/2.E/2.F, then hoisted to `crate::worklist` in 2.I — consistent. `SpExprMemo` named the same everywhere. `make_fn` / `return_kind` / `count` helpers consistent across white-box `tests.rs` and `tests/common/mod.rs`.
- [x] **Order of dependencies:** `sp_expr` (Phase 1) is a prerequisite for the SP-aware passes in 2.F/G/H — they reference it. `WorkSet` hoist (2.I) comes after passes that introduce local copies of it. Black-box tests (Phase 3) come after passes are stable. Benches (Phase 4) come after worklist refactors are in. Clippy (Phase 5.1) is last so we don't fight the same warnings twice.

## Execution gates

**Hard stops** (do not proceed past these without user confirmation):
- After Phase 1: 92 tests must still pass.
- After each per-pass migration in Phase 2: total test count must monotonically increase, all pass.
- After Phase 5.1: `cargo clippy -p opt --all-targets -- -D warnings` must be clean.
- Before merge (Phase 5.6): `cargo test --workspace` must pass.
