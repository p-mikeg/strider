# crates/strider review — round 6 (2026-04-30)

## Summary

- Files reviewed: 47 (all `.rs` under `crates/strider/src`, `crates/strider/tests`, `crates/strider/examples`; `crates/strider/benches/` does not exist)
- Findings total: 56
  - Correctness: 13
  - Dead code: 11
  - Duplication & unification: 11
  - Simplification: 9
  - Readability: 9
  - Performance: 3
- High-confidence findings: 36
- Low-risk findings: 41

The crate is in solid shape post-anyhow / post-Capture / post-F-022 migrations. The orchestrator's split between stable and destructive subsets is well-articulated, the cache invariants are documented in detail (with rollback machinery already in place), and the tier-2 classifier / inplace shims are thin and on-thesis. Standout themes:

- **Doc drift across the anyhow migration is widespread.** `crate::ErrorKind::*` variants the docstrings name no longer exist (`ErrorKind::TargetError`, `ErrorKind::CfgNoRegion`, `ErrorKind::IrError`, `ErrorKind::PatternError`, `ErrorKind::SwitchHasNoTargets`, `ErrorKind::SwitchTargetMissing`, `ErrorKind::UnimplementedOpcode`, `ErrorKind::MissingOutputVn`, `ErrorKind::UnsupportedVnSpace`, `ErrorKind::UnsupportedRegSize`, `ErrorKind::CfgError`). All current errors are bare `anyhow::anyhow!` strings. ~15 sites.
- **`crate::error` is a 3-line file** containing only `pub type Result<T> = anyhow::Result<T>;`. Two re-exports point at it (`pub mod error;` + `pub use error::Result;`). Could collapse into `lib.rs`.
- **`#[doc(hidden)] pub use cache as ir_cache;`** is a "back-compat alias" but the only users in the workspace are this crate's own integration tests + an outdated doc comment in `ir/`. No external consumers.
- **Codename comments (F2 / F5 / F6 / F7 / R1.4 / R2 / R3 / R4 / W1 / W2 / W5 / W6 / W7 / W9 / G1 / G2 / H0 / M1 / M2 / BUG-5 / BUG-25 / BUG-30)** — the user noted these were stripped recently from CLAUDE.md but ~49 references remain in strider source.
- **`build_anchor_calling_context` is a 60-line helper buried in `orchestrator.rs`** that builds a 3-list ABI context the in-place editors consume. It belongs in `inplace.rs` (or in `opt::indirect_branch_resolve::inplace`), and the linear-scan-the-cache trick to find a region's exit handles is one place where the cache should expose a "find by exit_control" helper.
- **The orchestrator threads a `CfgSplitSnapshot` between iterations** and re-implements `invalidate_split_regions` as `apply_split_invalidation` because the public helper takes a `Cfg` and the orchestrator wants to drop the old `Cfg` to recover its `Sleigh`. Two implementations of the same semantics.
- **`extend_predecessors_into` is documented as a no-op in round 1** and actually IS one (the body is `let _ = cache; let _ = cfg; Ok(())`). The whole function exists only so its name is exported. Real callers use `extend_predecessors_with_handle`.
- **`IrStrider::build_entry` is a 1-line trampoline** that just calls `self.builder.build_entry()` — the indirection adds nothing.
- **Test fixture duplication is widespread.** `make_strider_x86_64()` / `make_strider_for_test()` / `Strider::new(...)` boilerplate is reproduced in nearly every test file (8+ sites).
- **Per-iteration `lr_vn` / `sp_vn` reads in the orchestrator's hot loop**: every iteration re-reads `config.strider.calling_convention().link_register_vn` etc. — could be hoisted once.
- **`AnalyzeOutcome::region_handles`** is described as "Always populated" but the round-1 cache lifecycle effectively throws every snapshot away each iteration (entries are re-created from the next lift).
- **`Strider::find_all_unique_vns` uses `std::collections::HashSet`** despite `rustc-hash` being a workspace dep; one of the "deterministic order: sorted by (space-shortcut, off, size)" steps then re-sorts.
- **`rustc-hash` is in `Cargo.toml`** but nothing in `crates/strider/src/` uses it.
- **Some classifier integration tests deliberately bypass the validator** (synthesise `ValuePhi` with raw `graph.create_node` after `build()`). Leaves the classifier exercised under graphs the validator would reject.
- **Two helpers (`current_anchor_after_opt` / `anchor_value_input`)** in `tests/common/tier2_helpers/orchestrator.rs` have the same body; one is `pub(super)`, the other `pub` — the comment says "local copy because the existing `current_anchor_after_opt` is private."

## Table of contents

- F-001 `apply_link_register` zero-input-Return path docs assert "drop the placeholder slot" but slot 2 is unconditionally removed even when ret-vals replace it, leaving the assertion wrong (Correctness, `inplace.rs:25-32`)
- F-002 `analyze_cfg`'s `SpecialTerm` enum is defined inline INSIDE the per-region loop body (Correctness/Readability, `pipeline.rs:347-350`)
- F-003 `OrchestratorConfig::sleigh` is `Option<Sleigh>` and `run` `bail`s on `None` instead of asking for a non-optional `Sleigh` at the type level (Correctness/Simplification, `orchestrator.rs:149-227`)
- F-004 `unresolved` is cloned every iteration into `unresolved_after_edits`, then re-assigned and re-cloned at the rebuild path (Correctness/Performance, `orchestrator.rs:344-383`)
- F-005 `apply_split_invalidation` re-implements `invalidate_split_regions` against a snapshot — duplicated semantics, drift risk (Correctness/Duplication, `orchestrator.rs:100-123` vs `cache/invalidate.rs:50-96`)
- F-006 Iteration cap `2*pending + 4` doesn't bound iterations after in-place edits if no edge-set change but in-place edits keep firing (Correctness, `orchestrator.rs:265-396`)
- F-007 `build_anchor_calling_context` linear-scans the entire cache to find the region whose `exit_control` matches the placeholder's input (Performance, `orchestrator.rs:617-625`)
- F-008 `update_cache_exit_handle_after_tail_call` linear-scans the cache to patch a single entry (Performance, `orchestrator.rs:570-584`)
- F-009 `read_or_init_var` linear-scans the entire graph for an existing `InitialVar(vn)` (Performance, `orchestrator.rs:687-694`)
- F-010 `process_insn` says "MultiEqual is decompiler-internal" but Sleigh's MULTIEQUAL pcode op IS sometimes emitted in raw pcode for some arches (Correctness, `insn/mod.rs:75-78`)
- F-011 `handle_call_other`'s `u32::try_from` silently ignores user-op ids ≥ 2³² without surfacing the malformed input (Correctness, `insn/mod.rs:126-133`)
- F-012 `decode_space_id` uses an `unsafe { VnSpace::by_id(...) }` pattern with a SAFETY comment that conflates "verified non-CONST/CONST" semantics (Correctness, `insn/mod.rs:148-160`)
- F-013 `apply_split_invalidation` and `invalidate_split_regions` both ignore the `old_region.start_addr.machine_addr != key` corner-case (Correctness, two files)
- F-014 `crate::error` module is 3 lines containing one type alias; the wrapper is dead-weight (Dead code, `error.rs:1-3`)
- F-015 `#[doc(hidden)] pub use cache as ir_cache` has no external consumers (Dead code, `lib.rs:39-42`)
- F-016 `rustc-hash` workspace dep is unused inside `crates/strider/src/` (Dead code, `Cargo.toml:16`)
- F-017 `extend_predecessors_into` body is `let _ = cache; let _ = cfg; Ok(())` (Dead code, `cache/extend.rs:41-53`)
- F-018 `IrStrider::build_entry` is a 1-line trampoline (Dead code, `strider/mod.rs:55-57`)
- F-019 `RegionIrEntry::empty` is a sentinel constructor used only by tests; production callers use `from_lift_handles` (Dead code, `cache/entry.rs:65-88`)
- F-020 `GraphRewriter::new`, `from_builder`, `entry()`, `graph()` have zero non-test external callers (Dead code, `rewrite.rs:87-127`)
- F-021 `crate::cache_key_for_region` is `pub` but only used by tests + within strider's own modules (Dead code, `cache/mod.rs:84-93`)
- F-022 `crate::count_uncached_regions` is `pub` but only used by tests (Dead code, `cache/mod.rs:105-117`)
- F-023 `crate::predecessor_diffs` is `pub` but only used by tests (Dead code, `cache/mod.rs:130-145`)
- F-024 `Strider::arch()` is `pub` but only the orchestrator uses it (Dead code, `pipeline.rs:135-137`)
- F-025 `OrchestratorConfig::sleigh` documented as "owned by value" but actually `Option<...>` (Readability, `orchestrator.rs:144-165`)
- F-026 `CallingConvention` / `SleighArch` / `Endianness` / `BuiltCallingConvention` re-exports from `target` are still exposed; verify external usage (Dead code, `lib.rs:51`)
- F-027 `apply_split_invalidation` (orchestrator) and `invalidate_split_regions` (cache) are near-duplicates (Duplication, two files)
- F-028 `current_anchor_after_opt` and `anchor_value_input` are the same helper (Duplication, `tests/common/tier2_helpers/orchestrator.rs:27-46` vs `:112-124`)
- F-029 `make_strider_x86_64()` boilerplate duplicated across 5+ test files (Duplication, multiple files)
- F-030 `analyze_cfg`'s region-handles snapshot loop (`pipeline.rs:436-488`) duplicates the field-extraction logic in `cache/lift.rs::populate_cache_from_handles` (Duplication, two files)
- F-031 `decode_space_id`'s "expected at least 1 input" check is duplicated against the per-opcode dispatch's same check (Duplication, `insn/mod.rs:101-110, 148-160`)
- F-032 Locating the placeholder Return ("3-input Return") is reproduced in ~6 sites (Duplication, multiple files)
- F-033 `find_largest_fitting_register`-shaped logic isn't here, but the per-byte-size dispatch in `extend.rs:141-152` is the third copy of the `1/2/4/8/16/32 → NodeOutputType` switch (Duplication, `cache/extend.rs:141-152`)
- F-034 The cache split into 4 sub-modules (entry / extend / invalidate / lift) with `mod.rs` carrying 3 functions feels over-decomposed for ~600 LOC (Simplification, `cache/`)
- F-035 `IrStrider` per-function context owns `unresolved_branches: Vec<...>` mutated only by `handle_unresolved_indirect_branch` (Simplification, `strider/mod.rs:14-30`)
- F-036 `analyze_cfg`'s `cfg_ir_graph` is a structural clone created just to walk node indices in the same order as `cfg.graph` — `cfg.region_ids()` already does this (Simplification, `pipeline.rs:295-323`)
- F-037 `apply_in_place_edit::Single` re-walks the placeholder's inputs to find `old_exit_control` AFTER computing it from the cache (Simplification, `orchestrator.rs:499-512`)
- F-038 `OrchestratorConfig::rom: Option<Arc<dyn ReadOnlyMemory>>` doc says "cloned per-iteration" — but inside the orchestrator only `as_deref()` is used (Simplification, `orchestrator.rs:151-153, 287, 746-747`)
- F-039 `is_tail_call` saturating-add semantics (`end_exclusive <= target`) — the boundary case is documented in tests but the comment in `is_tail_call` itself says nothing (Readability, `orchestrator.rs:436-450`)
- F-040 `analyze_cfg`'s docstring lists 9 stale `ErrorKind::*` variants, none of which exist post-anyhow (Readability, `pipeline.rs:277-286`)
- F-041 `Strider::new` docstring lists `ErrorKind::TargetError` (Readability, `pipeline.rs:107-112`)
- F-042 `GraphRewriter::apply_rule` docstring says "wrapped in `crate::ErrorKind::PatternError`" — there is no such variant (Readability, `rewrite.rs:154-157`)
- F-043 `cache_key_for_region` docstring says "Returns `crate::error::ErrorKind::CfgNoRegion`" (Readability, `cache/mod.rs:79-83`)
- F-044 Inline `enum SpecialTerm { Unresolved(...), Switch(...) }` inside `analyze_cfg`'s per-region loop body (Readability, `pipeline.rs:347-350`)
- F-045 `analyze_cfg` `cfg_ir_graph.map(|_, _| None::<ir::RegionId>, |_, e| *e)` clones the entire CFG graph just for placeholder None storage (Readability/Performance, `pipeline.rs:297`)
- F-046 `RegionLiftHandles::predecessor_count` is captured at lift but only used by `from_lift_handles` to set `cached_predecessor_count`; the snapshot vs cache duplication is opaque (Readability, `pipeline.rs:21-24`)
- F-047 `IrStrider::value_lifter` is `pub(super)` but only called from `vn_io.rs` (the same module containing `value_lifter`) — the visibility is wider than necessary (Readability, `strider/vn_io.rs:17`)
- F-048 `tests/common/tier2_helpers/mod.rs` `#[allow(unused_imports)]` on the flat re-export surface masks legitimate dead-helper signals (Readability, `mod.rs:53-63`)
- F-049 `analyze_cfg`'s `ir_region_of` closure is defined as `Result<ir::RegionId>` but called inside the per-instruction loop dozens of times — could be precomputed into a `Vec<ir::RegionId>` indexed by `RegionId.index()` (Performance, `pipeline.rs:308-314`)
- F-050 The orchestrator's `for (addr, anchor_output) in &unresolved` loop reads `config.strider.calling_convention()` once per iteration but the cc is stable; trivially hoist (Performance, `orchestrator.rs:267-274`)
- F-051 `Strider::find_all_unique_vns` uses `std::HashSet`, then sorts; could collect into a `Vec` and `sort_unstable` + `dedup` to skip the hash entirely (Performance, `pipeline.rs:243-261`)
- F-052 `process_insn`'s `region_lookup_dyn: &dyn Fn(...)` trait-object adapter is set up regardless of whether any control-flow opcode actually fires (Performance, `insn/mod.rs:30-31`)
- F-053 `handle_call_other` builds a `Vec<ir::Value>` even for opcodes with no args (Simplification, `insn/mod.rs:110-113`)
- F-054 The classifier shim's three-arity API surface (`classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`) over-generates options-style entry points (Simplification, `classify.rs:13-58`)
- F-055 `apply_link_register` wraps `opt::apply_link_register` with no transformation — pure re-export shim (Simplification, `inplace.rs:25-32`)
- F-056 `apply_tail_call` is also a pure re-export shim (Simplification, `inplace.rs:48-65`)

## Findings

### Correctness

### F-001 `apply_link_register` shim doc says "drops placeholder `target_value` slot post-edit" but never enforces the input arity precondition

- **Category:** Correctness
- **Location:** `crates/strider/src/indirect_resolve_tier2/inplace.rs:25-32`
- **What:**
  ```rust
  pub fn apply_link_register(
      fg: &mut BuiltFunctionGraph,
      placeholder_return: NodeId,
      ret_val_outputs: &[NodeOutputId],
  ) -> Result<()> {
      opt::apply_link_register(fg, placeholder_return, ret_val_outputs)
          // opt is now anyhow-based; ? lifts directly via identity From.
      }
  ```
- **Why:** The shim's docstring (`# Errors`) says the function returns errors when "`placeholder_return` is not a `NodeKind::Return`, or when the opt-side `add_node_input` call fails." But `apply_link_register` (per `opt::apply_link_register`'s F-022 redesign) drops slot 2 unconditionally — i.e. it will happily corrupt a non-placeholder Return that happens to have ≥3 inputs (e.g. a real ABI Return with `ret_val_regs.len() == 1`). The opt-side test `apply_link_register_zero_ret_vals_drops_target_value` only exercises a 3-input Return; if the strider-level call site ever passes an already-resolved Return, the placeholder-slot drop would silently delete a real ret_val. The shim's doc claims the precondition is enforced, but neither the opt-side nor the shim verifies the Return is the original placeholder.
- **Proposed change:** Add a debug-time assertion (or a real `bail!` guard) that `placeholder_return` has exactly 3 inputs before delegating, OR document explicitly in the strider-side docstring that the caller is responsible for arity. The orchestrator's `find_placeholder_return_for_anchor` already filters; pin that contract in the shim.
- **Confidence:** medium
- **Risk if applied:** low

### F-002 `analyze_cfg`'s `SpecialTerm` enum defined inline INSIDE the per-region loop

- **Category:** Correctness / Readability
- **Location:** `crates/strider/src/strider/pipeline.rs:347-365`
- **What:**
  ```rust
  for node_idx in cfg_ir_graph.node_indices() {
      ...
      enum SpecialTerm {
          Unresolved(rsleigh::Vn, cfg::PcodeInsnAddr),
          Switch(rsleigh::Vn, Vec<u64>, Option<ir::Value>),
      }
      let special_terminator: Option<SpecialTerm> = match &region.terminator {
          ...
      };
  ```
- **Why:** Defining `enum SpecialTerm` inside the loop body is legal Rust but confusing — the enum is recreated (definitionally) per iteration and clutters the loop body. It's also a hint that the function is too long: `analyze_cfg` is ~210 lines. Pulling `SpecialTerm` out as a private module-level enum (or directly handling each terminator-arm in a helper) would clarify the dispatch and make the special-case path easier to extend.
- **Proposed change:** Lift `enum SpecialTerm { Unresolved(rsleigh::Vn, cfg::PcodeInsnAddr), Switch(rsleigh::Vn, Vec<u64>, Option<ir::Value>) }` to module level (or replace it with two handler-method calls that take the original terminator).
- **Confidence:** high
- **Risk if applied:** low

### F-003 `OrchestratorConfig::sleigh: Option<Sleigh>` and `run` bails on `None` — should be a non-optional field

- **Category:** Correctness / Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:149-165, 220-227`
- **What:**
  ```rust
  pub sleigh: Option<rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>>,
  ...
  let mut sleigh: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>> =
      config.sleigh.take().ok_or_else(|| anyhow!(
          "not yet implemented: OrchestratorConfig::sleigh was None — caller must supply an owned Sleigh"
      ))?;
  ```
- **Why:** Every test passes `sleigh: Some(...)`; the `None` branch surfaces a `not yet implemented` error string. The `Option` exists only because the orchestrator wants to `take()` ownership while keeping the rest of `config` alive — but that pattern is achievable by destructuring the config at the top of `run` (or by holding the `Sleigh` by value and dropping it from the `OrchestratorConfig` shape). Passing `None` is always a bug; making the field non-optional turns it into a compile-time error instead of a runtime `bail!`.
- **Proposed change:** Change `sleigh: Sleigh<...>` (no Option), and either destructure `config` into `(strider, sleigh, rest)` at the top of `run`, or store `sleigh` separately from the rest of the configuration.
- **Confidence:** high
- **Risk if applied:** low (touches every call site, but they all pass `Some(...)`)

### F-004 `unresolved` is cloned per iteration AND per in-place edit

- **Category:** Correctness / Performance
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:344-383`
- **What:**
  ```rust
  let unresolved_after_edits = if in_place_edits.is_empty() {
      unresolved.clone()
  } else {
      unresolved
          .iter()
          .filter(|(_, anchor)| {
              opt::find_placeholder_return_for_anchor(&graph.graph, *anchor).is_some()
          })
          .cloned()
          .collect()
  };
  ...
  unresolved = unresolved_after_edits;
  ```
- **Why:** `unresolved` is `Vec<(PcodeInsnAddr, ir::Value)>`. The `unresolved.clone()` in the no-in-place-edits path is wasteful — the value is then assigned back to `unresolved`, so we could just leave it. The filter-clone variant in the with-edits path is fine, but the unconditional clone for the diagnostic-only sentinel "did we make progress" is dead weight. More importantly, the `if in_place_edits.is_empty()` test happens AFTER the per-anchor classify loop completes; the `for (addr, anchor_output) in &unresolved` loop reads `unresolved` immutably, so we could move-and-rebuild without the clone.
- **Proposed change:** Fold the filter into a single `let unresolved_after_edits = if in_place_edits.is_empty() { std::mem::take(&mut unresolved) } else { unresolved.iter().filter(...).cloned().collect() };`. Or simpler: skip the clone and reuse `unresolved` directly when no edits fired.
- **Confidence:** medium
- **Risk if applied:** low

### F-005 `apply_split_invalidation` re-implements `invalidate_split_regions` against a `CfgSplitSnapshot`

- **Category:** Correctness / Duplication
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:100-123` vs `crates/strider/src/cache/invalidate.rs:50-96`
- **What:** `invalidate_split_regions(cache, old_cfg, new_cfg)` and `apply_split_invalidation(cache, prev_snapshot, new_cfg)` are semantically identical: they both walk `new_cfg`'s regions, look up the previous insn count by `MachineInsnAddr` key, and evict cache entries whose insn count shrank. The orchestrator can't use the public helper directly because it wants to drop `old_cfg` between iterations (to recover the `Sleigh` handle), so it carries a `CfgSplitSnapshot: HashMap<MachineInsnAddr, usize>` instead. The two implementations have to stay in lockstep — every change to one (e.g. handling brand-new regions, or adding tail-call detection) must be replicated.
- **Why:** A drift between the two implementations would mean the public API and the orchestrator behave differently on the same input. Bug surface: someone fixes a corner case in `invalidate_split_regions` (e.g. handling regions whose `start_addr` collides) and forgets the snapshot variant.
- **Proposed change:** Refactor `invalidate_split_regions` to take a `&CfgSplitSnapshot` instead of `&Cfg`. Provide `take_split_snapshot(cfg)` (already exists in orchestrator) as a public helper. The cache module becomes the canonical implementation; the orchestrator drops `apply_split_invalidation`.
- **Confidence:** high
- **Risk if applied:** low

### F-006 Iteration cap may not bound progress when in-place edits keep firing without edge-set change

- **Category:** Correctness
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:265, 388-396`
- **What:**
  ```rust
  let cap = 2usize.saturating_mul(pending_at_iter_0).saturating_add(4);
  for _iter_idx in 0..cap {
      ...
      // If only in-place edits fired (no edge-set change), no CFG
      // rebuild — re-run the stable subset on the freshly-edited
      // IR and re-classify in the next loop turn.
      if !edge_set_changed {
          run_stable_only(&config, &mut graph)?;
          if unresolved.is_empty() {
              run_destructive(&config, &mut graph)?;
              return Ok(graph);
          }
          continue;
      }
  ```
- **Why:** The cap formula `2*pending_at_iter_0 + 4` is justified by "every legal classification transition strictly grows the induced edge set." But the `if !edge_set_changed` branch fires when ONLY in-place edits happen — no edge-set growth — and `continue`s the loop. The monotonicity argument (edge set strictly grows) doesn't apply to in-place edits: an iteration that fires only in-place edits doesn't consume an edge-set step. If a tier-2 classifier returns the same `Single(K)` for the same anchor on every iteration (because the placeholder `find_placeholder_return_for_anchor` keeps finding the placeholder somehow), the loop could spin without progress until the cap. The cap saves us from a hang, but the resulting "did not converge" error misclassifies a soundness-violating tier-2 as a cap-overflow. The in-place-edit path SHOULD remove the placeholder (`apply_in_place_edit` calls `apply_link_register` / `apply_tail_call`, which detach), so subsequent iterations should see fewer `unresolved` — but the reasoning relies on tier-2 always finding `placeholder_return = Some(_)` BEFORE applying. If the placeholder lookup goes stale, the in-place edit isn't pushed and `unresolved.len()` stays the same.
- **Proposed change:** Tighten the loop invariant: assert that after an in-place-edits iteration, `unresolved_after_edits.len() < unresolved.len()`. If this fails, emit a typed "in-place edit didn't make progress" error rather than letting the loop spin to the cap.
- **Confidence:** medium
- **Risk if applied:** low

### F-010 `process_insn` rejects `Opcode::MultiEqual` as "decompiler-internal" but Sleigh emits MULTIEQUAL in some contexts

- **Category:** Correctness
- **Location:** `crates/strider/src/strider/insn/mod.rs:75-78`
- **What:**
  ```rust
  Opcode::MultiEqual => {
      bail!("opcode {:?} is decompiler-internal and should not appear in raw p-code", insn.opcode);
  }
  ```
- **Why:** GHIDRA's MULTIEQUAL is genuinely a decompiler-internal phi node and shouldn't appear in raw PcodeOp output from `Sleigh::lift_one`. But the bail returns a hard error and crashes the entire `analyze_cfg` call — for an unfamiliar arch where Sleigh DOES synthesise a MULTIEQUAL on lift (rare but possible per GHIDRA's internal coding), strider hard-fails. Compare with `Opcode::Nop` (silently OK) and the `_ => bail!("unimplemented opcode...")` catch-all. The hard fail is appropriate IF the assumption holds; the error message is clear; but the assertion deserves a code-comment citation so a future maintainer doesn't relax it accidentally.
- **Proposed change:** Either keep the bail but cite `rsleigh::Sleigh`'s contract that says MULTIEQUAL isn't produced by `lift_one`, or fall back to the unimplemented-opcode arm so the surface message is consistent.
- **Confidence:** low
- **Risk if applied:** low

### F-011 `handle_call_other` silently treats user-op IDs ≥ 2³² as nameless instead of erroring

- **Category:** Correctness
- **Location:** `crates/strider/src/strider/insn/mod.rs:126-133`
- **What:**
  ```rust
  // u32 is sleigh's native id width — anything wider is malformed.
  if let Ok(id_u32) = u32::try_from(user_op_id)
      && let Some(name) = self.cfg.sleigh.user_op_name(id_u32)
  {
      self.builder
          .body_mut()
          .graph
          .set_call_other_name(node_id, name.to_string());
  }
  ```
- **Why:** The comment says "anything wider is malformed", but the code silently skips name lookup if `u32::try_from` fails. The CallOther node is still created with its `user_op_id: u64` payload, just without a name in the side-table. Downstream, `opt::CallOtherElide` looks up names in `NO_OP_USER_OPS`; without a name a CallOther can never be elided. The "malformed input" path silently degrades to "unrecognised opcode kept around forever" — same as a legitimate user-op the elide pass doesn't know.
- **Proposed change:** Either bail explicitly when `user_op_id` doesn't fit in `u32`, or document the silent degradation in the comment and verify it cannot fire in practice (Sleigh only assigns u32 ids).
- **Confidence:** medium
- **Risk if applied:** low

### F-012 `decode_space_id` SAFETY comment conflates verified-CONST with by-id semantics

- **Category:** Correctness
- **Location:** `crates/strider/src/strider/insn/mod.rs:148-160`
- **What:**
  ```rust
  fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
      let space_id_vn = *insn
          .inputs
          .first()
          .ok_or_else(|| anyhow!("opcode {:?} has too few inputs: expected at least 1, got 0", insn.opcode))?;
      if space_id_vn.addr.space != rsleigh::VnSpace::CONST {
          bail!("opcode {:?} expects a CONST input at position 0", insn.opcode);
      }
      // SAFETY: `space_id_vn` is the `inputs[0]` of a LOAD/STORE p-code insn and
      // was just verified to live in CONST space, which is the precondition of
      // `VnSpace::by_id`.
      Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
  }
  ```
- **Why:** The SAFETY comment is partially correct: the precondition of `VnSpace::by_id` is that the varnode encodes a target space pointer in CONST space. However, NOT every CONST-space varnode is a valid space pointer — the offset still has to be a real pointer to a Sleigh `AddrSpace` object. The comment doesn't acknowledge this; a malformed pcode (e.g. `Load`, with a CONST input whose offset is `0xdeadbeef`) would pass the check and hit `unsafe by_id` UB. The pcode's structural validity is the real precondition, not just the space tag. In practice rsleigh always produces valid space pointers, but the safety contract is wider than the comment claims.
- **Proposed change:** Strengthen the SAFETY comment to "`by_id`'s precondition is that `space_id_vn.addr.off` is a valid pointer to a Sleigh `AddrSpace`; this holds because the pcode comes from `rsleigh::Sleigh::lift_one` which only emits Load/Store with a valid space-pointer encoding." Alternatively, replace the unsafe call with a safe wrapper if rsleigh exposes one.
- **Confidence:** medium
- **Risk if applied:** low (documentation-only change unless rsleigh adds a safe variant)

### F-013 `apply_split_invalidation` and `invalidate_split_regions` ignore `start_addr` mismatches

- **Category:** Correctness
- **Location:** `crates/strider/src/cache/invalidate.rs:73-87`, `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:104-118`
- **What:** Both `invalidate_split_regions` and `apply_split_invalidation` look up the cached entry by `MachineInsnAddr` key. `cache_key_for_region` derives the key from `region.start_addr.machine_addr`. But `RegionIrEntry` ALSO stores `start_addr: PcodeInsnAddr` independently. If the cache entry's `start_addr.machine_addr` ever drifts from its key, the invalidator silently mis-attributes counts. This shouldn't happen (both come from the same lift), but the redundancy is unchecked.
- **Why:** A future cache-population path that inserts an entry under a key derived elsewhere (or a test that hand-constructs an entry — `RegionIrEntry::empty(pcode_addr(0xdead))` and inserts under `MachineInsnAddr { addr: 0x1000 }`) would silently confuse the invalidator. The `pcode_addr` test sentinel uses `addr` `0xdead_beef`, so the key/start_addr alignment is enforced only by convention.
- **Proposed change:** Either compare `entry.start_addr.machine_addr == key` inside the invalidator and log/bail on mismatch, or remove the `start_addr` field from `RegionIrEntry` (use the key directly).
- **Confidence:** low
- **Risk if applied:** low

### F-007 `build_anchor_calling_context` linear-scans the cache to find the placeholder's region

- **Category:** Performance
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:617-625`
- **What:**
  ```rust
  let placeholder_ctrl_in: Option<ir::node::NodeOutputId> = {
      let inputs: Vec<_> = graph.graph.node_inputs(placeholder).into_iter().collect();
      inputs.first().copied()
  };
  let cache_entry: Option<&crate::RegionIrEntry> = placeholder_ctrl_in.and_then(|ctrl| {
      cache
          .values()
          .find(|entry| entry.exit_control == ctrl)
  });
  ```
- **Why:** O(cache_size) scan per placeholder per orchestrator iteration. The cache is keyed by `MachineInsnAddr`; the missing index is "given an `exit_control` `NodeOutputId`, find the entry." Currently cache is small (≤ region count of the function), so 100s of entries × 10s of placeholders × 10s of iterations = bounded. But the orchestrator's documented plan is to scale to larger functions; the linear scan needs to become a `FxHashMap<NodeOutputId, MachineInsnAddr>` index.
- **Proposed change:** Add a secondary index `exit_control_to_key: HashMap<NodeOutputId, MachineInsnAddr>` to `RegionIrCache` (or to the `OrchestratorConfig` struct). Update on every cache insert / `update_cache_exit_handle_after_tail_call`.
- **Confidence:** medium
- **Risk if applied:** medium (touches cache shape)

### F-008 `update_cache_exit_handle_after_tail_call` linear-scans the entire cache

- **Category:** Performance
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:570-584`
- **What:**
  ```rust
  fn update_cache_exit_handle_after_tail_call(
      cache: &mut RegionIrCache,
      old_exit_control: ir::node::NodeOutputId,
      new_exit_control: ir::node::NodeOutputId,
  ) {
      for entry in cache.values_mut() {
          if entry.exit_control == old_exit_control {
              entry.exit_control = new_exit_control;
          }
      }
  }
  ```
- **Why:** Same theme as F-007: O(cache_size) scan per tail-call resolution. The doc says "single match expected" but the implementation continues scanning anyway "for corruption tolerance." The fix and the previous finding share the same secondary index.
- **Proposed change:** Same as F-007: maintain `exit_control_to_key` so `update_cache_exit_handle_after_tail_call` is O(1).
- **Confidence:** medium
- **Risk if applied:** medium

### F-009 `read_or_init_var` linear-scans `all_node_ids()` for an existing `InitialVar(vn)`

- **Category:** Performance
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:687-694`
- **What:**
  ```rust
  for nid in graph.graph.all_node_ids() {
      if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
          && *existing == vn
          && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
      {
          return Some(out);
      }
  }
  ```
- **Why:** O(arena size) scan per varnode per anchor. The cc has ~6 arg regs + ~6 ret regs + ~6 callee saved = ~18 vars; orchestrator runs O(iterations × placeholders × 18) of these scans. Large functions amplify. The IR builder maintains a `HashMap<Vn, NodeId>` for `InitialVar` dedup internally; the comment says "InitialVars are not cache-deduplicated by `Graph::create_node`", which is what motivates the linear scan, but the orchestrator could cache its own per-call lookup table for the lifetime of one tier-2 iteration.
- **Proposed change:** Build a `FxHashMap<Vn, NodeOutputId>` index once at the top of each `apply_in_place_edit` call; reuse for every `read_or_init_var` invocation in that call's `build_anchor_calling_context`.
- **Confidence:** medium
- **Risk if applied:** low

### Dead code

### F-014 `crate::error` module is a 3-line file containing only one type alias

- **Category:** Dead code
- **Location:** `crates/strider/src/error.rs:1-3`
- **What:**
  ```rust
  //! Error type for the `strider` crate.

  pub type Result<T> = anyhow::Result<T>;
  ```
- **Why:** Pre-anyhow this file housed a wrapper struct + ErrorKind enum. Post-anyhow it's effectively dead — every consumer could use `anyhow::Result` directly. The `pub mod error;` + `pub use error::Result;` pattern adds two re-exports for an alias.
- **Proposed change:** Delete `error.rs` and remove the `pub mod error;` + `pub use error::Result;` lines from `lib.rs`. Replace internal `crate::error::Result` with `anyhow::Result` everywhere (or remove the `Result` re-export and leave it alone).
- **Confidence:** high
- **Risk if applied:** low

### F-015 `#[doc(hidden)] pub use cache as ir_cache` has no external consumers

- **Category:** Dead code
- **Location:** `crates/strider/src/lib.rs:39-42`
- **What:**
  ```rust
  // `ir_cache` is the historical name for what is now `cache::*`.  Kept
  // as an alias so external callers continue to compile.
  #[doc(hidden)]
  pub use cache as ir_cache;
  ```
- **Why:** A workspace-wide grep for `ir_cache` shows only:
  - `crates/ir/` doc comments referring to "`crate::ir_cache::RegionIrCache`" — stale doc references
  - `crates/strider/tests/tier2_cache.rs:1` — also a doc comment ("Integration tests for [`strider::ir_cache`]")
  - This crate's own internal references — none use the alias path
  - No production consumers
- **Proposed change:** Drop the alias + the doc comments that reference it.
- **Confidence:** high
- **Risk if applied:** low

### F-016 `rustc-hash` workspace dep is unused inside `crates/strider/src/`

- **Category:** Dead code
- **Location:** `crates/strider/Cargo.toml:16`
- **What:**
  ```toml
  rustc-hash = { workspace = true }
  ```
- **Why:** A workspace grep for `rustc_hash`/`FxHashMap`/`FxHashSet` inside `crates/strider/src/` returns zero hits. The crate uses `std::collections::HashMap`/`HashSet` everywhere.
- **Proposed change:** Remove the dependency. If a future fixed-point pass wants `FxHashMap` for performance, re-add it then.
- **Confidence:** high
- **Risk if applied:** low

### F-017 `extend_predecessors_into` is a `let _ = ...; Ok(())` no-op

- **Category:** Dead code
- **Location:** `crates/strider/src/cache/extend.rs:41-53`
- **What:**
  ```rust
  pub fn extend_predecessors_into<R: rsleigh::MemReader>(
      cache: &mut RegionIrCache,
      cfg: &Cfg<R>,
  ) -> Result<()> {
      // No-op: see correctness note above.  The IR arena does not
      // persist across orchestrator iterations, so we have no graph
      // handle to apply phi extensions to.
      let _ = cache;
      let _ = cfg;
      Ok(())
  }
  ```
- **Why:** The function body is `let _ = cache; let _ = cfg; Ok(())` — a deliberate no-op. The doc says "Production phi extension happens via `extend_predecessors_with_handle`." It's pinned by the test `extend_predecessors_into_no_new_preds_is_noop`, which proves the no-op contract. But the function itself does nothing useful — its only purpose is to exist as a public name.
- **Proposed change:** Either delete the function and its test, OR move the no-op to a `pub fn` clearly named `noop_phi_extension_in_round_one()` to make the intent explicit. The current name suggests it does work; that's misleading.
- **Confidence:** high
- **Risk if applied:** low

### F-018 `IrStrider::build_entry` is a 1-line trampoline

- **Category:** Dead code
- **Location:** `crates/strider/src/strider/mod.rs:55-57`
- **What:**
  ```rust
  /// Emits the function entry node into the IR graph.
  fn build_entry(&mut self) -> Result<()> {
      self.builder.build_entry()
  }
  ```
- **Why:** `IrStrider::build_entry` calls `FunctionBuilder::build_entry` and returns the same Result. The wrapper adds nothing — `analyze_cfg` could call `ir_strider.builder.build_entry()` directly. The other `IrStrider` methods (`process_insn`, `handle_call`, `handle_call_indirect`, `handle_return`, `handle_branch`, `handle_cond_branch`, `handle_call_other`, `handle_switch`, `handle_unresolved_indirect_branch`, `handle_store`, `value_lifter`, `read_vn`, `write_vn`) all do real work — `build_entry` is the only trampoline.
- **Proposed change:** Inline the call site: `ir_strider.builder.build_entry()?;` in `analyze_cfg`. Delete the wrapper.
- **Confidence:** high
- **Risk if applied:** low

### F-019 `RegionIrEntry::empty` is a sentinel constructor used only by tests

- **Category:** Dead code
- **Location:** `crates/strider/src/cache/entry.rs:65-88`
- **What:**
  ```rust
  /// Constructs a sentinel-handle entry for unit tests that don't
  /// need real `NodeId` plumbing. ...
  pub fn empty(start_addr: PcodeInsnAddr) -> Self {
  ```
- **Why:** Its docstring admits "unit tests"; the production callers go through `from_lift_handles`. Making the constructor `pub` exposes a sentinel-only API to external callers, who could construct an inconsistent entry and pass it to the cache helpers.
- **Proposed change:** Mark `pub(crate)` (or `pub(super)` if `cache::tests` is the only caller). It does not need to be on the public API surface.
- **Confidence:** high
- **Risk if applied:** low

### F-020 `GraphRewriter::{new, from_builder, entry, graph}` have no test or production callers

- **Category:** Dead code
- **Location:** `crates/strider/src/rewrite.rs:87-127`
- **What:**
  ```rust
  pub fn new(graph: &'a mut Graph, entry: NodeId) -> Self { ... }
  pub fn from_builder(builder: &'a mut FunctionBuilder) -> Self { ... }
  pub fn entry(&self) -> NodeId { ... }
  pub fn graph(&self) -> &Graph { ... }
  ```
- **Why:** A grep across the workspace shows only `wrap_built` is used (in tests + the `rewrite.rs` doc example). `new`, `from_builder`, `entry`, `graph` have zero callers. The rewrite_tests use only `wrap_built`; tests/graph_rewriter.rs uses only `wrap_built` (6 occurrences).
- **Proposed change:** Drop `new`, `from_builder`, `entry`, `graph` (or mark them `pub(crate)` until an external consumer needs them).
- **Confidence:** high
- **Risk if applied:** low

### F-021 `crate::cache_key_for_region` is `pub` but only test/internal consumers exist

- **Category:** Dead code
- **Location:** `crates/strider/src/cache/mod.rs:84-93`
- **What:** Workspace grep shows the function used only in:
  - Inside strider source (`orchestrator.rs`, `cache/invalidate.rs`, `cache/mod.rs::predecessor_diffs`)
  - Strider's own tests
- **Why:** The public re-export is unused outside strider. The orchestrator uses an internal copy logic; `predecessor_diffs` and `count_uncached_regions` use it via `super`.
- **Proposed change:** Mark `pub(crate)` and remove from `lib.rs`'s public re-export list.
- **Confidence:** high
- **Risk if applied:** low

### F-022 `crate::count_uncached_regions` is `pub` but only tests use it

- **Category:** Dead code
- **Location:** `crates/strider/src/cache/mod.rs:105-117`
- **What:** Function is used only in `tests/tier2_cache.rs`.
- **Why:** No production callers. The orchestrator doesn't call it; the doc says "in round-1 this is informational because the orchestrator re-lifts from scratch each iteration."
- **Proposed change:** Mark `pub(crate)` (test-only would need `#[cfg(test)]` since tests are in a separate crate) — actually the cleanest is to leave it `pub` but document the testing-purpose intent. Or move it to a `tests` module.
- **Confidence:** medium
- **Risk if applied:** low

### F-023 `crate::predecessor_diffs` is `pub` but only tests use it

- **Category:** Dead code
- **Location:** `crates/strider/src/cache/mod.rs:130-145`
- **What:** Used only in `tests/tier2_cache.rs:127, 149` and `extend.rs` doc references.
- **Why:** Same as F-022. The orchestrator does its own per-region predecessor count tracking via `cached_predecessor_count`.
- **Proposed change:** Same as F-022.
- **Confidence:** medium
- **Risk if applied:** low

### F-024 `Strider::arch()` is `pub` but only the orchestrator uses it

- **Category:** Dead code
- **Location:** `crates/strider/src/strider/pipeline.rs:135-137`
- **What:** Function is used only by `orchestrator.rs:760` (`config.strider.arch().endianness`).
- **Why:** Single internal caller. Could be `pub(crate)`.
- **Proposed change:** Mark `pub(crate)`.
- **Confidence:** high
- **Risk if applied:** low

### F-026 `target` re-exports may not be needed for back-compat

- **Category:** Dead code
- **Location:** `crates/strider/src/lib.rs:51`
- **What:**
  ```rust
  pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
  ```
- **Why:** A workspace grep shows external consumers (the `examples` directory + `tests/common/mod.rs`) using these via `strider::CallingConvention`, `strider::SleighArch`, etc. CLAUDE.md says these are re-exported from strider for back-compat. Both forms compile; `target::CallingConvention` would also work. Keeping the re-export is fine, but the docstring says "for back-compat with [old] external callers" — the only consumers are this crate's tests. If the re-export is "internal only", `pub(crate)` would do.
- **Proposed change:** Either confirm the public re-export is genuinely public-API and document it, or drop and let the `target::*` paths fall through.
- **Confidence:** low (depends on planned external API surface)
- **Risk if applied:** medium (would break tests that use `strider::CallingConvention`)

### Duplication & unification

### F-027 `apply_split_invalidation` (orchestrator) duplicates `invalidate_split_regions` (cache)

- **Category:** Duplication & unification
- **Location:** See F-005 (re-listed for taxonomy)
- **Why:** Same as F-005's reasoning.
- **Proposed change:** Same as F-005.
- **Confidence:** high
- **Risk if applied:** low

### F-028 `current_anchor_after_opt` and `anchor_value_input` are the same function

- **Category:** Duplication & unification
- **Location:** `crates/strider/tests/common/tier2_helpers/orchestrator.rs:27-46` vs `:112-124`
- **What:**
  ```rust
  pub(super) fn current_anchor_after_opt(graph: &BuiltFunctionGraph) -> ir::Value {
      let mut found: Option<ir::Value> = None;
      for nid in graph.preorder() {
          if !matches!(graph.graph.node_kind(nid), NodeKind::Return) { continue; }
          let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
          if inputs.len() != 3 { continue; }
          assert!(found.is_none(), "fixture must have exactly one placeholder Return; found a second");
          found = Some(inputs[2]);
      }
      found.expect("fixture must have one placeholder Return after optimisation")
  }
  ...
  pub fn anchor_value_input(graph: &BuiltFunctionGraph) -> Option<ir::Value> {
      for nid in graph.preorder() {
          if !matches!(graph.graph.node_kind(nid), NodeKind::Return) { continue; }
          let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
          if inputs.len() != 3 { continue; }
          return Some(inputs[2]);
      }
      None
  }
  ```
- **Why:** Identical logic, identical body modulo "first match" vs "panic on multiple". The doc-comment on the second one even says "Local copy of the helper at the top of this module because the existing `current_anchor_after_opt` is private." The simpler fix: make one of them public.
- **Proposed change:** Delete `anchor_value_input`; promote `current_anchor_after_opt` to `pub` and have callers handle `Option` (or change its signature to return `Option`).
- **Confidence:** high
- **Risk if applied:** low

### F-029 `make_strider_x86_64()` boilerplate duplicated 5+ times

- **Category:** Duplication & unification
- **Location:** Multiple files
- **What:** The pattern
  ```rust
  let arch = SleighArch::x86_64();
  let probe = BufMemReader::new(Vec::<u8>::new(), 0);
  let regs = Sleigh::new(arch.sla_spec, arch.pspec, probe).expect("probe sleigh").regs().expect("probe regs");
  Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).expect("strider")
  ```
  appears in:
  - `tests/tier2_orchestrator.rs:26-34` (`make_strider_x86_64`)
  - `tests/tier2_in_place_edits.rs:184-191` (inline)
  - `tests/tier2_cache.rs:39-58` (inline as `build_single_region_setup`)
  - `tests/r1_placeholder.rs:65-72`, `:115-122` (inline)
  - `tests/jump_table_lifting.rs:86-94`, `:235-242` (inline)
  - `tests/graph_rewriter.rs:84-90` (inline)
  - `src/indirect_resolve_tier2/orchestrator.rs:1398-1409` (`make_strider_for_test`)
  - `src/strider/pipeline.rs:518-529` (inline)
  - `src/indirect_resolve_tier2/classify.rs` and `src/indirect_resolve_tier2/inplace.rs` use `FunctionBuilder::new_raw` directly to bypass the strider construction
- **Why:** Each test/inline block re-creates the same scaffolding. A `tests/common::strider_x86_64()` (or `tests/common::strider_for(arch)`) helper would unify them. The `tests/common/mod.rs` already exists but doesn't expose a per-arch strider builder.
- **Proposed change:** Add a `pub fn strider_for(arch: Arch) -> Strider` to `tests/common/mod.rs`. Replace the inline scaffolds.
- **Confidence:** high
- **Risk if applied:** low

### F-030 `analyze_cfg`'s region-handles snapshot loop duplicates the field-extraction logic in `populate_cache_from_handles`

- **Category:** Duplication & unification
- **Location:** `crates/strider/src/strider/pipeline.rs:436-488` vs `crates/strider/src/cache/lift.rs:61-73`
- **What:** The snapshot construction in `analyze_cfg` (creating `RegionLiftHandles { entry_control_state, entry_mem_phi, entry_control, entry_memory, exit_control, exit_memory, entry_var_phis, exit_vn_to_value, start_addr, predecessor_count }`) and `populate_cache_from_handles` (which calls `RegionIrEntry::from_lift_handles` doing exactly the inverse mapping) are two halves of the same data marshalling. The intermediate `RegionLiftHandles` struct exists only to bridge the two — it has no other consumer.
- **Why:** A round-trip copy; one struct with the same field set, transformed via a 1-1 field map. The `RegionIrEntry` could be built directly inside `analyze_cfg` if the cache module exposed a builder API.
- **Proposed change:** Drop `RegionLiftHandles` + `populate_cache_from_handles` + `from_lift_handles`; have `analyze_cfg` accumulate `Vec<(MachineInsnAddr, RegionIrEntry)>` directly. The cache then accepts a `Vec<(key, entry)>` insert-list.
- **Confidence:** medium
- **Risk if applied:** medium (touches the public `RegionLiftHandles` API)

### F-031 `decode_space_id`'s "expected at least 1 input" check is duplicated against `handle_call_other`'s check

- **Category:** Duplication & unification
- **Location:** `crates/strider/src/strider/insn/mod.rs:101-110, 148-160`
- **What:** Two functions check `insn.inputs.is_empty()` / `insn.inputs.first()` and bail with the same "too few inputs: expected at least 1, got 0" message.
- **Why:** Both error paths are stylistically identical. A helper `fn first_input_or_err(insn) -> Result<&Vn>` would unify them.
- **Proposed change:** Add a small helper or just leave; cost / benefit is small.
- **Confidence:** medium
- **Risk if applied:** low

### F-032 The "find the unique 3-input Return" idiom is reproduced in ~6 sites

- **Category:** Duplication & unification
- **Location:** Multiple files
- **What:** The pattern
  ```rust
  let mut found: Option<...> = None;
  for nid in graph.preorder() {
      if !matches!(graph.graph.node_kind(nid), NodeKind::Return) { continue; }
      let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
      if inputs.len() != 3 { continue; }
      ...
  }
  ```
  appears in:
  - `src/indirect_resolve_tier2/inplace.rs::tests` (`build_placeholder_graph`)
  - `tests/common/tier2_helpers/orchestrator.rs::current_anchor_after_opt`
  - `tests/common/tier2_helpers/orchestrator.rs::anchor_value_input` (F-028)
  - `tests/common/tier2_helpers/classify.rs` (multiple)
  - `tests/tier2_in_place_edits.rs::locate_placeholder_return`
  - `src/strider/insn/control.rs::tests`
- **Why:** Every site implements the same "find placeholder Return" filter. There's already `opt::find_placeholder_return_for_anchor(graph, anchor_output)` for the anchor-keyed lookup, but no helper for "find the unique placeholder by shape".
- **Proposed change:** Promote `pattern::ret().preceded_by(...)` (already in pattern) or add a `strider::find_placeholder_return(&graph)` helper; replace the inline filters.
- **Confidence:** medium
- **Risk if applied:** low

### F-033 The 1/2/4/8/16/32 → `NodeOutputType` switch is reproduced

- **Category:** Duplication & unification
- **Location:** `crates/strider/src/cache/extend.rs:141-152`
- **What:**
  ```rust
  let ty: ir::node::NodeOutputType = match vn.size {
      1 => ir::node::NodeOutputType::U8,
      2 => ir::node::NodeOutputType::U16,
      4 => ir::node::NodeOutputType::U32,
      8 => ir::node::NodeOutputType::U64,
      16 => ir::node::NodeOutputType::U128,
      32 => ir::node::NodeOutputType::U256,
      other => { ... }
  };
  ```
- **Why:** The same dispatch lives in `ir::node::NodeOutputType::try_from(u32)` (the public conversion). Inlining it sidesteps the canonical converter and risks drift if a new size is added (e.g. `10` for x87 80-bit).
- **Proposed change:** Use `vn.size.try_into()` consistently; let the `try_from` impl handle the dispatch.
- **Confidence:** high
- **Risk if applied:** low

### Simplification

### F-034 The `cache/` 4-submodule split feels over-decomposed

- **Category:** Simplification
- **Location:** `crates/strider/src/cache/mod.rs` and submodules
- **What:** The W6 comment in `mod.rs` says it split a "~1200-line `ir_cache.rs`" into 4 focused submodules. Current sizes:
  - `entry.rs`: 134 LOC (the `RegionIrEntry` struct + `PredecessorHandles`)
  - `extend.rs`: 199 LOC (1 public function + helpers + a no-op shim)
  - `invalidate.rs`: 96 LOC (1 public function)
  - `lift.rs`: 73 LOC (2 functions)
  - `mod.rs`: 145 LOC (3 helpers + re-exports)
  - Total = 647 LOC. The cited "1200 lines" is no longer accurate; the split predates the simplifications that brought it down.
- **Why:** Four submodules for a 650-LOC component cost navigation overhead. Each submodule has 1-2 public functions; the "focus" rationale is thin when the helpers genuinely do touch multiple submodules (`extend_predecessors_with_handle` calls into `entry.rs`'s types and `mod.rs`'s `RegionIrCache` alias).
- **Proposed change:** Consider collapsing `entry.rs` + `mod.rs` into a single `mod.rs`, or `entry.rs` + `lift.rs` (entry is the data, lift is the constructor for it). The 4-way split was sized for a 1200-line file.
- **Confidence:** low
- **Risk if applied:** low

### F-035 `IrStrider::unresolved_branches` is mutated by exactly one method

- **Category:** Simplification
- **Location:** `crates/strider/src/strider/mod.rs:14-30`
- **What:**
  ```rust
  pub struct IrStrider<'a, R: rsleigh::MemReader> {
      pub(crate) strider: &'a Strider,
      pub(crate) builder: ir::FunctionBuilder,
      pub(crate) cfg: &'a cfg::Cfg<R>,
      pub(crate) unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
  }
  ```
- **Why:** `unresolved_branches` is mutated only by `handle_unresolved_indirect_branch` and read only by `analyze_cfg` at the end. It's threading a side-effect through state. An alternative: have `handle_unresolved_indirect_branch` return `(addr, value)` and have `analyze_cfg` collect into a local `Vec` directly. But the existing shape is fine; the field is well-named.
- **Proposed change:** No change. (Listed for completeness; not a problem.) — Actually keep `unresolved_branches` here, it's fine.
- **Confidence:** low
- **Risk if applied:** medium

### F-036 `analyze_cfg`'s `cfg_ir_graph` structural clone could be avoided

- **Category:** Simplification
- **Location:** `crates/strider/src/strider/pipeline.rs:295-323`
- **What:**
  ```rust
  // Step 1: create the structural clone of the CFG graph with None placeholders.
  let mut cfg_ir_graph = cfg.graph.map(|_, _| None::<ir::RegionId>, |_, e| *e);
  ...
  let ir_region_of = |region_id: cfg::RegionId| -> Result<ir::RegionId> {
      cfg_ir_graph
          .node_weight(region_id)
          .copied()
          .flatten()
          .ok_or_else(|| anyhow!("no region {region_id:?} in cfg"))
  };
  ```
- **Why:** Cloning the entire CFG graph just to store one `Option<ir::RegionId>` per node is wasteful: a `HashMap<cfg::RegionId, ir::RegionId>` (or an `IndexMap`) would store the same mapping with zero edge data and no need to clone the petgraph adjacency lists. The fallthrough-edge linker iterates `cfg_ir_graph.edge_indices()` afterwards — that loop could iterate `cfg.graph.edge_indices()` directly (the edge weights are the same since the closure is `|_, e| *e`).
- **Proposed change:** Replace `cfg_ir_graph` with a `Vec<Option<ir::RegionId>>` indexed by `RegionId.index()` (or a plain HashMap). Iterate `cfg.graph.edge_indices()` for the fallthrough loop.
- **Confidence:** medium
- **Risk if applied:** low

### F-037 `apply_in_place_edit::Single` reads the placeholder's inputs twice

- **Category:** Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:499-512, 526-535`
- **What:**
  ```rust
  let ctx = build_anchor_calling_context(graph, placeholder, config, cache);
  let old_exit_control_opt: Option<ir::node::NodeOutputId> = {
      let inputs: Vec<_> = graph.graph.node_inputs(placeholder).into_iter().collect();
      if inputs.len() == 3 { Some(inputs[0]) } else { None }
  };
  let new_return = apply_tail_call(...)?;
  if let Some(old_exit) = old_exit_control_opt {
      let new_inputs: Vec<_> = graph.graph.node_inputs(new_return).into_iter().collect();
      if let Some(new_exit_control) = new_inputs.first().copied() {
          update_cache_exit_handle_after_tail_call(cache, old_exit, new_exit_control);
      }
  }
  ```
- **Why:** `build_anchor_calling_context` ALREADY reads `placeholder`'s inputs at line 617-625 to find the cache entry. The next block re-reads them. A single read at the top of `apply_in_place_edit::Single` would suffice.
- **Proposed change:** Refactor to capture `placeholder_ctrl_in` once and pass it to both `build_anchor_calling_context` and the cache patch step.
- **Confidence:** medium
- **Risk if applied:** low

### F-038 `OrchestratorConfig::rom: Option<Arc<dyn ReadOnlyMemory>>` doc says "cloned per-iteration" but only `as_deref()` is used

- **Category:** Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:151-153, 287, 746-747`
- **What:**
  ```rust
  /// Read-only memory image for the optimiser's `LoadReadOnly`
  /// pass.  `None` to disable.  Cloned per-iteration via
  /// `Arc::clone` (cheap).
  pub rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
  ...
  let rom_ref: Option<&dyn ReadOnlyMemory> = config.rom.as_deref();
  ...
  if let Some(rom) = config.rom.clone() {
      opts_builder = opts_builder.set_read_only_memory(rom);
  }
  ```
- **Why:** The doc comment talks about per-iteration `Arc::clone`, but the orchestrator only `clone()`s once per iteration (in `build_lift_stable`'s opts builder) and `as_deref()`s for the classifier path. Two of the three production reads are `as_deref()`, not clones. The comment overstates the cost model; should match the code.
- **Proposed change:** Update the doc to "Borrowed via `as_deref()` for the classifier path; `Arc::clone`d once per CFG rebuild for the cfg builder's option."
- **Confidence:** high
- **Risk if applied:** low

### F-053 `handle_call_other` allocates a `Vec<ir::Value>` even for arg-less ops

- **Category:** Simplification
- **Location:** `crates/strider/src/strider/insn/mod.rs:110-113`
- **What:**
  ```rust
  let args: Vec<ir::Value> = insn.inputs[1..]
      .iter()
      .map(|vn| self.read_vn(vn))
      .collect::<Result<_>>()?;
  ```
- **Why:** Every CallOther goes through this `Vec` collection. Most CallOthers (`cpuid`, `rdtsc`, `setISAMode`) have 0 args. The `build_call_other` API takes `&[ir::Value]`; `Vec::new()` for empty is cheap, but the `collect::<Result<_>>` chain is non-trivial. Inlining the early-return for empty would save the allocation.
- **Proposed change:** Either accept the cost (negligible per-op) and document, or use a smallvec / arrayvec. Most likely leave as is — the Vec is short-lived.
- **Confidence:** low
- **Risk if applied:** low

### F-054 Three classifier shim entry points (`classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`) over-generate options

- **Category:** Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/classify.rs:13-58`
- **What:**
  ```rust
  pub fn classify_anchor(graph, anchor_output, link_register_vn) -> ...
  pub fn classify_anchor_with_rom(graph, anchor_output, link_register_vn, rom: Option<&dyn ROM>) -> ...
  pub fn classify_anchor_with_rom_and_sp(graph, anchor_output, link_register_vn, rom, stack_ptr_vn) -> ...
  ```
- **Why:** Three thin shims, each adding one more optional parameter. The `with_rom_and_sp` variant calls `opt::analyze_known_bits(graph)` which the others don't — but the orchestrator only ever calls the most-options variant. The others exist for tests + the Python (planned) layer. A single `classify_anchor(graph, anchor_output, ClassifierOptions { lr, rom, sp })` would unify.
- **Proposed change:** Collapse to one entry point with an options struct. The options struct already exists conceptually as `OrchestratorConfig`'s subset.
- **Confidence:** medium
- **Risk if applied:** low

### F-055 `apply_link_register` shim is a pure re-export

- **Category:** Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/inplace.rs:25-32`
- **What:**
  ```rust
  pub fn apply_link_register(
      fg: &mut BuiltFunctionGraph,
      placeholder_return: NodeId,
      ret_val_outputs: &[NodeOutputId],
  ) -> Result<()> {
      opt::apply_link_register(fg, placeholder_return, ret_val_outputs)
  }
  ```
- **Why:** No transformation: 1-to-1 forwarding. The doc says "Retained as a strider-side entry point so the orchestrator and integration tests can call into the classifier under a stable strider path." This was load-bearing pre-anyhow (when error types differed); post-anyhow the types are identical. Could `pub use opt::{apply_link_register, apply_tail_call};` directly.
- **Proposed change:** Replace with `pub use opt::{apply_link_register, apply_tail_call};` in `indirect_resolve_tier2/mod.rs`. Drop `inplace.rs` (or shrink to `pub use opt::indirect_branch_resolve::inplace::*;`).
- **Confidence:** high
- **Risk if applied:** low

### F-056 `apply_tail_call` shim is also a pure re-export

- **Category:** Simplification
- **Location:** `crates/strider/src/indirect_resolve_tier2/inplace.rs:48-65`
- **What:** Same shape as F-055, six-arg pure forward.
- **Why:** Same reason as F-055.
- **Proposed change:** Same as F-055.
- **Confidence:** high
- **Risk if applied:** low

### Readability

### F-039 `is_tail_call`'s `end_exclusive` boundary case isn't documented in the function's own comment

- **Category:** Readability
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:436-450`
- **What:**
  ```rust
  fn is_tail_call<B>(target: u64, config: &OrchestratorConfig<'_, B>) -> bool ... {
      if target < config.start_addr && !config.allow_code_before_start_addr {
          return true;
      }
      if let Some(fn_max_size) = config.fn_max_size {
          let end_exclusive = config.start_addr.saturating_add(fn_max_size);
          if end_exclusive <= target {
              return true;
          }
      }
      false
  }
  ```
- **Why:** The boundary `target == start_addr + fn_max_size` is a tail call (because `end_exclusive`); the test `is_tail_call_above_fn_max_size_is_tail_call` pins this. But the function comment says "tail call iff lies outside the function range" — ambiguous. A reader has to infer from the `<=` operator that the upper bound is exclusive.
- **Proposed change:** Add a one-line code comment: `// `[start_addr, start_addr + fn_max_size)` is the in-function half-open range; targets ≥ the upper bound are tail calls.`
- **Confidence:** high
- **Risk if applied:** low

### F-040 `analyze_cfg`'s docstring lists 9 stale `ErrorKind::*` variants

- **Category:** Readability
- **Location:** `crates/strider/src/strider/pipeline.rs:277-286`
- **What:**
  ```rust
  /// # Errors
  ///
  /// Returns an [`Error`] wrapping the underlying failure when:
  /// - the IR builder fails to create or look up a region
  ///   ([`ErrorKind::CfgNoRegion`], [`ErrorKind::IrError`]);
  /// - an instruction translation fails (any of the per-opcode error
  ///   variants in [`ErrorKind`]: [`ErrorKind::UnimplementedOpcode`],
  ///   [`ErrorKind::MissingOutputVn`], [`ErrorKind::UnsupportedVnSpace`],
  ///   [`ErrorKind::UnsupportedRegSize`], etc.);
  /// - the CFG itself is malformed ([`ErrorKind::CfgError`]).
  ```
- **Why:** None of `ErrorKind::CfgNoRegion`, `ErrorKind::IrError`, `ErrorKind::UnimplementedOpcode`, `ErrorKind::MissingOutputVn`, `ErrorKind::UnsupportedVnSpace`, `ErrorKind::UnsupportedRegSize`, `ErrorKind::CfgError` exist after the anyhow migration. The doc comment is post-merge drift.
- **Proposed change:** Replace with "Returns `anyhow::Error` wrapping the underlying failure (CFG region lookup, instruction translation, IR construction)."
- **Confidence:** high
- **Risk if applied:** low

### F-041 `Strider::new` docstring lists `ErrorKind::TargetError` (stale)

- **Category:** Readability
- **Location:** `crates/strider/src/strider/pipeline.rs:107-112`
- **What:**
  ```rust
  /// # Errors
  ///
  /// Returns [`ErrorKind::TargetError`] (wrapping
  /// [`target::ErrorKind::UnknownRegName`]) if any register name in
  /// `calling_convention` (including the stack pointer) does not resolve
  /// against `sleigh_regs`.
  ```
- **Why:** Same drift as F-040.
- **Proposed change:** Replace.
- **Confidence:** high
- **Risk if applied:** low

### F-042 `GraphRewriter::apply_rule` docstring claims `crate::ErrorKind::PatternError` exists

- **Category:** Readability
- **Location:** `crates/strider/src/rewrite.rs:154-157`
- **What:**
  ```rust
  /// Propagates the rule closure's first non-skip error, wrapped in
  /// [`crate::ErrorKind::PatternError`] via the `pattern::Error` →
  /// `crate::Error` bridge.
  ```
- **Why:** Same drift; `ErrorKind::PatternError` doesn't exist.
- **Proposed change:** Replace with "Propagates the rule closure's first non-skip error via `anyhow`."
- **Confidence:** high
- **Risk if applied:** low

### F-043 `cache_key_for_region` docstring claims `ErrorKind::CfgNoRegion`

- **Category:** Readability
- **Location:** `crates/strider/src/cache/mod.rs:79-83`, similar in `cache/invalidate.rs:44-47`
- **What:** Same drift.
- **Proposed change:** Replace.
- **Confidence:** high
- **Risk if applied:** low

### F-044 Stale codename comments — F2/F5/F6/F7/W1/W2/W5/W6/W7/W9/G1/G2/H0/M1/M2/R1.4/R2/R3/R4/BUG-5/BUG-25/BUG-30 — pervasive

- **Category:** Readability
- **Location:** Multiple files (49 occurrences)
- **What:** Examples:
  - `src/cache/extend.rs:93`: `// W1 — rollback contract: every append below contributes one input to a phi node.`
  - `src/strider/insn/control.rs:7`: `/// F7 — emit an If-ladder dispatching idx against targets_and_regions.`
  - `src/strider/insn/control.rs:122`: `// BUG-25 the cfg builder reclassifies a `Branch`...`
  - `src/strider/insn/control.rs:200`: `// W9: prefer the orchestrator's pinned NodeOutputId when available`
  - `src/strider/mod.rs:29`: `// R1.4: empty until a deferred BranchIndirect is lifted.`
  - `src/indirect_resolve_tier2/mod.rs:14`: `//! orchestrator (lands in R3).`
  - `src/indirect_resolve_tier2/inplace.rs:1`: `//! In-place IR edits — F5 shim.`
- **Why:** The user noted in CLAUDE.md / memory that codename references should be stripped. These are stragglers post-anyhow merge.
- **Proposed change:** Replace each codename with a description of the contract / behaviour. E.g. "W1 — rollback contract" → "Rollback contract: every append…"; "F7" → "Switch terminator support".
- **Confidence:** high
- **Risk if applied:** low

### F-045 `cfg_ir_graph.map(|_, _| None::<ir::RegionId>, |_, e| *e)` clones the entire graph for trivial mapping

- **Category:** Readability / Performance
- **Location:** `crates/strider/src/strider/pipeline.rs:295-297`
- **What:** See F-036.
- **Why:** Same as F-036.
- **Proposed change:** Same as F-036.
- **Confidence:** medium
- **Risk if applied:** low

### F-046 `RegionLiftHandles::predecessor_count` is captured at lift, used only by `from_lift_handles`

- **Category:** Readability
- **Location:** `crates/strider/src/strider/pipeline.rs:21-24`, `crates/strider/src/cache/entry.rs:99-112`
- **What:**
  ```rust
  /// Number of CFG predecessors at lift time.
  pub predecessor_count: usize,
  ```
- **Why:** This field is part of the snapshot only because `RegionIrEntry::from_lift_handles` copies it to `cached_predecessor_count`. Two near-identical names for the same data. Reducing the snapshot to "data the cache pinning needs" plus letting the cache compute predecessor count on insert (from the CFG it was given) would simplify.
- **Proposed change:** Drop `RegionLiftHandles::predecessor_count`; compute it on the cache side at insert time.
- **Confidence:** medium
- **Risk if applied:** low

### F-047 `IrStrider::value_lifter` is `pub(super)` but only `vn_io.rs` uses it

- **Category:** Readability
- **Location:** `crates/strider/src/strider/vn_io.rs:17`
- **What:**
  ```rust
  pub(super) fn value_lifter(&mut self) -> pcode_lift::ValueLifter<'_, R> { ... }
  ```
- **Why:** `value_lifter` is called by `vn_io.rs::read_vn`/`write_vn` AND by `insn/mod.rs::process_insn`. So `pub(super)` is the right scope (insn/mod is a sibling of vn_io). Confirmed via grep. (Listed for completeness; no change needed.) Actually re-checking the grep output: `value_lifter` is referenced from `insn/mod.rs:36` (`if self.value_lifter().lift(insn)?`). So `pub(super)` is correct.
- **Proposed change:** No change needed; remove this finding.
- **Confidence:** low
- **Risk if applied:** low

### F-048 `tests/common/tier2_helpers/mod.rs` `#[allow(unused_imports)]` masks dead-helper signals

- **Category:** Readability
- **Location:** `crates/strider/tests/common/tier2_helpers/mod.rs:53-63`
- **What:**
  ```rust
  #[allow(unused_imports)]
  pub use classify::{
      build_bx_lr_scenario, build_initial_var_target_scenario_x86_64, ...
  };
  ```
- **Why:** The comment notes that "each integration test compiles `common::tier2_helpers` independently and uses only a subset of these helpers, so per-test the other re-exports look 'unused'." Suppressing the warning is necessary, but it also hides the case where a helper is GENUINELY unused across the entire test suite.
- **Proposed change:** Either bump the helpers to a separate `tests/common::tier2_fixtures` crate (one consumer per test file, no `#[allow]`), or add a `#[cfg(any(...))]` whitelist that names which test file consumes which helper.
- **Confidence:** low
- **Risk if applied:** low

### Performance

### F-049 `analyze_cfg`'s `ir_region_of` closure is called per pcode-insn

- **Category:** Performance
- **Location:** `crates/strider/src/strider/pipeline.rs:308-314`
- **What:**
  ```rust
  let ir_region_of = |region_id: cfg::RegionId| -> Result<ir::RegionId> {
      cfg_ir_graph
          .node_weight(region_id)
          .copied()
          .flatten()
          .ok_or_else(|| anyhow!("no region {region_id:?} in cfg"))
  };
  ```
- **Why:** Called per branch / cond-branch / switch / fallthrough — i.e. up to once per pcode insn. Each call does an O(1) `node_weight` lookup, but allocates an error string on the unhappy path even when the call succeeds (`ok_or_else` defers the alloc, so this is fine actually). Real cost: dispatch through the closure indirection. Negligible for a single CFG; might matter at scale.
- **Proposed change:** Pre-compute a `Vec<Option<ir::RegionId>>` indexed by `RegionId.index()` once at the top of `analyze_cfg`. Inline the lookup. Combined with F-036.
- **Confidence:** low
- **Risk if applied:** low

### F-050 The orchestrator's per-iteration `lr_vn` / `sp_vn` reads are constant

- **Category:** Performance
- **Location:** `crates/strider/src/indirect_resolve_tier2/orchestrator.rs:267-275`
- **What:**
  ```rust
  for _iter_idx in 0..cap {
      ...
      let lr_vn = config.strider.calling_convention().link_register_vn;
      let sp_vn = Some(config.strider.calling_convention().stack_ptr_vn);
      ...
  }
  ```
- **Why:** `config.strider.calling_convention()` returns `&BuiltCallingConvention`; the lr / sp varnodes don't change across iterations. Tiny win, but trivially hoistable.
- **Proposed change:** Move the two `let` bindings above the `for _iter_idx` loop.
- **Confidence:** high
- **Risk if applied:** low

### F-051 `find_all_unique_vns` uses `HashSet` then sorts — could use `Vec` + `sort_unstable` + `dedup`

- **Category:** Performance
- **Location:** `crates/strider/src/strider/pipeline.rs:243-261`
- **What:**
  ```rust
  let mut all_vns: std::collections::HashSet<rsleigh::Vn> = std::collections::HashSet::new();
  for region in cfg.regions() {
      for wrapped in region.insns.iter() {
          for vn in wrapped.insn.all_vns() {
              all_vns.insert(vn);
          }
      }
  }
  let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
  vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));
  vns
  ```
- **Why:** HashSet pays O(n) hashing on insert + final O(n) collect. A direct `Vec::collect()` + `sort_unstable` + `dedup` is faster for small N (avoids hashing, especially since `Vn`'s hash hashes 3 u64-equivalent fields). Most functions have O(50) varnodes; the current approach is fine.
- **Proposed change:** Switch to Vec + sort + dedup if profiling shows a hotspot. Otherwise no change.
- **Confidence:** low
- **Risk if applied:** low

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| `crates/strider/Cargo.toml` | reviewed | F-016 (rustc-hash unused) |
| `crates/strider/src/lib.rs` | reviewed | F-015 (ir_cache alias), F-026 (re-exports) |
| `crates/strider/src/error.rs` | reviewed | F-014 (3-line file) |
| `crates/strider/src/rewrite.rs` | reviewed | F-020 (unused ctors), F-042 (doc drift) |
| `crates/strider/src/rewrite_tests.rs` | reviewed | uses `wrap_built` only |
| `crates/strider/src/cache/mod.rs` | reviewed | F-021/22/23 (pub items only used by tests) |
| `crates/strider/src/cache/entry.rs` | reviewed | F-019 (`empty` ctor sentinel) |
| `crates/strider/src/cache/extend.rs` | reviewed | F-017 (no-op `extend_predecessors_into`), F-033 (size-dispatch dup) |
| `crates/strider/src/cache/invalidate.rs` | reviewed | F-013 (key/start_addr drift) |
| `crates/strider/src/cache/lift.rs` | reviewed | F-030 (handles → entry duplication) |
| `crates/strider/src/cache/tests.rs` | reviewed | comprehensive cache unit tests |
| `crates/strider/src/strider/mod.rs` | reviewed | F-018 (`build_entry` trampoline) |
| `crates/strider/src/strider/pipeline.rs` | reviewed | F-002, F-024, F-036, F-040-41, F-44, F-045, F-049, F-051 |
| `crates/strider/src/strider/vn_io.rs` | reviewed | thin shim |
| `crates/strider/src/strider/insn/mod.rs` | reviewed | F-010, F-011, F-012, F-031, F-052, F-053 |
| `crates/strider/src/strider/insn/control.rs` | reviewed | F-044 (codenames) |
| `crates/strider/src/indirect_resolve_tier2/mod.rs` | reviewed | thin shim |
| `crates/strider/src/indirect_resolve_tier2/classify.rs` | reviewed | F-054 (3-arity API) |
| `crates/strider/src/indirect_resolve_tier2/inplace.rs` | reviewed | F-001, F-055, F-056 |
| `crates/strider/src/indirect_resolve_tier2/orchestrator.rs` | reviewed | F-003-F-009, F-027, F-037-F-038, F-050 |
| `crates/strider/tests/abi.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/arithmetic.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/builtins.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/calling_convention.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/calls.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/common_smoke.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/complex_patterns.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/control.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/floats.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/globals.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/graph_rewriter.rs` | skimmed | uses `wrap_built` exclusively |
| `crates/strider/tests/indirect_branch.rs` | skimmed | F-029 (Strider boilerplate) |
| `crates/strider/tests/jump_table_lifting.rs` | reviewed | F-029 (Strider boilerplate) |
| `crates/strider/tests/memory.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/patterns.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/r1_placeholder.rs` | reviewed | F-029 (boilerplate) |
| `crates/strider/tests/read_reg_vn_truncate.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/stack.rs` | skimmed | per_arch fixture-driven |
| `crates/strider/tests/tier2_cache.rs` | reviewed | uses `cache_key_for_region`, `count_uncached_regions`, etc. |
| `crates/strider/tests/tier2_classify.rs` | reviewed | uses tier2_helpers fixtures |
| `crates/strider/tests/tier2_in_place_edits.rs` | reviewed | F-029 (Strider boilerplate), F-032 (placeholder lookup dup) |
| `crates/strider/tests/tier2_jump_table.rs` | skimmed | uses tier2_helpers fixtures |
| `crates/strider/tests/tier2_optimizer_tiers.rs` | skimmed | exercises pipeline split |
| `crates/strider/tests/tier2_orchestrator.rs` | reviewed | F-029 (`make_strider_x86_64`) |
| `crates/strider/tests/common/mod.rs` | reviewed | per_arch_test! macro infrastructure (massive) |
| `crates/strider/tests/common/tier2_helpers/mod.rs` | reviewed | F-048 (allow unused), W7 split notes |
| `crates/strider/tests/common/tier2_helpers/orchestrator.rs` | reviewed | F-028 (anchor helper dup) |
| `crates/strider/tests/common/tier2_helpers/classify.rs` | reviewed | F-029, F-032 |
| `crates/strider/examples/strider.rs` | reviewed | uses `analyze_cfg`, `build_optimizer_pipeline`, `dot` |

## Out-of-scope items observed

These are noted but NOT flagged per the review's hard rules.

- The `unsafe { rsleigh::VnSpace::by_id(...) }` in `decode_space_id` calls into rsleigh; the safety contract lives there.
- `opt::find_placeholder_return_for_anchor` is used by the orchestrator; that helper's correctness is `opt`'s concern.
- `apply_link_register` / `apply_tail_call` correctness post F-022 is verified in `opt`; the strider shim correctly forwards the new `[ctrl, mem, ret_val_0, ...]` shape (no slot 2 assumption in the strider side).
- Several integration tests in `tests/common/tier2_helpers/classify.rs` synthesise `ValuePhi` nodes via raw `graph.create_node` and bypass the validator's per-predecessor arity check. The `classify_anchor` unit tests in `src/indirect_resolve_tier2/classify.rs::tests` do the same. This is documented as intentional ("the unit tests exercise `classify_anchor` against fully synthetic shapes that the validator would reject in production"); flag is on whether the validator's contract is too strict (`ir`'s concern) or whether the classifier should accept only validator-passing graphs (`opt`'s concern).
- The `per_arch_test!` macro in `tests/common/mod.rs` uses 15+ `__scan_ignore_<arch>!` helper macros — heavy but correct; the macro expansion strategy is solid.
- The `tier2_optimizer_tiers.rs` test file pins the stable / destructive split; correctness hinges on `opt::stable_default_pipeline` and `opt::destructive_default_pipeline` partition (an `opt` concern).

## Stopped here marker

Review complete; no stop-here marker.
