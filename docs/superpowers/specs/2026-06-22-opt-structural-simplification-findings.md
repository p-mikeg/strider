# Optimizer structural-simplification audit — findings

Date: 2026-06-22
Branch: `feature/opt-structural-simplification` (off `develop`)
Lens: reuse existing infrastructure instead of reinventing; correct data structures; remove redundant structs. Every finding screened for **behavior preservation** (these change pass internals, not just LOC — the optimizer must produce equivalent IR + preserve soundness). 7 read-only verification agents.

**Headline:** the codebase is already tightly factored. Of the specific hypotheses, **2 confirmed, 4 refuted, several qualified.** The genuine safe wins are a handful of infra-reuse swaps plus one meaty structural unification (LoadForward → peephole) that needs a design decision.

---

## Confirmed safe wins (behavior-preserving)

### A1. `region_collapse`: replace the reachable snapshot with the live-set
`region_collapse/mod.rs:46` does `let reachable = ctx.walk().collect()` (a full O(|reachable|) preorder walk + `DenseEntitySet` alloc), then at :113 checks `!reachable.contains(consumer)`. `EditFunction` already maintains the live-set incrementally — use `!ctx.is_live(consumer)` and drop the snapshot + the `reachable` parameter threaded through `try_collapse`. **SAFE-REUSE** (same set; `is_live` is actually *more* accurate mid-pass — it reflects same-sweep kills the stale snapshot misses, only ever making the pass more conservative, never wrongly detaching a live region). ~4 LOC + a param + the walk allocation.

### A2. `cfg_detach`: use the cached RPO instead of re-walking
`cfg_detach/mod.rs:68` calls `function.reverse_postorder_filter(...)` — the `Function` (IRWalker) version that re-walks from entry (`compute_full`, O(|reachable|)) every call. The `EditFunction` cached `edit.reverse_postorder_filter` yields the identical set (only order differs; any RPO works here) in O(|live|) with no re-walk. **SAFE-REUSE**, ~0 LOC, eliminates one full graph re-walk per pass run (needs a minor borrow-split tweak).

### A3. `SpExprMemo`: `FxHashMap<ValueId, …>` → `SecondaryMap<ValueId, …>`
`sp_expr/decompose.rs:52` `type SpExprMemo = FxHashMap<ValueId, Option<SpExpr>>`. This memo is **long-lived** (hoisted into `OptCtx::sp_memo`, reused across `StackOffsetDetect`/`FunctionArgDetect`/`CallStackArgCollect`/the evaluator, cleared between iterations) and probed once per `Load`/`Store` (thousands of nodes). `ValueId` is a dense entity key → `SecondaryMap` removes hashing on every probe, default `None` matches "absent = None". **SAFE-REUSE** (no caller iterates it in insertion order). *Note: this is the ONE data-structure win — the per-walk memos in `mem_ssa` and the cone memos in `eval`/`table`/`known_bits` are correctly `FxHashMap` (they're sparse/small vs the full id space; a `SecondaryMap` there would over-allocate 100–1000×).* 

### A4. `flag_cmp_canonicalize`: fix the stale comment (NOT the rules — see B-refuted)
The only real finding in flag_cmp is a **misleading comment** (`mod.rs:251-262`) claiming ConstFold has a "rule 5" that pre-decomposes the NZCV tree. **No such ConstFold rule exists** — the comment actively misled this very audit. Fix the comment; do not touch the rules.

### A5. (minor) extract a `value_input_producers` iterator
`cone_order` (eval.rs:232) and `find_index_candidates` (table.rs:142) both hand-roll "backward over value inputs filtered by `value_type_opt(i).is_some()`" — the same idiom used inline at eval.rs:109/211/246. Extract one `fn value_input_producers(f, v) -> impl Iterator<Item=ValueId>` and reuse. **SAFE-REUSE** for those sites (keep `is_load_derived` separate — it's a memoized predicate, not a traversal). ~15 LOC.

### A6. (cosmetic) `FunctionArgsOptions` / `AliasKnobs` inline
`FunctionArgsOptions` (options.rs:77, 2 bools, 1 consumer) could fold into `OptOptions`; `AliasKnobs` (function_args/mod.rs:282, private 3-field, 0 methods) could pass its fields directly. ~9 LOC, no invariant lost. Low value.

---

## The structural opportunity — needs a design decision

### B1. `LoadForward` → `PeepholePass` (and unify `LoadReadOnly`)
**Confirmed feasible, SAFE-REUSE.** `LoadForward::apply`'s bespoke `Worklist` + `reverse_postorder_filter(Load)` loop duplicates exactly what the `PeepholePass` driver already provides (seed + drain + Changed-accounting) — the same conversion `ConstantFold` already underwent. The memory-SSA narrowing side-effect survives untouched (it's idempotent + monotone `MayAlias→Disjoint`, so order-independent — the same loads forward regardless of iteration order).

**The one friction = a design choice.** `PeepholePass::try_rewrite(&self, ctx, root)` receives only `&mut EditFunction`, *not* `&mut OptCtx`. But `LoadForward` needs `alias_mode` + `sp_memo` from `OptCtx`, and `LoadReadOnly` needs `rom` from `OptCtx`. Two ways:
- **(i) Give `PeepholePass::try_rewrite` access to `&mut OptCtx`** (widen the trait method signature). Cleanest — lets BOTH `LoadForward` and `LoadReadOnly` (and future config-needing passes) become peepholes, unifying all per-node load passes under one driver. Touches the `PeepholePass` trait + its ~4 existing impls (ConstantFold/KnownBits-adjacent/phi_collapse/dead_branch) + the driver.
- **(ii) Move config onto the pass struct** (`LoadForward { alias_mode, sp_memo: RefCell<…> }`, like `ConstantFold`'s `Rc` state). Keeps the trait untouched, but scatters per-run config onto pass structs and doesn't help `LoadReadOnly`'s `rom`.

I recommend **(i)** — it's the real unification your "reuse the peephole driver" point is aiming at — but it's a trait change, so I want your sign-off before coding. Behavior is preserved either way (same loads forwarded, same narrowing, same memo semantics).

---

## Refuted — would regress or is already optimal (NO action)

- **`region_collapse` "iterates all root outputs when only phi_token matters"** — REFUTED. It already uses `node_outputs_exact::<2>` and ignores the phi_token where appropriate; the `all_outputs_unused` check *must* test BOTH outputs (a live control consumer would be missed otherwise).
- **`cfg_detach` should use the live-set for reachability** — REFUTED/BEHAVIOR-RISK. Its `cfg_reachable` is a **control-only** oracle; the live-set includes data-alive-but-control-dead nodes, so swapping it would wrongly skip dead-slot removal. Distinct, keep.
- **`cone_order` should be the existing RPO** — REFUTED. `cone_order` is a *scoped backward value-cone from one `ValueId`*, memory-excluded — not the whole-function, entry-rooted, control+data RPO. RPO would include wrong nodes and change the order (which `eval_target` depends on). Genuinely distinct.
- **`flag_cmp` "tries to do too much" because ConstFold canonicalizes first** — REFUTED (rigorously). ConstFold has **no symbolic-flag-tree decomposition rule** — every flag_cmp rule (rules 1-17 + the PPC CR-bit arm) handles a distinct per-arch shape (AArch64 NZCV, ARM/Thumb decomposed, Thumb flag-vs-0, PowerPC CR pack) that no other pass produces. Trimming any would regress the cross-arch flag suite (`strider-orchestrator/tests/flag_cmp.rs`, `cross_arch_shape.rs`). ConstFold running first actually *adds* required rules (14-17), the opposite of redundant.
- **`SSoT` / `clobber` structs exist "for no real reason"** — mostly REFUTED. `CallDescriptor`/`SpExpr`/`ReachingSpStore`/`KnownBitsFacts`/`ValidationErrors`/the clobber-derivation helpers all earn their keep (invariant enforcement, field-transposition guards, `thiserror`/`#[must_use]`). Only `FunctionArgsOptions`/`AliasKnobs` are mild (A6).
- **Broad reinvention** — the const-eval SSoT (`eval_node_const`/`read_rom_const`/`eval_int_*`), SP machinery (`SpDecomposer`/`SpAliasCfg`), rewrite DSL (`rewrite_rule!`/`apply_rules`), `remap_hashmap`, `container_of`, `Worklist` are all consistently reused with **zero** bespoke re-implementations. The codebase is already well-factored.

---

## Recommended scope
- **Tier A (A1–A6):** clear behavior-preserving infra-reuse + the one data-structure swap + the comment fix. Each gated by `cargo test --workspace`.
- **Tier B (B1):** LoadForward→peephole — pick design (i) or (ii) first; this is the substantive structural win.
