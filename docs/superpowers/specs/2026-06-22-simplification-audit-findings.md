# Workspace simplification audit — findings

Date: 2026-06-22
Branch: `feature/simplification-audit` (off `develop`)
Method: ponytail-audit lens, every production line of all 14 crates read by per-file agents (test code excluded). Each finding screened against three hard standards: **O-safe** (no Big-O regression), **readability not worsened**, **correctness preserved within/across crates + the lifting pipeline**.

The codebase is mature and largely lean — most crates returned few or no findings. The wins below are real but incremental (~-300–400 production LOC if all of Tier 1 is taken). Tier 2 holds the largest single cuts but each needs a verification spike or a readability judgment call before coding. Tier 3 is rejected, with reasons.

---

## Tier 1 — Clear wins (dead code / strict improvements / safe stdlib idioms)

All three standards pass on inspection; these are deletions, single-caller inlines, or idiom swaps with no behavioral change.

### Dead code (delete)
- **`InputSuccs` struct + `GraphRef` impl, and `raw_def_use_succs` / `def_use_succs` free fns** — zero workspace callers (grep-confirmed); the two free fns are file-local one-liners inlinable into their `try_successors` sites. `strider-ir/src/walk/mod.rs:135-189`. ~-33 LOC.
- **`ControlFlowView` `NodeCount` + `IntoNodeIdentifiers` petgraph impls** (+ `node_identifiers` / `control_nodes`) — `petgraph::algo::dominators::simple_fast` requires only `IntoNeighbors + Visitable`; these impls are speculative and uncalled (one even allocates an O(N) Vec). `strider-ir/src/control_flow_view.rs:66-79`. ~-14 LOC.
- **`const_value` + `ConstValue` enum, and `get_as_unsigned_int` / `get_as_signed_int` / `get_as_int`** — dead outside `strider-ir` (grep-confirmed; the two builder_ext callers inline trivially to the `int_const_*` primitives). `strider-ir/src/viewer.rs:27-39,534-588`. ~-50 LOC. *(VERIFY: confirm no strider-py FFI path before deleting — both agents found none.)*
- **`Clone for Box<dyn PostOptimizer>`** — never invoked; all cloning goes through `clone_box()`. `strider-opt/src/pipeline.rs:397-401`. ~-4 LOC.
- **`NodeIdRemap.inputs` field** — written in `retain_reachable_roots`, never read; demote to a function-local. `strider-graph/src/graph.rs:628,687`. ~-5 LOC.
- **`PredGraphRef` trait + blanket impl** — referenced only by graphwalk's own test DSL. `graphwalk/src/lib.rs:59-86`. ~-28 LOC.
- **`PyLifter::new_internal`** — byte-identical duplicate of `#[new] fn new`; the one `run.rs` caller can call `new` directly. `strider-py/src/strider_cls.rs:164-171`. ~-8 LOC.
- **`impl Default for Capture`** — no caller; a `Default` that burns a global unique-ID counter is a hazard. `strider-pattern/src/capture.rs:51-55`. ~-5 LOC.
- **`Clone` derives on `StackOffsetFilter` / `StackAccessSpec`** — never cloned (`apply` consumes by value). `strider-pattern/src/node_builders/memory.rs:36,51`. ~-2 LOC.
- **Stale doc link `Cfg::switch_target_boundary_warnings`** — method does not exist. `strider-cfg/src/types.rs:139`. ~-1 LOC.

### Strict improvements (simpler AND O-equal-or-better)
- **`NodeCount::node_count` O(N)→O(1)** — `store.nodes.len() + store.outputs.len()` instead of summing per-node. `strider-graph/src/petgraph_view.rs:108-112`. perf + ~-4 LOC.
- **`Resolve::Unseen` dead variant → two-variant enum + `memo.contains_key`** — `Unseen` is never stored (only a `Default`/`unwrap_or` sentinel for an absent key). `strider-opt/src/sp_expr/mem_ssa/mod.rs:207-303`. ~-8 LOC. *(VERIFY: mem-SSA is correctness-critical; confirm `contains_key` == the old skip semantics on InProgress/Done.)*
- **`mem_chain_is_dirty` `Result<bool>` → `bool`** — body is infallible (`nearest_clobber` returns `NodeId`). `strider-opt/src/post_opt/function_args/mod.rs:313`. ~-4 LOC.
- **`Abs::same` → `#[derive(PartialEq)]`** — hand-rolled structural eq. `strider-opt/.../indirect_branch_resolve/eval.rs:36-46`. ~-10 LOC.
- **value_range dead `unwrap_or_else` → `expect`** — `result` is provably `Some` by the two guards above. `strider-opt/src/value_range/mod.rs:271`. ~-2 LOC.
- **`const_eval` `ins` SmallVec+filter is a no-op** — every handled kind has all-value inputs; `Inputs` is `Copy` with `.get`/index. `strider-opt/src/const_eval.rs:53-57`. ~-5 LOC.
- **`StackOffsetDetect` reuse `ctx.sp_memo`** — matches sibling post-passes; drops a fresh alloc + the `_ctx` suppression. `strider-opt/src/post_opt/stack_offset_detect/mod.rs:31`. ~-2 LOC.
- **`build_region_index` → iterator `collect`**; **`resolve_symbol_target` → `map_or_else`**; **BE `write_at` loop → `copy_from_slice(&value.to_be_bytes()[8-n..])`** (byte-exact, `n<=8` asserted). `strider-reader/src/elf/relocations.rs:989,719,1120`. ~-10 LOC.
- **`merge_resolved` `BTreeSet`→`Vec`+`sort_unstable`+`dedup`**; **inline `opt_ctx_for_run`** (single caller). `strider-orchestrator/src/lib.rs:447,99`. ~-9 LOC.
- **`link_register_vn` `match`→`.map(..).transpose()?`**; **`classify_arch_specific` `if/else`→`.then_some`**; **merge two `if let Some(sa)=stack_args` guards in `try_new`**. `strider-target/.../calling_convention/mod.rs:999,304`, `call_other_abi.rs:132`. ~-9 LOC.
- **`region_id_at_start` `(Bound::Included,..)`→`lower..=upper`** (drops `use Bound`). `strider-cfg/src/query.rs:179`. ~-2 LOC.
- **`call_descriptor` compact remap → existing `remap_hashmap`** (the last open-coded drain-rebuild); **remove three always-true dedup guards in `extend_asm_fingerprint`** merge (both inputs pre-sorted+deduped). `strider-ir/src/function/data.rs:994,819`. ~-11 LOC.
- **`wide_const_expected_bytes` inline** (sole caller); **phi-check `SmallVec`→`Inputs` directly**; **use-list forward check `all_node_ids().filter`→`reachable.iter()`**. `strider-ir/src/validate/{graph_invariants.rs,use_list_consistency.rs}`. ~-20 LOC.
- **`WideConstStorage` I256/I512 `as_u64` arms unify** via `self.limbs()`. `strider-ir/src/wide_const.rs:97`. ~-2 LOC.
- **Lift:** inline `read_call_other_args` (single caller); hoist duplicated `require_output_vn` in `process_int_binary_op`; drop the redundant `?;Ok(())` trailers in `handle_store`/`handle_call`. `strider-lift/src/lift/{call.rs,arithmetic.rs,memory.rs,control.rs}`. ~-10 LOC.
- **Pattern crate small inlines:** `bool_one_out`, `exemplar_is_load`, `int_const_discriminant` (use existing helper in `AnyBoolConst`), `LowerResult`→`Option`, `StackOffsetFilter`→`Option<i64>`, redundant local `use IRViewer` in `bindings.rs::get_bool`. ~-35 LOC.
- **strider-py small inlines:** `rebuild_table`, `nonzero_size` helper for the duplicated zero-guard, `elf_to_mem/rom_regions` parameterized helper, `invalidate_and_extend` for the two `add_elf` blocks, `PyVnSpace::name` if-chain→`match`, remove `per_address_ccs.is_empty()` fast-path. ~-30 LOC.

**Tier 1 subtotal: ~-300 LOC** across ~40 findings, all behavior-preserving.

---

## Tier 2 — High value, needs a verification spike or a readability decision before coding

- **`ValueType` `TypeInfo` / `TYPE_INFO` / `NodeOutputTypeCategory` table → inline `match` per method** (`as_str`/`byte_size`/`bit_width`/`is_integer`/`is_float`). `strider-ir/src/node/value_type.rs:44-158`. **~-100 LOC** (the single biggest cut; also deletes the table-ordering test). **Decision needed:** this trades one central data table for ~5 parallel `match`es that must stay in sync when a `ValueType` is added. Two agents rate it a readability *win* (removes a two-layer indirection + an ordering-constraint comment/test); a maintainer could argue the table is *easier* to extend. This is a judgment call on your "don't worsen the standard" bar.
- **`build_largest_container_map` identity-map simplification.** `strider-ir/src/builder/mod.rs:155-196`. **~-35 LOC.** The agent argues the stack-sweep is provably the identity map on its only (deduped) input, with sub-registers handled by the separate `or_insert_with(largest_container_in)` fallback. **CORRECTNESS-CRITICAL (register aliasing) — needs a dedicated verification spike** (a wrong container silently miscompiles overlapping-register lifts) before any edit.
- **`ConstFoldRules` struct → flat `Vec<BoxedRule>`.** `strider-opt/src/opt/constant_fold/rules.rs:31-76`. **~-40 LOC.** Depends on the five rule groups being order-independent (first-fire vs last-fire equivalence) — the agent flagged this UNCERTAIN itself. Also collapses five *named* groups into one flat vec (a readability trade). **Needs verification of the group-disjointness invariant** + a readability call.
- **`bindings.rs` op-extractor `macro_rules!`** for the six identical `get_*_op` extractors. `strider-pattern/src/bindings.rs:261-332`. ~-25 LOC. **Readability call:** a local macro vs six explicit 7-line fns.
- **Lift unifications:** float-conversion envelope helper (`handle_float_{int_to_float,float_to_float,trunc}`) and `build_shift_const`/`build_all_ones` wide-const dispatch. `strider-lift/src/lift/{float.rs,cast.rs,arithmetic.rs}`. ~-27 LOC. Multiple agents rated these readability-*neutral* and lifting is correctness-critical — only worth it if the helper genuinely reads better; verify lifted-shape identity.

---

## Tier 3 — Rejected (fails a standard) / deferred

- **Remove `is_alignment_mask` `tz==128` guard** — REJECT: load-bearing; `0u128 >> 128` panics in debug. (The agent concurred: do not remove.)
- **`cond_true_labelled` `HashSet`→`Option`** (cfg dot) — REJECT: a multi-`CondBranch`-predecessor CFG would mislabel an edge; correctness-uncertain for marginal gain in a debug-render path.
- **value_range no-op `is_control()` / `is_empty()` signature guards** — KEEP: defense-in-depth in soundness-critical code; removing them weakens robustness against a future signature change (worsens the standard).
- **`is_load_derived` recursive→iterative** — DEFER: only matters on pathologically deep cones; low value, non-trivial.
- **`Cfg` `pub` field + same-name accessor (`entry`/`region_graph`)** — DEFER: design-in-transition; resolving needs a cross-crate field-vs-accessor sweep.
- **`extend_asm_fingerprint` full sort+dedup rewrite** — DEFER: the documented linear-merge perf contract; only the always-true-*guard* removal (Tier 1) is safe.
- **`SideTableRemap` trait → free fn**, **`Match::node`/`value`/`bindings_clone` shims**, **`with_read_value`**, various `pub`→`pub(crate)` tightenings — DEFER: marginal LOC, real (if small) documentation/ergonomic value; not clear wins on the readability bar.

---

## Recommended approach

1. **Tier 1** in one or a few commits grouped by crate, each gated by `cargo test --workspace` + `clippy` (the ultimate correctness verification). Two items carry a "VERIFY" note (`const_value`/`get_as_*` py-FFI, `Resolve::Unseen` semantics) — confirm those first.
2. **Tier 2** only after a per-item decision: `build_largest_container_map` and `ConstFoldRules` get a read-only verification spike; `ValueType` table and the macro/lift unifications are your readability calls.
3. **Tier 3** left as-is.
