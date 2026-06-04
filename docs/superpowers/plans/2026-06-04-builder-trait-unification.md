# Builder-Trait Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the three IR node-creation paths behind one `Builder` trait in `strider-ir`, move the editing context (`RewriteCtx` → `EditFunction`) into `strider-ir`, and make template instantiation track liveness + stamp fingerprints *at creation* — deleting the retroactive snapshot-diff reconciliation pass.

**Architecture:** A creation-only `Builder` trait (`create_node` + `function()` read accessor) in `strider-ir`, implemented by `Function` (plain), `FunctionBuilder` (ambient `lift_addr`), and `EditFunction` (ambient attribution + `track_created`). `template::instantiate` becomes generic over `B: Builder`; given an `EditFunction`, every fresh RHS node is tracked + fingerprinted as it is born, so `absorb_fingerprints_into_fresh_subtree` + `track_fresh_subtree` are deleted. `rewrite_rule` / `GraphRewriter` / `OptCtx` / passes stay in `strider-opt`.

**Tech Stack:** Rust workspace; `strider-ir`, `strider-opt`, `strider-pattern`, `strider-orchestrator`, `strider-py`. TDD via `cargo test`; gate `cargo test --workspace` + `cargo clippy --workspace --all-targets` + `uv run pytest`.

**Working tree:** worktree `.worktrees/builder-trait`, branch `refactor/builder-trait-unification`. Push every commit: `git push origin refactor/builder-trait-unification`.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/strider-ir/src/builder/build_trait.rs` (new) | `Builder` trait + `impl Builder for Function` + `impl Builder for FunctionBuilder` | 1 |
| `crates/strider-ir/src/builder/mod.rs` | declare `build_trait` submodule | 1 |
| `crates/strider-ir/src/lib.rs` | `pub use builder::Builder;`, add `pub mod edit;` + `pub use edit::EditFunction;` | 1, 2 |
| `crates/strider-ir/src/edit/mod.rs` (new) | `EditFunction` (was `RewriteCtx`) + `StateSlot` + all edit verbs + cached walks | 2 |
| `crates/strider-ir/src/edit/function_state.rs` (moved) | `FunctionState` + `NodeFlags` | 2 |
| `crates/strider-opt/src/rewrite/mod.rs` | keep `rewrite_rule` / `GraphRewriter` / `check_capture_coverage` / `boxed_rule`; reference `strider_ir::EditFunction`; delete `RewriteCtx`/`StateSlot`/`FunctionState`/`absorb_*`/`track_fresh_subtree` | 2, 3 |
| `crates/strider-opt/src/rewrite/function_state.rs` | **deleted** (moved to strider-ir) | 2 |
| `crates/strider-opt/src/lib.rs` | `pub use strider_ir::EditFunction;` (+ keep `Builder` reachable) | 2 |
| `crates/strider-opt/src/pipeline.rs` + passes | rename `RewriteCtx` → `EditFunction` | 2 |
| `crates/strider-orchestrator/src/**`, `crates/strider-py/src/**` | rename `RewriteCtx` → `EditFunction` | 2 |
| `crates/strider-pattern/src/template/mod.rs` | `instantiate<B: Builder>`; create via `builder.create_node` | 3 |
| `crates/strider-pattern/src/template/ctx.rs` | unchanged (`TemplateCtx.function: &Function`) | 3 |

---

## Task 1: Introduce the `Builder` trait in `strider-ir`

**Files:**
- Create: `crates/strider-ir/src/builder/build_trait.rs`
- Modify: `crates/strider-ir/src/builder/mod.rs` (add `mod build_trait;`)
- Modify: `crates/strider-ir/src/lib.rs` (`pub use builder::Builder;`)

This task is purely additive — no existing behavior changes. `Function` gets a plain `Builder` impl (no fingerprint, no tracking); `FunctionBuilder` gets one delegating to its existing `lift_addr`-stamping `create_node`.

- [ ] **Step 1: Write the failing test** — append to a new `crates/strider-ir/src/builder/build_trait.rs`:

```rust
//! The [`Builder`] creation trait: the single polymorphic node-creation
//! seam shared by the lift builder, the plain function, and the editing
//! context. Creation-only — liveness bookkeeping is the implementor's
//! concern, never part of the contract.

use crate::function::Function;
use crate::node::{NodeId, NodeKind};
use crate::ops::{ValueId, ValueKind};

/// A node-creation seam. Implementors decide their own fingerprint
/// attribution and bookkeeping policy; the trait itself only creates and
/// exposes read access to the function under construction/edit.
pub trait Builder {
    /// Create (or dedup to) a node with `kind`, `inputs`, `outputs`,
    /// applying this builder's attribution/bookkeeping policy.
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>;

    /// Read access to the underlying [`Function`].
    fn function(&self) -> &Function;
}

/// Plainest builder: structural creation only — no fingerprint, no
/// liveness. Used by template-instantiation contexts that need neither
/// (e.g. unit tests building a throwaway RHS).
impl Builder for Function {
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        self.graph_mut().create_node(kind, inputs, outputs)
    }

    fn function(&self) -> &Function {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir_test_utils::make_empty_fn;

    #[test]
    fn function_builder_trait_creates_node() {
        let mut fx = make_empty_fn(|b| b.build_int_const(7u64, crate::ValueType::I64)).unwrap();
        let c = <Function as Builder>::create_node(
            &mut fx,
            NodeKind::IntConst(9),
            [],
            [ValueKind::Typed(crate::ValueType::I64)],
        );
        // The created node is reachable in the graph and carries the kind.
        assert!(matches!(fx.node_kind(c), NodeKind::IntConst(9)));
        // The read accessor returns the same function.
        assert_eq!(Builder::function(&fx).node_kind(c), fx.node_kind(c));
    }
}
```

- [ ] **Step 2: Wire the module** — in `crates/strider-ir/src/builder/mod.rs`, add near the other submodule declarations:

```rust
mod build_trait;
pub use build_trait::Builder;
```

And in `crates/strider-ir/src/lib.rs`, alongside `pub use builder::FunctionBuilder;` (line ~74):

```rust
pub use builder::Builder;
```

- [ ] **Step 3: Run the test to verify it fails** (the `impl Builder for FunctionBuilder` is not yet present, but this test only needs the `Function` impl — confirm it COMPILES and PASSES once the module is wired; if `ValueType`/`ValueKind`/`ValueId` import paths differ, fix the `use`):

Run: `cargo test -p strider-ir build_trait::tests::function_builder_trait_creates_node`
Expected: PASS (the `Function` impl + module wiring make it compile and pass).

- [ ] **Step 4: Add the `FunctionBuilder` impl** — append to `build_trait.rs`:

```rust
use crate::builder::FunctionBuilder;

/// Lift-time builder: structural creation plus the ambient `lift_addr`
/// asm-fingerprint stamp (its existing inherent `create_node` policy).
impl Builder for FunctionBuilder {
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        FunctionBuilder::create_node(self, kind, inputs, outputs)
    }

    fn function(&self) -> &Function {
        FunctionBuilder::function(self)
    }
}
```

If `FunctionBuilder::create_node` (currently `pub(super)` at `builder/mod.rs:355`) is not visible from `build_trait.rs`, widen it to `pub(crate)`.

- [ ] **Step 5: Add a `FunctionBuilder` trait test** — append inside `mod tests`:

```rust
#[test]
fn lift_builder_trait_stamps_lift_addr() {
    use crate::builder::FunctionBuilder;
    let mut b = make_empty_fn_builder();
    b.set_lift_addr(Some(0x4000));
    let n = <FunctionBuilder as Builder>::create_node(
        &mut b,
        NodeKind::IntConst(3),
        [],
        [ValueKind::Typed(crate::ValueType::I64)],
    );
    assert_eq!(Builder::function(&b).asm_fingerprint(n), &[0x4000]);
}
```

If no in-crate `make_empty_fn_builder` helper exists, build the `FunctionBuilder` the way `make_empty_fn` does internally (it cannot use `strider-ir-test-utils` to return a `FunctionBuilder` here only if that introduces the dev-dep double-compile; `make_empty_fn` already works as a dev-dep, so a sibling `FunctionBuilder` constructor in the same dev-dep is fine — confirm by reuse). Adjust `set_lift_addr` / `asm_fingerprint` names to the real API if they differ.

- [ ] **Step 6: Run tests** — `cargo test -p strider-ir build_trait` → PASS. `cargo clippy -p strider-ir --all-targets` → clean.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir/src/builder/build_trait.rs crates/strider-ir/src/builder/mod.rs crates/strider-ir/src/lib.rs
git commit -m "feat(strider-ir): add Builder creation trait; impl for Function + FunctionBuilder"
git push origin refactor/builder-trait-unification
```

---

## Task 2: Move `RewriteCtx` → `EditFunction` into `strider-ir`

**Files:**
- Create: `crates/strider-ir/src/edit/mod.rs`, `crates/strider-ir/src/edit/function_state.rs`
- Modify: `crates/strider-ir/src/lib.rs`
- Delete: `crates/strider-opt/src/rewrite/function_state.rs`
- Modify: `crates/strider-opt/src/rewrite/mod.rs`, `crates/strider-opt/src/lib.rs`, all `RewriteCtx` references in `strider-opt`, `strider-orchestrator`, `strider-py`

This is a mechanical move + rename. The regression gate is the existing suite (the 8 tracking tests stay in `strider-opt`, now referencing the re-exported `EditFunction`). Plus two NEW pure-edit-verb unit tests land in `strider-ir` to exercise the type in its new home.

- [ ] **Step 1: Move `function_state.rs`** — `git mv crates/strider-opt/src/rewrite/function_state.rs crates/strider-ir/src/edit/function_state.rs`. Open it and change the module doc/imports so it compiles inside `strider-ir`: `FunctionState`, `NodeFlags` stay `pub(crate)`; replace any `crate::` paths that pointed at `strider-opt` internals with the `strider-ir` equivalents (`GraphWalkInfo` is `crate::walk::GraphWalkInfo`; `DenseEntitySet`/`Worklist` come from `entity_utils`). `FunctionState::populate` takes `&Function` and uses `crate::walk::GraphWalkInfo::compute_full` — both in-crate now.

- [ ] **Step 2: Move the `EditFunction` body** — create `crates/strider-ir/src/edit/mod.rs`. Move from `strider-opt/src/rewrite/mod.rs` into it: the `StateSlot` enum + its impls, the `RewriteCtx` struct (rename to `EditFunction`) + **every** `impl` block on it (constructors `try_for_built`/`new`/`for_built_with_state`, the edit verbs `kill_node`, `replace_value`, `replace_all_uses`, `update_input`, `add_node_input`, `remove_node_input`, `redirect_input`, `make_int_const`, `create_node`, `create_node_attributed`, the bookkeeping `track_created`, `will_detach_value`, `enqueue_killed_def_node`, `mark_node_dead`, `is_node_dead`, `clean`, `run_initial_cull`, `live_of_kind`, the cached `postorder`/`reverse_postorder`, AND `track_fresh_subtree`). Add `mod function_state; use function_state::{FunctionState, NodeFlags};` at the top. `EditFunction`'s edit verbs do not reference `strider-pattern`.

   Rename `RewriteCtx` to `EditFunction` throughout this moved file. Widen
   `track_fresh_subtree` to `pub` so `rewrite_rule_impl` (staying in
   `strider-opt`) keeps calling `ctx.track_fresh_subtree(...)`; it is tightly
   coupled to the private `track_created`/state, so it travels with the type
   and is deleted in Task 3.

- [ ] **Step 3: Export from `strider-ir`** — in `crates/strider-ir/src/lib.rs`:

```rust
pub mod edit;
pub use edit::{EditFunction, FunctionState};
```

`FunctionState` must be **exported** (not just `pub(crate)`) because the
`strider-opt` pipeline constructs one and threads `&mut state` across
fixed-point iterations into `EditFunction::new(function, &mut state)`. Make
the type `pub struct FunctionState` and `pub fn populate(&Function) -> Self`,
but keep its **fields `pub(crate)`** so it stays an opaque handle outside
`strider-ir`. `EditFunction::new` stays `pub` (it is `pub(crate)` today —
widen it). `EditFunction::try_for_built` stays `pub`.

- [ ] **Step 4: Keep the rewrite glue in `strider-opt`** — in `crates/strider-opt/src/rewrite/mod.rs`:
  - Delete the now-moved items (`StateSlot`, the `RewriteCtx`/`EditFunction` struct + all its `impl` blocks including `track_fresh_subtree`, the `mod function_state;`).
  - Keep `rewrite_rule`, `rewrite_rule_impl`, `rewrite_rule_runtime`, `GraphRewriter`, `check_capture_coverage`, `boxed_rule`, `BoxedRule`, `apply_rules_in_order`, and `absorb_fingerprints_into_fresh_subtree` (this free function and the `ctx.track_fresh_subtree` call in `rewrite_rule_impl` are deleted in Task 3, NOT now).
  - Add `use strider_ir::EditFunction;` at the top. `rewrite_rule_impl` keeps calling `ctx.track_fresh_subtree(new_node, pre_build_node_id)` (now a `pub` method on the moved `EditFunction`) and `absorb_fingerprints_into_fresh_subtree(ctx.function, ...)` exactly as today — Task 2 changes no rewrite behavior.

- [ ] **Step 5: Rename across the workspace** — mechanical `RewriteCtx` → `EditFunction`:

```bash
grep -rl "RewriteCtx" crates/strider-opt/src crates/strider-orchestrator/src crates/strider-py/src \
  | xargs sed -i 's/RewriteCtx/EditFunction/g'
```

Then in `crates/strider-opt/src/lib.rs` add (near the other re-exports) so existing `crate::EditFunction` paths resolve:

```rust
pub use strider_ir::{Builder, EditFunction, FunctionState};
```

Remove any now-duplicate local `pub use rewrite::RewriteCtx` / `EditFunction` line.

Fix the `FunctionState` import paths that the move broke: every `strider-opt`
file that did `use crate::rewrite::function_state::FunctionState;` (notably
`pipeline.rs` and the `rewrite` tests at `mod.rs:1466`/`:1503`/`:1541`) now
imports it from the re-export — `use crate::FunctionState;` (or
`use strider_ir::FunctionState;`). Grep to find them:

```bash
grep -rn "function_state::FunctionState\|rewrite::function_state" crates/strider-opt/src
```

- [ ] **Step 6: Add pure-edit-verb unit tests in `strider-ir`** — append to `crates/strider-ir/src/edit/mod.rs`. (Deep edit-verb coverage stays in the moved `strider-opt` tracking tests; these two prove the type works in its new home.)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeKind;
    use crate::{ValueKind, ValueType};
    use strider_ir_test_utils::make_empty_fn;

    /// `create_node` registers the fresh node as live; `kill_node` removes it.
    #[test]
    fn create_then_kill_tracks_liveness() {
        let mut fx = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();
        let mut ec = EditFunction::try_for_built(&mut fx).unwrap();
        let n = ec.create_node(NodeKind::IntConst(42), [], [ValueKind::Typed(ValueType::I64)]);
        assert!(ec.is_live(n));
        ec.kill_node(n);
        assert!(!ec.is_live(n));
    }

    /// `replace_value` enqueues the displaced cone; after `clean` the cached
    /// live set equals a fresh entry-reachable walk (the core invariant,
    /// exercised through the real edit verbs rather than a bare create).
    #[test]
    fn replace_value_then_clean_keeps_live_eq_reachable() {
        // Graph: root consumes `add(c1, c2)`. Replace the add's value with
        // `c1`; after clean, the add and c2 are culled.
        let mut fx = make_empty_fn(|b| {
            let c1 = b.build_int_const(1u64, ValueType::I64);
            let c2 = b.build_int_const(2u64, ValueType::I64);
            b.build_int_binary_operation(strider_ir::IntBinaryOp::Add, c1, c2, ValueType::I64)
        })
        .unwrap();
        let entry = fx.entry().unwrap();
        let mut state = FunctionState::populate(&fx);
        let mut ec = EditFunction::new(&mut fx, &mut state);
        // The add's value output and one of its const inputs:
        let add_val = ec.function().node_outputs(/* the add node */ root_add_node(&ec))[0];
        let c1_val = ec.function().node_inputs(root_add_node(&ec))[0];
        ec.replace_value(add_val, c1_val).unwrap();
        ec.clean();
        let cached = ec.live_snapshot();
        let fresh = crate::walk::GraphWalkInfo::compute_full(ec.function().graph(), entry).live_nodes;
        assert_eq!(cached, fresh);
    }

    /// Helper: the `Add` node feeding the function root.
    fn root_add_node(ec: &EditFunction<'_>) -> NodeId {
        ec.live_of_kind(|k| matches!(k, NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)))
            .next()
            .expect("the fixture builds exactly one Add")
    }
}
```

Use whatever the real fixture API is — `build_int_binary_operation`'s exact
signature, and how the builder's returned value becomes the function root,
may differ; adapt to the real `FunctionBuilder` surface (cross-check an
existing `strider-ir` builder test). Expose the minimal test-support
accessors as `pub(crate)` helpers on `EditFunction` if absent: `is_live(&self,
NodeId) -> bool` (reads `live_nodes.contains`), `live_snapshot(&self) ->
DenseEntitySet<NodeId>` (clones `live_nodes`), and `live_of_kind` (already
present — it is used by passes). If equivalents exist under other names, use
those.

- [ ] **Step 7: Build + test the move** —

```bash
cargo build -p strider-ir -p strider-opt -p strider-orchestrator
cargo test -p strider-ir edit::
cargo test -p strider-opt
```
Expected: compiles; `strider-ir` edit tests PASS; `strider-opt` suite (incl. the 8 tracking tests) PASS unchanged.

- [ ] **Step 8: Clippy + commit**

```bash
cargo clippy -p strider-ir -p strider-opt --all-targets
git add -A
git commit -m "refactor: move RewriteCtx into strider-ir as EditFunction (FunctionState colocated)"
git push origin refactor/builder-trait-unification
```

---

## Task 3: Ambient attribution + generic `instantiate`; delete the retroactive pass

**Files:**
- Modify: `crates/strider-ir/src/edit/mod.rs` (add `attribution` field + `with_attribution` + `impl Builder for EditFunction`)
- Modify: `crates/strider-pattern/src/template/mod.rs` (`instantiate<B: Builder>`)
- Modify: `crates/strider-opt/src/rewrite/mod.rs` (wrap with `with_attribution`; delete the free `absorb_fingerprints_into_fresh_subtree` here)
- Modify: `crates/strider-ir/src/edit/mod.rs` (delete the `EditFunction::track_fresh_subtree` method moved in Task 2)

This is a behavior-equivalent simplification. The fingerprint + liveness guarantees previously provided by the retroactive pass are now provided at creation. The 8 `strider-opt` tracking tests must stay green; add tests for attribution-at-creation and dedup-revival through the trait.

- [ ] **Step 1: Add a characterization assertion to the existing multi-interior test** — do NOT invent a new fixture. Open `crates/strider-opt/src/rewrite/mod.rs`, find the existing test `track_multi_output_template_interior` (it already applies a rule whose RHS builds multiple fresh interior nodes and then calls `assert_live_matches_reachable`). Capture the matched root's fingerprint before applying, and after applying add an assertion that every freshly-created RHS node carries it. Concretely, after the existing `assert_live_matches_reachable(&ctx, entry);` line in that test, insert:

```rust
    // Characterization lock: the matched root's asm-fingerprint reaches
    // EVERY fresh interior RHS node — stamped at creation (Task 3), not by
    // the retroactive absorb pass (deleted in Task 3).
    let root_fp: Vec<u64> = ctx.function().asm_fingerprint(matched_root).to_vec();
    assert!(!root_fp.is_empty(), "fixture's matched root must carry a fingerprint");
    for n in ctx.live_of_kind(|k| matches!(k, NodeKind::IntBinaryOp(_) | NodeKind::IntConst(_))) {
        let fp = ctx.function().asm_fingerprint(n);
        assert!(
            root_fp.iter().all(|a| fp.contains(a)),
            "fresh RHS node {n:?} missing root fingerprint",
        );
    }
```

Bind `matched_root` to the LHS root `NodeId` the test already locates before applying the rule (reuse the variable the test uses for the node it applies the rule at; rename the snippet's `matched_root` to that variable). If the test does not expose the OptCtx/apply locals needed, lift them into named `let` bindings — no new fixture.

- [ ] **Step 2: Run it to verify the lock passes today** — `cargo test -p strider-opt rewrite::tests::track_multi_output_template_interior`. Expected: PASS via the retroactive `absorb_fingerprints_into_fresh_subtree` (the guarantee already holds). This assertion is a *characterization lock*: its job is to FAIL if the Task-3 deletion (Step 5) regresses fingerprint coverage. Confirming it passes now establishes the baseline.

- [ ] **Step 3: Add attribution to `EditFunction`** — in `crates/strider-ir/src/edit/mod.rs`, add the field and helper, and the trait impl:

```rust
pub struct EditFunction<'g> {
    pub(crate) function: &'g mut Function,
    state: StateSlot<'g>,
    /// Ambient asm-fingerprint source: while `Some(src)`, every node
    /// created through this context absorbs `src`'s fingerprint. Mirrors
    /// `FunctionBuilder::lift_addr`. Set by `with_attribution`.
    attribution: Option<NodeId>,
}
```

Initialise `attribution: None` in every constructor (`try_for_built`, `new`, and any `for_built_with_state`). Then:

```rust
impl<'g> EditFunction<'g> {
    /// Run `f` with `src` as the ambient fingerprint source, restoring the
    /// previous source afterward (nestable, leak-proof).
    pub fn with_attribution<R>(&mut self, src: NodeId, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.attribution.replace(src);
        let r = f(self);
        self.attribution = prev;
        r
    }

    /// Shared creation + bookkeeping: create on the graph, absorb the
    /// ambient attribution source's fingerprint (if any), and register the
    /// node as live. The single creation choke-point for this context.
    fn track_and_create<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        let node = self.function.graph_mut().create_node(kind, inputs, outputs);
        if let Some(src) = self.attribution {
            self.function.extend_asm_fingerprint_from(node, src);
        }
        self.track_created(node);
        node
    }
}
```

Rewrite the existing inherent `create_node` to delegate (its body becomes `self.track_and_create(kind, inputs, output_kinds)`), and add the trait impl:

```rust
impl Builder for EditFunction<'_> {
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        self.track_and_create(kind, inputs, outputs)
    }

    fn function(&self) -> &Function {
        self.function
    }
}
```

The inherent `create_node` (kept) means the ~150 pass call sites need no `use Builder` — inherent resolution wins. The trait impl exists for `instantiate`'s generic bound.

- [ ] **Step 4: Make `instantiate` generic** — in `crates/strider-pattern/src/template/mod.rs`, add `use strider_ir::Builder;` and change the signature + the one creation site:

```rust
pub fn instantiate<B: Builder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueId> {
```

Inside the loop, the dynamic-closure context reads `builder.function()`:

```rust
let ctx = TemplateCtx { function: builder.function(), bindings, root: lhs_root, root_ty: value_ty };
```

and the creation site (was `function.graph_mut().create_node(...)`) becomes:

```rust
let node = builder.create_node(kind, inputs, outputs);
```

The two read sites that followed (`function.node_outputs(node)`, `first_value_output(function, node)`) become `builder.function().node_outputs(node)` and `first_value_output(builder.function(), node)`. The `template_build.rs` tests call `instantiate(&rhs, &mut fx, …)` where `fx: Function`; `Function: Builder` (from Task 1) keeps them compiling unchanged.

- [ ] **Step 5: Rewire `rewrite_rule_impl` and delete the retroactive pass** — in `crates/strider-opt/src/rewrite/mod.rs`, replace steps 3–4b (the `pre_build_node_id` snapshot, `instantiate(&rhs, ctx.function, …)`, the `extend_asm_fingerprint_from` + `absorb_fingerprints_into_fresh_subtree` + `track_fresh_subtree` block) with:

```rust
        // 3. Materialise the RHS THROUGH the editing context so every fresh
        //    node is tracked + fingerprinted (from the matched root) at
        //    creation. A closure may opt out via `Err(skip())`.
        let new_value = match ctx.with_attribution(node, |b| {
            instantiate(&rhs, b, &bindings, node, root_ty)
        }) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // 4. Redirect every consumer of the old root to the new output; this
        //    absorbs the old root's fingerprint into the new producer
        //    (superset-only) and enqueues the orphaned old root for the cull.
        let changed = ctx.replace_value(root_value, new_value)?;
        Ok(changed.then_some(new_value))
```

Then **delete** the `absorb_fingerprints_into_fresh_subtree` free function (in `strider-opt/src/rewrite/mod.rs`) and the `EditFunction::track_fresh_subtree` method (in `strider-ir/src/edit/mod.rs`, moved there in Task 2). Remove now-unused imports (e.g. `DenseEntitySet` / `next_node_id` if they were only used by those).

- [ ] **Step 6: Run the targeted + tracking tests** —

```bash
cargo test -p strider-opt rewrite::          # the 8 tracking tests + the new one
cargo test -p strider-pattern template       # instantiate + template_build
```
Expected: all PASS. The characterization test from Step 1 still passes (now via stamp-at-creation, not the deleted pass).

- [ ] **Step 7: Confirm the dedup-revival regression lock still passes** — the existing test `track_rhs_dedup_revives_culled_const` already asserts that a RHS constant dedup-hitting a culled-but-present node is re-registered into the live set (`assert_live_matches_reachable`). After Step 5 this guarantee now comes from `track_and_create` (the `track_created` call inside the trait `create_node`) instead of the deleted `track_fresh_subtree`. Do NOT add a new fixture — instead update that test's doc comment to state the new mechanism, and confirm it still passes:

```bash
cargo test -p strider-opt rewrite::tests::track_rhs_dedup_revives_culled_const
```
Expected: PASS (revived const present via stamp-and-track at creation).

- [ ] **Step 8: Full gate**

```bash
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets 2>&1 | tail -5
cd crates/strider-py && uv run pytest -q 2>&1 | tail -5 ; cd ../..
```
Expected: workspace tests 0 failures (modulo the known environmental strider-py rust-test-binary link issue — strider-py *lib* + clippy must build clean and `uv run pytest` must pass); clippy 0; pytest green.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: track+fingerprint RHS nodes at creation via Builder trait; drop retroactive reconciliation"
git push origin refactor/builder-trait-unification
```

---

## Final: review + merge

- [ ] Dispatch a holistic code review over `develop..HEAD` (focus: the `Builder` trait surface, the `EditFunction` move's faithfulness, the attribution mechanism, and that `live_nodes == compute_full(entry)` still holds after the retroactive pass is gone). Fix Critical/Important.
- [ ] Confirm `absorb_fingerprints_into_fresh_subtree` and `track_fresh_subtree` are fully gone: `grep -rn "absorb_fingerprints_into_fresh_subtree\|track_fresh_subtree" crates` is empty.
- [ ] Merge `--no-ff` into `develop`, push `origin develop`, remove the worktree + branch (after user confirmation, per standing instruction).

## Non-goals (deferred — do NOT implement here)

- Graph crate split / generic `Graph<N, V>`.
- Moving `wide_const_interner` onto `Function`.
- Routing matcher/template *match-graphs* through `Builder`.
