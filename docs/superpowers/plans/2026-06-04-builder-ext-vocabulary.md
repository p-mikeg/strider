# IRBuilder / IRBuilderExt Construction Vocabulary — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Rename the merged `Builder` trait to `IRBuilder` with `create_node_attributed` as its primary primitive, add a blanket `IRBuilderExt` carrying the lifter's pure `build_*` construction vocabulary so every builder (`Function`, `FunctionBuilder`, `EditFunction`) shares it, and fold in the agreed `EditFunction` slimming (drop `with_attribution`, cull dead forwarders). `make_int_const` → `build_int_const`.

**Architecture:** Minimal core `IRBuilder` (`create_node_attributed` + `function()` + provided `create_node`) + blanket `impl<B: IRBuilder> IRBuilderExt for B {}` holding ~25 pure constructors expressed via `self.create_node[_attributed]` + `self.function()`. Pure `build_*` bodies move off `FunctionBuilder` into `IRBuilderExt` defaults; lift-stateful ones + `build_int_const_wide` stay inherent.

**Tech Stack:** Rust workspace; `strider-ir`, `strider-lift`, `strider-opt`, `strider-pattern`, `strider-orchestrator`, `strider-py`. Gate: `cargo test --workspace` + `cargo clippy --workspace --all-targets` + `uv run pytest`.

**Working tree:** worktree `.worktrees/builder-ext`, branch `refactor/builder-ext-vocabulary`. Push every commit: `git push origin refactor/builder-ext-vocabulary`.

**Naming note:** the rename is whole-word `Builder` → `IRBuilder`. NEVER touch `FunctionBuilder` (it contains "Builder" but is a different type). Use `\bBuilder\b` word boundaries in any sed.

---

## Starting point (on this branch, inherited from develop)

`crates/strider-ir/src/builder/build_trait.rs` defines:
```rust
pub trait Builder {
    fn create_node<I,O>(&mut self, kind, inputs: I, outputs: O) -> NodeId where ...;
    fn function(&self) -> &Function;
}
impl Builder for Function { /* graph_mut().create_node */ }
impl Builder for FunctionBuilder { /* inherent create_node = lift_addr stamp */ }
```
`EditFunction` (in `crates/strider-ir/src/edit/mod.rs`) has: an `attribution: Option<NodeId>` field, `with_attribution`, `track_and_create`, inherent `create_node` + `create_node_attributed` + `make_int_const`, and `impl Builder for EditFunction { create_node -> track_and_create }`. `template::instantiate<B: Builder>` (in `crates/strider-pattern/src/template/mod.rs`) calls `builder.create_node(...)`. `rewrite_rule_impl` (in `crates/strider-opt/src/rewrite/mod.rs`) wraps it in `ctx.with_attribution(node, |b| instantiate(...))`.

---

## Task 1: Reshape the core trait to `IRBuilder` (create_node_attributed primary); drop ambient attribution

**Files:**
- Modify: `crates/strider-ir/src/builder/build_trait.rs`, `crates/strider-ir/src/lib.rs`
- Modify: `crates/strider-ir/src/edit/mod.rs` (drop `attribution`/`with_attribution`/`track_and_create`; impl the new primary)
- Modify: `crates/strider-pattern/src/template/mod.rs` (`instantiate` threads contributor)
- Modify: `crates/strider-opt/src/rewrite/mod.rs` (drop the `with_attribution` wrapper)
- Modify: all `Builder` references in `strider-opt`/`strider-orchestrator`/`strider-py` (rename)

Behavior-preserving. Gate = the 8 `track_*` tests + workspace suite.

- [ ] **Step 1: Reshape the trait.** In `build_trait.rs`, change the trait to:
```rust
pub trait IRBuilder {
    /// The one creation primitive: create (or dedup to) a node, applying this
    /// builder's attribution/bookkeeping, unioning each contributor's
    /// asm-fingerprint into the result.
    fn create_node_attributed<I, O>(
        &mut self, kind: NodeKind, inputs: I, outputs: O, contributors: &[NodeId],
    ) -> NodeId
    where I: IntoIterator<Item = ValueId>, O: IntoIterator<Item = ValueKind>;

    /// Read access to the function under construction/edit.
    fn function(&self) -> &Function;

    /// Unattributed creation — provided.
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where I: IntoIterator<Item = ValueId>, O: IntoIterator<Item = ValueKind> {
        self.create_node_attributed(kind, inputs, outputs, &[])
    }
}
```
Update the two impls in this file:
```rust
impl IRBuilder for Function {
    fn create_node_attributed<I, O>(&mut self, kind, inputs, outputs, contributors) -> NodeId
    where ... { self.create_node_attributed(kind, inputs, outputs, contributors) } // Function's existing inherent unioning method
    fn function(&self) -> &Function { self }
}
impl IRBuilder for FunctionBuilder {
    fn create_node_attributed<I, O>(&mut self, kind, inputs, outputs, contributors) -> NodeId
    where ... {
        // create with ambient lift_addr stamp (existing inherent create_node),
        // then union the explicit contributors.
        let node = FunctionBuilder::create_node(self, kind, inputs, outputs);
        for &c in contributors { self.function_mut().extend_asm_fingerprint_from(node, c); }
        node
    }
    fn function(&self) -> &Function { FunctionBuilder::function(self) }
}
```
Confirm `Function::create_node_attributed` exists (it does — `EditFunction::create_node_attributed` delegates to it today) and that `FunctionBuilder::function_mut()` + `extend_asm_fingerprint_from` are reachable. Update the file's tests to the new method names.

- [ ] **Step 2: Rename `Builder` → `IRBuilder` — SCOPED to the 6 strider-ir-trait files only.**

⚠️ **Do NOT run a workspace-wide sed.** Two unrelated `Builder` types exist: the strider-ir trait (rename this) and `strider_lift::cfg::Builder` (the CFG builder — used as `cfg::Builder::for_arch` across strider-lift/opt/orchestrator/py — must stay). Also `strider_pattern::BuilderLike` exists (a `\bBuilder\b` boundary won't match it, but be aware). The strider-ir trait is referenced in EXACTLY these 6 files, none of which use `cfg::Builder`:
```bash
cd /mnt/c/Users/mikeg/Documents/strider/.worktrees/builder-ext
sed -i -E 's/\bBuilder\b/IRBuilder/g' \
  crates/strider-ir/src/builder/build_trait.rs \
  crates/strider-ir/src/builder/mod.rs \
  crates/strider-ir/src/lib.rs \
  crates/strider-ir/src/edit/mod.rs \
  crates/strider-ir/tests/builder_trait.rs \
  crates/strider-pattern/src/template/mod.rs
```
(`\bBuilder\b` leaves `FunctionBuilder`/`BuilderLike` untouched.) Then **sweep for any strider-ir-trait reference the 6-file list missed**, while leaving `cfg::Builder` alone:
```bash
grep -rn '\bBuilder\b' crates --include=*.rs | grep -v FunctionBuilder | grep -vE 'cfg::Builder|cfg/builder|cfg/mod.rs|cfg/options|cfg/query|cfg/types|BuilderLike'
```
For any hit that is the strider-ir trait (e.g. a stray `use strider_ir::Builder` or `B: Builder` bound — check `strider-opt/src/rewrite/mod.rs`'s 2 occurrences, which may be doc references to the trait), rename it by hand to `IRBuilder`. Do NOT touch any `cfg::Builder` / `cfg::builder::Builder` / `pub use builder::Builder` inside `strider-lift/src/cfg/`. Verify clean: `grep -rn 'FunctionIRBuilder' crates` is empty, and `cargo build -p strider-lift` still compiles (proves `cfg::Builder` intact).

- [ ] **Step 3: Drop ambient attribution from `EditFunction`** (`edit/mod.rs`):
  - Delete the `attribution: Option<NodeId>` field (and its `None` init in both constructors), `with_attribution`, and `track_and_create`.
  - Make `create_node_attributed` the bookkeeping choke-point:
```rust
pub fn create_node_attributed<I, O>(&mut self, kind, inputs, outputs, contributors: &[NodeId]) -> NodeId
where ... {
    let node = self.function.create_node_attributed(kind, inputs, outputs, contributors);
    self.track_created(node);
    node
}
```
  - Keep the inherent `create_node` delegating: `self.create_node_attributed(kind, inputs, output_kinds, &[])`.
  - Replace `impl IRBuilder for EditFunction`'s body to provide `create_node_attributed` (same as the inherent — call the inherent, or share a private helper) + `function()`.

- [ ] **Step 4: Thread the contributor through `instantiate`** (`template/mod.rs`): change the creation site from `builder.create_node(kind, inputs, outputs)` to `builder.create_node_attributed(kind, inputs, outputs, &[lhs_root])`. (`lhs_root` is already a param.) Leave the rest unchanged.

- [ ] **Step 5: Drop the `with_attribution` wrapper** (`rewrite/mod.rs`): replace
```rust
let new_value = match ctx.with_attribution(node, |b| instantiate(&rhs, b, &bindings, node, root_ty)) {
```
with
```rust
let new_value = match instantiate(&rhs, ctx, &bindings, node, root_ty) {
```
(`instantiate` now attributes each node to `node`/`lhs_root` itself.) Keep the `Ok/Err(skip)/Err` arms and the subsequent `replace_value`.

- [ ] **Step 6: Build + gate.**
```bash
cargo test -p strider-ir edit:: 2>&1 | tail
cargo test -p strider-opt rewrite:: 2>&1 | tail        # 8 track_* tests
cargo test -p strider-pattern template 2>&1 | tail
cargo test --workspace 2>&1 | tail -12
cargo clippy --workspace --all-targets 2>&1 | tail -3
```
All pass, 0 warnings. The 8 `track_*` tests confirm the attribution-via-`&[lhs_root]` preserves `live_nodes == compute_full(entry)` and the fingerprint stamping.

- [ ] **Step 7: Commit.**
```bash
git add -A
git commit -m "refactor(strider-ir): IRBuilder trait with create_node_attributed primary; explicit contributor threading"
git push origin refactor/builder-ext-vocabulary
```

---

## Task 2: Add `IRBuilderExt` blanket trait; migrate the pure construction vocabulary

**Files:**
- Create: `crates/strider-ir/src/builder/builder_ext.rs` (the `IRBuilderExt` trait + blanket impl)
- Modify: `crates/strider-ir/src/builder/mod.rs` (`mod builder_ext; pub use builder_ext::IRBuilderExt;`), `crates/strider-ir/src/lib.rs` (`pub use builder::IRBuilderExt;`)
- Modify: `crates/strider-ir/src/builder/nodes.rs` (delete the migrated inherent `build_*`)
- Modify: `crates/strider-ir/src/edit/mod.rs` (delete `make_int_const`)
- Modify: lifter (`strider-lift`) + `strider-opt` files calling `build_*`/`make_int_const` (add `use strider_ir::IRBuilderExt;`)

This is a mechanical move: each pure `build_*` body moves verbatim from `FunctionBuilder` (`nodes.rs`) into an `IRBuilderExt` default, with `self.create_node(...)` already being the call it makes. Gate = lifter's existing tests (they exercise every constructor end-to-end).

- [ ] **Step 1: Inventory + confirm purity.** For each candidate constructor in `crates/strider-ir/src/builder/nodes.rs`, read its body and confirm it uses ONLY `self.create_node(...)` (or `self.build_single_output_pure`) + `self.function()` reads — NO `var_table`/`regions`/`cur_region`/`entry_memory`/`largest_container`/`read_vn`/`write_vn`. The expected PURE set:
`build_boolean_const`, `build_int_const`, `build_single_output_pure`, `build_int_binary_operation`, `build_int_unary_operation`, `build_sub_as_add_neg`, `build_popcount`, `build_lzcount`, `build_int_cmp_operation`, `build_float_const`, `build_float_binary_op`, `build_float_unary_op`, `build_float_cmp_op`, `build_int_to_float`, `build_float_to_int`, `build_float_to_float`, `build_int_bits_to_float`, `build_float_bits_to_int`, `build_segment_op`, `build_cpool_ref`, `build_new`, `build_store`, `build_load`.
The EXCLUDED set (stay inherent — lift-stateful or graph-mutating beyond create_node): `build_entry`, `build_return`, `build_function_return`, `build_if`, `build_branch`, `build_indirect_branch`, `build_vn_phi`, `build_call`, `build_call_kind`, `build_call_other`, `build_masked_insert`, **`build_int_const_wide`** (interns wide consts). If any "pure" candidate actually reads lift state, leave it inherent and note it in your report.

- [ ] **Step 2: Write the failing test** — create `crates/strider-ir/tests/builder_ext.rs`:
```rust
//! IRBuilderExt: the shared construction vocabulary works through every builder.
use strider_ir::{IRBuilder, IRBuilderExt, ValueType, IntBinaryOp};
use strider_ir_test_utils::{make_empty_fn, empty_builder};

#[test]
fn build_int_const_masks_and_dedups_via_ext() {
    let mut fx = make_empty_fn(|b| b.build_int_const(0u64, ValueType::I64)).unwrap();
    // 0x1FF masked to I8 == 0xFF; building it twice dedups to one node.
    let a = fx.build_int_const(0x1FFu64, ValueType::I8).unwrap();
    let b = fx.build_int_const(0xFFu64, ValueType::I8).unwrap();
    assert_eq!(a, b, "masked-equal consts must dedup to the same value");
}

#[test]
fn build_int_binary_through_function_builder() {
    let mut b = empty_builder().unwrap();   // a FunctionBuilder
    let c1 = b.build_int_const(3u64, ValueType::I64).unwrap();
    let c2 = b.build_int_const(4u64, ValueType::I64).unwrap();
    let sum = b.build_int_binary_operation(IntBinaryOp::Add, c1, c2, ValueType::I64);
    assert!(matches!(b.function().value_kind(sum).as_value(), Some(ValueType::I64)));
}
```
Adapt `as_value()` / `empty_builder` to the real API. Add an `EditFunction` case asserting `build_int_const` through an editing context tracks the new node as live (use the `edit::` test pattern). Run: `cargo test -p strider-ir --test builder_ext` → FAILS to compile (`IRBuilderExt` not defined yet).

- [ ] **Step 3: Create `IRBuilderExt`.** New `crates/strider-ir/src/builder/builder_ext.rs`:
```rust
//! The shared IR construction vocabulary. Any [`IRBuilder`] gains every
//! `build_*` constructor for free via the blanket impl — the lifter, the
//! optimizer's editing context, and a plain function all build IR the same way.
use crate::builder::IRBuilder;
use crate::node::{NodeKind, NodeId};
use crate::ops::{ValueId, ValueKind};
use crate::{ValueType, IntBinaryOp, /* ...the op enums used... */};
use crate::error::Result;

pub trait IRBuilderExt: IRBuilder {
    fn build_int_const(&mut self, val: impl Into<u128>, ty: ValueType) -> Result<ValueId> {
        // moved from Graph::make_int_const: reject wide ty, mask val to ty's
        // bit width, create the IntConst, return its value output.
        // (copy the exact masking + wide-reject logic; then:)
        let node = self.create_node(NodeKind::IntConst(masked), [], [ValueKind::Typed(ty)]);
        Ok(self.function().node_outputs(node)[0])
    }
    fn build_single_output_pure(&mut self, kind: NodeKind, inputs: impl IntoIterator<Item = ValueId>, ty: ValueType) -> ValueId {
        let node = self.create_node(kind, inputs, [ValueKind::Typed(ty)]);
        self.function().node_outputs(node)[0]
    }
    // ... the remaining pure constructors, each body moved verbatim from
    //     FunctionBuilder (nodes.rs), with `self.` receiver ...
}
impl<B: IRBuilder + ?Sized> IRBuilderExt for B {}
```
Move each pure `build_*` body from `nodes.rs` into a default here (they already call `self.create_node`/`self.build_single_output_pure`, so the bodies port directly). For `build_int_const`, lift the masking + wide-reject from `Graph::make_int_const` (`crates/strider-ir/src/ops/consts.rs`) into the default. Wire the module in `builder/mod.rs` + `lib.rs`.

- [ ] **Step 4: Delete the migrated inherent copies.** Remove the moved `build_*` from `FunctionBuilder` (`nodes.rs`) and `build_single_output_pure` from `builder/mod.rs`. Keep the EXCLUDED set. Remove `EditFunction::make_int_const` (`edit/mod.rs`).

- [ ] **Step 5: Repoint callers.** Every file that calls a migrated `build_*` on a `FunctionBuilder`/`EditFunction`/`Function`, or `make_int_const`, needs `use strider_ir::IRBuilderExt;` (and `use strider_ir::IRBuilder;` where `create_node` is used). Find them:
```bash
grep -rln '\.build_\(int_const\|boolean_const\|int_binary\|int_unary\|sub_as_add\|popcount\|lzcount\|int_cmp\|float_const\|float_binary\|float_unary\|float_cmp\|int_to_float\|float_to_int\|float_to_float\|int_bits_to_float\|float_bits_to_int\|segment_op\|cpool_ref\|new\|store\|load\|single_output_pure\)\|\.make_int_const(' crates --include=*.rs
```
Replace `ctx.make_int_const(v, ty)` calls with `ctx.build_int_const(v, ty)`. Add the `use` to each file's import block. The lifter (`strider-lift/src/pcode_lift/**`) is the heaviest user.

- [ ] **Step 6: Resolve `Graph::make_int_const`.** Check its remaining callers (`grep -rn 'make_int_const' crates`). If `build_int_const` now owns the masking and nothing else calls `Graph::make_int_const` except through the ext, either delete `Graph::make_int_const` or keep it as the masking helper that `build_int_const` calls (cleanest if `IRBuilder` can reach it — it can't through the trait, so the masking lives in the default). Decide and note it; don't leave two divergent masking implementations.

- [ ] **Step 7: Gate.**
```bash
cargo test -p strider-ir --test builder_ext 2>&1 | tail
cargo test --workspace 2>&1 | tail -12
cargo clippy --workspace --all-targets 2>&1 | tail -3
cd crates/strider-py && uv run pytest -q 2>&1 | tail -6 ; cd ../..
```
All pass; clippy 0; pytest green. The lifter's existing per-arch tests are the real regression net for the moved constructors.

- [ ] **Step 8: Commit.**
```bash
git add -A
git commit -m "feat(strider-ir): IRBuilderExt blanket construction vocabulary shared by every builder; make_int_const becomes build_int_const"
git push origin refactor/builder-ext-vocabulary
```

---

## Task 3: Cull dead / single-use `EditFunction` forwarders

**Files:** `crates/strider-ir/src/edit/mod.rs` + the ≤1 call sites that get inlined.

- [ ] **Step 1: Delete the zero-caller methods.** Remove `EditFunction::absorb_fingerprint` (0 callers), `node_inputs_exact` (0 — callers use `graph_ref().node_inputs_exact`), `node_outputs_exact` (0). Confirm zero callers first: `grep -rn '\.absorb_fingerprint(\|\.node_inputs_exact(\|\.node_outputs_exact(' crates/strider-opt crates/strider-orchestrator` returns only `graph_ref().node_*` chains, not bare `ctx.node_*_exact`.

- [ ] **Step 2: Inline the single-use forwarders.** `set_stack_offset` (1 prod caller, `stack_offset_detect/mod.rs`) → replace `rctx.set_stack_offset(node, base, offset)` with `rctx.function_mut().set_stack_offset(node, base, offset)`, delete the method. `clear_arg_values` (1 caller, `function_args/mod.rs:95`) → `ctx.function_mut().clear_arg_values()`, delete the method. (Confirm `Function` has these methods directly; it does — the forwarders just delegate.)

- [ ] **Step 3: Keep the rest.** Do NOT touch `graph_ref` (25), `walk` (42), `walk_kind` (5), `is_root` (6), `register_arg_value` (2), `remove_region_predecessors` (2), the cached walks, the edit verbs, or the `#[cfg(test)]` `live_snapshot`/`roots_snapshot`.

- [ ] **Step 4: Gate + commit.**
```bash
cargo test --workspace 2>&1 | tail -8
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A
git commit -m "refactor(strider-ir): drop dead/single-use EditFunction forwarders"
git push origin refactor/builder-ext-vocabulary
```

---

## Final: review + merge

- [ ] Holistic review over `develop..HEAD` (focus: the `IRBuilder`/`IRBuilderExt` design, that every pure constructor moved verbatim with no logic change, the lifter still stamps `lift_addr`, `EditFunction` still tracks liveness, `make_int_const`→`build_int_const` masking is identical, and the `with_attribution` removal preserves `live==compute_full`). Fix Critical/Important.
- [ ] Confirm `grep -rn '\bBuilder\b' crates` shows no stray un-renamed core-trait `Builder` (only `FunctionBuilder` / `strider_lift::cfg::Builder`).
- [ ] Merge `--no-ff` into `develop`, push, remove worktree + branch (after user confirmation).

## Non-goals (deferred)
- Rewriting passes to USE `build_store`/`build_load`/`build_int_binary_operation` (separate increments).
- Routing matcher/template match-graphs through `IRBuilder`.
- Graph-crate split / wide-const-to-`Function`.
