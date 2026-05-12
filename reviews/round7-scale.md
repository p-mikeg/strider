# Round 7 — Scalability Audit (thousands of IR nodes)

Trust model: did not read `*-r6.md` files. Read `round7-*.md` outputs only as starting context. All findings re-derived by source inspection.

Target scale: ~10k post-lift IR nodes, ~hundreds of regions, ~dozens of indirect branches, ~hundreds of stack stores. Default thread stack 8 MB.

---

## A. Recursion-induced stack-overflow risk

| # | Site (file:line) | Cycle / depth bound | Worst-case depth (vs N) | Severity |
|---|------------------|---------------------|--------------------------|----------|
| A1 | `crates/opt/src/stack_load_forward/mod.rs:489` `find_stack_stored_value_at_offset` | `walk_memo` cache (key = `(NodeOutputId, i64, NodeOutputType)`) prevents revisits. **No depth bound.** | O(memory-chain length) — proportional to the count of `StackStore`/`Store` nodes on a memory predecessor chain. With hundreds of stack stores in one region the chain is hundreds deep; with the orchestrator iterating per-index for stack-array dispatch it stays bounded but unbounded in pathological IR. | **HIGH** — confirmed pre-existing. Convert to iterative worklist; the `realize`-style two-phase model in `probe` (line 182) is the template. |
| A2 | `crates/opt/src/sp_expr.rs:247` `decompose_sp` (recursive via `decompose_sp_inner`/`decompose_sp_phi`) | `visiting: FxHashSet<NodeId>` cycle guard + memo. **No explicit depth bound.** | O(sp-arithmetic chain length). Classic shape: `(((sp-4)-8)-12)-…` after a long alloca / spill prologue. Bounded in practice by the number of `Add`/`Sub`/`And`/`VarPhi(sp)` nodes, ~dozens. Phi recursion through `decompose_sp_phi` → `decompose_sp` is bounded by the visiting set. | MED — won't realistically hit overflow but does pay function-call overhead per chain link; not critical to fix. |
| A3 | `crates/opt/src/function_args/mod.rs:393` `mem_chain_is_dirty` | `seen: FxHashSet<NodeOutputId>` cycle guard + memo (top-level only — see line 412). **No explicit depth bound.** | O(memory-chain length × MemPhi fan-out). On every `MemPhi` it recurses into every predecessor (line 434–436). For ~hundreds of stack stores plus several MemPhis per loop, depth is hundreds. | **HIGH** — same shape as A1 but worse: MemPhi fan-out spawns one nested call per pred while `seen` is shared, so bookkeeping is sound but depth grows by chain length. Convert to explicit-stack DFS. |
| A4 | `crates/opt/src/stack_load_forward/mod.rs:381` `realize` | None. Recurses through `ResolveShape::Phi { preds }`. | O(MemPhi nesting depth). Bounded by the depth of nested `MemPhi`s which is bounded by CFG region nesting (typically ≤ 20 for real binaries). | LOW — real-world bounded; would only blow the stack on adversarial IR. |
| A5 | `crates/opt/src/indirect_branch_resolve/jump_table.rs:691` `same_value` (inner `root` fn) | `budget = 64usize` + `visited: DenseEntitySet`. | Hard-capped at 64. | NONE. |
| A6 | `crates/opt/src/indirect_branch_resolve/jump_table.rs:407` `walk_control_for_if_bound_iter` | Iterative; explicit `JoinNext` stack. Cycle guard via `visited`. | O(control-chain length). | NONE — already iterative. |
| A7 | `crates/ir/src/walk.rs:26` `cfg_reachable` and `crates/ir/src/walk.rs:123` `walk_graph` | Iterative. `walk_graph` uses `graphwalk::PreOrder` with `DenseEntitySet`. | O(N+E). | NONE — confirmed iterative. |
| A8 | `crates/cfg/src/cfg/builder/mod.rs:190` `Builder::explore` / `region_builder.rs` `build` | Iterative work-queue (verified at `mod.rs:211` — `RegionBuilder::new(...).build()` is called from a loop, not recursively). `find_region_containing_addr` uses `BTreeMap::range` — O(log R). | O(R + I). | NONE. |
| A9 | `crates/strider/src/orchestrator.rs:174` `run` / `LoopState::step` | Iterative; cap `2*pending_at_iter_0 + 4` plus `stall_budget`. | Bounded. | NONE. |
| A10 | `crates/ir/src/graph/compact.rs:67` `retain_reachable` | Iterative passes over `reachable` Vec. | O(N+E). | NONE. |
| A11 | `crates/pattern/src/matcher/mod.rs:610` `match_output_with_walk_through` | Cast walk-through is iterative (line 622 doc + impl). ControlState walk-through (`try_walk_through_control_state`) recurses via `match_output_with_walk_through` per pred. | Pattern depth × CS-nesting depth — bounded by user-written patterns + CFG join-nesting. | LOW — real-world bounded. |
| A12 | `crates/pattern/src/pat/node_pat.rs:519` `match_consumer_node` | Recursive (line 550). Used by `IfPat::true_branch`/`.false_branch` to pierce ControlStates. | CFG ControlState chain length. | LOW. |

**Net new HIGH findings:** A3 `mem_chain_is_dirty`. A1 was already known.

---

## B. Asymptotic complexity in hot paths

| # | Hot path | Code-derived complexity | Notes |
|---|----------|--------------------------|-------|
| B1 | `Matcher::find_all(&pat)` (`crates/pattern/src/matcher/mod.rs:221`) | O(B · M(pat)) where B = bucket size for pat's root discriminant (kind-index hit) **OR** O(N · M(pat)) for wildcard root. M(pat) = match-attempt cost ≈ pattern depth, O(d_pat). | Kind-index lazy build is O(N) once per `Matcher`. Subsequent calls reuse it. |
| B2 | `Matcher::find_all_requirements(&[pat1,…,patM])` (`crates/pattern/src/matcher/mod.rs:435`) | O(N1 · N2 · … · Nm) cross-product where Ni = matches per pattern. Inside each cross-product, `prefix_agrees` (line 676) is O(|prefix| · max(captures per match)). **No early-exit pruning beyond `acc.is_empty()`.** | Doc explicitly warns "worst case unbounded; expected closer to O(N_max)". For 2 patterns each matching 1k nodes it's 1M tuples scanned. **Confirmed pre-existing concern.** |
| B3 | `ir::validate::validate` (`crates/ir/src/validate/mod.rs:75`) | Layer A: O(reachable nodes). Layer B (`crates/ir/src/validate/layer_b.rs`): O(all nodes + total use-list size) = O(N+E). Layer C: `check_layer_c_uniqueness` O(N), `check_layer_c_control_state` O(R + R · max_pred_arity) ≈ O(N+E), `check_layer_c_phis` — at line 147 calls `graph.node_inputs(owner).len()` per phi; O(P · pred_arity). `check_layer_c_function_arg_uniqueness` uses `HashMap`, O(N). | Total O(N+E). Each opt iteration calls `validate` exactly **once** at the very end of `OptimizerPipeline::run` (line 290). |
| B4 | `cfg::Cfg::region_id_at_start` (`crates/cfg/src/cfg/query.rs:140`) | **O(R)** — linear scan of `self.graph.node_indices()` (line 141). | Called inside `handle_switch` (`crates/strider/src/strider/insn/control.rs:172`) **once per switch target**, so handling a switch with T targets in a function with R regions is **O(T·R)**. Per-iteration cost in the tier-2 fixed point: orchestrator rebuilds the CFG, lifts the IR, and calls `handle_switch` once per resolved indirect branch. With dozens of switches × hundreds of regions × multiple iterations, this is hot. **Confirmed already known. Fix: maintain a `start_addr_to_region_id: BTreeMap<MachineInsnAddr, RegionId>` on the `Cfg`** — the builder already keeps this map (`crates/cfg/src/cfg/builder/mod.rs:165`), the public type just needs to expose / persist it. |
| B5 | `ir::Graph::create_node` dedup (`crates/ir/src/graph/store.rs:179`) | O(K) per call where K = fanin + fanout slot count, dominated by HashMap key build (`inputs.to_vec()` and `output_kinds.to_vec()` allocations on every cacheable lookup, lines 185–192). HashMap O(1) avg. | Total construction O(N · avg_K). Two `Vec` allocations per call; `SmallVec<[_;4]>` is materialised into `Vec` for the key (line 192 `inputs.to_vec()`). MED — measurable but linear in graph size. |
| B6 | `opt::OptimizerPipeline::run` (`crates/opt/src/pipeline.rs:265`) | `MAX_ITERS = 1024` outer cap. Per iteration: K passes, each O(N) (worklist-driven for ConstantFold/KnownBits/DeadBranchElimination; O(reachable) for RedundantPhis; O(loads × chain_length) for StackLoadForward). Final `validate` is O(N). **Total worst-case: O(N · K · MAX_ITERS) = O(N · 1024 · K).** | In practice each iteration removes ≥1 node or no change fires. With `Changed`/`NoChange` strict-progress contract the loop converges in O(N) iterations max, but the cap is set generously. The N=10k × K=8 × 1024 worst case is 80M ops/iter — measurable. |
| B7 | `strider::orchestrator` indirect-resolve fixed point (`crates/strider/src/orchestrator.rs:184`) | Cap `2 * pending_at_iter_0 + 4` (line 363). Each iteration: full CFG rebuild + full IR re-lift + stable optimizer pipeline. Stable pipeline = ConstantFold + KnownBits + IfCondInversion + StackStoreDetect + StackLoadForward + FunctionArgDetect post-pass. **Per iteration: O((N + N · 1024) per `Pipeline::run`)** plus the lift cost which is O(I · pcode_per_insn). | With `pending_at_iter_0 = 24`, cap = 52 iterations of full rebuild. For N=10k that's 50× O(N · K · iters_per_pipeline). **HIGH cost path.** Note the `vn_cache` already amortises `find_all_unique_vns` via `vn_cache_region_count` (line 814–823) — that's good. |
| B8 | `Matcher::find_all_multi(&pats)` (`crates/pattern/src/matcher/mod.rs:282`) | One preorder walk built once (cached), then O(sum_i bucket_size_i · M(pat_i)) for kind-discriminant patterns + O(W · N) where W = wildcard pat count. | O(N) preorder cost is paid once per `Matcher`. Confirmed shared. |
| B9 | `ir::walk::walk_graph` (`crates/ir/src/walk.rs:123`) | O(N+E). `graphwalk::PreOrder` with `DenseEntitySet` tracker. Each node visited once; each edge traversed once via `graph_walk_succs`. | Confirmed. |
| B10 | `opt::sp_expr::decompose_sp` (`crates/opt/src/sp_expr.rs:247`) | O(chain length) on miss; O(1) on memo hit. | Memo is keyed on `NodeOutputId` and threaded through every caller. |
| B11 | `apply_in_place_edit` → `build_anchor_calling_context` (`crates/strider/src/orchestrator.rs:685`) | **Builds `initial_var_index: HashMap<rsleigh::Vn, NodeOutputId>` by iterating `graph.graph.all_node_ids()` — O(N) per call.** The function is called inside the `for (placeholder, resolved) in in_place_edits` loop (line 523), so total cost per iteration is **O(K · N)** where K = number of in-place edits. | **MED–HIGH** — a function with dozens of indirect tail calls + LR returns scales as O(K·N) per orchestrator iteration. Fix: hoist `initial_var_index` build to `apply_in_place_edits` (line 512) and pass it down, **or** build it once per iteration outside the in-place-edit loop. |
| B12 | `cfg::Cfg::regions().collect::<Vec<&Region>>()` then iterate `wrapped.insn.all_vns()` (`crates/strider/src/orchestrator.rs:815–822`) | O(I) per CFG rebuild — bounded above by total instruction count. Already amortised: only new regions (`skip(*vn_cache_region_count)`) are scanned. | OK. |
| B13 | `Matcher`'s `kind_index`/`preorder` lazy caches (`crates/pattern/src/matcher/mod.rs:115–138`) | O(N) once per `Matcher`. | OK. |

---

## C. Memory growth

| # | Storage | Per-graph cost | Lifecycle | Concern |
|---|---------|----------------|-----------|---------|
| C1 | `ir::Graph.nodes: PrimaryMap<NodeId, Node>` | O(N) | Persists; never shrinks until `retain_reachable`/`compact`. | OK. |
| C2 | `ir::Graph.inputs: PrimaryMap<NodeInputId, NodeInput>` | O(E) — sum of fanin | Same as nodes. | OK. |
| C3 | `ir::Graph.outputs: PrimaryMap<NodeOutputId, NodeOutput>` | O(O) — sum of output slots ≈ O(N) | Same. | OK. |
| C4 | `ir::Graph.node_to_id: HashMap<(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>), NodeId>` (`crates/ir/src/graph/mod.rs:52`) | O(N · avg_K) where K = fanin + fanout. **Each entry stores TWO heap `Vec`s.** | Persists; entries evicted on rewrite (via `evict_cache_entry_if_cacheable` in `store.rs:241`). | MED — each cacheable node carries two extra `Vec` allocations in the dedup map. Dominates Graph memory for trivially-cacheable graphs. |
| C5 | `ir::Graph.asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>` | O(N + sum of contributing addrs) | Persists. After absorptions (e.g. RedundantPhis line 105 absorbs into surviving value), entries grow. | LOW — sorted-deduped lists are small (≤ a few addresses per node). |
| C6 | `ir::Graph.stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>` | O(P · avg pred-arity) where P = phi count | Persists. | OK. |
| C7 | `ir::Graph.call_other_names: SecondaryMap<NodeId, Option<String>>` | O(N) array (mostly `None`); `String` only for CallOther nodes. | OK. |
| C8 | `ir::Graph.call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>` | O(N) array; `Some` only on per-address-CC overrides. | OK. |
| C9 | **Zombie node pollution** | `RedundantPhis::detach_unreachable_nodes` and `DeadBranchElimination` detach inputs but leave nodes in `nodes` arena. Side-tables retain entries. | `crates/strider/src/orchestrator.rs:452` calls `graph.compact()` only **once** at `finalize` (when `compact: true`). Inside the fixed-point loop the IR `Graph` keeps growing with zombies across CFG rebuilds. | **HIGH** — for the orchestrator's tier-2 loop, every rebuild creates a NEW `BuiltFunctionGraph` (via `build_lift_stable`, line 771), so the *previous* graph's zombies are dropped at `Drop`. The new one accumulates only this iteration's zombies. So zombie growth is per-iteration, not per-run. **OK in current shape**, but: if `Strider::analyze_cfg` builds the graph and doesn't compact, downstream callers running multiple optimisations get zombies until they explicitly `compact`. Pattern queries already skip zombies via `walk_graph`. Document this clearly; the implication for embeddings (Python) is non-obvious. |
| C10 | `pattern::Match` / `Bindings` | `Bindings` uses a journal (BTreeMap-style) keyed on `Capture`; per-Match cost = O(captures per pattern). Across `find_all` → M matches × C captures each = O(M·C) total. | Per-call. | OK. |
| C11 | `pattern::Matcher` lazy caches: `preorder: OnceCell<Vec<NodeId>>` and `kind_index: OnceCell<FxHashMap<Discriminant, Vec<NodeId>>>` | O(N) preorder + O(N) kind buckets | Persists for the `Matcher`'s lifetime. | OK — bounded by N. |
| C12 | `cfg::Cfg.graph: petgraph::StableDiGraph<Region, RegionEdgeKind>` + per-region `insns: Vec<RegionInstruction>` | O(R + E_cfg + I) | Persists. **`StableDiGraph` keeps removed-region slots as tombstones** — but `cfg::Builder` only ADDs regions, never removes (split is the only mutation). | OK. |
| C13 | `opt::known_bits::analyze` `known: FxHashMap<NodeOutputId, Kb>` | O(N · 16 bytes) | Per-call. | OK. |
| C14 | `opt::sp_expr::SpExprMemo` | O(chains touched · 32 bytes) | Per-pass-call (e.g. `StackStoreDetect::optimize_built` line 114). | OK. |
| C15 | `strider::LoopState.known_targets` (`crates/strider/src/orchestrator.rs:222`) | Monotonically grows; one entry per resolved indirect branch. | Per-run; resets each call to `run`. Bounded by number of indirect branches. | OK. |
| C16 | `strider::LoopState.vn_cache` (`crates/strider/src/orchestrator.rs:265`) | Bounded by distinct varnodes (≤ register file × ~4 widths ≈ ~hundreds). | Per-run. | OK. |
| C17 | `strider::LoopState.region_handles` / `region_index.by_exit_control` | Refreshed each iteration via `RegionIndex::from_handles`; replaces the previous index. | Per-iteration. | OK. |
| C18 | `cfg::DecodeCache` | One Arc-wrapped `LiftRes` per machine address, shared across CFG rebuilds in one `run`. | Per-run. | OK. |

---

## D. Fixed-point convergence proofs (sketch)

### D1. `opt::OptimizerPipeline::run` (`crates/opt/src/pipeline.rs:265`)

**Cap:** `MAX_ITERS = 1024`.

**Monotone metric per iteration:** `OptimizationResult::Changed` is returned by a pass iff it called `replace_all_uses` (which decrements someone's use-count) or `detach_node_inputs` (which monotonically removes edges). Both are strictly progress on a finite resource (edge count, dedup-cache entries, or node-output-use-count). Termination is by edge-count exhaustion: each productive pass at least breaks one use → finite use-count → finite iterations.

**Risk:** A pass that "changes" without making forward progress would diverge. None observed; ConstantFold guards against re-fold via dedup cache (rebuilt-equal IntConst returns the existing NodeId, so `replace_all_uses(out, out)` returns `false → NoChange`). KnownBits same. RedundantPhis only `simplifies` when a strict reduction is provable.

**Verdict:** Tight. The 1024 cap is a **safety net for a regression**, not the convergence bound. With N=10k nodes the pipeline should converge in ≪ 100 iterations.

### D2. `strider::orchestrator::run` (`crates/strider/src/orchestrator.rs:174`)

**Cap:** `2 * pending_at_iter_0 + 4` (line 363).

**Monotone metric:** Each `Decision::Rebuild` strictly grows the induced edge set of `known_targets` (`edge_set_of`, line 851). Each `Decision::StableOnly` strictly reduces `unresolved.len()` (or hits the `stall_budget`).

**Stall-budget protection:** `stall_budget` decrements on each in-place-only iteration whose `unresolved_after_edits.len() >= prev_unresolved_len` — protects against a misclassifying-resolver spin (line 396–404).

**Risk:** A `Multiple` resolution that adds K targets where K > pending grows the edge set sub-linearly w.r.t. iterations; the cap is conservative enough.

**Verdict:** Tight. Two independent termination guarantees (edge-set monotonicity for Rebuild, stall-budget for in-place stagnation) plus cap.

### D3. `cfg::Builder` worklist (`crates/cfg/src/cfg/builder/mod.rs:190`)

**Cap:** None explicit.

**Monotone metric:** `find_region_containing_addr` checks if `addr` is already covered; if yes, just adds an edge (no new region). New regions are created only at fresh start addresses. The address space is bounded by `[start_addr, start_addr + fn_max_size)` (or u64 max), so the work-queue terminates when every reachable pcode address has been visited.

**Verdict:** Bounded by reachable pcode-instruction count. Tight.

---

## E. Benchmark / profiling recommendations

### Synthetic harness

Create `crates/strider/benches/scale.rs` (Criterion or `#[bench]`) with:

1. **Chain-of-stores benchmark** — generate a synthetic single-region IR with N stack stores at distinct offsets followed by N matching loads. Drive `StackLoadForward` end-to-end. Targets: N ∈ {1k, 5k, 10k}. Measure `try_forward_load` total time and peak stack depth (use `cargo flamegraph` or `psm::stack_pointer`).

2. **Deep diamond CFG** — generate D nested if/else regions producing a CFG with ~D regions, ~D control-states, ~D varphis, no instructions to lift (use `FunctionBuilder::build_if`). Run `default_pipeline()`. Targets: D ∈ {100, 500, 1000}. Measures: `RedundantPhis` per-iteration cost, `OptimizerPipeline::run` iteration count, `validate` time.

3. **Wide jump-table dispatch** — generate one switch with T targets within an otherwise-trivial function. Run `strider::run`. Targets: T ∈ {16, 64, 256}. Measures: tier-2 fixed-point iteration count, `region_id_at_start` cumulative time (B4).

4. **Cross-product join** — generate N IntConst nodes then run `find_all_requirements(&[any_int_const, any_int_const])` (no shared captures). Targets: N ∈ {100, 1000}. Pins B2 worst-case.

### Top-3 hot spots to profile first

1. **`cfg::Cfg::region_id_at_start` (B4)** — already-known O(R) per call inside `handle_switch`; switching a function with T switches and R regions costs O(T·R) per CFG rebuild × `cap` orchestrator iterations. Trivial fix: persist the existing `start_addr_to_region_id` BTreeMap from the builder onto the `Cfg`.
2. **`mem_chain_is_dirty` (A3) and `find_stack_stored_value_at_offset` (A1)** — both recurse through long memory chains without depth bounds. Convert to iterative DFS with explicit stack. Same shape change as `walk_control_for_if_bound_iter` already uses (`jump_table.rs:407`).
3. **`build_anchor_calling_context::initial_var_index` rebuild (B11)** — O(N) per in-place edit × K edits per orchestrator iteration. Hoist the index build to `apply_in_place_edits` and pass the prebuilt map down.

### Secondary optimisation candidates

- `ir::Graph::create_node` dedup-cache key allocation (B5, C4) — replace `(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>)` HashMap key with a hashed-input fingerprint stored on the side; avoids two `Vec` clones per cacheable node.
- `Matcher::find_all_requirements` (B2) — add early-exit when no shared captures exist between any two patterns (degenerates to plain cartesian product, which is the user's bug to avoid). Document explicit warning when a cross-product would exceed e.g. 1M tuples.

---

## Summary of new HIGH findings
- **A3** `mem_chain_is_dirty` recursive without depth bound (`function_args/mod.rs:393`).
- **B11** `build_anchor_calling_context` rebuilds `initial_var_index` per in-place edit (`orchestrator.rs:685`).
- **B7** Tier-2 orchestrator full CFG-rebuild + IR-relift per iteration is expensive (`orchestrator.rs:184`); not a bug but the dominant cost.

Confirmed pre-existing HIGH (from prior audits): A1, B4 (`region_id_at_start`), B2 (`find_all_requirements` cross-product).
