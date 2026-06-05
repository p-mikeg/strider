# IR function-module refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relocate shared IR builder vocabulary onto the right traits, record register args at builder entry instead of a post-pass, collapse redundant `value_vn` accessors, drop two unused/test-only `Function` methods, delete a dead generic walker, and consolidate the function data structures into one `function/` directory — all without changing lifted-IR semantics.

**Architecture:** Seven independent refactors against `strider-ir` (+ ripples into `strider-opt`, `strider-orchestrator`, `strider-pattern`, `strider-py`, `strider-ir-test-utils`, `strider-graph`). Six are pure moves/renames verified by the existing test suite; one (item 2) is a behavior change covered by a new test. The big file move (item 3) is sequenced last so all content edits land at stable paths first.

**Tech Stack:** Rust workspace, `cargo test`/`cargo clippy`, `cranelift-entity`, `anyhow`. Python bindings via PyO3 (`uv run pytest`).

**Working rules:** Branch `develop`. One commit per task; `git push origin develop` after each commit. End commit messages with the `Co-Authored-By` trailer. Do NOT mention plan/item identifiers in code or commit messages. Prompt the user before merging to `master`. Full-workspace gate (`cargo test --workspace` + `cargo clippy --workspace` + pytest) runs before the merge prompt.

Spec: `docs/superpowers/specs/2026-06-05-ir-function-refactor-design.md`

---

## Task 1: Relocate the three builder functions (spec item 1)

**Files:**
- Modify: `crates/strider-ir/src/viewer.rs` (add two `require_*` to `IRViewer`)
- Modify: `crates/strider-ir/src/region.rs` (delete the two inherent `require_*`)
- Modify: `crates/strider-ir/src/builder/build_trait.rs` (add `function_mut` to `IRBuilder` + both impls)
- Modify: `crates/strider-ir/src/builder/builder_ext.rs` (add `build_int_const_wide`)
- Modify: `crates/strider-ir/src/builder/nodes.rs` (delete `build_int_const_wide`)
- Modify: `crates/strider-ir/src/edit/mod.rs` (`IRBuilder` impl for `EditFunction` gains `function_mut`)

- [ ] **Step 1: Move `require_control_kind`/`require_memory_kind` to `IRViewer`.**

In `region.rs`, delete these two `pub(crate)` methods (lines ~52-72):
```rust
pub(crate) fn require_control_kind(&self, value: ValueId) -> Result<()> { ... }
pub(crate) fn require_memory_kind(&self, value: ValueId) -> Result<()> { ... }
```
In `viewer.rs`, add them as default methods on `trait IRViewer`, immediately after `require_phi_token_kind`:
```rust
    /// Errors unless `value_id` is a control edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a control edge.
    fn require_control_kind(&self, value_id: ValueId) -> crate::Result<()> {
        let kind = self.function().graph().value_kind(value_id);
        if !kind.is_control() {
            return Err(anyhow!("output {value_id:?} is not a control edge (got {kind:?})"));
        }
        Ok(())
    }

    /// Errors unless `value_id` is a memory edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a memory edge.
    fn require_memory_kind(&self, value_id: ValueId) -> crate::Result<()> {
        let kind = self.function().graph().value_kind(value_id);
        if !kind.is_memory() {
            return Err(anyhow!("output {value_id:?} is not a memory edge (got {kind:?})"));
        }
        Ok(())
    }
```
(`anyhow` is already imported in `viewer.rs`.) `region.rs` keeps `require_terminator_kinds` (it calls the two via `self.` — now resolved through the trait). No call-site changes: every caller is a `FunctionBuilder`, which impls `IRViewer`.

- [ ] **Step 2: Add `function_mut` to the `IRBuilder` trait.**

In `build_trait.rs`, add a required method to `trait IRBuilder` (before `create_node_attributed`):
```rust
    /// Mutable access to the function under construction/edit.
    ///
    /// The write-side counterpart to [`crate::IRViewer::function`]. NOTE: this
    /// is a structural escape hatch — mutating graph *structure* through it
    /// bypasses [`crate::EditFunction`]'s cached live/roots bookkeeping (same
    /// caveat as [`crate::EditFunction::function_mut`]). Default methods on
    /// [`crate::IRBuilderExt`] may use it only for side-table-local work
    /// (e.g. interning a wide const), never to add/remove nodes or edges.
    fn function_mut(&mut self) -> &mut crate::Function;
```
Add to the `impl IRBuilder for FunctionBuilder` block:
```rust
    fn function_mut(&mut self) -> &mut crate::Function {
        &mut self.function
    }
```
In `edit/mod.rs`, add to the `impl IRBuilder for EditFunction<'_>` block (alongside `create_node_attributed`):
```rust
    fn function_mut(&mut self) -> &mut crate::Function {
        self.function
    }
```
(Both types keep their inherent `function_mut()`; concrete calls resolve to the inherent one, the trait method is used only through a generic `B: IRBuilder` bound.)

Update the `IRBuilder` doc comment in `build_trait.rs` that says the trait "only creates and exposes read access" → note it now also hands out `&mut Function` with the escape-hatch caveat above.

- [ ] **Step 3: Move `build_int_const_wide` to `IRBuilderExt`.**

Delete the `pub fn build_int_const_wide(...)` method from `nodes.rs` (lines ~25-49) and its surrounding now-unused imports if any become dead (check `WideConstStorage`). Add it to `builder_ext.rs` as a default method on `trait IRBuilderExt`, after `build_int_const`:
```rust
    /// Builds a wide integer constant — `I256` (32 bytes) or `I512`
    /// (64 bytes) — interning `value` so equal values share a `WideConstId`
    /// (and hence a `NodeId` under the dedup cache).
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not `I256`/`I512`, or when
    /// `value.byte_size()` doesn't match `output_type`'s byte size.
    fn build_int_const_wide(
        &mut self,
        value: crate::wide_const::WideConstStorage,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let expected = match output_type {
            ValueType::I256 => 32usize,
            ValueType::I512 => 64usize,
            other => {
                return Err(anyhow!(
                    "build_int_const_wide called with non-wide output type {other:?}; \
                     use build_int_const for ≤ I128"
                ));
            }
        };
        if value.byte_size() != expected {
            return Err(anyhow!(
                "WideConstStorage byte_size {} does not match output type {output_type:?} \
                 (expected {expected})",
                value.byte_size()
            ));
        }
        let id = self.function_mut().intern_wide_const(value);
        Ok(self.build_single_output_pure(NodeKind::IntConstWide(id), [], output_type))
    }
```
(`self.function_mut()` now resolves through the `IRBuilder` supertrait method added in Step 2.)

- [ ] **Step 4: Fix the stale doc reference.**

In `builder_ext.rs`, the `build_int_const` error message + rustdoc reference `FunctionBuilder::build_int_const_wide`. Change both to `Self::build_int_const_wide` (or `IRBuilderExt::build_int_const_wide`).

- [ ] **Step 5: Build + test strider-ir.**

Run: `cargo test -p strider-ir`
Expected: PASS (existing tests still green; the `build_int_const_wide_*` tests in `builder/tests.rs` now call the trait method — they already call it on a `FunctionBuilder` `b`, which has `IRBuilderExt` via the blanket impl; add `use strider_ir::IRBuilderExt;` to the test module if not already in scope).

Run: `cargo clippy -p strider-ir`
Expected: zero warnings.

- [ ] **Step 6: Commit + push.**

```bash
git add crates/strider-ir/src/
git commit -m "refactor(strider-ir): move require_control/memory_kind to IRViewer, build_int_const_wide to IRBuilderExt

Add function_mut to the IRBuilder seam so the wide-const constructor is
available to every builder, not just FunctionBuilder.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 2: Record register args at builder entry; make FunctionArgDetect stack-only (spec item 2)

**Files:**
- Modify: `crates/strider-ir/src/builder/vars.rs` (register args in `set_entry_region`)
- Test: `crates/strider-ir/src/builder/tests.rs` (new build-time-registration test)
- Modify: `crates/strider-ir/src/function.rs` (add `clear_arg_values_from`, remove `clear_arg_values`)
- Modify: `crates/strider-opt/src/function_args/mod.rs` (delete register-arg half; use ranged clear)
- Modify: `crates/strider-opt/src/function_args/tests.rs` (rewrite the unused-arg test)

- [ ] **Step 1: Write the failing build-time-registration test.**

In `crates/strider-ir/src/builder/tests.rs`, add (adapt `reg_vn`/CC helpers already used in that file; if the file builds CCs inline, mirror that):
```rust
/// Every arg-passing register's InitialVar is registered as its positional
/// arg carrier at builder-entry time, before any optimization runs.
#[test]
fn register_args_recorded_at_builder_entry() -> Result<()> {
    let rdi = reg_vn(0x38, 8);
    let rsi = reg_vn(0x30, 8);
    let sp = reg_vn(0x20, 8);
    let cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![rdi, rsi],
        callee_saved_regs: vec![],
        ret_val_regs: vec![rdi],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    };
    let mut b = FunctionBuilder::new(vec![rdi, rsi, sp], &cc, strider_target::Endianness::Little)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // No reads, no opt: build-time registration alone populates the table.
    let arg0 = b.function().arg_index_to_values(0);
    let arg1 = b.function().arg_index_to_values(1);
    assert_eq!(arg0.len(), 1, "arg 0 carrier registered at entry");
    assert_eq!(arg1.len(), 1, "arg 1 carrier registered at entry");
    assert!(matches!(b.function().node_kind(b.function().producer(arg0[0])),
        NodeKind::InitialVar(v) if *v == rdi));
    assert!(matches!(b.function().node_kind(b.function().producer(arg1[0])),
        NodeKind::InitialVar(v) if *v == rsi));
    Ok(())
}
```
(Use `reg_vn` from test-utils if imported in that test module; otherwise construct `rsleigh::Vn { size, addr_off, addr_space: REGISTER }` directly. `strider-ir`'s own tests can't use test-utils helpers that *return* `strider_ir::Function`, but plain `Vn` constructors / `reg_vn` are fine.)

- [ ] **Step 2: Run it; verify it fails.**

Run: `cargo test -p strider-ir register_args_recorded_at_builder_entry`
Expected: FAIL — `arg_index_to_values(0)` is empty (registration not yet wired).

- [ ] **Step 3: Register args in `set_entry_region`.**

In `vars.rs::set_entry_region`, after the `for var_id in var_ids` loop that creates InitialVars (after the loop body, before `link_region_variables`), add:
```rust
        // Record register-passed arguments unconditionally: each arg-passing
        // register's (largest-container) InitialVar is the carrier for its
        // positional index. We don't filter on use here — an argument the
        // function never reads is culled by DCE and dropped from the arg
        // table by `Function::compact`, so patterns won't find it.
        let arg_regs: Vec<rsleigh::Vn> =
            self.function.default_cc().arg_passing_regs.clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            if let Some(var_id) = self.var_table.key_of(reg) {
                let value = initial_variables[var_id];
                self.function_mut().register_arg_value(i as u32, value);
            }
        }
```

- [ ] **Step 4: Run the test; verify it passes.**

Run: `cargo test -p strider-ir register_args_recorded_at_builder_entry`
Expected: PASS.

- [ ] **Step 5: Add `clear_arg_values_from`, remove `clear_arg_values`.**

In `function.rs`, replace the `clear_arg_values` method (lines ~584-593) with:
```rust
    /// Drop registered argument carriers for every index `>= first`.
    ///
    /// Lets the stack-arg detection pass rebuild only the stack-arg portion of
    /// the table idempotently across the orchestrator's stable iterations,
    /// without disturbing the register-arg carriers recorded at builder entry
    /// (which occupy indices `0 .. first`).
    #[inline]
    pub fn clear_arg_values_from(&mut self, first: u32) {
        self.arg_index_to_values.retain(|&index, _| index < first);
    }
```

- [ ] **Step 6: Strip the register-arg half from `FunctionArgDetect`.**

In `function_args/mod.rs`:
- Delete `fn detect_register_args(...)` and `fn largest_sub_in(...)` entirely.
- In `Optimizer::apply`, replace the `ctx.function_mut().clear_arg_values();` + `detect_register_args(...)` lines with a ranged clear keyed on the stack boundary, keeping only the stack-arg call:
```rust
        // Register args are recorded at builder entry; this pass owns only the
        // stack-arg indices (>= first_stack_arg). Clear just those so re-running
        // across stable iterations stays idempotent without wiping the
        // build-time register-arg carriers.
        ctx.function_mut().clear_arg_values_from(first_stack_arg as u32);
        detect_stack_args(
            ctx,
            stack_vn,
            &stack_arg_offsets,
            first_stack_arg,
            alias_mode,
            call_clobbers_args,
            &mut opt_ctx.sp_memo,
        )?;
```
- Update the module-level doc comment: remove the "Register args" detection-rule paragraph and the sub-register-fallback prose; keep the stack-arg rule. Note that register args are now recorded by the builder.
- Remove the now-unused `arg_passing_regs` binding if it's only used by the deleted register path (the stack path uses `first_stack_arg` and `stack_arg_offsets`; keep those).

- [ ] **Step 7: Rewrite the unused-arg test.**

In `function_args/tests.rs`, replace `unused_register_arg_yields_no_node` (lines ~453-495) with:
```rust
/// An unused arg register is registered at builder entry unconditionally, then
/// dropped by `compact` once DCE has made its InitialVar unreachable — so
/// patterns can no longer find it.
#[test]
fn unused_register_arg_dropped_by_compact() -> Result<()> {
    let rdi = rdi_like_vn();
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(sp)
        .arg(rdi)
        .callee_saved(rdi)
        .ret(rdi)
        .build_fn_single_region()?;

    // Return a constant — rdi is never read.
    let c = b.build_int_const(0u64, ValueType::I64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Build-time: arg 0 is registered regardless of use.
    assert!(!fg.arg_index_to_values(0).is_empty(), "arg 0 registered at build time");

    // The unread InitialVar(rdi) is unreachable; compaction drops it and its
    // arg-table entry.
    fg.compact()?;
    assert!(
        fg.arg_index_to_values(0).is_empty(),
        "unused arg carrier dropped after compact"
    );
    assert_eq!(fg.iter_arg_indices().count(), 0, "table empty after compact");
    Ok(())
}
```
Review the other register-arg tests in this file (e.g. `reads_rdi_emits_function_arg_0`, the idempotency test around line 75): they should still pass because build-time registration populates arg 0 before the (now stack-only) pass runs. If any asserts that the *pass itself* registers a register arg, re-target it to assert the post-build state. Fix only what fails the run in Step 8.

- [ ] **Step 8: Build + test the touched crates.**

Run: `cargo test -p strider-ir`
Expected: PASS.
Run: `cargo test -p strider-opt`
Expected: PASS (fix any register-arg test that asserted pass-time registration per Step 7).
Run: `cargo clippy -p strider-ir -p strider-opt`
Expected: zero warnings.

- [ ] **Step 9: Commit + push.**

```bash
git add crates/strider-ir/src/ crates/strider-opt/src/
git commit -m "refactor: record register args at builder entry; FunctionArgDetect handles only stack args

Register-passed argument carriers are now registered unconditionally when the
entry region is wired; DCE + compact drop unused ones. The post-pass keeps the
stack-arg detection that genuinely needs the optimized memory graph, and clears
only its own (stack) indices to stay idempotent.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 3: Collapse the four `value_vn` accessors into one value-keyed pair (spec item 5)

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (replace 4 accessors with 2)
- Modify (production callers): `crates/strider-ir/src/builder/call.rs`, `crates/strider-ir/src/builder/nodes.rs`, `crates/strider-ir/src/function_dot/label.rs`, `crates/strider-ir/src/function_dot/raw.rs`, `crates/strider-ir/src/validate/graph_invariants.rs`, `crates/strider-opt/src/indirect_branch_resolve/classify.rs`, `crates/strider-opt/src/rewrite/mod.rs`, `crates/strider-orchestrator/src/orchestrator/mod.rs`, `crates/strider-pattern/src/node_builders/phi.rs`, `crates/strider-pattern/src/match_result.rs`
- Modify (tests): the test files enumerated by grep (`strider-ir`, `strider-opt`, `strider-orchestrator`)
- Modify (docs only): `crates/strider-ir/src/node/kind.rs`, `crates/strider-ir/src/node_signature.rs`, `crates/strider-py/src/matcher.rs`

- [ ] **Step 1: Replace the four accessors with two.**

In `function.rs`, delete `phi_var_tag`, `set_phi_var_tag`, `clobbered_vn`, `set_clobbered_vn` and add:
```rust
    /// Returns the source varnode a value represents — the lift-time `Phi`'s
    /// tracked varnode, or a `Call`/`CallOther` clobber output's clobbered
    /// register — or `None`. Single value-keyed view over `value_vn`.
    #[inline]
    pub fn get_vn_for_value(&self, value: ValueId) -> Option<rsleigh::Vn> {
        self.value_vn.get(&value).copied()
    }

    /// Records that `value` represents varnode `vn`. Replaces any prior value.
    #[inline]
    pub fn set_vn_for_value(&mut self, value: ValueId, vn: rsleigh::Vn) {
        self.value_vn.insert(value, vn);
    }
```

- [ ] **Step 2: Rewrite the value-keyed production callers (straight rename).**

`clobbered_vn(v)` → `get_vn_for_value(v)`; `set_clobbered_vn(v, vn)` → `set_vn_for_value(v, vn)`:
- `builder/call.rs:129,135`: `self.function_mut().set_clobbered_vn(*value, *vn)` → `set_vn_for_value(*value, *vn)`.
- `function_dot/label.rs:303`: `self.function.clobbered_vn(value_id)` → `get_vn_for_value(value_id)`.
- `function_dot/raw.rs:106`: `f.clobbered_vn(v)` → `f.get_vn_for_value(v)`.
- `validate/graph_invariants.rs:245`: `function.clobbered_vn(v)` → `get_vn_for_value(v)`.
- `orchestrator/mod.rs:1016`: `function.set_clobbered_vn(*value, *vn)` → `set_vn_for_value(*value, *vn)`.
- `pattern/src/match_result.rs:98`: `function.clobbered_vn(value)` → `get_vn_for_value(value)`.

- [ ] **Step 3: Rewrite the node-keyed production callers (derive value first).**

`set_phi_var_tag(node, vn)` → derive the node's single output, then `set_vn_for_value`:
- `builder/nodes.rs:300` (in `build_vn_phi`): replace
  ```rust
  let (node_id, _slot) = self.function().value_definition(phi_value);
  self.function_mut().set_phi_var_tag(node_id, var);
  ```
  with
  ```rust
  self.function_mut().set_vn_for_value(phi_value, var);
  ```
  (`phi_value` is the Phi's output, already in scope — no need to round-trip through `value_definition`.)
- `indirect_branch_resolve/classify.rs:480,561` (`function.set_phi_var_tag(node, vn)`): replace with `function.set_vn_for_value(function.node_outputs(node)[0], vn)` (these tag freshly-built Phi nodes; a Phi has exactly one output).

`phi_var_tag(node)` → `get_vn_for_value(node_outputs(node)[0])`. For callers already guarded by a `NodeKind::Phi` arm or known-Phi producers, index `[0]` directly:
- `function_dot/label.rs:117` (`match self.function.phi_var_tag(node)` inside `NodeKind::Phi =>`): `self.function.get_vn_for_value(self.function.node_outputs(node)[0])`.
- `function_dot/raw.rs:89`: same pattern (`f.get_vn_for_value(f.node_outputs(node)[0])`).
- `indirect_branch_resolve/classify.rs:113,316,364,407` (`function.phi_var_tag(pid)` where `pid` is a Phi producer): `function.get_vn_for_value(function.node_outputs(pid)[0])`.
- `rewrite/mod.rs:558` (`function.phi_var_tag(n)` inside a Phi-kind context): `function.get_vn_for_value(function.node_outputs(n)[0])`.
- `pattern/src/node_builders/phi.rs:31` (`m.function().phi_var_tag(n) == Some(want)`): `m.function().get_vn_for_value(m.function().node_outputs(n)[0]) == Some(want)` (the matcher only invokes this limit on a matched `Phi` node, which has one output).

- [ ] **Step 4: Rewrite the test callers.**

Apply the same two transforms across the test files surfaced by grep:
`strider-ir/src/function.rs` (test mods), `strider-ir/src/function_dot/tests.rs`, `strider-ir/src/validate/tests.rs`, `strider-ir/src/builder/tests.rs`, `strider-ir/src/graph/tests.rs`, `strider-opt/src/cfg_detach/tests.rs`, `strider-opt/src/load_forward/tests.rs`, `strider-opt/src/phi_collapse/tests.rs`, `strider-orchestrator/tests/calling_convention.rs`, `strider-orchestrator/tests/cross_arch_shape.rs`, `strider-orchestrator/tests/complex_patterns.rs`, `strider-orchestrator/tests/optimizer_pipeline_subsets.rs`.

**Empty-output guard for arbitrary-node scans.** Two scans in `function.rs` test mods iterate `all_node_ids()` and call `phi_var_tag(n)` on *every* node (including `Return`, which has zero outputs). `phi_var_tag` used `.first().copied()?` (→ `None`); `node_outputs(n)[0]` would panic. Rewrite those as:
```rust
.any(|n| f.node_outputs(n).first().copied()
    .and_then(|v| f.get_vn_for_value(v)) == Some(dead_vn))
```
(at `function.rs:1243` and `function.rs:1377`). Callers inside a `NodeKind::Phi` arm index `[0]` directly.

- [ ] **Step 5: Update doc references.**

Replace prose mentions of the removed names with the new ones:
- `node/kind.rs:52,54`, `node_signature.rs:307` (`phi_var_tag`), `function.rs:100` field doc, `function_dot/raw.rs:9` module doc, `pattern/src/match_result.rs:80` doc, `pattern/src/node_builders/phi.rs` module docs, `strider-py/src/matcher.rs:224` doc. Keep them accurate (e.g. "stored in `Function::value_vn`, read via `get_vn_for_value`"). `CLAUDE.md` references are intentionally out of scope (separate CLAUDE.md pass).

- [ ] **Step 6: Build + test all touched crates.**

Run: `cargo test -p strider-ir -p strider-opt -p strider-pattern -p strider-orchestrator`
Expected: PASS.
Run: `cargo clippy -p strider-ir -p strider-opt -p strider-pattern -p strider-orchestrator`
Expected: zero warnings.

- [ ] **Step 7: Commit + push.**

```bash
git add crates/
git commit -m "refactor(strider-ir): collapse value_vn accessors to get_vn_for_value/set_vn_for_value

phi_var_tag/set_phi_var_tag were node-keyed wrappers over the same value_vn map
as clobbered_vn/set_clobbered_vn; unify on one value-keyed pair, with callers
deriving the Phi output value at the call site.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 4: Remove the dead `stack_offsets()` iterator (spec item 6)

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (delete the iterator method)

- [ ] **Step 1: Delete the method.**

In `function.rs`, delete `pub fn stack_offsets(&self) -> impl Iterator<...>` (lines ~614-620). Keep the `stack_offsets` field, `stack_offset(id)`, `set_stack_offset`, and the `compact` remap.

- [ ] **Step 2: Build + test.**

Run: `cargo test -p strider-ir`
Expected: PASS (no callers).
Run: `cargo clippy -p strider-ir`
Expected: zero warnings (confirms truly unused).

- [ ] **Step 3: Commit + push.**

```bash
git add crates/strider-ir/src/function.rs
git commit -m "refactor(strider-ir): drop unused stack_offsets() iterator

No callers; the singular stack_offset(id) lookup and set_stack_offset remain.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 5: Remove `set_asm_fingerprint`, migrate to `extend_asm_fingerprint` (spec item 7)

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (delete `set_asm_fingerprint`; fix docs)
- Modify: `crates/strider-ir-test-utils/src/lib.rs:383` (stamper)
- Modify (tests): `strider-ir/src/builder/tests.rs`, `strider-ir/src/graph/tests.rs`, `strider-ir/src/validate/tests.rs`, `strider-ir/src/function.rs` (test mods), `strider-opt/src/call_stack_args/tests.rs`, `strider-opt/tests/rewrite_match.rs`, `strider-orchestrator/tests/indirect_resolve_in_place_edits.rs`, `strider-orchestrator/tests/common/indirect_resolve_helpers/classify.rs`

- [ ] **Step 1: Migrate the test-utils stamper first.**

In `strider-ir-test-utils/src/lib.rs:383`, change
`function.set_asm_fingerprint(n, vec![SENTINEL_LIFT_ADDR]);`
→ `function.extend_asm_fingerprint(n, &[SENTINEL_LIFT_ADDR]);`
(the node is freshly created → empty → identical result).

- [ ] **Step 2: Migrate the test call sites.**

For each `f.set_asm_fingerprint(n, vec![A, B, ...])`, replace with `f.extend_asm_fingerprint(n, &[A, B, ...])`. **Per-site replace-vs-union check:** the union differs from the old replace only when the node already carries a fingerprint at the call. Audit each:
  - Nodes built via raw `Function::default()` + `graph_mut().create_node()` (the `function.rs`, `graph/tests.rs` synthetic graphs): start empty → safe.
  - Nodes built via test-utils helpers (auto-stamped `SENTINEL`) that then pin an *exact* fingerprint (e.g. `rewrite_match.rs:466`, `call_stack_args/tests.rs:1022`, the `indirect_resolve` helpers): under `extend`, `SENTINEL` would remain alongside the new addr. If the test asserts exact contents, either (a) build the node so it's empty before stamping, or (b) relax the assertion to "contains" the pinned addr. Prefer (b) when the test's intent is "this addr is attributed", since the post-change fingerprint legitimately includes both.

- [ ] **Step 3: Delete the method + fix docs.**

In `function.rs`, delete `pub fn set_asm_fingerprint(...)` (lines ~659-664). Update the `asm_fingerprints` field doc (line ~100) and any doc that names `set_asm_fingerprint` as the test entry point → reference `extend_asm_fingerprint` / `extend_asm_fingerprint_from`. Check `edit/mod.rs:8,370` comments (they say "no raw set_asm_fingerprint here" — still true, no change needed, but verify they don't imply it exists elsewhere).

- [ ] **Step 4: Build + test all touched crates.**

Run: `cargo test -p strider-ir -p strider-opt -p strider-orchestrator`
Expected: PASS. Fix any exact-fingerprint assertion broken by the union per Step 2.
Run: `cargo clippy -p strider-ir`
Expected: zero warnings.

- [ ] **Step 5: Commit + push.**

```bash
git add crates/
git commit -m "refactor(strider-ir): remove test-only set_asm_fingerprint; stamp via extend

The fingerprint mutation API is now the two no-shrink mutators. Test stamping
uses extend on freshly-built (empty) nodes, preserving the superset-only invariant.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 6: Delete `strider-graph/src/walk.rs`; re-anchor its proptest (spec item 4)

**Files:**
- Delete: `crates/strider-graph/src/walk.rs`
- Modify: `crates/strider-graph/src/lib.rs` (drop `mod walk;`)
- Modify: `crates/strider-graph/tests/proptest_invariants.rs:~275` (re-anchor the reachability oracle)

- [ ] **Step 1: Inspect the proptest usage.**

Read `crates/strider-graph/tests/proptest_invariants.rs` around line 275. It computes `let expected: HashSet<NodeId> = g.preorder_seeds([root]).into_iter().collect();` as a reachability oracle and compares against the property under test.

- [ ] **Step 2: Replace `preorder_seeds` with an inline backward-input walk.**

Substitute an inline computation (same semantics — backward-input reachability from `root`):
```rust
        let expected: HashSet<NodeId> = {
            let mut seen: HashSet<NodeId> = HashSet::new();
            let mut stack = vec![root];
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                for input in g.node_inputs(n) {
                    stack.push(g.producer(input));
                }
            }
            seen
        };
```
(`g.node_inputs` / `g.producer` are inherent on the generic `Graph`. Confirm `HashSet`/`NodeId` are already imported in the test; add `use std::collections::HashSet;` / the `NodeId` path if needed.)

- [ ] **Step 3: Delete the file + module decl.**

```bash
git rm crates/strider-graph/src/walk.rs
```
In `crates/strider-graph/src/lib.rs`, delete the `mod walk;` line.

- [ ] **Step 4: Build + test strider-graph.**

Run: `cargo test -p strider-graph`
Expected: PASS (proptest oracle now inline; no other `walk` references).
Run: `cargo clippy -p strider-graph`
Expected: zero warnings.

- [ ] **Step 5: Commit + push.**

```bash
git add crates/strider-graph/
git commit -m "refactor(strider-graph): drop unused def-use walkers; inline the proptest oracle

preorder_seeds/reverse_postorder_seeds had no cross-crate users; the control-aware
walkers live in strider-ir. The lone proptest user computes its reachability
oracle inline.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 7: Consolidate the function data structures into `function/` (spec item 3)

**Files:**
- Move: `crates/strider-ir/src/function.rs` → `crates/strider-ir/src/function/data.rs`
- Move: `crates/strider-ir/src/edit/function_state.rs` → `crates/strider-ir/src/function/state.rs`
- Move: `crates/strider-ir/src/edit/mod.rs` → `crates/strider-ir/src/function/edit.rs`
- Move: `crates/strider-ir/src/function_dot/` → `crates/strider-ir/src/function/dot/`
- Create: `crates/strider-ir/src/function/mod.rs`
- Modify: `crates/strider-ir/src/lib.rs` (module decls + re-exports)
- Modify: any in-crate path references to `crate::edit::` / `crate::function_dot::` / `crate::function::`

- [ ] **Step 1: Do the file moves with git (preserve history).**

```bash
cd crates/strider-ir/src
mkdir -p function/dot
git mv function.rs function/data.rs
git mv edit/function_state.rs function/state.rs
git mv edit/mod.rs function/edit.rs
git mv function_dot/mod.rs function/dot/mod.rs
git mv function_dot/label.rs function/dot/label.rs
git mv function_dot/raw.rs function/dot/raw.rs
git mv function_dot/render.rs function/dot/render.rs
git mv function_dot/tests.rs function/dot/tests.rs
rmdir edit function_dot
cd -
```

- [ ] **Step 2: Write the `function/mod.rs` root.**

```rust
//! The function data structures: the [`Function`] graph-plus-overlay
//! ([`data`]), the self-cleaning editing context [`EditFunction`] ([`edit`])
//! and its [`FunctionState`] bookkeeping ([`state`]), and the IR-specific dot
//! rendering ([`dot`]).

mod data;
pub mod dot;
mod edit;
mod state;

pub use data::Function;
pub use edit::{EditFunction, FunctionState};
```
Check `state.rs`: if `FunctionState` was re-exported from `edit/mod.rs` via `pub use function_state::FunctionState;`, keep `edit.rs` doing `use crate::function::state::{FunctionState, NodeFlags};` and re-exporting `FunctionState` — OR re-export `FunctionState` from `function/mod.rs` (as above) and have `edit.rs` import it. Pick one; the crate-root re-export must still expose `crate::FunctionState` (see Step 4). Adjust the `mod state; use state::NodeFlags;` wiring inside `edit.rs` to the new path.

- [ ] **Step 3: Fix module decls inside the moved files.**

- `function/edit.rs`: it declared `mod function_state; pub use function_state::FunctionState; use function_state::NodeFlags;`. Replace with the new location: `use crate::function::state::{FunctionState, NodeFlags};` (state is now a sibling module declared by `function/mod.rs`). Remove the inline `mod function_state;`.
- `function/dot/mod.rs`, `label.rs`, `raw.rs`, `render.rs`, `tests.rs`: change any `crate::function_dot::` self-references to `crate::function::dot::`.

- [ ] **Step 4: Update `lib.rs`.**

Replace the three module decls (`mod function;`, `mod edit;`, `mod function_dot;` — find exact lines) with a single `mod function;`. Keep every crate-root re-export resolving to the same public path:
```rust
pub use function::{EditFunction, Function, FunctionState};
pub use function::dot::{ /* whatever was re-exported from function_dot before: FunctionDotDumper, etc. */ };
```
Grep the old `pub use function_dot::...` / `pub use edit::...` / `pub use function::...` lines in `lib.rs` and re-point them at `function::dot::` / `function::` so downstream names (`strider_ir::Function`, `strider_ir::EditFunction`, `strider_ir::FunctionDotDumper`, …) are unchanged.

- [ ] **Step 5: Fix in-crate path references.**

Grep within `crates/strider-ir/src` for `crate::function_dot` and `crate::edit::` and repoint:
```bash
grep -rn "crate::function_dot" crates/strider-ir/src
grep -rn "crate::edit::" crates/strider-ir/src
```
- `crate::function_dot::X` → `crate::function::dot::X` (notably in `function/data.rs`: `dot_dumper` references `crate::function_dot::FunctionDotDumper` / `build_arg_reverse_map`).
- `crate::edit::X` → `crate::function::edit::X` (or the crate-root `crate::X` re-export).
`crate::function::Function` keeps resolving (re-exported by `function/mod.rs`), so `crate::function::...` references are unaffected unless they named a submodule.

- [ ] **Step 6: Build + test strider-ir, then the dependents.**

Run: `cargo build -p strider-ir`
Expected: compiles (fix any missed path).
Run: `cargo test -p strider-ir`
Expected: PASS.
Run: `cargo build -p strider-opt -p strider-orchestrator -p strider-pattern -p strider-py`
Expected: compiles — confirms the crate-root re-exports are unchanged for downstream.
Run: `cargo clippy -p strider-ir`
Expected: zero warnings.

- [ ] **Step 7: Commit + push.**

```bash
git add crates/strider-ir/
git commit -m "refactor(strider-ir): consolidate function data structures under function/

Function (data.rs), EditFunction (edit.rs), FunctionState (state.rs), and the
dot renderer (dot/) now live in one function/ directory. Crate-root re-exports
unchanged, so downstream paths are unaffected.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Final verification gate (before merge prompt)

- [ ] **Full workspace tests.**

Run: `cargo test --workspace`
Expected: all pass — no NEW failures vs. the pre-existing baseline (see the v2-baseline note: a small number of fixture-dirty failures may pre-exist; "no new failures" is the criterion).

- [ ] **Workspace clippy.**

Run: `cargo clippy --workspace`
Expected: zero warnings.

- [ ] **Python bindings.**

Run: `cd crates/strider-py && uv sync --group dev && uv run maturin develop && uv run pytest`
Expected: all pass.

- [ ] **Prompt the user** to merge `develop` → `master` (do not merge unprompted).

---

## Self-review notes

- **Spec coverage:** item 1 → Task 1; item 2 → Task 2; item 3 → Task 7; item 4 → Task 6; item 5 → Task 3; item 6 → Task 4; item 7 → Task 5. All seven covered.
- **Type consistency:** new names used consistently — `function_mut` (IRBuilder), `build_int_const_wide` (IRBuilderExt), `get_vn_for_value`/`set_vn_for_value` (Function), `clear_arg_values_from` (Function).
- **Ordering:** content edits (Tasks 1–6) precede the file move (Task 7) so paths are stable during edits; `strider-graph` (Task 6) is fully independent.
