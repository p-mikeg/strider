# Jump-table Abstract Evaluator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the jump-table classifier's clone-and-optimize core with a read-only abstract evaluator that computes each table target by evaluating the dispatch cone under a concrete index, and delete `Function`/`Graph` `Clone`.

**Architecture:** A recursive, memoized `Evaluator` walks the dispatch value's cone over value edges (including the memory token). Three node-kind families do the work, mirroring the only passes that move a value from a concrete index to a concrete target: ConstFold arithmetic (reusing the existing pure `eval_int_*` helpers), LoadReadOnly (constant-address ROM read), and LoadForward (exact-match dominating-store forwarding via the read-only memory-SSA walk). Any unresolved value or a cycle yields `None`, rejecting the candidate (fail-closed, never over-approximating). `table.rs` keeps candidate detection unchanged and swaps its per-index fold from clone+pipeline to the evaluator. With the only `Function` clones gone, `Clone` is removed from `Function` and the generic `Graph`.

**Tech Stack:** Rust workspace (`strider-opt`, `strider-ir`, `strider-graph`), `rustc_hash`/`smallvec`, `cargo test`/`cargo clippy`.

## Global Constraints

- Rust-only workspace; obey clippy + workspace lints (`cargo clippy --workspace` must be clean).
- Panics/`unwrap`/indexing are acceptable ONLY for validator-guaranteed structural invariants; genuinely-fallible analysis steps return `Option`/`Result` (an unresolvable value is `None`, not a panic).
- Soundness contract (unchanged): the table classifier must NEVER under-approximate the target set. Any value that fails to collapse to a constant ⇒ `None` ⇒ the whole candidate is rejected. Over-approximation (extra dead edges) is acceptable; missing a real target is not.
- Reuse existing pure helpers; do not re-implement folding arithmetic.
- No `Arc`/`Rc`/`Send`/`Sync` added to core types.
- Behavioral regression gate: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs` (x86 / x64 / aarch64 / arm / thumb / mips32 / ppc32; mips64 stays a known pre-existing gap) must stay green through every task.
- Final merge gate: `cargo test --workspace` + `cargo clippy --workspace` + `uv run pytest` (in `crates/strider-py`) all green.

---

## File Structure

- `crates/strider-opt/src/opt/constant_fold/eval_int.rs` — **Modify.** Add pure `eval_int_unary` / `eval_sign_extend` / `eval_popcount` / `eval_lzcount` next to the existing `eval_int_binary` / `eval_int_cmp`, so ConstFold rules and the new evaluator share one source of truth.
- `crates/strider-opt/src/opt/constant_fold/rules.rs` — **Modify.** Refactor the inline unary/sign-extend/popcount/lzcount closures to call the new helpers.
- `crates/strider-opt/src/sp_expr/cfg.rs` — **Modify.** Add a read-only `forwardable_store_data` method on `SpAliasCfg` (the read-only twin of `LoadForward::try_forward_load` steps 1–3).
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` — **Create.** The `Evaluator` struct + recursive `eval`.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs` — **Modify.** Declare `mod eval;`.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs` — **Modify.** Swap the per-index fold to the evaluator; delete `fold_dispatch_to_const`, the clone, the compact/remap, and the pipeline run.
- `crates/strider-ir/src/function/data.rs` — **Modify.** Remove `Clone` from `Function`'s derive.
- `crates/strider-graph/src/graph.rs` — **Modify.** Delete the manual `Clone` impl for the generic `Graph`.

---

### Task 1: Pure integer eval helpers + ConstFold refactor

Extract the inline unary/sign-extend/popcount/lzcount fold logic into pure functions so the evaluator (Task 3) and ConstFold share one implementation.

**Files:**
- Modify: `crates/strider-opt/src/opt/constant_fold/eval_int.rs`
- Modify: `crates/strider-opt/src/opt/constant_fold/rules.rs:612-622, 696-750`
- Test: `crates/strider-opt/src/opt/constant_fold/eval_int.rs` (inline `#[cfg(test)]`), plus existing `crates/strider-opt/src/opt/constant_fold/tests.rs` as the refactor safety net.

**Interfaces:**
- Produces:
  - `pub(crate) fn eval_int_unary(op: strider_ir::IntUnaryOp, v: u128, ty: ValueType) -> Option<u128>`
  - `pub(crate) fn eval_sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128>`
  - `pub(crate) fn eval_popcount(v: u128, in_ty: ValueType) -> Option<u128>`
  - `pub(crate) fn eval_lzcount(v: u128, in_ty: ValueType) -> Option<u128>`
- Consumes: existing `pub(crate) fn require_signed(ty: ValueType, v: u128) -> Result<i128>` (same file).

- [ ] **Step 1: Write the failing test**

Append to `crates/strider-opt/src/opt/constant_fold/eval_int.rs`:

```rust
#[cfg(test)]
mod eval_helper_tests {
    use super::*;
    use strider_ir::IntUnaryOp;
    use strider_ir::node::ValueType;

    #[test]
    fn unary_neg_masks_to_width() {
        // -1 in I8 is 0xFF.
        assert_eq!(eval_int_unary(IntUnaryOp::Neg, 1, ValueType::I8), Some(0xFF));
    }

    #[test]
    fn sign_extend_i8_to_i32() {
        // 0x80 (I8 = -128) sign-extends to 0xFFFF_FF80.
        assert_eq!(
            eval_sign_extend(0x80, ValueType::I8, ValueType::I32),
            Some(0xFFFF_FF80)
        );
    }

    #[test]
    fn popcount_masks_input_width() {
        // 0x1FF masked to I8 is 0xFF → 8 ones.
        assert_eq!(eval_popcount(0x1FF, ValueType::I8), Some(8));
    }

    #[test]
    fn lzcount_zero_is_width_and_msb_is_zero() {
        assert_eq!(eval_lzcount(0, ValueType::I8), Some(8));
        assert_eq!(eval_lzcount(0x80, ValueType::I8), Some(0));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-opt eval_helper_tests`
Expected: FAIL — `cannot find function eval_int_unary` (and the other three).

- [ ] **Step 3: Add the helpers**

Insert into `crates/strider-opt/src/opt/constant_fold/eval_int.rs` after `eval_int_cmp` (keep the existing `use` lines; add `use strider_ir::IntUnaryOp;` to the file's imports):

```rust
/// Evaluates a unary integer op on a constant, masked to `ty`.
/// `IntUnaryOp` has only `Neg` (bitwise complement is `Xor(x, all_ones)`).
pub(crate) fn eval_int_unary(op: IntUnaryOp, v: u128, ty: ValueType) -> Option<u128> {
    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
    };
    ty.get_unsigned_int(raw)
}

/// Sign-extends `v` from `in_ty` and masks the result to `out_ty`.
pub(crate) fn eval_sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128> {
    let signed = require_signed(in_ty, v).ok()? as u128;
    out_ty.get_unsigned_int(signed)
}

/// Population count of `v` masked to `in_ty`.
pub(crate) fn eval_popcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    Some(u128::from(masked.count_ones()))
}

/// Leading-zero count of `v` within `in_ty`'s bit width (input-width-relative).
/// `None` for widths > 128 bits (cannot be carried in u128).
pub(crate) fn eval_lzcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    let bits = in_ty.bit_width() as u32;
    if bits > 128 {
        return None;
    }
    Some(if masked == 0 {
        u128::from(bits)
    } else if bits == 128 {
        u128::from(masked.leading_zeros())
    } else {
        u128::from((masked << (128 - bits)).leading_zeros())
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-opt eval_helper_tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Refactor `rules.rs` to call the helpers**

In `crates/strider-opt/src/opt/constant_fold/rules.rs`, replace the bodies of closures 2, 6, 7, 8 (keep the `rewrite_rule(...)` matcher arguments and surrounding structure identical):

Closure 2 (`int_unary_any`, ~line 615):
```rust
                int_const_with!([op: int_unary_op, v: uint, ty] =>
                    super::eval_int::eval_int_unary(op, v, ty).ok_or_else(strider_pattern::skip)?
                ),
```

Closure 6 (`sign_extend`, ~line 699):
```rust
                int_const_with!([v: uint, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_sign_extend(v, input_ty, ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
```

Closure 7 (`popcount`, ~line 711):
```rust
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_popcount(v, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
```

Closure 8 (`lzcount`, ~line 728):
```rust
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_lzcount(v, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
```

- [ ] **Step 6: Run the ConstFold suite to verify the refactor preserved behavior**

Run: `cargo test -p strider-opt constant_fold`
Expected: PASS (all existing constant_fold tests + the 4 new helper tests).

- [ ] **Step 7: Commit**

```bash
git add crates/strider-opt/src/opt/constant_fold/eval_int.rs crates/strider-opt/src/opt/constant_fold/rules.rs
git commit -m "refactor(opt): extract pure int unary/extend/popcount/lzcount evaluators"
```

---

### Task 2: Read-only forwardable-store lookup on `SpAliasCfg`

Add the read-only twin of `LoadForward::try_forward_load` steps 1–3: given a `Load`, return the data value of the exact-match dominating `Store`, or `None`. Uses the no-narrowing `find_nearest_clobber` so it works on `&Function`.

**Files:**
- Modify: `crates/strider-opt/src/sp_expr/cfg.rs` (add method on `impl SpAliasCfg`; ensure `use super::{alias_verdict, AliasVerdict};` is present — they are re-exported `pub(crate)` from `sp_expr` and `AliasVerdict` is already referenced in this file).
- Test: covered indirectly by Task 4's `table_tests.rs` integration gate (constructing an isolated SP-rooted store/load chain by hand is higher-cost than the lifted-binary coverage already exercises this path).

**Interfaces:**
- Produces: `pub(crate) fn forwardable_store_data(&mut self, function: &Function, load: NodeId) -> Option<ValueId>` on `SpAliasCfg`.
- Consumes: existing private `fn oracle(&mut self, AddrClass, i64, VnSpace) -> SpAliasOracle<'_>`, `fn classify_addr`, the `MemorySSAWalker::find_nearest_clobber` default method, `alias_verdict`, `AliasVerdict::Match`.

- [ ] **Step 1: Add the method**

Insert into `crates/strider-opt/src/sp_expr/cfg.rs` inside `impl<'m> SpAliasCfg<'m>` (after `nearest_clobber`):

```rust
    /// Read-only twin of `LoadForward`'s forward decision: the data value of
    /// the exact-match `Store` that is the nearest may-aliasing definition
    /// reaching `load`, or `None` when the nearest covering def is not an
    /// exact-match same-location store (a `Call`, a disagreeing `MemPhi`,
    /// `InitialMemory`, an overlapping-but-shifted store, or an opaque
    /// producer). Performs NO narrowing — safe from a `&Function` context.
    /// The caller reshapes the returned value from the store width to the
    /// load width (`Endianness`-aware), exactly as `LoadForward::narrow` does.
    pub(crate) fn forwardable_store_data(
        &mut self,
        function: &Function,
        load: NodeId,
    ) -> Option<ValueId> {
        // Load inputs: [memory, addr].
        let [mem, addr] = function.node_inputs_exact::<2>(load).ok()?;
        let [load_value] = function.node_outputs_exact::<1>(load).ok()?;
        let load_ty = function.value_type_opt(load_value)?;
        let load_size = load_ty.byte_size() as i64;
        let load_space = match function.node_kind(load) {
            NodeKind::Load(s) => *s,
            _ => return None,
        };
        let load_class = self.classify_addr(function, addr);

        // Nearest may-aliasing def (read-only walk, no narrowing).
        let clobber = {
            use super::mem_ssa::MemorySSAWalker;
            let mut oracle = self.oracle(load_class, load_size, load_space);
            oracle.find_nearest_clobber(function, function.producer(mem))
        };
        if !matches!(function.node_kind(clobber), NodeKind::Store(_)) {
            return None;
        }

        // Exact-match check: same location, store covers the load's bytes.
        let store_addr = function.store_addr(clobber);
        let data = function.store_data(clobber);
        let data_ty = function.value_type_opt(data)?;
        let store_size = data_ty.byte_size() as i64;
        let store_class = self.classify_addr(function, store_addr);
        if alias_verdict(
            load_class,
            load_size,
            store_class,
            store_size,
            self.alias_mode,
            false,
        ) != AliasVerdict::Match
        {
            return None;
        }
        Some(data)
    }
```

If `cfg.rs` does not already `use` the free `alias_verdict` function or `AliasVerdict`, add at the top: `use super::{AliasVerdict, alias_verdict};`. (`NodeKind`, `NodeId`, `ValueId`, `Function` are already imported in this file.)

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p strider-opt`
Expected: builds clean (no callers yet — `#[allow(dead_code)]` is not needed because Task 3 consumes it in the same PR; if building this task in isolation triggers a dead-code lint, the next task removes it).

- [ ] **Step 3: Run the load_forward suite (sanity — unchanged pass)**

Run: `cargo test -p strider-opt load_forward`
Expected: PASS (this task adds a method but changes no existing behavior).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-opt/src/sp_expr/cfg.rs
git commit -m "feat(opt): read-only forwardable_store_data lookup on SpAliasCfg"
```

---

### Task 3: The `Evaluator`

The recursive, memoized abstract evaluator over the dispatch cone.

**Files:**
- Create: `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs`
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs` (add `mod eval;` near the other module declarations)
- Test: inline `#[cfg(test)]` in `eval.rs` (arithmetic + fail-closed), using `strider-ir-test-utils` + `IRBuilderExt`.

**Interfaces:**
- Produces:
  - `pub(crate) struct Evaluator<'a>` with `pub(crate) fn new(function: &'a strider_ir::Function, rom: Option<&'a dyn strider_ir::ReadOnlyMemory>, alias_mode: crate::AliasMode) -> Self`
  - `pub(crate) fn eval_target(&mut self, dispatch: ValueId, idx_value: ValueId, idx: u128) -> Option<u64>`
- Consumes: Task 1 helpers (`crate::opt::constant_fold::eval_int::{eval_int_binary, eval_int_cmp, eval_int_unary, eval_sign_extend, eval_popcount, eval_lzcount}`), Task 2 `SpAliasCfg::forwardable_store_data`, `crate::sp_expr::{SpAliasCfg, SpExprMemo}`.

- [ ] **Step 1: Declare the module**

In `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs`, add alongside the existing `pub mod table;`:

```rust
mod eval;
```

- [ ] **Step 2: Write the failing test**

Create `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::Evaluator;
    use strider_ir::IRBuilderExt;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::make_empty_fn;

    // Build `Add(idx, 100)` over a fresh function and return (function, idx, sum).
    // `make_empty_fn` yields a built Function plus a FunctionBuilder-style handle;
    // adapt to the test-utils surface in use (see existing strider-opt tests that
    // build small graphs).
    #[test]
    fn evaluates_add_under_seed() {
        let (function, idx, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // idx = 5 → 105
        assert_eq!(ev.eval_target(sum, idx, 5), Some(105));
        // idx = 7 → 107 (re-seed, fresh memo)
        assert_eq!(ev.eval_target(sum, idx, 7), Some(107));
    }

    #[test]
    fn unresolvable_input_is_none() {
        // A value whose cone has a non-seeded, non-constant leaf cannot collapse.
        let (function, unrelated, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // Seed a DIFFERENT value than the one in the cone → sum stays symbolic.
        assert_eq!(ev.eval_target(sum, unrelated_other_value(&function), 5), None);
        let _ = unrelated;
    }

    // Helpers below are written against the project's test-utils graph builder.
    fn build_add_idx_100() -> (strider_ir::Function, strider_ir::node::ValueId, strider_ir::node::ValueId) {
        // Construct via the test-utils builder: an InitialVar `idx` and an
        // IntConst 100, summed at I64. Return (built function, idx value, sum value).
        // Fill in using the same builder calls existing strider-opt unit tests use
        // (e.g. constant_fold/tests.rs builds `IntBinaryOp` nodes via IRBuilderExt).
        todo!("build with test-utils FunctionBuilder; see constant_fold/tests.rs")
    }
    fn unrelated_other_value(_f: &strider_ir::Function) -> strider_ir::node::ValueId {
        todo!("a value id not present in the dispatch cone")
    }
}
```

> NOTE TO IMPLEMENTER: `build_add_idx_100` / `unrelated_other_value` are the ONLY
> `todo!`s in this plan and exist because the exact test-utils builder calls must
> be copied from a working example. Before Step 3, open
> `crates/strider-opt/src/opt/constant_fold/tests.rs` and replicate its graph-build
> pattern (it builds `IntBinaryOp(IntConst, …)` graphs via `IRBuilderExt`), then
> replace both `todo!`s with real construction. Do not proceed with `todo!`s in place.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p strider-opt indirect_branch_resolve::eval`
Expected: FAIL — `Evaluator` not found / `todo!` panic.

- [ ] **Step 4: Implement the evaluator**

Prepend to `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` (above the test module):

```rust
//! Read-only abstract evaluation of a jump-table dispatch cone.
//!
//! Computes the concrete branch target for a concrete index by walking the
//! dispatch value's cone (over value edges, including the memory token) and
//! evaluating each node. Three node families do the work — mirroring the only
//! passes that move a value from a concrete index to a concrete target:
//! ConstFold arithmetic, `LoadReadOnly` (constant-address ROM read), and
//! `LoadForward` (exact-match dominating-store forwarding). No graph mutation,
//! no clone, no pipeline.
//!
//! Soundness: any unresolved value or a cycle yields `None`, so the caller
//! rejects the candidate. The evaluator never over- or under-approximates a
//! concrete value — it returns the exact constant or nothing.

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::{
    eval_int_binary, eval_int_cmp, eval_int_unary, eval_lzcount, eval_popcount, eval_sign_extend,
};
use crate::sp_expr::{SpAliasCfg, SpExprMemo};

/// Read-only abstract evaluator over a single function's dispatch cones.
/// `sp_memo` (index-independent SP classification) persists across indices;
/// the value `cache` and `in_progress` guard are reset per index.
pub(crate) struct Evaluator<'a> {
    function: &'a strider_ir::Function,
    rom: Option<&'a dyn ReadOnlyMemory>,
    alias_mode: crate::AliasMode,
    endianness: Endianness,
    sp_memo: SpExprMemo,
    cache: FxHashMap<ValueId, Option<u128>>,
    in_progress: FxHashSet<ValueId>,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(
        function: &'a strider_ir::Function,
        rom: Option<&'a dyn ReadOnlyMemory>,
        alias_mode: crate::AliasMode,
    ) -> Self {
        Self {
            function,
            rom,
            alias_mode,
            endianness: function.endianness(),
            sp_memo: SpExprMemo::default(),
            cache: FxHashMap::default(),
            in_progress: FxHashSet::default(),
        }
    }

    /// Evaluate `dispatch` with `idx_value` bound to `idx`. Returns the
    /// resolved branch target narrowed to `u64`, or `None` if anything in the
    /// cone fails to collapse (reject this candidate).
    pub(crate) fn eval_target(
        &mut self,
        dispatch: ValueId,
        idx_value: ValueId,
        idx: u128,
    ) -> Option<u64> {
        // Per-index reset: the value cache and cycle guard depend on the seed.
        // sp_memo is index-independent and intentionally kept.
        self.cache.clear();
        self.in_progress.clear();
        self.cache.insert(idx_value, Some(idx));
        u64::try_from(self.eval(dispatch)?).ok()
    }

    fn eval(&mut self, value: ValueId) -> Option<u128> {
        if let Some(&cached) = self.cache.get(&value) {
            return cached;
        }
        // Cycle (loop-carried phi / self-referential value) → fail closed.
        if !self.in_progress.insert(value) {
            return None;
        }
        let result = self.eval_uncached(value);
        self.in_progress.remove(&value);
        self.cache.insert(value, result);
        result
    }

    fn eval_uncached(&mut self, value: ValueId) -> Option<u128> {
        let f = self.function;
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        let out_ty = f.value_type_opt(value);
        // Value-typed inputs only (drops Control / Memory / PhiToken edges).
        let ins: SmallVec<[ValueId; 2]> = f
            .node_inputs(node)
            .into_iter()
            .filter(|&i| f.value_type_opt(i).is_some())
            .collect();
        match kind {
            NodeKind::IntConst(_) => f.int_const_u128(value),
            NodeKind::IntBinaryOp(op) => {
                let l = self.eval(*ins.first()?)?;
                let r = self.eval(*ins.get(1)?)?;
                eval_int_binary(op, l, r, out_ty?)
            }
            NodeKind::IntUnaryOp(op) => {
                let v = self.eval(*ins.first()?)?;
                eval_int_unary(op, v, out_ty?)
            }
            NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
                let v = self.eval(*ins.first()?)?;
                out_ty?.get_unsigned_int(v)
            }
            NodeKind::Extend(ExtendOp::SignExtend) => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.eval(ins[0])?;
                eval_sign_extend(v, in_ty, out_ty?)
            }
            NodeKind::Popcount => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.eval(ins[0])?;
                eval_popcount(v, in_ty)
            }
            NodeKind::Lzcount => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.eval(ins[0])?;
                eval_lzcount(v, in_ty)
            }
            NodeKind::IntCmpOp(op) => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let l = self.eval(ins[0])?;
                let r = self.eval(*ins.get(1)?)?;
                eval_int_cmp(op, l, r, in_ty).ok().map(u128::from)
            }
            NodeKind::Load(_) => self.eval_load(node, value),
            NodeKind::Phi => self.eval_phi(node),
            _ => None,
        }
    }

    /// `LoadReadOnly` then `LoadForward`, read-only.
    fn eval_load(&mut self, node: NodeId, value: ValueId) -> Option<u128> {
        let f = self.function;
        let rom = self.rom;
        let endianness = self.endianness;
        let load_ty = f.value_type_opt(value)?;

        // LoadReadOnly: constant address resolvable in the ROM image.
        if let Some(rom) = rom {
            let addr_value = f.load_addr(node);
            if let Some(addr) = self.eval(addr_value).and_then(|a| u64::try_from(a).ok()) {
                let size = load_ty.byte_size();
                if size <= 16 {
                    let mut bytes = [0u8; 16];
                    if rom.read(addr, &mut bytes[..size]).is_ok() {
                        let loaded = endianness.read_uint(&bytes[..size]);
                        return load_ty.get_unsigned_int(loaded);
                    }
                }
            }
        }

        // LoadForward: exact-match dominating store; reshape store→load width.
        let data = {
            let mut cfg = SpAliasCfg::call_blocking(&mut self.sp_memo, self.alias_mode);
            cfg.forwardable_store_data(f, node)
        }?;
        let data_ty = f.value_type_opt(data)?;
        let v = self.eval(data)?;
        self.reshape(v, data_ty, load_ty)
    }

    /// Reshape a stored value to a narrower load width (mirrors
    /// `LoadForward::narrow`). Equal widths pass through; a wider load from a
    /// narrower store cannot be backed → `None`.
    fn reshape(&self, v: u128, data_ty: ValueType, load_ty: ValueType) -> Option<u128> {
        if data_ty == load_ty {
            return Some(v);
        }
        if data_ty.is_integer()
            && load_ty.is_integer()
            && load_ty.byte_size() < data_ty.byte_size()
        {
            let shifted = match self.endianness {
                Endianness::Little => v,
                Endianness::Big => {
                    let shift_bits =
                        ((data_ty.byte_size() - load_ty.byte_size()) as u32) * 8;
                    v >> shift_bits
                }
            };
            return load_ty.get_unsigned_int(shifted);
        }
        None
    }

    /// All-arms-agree: every value arm must collapse to the same constant.
    fn eval_phi(&mut self, node: NodeId) -> Option<u128> {
        let arms: SmallVec<[ValueId; 4]> = self
            .function
            .node_inputs(node)
            .into_iter()
            .filter(|&i| self.function.value_type_opt(i).is_some())
            .collect();
        let mut agreed: Option<u128> = None;
        for arm in arms {
            let v = self.eval(arm)?;
            match agreed {
                None => agreed = Some(v),
                Some(prev) if prev == v => {}
                Some(_) => return None,
            }
        }
        agreed
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p strider-opt indirect_branch_resolve::eval`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs
git commit -m "feat(opt): abstract evaluator for jump-table dispatch cones"
```

---

### Task 4: Rewire `table.rs` onto the evaluator; delete clone+pipeline

Swap the per-index fold from clone+optimize to the evaluator. The existing `table_tests.rs` suite is the characterization gate.

**Files:**
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs`
- Test: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs` (unchanged — the regression gate).

**Interfaces:**
- Consumes: Task 3 `Evaluator::{new, eval_target}`.
- Removes: `fold_dispatch_to_const`.

- [ ] **Step 1: Run the integration gate BEFORE the change (baseline)**

Run: `cargo test -p strider-opt indirect_branch_resolve::table`
Expected: PASS (record the passing test count — this is the baseline to preserve).

- [ ] **Step 2: Rewrite the body of `classify_table_dispatch`**

In `table.rs`, replace everything from `// Clone + compact ONCE up front:` through the end of the `for (idx_value, lo, hi) in candidates { ... } None` block with:

```rust
    // Evaluate the dispatch cone under each concrete index — no clone, no
    // pipeline. The candidate whose whole range collapses to constants IS the
    // index; the constants are the targets. A wrong candidate leaves the cone
    // dependent on a non-seeded runtime value and fails to collapse → rejected.
    let mut ev = super::eval::Evaluator::new(ctx, rom, alias_mode);
    for (idx_value, lo, hi) in candidates {
        if let Some(targets) =
            enumerate_targets(lo, hi, |v| ev.eval_target(anchor_value, idx_value, v))
        {
            return Some(ResolvedTargets::Multiple(targets));
        }
    }
    None
}
```

- [ ] **Step 3: Delete `fold_dispatch_to_const` and now-unused imports**

Remove the entire `fn fold_dispatch_to_const(...) { ... }` function. Then drop imports that only it used. Check and remove if now-unused: `use crate::ReadOnlyMemory;` stays (still a parameter type); `strider_ir::IRBuilderExt`, `crate::EditFunction`, `crate::OptCtx`, `crate::default_pipeline` were local to `fold_dispatch_to_const` — confirm none remain referenced. Run `cargo build -p strider-opt` and fix any unused-import warnings it flags.

- [ ] **Step 4: Update the module doc comment**

Replace the "2. **Pin and fold.**" bullet and the `## Soundness` "Complete fold" paragraph references to cloning/pipeline with the evaluator description. Minimal edit: change "clone the function, substitute the candidate with `IntConst(i)` … and run the canonical `crate::default_pipeline` on the clone" to "evaluate the dispatch cone under `index = i` via the read-only `eval::Evaluator` (ConstFold arithmetic + `LoadReadOnly` ROM reads + `LoadForward` store forwarding)"; change "The clone is disposable, so a destructive pipeline run leaves the analysed function untouched" to "The evaluator is read-only, so the analysed function is never mutated." Keep the soundness gates wording otherwise intact.

- [ ] **Step 5: Run the integration gate AFTER the change**

Run: `cargo test -p strider-opt indirect_branch_resolve`
Expected: PASS with the SAME test count as Step 1's baseline. If any arch table regresses, STOP and debug with superpowers:systematic-debugging before continuing — a regression here means the evaluator misses a shape the pipeline folded (likely a value-`Phi` arm, a reshape, or a node kind returning `None` in `eval_uncached`'s `_` arm).

- [ ] **Step 6: Run the full strider-opt suite**

Run: `cargo test -p strider-opt`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs
git commit -m "refactor(opt): resolve jump tables via abstract eval, drop clone+pipeline"
```

---

### Task 5: Remove `Clone` from `Function` and the generic `Graph`

With the only whole-`Function` clones gone, delete the capability. The compiler is the gate.

**Files:**
- Modify: `crates/strider-ir/src/function/data.rs:96`
- Modify: `crates/strider-graph/src/graph.rs:57-66`

**Interfaces:** none (pure capability removal).

- [ ] **Step 1: Remove `Clone` from `Function`'s derive**

In `crates/strider-ir/src/function/data.rs`, change line 96:
```rust
#[derive(Default)]
pub struct Function {
```
(was `#[derive(Default, Clone)]`).

- [ ] **Step 2: Delete the generic `Graph` Clone impl**

In `crates/strider-graph/src/graph.rs`, delete the entire `impl<N: Clone, V: Clone, C: NodeCacheable<N, V>> Clone for Graph<N, V, C> { ... }` block (lines ~52–66, including the preceding `// Manual Clone ...` comment).

- [ ] **Step 3: Build the workspace to find any remaining clone consumers**

Run: `cargo build --workspace`
Expected: builds clean. If a `.clone()` on a `Function`/`Graph` surfaces (it should not — exploration confirmed table.rs was the only consumer), that call site was missed in Task 4; remove it.

- [ ] **Step 4: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS (no NEW failures vs the known pre-existing baseline).

- [ ] **Step 5: Clippy**

Run: `cargo clippy --workspace`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-ir/src/function/data.rs crates/strider-graph/src/graph.rs
git commit -m "refactor(ir): drop Clone from Function and the generic Graph"
```

---

## Final Verification

- [ ] `cargo test --workspace` — green (no new failures).
- [ ] `cargo clippy --workspace` — clean.
- [ ] `cd crates/strider-py && uv run pytest` — green.
- [ ] Push the branch and STOP — prompt the user before merging (do not merge autonomously).

---

## Self-Review Notes

- **Spec coverage:** trait/evaluator (Task 3), ConstFold/LoadReadOnly/LoadForward reuse (Tasks 1–3), memory-inclusive cone via recursive eval (Task 3 `eval` follows all value inputs incl. memory token through `eval_load`), fail-closed + cycle guard (Task 3), phi all-arms-agree (Task 3 `eval_phi`), clone deletion (Task 5), integration-suite gate (Task 4). All covered.
- **Deviation from the committed spec's test list:** the spec listed per-node-kind unit suites plus dedicated phi/reshape unit tests. This plan unit-tests the pure arithmetic spine (Task 1 helpers) + the evaluator's arithmetic/fail-closed paths (Task 3), and relies on the 7-arch `table_tests.rs` characterization suite (Task 4) for the graph-heavy load/forward/phi/reshape paths — building those in isolated unit tests costs more than the lifted-binary coverage already provides. Flagged for user awareness.
- **Recursion depth:** dispatch cones are shallow; `eval` recurses on a DAG with memoization + cycle guard. If a pathological depth ever appears, convert to an explicit work-stack — not pre-solved (YAGNI).
- **Type consistency:** `Evaluator::eval_target(dispatch, idx_value, idx)` / `Evaluator::new(function, rom, alias_mode)` used identically in Task 3 and Task 4. `forwardable_store_data(function, load)` defined in Task 2, consumed in Task 3.
