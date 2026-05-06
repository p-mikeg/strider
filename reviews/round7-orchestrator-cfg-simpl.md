# Round 7 — Orchestrator + CFG simplifications (NEW findings)

Audit of `crates/strider/src/orchestrator.rs` and `crates/cfg/`.  Findings **already** covered by `reviews/round7-flows.md` are skipped (F-1 `is_addr_tail_call`, F-3 `CondBranch` 4-arm, F-9 StableOnly vs Rebuild, F-14 `_check`/`_nocheck`, F-16 `region_id_at_start` linear scan, F-18 override clobber dup).

---

## N-1 — `LoopState::build_iter_0` and `LoopState::rebuild` are textually identical

**File:** `crates/strider/src/orchestrator.rs:332-356, 425-443`

**What's complex.** Both methods (a) take the Sleigh handle, (b) call `build_lift_stable` with the *same* arg list, (c) re-seat the harvested Sleigh, region_index, graph, and unresolved.  `build_iter_0` additionally seeds `pending_at_iter_0` and `stall_budget`.  ~25 lines of duplication.

**Sketch.** Add one private helper and have iter-0 wrap it:
```rust
fn lift_and_seat(&mut self) -> Result<()> {
    let sleigh = self.sleigh.take()
        .ok_or_else(|| anyhow!("orchestrator: sleigh handle missing"))?;
    let (graph, unresolved, region_index, sleigh) = build_lift_stable(
        sleigh, &self.opts, &self.known_targets, &self.decode_cache,
        &mut self.vn_cache, &mut self.vn_cache_region_count,
    )?;
    self.sleigh = Some(sleigh);
    self.region_index = region_index;
    self.graph = Some(graph);
    self.unresolved = unresolved;
    Ok(())
}
fn build_iter_0(&mut self) -> Result<()> {
    self.lift_and_seat()?;
    self.pending_at_iter_0 = self.unresolved.len();
    self.stall_budget = self.pending_at_iter_0;
    Ok(())
}
fn rebuild(&mut self) -> Result<()> { self.lift_and_seat() }
```

**LOC delta.** −15.  **Risk.** LOW (pure factoring; both call sites currently share identical fixup logic).

---

## N-2 — `Option<rsleigh::Sleigh<R>>` field that is "always Some except for two-line `take`/`Some(_)` handoff"

**File:** `crates/strider/src/orchestrator.rs:228, 304, 333-345, 426-438`

**What's complex.** `LoopState::sleigh: Option<rsleigh::Sleigh<R>>` — every method comments "`None` only momentarily inside `build_lift_stable`".  The Option exists solely because `build_lift_stable` consumes the Sleigh by value and returns it.  Each call site has the dance: `take()` → `ok_or_else` panic-message → call → `self.sleigh = Some(harvested)`.

**Sketch.** Have `build_lift_stable` take `&mut Option<Sleigh<R>>` (or `&mut Sleigh<R>` after refactoring `cfg::Builder` to not own-by-value, which is a wider change).  The cheap fix: a tiny helper:
```rust
fn with_sleigh<F, T>(&mut self, f: F) -> Result<T>
where F: FnOnce(rsleigh::Sleigh<R>, /* args */) -> Result<(T, rsleigh::Sleigh<R>)> {
    let s = self.sleigh.take().ok_or_else(|| anyhow!("sleigh missing"))?;
    let (out, s) = f(s /* … */)?;
    self.sleigh = Some(s);
    Ok(out)
}
```
Eliminates the literal-string error duplicated at `:336` and `:429`.

**LOC delta.** −6.  **Risk.** LOW (mechanical).

---

## N-3 — `LoopState::recompute_unresolved` early returns make the body asymmetric

**File:** `crates/strider/src/orchestrator.rs:539-556`

**What's complex.** Three exit paths: (1) no edits → return original vec; (2) edits but graph missing → return empty vec (silent error swallow); (3) normal filter.  Path 2 is unreachable in practice — `apply_in_place_edits` already errored on a missing graph — but the code defensively returns `Vec::new()` rather than propagating.

**Sketch.** Make the "graph present" precondition explicit:
```rust
fn recompute_unresolved(&mut self, edits: &[(NodeId, ResolvedTargets)])
    -> Vec<(PcodeInsnAddr, ir::Value)>
{
    let unresolved = std::mem::take(&mut self.unresolved);
    if edits.is_empty() { return unresolved; }
    let graph = self.graph.as_ref().expect("recompute_unresolved called after apply_in_place_edits sets graph");
    unresolved.into_iter()
        .filter(|(_, anchor)| opt::find_placeholder_return_for_anchor(&graph.graph, *anchor).is_some())
        .collect()
}
```
Or take `&BuiltFunctionGraph` as a param and let the caller hold the borrow.

**LOC delta.** −2 (silent path → expect()).  **Risk.** LOW.

---

## N-4 — `RegionExitInfo` newtype wraps a single `Arc<HashMap>`

**File:** `crates/strider/src/orchestrator.rs:134-140, 142-165`

**What's complex.** `RegionExitInfo` is a struct with exactly one field, `exit_vn_to_value: Arc<HashMap<rsleigh::Vn, NodeOutputId>>`.  No methods, no other state.  `RegionIndex` stores `HashMap<NodeOutputId, RegionExitInfo>` — i.e. a map of one-field structs.  All call sites do `r.exit_vn_to_value.get(&vn)`.

**Sketch.** Drop the wrapper:
```rust
struct RegionIndex {
    by_exit_control: HashMap<NodeOutputId, Arc<HashMap<rsleigh::Vn, NodeOutputId>>>,
}
fn region_for_placeholder(&self, graph: &BuiltFunctionGraph, p: NodeId)
    -> Option<&Arc<HashMap<rsleigh::Vn, NodeOutputId>>> { … }
```
And change `read_or_init_var`'s `region: Option<&RegionExitInfo>` to `region: Option<&HashMap<rsleigh::Vn, NodeOutputId>>`.

**LOC delta.** −10 (struct + all `r.exit_vn_to_value.` accesses become direct).  **Risk.** LOW (pure refactor).

---

## N-5 — `build_anchor_calling_context` rebuilds `initial_var_index` per anchor

**File:** `crates/strider/src/orchestrator.rs:684-691`

**What's complex.** Each call to `build_anchor_calling_context` (called from `apply_in_place_edit` once per in-place edit) walks `graph.graph.all_node_ids()` to build a `HashMap<rsleigh::Vn, NodeOutputId>` of every `InitialVar` node.  When 50 in-place edits fire in one iteration, this is 50 full-arena scans of a graph that itself grows by **at most one** `InitialVar` per call (only `read_or_init_var`'s third branch creates new ones).

**Sketch.** Hoist the `initial_var_index` to `LoopState`, build it once per `lift_and_seat`, and pass it as `&mut HashMap<…>` so `read_or_init_var` keeps inserting newly-created nodes:
```rust
struct LoopState<…> {
    …
    initial_var_index: HashMap<rsleigh::Vn, NodeOutputId>,
}
// On lift_and_seat: rebuild from scratch (graph identity changes).
self.initial_var_index = collect_initial_vars(&graph.graph);
// apply_in_place_edits passes &mut self.initial_var_index into build_anchor_calling_context.
```

**LOC delta.** +5 in setup, −7 per call site, net ≈ −5 with **N-edits speedup** that grows with edits-per-iteration.  **Risk.** LOW (the existing call sites already mutate the local map — promoting it to long-lived state is mechanical).

---

## N-6 — `build_anchor_calling_context`'s clobber-list two-arm split is symmetric

**File:** `crates/strider/src/orchestrator.rs:701-729`

**What's complex.** A 30-line `if override_cc.is_some() { … } else { … }` block.  Both arms iterate a varnode set, filter, and `push` `NodeOutputKind::OutputType(ty)`.  The override arm filters `cc.callee_saved_regs.contains(vn) || Some(*vn) == sp_vn` from `graph.variables.values()`; the default arm iterates `graph.call_clobbered.iter()` (already pre-filtered).

**Sketch.** Decide the *iterator source* up front, and run a single `for` body:
```rust
let clobber_iter: Box<dyn Iterator<Item = rsleigh::Vn>> = if override_cc.is_some() {
    let sp = strider.calling_convention().stack_ptr_vn;
    Box::new(graph.variables.values().copied()
        .filter(move |v| !cc.callee_saved_regs.contains(v) && *v != sp))
} else {
    Box::new(graph.call_clobbered.iter().copied())
};
for vn in clobber_iter {
    if let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) {
        ctx.clobbered_kinds.push(ir::node::NodeOutputKind::OutputType(ty));
    }
}
```

**LOC delta.** −10.  **Risk.** LOW (preserves both filter rules verbatim).  Note: combine with **F-18** (round 7-flows) for full benefit — same clobber-list shape.

---

## N-7 — `Builder::new` / `with_endianness` / `with_preset` defaults force every caller to chain three setters

**File:** `crates/cfg/src/cfg/builder/mod.rs:86-129`

**What's complex.** Three constructor variants:
- `Builder::new(sleigh, start, opts)` → defaults LE + x86_64 preset.
- `Builder::with_endianness(sleigh, start, opts, end)` → explicit endianness, defaults x86_64 preset.
- `with_preset(self, preset)` → fluent override of the preset.

Every non-x86_64 caller writes `Builder::with_endianness(sleigh, start, opts, arch.endianness).with_preset(preset)`.  The orchestrator at `:803` does exactly this; `target::SleighArch` already pairs `(sla_spec, pspec, endianness)` and the strider crate already exposes `target::ArchPreset` for the preset.

**Sketch.** Add `Builder::for_arch(sleigh, start, opts, arch_preset)` that derives `endianness` from the preset (or accepts `&SleighArch`):
```rust
pub fn for_arch(sleigh: rsleigh::Sleigh<R>, start_addr: u64, options: Options,
                preset: target::ArchPreset) -> Self {
    let endianness = preset.endianness();  // add this small helper to target
    let mut b = Self::with_endianness(sleigh, start_addr, options, endianness);
    b.preset = preset;
    b
}
```
Or, more bluntly, replace `with_endianness` with `for_arch` (single source of truth) and keep `Builder::new` as the LE-x86_64 convenience for tests.

**LOC delta.** −5 in the orchestrator + every caller; +10 in the new ctor.  Net 0; the **clarity gain** is real (one ctor matches one architecture in calling code).

**Risk.** MED (public API change — `with_endianness` is `pub`; tests in `crates/cfg/tests/` use it).

---

## N-8 — `Switch.target_value: Option<ir::Value>` is "always None at construction site"

**File:** `crates/cfg/src/cfg/types.rs:142-148` and `crates/cfg/src/cfg/builder/region_builder.rs:516`

**What's complex.** `RegionTerminator::Switch.target_value` is documented as "OPTIONAL pinned NodeOutputId for the dispatch value … cfg builder always sets this to `None`".  Search confirms only one construction site (`region_builder.rs:516`) and it hard-codes `None`.  The doc says it's "plumbing for an incremental-rebuild round"; that round hasn't landed.

**Sketch.** Drop the field and the documentation about plumbing.  When the incremental-rebuild round lands, add it back with an actual producer.  Carrying always-None state confuses readers.
```rust
Switch { target_vn: rsleigh::Vn, targets: Vec<u64> }
```

**LOC delta.** −10 (field, doc paragraph, two consumer reads of `target_value`).  **Risk.** MED — this is the YAGNI tradeoff.  If the incremental-rebuild plan is imminent, leave it.  If not, drop it.

---

## N-9 — `process_branch_indirect`'s `Single` path duplicates the `Branch` opcode arm's tail-call dance

**File:** `crates/cfg/src/cfg/builder/region_builder.rs:486-498` vs `:289-311`

**What's complex.** The `Branch` arm at lines 289-311 and the `BranchIndirect → Single` arm at lines 486-498 both do: classify the target as tail-call; if so, emit `TailCall { target }` with no successor; if not, emit `Branch` and enqueue the successor with `Branch` edge kind.  ~13 lines duplicated, modulo the `Branch` arm's extra Fallthrough-reclassification.

**Sketch.** Extract a helper:
```rust
fn finish_with_intra_or_tail(&mut self, target: PcodeInsnAddr, edge: RegionEdgeKind) -> Result<()> {
    if self.is_branch_tail_call_nocheck(target) {
        self.finish_current_region(RegionTerminator::TailCall { target: target.machine_addr.addr })?;
    } else {
        let region = self.finish_current_region(RegionTerminator::Branch)?;
        self.builder.work_queue.push((Some((region, edge)), target));
    }
    Ok(())
}
```
The `Branch` arm calls it with the conditionally-Fallthrough edge kind; the `Single` arm calls it with `Branch`.  The `is_tail_call` value is recomputed once instead of stamped through.

**LOC delta.** −8.  **Risk.** LOW (the helper preserves both arms' semantics; the Fallthrough-reclassification stays at the call site since it's specific to the Branch-opcode case).

---

## N-10 — `LoopState::vn_cache` + `vn_cache_region_count` should be a self-contained struct

**File:** `crates/strider/src/orchestrator.rs:265-270, 815-823`

**What's complex.** Two parallel fields that must stay synchronized: `vn_cache: HashSet<rsleigh::Vn>` and `vn_cache_region_count: usize`.  The "incremental scan" logic at `:815-823` is in `build_lift_stable` (a free function), reads both via `&mut`, and the synchronization invariant is documented in two field-level doc comments.

**Sketch.** Bundle them and put the logic on the bundle:
```rust
struct VnCache { seen: HashSet<rsleigh::Vn>, scanned_regions: usize }
impl VnCache {
    fn ingest_new_regions(&mut self, regions: &[&cfg::Region]) {
        for region in regions.iter().skip(self.scanned_regions) {
            for w in &region.insns { self.seen.extend(w.insn.all_vns()); }
        }
        self.scanned_regions = regions.len();
    }
    fn sorted_vns(&self) -> Vec<rsleigh::Vn> {
        let mut v: Vec<_> = self.seen.iter().copied().collect();
        v.sort_unstable_by_key(pcode_lift::vn_sort_key);
        v
    }
}
```
`build_lift_stable` then takes `&mut VnCache` instead of two `&mut`s.

**LOC delta.** −5 (consolidates 8 lines of inline scan into a 6-line method).  **Risk.** LOW.  Already marked TODO(Task17) for removal — but until it's removed, the bundling improves readability.

---

## N-11 — `is_tail_call` (orchestrator) is a 6-line wrapper for one caller

**File:** `crates/strider/src/orchestrator.rs:569-576`

**What's complex.** A free function that delegates 1:1 to `cfg::is_addr_tail_call`, just unpacking four fields out of `RunOpts`.  Single caller (`classify_and_partition` at `:497`).  The doc comment is longer than the body.

**Sketch.** Inline it:
```rust
let target_oob = cfg::is_addr_tail_call(*target, opts.start_addr,
    opts.fn_max_size, opts.allow_code_before_start_addr);
```
Or keep the helper as a method on `RunOpts` so `self` carries the args:
```rust
impl RunOpts<'_> {
    fn is_tail_call(&self, target: u64) -> bool {
        cfg::is_addr_tail_call(target, self.start_addr, self.fn_max_size,
            self.allow_code_before_start_addr)
    }
}
```
That preserves the 7 unit tests at `:979-1017` (they take `&RunOpts`).

**LOC delta.** −6 free fn → +4 method.  Net −2.  **Risk.** LOW.

---

## N-12 — `EdgeKind` enum private to `edge_set_of`

**File:** `crates/strider/src/orchestrator.rs:879-883, 851-873`

**What's complex.** `EdgeKind { LinkRegister, Target(u64) }` is a private enum used solely to dedup-and-sort the "induced edge set" for `edge_set_changed` comparison.  `edge_set_of` walks the resolved-targets map and produces `Vec<(addr, EdgeKind)>` so two iterations can compare for equality.

**General case.** This is a fingerprint-only need.  A `BTreeSet<(PcodeInsnAddr, Option<u64>)>` (where `None` ≡ LinkRegister) is structurally identical and removes the named enum:
```rust
fn edge_set_of(map: &HashMap<PcodeInsnAddr, ResolvedTargets>)
    -> BTreeSet<(PcodeInsnAddr, Option<u64>)> {
    let mut out = BTreeSet::new();
    for (addr, r) in map { match r {
        ResolvedTargets::LinkRegister => { out.insert((*addr, None)); }
        ResolvedTargets::Single(k)    => { out.insert((*addr, Some(*k))); }
        ResolvedTargets::Multiple(ts) => for k in ts { out.insert((*addr, Some(*k))); }
    }}
    out
}
```
BTreeSet handles sort + dedup automatically.

**LOC delta.** −10 (enum, sort/dedup).  **Risk.** LOW (the unit tests at `:923-975` are about set equality, not the EdgeKind name).

---

## N-13 — `is_branch_tail_call` validation + name asymmetry

**File:** `crates/cfg/src/cfg/builder/region_builder.rs:215-243`

**What's complex.** Two near-identical methods: `_nocheck` returns `bool`, the validating variant adds an `insn_index != 0 → Err` branch.  This was **already covered by F-14 in round7-flows.md**.  Adding here only because of one extra observation: the validating variant is called from exactly two sites (lines 270, 328), both of which already operate on `decode_branch_target`-produced `PcodeInsnAddr`s — i.e. `insn_index` is already the *Sleigh-encoded* target index, not arbitrary.  Lifting the validation to `decode_branch_target` itself would push the check to the producer site and let every consumer use one helper.  Skipping in favour of F-14's lighter recommendation.

---

## Top-5 ranked by `(LOC) × (1/risk) × clarity`

| # | Finding | LOC | Risk | Clarity gain | Score |
|---|---------|-----|------|--------------|-------|
| 1 | **N-1** Extract `lift_and_seat` from `build_iter_0` / `rebuild` | −15 | LOW | High | ★★★★★ |
| 2 | **N-4** Drop `RegionExitInfo` newtype wrapper | −10 | LOW | Medium | ★★★★ |
| 3 | **N-5** Hoist `initial_var_index` from per-anchor to per-iteration | −5 + perf | LOW | Medium (kills N-times-quadratic) | ★★★★ |
| 4 | **N-6** Unify `build_anchor_calling_context` clobber arms | −10 | LOW | High | ★★★★ |
| 5 | **N-9** Extract `finish_with_intra_or_tail` from Branch + Single arms | −8 | LOW | Medium | ★★★ |

Honourable mentions: **N-12** (BTreeSet collapses EdgeKind enum, −10 LOW), **N-2** (Sleigh `Option` dance, −6 LOW), **N-8** (drop unused `Switch.target_value`, −10 MED — only if the planned plumbing isn't imminent).

Skipped or mostly-covered: N-13 (= F-14), N-3 (cosmetic only), N-7 (architectural, MED risk; defer to a separate cfg-API audit), N-10 (about-to-be-deleted under Task17).
