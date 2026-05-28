# Fold RegionEdgeKind into RegionTerminator

> **For agentic workers:** implement test-first, commit per task, push after each commit.

**Goal:** Delete `RegionEdgeKind` and make `RegionTerminator` the single
source of truth for control-flow classification. `CondBranch` gains a
`true_target: PcodeInsnAddr` field (the taken successor's address); CFG
edges become unweighted topology (`StableDiGraph<Region, ()>`).

**Why:** `RegionEdgeKind` is the thinner of the two types — the terminator
already carries every distinction it makes plus payloads, EXCEPT which of a
`CondBranch`'s two successors is the taken side. Moving that one bit onto
`CondBranch` lets the edge enum disappear. The recent Switch double-link bug
showed the edge kind (`Unconditional` shared by Fallthrough/Branch/Switch)
was already too coarse to stand alone — the linker had to consult the
terminator anyway.

**Key design points (verified during the spike):**
- `true_target` is a `PcodeInsnAddr` (not `u64`): intra-machine-instruction
  `CBRANCH` can put taken + fall-through at the same machine address with
  different pcode indices.
- `region_if` resolves polarity by matching each outgoing edge's target
  `start_addr` against `true_target`. The first edge matching `true_target`
  is the true side; the remaining edge is false. This handles the degenerate
  "both arms → same region" case (both come back as that region).
- Split-safe: `split_region` keeps the original `start_addr` on the FIRST
  half and re-targets incoming edges there, so a parent's `true_target`
  (an address) still resolves correctly post-split.
- `region_successor` is test-only in production; `region_if` is the sole
  production consumer (`handle_cond_branch`). `graphwalk::GraphRef` ignores
  edge weights entirely.

## Task 1: types.rs — drop RegionEdgeKind, enrich CondBranch

- Delete the `RegionEdgeKind` enum.
- `CondBranch` → `CondBranch { true_target: PcodeInsnAddr }`; rewrite its doc
  (taken successor recorded here; fall-through is the other outgoing edge).
- `RegionGraph = StableDiGraph<Region, ()>`.
- Update `Fallthrough` / `Branch` / `Switch` / `RegionGraph` docs that
  reference `RegionEdgeKind::*`.

## Task 2: builder — unweighted edges, stamp true_target

Files: `builder/mod.rs`, `builder/region_builder.rs`, `builder/split.rs`.

- `work_queue: Vec<(Option<NodeIndex>, PcodeInsnAddr)>` (mod.rs:76).
- `RegionBuilder::parent_edge: Option<NodeIndex>` + ctor param.
- `explore` parent param `Option<NodeIndex>`.
- `finish_current_region`: `add_edge(parent_id, region, ())`.
- `process_insn` fall-into-existing: `add_edge(parent_id, existing, ())`.
- `finish_branch_or_tail_call`: drop `edge_kind` param.
- `process_cond_branch` (false,false) arm:
  `CondBranch { true_target: target_addr }`; push both work items as
  `Some(region)`.
- Switch construction + Single + Branch arms: push `Some(region)`.
- `split.rs`: collect `(e.id(), e.source())`; `add_edge(.., ())` for both the
  re-targeted parents and the first→second split edge.
- Remove `RegionEdgeKind` imports.

## Task 3: query.rs — terminator-driven region_if / region_successor

- Drop `unique_outgoing(kind)`.
- `region_successor`: the unique outgoing edge target; error if >1.
- `region_if`: read the region's terminator; if `CondBranch { true_target }`,
  walk outgoing edges, the first whose target `start_addr == true_target` is
  `if_true`, the remaining one is `if_false`; else both `None`.
- Rewrite the two duplicate-edge tests to the new shapes.

## Task 4: dot.rs — style/label from source terminator

- For each incoming edge, derive `(label, style)` from the SOURCE region's
  terminator: `CondBranch` → dashed, label `if-true`/`if-false` by comparing
  THIS node's `start_addr` to the source's `true_target`; `Switch`/`Branch`/
  `Fallthrough` → solid with a descriptive label.
- Import `RegionTerminator`; drop `RegionEdgeKind`.

## Task 5: pipeline.rs — link purely on source terminator

- `link_region_edges`: iterate edges, link when the SOURCE terminator is
  `Fallthrough | Branch` (drop the edge-weight check entirely).

## Task 6: test fixups + full verification

- `cfg_types.rs`: drop the variant-distinctness test + import.
- `cfg_build_end_to_end.rs`: replace `Unconditional`-edge counts with
  `edge_count()` assertions; `CondBranch` → `CondBranch { .. }`; drop import.
- `cfg_dot_output.rs`: assert new `if-true`/`if-false` labels; doc update.
- `region_builder.rs` tests: `CondBranch { true_target }` assertions; drop the
  edge-kind work-queue assertion.
- Run `cargo test -p strider-lift -p strider-analyze` + clippy; green.
- code-review subagent on the diff.
