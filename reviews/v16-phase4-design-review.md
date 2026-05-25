# Phase 4 Memory SSA Design Review

**Branch:** `rewrite/ai` at commit `83683406`
**Date:** 2026-05-25
**Scope:** Verify the 2-kind non-phi `MemPartition`/`MemUnion` design (introduced by `AliasSplit` as an optimization) against the actual code before implementation.

---

## Gate 1: NodeOutputKind extension impact

### Findings

`NodeOutputKind` is currently a unit-variant enum:

```rust
// crates/strider-ir/src/node/output_kind.rs:9-21
pub enum NodeOutputKind {
    OutputType(NodeOutputType),
    Control,
    PhiToken,
    Memory,         // ← unitary, no payload
}
```

The plan proposes extending it to `Memory(Option<MemPartitionId>)`.

**Total `NodeOutputKind::Memory` references:** 75 (grep count — includes test files).
**Non-test production references:** 20 (excluding `*/tests.rs`, `*/snap`, `//` comments).

**Critical construction + match sites that need updates:**

- `crates/strider-ir/src/validate/mod.rs:100` — `kind_matches` uses `matches!(actual, NodeOutputKind::Memory)`. After the change this must become `matches!(actual, NodeOutputKind::Memory(_))` — otherwise the validator will reject all Memory edges.
- `crates/strider-ir/src/graph/access.rs:139` — `memory_output_of` uses `matches!(self.output_kind(out), NodeOutputKind::Memory)`. Same fix needed.
- `crates/strider-ir/src/node/output_kind.rs:85-87` — `is_memory()` uses `self == Self::Memory`. With a payload, `PartialEq` derivation still works; the equality check becomes `matches!(self, Self::Memory(_))` if `is_memory()` is refactored, OR stays as `==` only if `MemPartitionId` derives `PartialEq` (which the plan does: `#[derive(... Eq, PartialEq ...)]`). However `Memory(None) != Memory(Some(p))` by derived equality, so `is_memory()` must use `matches!` rather than `==` to match any Memory variant.

**Exhaustive match arms:** Because Rust exhaustive matching will reject `NodeOutputKind::Memory` arms at compile time after the change, the compiler will surface EVERY construction and match site. This is actually a benefit — the compiler becomes the touch-count tool.

**Estimate:** ~75 sites total (20 non-test production); all reachable by compiler errors after the variant change. The `(None)` suffix is mechanical to apply. Two sites (`kind_matches` in `validate/mod.rs` and `memory_output_of` in `access.rs`) require semantic decisions about whether to match any Memory or only unified Memory.

**The `ExpectedOutputKind::Memory` in `node_signature.rs`** is a separate enum, not the same type. Its `Memory` arm in `expected_signature` outputs stays as-is — the signature table describes "should be some Memory kind", and `kind_matches` is the one that does the actual comparison. Only `kind_matches` needs to know about partition variants.

**Verdict: ✅ Feasible.** ~75 sites, compiler-guided, mostly mechanical. Two sites need semantic thought (`kind_matches` and `is_memory()`).

---

## Gate 2: MemPhi phi-token invariant survives type change

### Findings

The phi-token checker lives in `crates/strider-ir/src/validate/graph_invariants.rs:103-167` (`check_graph_invariants_phis`).

It checks three things for `Phi | MemPhi | StackStorePhi`:
1. `input[0]` must have kind `PhiToken` (`token_kind != NodeOutputKind::PhiToken`)
2. The PhiToken producer must be a `Region` node
3. The count of value inputs (`inputs.len() - 1`) must match the Region's predecessor count

**None of these checks inspect the type of the value inputs (inputs[1..])** — only their COUNT. The checker only looks at `token_kind` (which is always `PhiToken`, not `Memory`) and counts predecessors.

After AliasSplit re-types a MemPhi's memory inputs from `Memory(None)` to `Memory(Some(P))` and its output from `Memory(None)` to `Memory(Some(P))`, the phi-token checker:
- Still sees input[0] with kind `PhiToken` — passes check 1.
- Still sees the PhiToken came from a Region — passes check 2.
- Still counts `inputs.len() - 1` memory inputs against Region predecessors — passes check 3.

**The `kind_matches` function in `validate/mod.rs:97-112`** handles local typing (Gate 1's concern). After the update to `matches!(actual, NodeOutputKind::Memory(_))`, the local-typing check on MemPhi's Memory inputs and output will accept both `Memory(None)` and `Memory(Some(P))` — correctly.

**Verdict: ✅ The existing phi-token checker does not care about the type of value inputs. A re-typed MemPhi with `Memory(Some(P))` inputs and output passes all existing invariant checks without modification.**

---

## Gate 3: MemPartition + MemUnion shape conflicts

### Findings

**`MemPartition { partition }` proposed signature:** `[Memory(None)] → [Memory(Some(P))]` — 1 input, 1 output.

Existing kinds with 1 Memory input → 1 Memory output:
- `Store(VnSpace)`: `[MEM, ADDR, DATA] → [MEM]` — 3 inputs, not 1. No conflict.
- `StackStore { .. }`: `[MEM, SP, DATA] → [MEM]` — 3 inputs. No conflict.
- `StackStorePhi { .. }`: `[PHI, MEM, DATA] → [MEM]` — 3 inputs. No conflict.

No existing kind has the `[Memory] → [Memory]` shape (single Memory input, single Memory output). **No shape collision for `MemPartition`.**

**`MemUnion` proposed signature:** `[Memory(Some(P_0)), Memory(Some(P_1)), …] → [Memory(None)]` — variadic Memory inputs, 1 output.

Existing kinds with variadic Memory inputs: **none**. `MemPhi` has variadic inputs but its signature is `[PHI; in_tail: MEM] → [MEM]` — the first input is `PhiToken`, not `Memory`. `MemUnion` starts at `Memory` for input[0].

Checked `node_signature.rs` exhaustively — no existing kind has an all-Memory variadic input list. **No shape collision for `MemUnion`.**

**Note on node cacheability:** Both `MemPartition` and `MemUnion` are non-phi, non-region nodes. The plan says they carry no phi-token. As non-cacheable nodes they must be declared so in `NodeKind::is_cacheable()` (or alternately, if cacheable, the dedup logic must be correct). The plan does not explicitly address this. `MemPartition { partition }` could be cacheable (same unified memory + same partition → same partition output). `MemUnion` is variadic and order-dependent (canonical partition-id order) so cacheability depends on whether input ordering is enforced. This is a design detail to nail down in Task 4.4 — it doesn't block the design but should be documented. See Gate 8.

**Verdict: ✅ No signature-shape collisions with existing kinds.**

---

## Gate 4: Orchestrator indirect-resolve loop interaction

### Findings

The orchestrator resolves `IndirectBranch` nodes via two mechanisms:

**Path A — `classify_anchor`** (`classify.rs:68`): Takes `anchor_output` (the `TARGET` value input of `IndirectBranch`, not the memory input) and classifies it by pattern-matching on `NodeKind` (IntConst, InitialVar, Phi, Load). **Never touches the memory chain.** `classify_anchor` is independent of memory partitioning.

**Path B — in-place editing** (`inplace.rs`): `detach_placeholder` reads `IndirectBranch`'s three inputs `[control_in, memory_in, target_value]` and passes `memory_in` to `apply_link_register` / `apply_tail_call`. Those functions wire `memory_in` directly into a new `Return` or `Call → Return` chain. They do not inspect the *type* of `memory_in` — they just pass it as an edge.

After AliasSplit runs:
- If the IndirectBranch's memory input is `Memory(None)` (unified), it passes through unchanged.
- If the memory chain has been partitioned, the IndirectBranch placeholder would be inside a partitioned subgraph. Whether `memory_in` would be `Memory(Some(P))` at that point depends on the AliasSplit pass's decision of whether to partition across `IndirectBranch` nodes.

**The real question:** Does AliasSplit need to emit a `MemUnion` before an `IndirectBranch`, so the placeholder always sees unified memory?

Per the plan: "force a MemUnion before the call, MemPartition after" — the same rule should apply to `IndirectBranch` (which semantically acts like an unresolved Call/Return). After resolution, the spliced `Call` or `Return` node's memory input should be unified.

**Practical consequence:** If AliasSplit always inserts `MemUnion` before any `IndirectBranch` (same rule as `Call`), then `memory_in` passed to `apply_link_register` / `apply_tail_call` is `Memory(None)`, and the newly created Call/Return nodes get wired with unified memory. The spliced `call_outputs.push(NodeOutputKind::Memory)` at `inplace.rs:228` emits a unified memory output — which is correct since the post-Call memory needs repartitioning.

**If AliasSplit does NOT add MemUnion before IndirectBranch**, the splicer gets `Memory(Some(P))` and wires it into the new Call. This would be wrong — `Call` nodes are defined to produce unified memory (all partitions clobbered).

**Verdict: ⚠️ The resolver code is oblivious to Memory kind and works correctly IF AliasSplit enforces the rule "always MemUnion before IndirectBranch." This rule is implied by the plan's "force MemUnion before external calls" policy but is not explicitly stated for IndirectBranch. Task 4.5's AliasSplit implementation must explicitly treat IndirectBranch like a Call w.r.t. MemUnion insertion. Low risk to add, but it must be in the spec for Task 4.5.**

---

## Gate 5: Partition consistency invariant enforceability

### Findings

The proposed invariant: "every node downstream of `MemPartition{P}` that produces Memory must produce `Memory(Some(P))`, until a MemUnion node consumes it."

**Existing validator structure:** `crates/strider-ir/src/validate/` has three checking functions called from `validate()`:
- `check_local_typing` — per-node signature check (already walks reachable nodes)
- `check_use_list_consistency` — bidirectional use-list check
- `check_graph_invariants_*` — graph-level rules

**Proposed check algorithm:** Start at each `MemPartition{P}` output. Walk forward through the Memory use-chain. Every node that consumes the Memory edge and produces a new Memory edge must produce `Memory(Some(P))`. Stop at `MemUnion` (correct boundary) or on reaching a node that produces `Memory(None)` (violation). This is a simple forward walk.

**Existing memory-edge walker:** No existing pass walks Memory edges *forward*. The existing `walk_mem_chain` walks *backward*. However, the Graph stores use-lists (`output.uses → Vec<NodeInputId>`) which enable forward traversal in O(n) — no new infrastructure needed.

**Complexity:** O(n) where n is the number of memory-producing nodes. No cycles are possible in the partition subgraph (MemPhi back-edges are handled by the existing cycle guard in the backward walker; forward walks through MemPhi successors would visit each MemPhi output exactly once).

**Edge case — MemPhi back-edges at loop headers:** A loop-header MemPhi has a back-edge input from the loop body. In a partitioned subgraph, that back-edge input would be `Memory(Some(P))`. Walking forward from `MemPartition{P}`, we'd arrive at the MemPhi from two directions: the forward entry edge and the MemPhi output re-entering the loop body. A standard visited-set guard handles this.

**Edge case — unreachable subgraphs:** The validator already restricts local-typing and phi checks to reachable nodes (`graph.reachable_kind_iter(&reachable)`). The partition consistency check should do the same.

**Verdict: ✅ The invariant check is O(n), implementable using the existing use-list infrastructure, and fits naturally in `check_graph_invariants_*`. The visited-set guard already used by other validators handles loop back-edges.**

---

## Gate 6: SP-decomposition reusability for AliasSplit

### Findings

**`decompose_sp`** lives at `crates/strider-analyze/src/opt/sp_expr/decompose.rs:134`.

Signature:
```rust
pub fn decompose_sp(
    g: &Function,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Option<SpExpr>
```

This is exactly what AliasSplit needs for Stack partition detection: feed it the address operand of a `Store` and get back an `Option<SpExpr>`. The `SpExprMemo` is a `SecondaryMap<NodeId, Option<SpExpr>>` — reusable across all store inspections in one AliasSplit run. **Reusable as-is.**

**`walk_mem_chain`** lives at `crates/strider-analyze/src/opt/mem_walk.rs:166`.

Signature:
```rust
pub(crate) fn walk_mem_chain<S: MemChainStep>(
    graph: &Function,
    initial_mem: NodeOutputId,
    cycle_policy: CyclePolicy,
    seen: &mut entity_utils::DenseEntitySet<NodeOutputId>,
    is_mem_phi: impl Fn(NodeId) -> bool,
    step: &mut S,
) -> Result<S::Verdict>
```

AliasSplit's chain scan is a FORWARD walk, not a backward walk. `walk_mem_chain` is a backward walker (starts at a memory output, walks to producers). AliasSplit needs to identify contiguous subgraphs that can be partitioned — it needs to walk the memory *successor* chain, not the *predecessor* chain.

**This is the primary infrastructure gap.** `walk_mem_chain` solves backward forking walks (used by `StackLoadForward.probe` and `FunctionArgDetect.mem_chain_is_dirty`). AliasSplit needs a forward successor traversal over the memory chain.

**Two implementation options for AliasSplit:**
1. **Direct use-list traversal** — for each memory-producing node, iterate `graph.output_uses(mem_out)` to find consumers. A simple BFS/DFS over Memory-kinded edges, O(n). No new generic primitive needed.
2. **New `walk_mem_chain_forward`** — a mirror of `walk_mem_chain` that starts at a memory output and walks forward to consumers. Reusable if a second forward walker arrives later.

Option 1 is simpler for a first implementation. The module doc at `mem_walk.rs:24-63` explicitly scopes `walk_mem_chain` to "forking backward walks only" and describes the two excluded linear backward walkers — suggesting that forward traversal should similarly stay local.

**`sp_expr/walk.rs`** contains the aliasing helpers (`step_through_store`, `step_through_stack_store`, `step_through_stack_store_phi`, `ranges_disjoint`). These are reusable by AliasSplit when classifying individual stores during the chain scan. **Reusable as-is.**

**Verdict: ✅ `decompose_sp` and the aliasing helpers are directly reusable. `walk_mem_chain` covers the backward direction but AliasSplit needs a forward traversal — direct use-list iteration is sufficient and does not require a new generic primitive.**

---

## Gate 7: Existing pass migration complexity

### `StackLoadForward`

Current code path for probe: calls `decompose_sp` on the load's address to get offset K, then walks the memory chain backward via `walk_mem_chain` with a custom `MemChainStep` that checks each node for `StackStore { offset: K }`, calling `step_through_store` / `step_through_stack_store_phi` for aliasing decisions.

After AliasSplit: the load lives inside the `Memory(Some(Stack))` partition. The probe only needs to walk the Stack partition chain — no `decompose_sp` on the address needed (partition membership is explicit). The `step_through_store` aliasing helper is eliminated from the hot path (those stores are now in the Heap/Unknown partition chain, invisible to the Stack probe). The `MemPhi` fork handling and cycle guard stay.

**Estimated reduction:** ~30% of `StackLoadForward` LOC comes from `decompose_sp` calls in the probe and the aliasing step helpers. The `realize` side is untouched. Consistent with plan's ~30% estimate.

**Test fixture impact:** Every test graph that creates a `StackStore` via `StackStoreDetect` must now have AliasSplit run first to produce `Memory(Some(Stack))` before `StackLoadForward` can see the partitioned chain. OR: `StackLoadForward` must remain backward-compatible during the transition period (Tasks 4.6-4.10 are sequential — 4.6 migrates SLF while 4.10 deletes StackStore). The plan's migration order means SLF must handle both forms during the transition.

### `CallStackArgCollect`

Currently walks the memory chain backward from each Call's memory input, stopping at `StackStore` nodes at known offsets. After AliasSplit, the Call sees unified memory (per Gate 4). The Stack partition chain is accessible via `MemPartition{Stack}` hanging off the Call's input unified-memory predecessor. The walk would start at the MemUnion preceding the Call, then follow the Stack partition arm.

**Estimated reduction:** ~15% as planned (the `decompose_sp` calls inside the aliasing step are removed, replaced by partition membership). Chain traversal logic stays.

**Test fixture impact:** Same as StackLoadForward — tests need AliasSplit to run before this pass.

### `FunctionArgDetect`

The no-shadow rule walks the memory chain backward from a `Load[sp+K]`'s memory input looking for disqualifying `StackStore { offset: K }`. After AliasSplit, the load is in the Stack partition, and the no-shadow walk only traverses `Memory(Some(Stack))` edges — skipping Heap/Call/Unknown nodes entirely.

**Estimated reduction:** ~5-10%. The fundamental DFS logic stays; the per-step classifier simplifies.

**Test fixture impact:** Same pattern as above.

### `LoadReadOnly`

Currently peepholes on any `Load(VnSpace::RAM)` with a constant address. After AliasSplit, ROM loads are in the `Memory(Some(Rom))` partition. The pass can simply gate on partition membership instead of doing address-space filtering.

**Estimated reduction:** 10-15% (address-space filtering replaced by a partition membership check). The `ReadOnlyMemory::read` call stays.

**Test fixture impact:** Tests that exercise `LoadReadOnly` without AliasSplit will need updating — the pass will no longer fire on `Memory(None)` loads.

**Overall test fixture assessment:** The biggest cost is that every unit test for memory-aware passes needs AliasSplit to run first (or needs a pre-partitioned graph). Since AliasSplit is planned to run always, integration tests are unaffected. Unit tests with synthetic graphs will need either: (a) AliasSplit called before the pass under test, or (b) a test-helper that constructs pre-partitioned graphs directly. Either is straightforward but adds ~10-20 LOC per affected unit test. The existing 417-snapshot test suite was already retired per the plan notes.

**Verdict: ✅ Estimates are consistent with the plan. Test fixture migration is real but manageable; the biggest effort is establishing the "run AliasSplit first" pattern in unit tests.**

---

## Gate 8: Risks for the AliasSplit pass design

### Risk 1: Loop-carried memory dependencies (MemPhi back-edges)

**Assessment:** When AliasSplit encounters a loop-header MemPhi, it sees a cyclic graph. The forward traversal from a potential `MemPartition{P}` insertion point would need to know "can this subgraph be cleanly bounded?" — but loop back-edges create cycles in the Memory successor graph.

**Mitigation (per the plan):** AliasSplit's idempotency requirement helps here: if a MemPhi has one input that is already `Memory(Some(P))` (from a prior iteration) and another that is `Memory(None)` (the back-edge not yet partitioned), AliasSplit can treat the entire MemPhi as "can't partition yet" and leave it as unified. On re-run, once the back-edge input is also partitioned, the MemPhi can be re-typed.

**Gap:** The plan does not describe how AliasSplit identifies the "safe boundary" for a partition around a loop. A conservative approach — only partition acyclic subgraphs — is sound but loses optimization opportunity for loops that store only to the stack. This is a design decision that needs to be made explicit in Task 4.5.

**Severity: ⚠️ A mitigation path exists (be conservative at MemPhi boundaries) but the exact rule is not specified.**

### Risk 2: Unknown-address stores

**Assessment:** A `Store(RAM, unknown_addr, data)` where `decompose_sp` returns `None` must go to the Unknown partition. Per the plan: "force MemUnion immediately." This means after every Unknown-partition store, unified memory is reconstituted. This is sound.

**Gap:** If many stores in a function have unknown addresses, AliasSplit inserts many MemPartition/MemUnion pairs. The overhead is bounded by the number of stores (O(n)) but the IR bloat could be significant for pointer-heavy functions. The plan does not discuss a threshold for "too many partitions → bail out and keep unified memory."

**Severity: ✅ Sound, manageable. No blocking gap.**

### Risk 3: External Calls

**Assessment:** The plan says "force MemUnion before the call, MemPartition after." This is correct: a Call node that clobbers all partitions must see unified memory and produce unified memory, from which new MemPartition nodes project fresh partition states.

**Gap:** This rule also applies to `CallOther` nodes (which model syscalls, intrinsics). The `target::CallOtherAbi` has a `memory_edge` flag — when false, the CallOther doesn't clobber memory. AliasSplit must consult this flag to decide whether to insert MemUnion/MemPartition around a CallOther. The plan mentions this implicitly ("external Calls force MemUnion") but doesn't explicitly list CallOther as requiring the same treatment.

**Severity: ✅ The fix is mechanical — treat CallOther with `memory_edge=true` the same as Call. The `target::call_other_abi::classify` already exposes `memory_edge` per ABI entry.**

### Risk 4: Idempotency

**Assessment:** "Re-running AliasSplit on partitioned IR must be a no-op."

A partitioned node produces `Memory(Some(P))`. If AliasSplit re-runs, it must recognize that a `MemPartition{P}` node is already a partition boundary and not insert another one upstream. The pass needs to guard: "if this node already produces `Memory(Some(P))`, skip it."

**Gap:** The forward chain scan must check `output_kind(mem_out) == Memory(Some(_))` as a "already done" guard. This is O(1) per node and straightforward, but must be explicitly in the AliasSplit spec.

**Severity: ✅ Straightforward guard; low risk.**

### Risk 5: MemPartition cacheability

**Assessment:** `MemPartition { partition }` is a non-phi node with 1 Memory input. If declared cacheable, two `MemPartition{Stack}` nodes fed by the same unified Memory output would deduplicate to one node. This is correct semantically — projecting the same partition from the same unified memory should yield the same partition token.

However, if AliasSplit inserts `MemPartition{Stack}` at two different program points in the chain (e.g., after two separate Calls that reconstitute unified memory), those two insertions must produce distinct partition tokens (they represent distinct memory states). Caching them together would merge distinct states.

**This is a real correctness risk**: if `MemPartition{Stack}` is cacheable and the graph deduplicator merges two `MemPartition{Stack}` nodes that project the same partition from *different* unified-memory predecessors, it would be wrong only if the two inputs are different nodes. The dedup cache keys on `(NodeKind, inputs, output_kinds)` — two `MemPartition{Stack}` nodes with different Memory inputs would NOT deduplicate (different keys). Same Memory input + same partition → same state → safe to deduplicate.

**Verdict:** Cacheability is safe if keyed correctly, because the cache key includes the input (unified Memory node). Two partition projections from different unified-memory states have different inputs → different cache keys → no spurious merge. **Recommend declaring MemPartition as cacheable.**

`MemUnion` is variadic. Its dedup key includes all N Memory inputs. Two MemUnion nodes with the same N partition inputs in the same canonical order would correctly deduplicate. **Recommend declaring MemUnion as cacheable.**

**Severity: ✅ No risk if the cacheability decision is documented in Task 4.4.**

### Risk 6: Node kind exhaustiveness in passes

**Assessment:** Every pass that exhaustively matches on `NodeKind` (or `NodeOutputKind`) in a memory context must be updated to handle `MemPartition` and `MemUnion`. The compiler will catch missing arms in exhaustive `match` expressions. Pattern builders in the Pattern DSL and PyO3 mirrors also need updates (plan lists these in Task 4.4 and the file list).

The plan enumerates these sites well. No hidden exhaustiveness gaps found beyond what's already listed.

**Severity: ✅ Compiler-detected. Plan already covers it.**

---

## Summary Table

| Gate | Status | Key Finding |
|------|--------|-------------|
| 1. NodeOutputKind extension impact | ✅ | 75 sites (20 non-test), all compiler-guided. Two semantic callsites: `kind_matches` and `is_memory()`. |
| 2. MemPhi phi-token invariant | ✅ | Validator checks only PhiToken type + input count — neither depends on Memory output type. Re-typed MemPhi passes unchanged. |
| 3. MemPartition/MemUnion shape conflicts | ✅ | No existing kind has `[Memory]→[Memory]` or variadic-Memory-inputs shapes. No collision. |
| 4. Orchestrator indirect-resolve loop | ⚠️ | `classify_anchor` is memory-oblivious (safe). In-place splicers pass `memory_in` directly. AliasSplit MUST treat IndirectBranch like Call when inserting MemUnion boundaries — this rule needs explicit documentation in Task 4.5 but is architecturally sound. |
| 5. Partition consistency enforceability | ✅ | O(n) forward walk via use-lists. Standard visited-set handles MemPhi back-edges. Fits existing validator structure. |
| 6. SP-decomposition reusability | ✅ | `decompose_sp` + aliasing helpers reusable as-is. `walk_mem_chain` is backward only; AliasSplit needs forward use-list traversal (direct, no new generic needed). |
| 7. Existing pass migration complexity | ✅ | Estimates consistent with plan. Test fixtures need "AliasSplit first" pattern for unit tests; integration tests unaffected. |
| 8. AliasSplit risk assessment | ⚠️ | Loop-MemPhi boundary rule not specified (needs conservative rule for loop headers). Unknown-addr/Call/CallOther handling sound but CallOther needs explicit mention. Cacheability and idempotency have clear solutions. |

---

## VERDICT

**YELLOW — proceed with two clarifications before starting Task 4.5.**

The overall design is sound. The 2-kind non-phi approach (MemPartition + MemUnion introduced by AliasSplit) is architecturally consistent with the existing codebase. No blocking structural contradictions found in any gate.

Two items need to be written into the Task 4.5 spec before implementation begins:

**Clarification 1 — IndirectBranch MemUnion rule (Gate 4):**
Add to Task 4.5: "AliasSplit treats `IndirectBranch` identically to `Call` when deciding where to insert MemUnion/MemPartition boundaries. Every IndirectBranch placeholder must see unified Memory(`None`) on its memory input after AliasSplit runs." This ensures the in-place splicers in `inplace.rs` always wire `Memory(None)` into spliced Call/Return nodes.

**Clarification 2 — MemPhi loop-header boundary rule (Gate 8, Risk 1):**
Add to Task 4.5: "At a MemPhi whose inputs include a back-edge (i.e., one input is already Memory(Some(P)) and another is Memory(None) from a not-yet-partitioned loop predecessor), AliasSplit must NOT insert MemPartition/MemUnion around the MemPhi in the current pass iteration. Leave the MemPhi and the entire loop's memory chain in unified form for that iteration. Idempotency ensures a subsequent AliasSplit run can partition it once the loop body is also partitioned." This prevents the AliasSplit pass from generating an invalid mixed-type MemPhi (one arm None, one arm Some(P)).

Both clarifications are **additive spec text**, not design changes. The existing code infrastructure supports both rules without modification.
