# Indirect-branch resolution redesign — design

Date: 2026-06-06
Branch: `feature/indirect-branch-redesign` (off `develop`; merge to `develop`
after approval + green gate; later `develop` → `master`).

## Motivation

Indirect-branch resolution today is split across two mechanisms that are both
fragile and complex:

1. A **per-region mini-IR-graph resolver** (`resolve_indirect_target` /
   `build_resolver_mini_graph` in `strider-orchestrator/src/indirect_resolver.rs`)
   runs at CFG-build time. It rebuilds a single-region mini graph from *only
   that region's pcode*, optimizes it, and classifies `bx lr` / `bx const`. Its
   defect: a single-region graph cannot see LR writes from other regions, so
   `PhiCollapse` folds `lr` back to `InitialVar(lr)` even in a region whose
   runtime LR is a return address from an intervening intra-function call. It
   can misclassify a non-return `bx lr` as a link-register return.

2. **In-place IR editors** (`apply_link_register` / `apply_tail_call` in
   `strider-opt/src/indirect_branch_resolve/inplace.rs`) rewrite the
   `IndirectBranch` placeholder into a `Return` or a `Call`+`Return` chain by
   hand. This hand-editing requires the orchestrator to maintain a
   `RegionIndex` (NodeId-keyed) that persists across `StableOnly` iterations to
   recover ABI register context — which in turn is the *only* reason the
   optimizer pipeline is split into "stable" and "destructive" halves.

Index/array bounding for jump tables and stack arrays uses an ad-hoc backward
control walk (`bound_via_predecessor_if` in
`strider-opt/src/indirect_branch_resolve/jump_table.rs`) plus KnownBits. It
misses AArch64 flag-`cmp` guards, multi-predecessor joins, and cases where
`PhiCollapse` hasn't run yet. There is no dominator tree and no range analysis.

The redesign replaces all of this with: an IR dominator tree, a dominator-scoped
per-varnode range analysis, a **classify-only** resolver, and a single global
resolution map that drives CFG rebuilds — eliminating the mini-graph resolver,
the in-place editors, and the stable/destructive pipeline split.

## New model

### A. IR dominator tree

Compute immediate dominators (`idom`) over the IR's **control subgraph** —
the control-edge-connected nodes (`Entry`, `Region`, `If`, `Call`, `Return`,
`IndirectBranch`). The graph lives on the `Function`, so the domtree is
available wherever IR analysis runs; **no CFG retention** and no CFG-block↔
Region mapping is needed (the lift-time `Cfg` is discarded after lifting, by
design).

- Entry = `Function::entry()`. Edges follow `Control`-typed value outputs from
  a control node to the control inputs of the next control node (an `If`
  produces two control outputs → its two successors; a `Region` consumes one
  control input per predecessor).
- A small dedicated control-only idom (Cooper–Harvey–Kennedy "simple, fast"
  algorithm, the same one petgraph implements) keyed by `NodeId`. We do NOT
  reuse petgraph here: the IR's bipartite petgraph view (`strider-graph`'s
  `petgraph_view`) walks the full sea-of-nodes (data + control, with Phi
  back-edges), which is the wrong graph and would surface cycles. The control
  subgraph is a proper rooted flow graph.
- Output: an `IrDomTree` exposing `idom(node) -> Option<NodeId>` and a
  `dominates(a, b) -> bool` query, built on demand from `(&Function)`.

This is a standalone, independently-testable unit.

### B. Range-analysis pass

A forward, dominator-scoped analysis producing a per-`(ValueId, RegionId)`
integer-interval side-table: "within the control region `R`, value `v` is known
to lie in `[lo, hi]`."

- **Seeds from branch guards.** An `If(IntCmpOp(v, IntConst(N)))` whose true
  successor dominates a region `R` injects an edge-sensitive range for `v` into
  `R` and everything `R` dominates: `Less(v, N)` ⇒ `v ∈ [0, N)` (unsigned);
  the lowered `<=` shape (`Xor(Less(N, v), 1):I1`) ⇒ `v ∈ [0, N]`; signed forms
  (`Sless`) contribute only when non-negativity is independently known.
- **Seeds from KnownBits.** A value whose KnownBits give an upper bound (e.g.
  an explicit `idx & mask`) seeds `[0, max]` everywhere (flow-insensitive).
- **Lattice.** Bounded integer intervals over `u128` widths, meet = interval
  intersection at dominator joins. Minimal and monotone — NOT a general
  abstract interpreter. It tracks only what jump-table / stack-array bounding
  needs (upper bounds on switch indices).
- **Output:** a `RangeMap` queried as `range_of(value, region) -> Interval`,
  consumed by the classifier. Replaces `bound_via_known_bits` +
  `bound_via_predecessor_if`.

Standalone, independently-testable.

### C. Classify-only resolver

For each unresolved `IndirectBranch`, inspect the producer of its target value
and classify (no graph mutation):

- **Return** — the target resolves to `InitialVar(link_register_vn)` at the
  full-function IR level (after the pipeline has propagated the real SSA LR
  value). Because this is whole-function IR, an intervening call that clobbered
  LR yields a different value, so a non-return `bx lr` is *not* misclassified
  (the per-region fragility is gone).
- **Constant(k)** — target folds to `IntConst(k)` (intra- or inter-function).
- **JumpTable(targets)** — `Load[base + idx*stride]` shape; `idx` bounded by the
  range pass (in the dispatch region); table read from ROM up to the bound.
- **StackArray(targets)** — `Load[sp + K + idx*stride]` shape; `idx` bounded by
  the range pass; per-element stored values found via the shared mem-walker
  (see F).

The classifier returns a classification value; it performs **no** in-place edit.
`classify_jump_table` / `classify_stack_array` keep their shape-matching but
swap their bounding onto the range pass.

### D. Global resolution map drives rebuilds

The orchestrator's existing `known_targets: FxHashMap<PcodeInsnAddr,
ResolvedTargets>` becomes the single source of truth for indirect-branch
resolution. (The key is the branch's pcode **address** — the only identifier
stable across CFG rebuilds; an IR `ValueId` is not.)

- Classifications (including **Return**) are recorded in this map.
- The **CFG builder consumes the map at build time** to synthesize the correct
  region terminator directly: a `Return` terminator for a link-register return,
  a `Call`+`Return` (tail-call) shape for a constant target outside the
  function, and successor edges for a constant in-function target or a jump/
  stack table. The tail-call materialization logic moves from the post-lift
  in-place editor into the lift path (CFG terminator → IR).
- Resolution is therefore **uniformly rebuild-driven**: classify → record →
  rebuild if the map grew → repeat to a fixed point. No path edits the IR
  in place.

**Tradeoff (accepted):** a return/tail-call resolution that used to be an
in-place edit with no rebuild now costs one rebuild iteration (it grows the
map). The map grows monotonically so the loop still converges; the uniform,
hand-edit-free flow is the goal.

### E. Deletions

- `strider-orchestrator/src/indirect_resolver.rs` — `resolve_indirect_target`,
  `build_resolver_mini_graph`, and the `Builder::with_indirect_resolver` call
  site. The `IndirectResolverFn` callback type + `with_indirect_resolver` on the
  cfg builder are removed if no other caller remains.
- `strider-opt/src/indirect_branch_resolve/inplace.rs` — `apply_link_register`
  and `apply_tail_call` (and `detach_placeholder` if unused elsewhere).
- The orchestrator's in-place-edit machinery: `apply_in_place_edits`,
  `anchor_calling_context_for`, the `RegionIndex` persistence, and the
  `Decision::StableOnly` path that existed to protect it.
- The **stable/destructive pipeline split**: a single `OptimizerPipeline` runs
  the full pass set each iteration. `build_stable_optimizer_pipeline` /
  `build_destructive_optimizer_pipeline` collapse into one builder. (The split's
  sole justification — NodeId-keyed `RegionIndex` stability across `StableOnly`
  — no longer exists.)

### F. Stack-array stored-value lookup via the mem-walker

Replace `find_stack_stored_value_at_offset`'s bespoke loose backward scan with
the shared `memory_ssa::may_clobber` + `SpAliasOracle` path used by
`LoadForward` / `FunctionArgDetect`. This unifies the memory-dependence logic
and tightens stack-array resolution (respects `base` equality, `AliasMode`, and
forks at `MemPhi` instead of bailing). Soundness is preserved or improved.

## Data flow (the new loop)

```
build CFG  (the builder consults the global map to seat resolved terminators)
   → lift to IR
   → run the single optimizer pipeline (fold/known-bits/load-forward/… )
   → compute IR domtree
   → run the range-analysis pass  (uses domtree)
   → for each unresolved IndirectBranch: classify  (C, uses ranges + mem-walker)
   → record new classifications in the global map
   → if the map grew → rebuild (loop);  else → done
   → (no unresolved remain ⇒ ok; any remaining ⇒ surfaced as an error)
   → compact, return Function
```

## Components & boundaries

| Unit | Crate | Responsibility | Depends on |
|------|-------|----------------|-----------|
| `IrDomTree` | strider-ir (or strider-opt) | idom over the control subgraph | `Function` control edges |
| Range pass | strider-opt | per-(value, region) intervals | `IrDomTree`, KnownBits |
| Classifiers | strider-opt | classify-only (return/const/jt/stack) | Range pass, mem-walker, ROM |
| Global map + loop | strider-orchestrator | record + rebuild-drive | classifiers, CFG builder |
| CFG terminator seating | strider-lift | map → region terminator | global map |

## Phasing (the implementation plan will follow this; each phase compiles + green)

1. **IR control-subgraph dominator tree** (`IrDomTree`) + tests. Pure addition.
2. **Range-analysis pass** + tests. Pure addition (not yet consumed).
3. **Jump-table + stack-array consume the range pass**; stack-array switches to
   the mem-walker; delete `bound_via_predecessor_if` / the bespoke scan. Behavior
   preserved or improved; existing resolution tests still pass.
4. **Resolution cutover**: classify-only + global-map-driven rebuild; CFG
   builder seats terminators; delete the mini-graph resolver and the in-place
   editors + their orchestrator scaffolding. (The big behavioral change.)
5. **Merge the stable/destructive pipelines** into one.

## Error handling

Unchanged in shape: an indirect branch that can't be classified at the
fixed-point exit surfaces as `RegionTerminator::UnresolvedIndirectBranch` + an
`anyhow` error from the orchestrator (Python: `StriderError`). The classifier
fails closed (returns "unresolved") rather than guessing — same conservative
stance as today.

## Testing

- `IrDomTree`: unit tests over synthetic control shapes (diamond, loop,
  nested), cross-checked against a reference (e.g. petgraph `simple_fast` on an
  equivalent toy graph) for the same dominance relation.
- Range pass: unit tests for guard-seeded ranges (`If(idx<N)` ⇒ `[0,N)` in the
  dominated region), KnownBits-seeded ranges, and interval meet at joins.
- Classifiers: the existing jump-table / stack-array / return / constant
  resolution tests must pass, plus new tests for the cases the old bounding
  missed (multi-pred join bound; a `bx lr` that is NOT a return because an
  intervening call clobbered LR — must classify as unresolved/not-return, the
  bug the mini-graph could hit).
- End-to-end: the orchestrator fixture suite (real binaries) resolves the same
  or more indirect branches than before; no regressions in the workspace gate.

## Non-goals

- A general abstract-interpretation framework (the range lattice is minimal).
- Resolving indirect branches the old code couldn't (correctness/simplicity
  first; new patterns are follow-ups), except where the range pass naturally
  picks up the multi-pred / flag-cmp cases.
- Changing the `ResolvedTargets` external shape beyond what the rebuild-driven
  flow requires.

## Verification gate (before merge)

`cargo test --workspace` + `cargo clippy --workspace --all-targets` + (strider-py)
`uv run pytest`. Per the workspace rule. Prompt before merging
`feature/indirect-branch-redesign` → `develop`.
