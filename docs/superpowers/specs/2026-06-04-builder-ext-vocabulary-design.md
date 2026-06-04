# IRBuilder Construction Vocabulary (IRBuilderExt) Design

**Date:** 2026-06-04
**Status:** Approved (pending spec review)
**Scope:** Increment 1 — build the unified construction capability. Rewriting
optimizer passes to *use* the new vocabulary is explicitly deferred to later
increments.

## Goal

Make the IR's rich node-construction vocabulary (the `build_*` helpers today
inherent to the lifter's `FunctionBuilder`) available to **every** `IRBuilder` —
the optimizer's `EditFunction` and the plain `Function` included — by lifting
the pure constructors onto a blanket extension trait keyed on one primitive,
`create_node_attributed`. One construction API for the whole IR (lift + opt),
instead of two parallel ones.

## Background — the two parallel construction APIs today

- **Lifter:** `FunctionBuilder` owns ~25 pure value/memory constructors
  (`build_int_const`, `build_int_binary_operation`, `build_store`, …) plus ~10
  lift-stateful ones (`build_entry`, `build_return`, `build_if`, `build_call`,
  `build_vn_phi`, register aliasing). The pure ones each compute a `NodeKind` +
  output `ValueKind`s and bottom out in `FunctionBuilder::create_node` (which
  stamps the ambient `lift_addr`).
- **Optimizer:** `EditFunction` has only `create_node`, `create_node_attributed`,
  and `make_int_const`. To build anything richer it hand-wires `create_node`
  with manual `ValueKind` arrays, or routes through the pattern/template DSL.

The pure `build_*` helpers depend on nothing lift-specific — only `create_node`
and reading the graph for types. So they don't belong to `FunctionBuilder`; they
belong to any builder.

## Architecture

### Core trait — minimal

```rust
// strider-ir
pub trait IRBuilder {
    /// The one creation primitive: create (or dedup to) a node, applying this
    /// builder's own attribution/bookkeeping, unioning each contributor's
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

`create_node_attributed` (not `create_node`) is the primary method each
implementor provides — attribution is the general case, plain creation the
`&[]` specialization.

### The three implementors

| IRBuilder | `create_node_attributed` policy |
|---|---|
| `Function` | create + union contributors. No ambient. |
| `FunctionBuilder` | create + stamp ambient `lift_addr` + union contributors. |
| `EditFunction` | create + union contributors + `track_created` (liveness). |

`FunctionBuilder` **stays** a `IRBuilder` — it is the foundation: the lifter keeps
`lift_addr` while sharing the constructors. (This reverses an earlier throwaway
idea to drop its impl.)

### Blanket extension trait — the shared vocabulary

```rust
// strider-ir
pub trait IRBuilderExt: IRBuilder {
    fn build_int_const(&mut self, val: impl Into<u128>, ty: ValueType) -> Result<ValueId> { … }
    fn build_boolean_const(&mut self, val: bool) -> ValueId { … }
    fn build_single_output_pure(&mut self, kind: NodeKind, inputs: …, ty: ValueType) -> ValueId { … }
    fn build_int_binary_operation(&mut self, op: IntBinaryOp, l: ValueId, r: ValueId, ty: ValueType) -> ValueId { … }
    fn build_int_unary_operation(&mut self, op, x, ty) -> ValueId { … }
    fn build_sub_as_add_neg(&mut self, …) -> ValueId { … }
    fn build_popcount(&mut self, …) -> ValueId { … }
    fn build_lzcount(&mut self, …) -> ValueId { … }
    fn build_int_cmp_operation(&mut self, …) -> ValueId { … }
    fn build_float_const(&mut self, …) -> ValueId { … }
    fn build_float_binary_op(&mut self, …) -> ValueId { … }
    fn build_float_unary_op(&mut self, …) -> ValueId { … }
    fn build_float_cmp_op(&mut self, …) -> ValueId { … }
    fn build_int_to_float(&mut self, …) -> ValueId { … }
    fn build_float_to_int(&mut self, …) -> ValueId { … }
    fn build_float_to_float(&mut self, …) -> ValueId { … }
    fn build_int_bits_to_float(&mut self, …) -> ValueId { … }
    fn build_float_bits_to_int(&mut self, …) -> ValueId { … }
    fn build_segment_op(&mut self, …) -> ValueId { … }
    fn build_cpool_ref(&mut self, …) -> ValueId { … }
    fn build_new(&mut self, …) -> ValueId { … }
    fn build_store(&mut self, mem: ValueId, addr: ValueId, data: ValueId, space: VnSpace) -> ValueId { … }
    fn build_load(&mut self, mem: ValueId, addr: ValueId, ty: ValueType, space: VnSpace) -> ValueId { … }
}
impl<B: IRBuilder + ?Sized> IRBuilderExt for B {}
```

Every default body is written in terms of `self.create_node[_attributed](…)` and
`self.function()` (to read operand types / extract the new node's output). The
exact list mirrors the *pure* constructors currently on `FunctionBuilder`
(`builder/nodes.rs`) — confirm each is pure during implementation and move its
body verbatim into the default, deleting the `FunctionBuilder` inherent copy.

### `make_int_const` → `build_int_const`

`EditFunction::make_int_const` and the lifter's `build_int_const` collapse into
one `IRBuilderExt::build_int_const`. The value-masking (`make_int_const(0x1FF, I8)`
→ `IntConst(0xFF)` so equal constants dedup) and the wide-type (`I256`/`I512`)
rejection move into the default body: mask `val` to `ty`'s bit width, reject
wide `ty` with an error, `create_node(IntConst(masked), …)`, return the output
`ValueId`. `EditFunction::make_int_const` is removed; its callers use
`build_int_const`. `Graph::make_int_const`'s fate (fold into the default vs keep
for direct `Graph` callers) is settled during implementation by checking its
remaining callers.

### What stays inherent on `FunctionBuilder`

The lift-stateful constructors (need `var_table`/`regions`/`cur_region`/
`entry_memory` or register aliasing): `build_entry`, `build_return`,
`build_function_return`, `build_if`, `build_branch`, `build_indirect_branch`,
`build_vn_phi`, `build_call`, `build_call_kind`, `build_call_other`,
`build_masked_insert`, `read_vn`/`write_vn`. **Plus `build_int_const_wide`** —
it interns via `Graph::intern_wide_const`, a mutation beyond the trait's
`create_node`. The optimizer never builds wide consts (loads are ≤8 bytes;
`ConstantFold` rejects I256/I512), so widening the trait to full mutation for
one method isn't worth it — `build_int_const_wide` stays `FunctionBuilder`-only.

### Folded into this increment (the earlier cleanup)

Because they touch the same surface, this increment also lands the agreed
`EditFunction` slimming:
- **Drop `with_attribution` + the `attribution` field + `track_and_create`.**
  `template::instantiate` threads the matched root explicitly:
  `builder.create_node_attributed(kind, inputs, outputs, &[lhs_root])` per node.
  `rewrite_rule_impl` drops the closure wrapper — it just calls
  `instantiate(&rhs, ctx, &bindings, node, root_ty)` (instantiate passes
  `lhs_root` as the contributor internally). Semantics identical (single source =
  matched root), no ambient state.
- **Delete dead/single-use `EditFunction` forwarders:** `absorb_fingerprint`
  (0 callers), `node_inputs_exact`/`node_outputs_exact` (0 — callers use
  `graph_ref().node_*_exact`), `clear_arg_values` (1) and `set_stack_offset`
  (1 prod) inlined to `function_mut()`. Keep `graph_ref` (25), `walk` (42),
  `walk_kind` (5), `is_root` (6), `register_arg_value` (2),
  `remove_region_predecessors` (2), the cached walks, and the real edit verbs.
  `live_snapshot`/`roots_snapshot` stay `#[cfg(test)]`.

## NOT in scope (deferred to later increments)

- Rewriting optimizer passes (`load_forward`, `known_bits`,
  `indirect_branch_resolve` in-place editors, imperative `ConstantFold` arms) to
  *use* `build_store`/`build_load`/`build_int_binary_operation`/… instead of raw
  `create_node`. The capability lands now; each pass is simplified separately so
  diffs stay legible.
- Routing matcher/template *match-graphs* through `IRBuilder` (a separate prior
  follow-up).
- The graph-crate split / wide-const-to-`Function` move.

## Testing strategy (TDD)

- `IRBuilderExt` defaults: a `strider-ir` test that exercises a representative
  spread (`build_int_const` masking + dedup, `build_int_binary_operation`,
  `build_store`/`build_load`) through *all three* builders, asserting structural
  correctness + (for `FunctionBuilder`) `lift_addr` stamping + (for
  `EditFunction`) `track_created` liveness + contributor-fingerprint union.
- The lifter's existing tests are the regression gate for the moved
  constructors (the pcode lifter exercises them end-to-end).
- The 8 `track_*` tests gate the `with_attribution`-removal behavior preservation
  (`cached live_nodes == compute_full(entry)` still holds).
- Full gate: `cargo test --workspace` 0 failures, `cargo clippy --workspace
  --all-targets` 0, `uv run pytest` green.

## Risks / notes

- **Import churn:** lifter (`strider-lift`) + optimizer (`strider-opt`) files
  that call `build_*`/`create_node` need `use strider_ir::{IRBuilder, IRBuilderExt}`.
  Bounded (~15-20 files); the price of one construction API.
- **Per-method purity:** the spec lists the *expected* pure set; the implementer
  confirms each `build_*` body uses only `create_node` + `function()` before
  moving it. Any that secretly read lift state stay on `FunctionBuilder` and are
  reported.
- **Behavior preservation:** moving a `build_*` body verbatim into a default
  changes no logic; `FunctionBuilder` still stamps `lift_addr` because its
  `create_node_attributed` does. The optimizer gaining the vocabulary is
  additive — no existing opt call site changes meaning.
