# Round 7 — Logic-Flow Simplifications

Audit of `crates/{cfg, ir, opt, pattern, pcode-lift, reader, strider, strider-py, target}` looking for special-case branches whose logic could collapse into the general case.

Findings keyed by hypothesis number from the prompt where applicable; "F-N" entries are independent finds. Each item lists file:line, the duplicated general case, a unification sketch, risk, and rough LOC delta.

---

## F-1 — `is_addr_tail_call` lower-bound check (Hypothesis 2)

**File:** `crates/cfg/src/cfg/query.rs:25-45`

**Special-case shape.** The fn carries two booleans intermixed: `lower_bound_strict = fn_max_size.is_some() || !allow_code_before_start_addr`. The "bounded" path adds an upper-bound test; the "unbounded" path skips it. Two distinct conditional branches (`if target < start && lower_bound_strict { … }`, `if let Some(max) = fn_max_size { … }`).

**General case.** Both branches reduce to a half-open range test. Treat *unbounded* as `end_exclusive = u64::MAX` and *"allow code below start"* as widening the lower bound to 0.

**Sketch.**
```rust
pub fn is_addr_tail_call(target: u64, start: u64, fn_max_size: Option<u64>,
                         allow_code_before_start_addr: bool) -> bool {
    let lower = if fn_max_size.is_none() && allow_code_before_start_addr { 0 } else { start };
    let upper = match fn_max_size {
        Some(max) => start.saturating_add(max),
        None      => u64::MAX,
    };
    target < lower || target >= upper
}
```
The `u64::MAX` sentinel makes "unbounded above" `target >= u64::MAX` — false for every reasonable address but technically wrong for `target == u64::MAX`. The existing `saturating_add` already collapses to `u64::MAX` on overflow, and the existing test (`is_tail_call_fn_max_size_saturates_on_overflow`) asserts `is_tail_call(u64::MAX) == true` *only when* a `fn_max_size` is supplied. The unbounded `target == u64::MAX` case is unobserved, but to preserve current behaviour use `Option<u64>` with a `match` rather than the sentinel.

**Risk.** LOW. Unit tests at `crates/strider/src/orchestrator.rs:979-1017` pin every branch — they should still pass.

**LOC delta.** −5 to −8 (collapses 14 LoC into ~7).

---

## F-2 — Mode flags in `pcode_lift::vn_io::calculate_reg_shift_from_container` (Hypothesis 4)

**File:** `crates/pcode-lift/src/vn_io.rs:183-196`

**Special-case shape.** A `match self.endianness { Little => …, Big => … }` branching on two distinct integer-arithmetic formulas.

**General case.** Both formulas measure "byte distance from the container's least-significant byte to `reg`'s least-significant byte." For LE the LSB is at offset 0; for BE it's at offset `container.size - 1`. Compute the LSB offset once, subtract `reg.addr_off`, multiply by 8.

**Sketch.**
```rust
pub(crate) fn calculate_reg_shift_from_container(
    &self, reg: &rsleigh::Vn, container: &rsleigh::Vn,
) -> u64 {
    // LSB byte index inside the container, in container-local coordinates.
    let lsb_local = match self.endianness {
        Endianness::Little => 0u64,
        Endianness::Big    => container.size as u64 - 1,
    };
    let reg_local = reg.addr_off - container.addr_off;
    // LE: shift right by reg_local*8 to bring reg to LSB.  BE: container's
    // LSB sits `lsb_local` bytes from container start; reg's LSB sits
    // `reg_local + reg.size - 1` bytes from container start; difference *8.
    let reg_lsb_local = reg_local + match self.endianness {
        Endianness::Little => 0,
        Endianness::Big    => reg.size as u64 - 1,
    };
    8 * (lsb_local.abs_diff(reg_lsb_local))
}
```
This is *more* code than the current version, not less — the LE/BE asymmetry (LE measures distance from container start; BE measures distance from container end) genuinely needs both arms. **Verdict: SKIP.** Marked here so future readers don't re-explore.

**Risk.** N/A.
**LOC delta.** 0.

---

## F-3 — `CondBranch` 4-arm match on (true_oob, false_oob) (Hypothesis 1)

**File:** `crates/cfg/src/cfg/builder/region_builder.rs:328-387`

**Special-case shape.** A 2x2 match `(true_oob, false_oob)` with three concrete arms (both-in, both-out, exactly-one-out). The "exactly-one-out" arm splits *again* on `self.insns.len() > 1` to handle a degenerate-empty case.

**General case.** "For each successor, drop OOB and keep in-range; emit Branch on 1, CondBranch on 2, TailCall on 0." The hypothesis suggests collapsing further. Reality check:

- "Both in" needs *two* `work_queue` pushes with `IfCaseTrue` + `IfCaseFalse`.
- "Exactly one in" pushes one with `Branch` AND must `self.insns.pop()` (discard the trailing `CondBranch` insn that the IR loop would otherwise re-route).
- "Both out" must skip `insns.pop()` because `SpecialTerm::TailCall::skips_opcode` accounts for the trailing pcode op separately.

The ancillary differences (which edge kind to push, whether to pop trailing insn) all key off how many successors are in-range, so a unified "collect in-range, dispatch on count" form *does* fold cleanly:

**Sketch.**
```rust
let in_range: SmallVec<[(PcodeInsnAddr, RegionEdgeKind); 2]> = [
    (target_addr,    RegionEdgeKind::IfCaseTrue,  true_oob),
    (next_insn_addr, RegionEdgeKind::IfCaseFalse, false_oob),
].into_iter().filter(|(_, _, oob)| !*oob).map(|(a, k, _)| (a, k)).collect();
match in_range.as_slice() {
    [a, b] => {
        let r = self.finish_current_region(RegionTerminator::CondBranch)?;
        self.builder.work_queue.push((Some((r, a.1)), a.0));
        self.builder.work_queue.push((Some((r, b.1)), b.0));
    }
    [(addr, _)] => {
        if self.insns.len() > 1 {
            self.insns.pop();
            let r = self.finish_current_region(RegionTerminator::Branch)?;
            self.builder.work_queue.push((Some((r, RegionEdgeKind::Branch)), *addr));
        } else {
            self.finish_current_region(RegionTerminator::TailCall { target: addr.machine_addr.addr })?;
        }
    }
    [] => { self.finish_current_region(RegionTerminator::TailCall { target: target_addr.machine_addr.addr })?; }
    _ => unreachable!(),
}
```

**Risk.** MED. The single-in-range arm has the subtle `insns.pop()` and degenerate-empty fallback that must survive intact. Test coverage at `crates/cfg/tests/condbranch_oob.rs` (if exists) needs verifying.

**LOC delta.** Roughly 0 to −5; the structural cleanup is the win, not LOC.

---

## F-4 — `Region.variables: SecondaryMap<VarId, NodeOutputId>` sentinel (Hypothesis 5)

**File:** `crates/ir/src/region.rs:36, 39`

**Special-case shape.** `SecondaryMap` returns the default `NodeOutputId(0)` for unset entries. Every reader of `regions[r].variables[v]` and `initial_variables[v]` silently gets a sentinel when the var was never written. The map is treated as total ("every VarId has an entry") because in practice it is — `set_entry_region` and `create_region` initialise every key — but the *type* doesn't enforce it.

**General case.** `SecondaryMap<VarId, Option<NodeOutputId>>` with explicit `None` for unset entries. Forces every reader to handle the unset case rather than silently aliasing the dummy id.

**Sketch.** Same shape, change the value type and update the (small number of) call sites:
```rust
pub fn read_variable_from_id(&self, var_id: VarId) -> Result<NodeOutputId> {
    let region_id = self.require_cur_region()?;
    self.regions[region_id].variables[var_id]
        .ok_or_else(|| anyhow!("variable {var_id:?} unset in region {region_id:?}"))
}
```

**Risk.** MED. `link_region_variables` iterates `variables.keys()` — that already returns only set keys for `Option<_>` values.  `link_region` does `clone()` — that survives. Every read path becomes `?` — small mechanical update.

**LOC delta.** +5 to +10 (extra `?`s) but the *correctness* gain is real: silent `NodeOutputId(0)` aliasing is a footgun.

---

## F-5 — `find_stack_stored_value_at_offset`'s StackStore vs Store arms (Hypothesis 6)

**File:** `crates/opt/src/stack_load_forward/mod.rs:489-549`

**Special-case shape.** Two top-level match arms for `StackStore` and `Store` that both ultimately do "if disjoint or non-aliasing, recurse on prior memory."

**General case.** Compute a unified `AliasStep` (the helper enum already exists in `crates/opt/src/sp_expr.rs`) for both kinds.

**Sketch.**
```rust
let result = match *graph.node_kind(node) {
    NodeKind::StackStore { offset: k, space: _ } => {
        let inputs = graph.node_inputs(node);
        let data = inputs[2];
        let data_ty = graph.output_kind(data).as_value();
        match (data_ty, k == offset) {
            (Some(t), true) if t == value_type => Some(data),
            (Some(t), true)                    => None,
            (Some(t), false) if ranges_disjoint(k, t.byte_size() as i64, offset, load_size) =>
                find_stack_stored_value_at_offset(graph, inputs[0], offset, value_type, sp_vn, sp_memo, walk_memo),
            _ => None,
        }
    }
    NodeKind::Store(_) => match step_through_store(graph, node, sp_vn, sp_memo, offset, load_size) {
        AliasStep::PassThrough { prev_mem } =>
            find_stack_stored_value_at_offset(graph, prev_mem, offset, value_type, sp_vn, sp_memo, walk_memo),
        AliasStep::MayAlias => None,
    },
    _ => None,
};
```
The structural saving is small — the logic *is* genuinely different (StackStore checks the data slot for an exact-offset match; raw Store can never match because StackStoreDetect would have classified it). True unification requires `step_through_store` to gain a "matches at exact offset" return variant, which is a wider refactor.

**Verdict: SKIP** — unification is illusory; the two arms aren't doing the same thing.

**Risk.** N/A.
**LOC delta.** 0.

---

## F-6 — `build_call_other_terminal` vs `build_call_other_modeled` (Hypothesis 7)

**File:** `crates/ir/src/builder/call.rs:166-188` and `:233-307`

**Special-case shape.** Two methods doing morphologically the same thing: emit a `CallOther` node with ctrl/mem inputs and a stamp on `call_other_names`. Differences:
- `_terminal`: no args, no implicit reads/writes, dangling outputs, no ctrl advance.
- `_modeled`: explicit args + implicit reads/writes, optional value output, ctrl advance, optional clobber-override side-table.

The third case (`NoOp`) is handled at lift time by *not* calling either method.

**General case.** The two methods are different enough that fully unifying them adds parameters without simplifying call sites. But there *is* a real pattern simplification: thread `CallOtherClass` through to a single `build_call_other(class)` dispatcher in *FunctionBuilder* (or in the strider lift path).

**Sketch.**  Move the `match class { NoOp => skip, NoReturn => build_call_other_terminal, Call(abi) => build_call_other_modeled }` from `IrStrider::handle_call_other` into a single `build_call_other(class, …)` API in `FunctionBuilder`.  The internals stay split but the IR-level call site collapses to one match.

**Risk.** MED. Clean dispatch but pushes the policy decision (which method to call) further away from the lift loop, where context is richer. Marginal benefit; current shape is fine.

**LOC delta.** −5 at the lift call site, +5 in the new dispatcher. Net 0.

**Verdict: SKIP** — current factoring is actually clearer than the unified version.

---

## F-7 — `apply_elf_relocations` vs `apply_elf_relocations_autoload` (Hypothesis 8)

**File:** `crates/reader/src/elf.rs:460-640` (pure) and `:678-711` (autoload)

**Special-case shape.** The autoload variant pre-walks the dynamic relocations, finds owning sections for missing sites, appends them to `regions`, then delegates to the pure variant.

**General case.** Make the pure variant accept an `extend_on_missing: impl FnMut(u64) -> Option<MemRegion>` parameter; the no-autoload caller passes `|_| None`, and `_autoload` passes a closure that calls `find_loadable_section_containing` and materialises a region.

**Sketch.**
```rust
pub fn apply_elf_relocations_with_extender<F>(
    regions: &mut Vec<MemRegion>, obj: &object::File<'_>, mut extend_on_missing: F,
) -> Result<RelocationStats>
where F: FnMut(u64, &object::File<'_>) -> Result<Option<MemRegion>>,
{
    // existing body, but at every `skipped_no_region` decision call extend_on_missing(site_addr, obj)
    // and on `Some(region)` push it and retry the find.
}
```
Two thin wrappers (`apply_elf_relocations`, `apply_elf_relocations_autoload`) become 3-line shims over the unified function.

**Risk.** MED.  The pure version mutates `regions: &mut [MemRegion]` (slice), the autoload version takes `&mut Vec<MemRegion>` (owned). The unified extender form *requires* `&mut Vec<MemRegion>`. Public API breakage.

**LOC delta.** −30 to −40 over the two functions (autoload's pre-walk + the duplicate "find/skip/apply" loop body collapses).

---

## F-8 — `MemoryMap.add_region_from_elf(apply_relocations=True)` vs `MemoryMap.apply_elf_relocations` (Hypothesis 9)

**File:** `crates/strider-py/src/reader.rs:204-238` (combined load + reloc) vs `:264-281` (apply over already-loaded regions)

**Special-case shape.** Two paths to apply relocations. `add_region_from_elf(path, True)` loads code+rodata+rel-data via `elf_load_with_relocations` and applies in one shot. `apply_elf_relocations(path)` operates on already-loaded regions and autoloads missing sections. They share zero code; both wrap the underlying `reader::elf::*` helpers.

**General case.** Drop the `apply_relocations` param entirely. The Python idiom becomes: always `add_region_from_elf(path)` then optionally `apply_elf_relocations(path)`. The autoload step in the second call covers the section-widening that `apply_relocations=True` did inline.

**Sketch.**
```rust
fn add_region_from_elf(&self, path: &str) -> PyResult<()> {
    // existing code w/o the apply_relocations branch — always uses
    // elf_get_code_and_readonly_sections_as_mem_regions.
}
// apply_elf_relocations stays as-is.  Power users do both in sequence.
```

**Risk.** HIGH. Public Python API change — pre-existing callers that pass `apply_relocations=True` break.

**LOC delta.** −10 in `reader.rs`; the test plan is the cost.

---

## F-9 — `LoopState::run_stable_only` vs `rebuild` Decision (Hypothesis 14)

**File:** `crates/strider/src/orchestrator.rs:194-205, 416-443`

**Special-case shape.** `Decision::StableOnly` runs `build_stable_optimizer_pipeline().run_on_built(graph)`. `Decision::Rebuild` runs `build_lift_stable(…)` which builds the CFG, lifts, and runs the same stable pipeline.

**General case.** The two are genuinely different: `StableOnly` skips the CFG rebuild + IR re-lift. Stripping to a single Decision would force every iteration to rebuild even when in-place edits sufficed. **Verdict: SKIP** — these are semantically distinct loop bodies.

**LOC delta.** 0.

---

## F-10 — `OptimizerPipeline::add` vs `add_post_pass` (Hypothesis 13)

**File:** `crates/opt/src/pipeline.rs:211-219, 248-250, 265-292`

**Special-case shape.** Two parallel registration paths and two separate iteration loops in `run`.

**General case.** Tag every pass with a `PassPhase { FixedPoint, PostConvergence }` and store one `Vec<(Box<dyn Optimizer>, PassPhase)>`. Single `run` partitions in-place.

**Sketch.**
```rust
enum PassPhase { FixedPoint, PostConvergence }
struct PipelineEntry { opt: Box<dyn Optimizer>, phase: PassPhase, name: &'static str }
pub struct OptimizerPipeline { entries: Vec<PipelineEntry> }
impl OptimizerPipeline {
    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) { /* push FixedPoint */ }
    pub fn add_post_pass<O: Optimizer + 'static>(&mut self, opt: O) { /* push PostConvergence */ }
    pub fn run(&self, …) -> Result<()> {
        loop { for e in entries { if matches!(e.phase, FixedPoint) { … } } }
        for e in entries { if matches!(e.phase, PostConvergence) { e.opt.optimize(…)?; } }
    }
}
```

**Risk.** LOW. Pure refactor; public API (`add`, `add_post_pass`) stays. Test for `optimizer_count` / `optimizer_names` may need a lookup-by-phase variant.

**LOC delta.** −5 (two vecs collapsed; one less field) but *no functional gain*. Dispatching by `PassPhase` inside `run` adds the `matches!` overhead twice per iteration. Marginal.

**Verdict: SKIP for LOC; accept if you also want a `PassPhase::AfterEachIter` or similar future variant.**

---

## F-11 — `Matcher::find_all` vs `match_at` (Hypothesis 12)

**File:** `crates/pattern/src/matcher/mod.rs:221-263, 481-488`

**Verified.** `find_all` already calls `match_node_id` per candidate root, and `match_at` is `match_node_id` for one root. Both share the matching engine; `find_all` adds candidate selection (kind-index bucket vs preorder fallback). No duplication. **Verdict: ALREADY UNIFIED.**

**LOC delta.** 0.

---

## F-12 — `RedundantPhis::remove_phis` VarPhi/MemPhi vs ControlState arms (Hypothesis 11)

**File:** `crates/opt/src/redundant_phis/mod.rs:32-162`

**Special-case shape.** Two arms — one for `VarPhi | MemPhi`, one for `ControlState` — each detecting "single reachable predecessor" and replacing uses. Differences:
- VarPhi/MemPhi: `inputs[0]` is the phi token, `inputs[1..]` are per-pred values; the arm gathers `reachable_ctrl` from the owner ControlState, then maps positional → value via the phi token's owner.
- ControlState: `inputs[..]` are all control predecessors; gather the reachable ones directly.

**General case.** ControlState is *the* reachability source — both arms decide simplification on `reachable_ctrl_predecessors_of(this_or_owner_control_state).len() == 1`. The fingerprint absorption + `replace_all_uses` step is identical.

**Sketch.**  Extract a shared helper:
```rust
fn unique_reachable_ctrl(graph: &Graph, control_state: NodeId, reachable: &NodeIdSet)
    -> Option<NodeOutputId>
{
    let inputs = graph.node_inputs(control_state);
    let mut iter = inputs.into_iter().filter(|i| reachable.contains(graph.output_definition(*i).0));
    iter.next().filter(|_| iter.next().is_none())
}
```
Then both arms become:
```rust
let owner = match kind { VarPhi(_) | MemPhi => owner_of_phi_token, ControlState => self };
match unique_reachable_ctrl(graph, owner, reachable) {
    Some(unique) => /* find positional value, replace_all_uses, fingerprint absorb */,
    None => /* equality-of-values fallback for VarPhi/MemPhi only */,
}
```

**Risk.** MED.  The `live_values` fallback (`phi(v, phi)` collapsing to `v`) is VarPhi/MemPhi-only and stays in that arm. Self-reference filtering is positional, also phi-only.

**LOC delta.** −15 to −20 (most of the duplication is the iterator-singularity dance).

---

## F-13 — Two-pass IR validation: Layer A scoped vs Layer B/C unscoped (Hypothesis 10)

**File:** `crates/ir/src/validate/mod.rs:75-114`

**Verified.** Layer A iterates `nodes.keys()` filtered by `reachable.contains`; Layer B iterates all nodes (use-list consistency must include zombies); Layer C is split — `check_layer_c_control_state` takes `reachable`, others don't.

**General case.** Three layers, three different node-sets. Layer A's per-node check (`expected_signature` typing) would *fail* on zero-input zombies left by `RedundantPhis::detach_node_inputs` because the signature requires non-zero inputs. So Layer A genuinely needs the reachability filter.

**Verdict: SKIP** — the three scopes are intentional and independently justified.

**LOC delta.** 0.

---

## F-14 — `Strider::is_branch_tail_call` vs `is_branch_tail_call_nocheck`

**File:** `crates/cfg/src/cfg/builder/region_builder.rs:215-243`

**Special-case shape.** Two methods. `_nocheck` is `is_addr_tail_call`. `_check` calls `_nocheck` then asserts `insn_index == 0`.

**General case.** Inline the check: any caller that knows their `target_addr` came from `at_machine_start` already pins `insn_index == 0` (line 491 documents this pattern). Callers that decode a branch target from a CONST varnode (line 270) need the validation. There are exactly 5 call sites; 3 use `_nocheck`, 2 use `_check`. Could be replaced by a single `is_branch_tail_call(addr) -> Result<bool>` whose insn_index check is trivially satisfied for `at_machine_start` constructions.

**Risk.** LOW. Mechanical rename.

**LOC delta.** −10 (one method removed, one helper inlined; saves 14 LoC, costs ~4 in two callers).

---

## F-15 — `GotPlt slot` block vs general "find region and write" block in `apply_elf_relocations`

**File:** `crates/reader/src/elf.rs:485-546, 622-636`

**Special-case shape.** The `image_relative_reloc` block (485-497), the `got_or_plt_slot_reloc_size` block (505-546), and the general fall-through (622-636) all do the same final 4 lines:
```rust
let region = regions.iter_mut().find(|r| r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())?;
let off = (site_addr - region.start_addr()) as usize;
write_at(region.data_mut(), off, value, size_bytes, endian_le);
stats.applied += 1;
```

**General case.** Extract `fn locate_and_write(regions, site_addr, size_bytes, value, endian_le, stats) -> ()`. Each of the three sites already computes `(value, size_bytes)`; deduplication is mechanical.

**Sketch.**
```rust
fn locate_and_write(regions: &mut [MemRegion], site_addr: u64, size_bytes: usize,
                    value: u64, endian_le: bool, stats: &mut RelocationStats) {
    if let Some(region) = regions.iter_mut().find(|r|
        r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())
    {
        let off = (site_addr - region.start_addr()) as usize;
        write_at(region.data_mut(), off, value, size_bytes, endian_le);
        stats.applied += 1;
    } else {
        stats.skipped_no_region += 1;
    }
}
```

**Risk.** LOW. Pure refactor.

**LOC delta.** −15 (three call sites of ~7 lines each → three 1-line calls + ~10 line helper).

---

## F-16 — `cfg::region_id_at_start` linear scan duplicates `start_addr_to_region_id` map

**File:** `crates/cfg/src/cfg/query.rs:139-149`

**Special-case shape.** `region_id_at_start(addr)` does an O(N) linear scan over `graph.node_indices()` for a lookup that the cfg builder *already maintains* in `start_addr_to_region_id` (used at `region_builder.rs:573`). The map is a private field of `Builder`, not exposed on `Cfg`.

**General case.** Persist the lookup map onto `Cfg` (or expose it via `Cfg::start_addr_index() -> &HashMap<…>`). Linear-to-O(1).

**Risk.** LOW. Performance fix; behaviour identical.

**LOC delta.** Roughly neutral; perf gain is real on large CFGs.

---

## F-17 — `region_entry_control` and `region_entry_memory` duplicated structure

**File:** `crates/ir/src/region.rs:294-351`

**Special-case shape.** Two functions doing the same thing: walk `node_outputs(node)`, return the first one whose kind matches a predicate, error if none. ~28 lines each, identical except `is_control` vs `is_memory`.

**General case.**
```rust
fn first_output_matching(
    &self, node_id: NodeId, pred: impl Fn(NodeOutputKind) -> bool, want: &str,
) -> Result<NodeOutputId> {
    let mut first_seen = None;
    for out in self.graph().node_outputs(node_id) {
        if first_seen.is_none() { first_seen = Some(out); }
        if pred(self.graph().output_kind(out)) { return Ok(out); }
    }
    match first_seen {
        Some(first) => Err(anyhow!("output {first:?} is not a {want} edge (got {:?})",
                                   self.graph().output_kind(first))),
        None        => Err(anyhow!("node {node_id:?} has no outputs")),
    }
}
```
Then `region_entry_control` is one line; same for `region_entry_memory`.

**Risk.** LOW. Pure dedup.

**LOC delta.** −30 (two 28-line bodies → one 12-line helper + two 3-line callers).

---

## F-18 — `apply_in_place_edit` Single arm's override path duplicates `build_anchor_calling_context`'s clobber recompute

**File:** `crates/strider/src/orchestrator.rs:599-631` and `:703-714`

**Special-case shape.** When `override_cc` is `Some`, both `apply_in_place_edit` (post-splice path) and `build_anchor_calling_context` (pre-splice path) recompute the same clobber list (`tracked_vars - callee_saved - SP`). Two separate iterations of `graph.variables.values()` with the same filter.

**General case.** Compute it once in `build_anchor_calling_context`, return it as part of `AnchorCallingContext`, and have `apply_in_place_edit`'s spliced-Call recording path read from the context.

**Risk.** LOW. Mechanical.

**LOC delta.** −10 (the second filter loop disappears).

---

## Top-5 ranked by `(LOC reduced) × (1/risk) × (clarity gained)`

| # | Finding | LOC | Risk | Clarity gain | Score |
|---|---------|-----|------|--------------|-------|
| 1 | **F-17** Extract `first_output_matching` helper for `region_entry_control` / `_memory` | −30 | LOW | High (kills 28-line dup) | ★★★★★ |
| 2 | **F-15** Extract `locate_and_write` in `apply_elf_relocations` | −15 | LOW | High (kills 3-way dup) | ★★★★ |
| 3 | **F-12** Unify `RedundantPhis` VarPhi/MemPhi/ControlState single-pred collapse | −15 | MED | Medium (single-pred reasoning lives in one place) | ★★★ |
| 4 | **F-7** `apply_elf_relocations_with_extender` unifies pure + autoload | −35 | MED | Medium (one fn, one body) | ★★★ |
| 5 | **F-1** Collapse `is_addr_tail_call` to half-open range form | −5 | LOW | High (eliminates the "lower_bound_strict" helper bool) | ★★★ |

Honourable mentions: **F-14** (collapse `_check`/`_nocheck` tail-call helpers, −10 LOW) and **F-3** (collapse `CondBranch` 4-arm match into "filter-and-dispatch", structural win without LOC change).

Skipped/already-unified: F-2, F-5, F-6, F-9, F-11, F-13.
