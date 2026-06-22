# Jump-table Abstract Evaluator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the jump-table classifier's clone-and-optimize core with a read-only abstract evaluator that computes each table target by evaluating the dispatch cone under a concrete index, and delete `Function`/`Graph` `Clone`.

**Architecture:** Build the dispatch value's cone once (over value edges), topologically order it, and run a flat per-index pass. The abstract value is `Const(u128) | SpRel{base, offset}` (the stack pointer is symbolic, so stack addresses can't be a pure number). Three node families do the work, mirroring the only passes that move a value from a concrete index to a concrete target: ConstFold arithmetic (reusing the pure `eval_int_*` helpers), LoadReadOnly (constant-address ROM read), and LoadForward (the index is folded into an `SpRel` offset, then the existing `SpAliasCfg::reaching_store` finds the store at that concrete offset). Any unresolved value, a non-`Const` dispatch result, or a cycle yields `None`, rejecting the candidate (fail-closed). With the only `Function` clones gone, `Clone` is removed from `Function` and the generic `Graph`.

**Tech Stack:** Rust workspace (`strider-opt`, `strider-ir`, `strider-graph`), `rustc_hash`/`smallvec`, `cargo test`/`cargo clippy`.

## Global Constraints

- Rust-only workspace; obey clippy + workspace lints (`cargo clippy --workspace` clean).
- Panics/`unwrap`/indexing only for validator-guaranteed structural invariants; genuinely-fallible analysis steps return `Option` (an unresolvable value is `None`, not a panic).
- Soundness (unchanged): the classifier must NEVER under-approximate the target set. Any value that fails to resolve to a concrete number ⇒ `None` ⇒ candidate rejected. Over-approximation (extra dead edges) is acceptable; missing a real target is not.
- Reuse existing pure helpers (`eval_int_*`, `reaching_store`); do not re-implement folding or store lookup.
- No `Arc`/`Rc`/`Send`/`Sync` added to core types.
- Behavioral regression gate: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs` (x86 / x64 / aarch64 / arm / thumb / mips32 / ppc32 — incl. SP-rooted stack-table + alias-mode cases; mips64 stays a known gap) must stay green through every task.
- Final merge gate: `cargo test --workspace` + `cargo clippy --workspace` + `uv run pytest` (in `crates/strider-py`) all green.

---

## File Structure

- `crates/strider-opt/src/opt/constant_fold/eval_int.rs` — **Modify.** Add pure `eval_int_unary` / `eval_sign_extend` / `eval_popcount` / `eval_lzcount`.
- `crates/strider-opt/src/opt/constant_fold/rules.rs` — **Modify.** Refactor the inline unary/sign-extend/popcount/lzcount closures to call the new helpers.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` — **Create (Task 2), then Modify (Task 3).** The `Abs` value, the `Evaluator`, and `cone_order`; Task 3 slims it to delegate const arms to the shared utility.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs` — **Modify.** Declare `mod eval;`.
- `crates/strider-opt/src/const_eval.rs` — **Create (Task 3).** Shared `read_rom_const` + `eval_node_const` — the single "node → constant from constant inputs" SSoT, used by the evaluator and `LoadReadOnly`.
- `crates/strider-opt/src/lib.rs` — **Modify (Task 3).** Declare `mod const_eval;`.
- `crates/strider-opt/src/opt/load_readonly/mod.rs` — **Modify (Task 3).** Fold via `eval_node_const` instead of an inline ROM decode.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs` — **Modify.** Swap the per-index fold to the evaluator; delete `fold_dispatch_to_const`, the clone, the compact/remap, the pipeline run.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` — **Modify again (Task 5).** Replace the `initial_sp_value` sp-leaf detection with `SpDecomposer::decompose` so alignment-masked `(sp & mask)` frame bases resolve.
- `crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs` — **Modify (Task 5).** Add an aligned-stack (`& mask`) resolution test.
- `crates/strider-ir/src/function/data.rs` — **Modify (Task 6).** Remove `Clone` from `Function`'s derive.
- `crates/strider-graph/src/graph.rs` — **Modify (Task 6).** Delete the generic `Graph` `Clone` impl.

`SpAliasCfg::reaching_store` and `ReachingSpStore` already exist (`crates/strider-opt/src/sp_expr/cfg.rs`) — no new SP-lookup helper is needed.

---

### Task 1: Pure integer eval helpers + ConstFold refactor

Extract the inline unary/sign-extend/popcount/lzcount fold logic into pure functions so the evaluator (Task 2) and ConstFold share one implementation.

**Files:**
- Modify: `crates/strider-opt/src/opt/constant_fold/eval_int.rs`
- Modify: `crates/strider-opt/src/opt/constant_fold/rules.rs:612-622, 696-750`
- Test: inline `#[cfg(test)]` in `eval_int.rs`, plus existing `constant_fold/tests.rs` as the refactor safety net.

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
        assert_eq!(eval_int_unary(IntUnaryOp::Neg, 1, ValueType::I8), Some(0xFF));
    }

    #[test]
    fn sign_extend_i8_to_i32() {
        assert_eq!(
            eval_sign_extend(0x80, ValueType::I8, ValueType::I32),
            Some(0xFFFF_FF80)
        );
    }

    #[test]
    fn popcount_masks_input_width() {
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

Insert into `eval_int.rs` after `eval_int_cmp` (add `use strider_ir::IntUnaryOp;` to the file imports):

```rust
/// Evaluates a unary integer op on a constant, masked to `ty`.
pub(crate) fn eval_int_unary(op: IntUnaryOp, v: u128, ty: ValueType) -> Option<u128> {
    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
    };
    ty.get_unsigned_int(raw)
}

/// Sign-extends `v` from `in_ty`, masked to `out_ty`.
pub(crate) fn eval_sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128> {
    let signed = require_signed(in_ty, v).ok()? as u128;
    out_ty.get_unsigned_int(signed)
}

/// Population count of `v` masked to `in_ty`.
pub(crate) fn eval_popcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    Some(u128::from(masked.count_ones()))
}

/// Leading-zero count of `v` within `in_ty`'s width; `None` for widths > 128.
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

- [ ] **Step 5: Refactor `rules.rs` closures 2, 6, 7, 8**

In `rules.rs`, replace the bodies (keep the `rewrite_rule(...)` matcher arguments identical):

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

- [ ] **Step 6: Run the ConstFold suite (refactor preserved behavior)**

Run: `cargo test -p strider-opt constant_fold`
Expected: PASS (existing tests + the 4 new helper tests).

- [ ] **Step 7: Commit**

```bash
git add crates/strider-opt/src/opt/constant_fold/eval_int.rs crates/strider-opt/src/opt/constant_fold/rules.rs
git commit -m "refactor(opt): extract pure int unary/extend/popcount/lzcount evaluators"
```

---

### Task 2: The `Evaluator`

The flat-RPO abstract evaluator over the dispatch cone, with the `Const | SpRel` domain.

**Files:**
- Create: `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs`
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs` (add `mod eval;`)
- Test: inline `#[cfg(test)]` in `eval.rs` (arithmetic + fail-closed).

**Interfaces:**
- Produces:
  - `pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId>`
  - `pub(crate) struct Evaluator<'a>` with `pub(crate) fn new(function: &'a strider_ir::Function, rom: Option<&'a dyn strider_ir::ReadOnlyMemory>, alias_mode: crate::AliasMode) -> Self` and `pub(crate) fn eval_target(&mut self, order: &[ValueId], dispatch: ValueId, idx_value: ValueId, idx: u128) -> Option<u64>`
- Consumes: Task 1 helpers; `crate::sp_expr::{SpAliasCfg, SpExprMemo}` + `SpAliasCfg::reaching_store` / `ReachingSpStore`; `Function::{initial_sp_value, load_addr, int_const_u128, node_inputs, node_inputs_exact, value_type_opt, producer, node_kind, endianness}`.

- [ ] **Step 1: Declare the module**

In `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs`, alongside `pub mod table;`:

```rust
mod eval;
```

- [ ] **Step 2: Write the failing test**

Create `eval.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::{Evaluator, cone_order};

    // Build `Add(idx, 100):I64` and return (function, idx value, sum value).
    // Copy the graph-build pattern from constant_fold/tests.rs (it builds
    // IntBinaryOp graphs via IRBuilderExt over a test-utils FunctionBuilder).
    fn build_add_idx_100() -> (
        strider_ir::Function,
        strider_ir::node::ValueId,
        strider_ir::node::ValueId,
    ) {
        todo!("build with test-utils FunctionBuilder; see constant_fold/tests.rs")
    }

    #[test]
    fn evaluates_add_under_seed() {
        let (function, idx, sum) = build_add_idx_100();
        let order = cone_order(&function, sum);
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&order, sum, idx, 5), Some(105));
        assert_eq!(ev.eval_target(&order, sum, idx, 7), Some(107)); // re-seed, fresh map
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, idx, sum) = build_add_idx_100();
        let order = cone_order(&function, sum);
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // Seed the SUM (not idx) → idx stays symbolic → sum cannot collapse.
        assert_eq!(ev.eval_target(&order, sum, sum, 5), Some(5)); // sum seeded directly
        // A fresh eval where nothing relevant is seeded:
        let bogus = idx; // idx is in the cone but we seed a different value below
        let _ = bogus;
        assert_eq!(ev.eval_target(&order, sum, sum_unrelated_leaf(&function), 5), None);
    }

    fn sum_unrelated_leaf(_f: &strider_ir::Function) -> strider_ir::node::ValueId {
        todo!("a value id present in the cone but not the seed (e.g. the IntConst 100)")
    }
}
```

> NOTE TO IMPLEMENTER: the two `todo!`s are the ONLY ones in this plan. Before
> Step 3, open `crates/strider-opt/src/opt/constant_fold/tests.rs`, copy its
> graph-build pattern (`IRBuilderExt::build_int_const` / `build_int_binary_operation`
> over a `RegisterSet`/`make_*_fn` builder, then `.build()`), and replace both
> `todo!`s with real construction. Do not proceed with `todo!`s in place.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p strider-opt indirect_branch_resolve::eval`
Expected: FAIL — `Evaluator` / `cone_order` not found.

- [ ] **Step 4: Implement the evaluator**

Prepend to `eval.rs` (above the test module):

```rust
//! Read-only abstract evaluation of a jump-table dispatch cone.
//!
//! Computes the concrete branch target for a concrete index by evaluating the
//! dispatch value's cone in producers-before-consumers order. The abstract
//! value is a concrete number or stack-pointer-relative (`Abs`), because a
//! stack address can't be a pure number (the SP is symbolic). Three node
//! families do the work — ConstFold arithmetic, `LoadReadOnly` (constant-address
//! ROM read), and `LoadForward` (index folded into an `SpRel` offset, then the
//! existing `reaching_store` finds the store at that concrete offset). No graph
//! mutation, no clone, no pipeline. Any unresolved value, a non-`Const` dispatch
//! result, or a cycle yields `None`, so the caller rejects the candidate.

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::{
    eval_int_binary, eval_int_cmp, eval_int_unary, eval_lzcount, eval_popcount, eval_sign_extend,
};
use crate::sp_expr::{SpAliasCfg, SpExprMemo};

/// Abstract value: a concrete number, or `sp_base + offset`.
#[derive(Clone, Copy)]
enum Abs {
    Const(u128),
    SpRel { base: ValueId, offset: i64 },
}

impl Abs {
    fn as_const(self) -> Option<u128> {
        match self {
            Abs::Const(c) => Some(c),
            Abs::SpRel { .. } => None,
        }
    }
    fn same(self, other: Abs) -> bool {
        match (self, other) {
            (Abs::Const(a), Abs::Const(b)) => a == b,
            (
                Abs::SpRel { base: ba, offset: oa },
                Abs::SpRel { base: bb, offset: ob },
            ) => ba == bb && oa == ob,
            _ => false,
        }
    }
}

pub(crate) struct Evaluator<'a> {
    function: &'a strider_ir::Function,
    rom: Option<&'a dyn ReadOnlyMemory>,
    alias_mode: crate::AliasMode,
    endianness: Endianness,
    sp_base: Option<ValueId>,
    sp_memo: SpExprMemo,
    map: FxHashMap<ValueId, Abs>,
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
            sp_base: function.initial_sp_value(),
            sp_memo: SpExprMemo::default(),
            map: FxHashMap::default(),
        }
    }

    /// Evaluate `dispatch` over `order` (from [`cone_order`]) with `idx_value`
    /// bound to `idx`. Returns the target as `u64`, or `None` if anything fails
    /// to collapse to a concrete number.
    pub(crate) fn eval_target(
        &mut self,
        order: &[ValueId],
        dispatch: ValueId,
        idx_value: ValueId,
        idx: u128,
    ) -> Option<u64> {
        self.map.clear();
        self.map.insert(idx_value, Abs::Const(idx));
        for &val in order {
            if self.map.contains_key(&val) {
                continue;
            }
            if let Some(a) = self.eval_node(val) {
                self.map.insert(val, a);
            }
        }
        u64::try_from(self.map.get(&dispatch).copied()?.as_const()?).ok()
    }

    fn get(&self, value: ValueId) -> Option<Abs> {
        self.map.get(&value).copied()
    }

    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        if Some(value) == self.sp_base {
            return Some(Abs::SpRel { base: value, offset: 0 });
        }
        let f = self.function;
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        let out_ty = f.value_type_opt(value);
        let ins: SmallVec<[ValueId; 2]> = f
            .node_inputs(node)
            .into_iter()
            .filter(|&i| f.value_type_opt(i).is_some())
            .collect();
        match kind {
            NodeKind::IntConst(_) => Some(Abs::Const(f.int_const_u128(value)?)),
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => {
                self.eval_add(self.get(*ins.first()?)?, self.get(*ins.get(1)?)?, out_ty?)
            }
            NodeKind::IntBinaryOp(op) => {
                let l = self.get(*ins.first()?)?.as_const()?;
                let r = self.get(*ins.get(1)?)?.as_const()?;
                Some(Abs::Const(eval_int_binary(op, l, r, out_ty?)?))
            }
            NodeKind::IntUnaryOp(op) => {
                let v = self.get(*ins.first()?)?.as_const()?;
                Some(Abs::Const(eval_int_unary(op, v, out_ty?)?))
            }
            NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
                let v = self.get(*ins.first()?)?.as_const()?;
                Some(Abs::Const(out_ty?.get_unsigned_int(v)?))
            }
            NodeKind::Extend(ExtendOp::SignExtend) => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.get(ins[0])?.as_const()?;
                Some(Abs::Const(eval_sign_extend(v, in_ty, out_ty?)?))
            }
            NodeKind::Popcount => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.get(ins[0])?.as_const()?;
                Some(Abs::Const(eval_popcount(v, in_ty)?))
            }
            NodeKind::Lzcount => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let v = self.get(ins[0])?.as_const()?;
                Some(Abs::Const(eval_lzcount(v, in_ty)?))
            }
            NodeKind::IntCmpOp(op) => {
                let in_ty = f.value_type_opt(*ins.first()?)?;
                let l = self.get(ins[0])?.as_const()?;
                let r = self.get(*ins.get(1)?)?.as_const()?;
                Some(Abs::Const(u128::from(eval_int_cmp(op, l, r, in_ty).ok()?)))
            }
            NodeKind::Load(_) => self.eval_load(node, value),
            NodeKind::Phi => self.eval_phi(node),
            _ => None,
        }
    }

    /// `Add` in the abstract domain: `Const+Const`, or `SpRel ± Const`.
    fn eval_add(&self, l: Abs, r: Abs, ty: ValueType) -> Option<Abs> {
        match (l, r) {
            (Abs::Const(a), Abs::Const(b)) => Some(Abs::Const(eval_int_binary(
                strider_ir::IntBinaryOp::Add,
                a,
                b,
                ty,
            )?)),
            (Abs::SpRel { base, offset }, Abs::Const(c))
            | (Abs::Const(c), Abs::SpRel { base, offset }) => {
                // Signed interpretation so a negative frame offset (stored as
                // 0xFFFF..) subtracts correctly.
                let delta = i64::try_from(ty.get_signed_int(c)?).ok()?;
                Some(Abs::SpRel { base, offset: offset.wrapping_add(delta) })
            }
            (Abs::SpRel { .. }, Abs::SpRel { .. }) => None,
        }
    }

    /// `LoadReadOnly` (const address) then `LoadForward` (SP-relative).
    fn eval_load(&mut self, node: NodeId, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let load_ty = f.value_type_opt(value)?;
        match self.get(f.load_addr(node))? {
            Abs::Const(c) => {
                let rom = self.rom?;
                let a = u64::try_from(c).ok()?;
                let size = load_ty.byte_size();
                if size > 16 {
                    return None;
                }
                let mut bytes = [0u8; 16];
                rom.read(a, &mut bytes[..size]).ok()?;
                let loaded = self.endianness.read_uint(&bytes[..size]);
                Some(Abs::Const(load_ty.get_unsigned_int(loaded)?))
            }
            Abs::SpRel { base, offset } => {
                let [mem, _addr] = f.node_inputs_exact::<2>(node).ok()?;
                let load_size = load_ty.byte_size() as i64;
                let reaching = {
                    let mut cfg = SpAliasCfg::call_blocking(&mut self.sp_memo, self.alias_mode);
                    cfg.reaching_store(f, mem, base, offset, load_size)
                }?;
                // Exact anchor: the store must sit at the probed offset.
                if reaching.store_offset != offset {
                    return None;
                }
                // Jump targets are constants on the converged graph.
                let data_ty = f.value_type_opt(reaching.data)?;
                let raw = f.int_const_u128(reaching.data)?;
                Some(Abs::Const(self.reshape(raw, data_ty, load_ty)?))
            }
        }
    }

    /// Reshape a stored constant to a narrower load width (mirrors
    /// `LoadForward::narrow`). Equal widths pass through; load wider than store
    /// → `None`.
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
                    let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u32) * 8;
                    v >> shift_bits
                }
            };
            return load_ty.get_unsigned_int(shifted);
        }
        None
    }

    /// All-arms-agree: every value arm must resolve to the same `Abs`.
    fn eval_phi(&mut self, node: NodeId) -> Option<Abs> {
        let arms: SmallVec<[ValueId; 4]> = self
            .function
            .node_inputs(node)
            .into_iter()
            .filter(|&i| self.function.value_type_opt(i).is_some())
            .collect();
        let mut agreed: Option<Abs> = None;
        for arm in arms {
            let v = self.get(arm)?;
            match agreed {
                None => agreed = Some(v),
                Some(prev) if prev.same(v) => {}
                Some(_) => return None,
            }
        }
        agreed
    }
}

/// The dispatch cone in producers-before-consumers order: backward reachability
/// from `root` over value edges only (the memory token is not followed — store
/// data is resolved at eval time via `reaching_store`). Iterative postorder, so
/// a deep cone costs O(1) host stack; cycles terminate via `seen` (a back-edge
/// input is absent at eval time → `None`).
pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId> {
    let mut order: Vec<ValueId> = Vec::new();
    let mut seen: FxHashSet<ValueId> = FxHashSet::default();
    let mut stack: Vec<(ValueId, bool)> = vec![(root, false)];
    while let Some((v, processed)) = stack.pop() {
        if processed {
            order.push(v);
            continue;
        }
        if !seen.insert(v) {
            continue;
        }
        stack.push((v, true));
        for input in function.node_inputs(function.producer(v)) {
            if function.value_type_opt(input).is_some() && !seen.contains(&input) {
                stack.push((input, false));
            }
        }
    }
    order
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p strider-opt indirect_branch_resolve::eval`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs
git commit -m "feat(opt): abstract evaluator (Const|SpRel) for jump-table cones"
```

---

### Task 3: Extract shared const-eval utility; route the evaluator + LoadReadOnly through it

De-duplicate the ROM decode and per-op fold logic into one shared module that both the jump-table evaluator and `LoadReadOnly` call, so the evaluator stops re-implementing optimization logic. `Const | SpRel` and the SpRel-specific handling stay in the evaluator (the passes have no stack-relative domain). ConstFold is intentionally left sharing at the `eval_int_*` primitive level. Behavior-preserving: the existing `eval`, `load_readonly`, and `constant_fold` suites are the gate.

**Files:**
- Create: `crates/strider-opt/src/const_eval.rs`
- Modify: `crates/strider-opt/src/lib.rs` (add `mod const_eval;` near the other private module declarations)
- Modify: `crates/strider-opt/src/opt/load_readonly/mod.rs` (`try_fold_const_load_at` folds via `eval_node_const`)
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs` (delegate const arms to `eval_node_const`; `Const`-address `Load` via `read_rom_const`; drop the now-shared `eval_int_*` imports)
- Test: inline `#[cfg(test)]` in `const_eval.rs`; the existing `eval` / `load_readonly` / `constant_fold` suites are the behavior gate.

**Interfaces:**
- Produces:
  - `pub(crate) fn read_rom_const(rom: &dyn ReadOnlyMemory, addr: u64, ty: ValueType, endianness: Endianness) -> Option<u128>`
  - `pub(crate) fn eval_node_const(function: &Function, value: ValueId, resolve: &dyn Fn(ValueId) -> Option<u128>, rom: Option<&dyn ReadOnlyMemory>, endianness: Endianness) -> Option<u128>`
- The `Evaluator` public API (`new` / `eval_target` / `cone_order`) is UNCHANGED — Task 4 is unaffected.
- Consumes: Task 1 `eval_int_*` helpers.

- [ ] **Step 1: Write the failing test**

Create `crates/strider-opt/src/const_eval.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::eval_node_const;
    use strider_ir::IRBuilderExt;
    use strider_ir::node::ValueType;

    // Build `Add(IntConst(5), IntConst(100)):I64`; eval_node_const with an
    // int-const resolver folds it to 105. Copy the builder pattern from
    // constant_fold/tests.rs.
    #[test]
    fn folds_add_of_two_constants() {
        let (function, sum) = build_add_5_100();
        let resolve = |v| function.int_const_u128(v);
        let got = eval_node_const(&function, sum, &resolve, None, function.endianness());
        assert_eq!(got, Some(105));
    }

    fn build_add_5_100() -> (strider_ir::Function, strider_ir::node::ValueId) {
        // Build with the test-utils FunctionBuilder; see constant_fold/tests.rs.
        // Return (built function, the Add output value).
        todo!("build Add(IntConst(5), IntConst(100)):I64 via test-utils")
    }
}
```

> NOTE TO IMPLEMENTER: this `todo!` is the only one — fill it from
> `constant_fold/tests.rs`'s graph-build pattern before Step 3, and remove it.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-opt const_eval`
Expected: FAIL — `eval_node_const` not found.

- [ ] **Step 3: Implement `const_eval.rs`**

Prepend (above the test module):

```rust
//! Shared "what constant does this node produce from constant inputs" SSoT,
//! used by the jump-table abstract evaluator and by `LoadReadOnly` so the ROM
//! decode and the per-op fold dispatch live in exactly one place. ConstFold
//! shares the leaf arithmetic (`eval_int_*`) directly and does not route
//! through here.

use smallvec::SmallVec;
use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::ReadOnlyMemory;
use strider_ir::node::{ExtendOp, NodeKind, ValueId, ValueType};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::{
    eval_int_binary, eval_int_cmp, eval_int_unary, eval_lzcount, eval_popcount, eval_sign_extend,
};

/// Decode `ty` bytes at `addr` from a read-only image into an integer masked to
/// `ty`. The single ROM-decode site. `None` for widths > 16 bytes, an unmapped
/// read, or a non-integer `ty`.
pub(crate) fn read_rom_const(
    rom: &dyn ReadOnlyMemory,
    addr: u64,
    ty: ValueType,
    endianness: Endianness,
) -> Option<u128> {
    let size = ty.byte_size();
    if size > 16 {
        return None;
    }
    let mut bytes = [0u8; 16];
    rom.read(addr, &mut bytes[..size]).ok()?;
    let loaded = endianness.read_uint(&bytes[..size]);
    ty.get_unsigned_int(loaded)
}

/// The constant value of `value`, given `resolve` for its inputs' constants.
/// Covers `IntConst`, integer arithmetic/casts/compares (via the shared
/// `eval_int_*` helpers), and a constant-address `Load(RAM)` (via
/// [`read_rom_const`]). `None` for anything not foldable to one integer
/// constant from `resolve`d inputs. Does NOT handle `Phi` or any
/// stack-relative address — those stay in the jump-table evaluator.
pub(crate) fn eval_node_const(
    function: &Function,
    value: ValueId,
    resolve: &dyn Fn(ValueId) -> Option<u128>,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: Endianness,
) -> Option<u128> {
    let node = function.producer(value);
    let kind = *function.node_kind(node);
    let out_ty = function.value_type_opt(value);
    let ins: SmallVec<[ValueId; 2]> = function
        .node_inputs(node)
        .into_iter()
        .filter(|&i| function.value_type_opt(i).is_some())
        .collect();
    match kind {
        NodeKind::IntConst(_) => function.int_const_u128(value),
        NodeKind::IntBinaryOp(op) => {
            eval_int_binary(op, resolve(*ins.first()?)?, resolve(*ins.get(1)?)?, out_ty?)
        }
        NodeKind::IntUnaryOp(op) => eval_int_unary(op, resolve(*ins.first()?)?, out_ty?),
        NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
            out_ty?.get_unsigned_int(resolve(*ins.first()?)?)
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            let in_ty = function.value_type_opt(*ins.first()?)?;
            eval_sign_extend(resolve(ins[0])?, in_ty, out_ty?)
        }
        NodeKind::Popcount => {
            let in_ty = function.value_type_opt(*ins.first()?)?;
            eval_popcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::Lzcount => {
            let in_ty = function.value_type_opt(*ins.first()?)?;
            eval_lzcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::IntCmpOp(op) => {
            let in_ty = function.value_type_opt(*ins.first()?)?;
            Some(u128::from(
                eval_int_cmp(op, resolve(ins[0])?, resolve(*ins.get(1)?)?, in_ty).ok()?,
            ))
        }
        NodeKind::Load(_) => {
            let rom = rom?;
            let addr = u64::try_from(resolve(function.load_addr(node))?).ok()?;
            read_rom_const(rom, addr, out_ty?, endianness)
        }
        _ => None,
    }
}
```

Add `mod const_eval;` to `crates/strider-opt/src/lib.rs` (with the other private `mod` declarations, e.g. near `mod opt;` / `mod post_opt;`).

- [ ] **Step 4: Run the new test (GREEN)**

Run: `cargo test -p strider-opt const_eval`
Expected: PASS.

- [ ] **Step 5: Route `LoadReadOnly` through the utility**

In `crates/strider-opt/src/opt/load_readonly/mod.rs`, refactor `try_fold_const_load_at` so the address-constant check + ROM decode go through `eval_node_const`. Replace the body from the `let addr_value = ...` line through the `let masked = ...` block with:

```rust
    // SSoT: fold this Load via the shared const-eval utility (constant address
    // → ROM decode), so the decode logic is not duplicated in the jump-table
    // evaluator.
    let endianness = ctx.function().endianness();
    let [data_value] = ctx.node_outputs_exact::<1>(node_id)?;
    let resolve = |v| ctx.function().int_const_u128(v);
    let Some(masked) =
        crate::const_eval::eval_node_const(ctx.function(), data_value, &resolve, Some(rom), endianness)
    else {
        return Ok(false);
    };
    let ty = ctx
        .value_type_opt(data_value)
        .expect("Load output is a value");
```

Keep the existing asm-fingerprint + `build_int_const` + `replace_value` tail unchanged (it still needs `ty`, `data_value`, and the address producer for the fingerprint — derive `addr_value` once via `ctx.load_addr(node_id)` for the fingerprint step if the tail references it). Adjust only what the borrow checker / unused-var warnings require; do not change observable behavior.

- [ ] **Step 6: Run the LoadReadOnly suite**

Run: `cargo test -p strider-opt load_readonly`
Expected: PASS (behavior unchanged).

- [ ] **Step 7: Slim `eval.rs` to delegate**

In `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs`:

- Remove the `use crate::opt::constant_fold::eval_int::{...}` import (no longer used directly).
- Replace the arithmetic arms of `eval_node` (every arm except the `sp_base` short-circuit, `IntBinaryOp(Add)`, `Load(_)`, and `Phi`) with a single delegation:

```rust
            _ => {
                let resolve = |v| self.get(v).and_then(Abs::as_const);
                crate::const_eval::eval_node_const(
                    self.function,
                    value,
                    &resolve,
                    self.rom,
                    self.endianness,
                )
                .map(Abs::Const)
            }
```

  Keep the `sp_base` short-circuit, the `IntBinaryOp(strider_ir::IntBinaryOp::Add) => self.eval_add(...)` arm, `Load(_) => self.eval_load(...)`, and `Phi => self.eval_phi(...)` arms exactly as they are (these are the `Abs`/SpRel-specific cases the shared utility deliberately does not cover).
- In `eval_load`, replace the `Abs::Const` arm's inline ROM decode with:

```rust
            Abs::Const(c) => {
                let rom = self.rom?;
                let addr = u64::try_from(c).ok()?;
                crate::const_eval::read_rom_const(rom, addr, load_ty, self.endianness).map(Abs::Const)
            }
```

  Leave the `Abs::SpRel` arm (reaching_store + exact-anchor + `int_const_u128` + `reshape`) unchanged.

- [ ] **Step 8: Run eval + clippy**

Run: `cargo test -p strider-opt indirect_branch_resolve::eval` and `cargo clippy -p strider-opt`
Expected: the 2 eval tests PASS; clippy clean (dead-code on `Abs`/`Evaluator`/`cone_order` is still expected — Task 4 wires them).

- [ ] **Step 9: Run the three behavior gates together**

Run: `cargo test -p strider-opt constant_fold load_readonly indirect_branch_resolve::eval const_eval`
Expected: all PASS (the refactor preserved behavior).

- [ ] **Step 10: Commit**

```bash
git add crates/strider-opt/src/const_eval.rs crates/strider-opt/src/lib.rs crates/strider-opt/src/opt/load_readonly/mod.rs crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs
git commit -m "refactor(opt): shared const-eval utility for evaluator + LoadReadOnly"
```

---

### Task 4: Rewire `table.rs` onto the evaluator; delete clone+pipeline

The existing `table_tests.rs` suite is the characterization gate.

**Files:**
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs`
- Test: `table_tests.rs` (unchanged — the regression gate).

**Interfaces:**
- Consumes: Task 2 `Evaluator::{new, eval_target}`, `cone_order`.
- Removes: `fold_dispatch_to_const`.

- [ ] **Step 1: Run the integration gate BEFORE the change (baseline)**

Run: `cargo test -p strider-opt indirect_branch_resolve::table`
Expected: PASS (record the passing count — the baseline to preserve).

- [ ] **Step 2: Rewrite the body of `classify_table_dispatch`**

Replace everything from `// Clone + compact ONCE up front:` through the closing `None` of the candidate loop with:

```rust
    // Evaluate the dispatch cone under each concrete index — no clone, no
    // pipeline. The cone + its topological order are index-independent, so
    // build them once and reuse across candidates and indices. The candidate
    // whose whole range collapses to constants IS the index; the constants are
    // the targets. A wrong candidate leaves the cone dependent on a non-seeded
    // runtime value and fails to collapse → rejected.
    let order = super::eval::cone_order(ctx, anchor_value);
    let mut ev = super::eval::Evaluator::new(ctx, rom, alias_mode);
    for (idx_value, lo, hi) in candidates {
        if let Some(targets) =
            enumerate_targets(lo, hi, |v| ev.eval_target(&order, anchor_value, idx_value, v))
        {
            return Some(ResolvedTargets::Multiple(targets));
        }
    }
    None
}
```

- [ ] **Step 3: Delete `fold_dispatch_to_const` and now-unused imports**

Remove the entire `fn fold_dispatch_to_const(...) { ... }`. Then `cargo build -p strider-opt` and remove any imports it flags as unused (the function held `crate::EditFunction` / `crate::OptCtx` / `crate::default_pipeline` / `strider_ir::IRBuilderExt` locally; the top-level `use crate::ReadOnlyMemory;` and `use strider_ir::node::{IntBinaryOp, NodeId, NodeKind, ValueId};` and `use strider_ir::IRViewer;` all stay — still used by the surviving code).

- [ ] **Step 4: Update the module doc comment**

In the `//!` header: change bullet "2. **Pin and fold.**" — replace "clone the function, substitute the candidate with `IntConst(i)` … and run the canonical `crate::default_pipeline` on the clone" with "evaluate the dispatch cone under `index = i` via the read-only `eval::Evaluator` (ConstFold arithmetic + `LoadReadOnly` ROM reads + `LoadForward` via `reaching_store`)". In `## Soundness`, change "The clone is disposable, so a destructive pipeline run leaves the analysed function untouched" to "The evaluator is read-only, so the analysed function is never mutated." Leave the soundness gates otherwise intact.

- [ ] **Step 5: Run the integration gate AFTER the change**

Run: `cargo test -p strider-opt indirect_branch_resolve`
Expected: PASS with the SAME count as Step 1. If any arch table regresses, STOP and use superpowers:systematic-debugging — a regression means the evaluator misses a shape the pipeline folded (suspect: a node kind hitting `eval_node`'s `_ => None`, the `SpRel` `Add` propagation, the `reaching_store` exact-offset check, or a value-`Phi` arm).

- [ ] **Step 6: Run the full strider-opt suite**

Run: `cargo test -p strider-opt`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs
git commit -m "refactor(opt): resolve jump tables via abstract eval, drop clone+pipeline"
```

---

### Task 5: Fix evaluator SP detection to reuse `decompose_sp` (aligned `& mask` stacks)

The evaluator currently recognizes the stack-pointer terminal only as `value == initial_sp_value()`. That is wrong for a realigned frame, whose base is `(initial_sp & mask)` (e.g. `and rsp, -16`): the `&`-output is not `initial_sp`, so the load's address never becomes `SpRel`, and even if it did the wrong `base` would be handed to `reaching_store` (the stores decompose to the `&`-output base, so `base` equality would fail → no match). Replace the ad-hoc detection with `SpDecomposer::decompose` — the same decomposer `stack_offsets` / `reaching_store` use, which already anchors at an alignment-masked `(sp & mask)` and returns the correct terminal `base`.

**Files:**
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs`
- Modify: `crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs`
- Test: a new aligned-stack table test (RED on the current detection, GREEN after the fix) + the full `indirect_branch_resolve` suite stays green.

**Interfaces:** no public API change — `Evaluator::{new, eval_target}` / `cone_order` signatures are unchanged. Internally the `sp_base` field is removed.

- [ ] **Step 1: Write the failing test**

In `table_tests.rs`, add a test that builds the same SP-rooted two-target stack array as `build_two_target_array`, except the frame base is alignment-masked: replace the bare `sp_val` used for the stores and the load address with `aligned = And(sp_val, IntConst(0xFFFF_FFFF_FFFF_FFF0, I64))`, and use `aligned` everywhere `build_two_target_array` uses `sp_val`. (Either parameterize `build_two_target_array` with an optional align mask, or add a sibling `build_two_target_array_aligned`; do not duplicate the whole body verbatim — factor the shared part.) Then:

```rust
#[test]
fn classify_table_dispatch_aligned_stack_resolves() {
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_two_target_array_aligned(targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-opt classify_table_dispatch_aligned_stack_resolves`
Expected: FAIL — returns `None` (the `(sp & mask)` base is not `initial_sp_value()`, so the load address never resolves to `SpRel` and the table is left unresolved). This depends on Task 4 having rewired `classify_table_dispatch` onto the evaluator; if it still returns `Multiple` here, Task 4 was not applied — stop and check.

- [ ] **Step 3: Apply the fix in `eval.rs`**

(a) Add `SpDecomposer` and `SpExpr` to the `sp_expr` import:
```rust
use crate::sp_expr::{SpAliasCfg, SpDecomposer, SpExpr, SpExprMemo};
```

(b) Remove the `sp_base: Option<ValueId>,` field from `struct Evaluator` and the `sp_base: function.initial_sp_value(),` line from `Evaluator::new`.

(c) In `eval_node`, replace the opening sp-leaf short-circuit:
```rust
    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        if Some(value) == self.sp_base {
            return Some(Abs::SpRel { base: value, offset: 0 });
        }
        let f = self.function;
```
with a decompose-first block:
```rust
    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        let f = self.function;
        // An sp-rooted constant expression — InitialVar(sp), an alignment-masked
        // `(sp & mask)`, or either plus a constant `Add` chain — decomposes to
        // its SP terminal + offset via the same decomposer the stores /
        // `reaching_store` use, so the aligned base is recognized and matches
        // the stores' base. Memoized in `sp_memo`, so the load's index-
        // independent sp-spine is computed once and reused across indices.
        if let Some(SpExpr { base, offset }) =
            SpDecomposer::new(f, &mut self.sp_memo).decompose(value)
        {
            return Some(Abs::SpRel { base, offset });
        }
```
Leave the rest of `eval_node` (the `IntConst` / `Add` / `Load` / `Phi` / `_` arms), `eval_add`, `eval_load`, `reshape`, `eval_phi`, and `cone_order` unchanged. `eval_add` still handles the top-level `Add(sp_spine, idx*stride)` combination (`decompose` returns `None` for that index-dependent Add, so it reaches the `Add` arm where one operand is the decomposed `SpRel` and the other the evaluated `Const`).

- [ ] **Step 4: Run the aligned test (GREEN)**

Run: `cargo test -p strider-opt classify_table_dispatch_aligned_stack_resolves`
Expected: PASS.

- [ ] **Step 5: Run the full indirect-branch suite (no regression)**

Run: `cargo test -p strider-opt indirect_branch_resolve` and `cargo clippy -p strider-opt`
Expected: all PASS (the non-aligned stack tests still resolve — `decompose` returns `SpExpr{base: InitialVar(sp), offset}` for the bare-sp case too); clippy clean (no more `sp_base` field; `initial_sp_value` may now be unused elsewhere — if clippy flags it as dead, that is a pre-existing public API used by other passes, so confirm it is still referenced before touching it; do NOT delete `Function::initial_sp_value`).

- [ ] **Step 6: Commit**

```bash
git add crates/strider-opt/src/post_opt/indirect_branch_resolve/eval.rs crates/strider-opt/src/post_opt/indirect_branch_resolve/table_tests.rs
git commit -m "fix(opt): resolve aligned (sp & mask) stack jump tables via decompose_sp"
```

---

### Task 6: Remove `Clone` from `Function` and the generic `Graph`

With the only whole-`Function` clones gone, delete the capability. The compiler is the gate.

**Files:**
- Modify: `crates/strider-ir/src/function/data.rs:96`
- Modify: `crates/strider-graph/src/graph.rs:57-66`

- [ ] **Step 1: Remove `Clone` from `Function`'s derive**

`crates/strider-ir/src/function/data.rs:96`:
```rust
#[derive(Default)]
pub struct Function {
```
(was `#[derive(Default, Clone)]`).

- [ ] **Step 2: Delete the generic `Graph` Clone impl**

In `crates/strider-graph/src/graph.rs`, delete the entire `impl<N: Clone, V: Clone, C: NodeCacheable<N, V>> Clone for Graph<N, V, C> { ... }` block (and its preceding `// Manual Clone ...` comment).

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: clean. Any surfaced `Function`/`Graph` `.clone()` is a missed Task-3 site — remove it.

- [ ] **Step 4: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS (no NEW failures vs the known baseline).

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
- [ ] Push the branch and STOP — prompt the user before merging.

---

## Self-Review Notes

- **Spec coverage:** `Const|SpRel` value + evaluator (Task 2); ConstFold/LoadReadOnly reuse (Tasks 1–2); LoadForward via `reaching_store` with index-folded offset + exact-anchor check (Task 2 `eval_load`); value-edge-only cone + flat RPO, order built once (Task 2 `cone_order` + Task 3 driver); fail-closed + cycle handling (Task 2); phi all-arms-agree (Task 2 `eval_phi`); clone deletion (Task 4); integration gate (Task 3). All covered.
- **Test-scope deviation (carried from spec, user-approved):** unit tests cover the arithmetic + fail-closed spine; load/forward/phi/reshape rely on the 7-arch `table_tests.rs` characterization suite. Two `todo!`s in Task 2's test helpers must be filled from `constant_fold/tests.rs` before proceeding.
- **No new SP helper:** `reaching_store` / `ReachingSpStore` already exist; the earlier `forwardable_store_data` task was dropped (it used structural `classify_addr`, which can't see the index-folded offset).
- **Type consistency:** `Evaluator::{new, eval_target}` + `cone_order` signatures identical in Task 2 and Task 3. `Abs`/`reshape`/`eval_add`/`eval_load`/`eval_phi` are private to `eval.rs`.
- **Recursion removed:** flat RPO; `cone_order` is iterative (no host-stack blowup); cycles fail-closed via absent map entries — no cycle guard needed.
