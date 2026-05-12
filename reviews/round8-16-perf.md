# Round 8 / Ask 16 — Performance at 10k+ nodes

**Branch:** `review/ai2`.  Independent audit.

## Measured baselines

Benchmark binaries could not be executed in the audit environment (missing pre-built ELF fixtures + no Cargo cache).  All complexity claims below are derived from reading the code.

| Benchmark | N | P50 | P95 | Status |
|-----------|---|-----|-----|--------|
| synthetic/stack_store_chain | 1000 | not measured | — | — |
| synthetic/diamond_cfg | 1000 | not measured | — | — |
| synthetic/wide_jump_table | 256 | not measured | — | — |
| synthetic/find_all_requirements_shared | 1000 | not measured | — | — |

## Hot paths (asymptotic complexity)

| Site | Current | Pathological | Proposed |
|------|---------|--------------|----------|
| `OptimizerPipeline::run` fixed-point loop | O(K · P · N) | 100k × 8 × 50 = 40M visits | Per-pass already worklist-driven; OK |
| `KnownBits::analyze` | O(N · fan-out) | full re-propagation per change | Acceptable |
| `detach_unreachable_nodes` | O(N) via `FxHashSet<NodeId>` | per-pass invocation | Migrate to `DenseEntitySet` (Finding A) |
| `validate` Layer B | O(E) | reachability-scoped | OK |
| `validate` Layer C uniqueness/control_state | **O(N_arena)** including zombies | 100k nodes 90% zombie | Scope to reachable (Finding C) |
| `find_all_requirements` | O(N₁·N₂·…·N_M) | M=4, 1000 each = 10¹² | Sort by selectivity (Finding D) |
| `retain_reachable` (compact) | O(N + side-table size) | one-shot | OK; minor `HashMap` waste (Finding E) |
| `extend_asm_fingerprint_from` | O(src len) clone | 1-4 entries typical | OK |

## Findings

### A — `WorkSet::queued` uses `FxHashSet<NodeId>` instead of `DenseEntitySet<NodeId>`

- **Severity:** MED.
- **Where:** `crates/opt/src/worklist.rs:9-21, 52-55, 85`.
- **Current:** `FxHashSet<NodeId>` for the duplicate-prevention set of every opt pass's worklist.  Hashes the 32-bit dense entity index per push/pop.  At 10k+ nodes the table holds 10k+ entries, each requiring hash + bucket lookup.
- **Proposed:** `entity_utils::DenseEntitySet<NodeId>` — flat bit-vector indexed by raw u32; O(1) bitset ops with no hashing, better cache locality.  `detach_unreachable_nodes`'s local `reachable: FxHashSet<NodeId>` has the same issue — reuse the walker's `.visited` (already a `DenseEntitySet`).
- **Estimated improvement:** 15-30% reduction in per-pass iteration cost at 10k+ nodes.

### B — `KnownBits::analyze` returns `FxHashMap<NodeOutputId, Kb>` instead of `SecondaryMap`

- **Severity:** MED.
- **Where:** `crates/opt/src/known_bits/mod.rs:1, 367, 379-392`.
- **Current:** `FxHashMap<NodeOutputId, Kb>` allocated once and probed in tight per-node loop.  20k+ probes per propagation at 10k nodes.
- **Proposed:** `cranelift_entity::SecondaryMap<NodeOutputId, Kb>` using `Kb::default()` (`{ones:0, zeros:0}`) as the absent-entry sentinel — semantically equivalent to "no info".  Caller sites `known.get(&x).copied().unwrap_or_default()` become `known[x]` with same semantics, O(1) array access.
- **Estimated improvement:** 10-20% reduction in `KnownBits` pass time at 10k+ nodes.

### C — `validate::check_layer_c_uniqueness` + `check_layer_c_control_state` scan the full arena including zombies

- **Severity:** MED (perf only; never produces false-positives from zombies).
- **Where:** `crates/ir/src/validate/layer_c.rs:27, 61`.
- **Current:** Both iterate `graph.nodes.keys()` — full arena including detached zombies.  At 100k nodes with 50k zombies, half the scan is wasted.
- **Proposed:** Pass `reachable: &NodeIdSet` (already computed in `validate_with_options`) and guard each iteration.  `check_layer_c_uniqueness` may want to keep an arena-wide count for the multi-Entry/multi-InitialMemory check, but the per-node predicate body can be reachable-scoped.
- **Estimated improvement:** 20-40% reduction in `validate` time on graphs with high zombie ratio.

### D — `find_all_requirements` cross-product has no selectivity-based ordering

- **Severity:** LOW for M=2; HIGH for M≥4 with large per-pattern match sets.
- **Where:** `crates/pattern/src/matcher/mod.rs:469-485`.
- **Current:** O(N₁·N₂·…·N_M).  Only pruning is `acc.is_empty()` (entire result empty → stop).  No per-tuple short-circuit beyond that.  Within each prefix expansion, `prefix_agrees` is O(|prefix| · |bindings|).
- **Proposed:** Sort patterns by ascending match count before cross-product expansion.  Most selective pattern first → maximum chance of `acc.is_empty()` short-circuit.  `prefix_agrees` already returns false on first disagreement; the gain is purely from outer ordering.
- **Estimated improvement:** O(N_min/N_max) for unbalanced match counts.

### E — `compact.rs` builds an intermediate `HashMap<NodeId, NodeId>` for side-table remapping

- **Severity:** LOW.
- **Where:** `crates/ir/src/graph/compact.rs:201-206`.
- **Current:** Intermediate `HashMap` populated from `remap.nodes[old_id]` then immediately iterated.  Same data is already in `remap.nodes: SecondaryMap<NodeId, Option<NodeId>>` — direct lookup avoids the alloc.
- **Proposed:** Single-loop pattern over `&reachable` with direct `remap.nodes[old_id]` indexing.
- **Estimated improvement:** <1% (one-shot at finalize).

### F — `apply_in_place_edits` scans `graph.all_node_ids()` (zombie-inclusive) for `initial_var_index`

- **Severity:** LOW (in-place edits are uncommon).
- **Where:** `crates/strider/src/orchestrator.rs:529-535`.
- **Current:** Pre-built per-iteration index `for nid in graph.graph.all_node_ids()` — includes zombies.
- **Proposed:** Use `graph.preorder()` (reachable-only).  Live `InitialVar` nodes are always reachable through their consumers.

## Hash → DenseEntitySet missed migrations

| Location | Current | Should be |
|----------|---------|-----------|
| `opt/src/worklist.rs:21` | `FxHashSet<NodeId>` | `DenseEntitySet<NodeId>` |
| `opt/src/worklist.rs:85` | `FxHashSet<NodeId>` | `DenseEntitySet<NodeId>` |
| `opt/src/known_bits/mod.rs:379` | `FxHashMap<NodeOutputId, Kb>` | `SecondaryMap<NodeOutputId, Kb>` |
| `ir/src/graph/compact.rs:201` | intermediate `HashMap<NodeId, NodeId>` | direct `SecondaryMap` lookup |
| `strider/src/orchestrator.rs:137` | `HashMap<NodeOutputId, ExitVnToValue>` | `SecondaryMap` (LOW impact — bounded by region count) |

## Memory residency post-run

- `compact()` (in `retain_reachable`) GCs all four primary side-tables AND `wide_consts` via `gc_wide_consts()`.  Sound.
- `RunConfig::compact: bool` defaults to `true` → finalize call honours; zombie entries dropped at end of every `strider::run`.  If a caller sets `compact: false`, zombies persist (documented trade-off).
- `LoopState::vn_cache` (`HashSet<rsleigh::Vn>`) grows monotonically across CFG rebuilds — bounded by binary's distinct-varnode count (hundreds).  Not a concern.

## Python GIL hold-time

- `run_via_orchestrator` correctly wraps `strider::run` in `py.allow_threads`.
- `PyMemReaderAdapter::read` re-acquires GIL per call.  At ~5k machine instructions per 10k-node function, ~5k GIL ping-pong cycles × ~200-500 ns each = ~1-2.5 ms total — dominated by Sleigh decode time.  Acceptable.
- `PyReadOnlyMemoryAdapter::read` short-circuits non-RAM spaces before GIL acquisition.  Correct.

**No GIL action needed.**

## Summary

| Finding | Severity | Impact |
|---------|----------|--------|
| A — WorkSet hashset → DenseEntitySet | MED | 15-30% per pass |
| B — KnownBits hashmap → SecondaryMap | MED | 10-20% KnownBits |
| C — Layer C reachability scoping | MED | 20-40% validate on zombie-heavy graphs |
| D — find_all_requirements selectivity sort | LOW-HIGH | up to N_min/N_max |
| E — compact intermediate HashMap | LOW | <1% |
| F — orchestrator initial_var_index scan | LOW | rare path |

The two biggest wins at 10k+ are A (WorkSet) and B (KnownBits) — both inner-loop-of-every-pass.
