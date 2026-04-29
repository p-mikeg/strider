# Deep CR — 5-round summary

Consolidated outcome of the 5-round deep code review the user requested:
correctness across all 15 archs (don't trust comments), readability,
simplification, generalization, dead-code removal, and test gaps.

Range: `973679c..d5300f3` on `feature/ai`.

## Headline numbers

- **30 commits** across pre-deletion + 5 rounds + final report.
- **81 files changed**, +3 218 / −3 038 (net +180 LOC including six review reports totalling ~1 500 LOC; **code-only delta is roughly −1 200 LOC**).
- **Tests**: 2 896 → **2 878** (−18 net: 50 deleted with fingerprint/lift-stats, +32 added across rounds).
- **Clippy**: clean throughout, `--workspace --all-targets -- -D warnings`.

## Round-by-round

### Pre-step — surface deletion (3 commits)
User direction: *"delete fingerprint because it doesn't work due to caching"* and *"remove lift stats and stuff like it that you used to verify yourself."*

- `645d38e` strider: delete `OrchestratorStats` / debug-trace plumbing
- `a160a97` strider: delete `LiftStats` and the with_stats lift variant
- `5488f94` ir+opt+pattern+strider: delete the `Fingerprint` side-table and propagation

Surface: 26 files modified, 4 files deleted (`stats.rs`, `fingerprint.rs`, `fingerprint_tests.rs`, `tier2_debug_trace.rs`, `fingerprint_e2e.rs`). `PcodeInsnAddr` survived in `cfg::` (still used by `Region` for instruction tracking); `ir::PcodeInsnAddr` was a dead duplicate (caught by R3).

### R1 — full-codebase correctness audit
**2 critical shift-semantic bugs fixed**, both affecting all 15 archs:
- `1c2c9b0` opt: `INT_LEFT/RIGHT/SRIGHT` fold on shift `>= bit_width` returned the operand instead of 0 (or sign-extended -1 for SRIGHT). Diverged from Sleigh's `OpBehaviorIntLeft::evaluateBinary`.
- `60b46fe` opt: `KnownBits` shift propagation masked shift count with `bit_width − 1`, producing `1u8 << 8 → Kb { ones: 1, zeros: 0xFE }` and then planting a wrong `IntConst(1)`.

Per-arch lens (15 ISAs) caught arch-INDEPENDENT bugs in shared evaluators — confirms the lens but the fix lives outside any arch-specific layer.

### R2 — pattern dogfooding
**1 refactor landed, 11 considered + rejected with concrete rationale.**
- `c99805a` opt: `stack_array::strip_target_mask` migrated to `pattern::and / pattern::or` with auto-commutativity. 6 characterization tests pin both operand orderings, the canonical ARM/Thumb `And(Or(load, 1), 0xFFFE)` interworking, the no-strip-on-overlap branch, and nested-And mask intersection.

Honest about pattern's limits: most rejected refactors involve transitive walks (memory chains, SP decomposition, control walks) that are not tree-shaped and can't be expressed by `Pat`.

### R3 — simplification (initial pass, rejected by user)
5 wrapper-removals: dead `ir::PcodeInsnAddr`, `BranchResolution`/`ResolvedTargets` enum collapse, dropped 2 empty `tests/common/tier2_helpers/*.rs` placeholders, dropped 2 unused F5 strider shim files, inlined `find_placeholder_return_for_anchor`. Net −170 LOC, 5 files deleted.

The user pushed back: *"this is not a real simplification + generalization — you just removed wrappers. the walk back logic already exists in pattern for example, and the bit knowledge exists in known bit — so why redo it in jump table? THERE is still a lot to simplify / improve!"*

### R3-extension — real generalizations
**Two real consolidations the previous round missed**:
- `d5a6bf3` opt: extract `Kb` + `node_known_bits` as `pub`; new `pub fn opt::analyze_known_bits` runs the worklist to fixed point without rewriting. `KnownBits::optimize_built` now calls the analyzer + does the rewrite. `jump_table::compute_max_mask` deleted (~78 LOC); bounds are read from `Kb::max_value`. Strictly tighter — the analyzer covers `Or`, `Xor`, `Not`, `Popcount`, `Lzcount`, `ShiftLeft` in addition to the previous five.
- `1e90e9c` opt: `bound_from_if_condition`'s manual `match cmp_op { Less | Sless if !swapped => ... }` block collapsed to a single `pattern::int_cmp_any(op_var, var(idx_var), any_int_const(n_var))`.

**Honest scope note**: `bound_via_predecessor_if`'s backward dominator-walk has NO pattern equivalent. `pattern::try_walk_through_control_state` is single-hop; the walk is transitive across arbitrary `If`/`ControlState`/other Control producers with cycle detection. Verified by reading `crates/pattern/src/matcher/walk_through.rs`.

### R4 — readability
**7 commits**, code-only delta ~−111 LOC:
- `b6312bd` + `187709a` ir: add `Graph::kind_of_output(out) → &NodeKind` accessor + migrate ~50 callsites across 19 files.
- `45f6675` opt: strip stale `F5` / "canonical implementation" provenance prose from `indirect_branch_resolve/{mod,classify,inplace}.rs`.
- `638d74c` strider: rename `tests/manual_rewrite.rs` → `tests/graph_rewriter.rs` (the file's tests use `pattern::rewrite_rule` + `GraphRewriter`).
- `9187fd3` strider: merge tiny `insn/{memory,misc}.rs` (one method each, 80 LOC combined) into parent `mod.rs`.
- `40dd525` opt+pattern: drop three dead `#[allow(dead_code)]` items.
- `8ebc40f` workspace-wide: strip `F2/F3/F5/W2/W5/W10` project codenames from doc comments; deleted the 4-line "F2 bridge" comment block pasted at 10 `with_built` callsites.

### R5 — test gap audit
**17 tests added, 0 bugs fixed, 1 deferred (documented):**
- `d2e88d9` opt: 5 tests pinning `bound_via_known_bits` / `bound_via_predecessor_if` edge cases (multi-pred ControlState fail-closed, cycle handling in the new analyzer).
- `a6ee354` strider: 7 tests pinning orchestrator helpers — `apply_split_invalidation` eviction trigger, `is_tail_call` boundaries.
- `270f32f` opt: 1 test pinning `Sless` current-behavior bound + flag soundness caveat (Sless is treated identically to Less in `bound_from_if_condition`; only matters for negative-idx jump tables — typical compilers fold Sless+Sge into a single unsigned compare).
- `f631639` strider: 4 tests pinning `read_or_init_var` dedup contract (cache hit, scan reuse, fresh create, byte-size fallback).

R1's deferred F-R1-A (`extract_idx_and_stride` bit-width check): re-confirmed not exposable in practice. Real lifters never emit `<<8` on a U8 varnode because address arithmetic is wider.

## Cumulative impact

**What's actually different now:**

1. **Two real per-arch correctness bugs gone** (shift semantics) — both would have produced wrong constants in any compiled program containing a shift by a runtime-constant amount equal to the bit width. The fact that 15 archs share these evaluators meant 15 archs were broken.
2. **`KnownBits` is now a reusable analyzer** — `jump_table` no longer reinvents it, and any future pass that wants bit-knowledge can call `analyze_known_bits` instead of writing its own propagator.
3. **`graph.kind_of_output(out)` collapses the 50-callsite `get_node_from_output` + `node_kind` boilerplate** — removed actual repeated friction, not just renamed it.
4. **Telemetry surface gone** — `Fingerprint`, `OrchestratorStats`, `LiftStats`, `OrchestratorDebugConfig`, `EditEvent`, `KnownTargetUpdate`, `IterationSnapshot`, `DebugTrace` all deleted. Code that existed solely so the author could verify their own work no longer pollutes the production graph.
5. **Provenance prose stripped** — `F5 relocates`, `canonical implementation lives in opt`, `lands in R3`, `BUG-29`, `BUG-30` no longer appear in module headers. Doc comments now describe what code DOES, not what historical PR introduced it.
6. **Two cross-crate enums collapsed** (`BranchResolution` ↔ `ResolvedTargets`).
7. **17 new tests pin edge cases** that prior coverage missed — multi-predecessor ControlState bound, split-invalidation eviction, dedup contract, Sless soundness caveat.

## What deliberately did NOT change

- The `cfg::PcodeInsnAddr` type — actually used.
- F5 strider shim layer (`tier2/inplace.rs`, `tier2/classify.rs`) — kept; the orchestrator wraps them and they bridge `opt::Error` → `strider::Error`.
- The 15 calling-convention modules in `crates/strider/src/strider/cc/` — already share a `CallingConvention` data struct in `crates/target/`; no boilerplate to consolidate.
- `bound_via_predecessor_if`'s backward dominator-walk — no pattern equivalent exists.
- `same_value` (trivial-phi chaser) — single-purpose; not duplicated.
- Cycle protection in walks — kept where load-bearing.

## Hand-off for next deep CR

Per R5's hand-off, the most likely next bug-magnet area is **fixed-point convergence-cap testing** — the orchestrator's outer loop has a cycle/cap, and the cache invalidation paths interact with it in ways that haven't been fully exercised. The Sless soundness caveat in `bound_from_if_condition` is also worth tightening if a real binary surfaces a negative-idx switch.

Per-round reports for full detail:
- [R1 — correctness](2026-04-29-r1-correctness.md)
- [R2 — pattern dogfooding](2026-04-29-r2-pattern-dogfooding.md)
- [R3 — simplification (initial)](2026-04-29-r3-simplification.md)
- [R3 — extension (real generalizations)](2026-04-29-r3-extension.md)
- [R4 — readability](2026-04-29-r4-readability.md)
- [R5 — test gaps](2026-04-29-r5-test-gaps.md)
