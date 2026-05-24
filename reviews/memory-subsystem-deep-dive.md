# Strider Memory Subsystem: Deep-Dive Review

**Scope:** Inventory and architectural analysis of memory model primitives, optimization passes, and pattern DSL for an LLVM-style Memory SSA redesign.

**Date:** 2026-05-24

---

## 1. Current Memory Node-Kind Inventory

### Summary Table

| NodeKind | Signature | Producer(s) | Semantics |
|----------|-----------|-------------|-----------|
| `InitialMemory` | `[] → [Memory]` | IR builder (entry) | Memory state at function entry; one per function. |
| `MemPhi` | `[phi_token, mem_0, mem_1, ...] → [Memory]` | IR builder (control merge) | Memory join at CFG region header; selects live memory from predecessors. Non-cacheable (phi identity matters). |
| `Load(space)` | `[memory, addr] → [value]` | pcode-lift (`handle_load`); pattern rewrite | Read from address space `space` (typically `RAM`, rarely `REGISTER`/`UNIQUE`). Memory-dependent; addr may be computed. |
| `Store(space)` | `[memory, addr, data] → [memory]` | pcode-lift; pattern rewrite | Write to address space `space`. Advances memory token; clobbers alias-overlapping reads. |
| `StackStore { offset }` | `[memory, base, data] → [memory]` | `StackStoreDetect` pass | Store whose address resolves to `base + offset` where `base` is SP-rooted (`InitialVar(sp)` or `VarPhi(sp)`). Advances memory; disjoint from heap stores (separate partition). |
| `StackStorePhi` | `[phi_token, memory, data] → [memory]` | `StackStoreDetect` pass | Store whose address is an SP-phi of per-branch offsets; stored in `Graph::stack_phi_offsets` side-table (not inline). Advances memory; treated as transparent if per-branch offsets are disjoint from query range. |
| `Call` | `[ctrl, memory, call_addr, *args, *clobbers] → [ctrl, memory, *clobbers]` | IR builder; pcode-lift terminator | Function call. Clobbers caller-saved registers and **always produces a new memory token** (models memory arbitrarily modified by callee). Can be overridden: `BuiltCallingConvention::no_memory_clobber() = true` suppresses memory advance. |
| `CallOther { user_op_id }` | `[ctrl, memory, *args, *implicit_reads] → [ctrl, memory, value?, *implicit_writes]` | pcode-lift (user-op dispatch); `target::CallOtherAbi` | Opaque operation (intrinsic, syscall, etc.). Always clobbers memory (ABI-specific implicit reads/writes). Non-cacheable. |

### Key Observations

1. **Space field (`VnSpace`):** `Load` and `Store` carry a `VnSpace` tag (e.g., `RAM`, `REGISTER`, `UNIQUE`). In practice, strider lifters mostly emit `RAM` (the flat heap/stack address space); `REGISTER` and `UNIQUE` are rare and typically rewritten to value computation early by the optimizer.

2. **StackStore/StackStorePhi specialization:** These nodes carve out the SP-relative stack frame as a separate partition, allowing:
   - Store-to-load forwarding via `StackLoadForward` (classic SSA renaming within stack frame).
   - Stack-argument collection via `CallStackArgCollect` (reconstruct calling-convention args from store chain).
   - Function-argument detection via `FunctionArgDetect` (detect which loads are canonical arg accesses).

3. **MemPhi non-cacheability:** Memory joins are not deduplicated (`NodeKind::is_cacheable()` returns false for `MemPhi`). Each region header produces a unique `MemPhi` instance even if control structure is identical, because phi identity (which region, which predecessor index) carries semantic meaning.

4. **Call memory clobber contract:** `Call` nodes always produce a memory output (`outputs[1]`). When `no_memory_clobber = true`, the output is not wired into the region's memory chain (dangling), so downstream loads see the pre-call memory. This is used for pure functions or immutable-memory-only callees (e.g., ROM reads).

5. **CallOther opaqueness:** `CallOther` carries only a `user_op_id` reference to `target::CallOtherAbi` for the precise ABI shape (arg counts, implicit reads, memory edge flag). Memory is always clobbered unless the ABI specifies `memory_edge = false` (rare).

---

## 2. Memory-Aware Optimization Passes

### 2.1 `StackStoreDetect` (opt/src/stack_store/detect.rs)

**Contract:** Rewrites `Store(_)` nodes whose address resolves to `InitialVar(sp) + K` (or `VarPhi(sp) + per-branch offsets`) into specialized `StackStore` / `StackStorePhi` nodes.

**Inputs:** All `Store(_)` nodes in the graph.

**Outputs:** 
- For each `Store` at address `sp + K`: emit `StackStore { offset: K }` with same inputs/outputs as the original.
- For each `Store` at address that is a phi of SP with per-branch offsets: emit `StackStorePhi` and record per-branch offsets in `Graph::stack_phi_offsets`.
- Absorb rewritten Store's asm-fingerprint into the new node (so downstream consumers can recover source instructions).

**Invariants:**
- Runs inside the main fixed-point loop (reachable after `ConstantFold` and `RedundantPhis` fold address arithmetic).
- SP-expression decomposition uses memoized `SpExpr` walk to avoid redundant work across multiple stores.
- Only rewrites stores that provably decompose; non-SP-rooted stores are left untouched.

**Role:** Gate to downstream passes. Without this, raw `Store(RAM, ...)` nodes obscure stack-frame intent; with it, the stack frame becomes a first-class partition.

---

### 2.2 `StackLoadForward` (opt/src/stack_load_forward/mod.rs)

**Contract:** Forwards `Load(_, sp + K)` to its source `StackStore { offset: K }` (or synthesizes a `ValuePhi` when crossing a `MemPhi` with all-preds-agree stores).

**Inputs:** All `Load(_)` nodes; requires `StackStore` / `StackStorePhi` nodes in the graph (produced by `StackStoreDetect`).

**Outputs:**
- For matching `Load[sp + K]` with a dominating `StackStore { offset: K }`: replace load with the store's data output (or `Truncate(data)` for narrow loads on BE).
- For `Load[sp + K]` across a `MemPhi`: probe each predecessor memory chain; if all resolve to the same offset, emit `ValuePhi` with the per-pred values.
- Otherwise: no-op (leave `Load` untouched).

**Key sub-contracts:**
- `probe()` (read-only): Walks memory chain backward through `StackStore`, `Store` (with disjointness checks via `decompose_sp`), and `MemPhi`. Returns a `ResolveShape` tree describing how to materialize the forwarded value.
- `realize()` (creates IR): Materializes the shape, synthesizing `Truncate` / `ShiftRight` / `ValuePhi` nodes only once the full shape is known.
- Memoization: `SpExprMemo` amortizes repeated address decompositions; `visited` set guards cycle detection at `MemPhi` boundaries.
- Narrow-load handling: LE targets use `Truncate(data)` directly; BE targets synthesize `Truncate(ShiftRight(data, shift_bits))`.

**Public helper (`find_stack_stored_value_at_offset`):** Used by the indirect-branch classifier to enumerate jump-table entries without rewriting the load (returns `Option<NodeOutputId>` at a concrete offset, or `None` if chain lacks a matching store or hits an aliasing intermediate).

**Role:** Eliminates redundant stack-frame memory edges; enables subsequent optimizations to see constants and reconstructed values instead of opaque `Load` operations.

---

### 2.3 `CallStackArgCollect` (opt/src/stack_store/call_args.rs)

**Contract:** Walks the memory chain backward from each `Call` node's memory input, collects `StackStore` data outputs at known stack-argument offsets, and appends them as extra `Call` inputs (in positional order).

**Inputs:** All `Call` nodes; requires `StackStore` nodes upstream in the memory chain.

**Outputs:** For each `Call`, appends discovered stack-argument values as inputs: `[ctrl, mem, addr, *reg_args, *stack_args]`.

**Collection invariants (two safety rules):**
1. **Set membership:** Each store's `offset - chain_anchor` must be in `convention.stack_arg_offsets`. An out-of-set offset terminates the walk (boundary between outgoing-args window and local-variable region).
2. **Prefix monotonicity:** Once a contiguous slot prefix `[0..=k]` is filled, any new slot must be in `[0, k+1]`. A gap indicates we've crossed from the args-push window into frame locals.

**Chain-order notes:**
- Does NOT enforce store-in-program-order (earlier gcc/clang x86 cdecl emits arg0, arg1 in that order, landing arg1 at the chain head).
- Instead: first store anchors the relative origin; later stores fill by relative offset.
- Handles x86 ret-addr push edge case: when the anchor store itself is not in `stack_arg_offsets` (x86 call pushes ret-addr at SP-4/-8 pre-call), the `is_first_store` exception allows the walker to skip the boundary and continue upstream.
- Most-recent-wins for repeated-slot writes: backward walk sees most recent write first; later writes to the same slot are skipped.

**Aliasing:**
- `StackStore`: always chain-terminating (provably aliases the stack-arg space).
- `StackStorePhi`: conservatively chain-terminating.
- Plain `Store`: decompose address via `decompose_sp`. If non-SP-rooted (provably non-aliasing), pass through; if SP-rooted, terminate conservatively.

**Runs:** Once, as a post-pass after fixed-point loop converges.

**Role:** Reconstructs calling-convention arguments from prologue stores; enables value-range analysis and taint tracking of call arguments.

---

### 2.4 `FunctionArgDetect` (opt/src/function_args/mod.rs)

**Contract:** Detects canonical function arguments and replaces their reads with `FunctionArg` nodes keyed by calling-convention index.

**Inputs:** All `InitialVar` and `Load[sp + K]` nodes; requires `StackStore` nodes upstream (produced by `StackStoreDetect`).

**Outputs:**
- For each register arg `R` in `cc.arg_passing_regs()`: if `InitialVar(R)` has live uses, emit `FunctionArg { Register(R), index }` and rewire all uses.
- For each stack arg at offset `K` in `cc.stack_arg_offsets()`: if a `Load[sp + K]` is reachable from `InitialMemory` without shadowing stores, emit `FunctionArg { Stack { offset: K }, index }` and rewire all loads.

**Stack-arg detection (no-shadow rule):**
- Walks memory predecessors backward from each `Load[sp + K]`; stops at `InitialMemory` (success: no shadow) or any disqualifying node (failure).
- Disqualifying: `StackStore { offset: K }`, `StackStorePhi` whose per-branch offsets contain `K`, or any un-decomposed `Store(_)` (conservative).
- Non-disqualifying: `Call`, `CallOther`, `InitialMemory`, stores at other offsets.
- Treats `MemPhi` as a fork: must be non-disqualifying in ALL predecessors.

**Sub-register fallback:**
- IR builder may emit `InitialVar(ECX size=4)` instead of `InitialVar(RCX size=8)` for a 32-bit int arg on x86_64.
- When exact-`Vn` lookup misses, search for any `InitialVar` within the container register's byte range; pick the largest (most specific).
- Emitted `FunctionArg` records the actual sub-register, so downstream sees the width the function reads.

**Contiguity:** Stack args must form a gap-free prefix starting at index `|arg_passing_regs|`; first gap truncates.

**Hygiene:** Post-pass detaches unreachable address-computation chains (e.g., orphaned `Add(sp, K)` nodes) to avoid confusing downstream consumers.

**Runs:** Once, as a post-pass after fixed-point loop converges.

**Role:** Normalizes argument reads to a canonical form; enables higher-level analyses and value-inference queries keyed by calling convention.

---

### 2.5 `LoadReadOnly` (opt/src/load_readonly/mod.rs)

**Contract:** Resolves `Load(space, addr)` nodes with constant addresses against a `ReadOnlyMemory` ROM image, replacing them with the loaded constant value.

**Inputs:** All `Load` nodes with constant addresses.

**Outputs:** Replaces matching loads with `IntConst` nodes of the loaded value.

**Memory-space contract:**
- Forwards the `Load`'s `VnSpace` to `ReadOnlyMemory::read(space, addr, size)` verbatim.
- Impl **must** return `None` for any space it does not back (else silent data corruption).
- Endianness: impl is responsible for byte-swapping per target endianness; this pass does not double-swap.

**Width handling:** Rejects loads > 8 bytes (U80/U128/U256/U512) rather than asking impl to truncate.

**Role:** Constant-propagates ROM reads (e.g., `.rodata`, `.text` relocations); essential for x86 RIP-relative and ARM thumb constant pools.

---

### 2.6 Supporting Abstractions: `sp_expr` (opt/src/sp_expr.rs)

**`SpExpr` enum:**
- `Terminal { base: NodeOutputId, offset: i64 }` — an SP-rooted value `base + offset` where `base` is `InitialVar(sp)` or a `VarPhi(sp)`.
- `Phi { phi_node: NodeId, offsets: Vec<i64> }` — a `VarPhi(sp)` where every predecessor resolves to `Terminal` with the same base but different per-branch offsets.

**`decompose_sp(graph, output, sp_vn, memo, visiting)`:** Core workhorse. Given an output (typically an address), returns `Option<SpExpr>`. Walks through `Add`/`Sub` of constants, coalesces via `VarPhi(sp)`, and returns either a terminal offset or a per-branch phi. Uses cycle-guard `visiting` set to prevent infinite loops on SP phi cycles (common at loop headers).

**Memoization:** `SpExprMemo = SecondaryMap<NodeId, Option<SpExpr>>`. Amortizes decomposition cost across multiple passes and call sites.

**Aliasing helpers:**
- `ranges_disjoint(a_off, a_size, b_off, b_size) → bool`: Checks if `[a_off, a_off+a_size)` and `[b_off, b_off+b_size)` are disjoint. Uses saturating addition for unknown extents (treated as unbounded → "not disjoint").
- `step_through_store(graph, node, sp_vn, memo, query_off, query_size) → AliasStep`: Decides if a `Store` can be passed through (non-aliasing) or terminates the walk.
- `step_through_stack_store(...)`: Same for `StackStore` — uses exact offset/size stored in `NodeKind`.
- `step_through_stack_store_phi(...)`: Same for `StackStorePhi` — checks per-branch offsets in `Graph::stack_phi_offsets`.

**Role:** Abstraction boundary for SP-relative address reasoning. All stack-aware passes use this to classify stores and loads.

---

## 3. Pattern DSL Surface for Memory Operations

### Located in `crates/pattern/src/pat/builders/memory.rs`

**`LoadPat` builder:**
```rust
load()
    .space(rsleigh::VnSpace::RAM)          // Optional: restrict to address space
    .addr(pattern_for_address)              // Optional: constrain address
    .mem_in(pattern_for_memory)             // Optional: constrain memory predecessor
    .bit_width(64)                          // Optional: restrict load width
```
- Matches `Load` nodes; no `.next_mem()` method (Load produces only a value output, not memory).
- Captures via `.capture(var)` from `IntoPat` trait.

**`StorePat` builder:**
```rust
store()
    .space(rsleigh::VnSpace::RAM)           // Optional: restrict to address space
    .addr(pattern_for_address)              // Optional: constrain address
    .data(pattern_for_data)                 // Optional: constrain stored value
    .mem_in(pattern_for_memory)             // Optional: constrain memory predecessor
    .next_mem(pattern_for_consumer)         // Optional: match unique memory consumer
    .bit_width(64)                          // Optional: restrict data width
```
- Matches `Store` nodes; `.next_mem()` matches the unique consumer of the store's memory output (fails if 0 or >1 consumers).

**`StackStorePat` builder:**
```rust
stack_store()
    .offset(i64)                            // Optional: exact offset
    .offset_any(vec![offset1, offset2])     // Optional: set membership
    .data(pattern_for_data)                 // Optional: constrain stored value
```
- Matches `StackStore` nodes; offset constraints are AND-combined.
- Does not expose memory predecessor or base register in the builder API (but inputs are accessible via post-match callback).

**`StackStorePhiPat` builder:**
```rust
stack_store_phi()
    .offsets(vec![offset1, offset2, ...])   // Optional: per-branch offsets (multiset comparison)
    .data(pattern_for_data)                 // Optional: constrain stored value
```
- Matches `StackStorePhi` nodes; offset comparison is order-insensitive.

**`MemPhi` matching:**
- No dedicated builder (use `NodePat` directly with `KindSpec::variant(&NodeKind::MemPhi)`).
- Inputs: `[phi_token, mem_0, mem_1, ...]` — the phi-token dispatch edge followed by N memory predecessors (from CFG in-degree).

**Capture constraints:**
- `LoadPat` / `StorePat` capture the node's **value** output (index 0 for Load, index 0 for Store). Memory outputs are not value-typed, so pattern capture enforces value-kind filtering at bind time.
- Memory-chain walk patterns use `.mem_in()` / `.next_mem()` to navigate predecessors/consumers in the standard data-flow direction.

---

## 4. Sleigh Memory Model and VnSpace

### Address Space Representation

Sleigh p-code defines named address spaces (ram, register, unique, const, etc.). The strider lifter decodes the `space_id` varnode from each `Load`/`Store` p-code instruction via `rsleigh::VnSpace::by_id()`.

**In practice:**
- **RAM:** Flat heap/stack memory (the primary address space for all `Load`/`Store` operations on real programs). This is where `MemPhi`, `StackStore`, and memory-chain walks live.
- **REGISTER:** Virtual address space for register reads/writes. Rarely appears in lifted `Load`/`Store` (x86/ARM Sleigh specs mostly emit register operations as direct varnode reads/writes, not memory operations). When present, usually early-rewritten to value computation by the optimizer.
- **UNIQUE:** Sleigh's scratch space for temporary values. Even rarer in `Load`/`Store`.
- **CONST:** Read-only constant pool. Sleigh may emit `Load(CONST, offset)` for constant-pool access; strider's `LoadReadOnly` pass can resolve these.

### Key Insight

Strider's `VnSpace` field on `Load`/`Store` is architectural—it names the address space—but **the optimizer treats all non-RAM spaces as rare** and doesn't specialize passes for them. The memory chain (`MemPhi` → `StackStore` → `Call` etc.) is implicitly RAM-only.

Future extensions (e.g., taint tracking of I/O port reads, or modeling MMIO address spaces) would need to:
1. Extend passes to inspect `VnSpace` and apply different rules per space.
2. Or: introduce separate memory partitions per space (following the Memory SSA design below).

---

## 5. Proposed MemPartition/MemUnion Shape for LLVM-Style Memory SSA

### Design Goal

Generalize the stack-vs-heap partition (currently hard-coded in `StackStore` / `StackStorePhi`) into a dynamically-discovered set of N disjoint memory partitions. Each partition models a distinct alias class (stack frame, heap, I/O, ROM, etc.). `MemUnion` at control-flow joins reinstates the full memory state as a union of per-partition tokens.

### Proposed Node Kinds

```rust
/// Memory partition identifier: opaque u64 key into the partition table.
/// Assigned by alias-analysis pass; stable within a function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct MemPartitionId(u64);

// ── New memory model nodes ────────────────────────────────────────────

/// Single partition's memory state at a definition point.
/// Inputs: [memory_predecessor_in_same_partition, operation_output]
/// For a StackStore-like store: inputs[0] is prior mem, inputs[1] is data.
/// For a Call: inputs[0] is prior mem, inputs[1] is call output (or empty).
/// Outputs: [MemPartition(partition_id)]
MemDef { partition: MemPartitionId },

/// Single partition's memory state at a use point (load instruction).
/// Inputs: [full_memory_token, ...]
/// Outputs: [value_or_none] (load produces a value; use-only nodes produce nothing).
MemUse { partition: MemPartitionId },

/// Partition join at control-flow merge: selects live partition state from predecessors.
/// Inputs: [phi_token, state_0, state_1, ...] where states are MemDef or MemPartitionUnion outputs.
/// Outputs: [MemPartition(partition_id)]
MemPartitionPhi { partition: MemPartitionId },

/// Union of all partitions' states at a control-flow merge or after a call.
/// Represents "the full memory state after this point" by bundling partition tokens.
/// Inputs: [phi_token, partition_state_0, partition_state_1, ...]
/// Outputs: [MemoryUnion] (a new edge type, or a special multi-output structure)
MemUnion,

// ── Compatibility nodes (sunset after migration) ────────────────────

/// Alias-analysis discovery pass: scans the graph and assigns partitions.
/// Input: any valid IR graph (post-StackStoreDetect).
/// Output: annotates Graph with partition table; emits MemDef / MemUse nodes.
PartitionDiscovery,
```

### Memory SSA Invariants

1. **Partition exclusivity:** Every memory-modifying operation (store, call, user-op) belongs to exactly one partition set. A `Call` that clobbers multiple partitions creates a `MemDef` per partition.

2. **Partition consistency:** All `MemDef`/`MemUse`/`MemPartitionPhi` nodes referring to the same partition form a linear SSA chain within that partition. Across partitions, the chains are independent.

3. **MemUnion at joins:** After a `ControlState` (CFG merge), emit one `MemUnion` that bundles all per-partition phis. This reconstructs the full memory state for subsequent operations.

4. **Load/Store precision:** A `Load` / `Store` declares which partition(s) it accesses. A load from partition P only depends on `MemUse(P)`, not on unrelated partitions. A store to P invalidates all prior memory states in P but does not affect other partitions.

5. **Call clobbering:** When a `Call` has memory edge, it clobbers a caller-specified set of partitions (from ABI). Each partition gets a `MemDef(partition)` output. `no_memory_clobber = true` suppresses all partitions.

### Alias-Analysis Discovery Pass

**Input:** Post-`StackStoreDetect` IR graph (raw `Store`, `StackStore`, `Call`, `CallOther` nodes).

**Heuristics:**
1. **Stack partition:** All `StackStore` / `StackStorePhi` nodes → partition ID `STACK`.
2. **Heap partition(s):**
   - Raw `Store(RAM, ...)` at non-stack address → partition `HEAP` (or sub-partition by region/taint class if refined).
   - `Call` clobbering → partition `HEAP_CLOBBERED` (unless CC says `no_memory_clobber`).
3. **ROM partition:** `Load(CONST, ...)` or `Load(RAM, addr_in_readonly_section)` → partition `ROM` (read-only, no defs).
4. **MMIO partition:** Address in known I/O range → partition `MMIO` (if target architecture defines I/O space).
5. **Unknown partition:** `Store` with unknown address → partition `UNKNOWN` (conservative; aliases everything).

**Soundness:** Over-approximation is safe: if unsure whether two ops alias, assign them to the same partition (serializes them, which is sound but loses optimization opportunity).

### Migration Path and Subsumption

1. **`StackStoreDetect` → `PartitionDiscovery`:**
   - Same detection logic (SP-relative address decomposition).
   - Instead of emitting `StackStore`, emit generic `Store` nodes annotated with partition ID.
   - Or: keep `StackStore` nodes but tag them with partition IDs.

2. **`StackLoadForward` → Partition-aware load forwarding:**
   - Same probe/realize algorithm.
   - Checks `MemUse(P)` for partition P instead of walking raw memory chain.
   - Faster and cleaner (partition assignment is explicit, not implicit in address decomposition).

3. **`CallStackArgCollect` → unchanged or simplified:**
   - Still walks the `STACK` partition chain backward from a `Call`.
   - Simpler: no need for `decompose_sp` to distinguish stack from heap; partition membership is explicit.

4. **`FunctionArgDetect` → unchanged:**
   - Walks the memory chain of the stack partition to detect args.
   - No conceptual change; just uses partition membership instead of address-space inference.

5. **`LoadReadOnly` → partition-filtered:**
   - Only applies to `ROM` partition (or read-only flag).
   - Passes become more composable (partition-specific rather than space-specific).

### Proposed Concrete Signatures

```rust
/// MemDef: models a write to a memory partition.
/// Inputs:  [prior_mem_in_partition, data_or_empty]
/// Outputs: [MemPartition(partition_id)]
MemDef { partition: MemPartitionId }

/// MemPartitionPhi: SSA phi for a single partition at a CFG merge.
/// Inputs:  [phi_token, state_0, state_1, ...]
/// Outputs: [MemPartition(partition_id)]
MemPartitionPhi { partition: MemPartitionId }

/// MemUnion: joins all partition states at a CFG merge into a unified memory token.
/// Inputs:  [phi_token, partition_state_0, partition_state_1, ...]
/// Outputs: [Memory]  (the unified "full memory" edge)
MemUnion

/// Partition assignment side-table on Graph.
/// Per-operation, stores: (node_id → MemPartitionId).
/// Used by pattern queries and optimization passes to filter by partition.
pub struct MemoryPartitionTable {
    node_partitions: DenseMap<NodeId, MemPartitionId>,
    /// Per-partition info (e.g., name, alias class, read-only flag).
    partitions: Vec<PartitionInfo>,
}

pub struct PartitionInfo {
    id: MemPartitionId,
    name: String,
    read_only: bool,
    alias_class: AliasClass,  // STACK, HEAP, ROM, MMIO, UNKNOWN
}
```

### Example: Rewritten Store-to-Load Forwarding

**Before (current):**
```
Load(addr=sp+8) depends on:
  StackStore{offset:8} → StackStore{offset:0} → InitialMemory
  Walk backward, skip disjoint StackStores, bottom out at InitialMemory.
```

**After (Memory SSA):**
```
Load(addr=sp+8) depends on:
  MemUse(STACK) inputs[0] → MemPartitionPhi(STACK) → [MemDef(STACK, offset:8), MemDef(STACK, offset:0), InitialMemory(STACK)]
  No address decomposition needed; partition membership is explicit.
  MemPartitionPhi aggregates per-predecessor defs; forward to the matching def at offset 8.
```

---

## 6. Subsumption Map: Existing Passes Under Memory SSA

| Pass | Status | Reasoning |
|------|--------|-----------|
| `StackStoreDetect` | **Subsumed by `PartitionDiscovery`** | Both identify SP-relative stores; Memory SSA makes partition membership explicit instead of embedding in node kind. |
| `StackLoadForward` | **Survives, refactored** | Algorithm unchanged; now walks `MemPartitionPhi(STACK)` chain instead of raw memory + address decomposition. ~30% code reduction (no `decompose_sp` calls). |
| `CallStackArgCollect` | **Survives, simplified** | Same functionality; partition membership removes ambiguity in the chain walk. ~15% code reduction. |
| `FunctionArgDetect` | **Survives, unchanged** | No-shadow check still walks the stack partition; partition tagging may simplify the DFS. ~5% code reduction. |
| `LoadReadOnly` | **Survives, partitioned** | Only matches `ROM` partition; may extend to multiple ROM partitions per space (CONST, `.rodata`, etc.). ~10% code reduction. |
| `Call` / `CallOther` memory clobbering | **Refactored** | Per-partition clobber lists replace monolithic memory token; Call now outputs `MemDef` per clobbered partition. Requires ABI table updates. |
| `RedundantPhis` | **Refactored** | Collapses `MemPartitionPhi` with single predecessor (per partition). May merge with general phi elimination. |
| Dead-code elimination | **Enhanced** | Partition membership enables fine-grained reachability: an unreachable partition def doesn't kill the entire memory chain. |

---

## 7. Risks and Open Questions

### 7.1 Unknown-Bound Pointers (Binary Analysis Problem)

**Issue:** Strider lifts binary code with only static analysis; many stores have unknown addresses at lift time.

**Example:**
```c
void *p = ...;  // pointer value unknown
*p = data;       // address unknown; may alias heap, stack, globals, MMIO
```

**Current solution:** Conservative `Store(RAM, ...)` with unknown address; all passes treat it as potentially aliasing everything.

**Memory SSA challenge:** Can we partition a store with an unknown address? Conservative answer: assign to `UNKNOWN` partition, which aliases all others. This loses the opportunity to prove disjointness with `STACK` or known-address heap regions.

**Future refinement:** Taint-tracking or value-range analysis at partition-discovery time could assign unknown-address stores to a "best-guess" partition (e.g., if the pointer is stack-allocated, likely stack; if heap-allocated, likely heap). Unsound in general but practical for most binaries.

### 7.2 Segment-Relative Addressing (ISA-Specific)

**Issue:** x86 (pre-64-bit) and some embedded architectures use segmented addressing: `segment:offset` pairs mapped to flat addresses via descriptor tables (GDT/LDT).

**Current support:** `SegmentOp` p-code node handles this as a pure computation (`[segment, offset] → [flat_addr]`). Strider treats it as a value operation, not a memory-space issue.

**Memory SSA exposure:** None expected. `SegmentOp` outputs feed into `Load`/`Store` addresses; partition discovery happens after address computation is folded. Phew.

### 7.3 Indirect Calls and Call-Graph Uncertainty

**Issue:** Binary code may have unresolved indirect calls (computed jump tables, vtables, PLT stubs). The exact callee and its ABI (clobber list, memory edge) are unknown.

**Current approach:** `IndirectBranch` placeholder; post-pipeline resolver tries to classify the target. On failure, the `IndirectBranch` survives (IR is still valid, just incomplete).

**Memory SSA challenge:** If an indirect call's ABI is unknown, which partitions does it clobber? Conservative: all partitions except ROM. This is sound but pessimistic (may prevent forwarding through the call).

**Mitigation:** Partition-discovery pass could mark partitions as "clobbered by unknown calls" separately from "definitely clobbered by known calls", allowing passes to apply different heuristics.

### 7.4 Loop-Carried Memory Dependencies

**Issue:** Loop-header MemPhi may feed a back-edge where a store in the loop clobbers that partition.

**Current model:** `MemPhi` feeds memory chain; `StackLoadForward` uses a cycle guard (`visited` set) to detect loops and bail out rather than infinite-loop during probe.

**Memory SSA exposure:** `MemPartitionPhi` with a back-edge to a partition it feeds—the same cycle structure. Cycle guards still apply. No new risk.

### 7.5 Multi-Byte Atomics and Volatile Access

**Issue:** Some architectures support atomic stores (e.g., x86 LOCK prefix, ARM STXR). Memory model should respect atomicity barriers.

**Current support:** Sleigh (or target-specific p-code) emits `CallOther` for atomic ops (e.g., `LOCK_CAS`). The lifter treats them as opaque memory-clobbering operations.

**Memory SSA exposure:** `CallOther` with memory edge → `MemDef` for clobbered partitions. No problem.

### 7.6 Alias Analysis Soundness

**Question:** How confident are we in partition assignments? What happens if two ops appear disjoint but actually alias?

**Answer:** Unsoundness breaks optimization (wrong answer), not safety (crash). Conservative over-approximation (assigning two ops to the same partition when unsure) is always safe, just less optimized. The heuristics in Section 5 should err on the side of over-approximation; future refinement can tighten if needed.

---

## 8. Implementation Priorities and Staging

### Phase 1: Foundations
1. Add `MemDef` / `MemPartitionPhi` / `MemUnion` node kinds.
2. Implement partition-discovery pass (scan graph, assign partition IDs, annotate Graph).
3. Add `Graph::memory_partitions()` side-table.

### Phase 2: Migration
4. Rewrite `StackStoreDetect` → partition discovery.
5. Rewrite `StackLoadForward` to use partition membership.
6. Simplify `CallStackArgCollect` for partitioned stack.

### Phase 3: Cleanup
7. Sunset `StackStore` / `StackStorePhi` node kinds (or leave as legacy for compatibility).
8. Extend ABI tables to specify per-partition clobbering.
9. Refactor `Call` to emit `MemDef` per partition instead of monolithic memory token.

### Phase 4: Optimization
10. Partition-aware dead-code elimination.
11. Refine alias analysis (e.g., taint-based partition assignment).
12. Fine-grain forwarding queries across partition boundaries (if safe by ABI).

---

## Conclusion

Strider's current memory model is pragmatic: `MemPhi` provides standard SSA renaming, `StackStore` / `StackStorePhi` carve out the stack frame as a separate partition, and a suite of passes (`StackLoadForward`, `CallStackArgCollect`, `FunctionArgDetect`) leverage that partition to eliminate redundancy and reconstruct high-level intent.

An LLVM-style Memory SSA redesign would:
- **Unify** stack and heap partitioning under a common abstraction (`MemPartitionId`).
- **Clarify** aliasing reasoning (explicit partition membership instead of address decomposition heuristics).
- **Enable** future extensions (I/O spaces, taint partitions, fine-grain alias classes).
- **Simplify** the optimizer (partition chain walks replace address-space inference).

The migration is staged and low-risk: existing passes survive with mechanical refactoring, new infrastructure (partition discovery) is localized, and the IR remains valid throughout. The main architectural win is decoupling memory reasoning from address computation—a cleaner abstraction boundary for binary-analysis tools where address unknowns are frequent.

