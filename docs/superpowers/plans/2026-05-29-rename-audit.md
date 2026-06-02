# Rename-audit remediation

> Source: 8 read-only naming-review subagents (one per crate cluster). Each
> rename must be **re-verified against the code body** before applying, and is
> behavior-preserving (gate: per-crate `cargo test` + `clippy` must stay green;
> renames are mechanical). Tiering: **DO** = clear misnomer / stale-from-refactor;
> **DEFER** = subjective or high-churn, documented for a follow-up.

## DO — clear renames (by crate)

### strider-ir
- `build_control_phi` → `build_vn_phi` (builder/nodes.rs:692) — builds a varnode-tagged value Phi, not control. pub(super), 1 call site (vars.rs:137). **the canonical example.**
- `is_cs` → `is_region` (walk/mod.rs:131) — stale "ControlState" local; tests `NodeKind::Region`. private.
- DOC: `wide_consts`→`wide_const_interner` field refs (kind.rs:95/99, output_type.rs:188, graph_invariants.rs:321, wide_const.rs:6/23, function_dot/{label.rs:127,raw.rs:59}) — VERIFY whether `wide_consts()` is an accessor first; if not, fix to `wide_const_interner`. `Bool`→`I1` in kind.rs:93, op_kinds.rs:10/124. "four NodeId-keyed side tables"→"six" (compact.rs:6-7, graph/mod.rs:104).

### strider-analyze/pattern
- `next_control_node` → `next_unique_consumer` (matcher/consumer.rs:17) — returns the unique consumer of ANY output kind (used for memory walks too); the code's own comment says the name lies. pub(crate), ~5 refs.
- `NodeKindCheck` → `PostMatchFn` (pat/node_pat.rs:36) — it's the binding-capable post-match hook, not a kind check. pub(crate), ~15 refs.
- DOC: matcher/mod.rs preorder/graph cache comments; consts.rs:33 "(always Bool)"→I1; test `chained_controlstates_walk_through` + `cs_*` comments → Region.

### strider-analyze/opt
- `bool_neg_node`/`bool_neg_uses_before` → `bit_not_node`/`bit_not_uses_before` (if_cond_inversion/mod.rs:121-137) — the only code identifiers still naming the removed BoolNeg. private locals.
- `find_placeholder_return_for_anchor` → `find_indirect_branch_placeholder` (indirect_branch_resolve/mod.rs:101) — returns an IndirectBranch placeholder, never a Return. pub(crate), ~4 refs (orchestrator+tests).
- `try_remove_region_phi` → `try_collapse_single_pred_region` (redundant_phis/mod.rs:33) — a Region isn't a phi. private, 1 ref.
- `remove_phis` → `try_simplify_phi_like` (redundant_phis/mod.rs:68) — dispatches Phi|MemPhi|Region, may rewire. private, 2 refs.
- DOC/STRING: `"control-state inputs"` bail string (redundant_phis:147)→"Region inputs" (user-visible). `StackStore`/`StackStorePhi`/`ValuePhi`/`VarPhi` comment vocab (load_forward 589-635/142/163/459/701, call_stack_args 5/17/23/72/93/196/331, function_args:24, jump_table:763) → real kinds Store/MemPhi/Phi. BITCAST_EXTEND_RULES doc (rules.rs:292), IDENTITY_RULES "single-operand" doc, dangling `walk_control_for_if_bound` ref (jump_table:775).

### strider-analyze core/orchestrator
- `SpecialTerm::PendingIndirect` → `UnresolvedIndirect` (strider/pipeline.rs:611) — lone 3rd synonym; terminator/field/handler all say "Unresolved". ~4 refs.
- `RegionIndex::region_for_placeholder` → `exit_vars_for_placeholder` (orchestrator/mod.rs:139) — returns the exit vn→value table, not a region. private, 1 caller.
- `VnCache::scan_new` → `scan_new_regions` (orchestrator/mod.rs:300) — returns the full accumulated set, not the delta. private, 1 caller.
- `build_switch_if_ladder` param `caller_region` → `dispatch_region` (strider/insn/control.rs:56) — it's the switch/dispatch region.
- DOC: `ExitVnToValue` comment (orchestrator:123) claims NodeOutputId-keyed but it's Vn-keyed; rewrite.rs Graph→Function prose (try_wrap_built doc:71).
- LEAVE: `handle_return` also handles BranchIndirect — both emit a CC Return, name defensible; add a doc note instead of renaming.

### strider-lift
- `IfRegionState` → `IfRegionSuccessors` (cfg/query.rs:53) — just a pair of successor Options, no state; stale cfg/mod.rs:15 doc reinforces it. pub, ~1 ext ref (analyze insn/control.rs). + fix the doc.
- `ProcessInsnRes`/`FinishedProcessing`/`DidntFinishProcessing` → `RegionStep`/`RegionClosed`/`Continue` (region_builder.rs:51-56) — file-local, awkward. pub(crate), file-only.
- DOC: stale "Unconditional edge"/"unweighted" comments (region_builder.rs:265/294).

### strider-reader / strider-target
- `section_is_code_or_readonly` → `section_is_exec_or_readonly` (reader sections.rs:47) — passes .rodata too (body is `is_exec || !is_writable`). private, 1 use.
- `non_symbol_is_malformed` → `require_symbol_target` (reader relocations.rs:577) — controls routing, not an IS-A. private, 3 sites.

### strider-py / macros
- `count_loop_headers` → `count_regions` (function.rs:256) — counts Region nodes (loop detection removed). **Python-facing/breaking** — update pymethod + .pyi + tests.
- `function_max_size` (MemoryMap, reader.rs:396) → `symbol_addr_and_size` — returns a `(addr,size)` tuple, name implies scalar; collides with the run kwarg. **Python-facing/breaking** — update + .pyi + tests.
- `PyDeadBranchElim` → `PyDeadBranchElimination` (opt.rs:293) — Rust-internal only (Python name `"DeadBranchElim"` separate). ~4 refs.
- `take_pending_control_flow_peek` → `peek_pending_control_flow` (pattern.rs:385) — `take_` implies drain, it's a non-destructive peek. pub(crate), ~2 refs.

### dot
- DOC: module doc "Strider graphs" → "any graph implementing `GraphDotDumper`" (generic-crate domain leak).
- `build_dot` → `render_dot_string` (lib.rs:369) — private, single-shot render not a builder. 0 ext.

## DEFER — subjective / high-churn (recommend as follow-up, NOT in this PR)
- `no_memory_clobber`→`preserves_memory` (target): genuine double-negative but ~10 cross-crate + ~20 preset rows; pure rename, no value flip. Name isn't *wrong*, just awkward.
- `CallOtherClass`→`CallOtherKind` (target, ~20 cross-crate): convention (`*Kind`) not correctness.
- `CallingConvention::build`→`resolve` (target, ~5 + py): subjective.
- `GraphDotDumper::dump_as_dot`→`emit_node_dot` (dot, trait, ~6 cross-crate impls): genuine homonym with `GraphDot::dump_as_dot`; moderate churn — DO IF a dot-crate pass is run, else defer.
- test-utils `build_fn`→`make_builder` (~29), `build_fn_single_region`→`make_builder_single_region` (~60), `make_empty_fn`→`make_bare_fn` (~53), `make_sp_fn`→`make_sp_only_fn` (~11): real ("build_fn" returns a *builder*) but ~140 test-only call sites — huge churn for test-helper names.
- `build_int_*_operation`→`build_int_*_op` (ir, ~405 refs): consistency only.
- macro `*DefInner`→`Py*Inner`; `dark_cfg`→`dark_courier`; `NopTracker`→`NoopVisitTracker`; `entity_preorder/postorder`→`dense_*`; worklist field `worklist`→`queue`; `CoverageIndex`; `MemRegionsLookupTable`; `CallOtherRow`; `stack_ptr_reg_name`; `apply_elf_relocations_with_extender`; `endian_le: bool`→`Endianness` (type change, not rename — C4 leftover).
- `"UnkSytemRegRead"` typo: a Sleigh user-op string — load-bearing, do NOT touch.

## Then
Python binding docs (enforcement test already covers; add for any renamed binding), confirm dead-code + panic audit still clean, propagate renames into CLAUDE.md/READMEs, full gate, final review, PR → rewrite/strider.
