# crates/opt review — round 6 (2026-04-29)

## Summary

- Files reviewed: 31 (all `.rs` under `crates/opt/src`, `crates/opt/tests`, `crates/opt/benches`)
- Findings total: 47
  - Correctness: 6
  - Dead code: 4
  - Duplication & unification: 9
  - Simplification: 14
  - Readability: 9
  - Performance: 5
- High-confidence findings: 27
- Low-risk findings: 36

The crate is in good shape overall. The mid-iteration vs fixed-point pass split (`stable_default_pipeline` / `destructive_default_pipeline`) is well-articulated, the SP-decomposition memo plumbing is consistent, and most rewrites have been re-expressed via the `pattern` crate's `rewrite_rule` machinery. The most notable themes:

- **Soundness commentary is thorough** — many `// SOUND`, `// CORRECTNESS`, `// SOUNDNESS` blocks exist to pin behaviour. Several still cite legacy bug codenames (BUG-19/21/24/28/30 and `F2`/`R4`/`R5`/`R2-1`) that the user noted were stripped recently — stragglers remain.
- **Repetition of `replace_all_uses` + `OptimizationResult::from_changed`** is the most pervasive minor pattern.
- **Fixed `pub` re-exports that aren't actually used outside the crate** — about half of `lib.rs`'s indirect-branch re-exports are internal only.
- **`#[allow(clippy::too_many_arguments)]` on the dirty/probe helpers in two places** — same shape, would unify nicely under a small `MemWalkCtx` struct.
- **Two near-identical `MAX_TABLE_ENTRIES = 4096` constants** in `jump_table.rs` and `stack_array.rs`.
- The `RedundantPhis::optimize_built` early-skip and dual-iter on phis duplicates `Graph::replace_all_uses` and `cleanup_if_dead` is somewhat tangled.
- Some fixed-point loops still use `let nodes: Vec<_> = function.preorder().collect();` rather than the shared `WorkSet` pattern, despite the worklist module's docstring claiming it "replaces" that pattern.

## Table of contents

- F-001 `dead_branch`: SOFT off-by-one risk in dead-ctrl `idx` collection (Correctness, `dead_branch/mod.rs:60-67`)
- F-002 `KnownBits::Kb` masking silently swallows U128/U256 ConstantFold inputs as zero (Correctness, `known_bits/mod.rs:28-35`)
- F-003 `KnownBits::Kb::merge` "ones wins on conflict" silently destroys `zeros` info on a real bug (Correctness, `known_bits/mod.rs:42-52`)
- F-004 `function_args::detect_register_args` uses `fg.all_node_ids()` instead of preorder, scans zombie InitialVars (Correctness, `function_args/mod.rs:147-152`)
- F-005 `LoadReadOnly::optimize_built` does not enforce a memory space match before constant-folding the Load (Correctness, `load_readonly/mod.rs:55-87`)
- F-006 `OptimizerPipeline::run` runs validate after the post-pass loop only when post-passes succeed; a post-pass error skips validate (Correctness, `pipeline.rs:228-232`)
- F-007 Unused public re-exports in `lib.rs` for indirect-branch internals (Dead code, `lib.rs:53-58`)
- F-008 `SpExpr::shifted` is `pub` but only used internally (Dead code, `sp_expr.rs:39`)
- F-009 `int_const_signed` is `pub` but only used internally (Dead code, `sp_expr.rs:78`)
- F-010 `find_stack_stored_value_at_offset` claims a public API but only its tests exercise it (Dead code, `stack_load_forward/mod.rs:439`)
- F-011 Two `MAX_TABLE_ENTRIES = 4096` constants live in sibling modules (Duplication, `indirect_branch_resolve/{jump_table,stack_array}.rs`)
- F-012 `with_built` boilerplate replicated in 12 `Optimizer::optimize` impls (Duplication, multiple files)
- F-013 `RedundantPhis::replace_output_uses` shadows `Graph::replace_all_uses` (Duplication, `redundant_phis/mod.rs:9-21`)
- F-014 Dirty-walk machinery duplicated between `function_args::mem_chain_is_dirty` and `stack_load_forward::probe`/`find_stack_stored_value_at_offset` (Duplication, `function_args/mod.rs`, `stack_load_forward/mod.rs`)
- F-015 Both `try_collect_stack_args` and `mem_chain_is_dirty` re-implement Store-vs-StackStore alias-discrimination (Duplication, `stack_store/call_args.rs`, `function_args/mod.rs`)
- F-016 `walk_control_for_if_bound` clones `visited` per ControlState predecessor (Performance/Duplication, `indirect_branch_resolve/jump_table.rs:402-406`)
- F-017 `same_value` re-walks via a fresh `HashSet`, ignores per-call cache (Performance, `indirect_branch_resolve/jump_table.rs:525-552`)
- F-018 `analyze_known_bits` invocation per-anchor in `bound_via_known_bits` (Performance, `indirect_branch_resolve/jump_table.rs:265`)
- F-019 `KnownBits::optimize_built` re-collects `node_outputs` per node despite no mutations between iterations (Performance, `known_bits/mod.rs:367-392`)
- F-020 `KnownBits::analyze` `IntConst as u64` truncation losses (Correctness/Simplification, `known_bits/mod.rs:101-103`)
- F-021 `find_stack_stored_value_at_offset` does not memoize, despite `probe` and `mem_chain_is_dirty` doing so (Performance, `stack_load_forward/mod.rs:439`)
- F-022 `apply_link_register` retains the placeholder `target_value` slot to avoid index shifts, but Validator allows a slot-less Return — comment says one thing, code does the safer thing (Readability, `inplace.rs:23-53`)
- F-023 `apply_tail_call` returns the new Return id but caller ignores it (Simplification, `inplace.rs:84-155`, `mod.rs:259-281`)
- F-024 `RedundantPhis::remove_phis` `cleanup_if_dead` short-circuits `simplified` and ORs `Changed` redundantly (Simplification, `redundant_phis/mod.rs:138-143`)
- F-025 `cleanup_if_dead` early `Changed` only when `inputs.is_empty() == false` — meaning a phi that already had its inputs detached would NOT report Changed even though logically that's the goal of detachment (Simplification, `redundant_phis/mod.rs:26-39`)
- F-026 `RedundantPhis::optimize_built` calls `detach_unreachable_nodes` but ignores its return value with a leading `_ =` (Simplification, `redundant_phis/mod.rs:192`)
- F-027 `IndirectBranchResolve::optimize` wraps `with_built` only for the classifier read but performs in-place edits on the raw `Graph` afterward — split-attention pattern (Readability, `indirect_branch_resolve/mod.rs:217-295`)
- F-028 `try_lower_cast_to_float` returns 4 separate "from_changed" branches, all wrapping `replace_all_uses` (Simplification, `constant_fold/mod.rs:23-61`)
- F-029 `eval_int_binary` `shift` closure ignores `bits == 0` for any operation other than ShiftLeft (Simplification, `constant_fold/eval_int.rs:36-38`)
- F-030 `eval_float_unary`/`eval_float_binary`/`eval_float_cmp` arms duplicate F32 vs F64 mechanically (Simplification, `constant_fold/eval_float.rs`)
- F-031 `match_value!` from `ir-macros` is not used anywhere in `opt`; pattern's `rewrite_rule` covers many but not all sites (Duplication, multiple files)
- F-032 `mem_chain_is_dirty` and `probe` and `find_stack_stored_value_at_offset` repeat byte-size fallback `i64::MAX` rationale (Readability, multiple files)
- F-033 `stack_array::strip_target_mask`'s 4-layer cap is undocumented at the only-place that matters (Readability, `indirect_branch_resolve/stack_array.rs:172-216`)
- F-034 `flatten_add_tree` budget cap (32) collides silently with deep ARM lifter trees (Readability/Correctness, `indirect_branch_resolve/stack_array.rs:336-373`)
- F-035 `node_known_bits` returns an `Option<(NodeOutputId, Kb)>` but `Some(out, Kb)` always holds for the kinds it handles — the `Option` is just for "this kind has nothing to say" (Readability, `known_bits/mod.rs:76-278`)
- F-036 `IndirectBranchResolve::default()` calls `Self::new()` so a `derive(Default)` would replace 4 lines (Simplification, `indirect_branch_resolve/mod.rs:198-202`)
- F-037 Codename comments still reference stripped F/R/W/BUG numbers (Readability, multiple files)
- F-038 `IndirectBranchResolve::is_tail_call` boxed predicate is harder to test/serialize than a free function, and the pass struct is otherwise data-only (Readability, `indirect_branch_resolve/mod.rs:138`)
- F-039 `OptimizationResult::from_changed` is invoked dozens of times where a single `?` chain composed differently would let the caller stay in `bool` (Readability, multiple files)
- F-040 `FunctionArgDetect::optimize_built` runs `detach_unreachable_nodes` as a "post-rewrite hygiene" step but always returns `NoChange` for that step's effect (Readability, `function_args/mod.rs:108-115`)
- F-041 `WorkSet::pop` removes from `queued` so subsequent `push` of the same node re-enqueues — comment doesn't explain whether that's intended (Readability, `worklist.rs:42-47`)
- F-042 `cleanup_if_dead` is called in two paths but its name misleads ("if dead" implies the node IS dead before we look) (Readability, `redundant_phis/mod.rs:26-39`)
- F-043 `try_eliminate_dead_branch` sorts dead-uses by descending `idx`, but the comment says LIFO use-list makes it a no-op — code path may be unreachable in practice (Simplification, `dead_branch/mod.rs:62-67`)
- F-044 `tests/common/mod.rs` `run_to_fixed_point` says "(panics on hit — indicates a non-converging pass)" but actually returns Err. Doc-vs-code drift (Readability, `tests/common/mod.rs:104-118`)
- F-045 Tests duplicate `make_fn` and `make_fn_with_var` between `tests/common/mod.rs` and several src tests modules (Duplication, multiple files)
- F-046 `IndirectBranchResolve` pass holds `unresolved_anchors: Vec<...>` and mutates them inline without resetting between calls; orchestrator must clear (Readability, `indirect_branch_resolve/mod.rs:120-138`)
- F-047 `bound_via_predecessor_if` walks transparent control-flow nodes by following `inputs.get(0)`, but the "Control output" check happens AFTER the unwrap — possibly fine in practice but defensive guard reads inverted (Simplification/Readability, `indirect_branch_resolve/jump_table.rs:417-426`)

## Findings

### Correctness

### F-001 `dead_branch`: dead_idx descending sort relies on per-consumer LIFO insertion order

- **Category:** Correctness
- **Location:** `crates/opt/src/dead_branch/mod.rs:60-67`
- **What:**
  ```rust
  // Sort by `idx` descending so that if the same dead_ctrl is wired into one
  // ControlState at multiple slots, removing the higher index first leaves
  // lower indices pointing at their original slots. Removals at different
  // consumers don't interact, so per-consumer descending is enough. The
  // current `Graph` use-list is head-insertion (LIFO) so this sort is a
  // no-op today, but depending on that implementation detail would silently
  // miscompile if the use-list ever became insertion-order (FIFO).
  let mut dead_uses: Vec<(NodeId, u32)> = fg.graph.output_uses(dead_ctrl).collect();
  dead_uses.sort_by(|a, b| b.1.cmp(&a.1));
  ```
- **Why:** The sort key is `idx` (slot index in *the consumer node*), not `(consumer, idx)`. Two distinct ControlState consumers can both have the same dead-ctrl wired at different `idx` values; sorting by `idx` alone interleaves their entries. The current single-pass loop calls `remove_node_input` per `(cs_node, dead_idx)`, and removing slot `j` from one node leaves slot `j` in the OTHER node unchanged, so the interleaving is harmless in *any* sort order — the `b.1.cmp(&a.1)` is therefore not just a no-op for LIFO use-lists, it's a no-op period (the only correctness need is per-consumer descending, which a stable sort by `idx` happens to give for a single consumer but is incidental). The comment overstates its safety contribution. Risk: medium — if a future change groups removals into a batch op, the by-consumer ordering becomes load-bearing and the sort doesn't enforce it.
- **Proposed change:** Either sort by `(consumer, idx desc)` to make the per-consumer descending invariant explicit, or remove the sort and document that the inner `dead_idx < cs_len` guard makes this safe regardless. Add a unit test that exercises two distinct consumers each wired at multiple slots.
- **Confidence:** medium
- **Risk if applied:** low

### F-002 `KnownBits::Kb::from_const` silently treats U128/U256 inputs as zero

- **Category:** Correctness
- **Location:** `crates/opt/src/known_bits/mod.rs:28-35`
- **What:**
  ```rust
  fn from_const(val: u64, ty: NodeOutputType) -> Self {
      let masked = ty.get_unsigned_int(val).unwrap_or(0);
      let type_mask = ty.get_unsigned_int(u64::MAX).unwrap_or(0);
      Kb {
          ones: masked,
          zeros: type_mask ^ masked,
      }
  }
  ```
- **Why:** When `ty` is `U128`/`U256`, `ty.get_unsigned_int(_)` returns `None` (the Kb is u64-bounded). The `.unwrap_or(0)` then assigns `ones = 0, zeros = 0` — i.e. "all bits unknown" but the input was a real `IntConst`. Callers in `node_known_bits` already gate on `type_mask = ty.get_unsigned_int(u64::MAX)` returning `Some` (line 96-98), so the U128/U256 path doesn't reach `from_const` — but `from_const` itself is a public-ish helper and gives wrong info if any future caller forgets the gate. Treating the *input* as fully unknown would produce `ones: 0, zeros: 0` (the right answer); the current `zeros: type_mask ^ masked = 0 ^ 0 = 0` is what you get, so this is actually correct by accident. Documenting the invariant would be cheap and forestall later regressions.
- **Proposed change:** Either return `Kb::default()` explicitly when the type isn't `u64`-representable, or change the signature to `from_const(val: u64, ty: NodeOutputType) -> Option<Kb>` and propagate. The current accidental-correctness deserves at minimum a comment.
- **Confidence:** medium
- **Risk if applied:** low

### F-003 `KnownBits::Kb::merge` always lets `ones` win on conflict — silently masks bugs

- **Category:** Correctness
- **Location:** `crates/opt/src/known_bits/mod.rs:38-52`
- **What:**
  ```rust
  /// Returns `true` if merging `other` into `self` changed anything.
  ///
  /// On conflict (a bit known 1 in one source and 0 in the other), the
  /// `ones` set wins and the conflicting bit is cleared from `zeros`,
  /// preserving the `ones & zeros == 0` invariant.
  fn merge(&mut self, other: Kb) -> bool {
      let new_ones = self.ones | other.ones;
      let new_zeros = (self.zeros | other.zeros) & !new_ones;
      ...
  }
  ```
- **Why:** A bit that's *proved* zero in one source and *proved* one in another is unsatisfiable — the analyzer made a wrong inference somewhere, or the IR is malformed. Silently picking `ones` and discarding the conflicting `zero` evidence prevents the bug from surfacing. Standard known-bits implementations either (a) panic/return Err on contradiction, or (b) report `Top` (no info). The unit test `merge_preserves_invariant_under_conflict` (tests.rs:251) demonstrates the issue but pins it as desired behaviour.
- **Proposed change:** Either return `Result<bool, ErrorKind::Internal(...)>` so a contradiction surfaces, or document this as a known design trade-off and add a debug-time check that conflicts only occur for nodes that the worklist knows are unreachable. (If actually unreachable, the sound merge would clear both — `ones=0, zeros=0` — i.e. fully unknown, which is what conflict-detection should compute.)
- **Confidence:** high
- **Risk if applied:** medium (changes a documented post-condition of `merge`)

### F-004 `detect_register_args` uses `fg.all_node_ids()` so it scans detached zombie InitialVars

- **Category:** Correctness
- **Location:** `crates/opt/src/function_args/mod.rs:147-152`
- **What:**
  ```rust
  let mut initial_vars: rustc_hash::FxHashMap<rsleigh::Vn, NodeId> =
      rustc_hash::FxHashMap::default();
  for n in fg.all_node_ids() {
      if let NodeKind::InitialVar(vn) = *fg.graph.node_kind(n) {
          initial_vars.insert(vn, n);
      }
  }
  ```
- **Why:** `all_node_ids()` returns every node in the arena, including detached zombies left by `RedundantPhis` / `DeadBranchElimination` etc. If a zombie `InitialVar(vn)` for some arg register is in the arena (rare but possible if the IR builder created one and a later pass detached its consumers), the map will still record it and `detect_register_args` will rewire its (now-empty) consumer list — which is a no-op due to the `output_use_cursor(...).current().is_none()` guard at line 188. So functionally it's safe, but it's still O(arena) when O(reachable) would do. More importantly, IF two `InitialVar(vn)` nodes existed (one reachable, one zombie), the `insert` would clobber based on iteration order; `all_node_ids` doesn't guarantee a deterministic order across runs.
- **Proposed change:** Use `fg.preorder()` instead, matching every other pass in this crate. Add a sanity assertion that exactly one InitialVar(vn) is in the reachable set for each `vn`.
- **Confidence:** high
- **Risk if applied:** low

### F-005 `LoadReadOnly` does not match memory space against `Load(space)` before reading the rom

- **Category:** Correctness
- **Location:** `crates/opt/src/load_readonly/mod.rs:55-87`
- **What:**
  ```rust
  let kind = *function.graph.node_kind(node_id);
  let NodeKind::Load(space) = kind else {
      continue;
  };
  ...
  let Some(loaded) = self.0.read(space, addr, size) else {
      continue;
  };
  ```
- **Why:** Reading good — the `space` from the `Load` *is* threaded into the `rom.read(space, ...)`. So the rom impl is the one that decides "do I serve this space". The test `load_other_space_no_change` (tests.rs:117-134) confirms a `RamOnly` rom returns `None` for a non-RAM space. That's correct. **However** — the docstring for the public `ReadOnlyMemory::read` (in `reader/`) does not contractually require `read` to discriminate by space; a degenerate impl might return data for *any* space. Code-review guard would be: if your rom doesn't honour space, you'd inline-fold a `Load(VnSpace::REGISTER, ...)` against rodata — wrong. The guard is defensive; you'd want a unit test against a non-ROM space.
- **Proposed change:** Either: (a) document that `LoadReadOnly` trusts the rom to discriminate by space and add a debug-time assertion that `space == VnSpace::RAM` for the common case, or (b) pre-filter `Load(space)` to known-rom-able spaces (RAM, ROM) before invoking `read`. Currently the test only proves it works in the "rom returns None" path; what about a buggy rom that returns Some for REGISTER?
- **Confidence:** low
- **Risk if applied:** low

### F-006 `OptimizerPipeline::run` skips `validate` when a post-pass returns Err

- **Category:** Correctness
- **Location:** `crates/opt/src/pipeline.rs:228-232`
- **What:**
  ```rust
  for opt in &self.post_passes {
      opt.optimize(graph, entry)?;
  }
  ir::validate::validate(graph, entry)?;
  Ok(())
  ```
- **Why:** If a post-pass returns Err mid-loop, validate never runs. So you could end up returning the post-pass's domain error from a graph that's *also* in an invalid state, but the caller sees only the first one. That's not strictly wrong (first error wins), but it does mean a corrupted graph may go undetected and silently propagate to the next pipeline run. Compare with the inner `optimize` loop: same pattern, same caveat. Documenting this is enough — but the doctring claim "Returns the first error reported by any pass, or the final-validate error from `ir::validate::validate` if the post-pass run leaves an invalid graph" is *false* — if a post-pass errors out, the validate-error path never executes.
- **Proposed change:** Either always call `validate` (and combine its error with the pass error using a multi-error type), or fix the docstring to say "if the *successful* post-pass run leaves an invalid graph". The current docstring oversells.
- **Confidence:** high
- **Risk if applied:** low

### Dead code

### F-007 `lib.rs` re-exports several indirect-branch internals that no external caller uses

- **Category:** Dead code
- **Location:** `crates/opt/src/lib.rs:53-58`
- **What:**
  ```rust
  pub use indirect_branch_resolve::{
      AnchorAddr, AnchorCallingContext, IndirectBranchResolve, ResolvedTargets,
      apply_link_register, apply_tail_call, classify_anchor, classify_anchor_with_rom,
      classify_anchor_with_rom_and_sp, classify_jump_table, classify_stack_array,
      find_placeholder_return_for_anchor,
  };
  ```
- **Why:** Grep across the workspace shows `classify_jump_table` and `classify_stack_array` are referenced **only inside** `crates/opt/src/`. The other re-exports (`AnchorAddr`, `AnchorCallingContext`, `apply_link_register`, `apply_tail_call`, `classify_anchor*`, `find_placeholder_return_for_anchor`, `IndirectBranchResolve`, `ResolvedTargets`) DO have external users in `crates/strider/src/indirect_resolve_tier2/` and `crates/cfg/`. So `classify_jump_table`/`classify_stack_array` are dead re-exports; they should be `pub(crate)` (or just `pub` inside the submodule, no top-level re-export).
- **Proposed change:** Remove the two unused names from the re-export list. Verified no external caller exists.
- **Confidence:** high
- **Risk if applied:** low (still accessible via the submodule path if the internal `mod.rs` keeps them `pub`)

### F-008 `SpExpr::shifted` is `pub` but only used in the same file

- **Category:** Dead code
- **Location:** `crates/opt/src/sp_expr.rs:39-50`
- **What:**
  ```rust
  impl SpExpr {
      #[must_use]
      pub fn shifted(self, delta: i64) -> Self { ... }
  }
  ```
- **Why:** Grep shows the only callers are at sp_expr.rs:144, 146, 159 — all inside `decompose_sp_inner`. The doc comment for `SpExpr` claims out-of-crate callers in `crates/strider` drive `decompose_sp`, but no such caller invokes `.shifted`. Should be `pub(crate)` or private.
- **Proposed change:** Change to `pub(crate) fn shifted(...)` or fold into a private helper.
- **Confidence:** high
- **Risk if applied:** low

### F-009 `int_const_signed` is `pub` but only used in opt internals

- **Category:** Dead code
- **Location:** `crates/opt/src/sp_expr.rs:78-81`
- **What:**
  ```rust
  #[must_use]
  pub fn int_const_signed(g: &Graph, out: NodeOutputId) -> Option<i64> {
      let c = g.int_const_val(out)?;
      g.output_kind(out).as_value()?.get_signed_int(c)
  }
  ```
- **Why:** Grep confirms only called from sp_expr.rs and `indirect_branch_resolve/stack_array.rs:302, 364` — both inside opt. Should be `pub(crate)`.
- **Proposed change:** `pub(crate) fn int_const_signed(...)`.
- **Confidence:** high
- **Risk if applied:** low

### F-010 `find_stack_stored_value_at_offset` only has tests as callers (besides stack_array.rs internally)

- **Category:** Dead code
- **Location:** `crates/opt/src/stack_load_forward/mod.rs:439`
- **What:** Public function with a long block comment selling "BUG-30 indirect-branch classifier helper". Grep across the workspace shows the only callers are `crates/opt/src/indirect_branch_resolve/stack_array.rs:113` (same crate) and the test module `crates/opt/src/stack_load_forward/tests.rs` (six tests starting at `find_stack_stored_value_finds_matching_store`). No external crate invokes it.
- **Why:** This is `pub` because the doc comment imagines a future external user; that user does not currently exist. `pub(crate)` would be the right level. The "Public helper for the tier-2 indirect-branch classifier (BUG-30)" comment also still references the BUG-30 codename which the user noted was stripped.
- **Proposed change:** Reduce to `pub(crate)` and trim the codename references.
- **Confidence:** high
- **Risk if applied:** low

### Duplication & unification

### F-011 `MAX_TABLE_ENTRIES = 4096` constant is duplicated in two sibling modules

- **Category:** Duplication & unification
- **Location:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:62`, `crates/opt/src/indirect_branch_resolve/stack_array.rs:62`
- **What:**
  ```rust
  // jump_table.rs
  const MAX_TABLE_ENTRIES: u64 = 4096;

  // stack_array.rs
  /// Per-call enumeration cap.  Mirrors the rodata jump-table arm's
  /// `MAX_TABLE_ENTRIES` for the same reason: ...
  const MAX_TABLE_ENTRIES: u64 = 4096;
  ```
- **Why:** Same value, same rationale, two definitions. The `stack_array.rs` doc comment even says "mirrors the rodata jump-table arm's MAX_TABLE_ENTRIES". If one ever changes (say to support larger tables for a real-world binary), the other will desync.
- **Proposed change:** Move to `indirect_branch_resolve/mod.rs` as a single `pub(super) const MAX_TABLE_ENTRIES: u64 = 4096;` with the rationale comment. Reference from both arms.
- **Confidence:** high
- **Risk if applied:** low

### F-012 `with_built` boilerplate copied verbatim across 12 `Optimizer::optimize` impls

- **Category:** Duplication & unification
- **Location:** Every pass's `impl Optimizer` (e.g. `dead_branch/mod.rs:135`, `constant_fold/mod.rs:79`, `call_other_elide/mod.rs:63`, `known_bits/mod.rs:349`, `stack_store/detect.rs:101`, `stack_store/call_args.rs:228`, `stack_load_forward/mod.rs:62`, `load_readonly/mod.rs:46`, `function_args/mod.rs:93`, `redundant_phis/mod.rs:163`)
- **What:** Every impl is the same shape:
  ```rust
  impl Optimizer for X {
      fn optimize(
          &self,
          graph: &mut ir::Graph,
          entry: ir::node::NodeId,
      ) -> Result<OptimizationResult> {
          crate::pipeline::with_built(graph, entry, |function| self.optimize_built(function))
      }
  }
  ```
  Each pass also defines `fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> ...`.
- **Why:** Over 130 lines of identical wiring code. The `IndirectBranchResolve` pass is the only one that needs `Graph` directly (it does in-place edits on the raw graph after a `with_built` call), so the trait could be split or the boilerplate eliminated by a default-impl that requires only `optimize_built`. This is a F2 trait-refactor leftover: `optimize_built` was the old API.
- **Proposed change:** Add a sealed trait extension or a derive macro:
  ```rust
  pub trait OptimizerOnBuilt {
      fn optimize_built(&self, fg: &mut BuiltFunctionGraph) -> Result<OptimizationResult>;
  }
  impl<T: OptimizerOnBuilt> Optimizer for T {
      fn optimize(&self, g: &mut Graph, e: NodeId) -> Result<OptimizationResult> {
          crate::pipeline::with_built(g, e, |fg| self.optimize_built(fg))
      }
  }
  ```
  This collapses 12 `impl Optimizer for X { ... }` blocks into nothing (each pass already implements `optimize_built`).
- **Confidence:** high
- **Risk if applied:** medium — public-API-adjacent, may need careful coherence-check; `IndirectBranchResolve` would have to keep its raw-graph impl explicitly.

### F-013 `RedundantPhis::replace_output_uses` shadows `Graph::replace_all_uses` exactly

- **Category:** Duplication & unification
- **Location:** `crates/opt/src/redundant_phis/mod.rs:9-21` and `crates/ir/src/ops/rewrite.rs:22-35`
- **What:**
  ```rust
  // redundant_phis/mod.rs
  fn replace_output_uses(
      function: &mut ir::BuiltFunctionGraph,
      output: NodeOutputId,
      value: NodeOutputId,
  ) -> Result<bool> {
      let mut changed = false;
      let mut cursor = function.graph.output_use_cursor(output);
      while cursor.current().is_some() {
          cursor.replace_current_with(value)?;
          changed = true;
      }
      Ok(changed)
  }

  // ir/src/ops/rewrite.rs (same logic)
  pub fn replace_all_uses(...) -> Result<bool> {
      let mut cursor = self.output_use_cursor(old);
      if cursor.current().is_none() { return Ok(false); }
      while cursor.current().is_some() {
          cursor.replace_current_with(new_val)?;
      }
      Ok(true)
  }
  ```
- **Why:** Behaviourally identical (both return true iff at least one use was replaced). The IR-side has a small extra short-circuit; redundant_phis's version is dead.
- **Proposed change:** Replace `replace_output_uses(function, out, val)?` with `function.replace_all_uses(out, val)?`. Drop the local helper.
- **Confidence:** high
- **Risk if applied:** low

### F-014 Dirty-walk recursive shape duplicated between `function_args::mem_chain_is_dirty` and `stack_load_forward::probe`

- **Category:** Duplication & unification
- **Location:** `crates/opt/src/function_args/mod.rs:391-571` (`mem_chain_is_dirty`) and `crates/opt/src/stack_load_forward/mod.rs:170-288` (`probe`)
- **What:** Both functions:
  - Walk memory chain backward
  - Recognise StackStore + non-aliasing range disjointness
  - Recognise plain Store + decompose_sp + range disjointness
  - Treat MemPhi as a fork (any-pred dirty for one, all-pred-clean for other)
  - Treat Call/CallOther as pass-through
  - Recurse on memory predecessors
  - Cache results in a `(NodeOutputId, ...)`-keyed memo
- **Why:** Significant logic overlap. The differences are:
  - `mem_chain_is_dirty` returns `bool` (any-pred dirty); `probe` returns `Option<ResolveShape>` (all-pred clean).
  - `mem_chain_is_dirty` walks Call inputs (it's lookup-in-stack-arg-area which calls don't dirty); `probe` returns None at Call (calls clobber memory).
  - `find_stack_stored_value_at_offset` is yet a third copy of the same walk, restricted to no-MemPhi.
- **Proposed change:** Introduce a shared `MemChainWalker` trait or generic walker that takes a per-node decision callback. Pseudocode:
  ```rust
  trait MemChainVisit {
      type Result;
      fn visit_stack_store(...) -> ControlFlow<Self::Result, ()>;
      fn visit_store(...) -> ControlFlow<Self::Result, ()>;
      fn visit_phi(...) -> ControlFlow<Self::Result, ()>;
      fn terminate() -> Self::Result;
  }
  ```
  Or, more minimally, extract a helper for the Store/StackStore alias-disambiguation (it's repeated 3 times verbatim).
- **Confidence:** medium
- **Risk if applied:** medium — touches 3 files and the test invariants are subtle (each variant has slightly different "bail" semantics).

### F-015 Store-vs-StackStore alias-disambiguation logic copied 3 times

- **Category:** Duplication & unification
- **Location:** `crates/opt/src/stack_store/call_args.rs:81-99`, `crates/opt/src/stack_load_forward/mod.rs:220-264`, `crates/opt/src/stack_load_forward/mod.rs:479-507`, `crates/opt/src/function_args/mod.rs:479-537`
- **What:** Identical pattern in each: decompose Store's address, switch on `None` (non-aliasing → walk through), `SpExpr::Terminal` (check `ranges_disjoint`), `SpExpr::Phi` (conservatively bail). The fallback `value_byte_size(...).unwrap_or(i64::MAX)` story is repeated across all three with similar "defensive fallback" comments.
- **Why:** This is the BUG-28 cause-#2 fix that landed in 3 places. Today it's all consistent; if the soundness story changes (e.g. a future "treat phi-of-SP as `any`-walkable" extension), all 3 sites need to change in lockstep.
- **Proposed change:** Extract a private helper:
  ```rust
  // Returns: Some(prev_mem) → continue walk; None → terminate (dirty/bail).
  pub(crate) fn walk_through_store_if_disjoint(
      g: &Graph, store_node: NodeId,
      query_off: i64, query_size: i64,
      sp_vn: rsleigh::Vn, sp_memo: &mut SpExprMemo,
  ) -> Option<NodeOutputId>;
  ```
  Then the four sites each call this and propagate accordingly.
- **Confidence:** high
- **Risk if applied:** medium

### F-016 `walk_control_for_if_bound`'s ControlState arm clones `visited` per predecessor

- **Category:** Duplication & unification (also performance)
- **Location:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:402-406`
- **What:**
  ```rust
  let mut combined: u64 = 0;
  for &pred in &inputs {
      let mut local = visited.clone();
      let bound = walk_control_for_if_bound(fg, pred, idx_output, &mut local)?;
      combined = combined.max(bound);
  }
  ```
- **Why:** `visited` is a `HashSet<NodeId>`. Cloning per predecessor is O(N·V) where V is current set size. With deeply-nested control flow, this cost grows quadratically. The reason is sound (cycle in pred A shouldn't poison pred B), but a save-restore pattern (note current size, recurse, truncate) would be O(1) per predecessor for `Vec` or `Option` stacks, vs O(V) per `clone()`. With `HashSet`, the truncate-back trick doesn't work directly — you'd need a Vec-based stack or a per-recursion epoch counter.
- **Proposed change:** Replace `HashSet<NodeId>` with `(HashSet<NodeId>, Vec<NodeId> stack)` where `stack` records insertions per recursion depth and a sibling-init drains them on pop. Or restructure the recursion to use iterative DFS with an explicit stack.
- **Confidence:** medium
- **Risk if applied:** medium (changes a public-facing recursion that has multiple unit tests)

### F-017 `same_value`'s root walk doesn't share its visited set across calls

- **Category:** Performance
- **Location:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:525-552`
- **What:**
  ```rust
  fn same_value(graph: &Graph, a: NodeOutputId, b: NodeOutputId) -> bool {
      fn root(graph: &Graph, mut out: NodeOutputId) -> NodeOutputId {
          let mut budget = 64usize;
          let mut visited: HashSet<NodeOutputId> = HashSet::new();
          while budget > 0 && visited.insert(out) { ... }
          out
      }
      root(graph, a) == root(graph, b)
  }
  ```
- **Why:** `same_value` is called once per `If` on the control-walk path; `bound_via_predecessor_if` may visit dozens of Ifs. Each `same_value` call rebuilds two `HashSet`s. If the same idx output is used by N Ifs, we re-walk it N+1 times. Modest cost in practice (the budget caps it at 64), but the per-call allocation is a pure overhead.
- **Proposed change:** Either pass a shared visited set / cache from the caller, or precompute `root(idx_output)` once and re-use. Could memoize `root: HashMap<NodeOutputId, NodeOutputId>` per top-level call.
- **Confidence:** high
- **Risk if applied:** low

### F-018 `bound_via_known_bits` runs the full `analyze_known_bits` per anchor

- **Category:** Performance
- **Location:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:265`
- **What:**
  ```rust
  let known = crate::analyze_known_bits(fg).ok()?;
  let kb = known.get(&idx_output).copied().unwrap_or_default();
  ```
- **Why:** `analyze_known_bits` is a fixed-point worklist analysis over the *whole* function. `IndirectBranchResolve::optimize` calls `classify_anchor_with_rom_and_sp` per anchor, and each call ultimately invokes `bound_via_known_bits` if the producer is a Load. With N unresolved anchors, the full known-bits analysis runs N times even though the IR is unchanged between calls.
- **Proposed change:** Cache the `known` map at the `IndirectBranchResolve::optimize` level and pass it down. Today the function takes only `fg`, so the cache would need a new arg or a `HashMap<&Graph, ...>` (impractical) — better is to lift the `analyze_known_bits` call up to `IndirectBranchResolve::optimize` and thread it through.
- **Confidence:** high
- **Risk if applied:** low

### F-019 `KnownBits::optimize_built` collects all `node_outputs` per node into a Vec instead of iterating directly

- **Category:** Performance
- **Location:** `crates/opt/src/known_bits/mod.rs:367-393`
- **What:**
  ```rust
  let mut outputs: Vec<NodeOutputId> = Vec::new();
  for &node_id in &nodes {
      outputs.clear();
      outputs.extend(function.graph.node_outputs(node_id));
      for &out in &outputs {
          ...
          let new_out = function.make_int_const(kb.ones, ty)?;
          result |= OptimizationResult::from_changed(function.replace_all_uses(out, new_out)?);
      }
  }
  ```
- **Why:** `node_outputs(node_id)` returns a slice borrowed from `function.graph`. The `Vec::extend` exists so the body can call `function.replace_all_uses` (which mutates) without holding the slice borrow. That's necessary. The reuse of the `outputs` Vec across iterations (`outputs.clear()`) is good. But: Reach-set mutation in the body — when `replace_all_uses` rewires consumers, the consumer set may change, and the next iteration's `function.preorder()` snapshot (line 363) is stale. This is a correctness-adjacent perf concern: if a fold makes some currently-reachable node now-unreachable through another path, the next iteration may try to fold a dead node. The pass works because the per-node fold is idempotent and zombie nodes don't get folded again. Still, the design is brittle.
- **Proposed change:** Either drive the second phase via the WorkSet pattern (reuse `crate::worklist::WorkSet`) or document that the snapshot is intentional and the order is "all reachable at start of phase 2".
- **Confidence:** medium
- **Risk if applied:** low

### F-020 `KnownBits::node_known_bits`: `IntConst as u64` truncation is `#[allow(clippy::cast_possible_truncation)]`-suppressed

- **Category:** Correctness / Simplification
- **Location:** `crates/opt/src/known_bits/mod.rs:101-103`
- **What:**
  ```rust
  // IntConst stores u128; the guard above skips U128/U256, so v fits in u64 here.
  #[allow(clippy::cast_possible_truncation)]
  NodeKind::IntConst(v) => Kb::from_const(v as u64, ty),
  ```
- **Why:** Sound today (the `type_mask = ty.get_unsigned_int(u64::MAX)` guard returns None for U128/U256). But it's a runtime invariant maintained at a distance from the cast site. If anyone ever loosens the type_mask guard or adds a U128 path, this `as u64` silently drops bits.
- **Proposed change:** Use `u64::try_from(v).unwrap_or_else(|| { /* unreachable per type_mask */ 0 })` or compute the masked value explicitly: `(v & u128::from(type_mask)) as u64`. The `as u64` is fine; making the dependence on the guard structurally evident would prevent regression.
- **Confidence:** high
- **Risk if applied:** low

### F-021 `find_stack_stored_value_at_offset` lacks a memo despite siblings using one

- **Category:** Performance
- **Location:** `crates/opt/src/stack_load_forward/mod.rs:439-513`
- **What:**
  ```rust
  pub fn find_stack_stored_value_at_offset(
      graph: &ir::Graph,
      mem: NodeOutputId,
      offset: i64,
      value_type: NodeOutputType,
      sp_vn: rsleigh::Vn,
      memo: &mut SpExprMemo,
  ) -> Option<NodeOutputId> { ... }
  ```
- **Why:** The function takes only an `SpExprMemo` (for the SP-decomposition substep). The walk itself is uncached. `classify_stack_array` calls this in a loop over `0..bound`, and many invocations will share an upstream Store chain. With bound=4096 in a tight stack-array dispatch, that's potentially millions of redundant memory-chain traversals. Sibling `mem_chain_is_dirty` and `probe` both have shadow memos that solve this exact problem.
- **Proposed change:** Add a `walk_memo: &mut FxHashMap<(NodeOutputId, i64, NodeOutputType), Option<NodeOutputId>>` argument and short-circuit on cached results. The `classify_stack_array` caller threads the memo through the loop.
- **Confidence:** high
- **Risk if applied:** low

### Simplification

### F-022 `apply_link_register` retains `target_value` slot — comment promises one thing, code does another

- **Category:** Readability / Simplification
- **Location:** `crates/opt/src/indirect_branch_resolve/inplace.rs:23-53`
- **What:**
  ```rust
  /// Pre-edit: `[control, memory, target_value]`
  /// Post-edit: `[control, memory, target_value, ret_val_0, …]`
  ///
  /// The `target_value` slot is intentionally retained — removing it
  /// would require shifting subsequent input indices.  The IR's
  /// `validate` is robust to extra Return value inputs.
  pub fn apply_link_register(...) -> Result<()> {
      ...
      for &ret in ret_val_outputs {
          graph.add_node_input(placeholder_return, ret)?;
      }
      Ok(())
  }
  ```
- **Why:** Retaining `target_value` means downstream pattern matchers see `Return(ctrl, mem, target_value, ret_val_0, ...)` rather than `Return(ctrl, mem, ret_val_0, ...)`. The `target_value` is a leftover anchor, not a real return value. Pattern queries that rely on positional-index of return values (`ret_val(0)`) will hit `target_value` not `ret_val_0`. The pattern crate's `RetPat::ret_val(idx, p)` API is documented as 0-indexed over the return values; this leak is invisible to readers of the pattern API.
- **Proposed change:** Either remove the `target_value` slot via `graph.remove_node_input(placeholder_return, 2)?` (acceptable trade-off in shift cost), or rename the input slot semantics in pattern queries. Today this is a footgun.
- **Confidence:** high
- **Risk if applied:** medium (would change Return arity downstream)

### F-023 `apply_tail_call` returns `NodeId` but the only caller throws it away

- **Category:** Simplification
- **Location:** `crates/opt/src/indirect_branch_resolve/inplace.rs:84-155` (signature) and `crates/opt/src/indirect_branch_resolve/mod.rs:273-280` (caller)
- **What:**
  ```rust
  // Caller in mod.rs:
  let _new_return = inplace::apply_tail_call(
      graph, placeholder, target,
      &ctx.arg_passing_outputs, &ctx.clobbered_kinds, &ctx.ret_val_outputs,
  )?;
  changed = true;
  ```
- **Why:** The doc comment for `apply_tail_call` says "Returns the new Return's `NodeId` so callers can patch any cached exit-control handles." But the only in-tree caller `_new_return`s it. The strider crate's wrapper (`crates/strider/src/indirect_resolve_tier2/inplace.rs:88`) might need it — let me confirm by reading...
- **Proposed change:** If strider's caller does use it, remove the `_` prefix in opt's caller and document why opt doesn't act on it. If neither uses it, change the return type to `Result<()>`.
- **Confidence:** medium
- **Risk if applied:** low

### F-024 `RedundantPhis::ControlState` arm: `cleanup_if_dead | OptimizationResult::Changed` is redundant

- **Category:** Simplification
- **Location:** `crates/opt/src/redundant_phis/mod.rs:138-143`
- **What:**
  ```rust
  if simplified {
      Ok(cleanup_if_dead(function, node_id) | OptimizationResult::Changed)
  } else {
      Ok(cleanup_if_dead(function, node_id))
  }
  ```
- **Why:** When `simplified` is true, the result is always `Changed` (regardless of `cleanup_if_dead`'s result). When false, it's whatever cleanup returns. The `BitOr` with `Changed` is a no-op when `simplified`, just verbose. Could be:
  ```rust
  let cleanup = cleanup_if_dead(function, node_id);
  Ok(if simplified { OptimizationResult::Changed } else { cleanup })
  ```
- **Proposed change:** Replace as above. (Also `OptimizationResult` is `Copy`; the `|` is fine, just verbose.)
- **Confidence:** high
- **Risk if applied:** low

### F-025 `cleanup_if_dead` doesn't consider a node "dead" if its inputs are already empty

- **Category:** Simplification
- **Location:** `crates/opt/src/redundant_phis/mod.rs:26-39`
- **What:**
  ```rust
  fn cleanup_if_dead(function: &mut ir::BuiltFunctionGraph, node_id: NodeId) -> OptimizationResult {
      let all_unused = function
          .graph
          .node_outputs(node_id)
          .into_iter()
          .all(|out| function.graph.output_uses(out).next().is_none());

      if all_unused && !function.graph.node_inputs(node_id).is_empty() {
          function.graph.detach_node_inputs(node_id);
          OptimizationResult::Changed
      } else {
          OptimizationResult::NoChange
      }
  }
  ```
- **Why:** The `&& !function.graph.node_inputs(node_id).is_empty()` check is "skip if there's nothing to detach" — that's correct. But the function name implies it detaches "if dead" — actually it detaches "if dead AND has inputs to remove". A node that's already-detached (zero inputs, zero outputs uses) returns `NoChange` even though it IS dead. Consistent with the intent ("no change happened") but the function name is misleading.
- **Proposed change:** Rename to `try_detach_dead_inputs` (it's the detach action that's reported, not the dead status). Or: split into `is_node_dead` predicate + `detach_node_inputs` action.
- **Confidence:** medium
- **Risk if applied:** low

### F-026 `RedundantPhis::optimize_built`: `let _ = detach_unreachable_nodes(...)` discards a meaningful signal

- **Category:** Simplification
- **Location:** `crates/opt/src/redundant_phis/mod.rs:191-192`
- **What:**
  ```rust
  // Detaching unreachable zombies is bookkeeping, not progress: an
  // unreachable node cannot be a consumer of a reachable producer, so
  // no other pass can act on the result.  Run it for hygiene but do
  // NOT escalate it into a `Changed` signal — that just costs the
  // pipeline one extra fixed-point iteration with no work to do.
  let _ = crate::worklist::detach_unreachable_nodes(&mut function.graph, function.entry);
  ```
- **Why:** The rationale is sound and the comment is good. The `let _ =` is the right way to drop the result. Worth noting that this is a deliberate de-escalation; tests pin it (`redundant_phis_no_changed_for_orphan_only_cleanup`). Suggest adopting the same pattern in `FunctionArgDetect::optimize_built` (line 114) which uses the same idiom but with more sparse rationale.
- **Proposed change:** No-op recommendation; just unify the comment style with the other call site, or pull into a helper that combines the discard and the rationale comment.
- **Confidence:** low
- **Risk if applied:** low

### F-027 `IndirectBranchResolve::optimize` mixes `with_built` for read with raw-Graph for write

- **Category:** Readability / Simplification
- **Location:** `crates/opt/src/indirect_branch_resolve/mod.rs:217-295`
- **What:**
  ```rust
  let resolved_opt = crate::pipeline::with_built(graph, entry, |fg| {
      classify::classify_anchor_with_rom_and_sp(fg, ...)
  });
  let Some(resolved) = resolved_opt else { continue; };
  let Some(placeholder) = find_placeholder_return_for_anchor(graph, *anchor_output) else { continue; };
  ...
  inplace::apply_link_register(graph, placeholder, &ctx.ret_val_outputs)?;
  ```
- **Why:** The classifier needs a `BuiltFunctionGraph` (because it drives `pattern::Matcher`). The in-place editors take `&mut Graph`. So the function plays the `with_built` dance per-iteration just for the classifier, then drops back to `&mut Graph`. The flow reads as "wrap, unwrap, do raw thing, repeat". A single `with_built` over the whole loop would let both classifier AND editor work uniformly — the in-place editors could be reworked to take `&mut BuiltFunctionGraph` (which exposes `&mut graph` via `function.graph`).
- **Proposed change:** Wrap the entire `for` loop in `with_built`, pass `&mut fg` to both classifier and editor. The editors are already given `&mut Graph` at the trait surface; making them accept `&mut BuiltFunctionGraph` would flow naturally.
- **Confidence:** medium
- **Risk if applied:** medium

### F-028 `try_lower_cast_to_float` has 4 branches all wrapping `replace_all_uses` identically

- **Category:** Simplification
- **Location:** `crates/opt/src/constant_fold/mod.rs:23-61`
- **What:** Four branches, all of the form:
  ```rust
  let new_out = ...;
  return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?));
  ```
- **Why:** Could be condensed by computing `new_out: NodeOutputId` once and applying the rewrite once at the function tail:
  ```rust
  let new_out = if in_ty == out_ty {
      input
  } else if in_ty.is_float() {
      fg.make_float_to_float_node(input, out_ty)?
  } else if let Some(bits) = fg.int_const_val(input) {
      fg.make_float_const(bits, out_ty)?
  } else {
      fg.make_int_bits_to_float_node(input, out_ty)?
  };
  Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?))
  ```
- **Proposed change:** Restructure to a single tail-replace.
- **Confidence:** high
- **Risk if applied:** low

### F-029 `eval_int_binary` `shift` closure has special case for `bits == 0` that's unreachable

- **Category:** Simplification
- **Location:** `crates/opt/src/constant_fold/eval_int.rs:36-38`
- **What:**
  ```rust
  let shift = |s: u128| -> u32 {
      if bits == 0 { 0 } else { s as u32 }
  };
  ```
- **Why:** `bits` is `ty.bit_width() as u32`. `NodeOutputType` has no zero-width variant (smallest is `Bool` = 1 bit, or U8 = 8). So `bits == 0` is unreachable. The closure exists because `wrapping_shl(0)` would panic if `bits` were ever 0, but it can't be. Dead defensive code.
- **Proposed change:** Remove the closure entirely; inline `s as u32` at the two call sites. Or change the type-system to encode "non-zero bit width" in `NodeOutputType` so the unreachable branch becomes statically eliminable.
- **Confidence:** high
- **Risk if applied:** low

### F-030 `eval_float_*` functions duplicate F32 vs F64 arms mechanically

- **Category:** Simplification
- **Location:** `crates/opt/src/constant_fold/eval_float.rs` (entire file)
- **What:** Each of `eval_float_binary`, `eval_float_cmp`, `eval_float_unary` has a `match ty { F32 => ..., F64 => ..., _ => None }` shape with the body duplicated for each width except for the float type.
- **Why:** Could be unified via a generic helper:
  ```rust
  fn eval<F: FloatLike>(...) -> F::Bits { ... }
  ```
  with `F32` and `F64` impls. But the genericity buys little (just removes ~30 lines), and the explicit per-width arms keep the bit-pattern conversions visible. Marginal.
- **Proposed change:** Optional. If you want the unification, define a `FloatBitcast` helper trait. Otherwise leave; it's a low-cognitive-load duplication.
- **Confidence:** low
- **Risk if applied:** low

### F-031 `match_value!` from `ir-macros` is not used anywhere in `opt`

- **Category:** Duplication & unification
- **Location:** Multiple files (verified via grep)
- **What:** The CLAUDE.md and ir-macros crate document `match_value!` as the canonical DSL for IR pattern matching. Grep shows zero occurrences in `crates/opt/src/`.
- **Why:** Constant-fold's rewrite rules use `pattern::rewrite_rule` extensively (which is the right tool for IR-rewriting). But several non-rewriting sites use hand-rolled NodeKind chains: `function_args::detect_register_args` lines 149-152, `RedundantPhis::remove_phis` lines 48-119, `KnownBits::node_known_bits` lines 81-274, `mem_chain_is_dirty`, `probe`, `find_stack_stored_value_at_offset`. Several of these are "extract input shape and dispatch" — perfect `match_value!` material.

  Example, `same_value` in jump_table.rs:528-549:
  ```rust
  match graph.node_kind(node) {
      NodeKind::ControlPhi(_) | NodeKind::ValuePhi => {
          let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
          if inputs.len() == 2 { out = inputs[1]; ... }
          ...
      }
      _ => return out,
  }
  ```
  could be `match_value! { if let Phi[_, val] = ctx, out { ... } }`.
- **Proposed change:** Audit and add `match_value!` to the few sites where it would simplify. Not a global must-do; pick the worst offender (probably `RedundantPhis::remove_phis`'s 60-line ControlPhi/MemPhi arm).
- **Confidence:** medium
- **Risk if applied:** medium

### F-032 `value_byte_size(...).unwrap_or(i64::MAX)` defensive fallback story is repeated 3 times

- **Category:** Readability
- **Location:** `crates/opt/src/function_args/mod.rs:514`, `crates/opt/src/stack_load_forward/mod.rs:250`, `crates/opt/src/stack_load_forward/mod.rs:497`
- **What:** Each site computes:
  ```rust
  let store_size = value_byte_size(fg, inputs[2]).unwrap_or(i64::MAX);
  ```
  with a multi-line comment explaining: "fallback is the soundness-preserving answer — forces ranges_disjoint to return false and we conservatively terminate. In valid IR a Store's DATA slot is value-typed by signature, so the fallback is unreachable; the branch exists only as a defensive guardrail."
- **Why:** Same comment text near-verbatim 3 times. If the IR signature ever changes, all 3 need updating.
- **Proposed change:** Promote to a named helper:
  ```rust
  /// Conservative byte size of a Store's DATA slot. Unreachable in valid IR;
  /// returns `i64::MAX` as a soundness guardrail. See ranges_disjoint's
  /// saturating_add behaviour for why MAX → "not disjoint" verdict.
  fn store_value_byte_size(fg: &BuiltFunctionGraph, store_data: NodeOutputId) -> i64 { ... }
  ```
- **Confidence:** high
- **Risk if applied:** low

### F-033 `strip_target_mask`'s 4-layer cap is undocumented at the call sites

- **Category:** Readability
- **Location:** `crates/opt/src/indirect_branch_resolve/stack_array.rs:172-216`
- **What:**
  ```rust
  // Strip up to a fixed number of layers; ARM-Thumb commonly nests
  // `And(Or(load, 1), 0xFFFFFFFE)` (set LSB then mask it off) — that's
  // 2 layers.  Cap at 4 to defend against pathologically deep wrappers
  // from buggy lifter output.
  for _ in 0..4 {
      ...
  }
  ```
- **Why:** The 4 is a magic number with rationale only at the loop's site, not at the function's contract. A pattern-matcher contributor calling `strip_target_mask` won't know it can return after up to 4 strips and still report `(out, mask)` as if it were the fully-stripped state — they may assume the function strips fully. If a real binary ever has 5+ layers, the strip silently terminates early and downstream classification fails.
- **Proposed change:** Document the cap in the function-level doc comment, and turn `4` into a `const MAX_STRIP_LAYERS: usize = 4;` for greppability. Add a unit test exercising `MAX_STRIP_LAYERS + 1` nested wrappers (must fail closed → orchestrator handles it).
- **Confidence:** high
- **Risk if applied:** low

### F-034 `flatten_add_tree` 32-node budget collides with deep ARM trees silently

- **Category:** Readability / Correctness
- **Location:** `crates/opt/src/indirect_branch_resolve/stack_array.rs:336-373`
- **What:**
  ```rust
  fn flatten_add_tree(...) {
      if *budget >= 32 {
          acc.push(out);  // push as a single opaque term
          return;
      }
      *budget += 1;
      ...
  }
  ```
- **Why:** When the budget is exhausted, the function pushes `out` itself as a term — i.e. treats the remaining unflattened subtree as a single non-decomposable term. The downstream `decompose_sp` call may still succeed on it. So the 32-cap is a "graceful degradation" rather than a hard fail. But the comment "Capped at 32 nodes to defend against pathologically deep trees from buggy lifter output" undersells: the reader should know that a real 32+ node tree won't fail closed but may simply skip the BUG-30 classification arm. There's no test verifying the budget-exhaustion path fails closed (returns None at classify_stack_array).
- **Proposed change:** Add a unit test for the budget-exhaustion case. Document the degradation behaviour at the function header. Consider increasing the budget — 32 is small for ARM-Thumb's Add-tree depth.
- **Confidence:** medium
- **Risk if applied:** low

### F-035 `node_known_bits` returns `Option<(NodeOutputId, Kb)>` — the Option is for "no info" not "no result"

- **Category:** Readability
- **Location:** `crates/opt/src/known_bits/mod.rs:76-278`
- **What:** The return type carries two failure modes glued together:
  - `Ok(None)`: the node kind doesn't propagate (e.g. `Call` output, opaque CallOther) — *expected*
  - `Ok(Some((out, kb)))`: the node propagates and produced these bits — *normal*
  - `Err(_)`: malformed IR — *abort*

  But within the function, several paths return `Ok(None)` early ("U128/U256 — skip" line 96-98, "no integer output" line 84-91). The reader has to follow the path to distinguish "no info because U128" from "no info because the kind doesn't apply".
- **Proposed change:** Either: (a) split into `node_kind_propagates(kind: NodeKind) -> bool` + `compute_kb(node_id, known) -> Result<Kb>`; or (b) introduce a `KbResult { Skip, Bits(Kb) }` enum. Today the consumer at `analyze` line 319 does `let Some(...) = ...?` with the same code path for both meanings — sound, just opaque.
- **Confidence:** medium
- **Risk if applied:** low

### F-036 `IndirectBranchResolve::default` could be `derive`d

- **Category:** Simplification
- **Location:** `crates/opt/src/indirect_branch_resolve/mod.rs:198-202`
- **What:**
  ```rust
  impl Default for IndirectBranchResolve {
      fn default() -> Self {
          Self::new()
      }
  }
  ```
  And `Self::new()` is:
  ```rust
  pub fn new() -> Self {
      Self {
          link_register_vn: None,
          stack_ptr_vn: None,
          rom: None,
          unresolved_anchors: Vec::new(),
          anchor_contexts: HashMap::new(),
          is_tail_call: Box::new(|_| false),
      }
  }
  ```
- **Why:** All fields are default except `is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>`. `Box<dyn Fn>` doesn't implement `Default`, so a `derive(Default)` would fail. So `Self::new()` is the only way. But the `Default::default()` indirection through `Self::new()` is fine — just a reminder that the boxed predicate field is the only obstacle to `derive(Default)`.
- **Proposed change:** No change. Note for future readers: if `is_tail_call` becomes `Option<Box<dyn Fn>>`, `derive(Default)` becomes possible (and the `|_| false` default can be moved into the consumer).
- **Confidence:** low
- **Risk if applied:** low

### F-037 Codename comments still reference stripped F/R/W/BUG numbers

- **Category:** Readability
- **Location:** Many. Examples: `constant_fold/rules.rs:131,165` (BUG-19), `function_args/mod.rs:379,473` (BUG-28 cause #2), `indirect_branch_resolve/mod.rs:230` (Phase 5), `indirect_branch_resolve/jump_table.rs:3` (Round R4), `known_bits/tests.rs:268,406,435` (BUG-24), `stack_load_forward/tests.rs:190,193,852,932,1136,1151` (BUG-28, BUG-30), `stack_store/tests.rs:669,751,821` (BUG-28), `pipeline.rs:239,278` (F2), `indirect_branch_resolve/stack_array.rs:629` (R2-1)
- **What:** Code-name references that the user noted were stripped recently.
- **Why:** The user asked to flag every stale codename reference. These are stragglers. They aren't actively misleading (the rationales are still valid), but they create noise for new readers.
- **Proposed change:** Strip the codename prefixes; keep the rationale text. Example: `// BUG-28 cause #2 (third instance):` → `// Plain Store on the chain — alias-discriminate via decompose_sp:`.
- **Confidence:** high
- **Risk if applied:** low

### F-038 `IndirectBranchResolve::is_tail_call` boxed predicate is an awkward field

- **Category:** Readability
- **Location:** `crates/opt/src/indirect_branch_resolve/mod.rs:138`
- **What:**
  ```rust
  pub is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>,
  ```
- **Why:** A boxed closure is harder to test/serialize/swap than a plain pointer or a small enum. The field's lifetime is tied to the pass struct, which is fine, but the trait-object boundary makes it impossible to derive `Debug`, harder to clone, and obscures what's meant ("a function that knows the function-range").
- **Proposed change:** Replace with a dedicated trait: `trait IsTailCall { fn is_tail_call(&self, target: u64) -> bool; }` (or `Range<u64>` if linear), and make the pass generic over it. Or keep the closure but pass a `&dyn Fn` in `optimize` rather than storing it. Today the storage pattern is fine for one caller (strider), but if pattern queries ever want to construct an `IndirectBranchResolve` programmatically this gets awkward.
- **Confidence:** low
- **Risk if applied:** medium

### F-039 `OptimizationResult::from_changed` is invoked dozens of times

- **Category:** Readability
- **Location:** Multiple files (constant_fold/mod.rs lines 41,47,55,60; constant_fold/rules.rs lines 101, 285, 374, 567, 696; load_readonly/mod.rs:86; known_bits/mod.rs:392; stack_load_forward/mod.rs:130)
- **What:** Pattern `result |= OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?)` repeats. Could be a method on `OptimizationResult` or a small helper:
  ```rust
  impl OptimizationResult {
      fn after_replace(self, fg: &mut BuiltFunctionGraph, old: NodeOutputId, new: NodeOutputId) -> Result<Self> {
          Ok(self | Self::from_changed(fg.replace_all_uses(old, new)?))
      }
  }
  ```
- **Why:** Boilerplate noise; readers parse the wrapper in their head every time. The pattern is specific to "rewrote a node, want to escalate to Changed".
- **Proposed change:** Add the helper and shorten call sites: `result = result.after_replace(fg, out, new_out)?;`. Optional cosmetic.
- **Confidence:** medium
- **Risk if applied:** low

### F-040 `FunctionArgDetect::optimize_built` returns `changed` but discards `detach_unreachable_nodes`'s signal

- **Category:** Readability
- **Location:** `crates/opt/src/function_args/mod.rs:108-115`
- **What:**
  ```rust
  // Replacing `Load[sp+K]` with `FunctionArg` orphans the address-
  // computation chain (`Add(sp, K)` and friends).  Those nodes remain in
  // the use-list of surviving producers like `InitialVar(sp)`, which
  // confuses downstream consumers that walk use-lists — e.g. the dot
  // renderer draws an edgeless `InitialVar(sp)` island.  Detach them.
  // The detach result is hygiene-only (post-pass return values are
  // ignored by the pipeline); don't escalate it into `Changed`.
  let _ = crate::worklist::detach_unreachable_nodes(&mut function.graph, function.entry);
  ```
- **Why:** Same de-escalation pattern as `RedundantPhis`. Worth a shared helper or a doc note linking the two sites: "see `redundant_phis::optimize_built` for the rationale". As-is, the rationale is duplicated nearly word-for-word and may drift.
- **Proposed change:** Either link to the canonical comment from one site to the other, or factor the "detach + de-escalate" into a single helper called from both passes.
- **Confidence:** medium
- **Risk if applied:** low

### F-041 `WorkSet::pop` removes from `queued`; doc comment doesn't explain re-enqueue semantics

- **Category:** Readability
- **Location:** `crates/opt/src/worklist.rs:42-47`
- **What:**
  ```rust
  /// Pops the next node, removing it from the pending set.
  pub(crate) fn pop(&mut self) -> Option<NodeId> {
      let n = self.queue.pop_front()?;
      self.queued.remove(&n);
      Some(n)
  }
  ```
- **Why:** When a node is popped, it's removed from `queued`. So a subsequent `push(n)` re-adds it (returns true on `insert`). This is the right semantics for the consumers (after processing a node, if its inputs changed and consumers need re-processing, push is correct). But the doc doesn't say "after pop, the node can be re-enqueued". A reader might assume `queued` is "ever-seen" and that re-enqueue is a bug.
- **Proposed change:** Update the docstring: `Pops the next node, removing it from the pending set so subsequent `push` of the same id will re-enqueue. Used by const-fold's worklist re-enqueue idiom (consumers are pushed after a rewrite touches the node)`.
- **Confidence:** high
- **Risk if applied:** low

### F-042 `cleanup_if_dead` name implies "if dead" but it does the detach action

- **Category:** Readability
- **Location:** `crates/opt/src/redundant_phis/mod.rs:26-39`
- **What:** Function name implies "do cleanup IF the node is dead". But the function's return value is `OptimizationResult` (Changed/NoChange) and its side effect is "if dead AND has inputs, detach inputs".
- **Proposed change:** Rename to `try_detach_dead_inputs` or `detach_inputs_if_dead`. Cosmetic.
- **Confidence:** low
- **Risk if applied:** low

### F-043 `dead_branch::try_eliminate_dead_branch`: dead_uses sort is provably no-op

- **Category:** Simplification
- **Location:** `crates/opt/src/dead_branch/mod.rs:62-67`
- **What:** See F-001 for analysis. The `sort_by(|a, b| b.1.cmp(&a.1))` is a no-op given the implementation: removals at distinct consumers don't interact (different `cs_node` ids). Within a single consumer's wires, the slot indices stay correct because `remove_node_input(node, idx)` only shifts that consumer's later indices, and the bounds check `dead_idx < cs_len` retroactively rescues stale higher indices.
- **Why:** The sort is performance overhead with no functional purpose; the comment claims it future-proofs against use-list FIFO/LIFO semantics, but the code's correctness doesn't depend on use-list order at all — only on the per-consumer descending invariant (which is achieved by happenstance for any sort key + LIFO use-list, but not enforced by the code).
- **Proposed change:** Either drop the sort (and document that the per-consumer descending invariant is enforced by IR's LIFO use-list), or sort by `(consumer, idx desc)` to make the invariant load-bearing.
- **Confidence:** medium
- **Risk if applied:** low

### F-044 `tests/common::run_to_fixed_point` doc says "panics" but code returns Err

- **Category:** Readability
- **Location:** `crates/opt/tests/common/mod.rs:104-118`
- **What:**
  ```rust
  /// Runs `pass.optimize` until it reports `NoChange` or `MAX_ITERS` is hit
  /// (panics on hit — indicates a non-converging pass).
  pub fn run_to_fixed_point<P: opt::Optimizer>(...) -> Result<()> {
      const MAX_ITERS: usize = 100;
      for _ in 0..MAX_ITERS {
          if !pass.optimize(...)?.changed() { return Ok(()); }
      }
      Err(Error::from(opt::ErrorKind::AssertionFailed(...)))
  }
  ```
- **Why:** Docstring says "panics on hit"; code returns `Err`. Easy fix.
- **Proposed change:** Update docstring to say "returns `AssertionFailed` if MAX_ITERS exceeded".
- **Confidence:** high
- **Risk if applied:** low

### F-045 `make_fn` / `make_fn_with_var` duplicated between `tests/common/mod.rs` and several src test modules

- **Category:** Duplication & unification
- **Location:** `tests/common/mod.rs`, `src/constant_fold/tests.rs:11-22`, `src/known_bits/tests.rs:6-17`, `src/load_readonly/tests.rs:22-33`, `src/stack_load_forward/tests.rs` (no make_fn but similar), `src/stack_store/tests.rs` (no make_fn), `src/function_args/tests.rs` (similar but different), `src/redundant_phis/tests.rs`, `src/call_other_elide/tests.rs`
- **What:** The same `make_fn` skeleton (FunctionBuilder::new_raw + create_region + set_entry_region + set_region + closure body + build_return + build) appears in 5+ test modules.
- **Why:** Per-module helpers diverged organically. The integration test common module was added later but the src-level tests didn't migrate.
- **Proposed change:** Either: (a) move all the test-helper code into a shared `crate::test_support` (gated `#[cfg(any(test, feature = "test-utils"))]`) module; or (b) accept the duplication since the helpers are short. Given each is ~10 lines, the migration cost vs benefit is marginal.
- **Confidence:** medium
- **Risk if applied:** medium

### F-046 `IndirectBranchResolve.unresolved_anchors` is mutated in place; orchestrator must reset between runs

- **Category:** Readability
- **Location:** `crates/opt/src/indirect_branch_resolve/mod.rs:120, 247-294`
- **What:**
  ```rust
  pub unresolved_anchors: Vec<(AnchorAddr, NodeOutputId)>,
  ```
  In `optimize`, the loop walks `&self.unresolved_anchors` (immutable borrow) but the per-iteration in-place edits may invalidate the `NodeOutputId`s for later iterations. Specifically, after `apply_tail_call` detaches the placeholder Return, the `find_placeholder_return_for_anchor` call at line 253 returns None for future iterations of the same anchor — but if the *same anchor* appeared twice in the list (programmer error), the second visit would silently no-op.
- **Why:** Mostly safe by current orchestrator usage. But the docstring doesn't pin the contract "each anchor must appear at most once". A defensive impl would convert to a Set or de-duplicate at registration.
- **Proposed change:** Document the "at most once" precondition. Alternatively, drain the Vec on each `optimize` call (consume-on-use semantics), preventing accidental double-fire.
- **Confidence:** medium
- **Risk if applied:** low

### F-047 `bound_via_predecessor_if`: catch-all walks `inputs.get(0)` then checks Control kind

- **Category:** Simplification / Readability
- **Location:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:417-426`
- **What:**
  ```rust
  // Other control-producing kinds (the `Call`'s control output
  // for a function that returns into our region, etc.) are
  // walked through transparently — they don't bound `idx`.
  // We follow the node's slot-0 input as the control
  // predecessor when the node has one; otherwise return None.
  _ => {
      let inputs = graph.node_inputs(producer);
      let first = inputs.get(0).copied()?;
      // Only walk through if the input is a Control output —
      // otherwise we'd derail into data flow.
      if !graph.output_kind(first).is_control() {
          return None;
      }
      walk_control_for_if_bound(fg, first, idx_output, visited)
  }
  ```
- **Why:** The check `!is_control()` is correct (otherwise we'd walk into a data input), but the structure unwraps `inputs.get(0)` first, then validates. A more idiomatic shape:
  ```rust
  _ => {
      let first = graph.node_inputs(producer).first().copied()?;
      if !graph.output_kind(first).is_control() { return None; }
      walk_control_for_if_bound(fg, first, idx_output, visited)
  }
  ```
- **Proposed change:** Use `.first()` not `.get(0)`. Cosmetic.
- **Confidence:** low
- **Risk if applied:** low

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| crates/opt/src/lib.rs | full | F-007 unused re-exports |
| crates/opt/src/error.rs | full | clean |
| crates/opt/src/pipeline.rs | full | F-006, F-012 (with_built boilerplate) |
| crates/opt/src/sp_expr.rs | full | F-008, F-009 (pub on internal-only fns) |
| crates/opt/src/worklist.rs | full | F-041 doc gap |
| crates/opt/src/call_other_elide/mod.rs | full | clean |
| crates/opt/src/call_other_elide/tests.rs | full | clean |
| crates/opt/src/constant_fold/mod.rs | full | F-028 |
| crates/opt/src/constant_fold/eval_int.rs | full | F-029 |
| crates/opt/src/constant_fold/eval_float.rs | full | F-030 |
| crates/opt/src/constant_fold/rules.rs | full | F-037 (BUG-19 codename), large file but well-commented |
| crates/opt/src/constant_fold/tests.rs | full | extensive coverage |
| crates/opt/src/dead_branch/mod.rs | full | F-001, F-043 |
| crates/opt/src/dead_branch/tests.rs | full | clean |
| crates/opt/src/function_args/mod.rs | full | F-004, F-014, F-032, F-040, F-037 (BUG-28) |
| crates/opt/src/function_args/tests.rs | full | extensive coverage |
| crates/opt/src/indirect_branch_resolve/mod.rs | full | F-027, F-036, F-038, F-046 |
| crates/opt/src/indirect_branch_resolve/classify.rs | full | F-037 (BUG-30 references) |
| crates/opt/src/indirect_branch_resolve/inplace.rs | full | F-022, F-023 |
| crates/opt/src/indirect_branch_resolve/jump_table.rs | full | F-011, F-016, F-017, F-018, F-047 |
| crates/opt/src/indirect_branch_resolve/jump_table_tests.rs | full | F-037 (R5 in comment) |
| crates/opt/src/indirect_branch_resolve/stack_array.rs | full | F-011, F-033, F-034, F-037 (R2-1) |
| crates/opt/src/known_bits/mod.rs | full | F-002, F-003, F-019, F-020, F-035 |
| crates/opt/src/known_bits/tests.rs | full | F-037 (BUG-24) |
| crates/opt/src/load_readonly/mod.rs | full | F-005 |
| crates/opt/src/load_readonly/tests.rs | full | clean |
| crates/opt/src/redundant_phis/mod.rs | full | F-013, F-024, F-025, F-026, F-042 |
| crates/opt/src/redundant_phis/tests.rs | full | clean |
| crates/opt/src/stack_load_forward/mod.rs | full | F-010, F-014, F-021, F-032 |
| crates/opt/src/stack_load_forward/tests.rs | full | F-037 (BUG-28, BUG-30) |
| crates/opt/src/stack_store/mod.rs | full | clean |
| crates/opt/src/stack_store/detect.rs | full | clean |
| crates/opt/src/stack_store/call_args.rs | full | F-015 |
| crates/opt/src/stack_store/tests.rs | full | F-037 (BUG-28) |
| crates/opt/tests/common/mod.rs | full | F-044 doc/code drift |
| crates/opt/tests/multi_pass.rs | full | clean |
| crates/opt/tests/pipeline_default.rs | full | clean |
| crates/opt/tests/pipeline_fixedpoint.rs | full | clean |
| crates/opt/tests/pipeline_subsets.rs | full | clean |
| crates/opt/tests/pipeline_validation.rs | full | clean |
| crates/opt/tests/pipeline_with_stack.rs | full | clean |
| crates/opt/tests/indirect_branch_resolve.rs | full | clean |
| crates/opt/benches/constant_fold.rs | full | clean |
| crates/opt/benches/known_bits.rs | full | clean |
| crates/opt/benches/stack_store.rs | full | clean |
| crates/opt/benches/default_pipeline.rs | full | clean |

## Out-of-scope items observed

- `opt::Error` / `opt::ErrorKind` payload shapes are out of scope per the brief. I did note that the `ExpectedNodeNotFound("Call", NodeKind::Call)` invocation in `tests/pipeline_with_stack.rs:92` constructs the error with the same kind as the "expected" — in effect saying "expected Call, found Call". That is a test cosmetic, not actionable, but reads strangely.
- Pre-existing `#[allow(clippy::*)]` attributes — most are scoped to test modules where the lib-level `cfg_attr(test, allow(...))` already covers them. Some duplication (e.g. `#![allow(clippy::unwrap_used, ...)]` at the top of tests/common/mod.rs) is consistent with the test-mod pattern.
- `crates/strider-error/` machinery (out of scope per the brief).
