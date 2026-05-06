# Round 7 — Generalization Opportunities

Audit of repeated logic across crates that could be lifted into shared utilities while preserving correctness. Each section cites file:line for every duplication, summarises what's shared vs different, proposes an API, and ranks risk.

The user's seed example — "many places walk the graph; lift it into `walk.rs`" — is category 1 below. Several other categories are equally tractable.

---

## 1. Graph walks (the user's seed example)

The IR already has good primitives in `crates/ir/src/walk.rs` (`walk_graph`, `cfg_reachable`) and `crates/graphwalk/src/lib.rs` (generic `PreOrder` / `PostOrder`). The duplication is **not** at the walker level — that's already shared. The duplication is at the **kind-filter + collect-into-Vec for mutation-safe iteration** level.

### Sites where the pattern repeats

| File:line | Pattern |
|---|---|
| `crates/opt/src/constant_fold/mod.rs:64` | `WorkSet::seeded(function.preorder())` then drain |
| `crates/opt/src/dead_branch/mod.rs:264` | `WorkSet::seeded(function.preorder())` then drain |
| `crates/opt/src/known_bits/mod.rs:333,370` | `WorkSet::seeded(function.preorder())` (twice — analyze + rewrite) |
| `crates/opt/src/redundant_phis/mod.rs:188-198` | `function.preorder().filter(matches!(...VarPhi/MemPhi/ControlState)).collect::<Vec<_>>()` |
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:77` | `function.graph.preorder(function.entry).collect::<Vec<NodeId>>()` |
| `crates/opt/src/if_cond_inversion/mod.rs:64-67` | `preorder_kind(NodeKind::If).filter(is_inverted_cond).collect()` |
| `crates/opt/src/load_readonly/mod.rs:54-56` | `preorder_kind(NodeKind::Load(_)).collect::<Vec<_>>()` |
| `crates/opt/src/stack_store/detect.rs:111-113` | `preorder_kind(NodeKind::Store(_)).collect::<Vec<_>>()` |
| `crates/opt/src/stack_store/call_args.rs:303-306` | `preorder().filter(NodeKind::Call).collect::<Vec<_>>()` |
| `crates/opt/src/stack_load_forward/mod.rs:61-63` | `preorder_kind(NodeKind::Load(_)).collect::<Vec<_>>()` |
| `crates/opt/src/function_args/mod.rs:145,233` | two more `preorder()` / `preorder_kind` collects |
| `crates/opt/src/indirect_branch_resolve/mod.rs:415-417` | `preorder().find(matches!(IndirectBranch))` |
| `crates/opt/src/redundant_phis/mod.rs:184` | `cfg_reachable(...)` |
| `crates/strider/src/rewrite.rs:124` | `self.graph.preorder(self.entry).collect::<Vec<NodeId>>()` |
| `crates/strider/src/orchestrator.rs:685-691` | `for nid in graph.graph.all_node_ids()` building `vn → InitialVar.output` lookup |
| `crates/opt/src/indirect_branch_resolve/inplace.rs:203,244` | preorder + all_node_ids scans |
| `crates/strider/src/strider/insn/control.rs:387,394,406` | three test helpers building preorder + filter (`count_if_nodes`, `count_eq_cmps`, `count_int_consts_eq`) |

### Same vs different

* **Same.** Order = reachable preorder; visited tracker = `DenseEntitySet`; mutation-safe iteration via `.collect()` first; all callers that mutate then re-enqueue consumers (`ConstantFold`, `KnownBits`) re-implement the same `for out: outputs { for (consumer,_): output_uses { stash }}` snapshot dance.
* **Different.** (a) Kind-filter (some pass concrete kinds, some pass `Any`); (b) whether the body re-enqueues consumers (mutation-aware passes do, passes like `flag_cmp_canonicalize` don't); (c) seed wrapper (`WorkSet::seeded` vs raw `Vec<NodeId>`).

### Proposed API

```rust
// crates/ir/src/walk.rs — add to existing module
pub fn collect_kind<'a>(
    graph: &'a BuiltFunctionGraph,
    pred: impl FnMut(&NodeKind) -> bool,
) -> Vec<NodeId> { /* preorder_kind + collect */ }

// crates/opt/src/worklist.rs — add a kind-aware seed
impl WorkSet {
    pub fn seeded_kind(
        graph: &BuiltFunctionGraph,
        pred: impl FnMut(&NodeKind) -> bool,
    ) -> Self { … }
}

// New: a "rewrite reachable nodes, snapshotting consumers" driver
pub fn drive_with_consumer_reenqueue(
    graph: &mut BuiltFunctionGraph,
    seed: impl IntoIterator<Item = NodeId>,
    mut visit: impl FnMut(&mut BuiltFunctionGraph, NodeId) -> Result<OptimizationResult>,
) -> Result<OptimizationResult> { … }
```

The `drive_with_consumer_reenqueue` driver consolidates the `ConstantFold` (mod.rs:74-93) and `KnownBits` (mod.rs:377-417) bodies — their `consumers.clear(); for out: outputs { for cons in output_uses { consumers.push } }` snapshot then `if r.changed() { for c in consumers { work.push(c) } }` is identical except for which rule(s) the body invokes.

### Estimated LOC removable

~120 LOC across opt passes. The bigger win is the **uniform mental model** for new passes — every author currently re-derives the snapshot dance.

### Risk

LOW for `collect_kind` and `seeded_kind` (mechanical, type-driven). MED for `drive_with_consumer_reenqueue` (need to preserve the SmallVec<[NodeId;8]> heap-allocation hint that ConstantFold and KnownBits both invested in).

---

## 2. Memo / fixed-point caches

Every SP-aware pass threads its own `SpExprMemo`-typed `FxHashMap`. Beyond `decompose_sp`'s memo, several other compute-or-cache patterns repeat.

### Sites

| File:line | Memo |
|---|---|
| `crates/opt/src/sp_expr.rs:241` | `pub type SpExprMemo = FxHashMap<NodeOutputId, Option<SpExpr>>` |
| `crates/opt/src/known_bits/mod.rs:332` | `let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();` |
| `crates/opt/src/stack_load_forward/mod.rs:461-462` | `pub type StackStoredValueMemo = FxHashMap<(NodeOutputId, i64, NodeOutputType), Option<NodeOutputId>>` |
| `crates/opt/src/function_args/mod.rs:360` | `type ShadowMemo = FxHashMap<(NodeOutputId, i64, i64), bool>` |
| `crates/opt/src/function_args/mod.rs:393-417` | `mem_chain_is_dirty` does its own outermost-only caching (`is_outermost = seen.is_empty()`) — the same dance `decompose_sp` does at sp_expr.rs:267-273 |
| `crates/strider/src/orchestrator.rs:142-154` | `RegionIndex` (effectively a memo from `exit_control` → region info, rebuilt every iteration) |

### Same vs different

* **Same.** Compute-or-cache: lookup → return cached, else compute → insert. `seen` cycle-guard set threaded alongside. None-vs-Some asymmetric caching for cycle-truncation soundness.
* **Different.** Key types vary; cycle-handling rule is identical though (don't cache `None` at the cycle break point — both `decompose_sp` and `mem_chain_is_dirty` reinvent this).

### Proposed API

A new `entity_utils::Memo<K, V>` wouldn't capture the cycle-guard. The right primitive is a **DFS context** type:

```rust
// crates/entity-utils/src/dfs.rs (new) — or opt::sp_expr (closer to home)
pub struct DfsCtx<K: Eq + Hash, V: Clone> {
    pub memo: FxHashMap<K, V>,
    pub visiting: FxHashSet<K>,  // cycle guard
}
impl<K, V> DfsCtx<K, V> {
    /// Compute-or-cache with cycle-aware "don't cache None" semantics.
    pub fn compute<F>(&mut self, key: K, compute: F) -> V
    where
        F: FnOnce(&mut Self) -> V,
        V: Default,
    { … }
}
```

### Estimated LOC removable

~60 LOC (cache lookup + insert boilerplate at four sites).

### Risk

MED — `decompose_sp`'s "cache `Some(_)` only" semantics is load-bearing for soundness; the abstraction must preserve it without leaking the rule into every caller. A naïve `Memo::get_or_compute` would silently break the cycle case.

---

## 3. Worklist / fixed-point loops

Three independent fixed-point drivers exist; only one uses `entity_utils::Worklist`.

### Sites

| File:line | Loop |
|---|---|
| `crates/opt/src/pipeline.rs:265-292` | `OptimizerPipeline::run` — `MAX_ITERS=1024` cap, change-flag fixed-point |
| `crates/strider/src/orchestrator.rs:174-192` | `run` — cap = `2 * pending_at_iter_0 + 4`, `LoopState::step → Decision`, separate `stall_budget` |
| `crates/cfg/src/cfg/builder/mod.rs:248-249` | `work_queue.pop()` while-let — region-discovery worklist (no cap, terminates by exhaustion) |
| `crates/opt/src/stack_load_forward/mod.rs:208,287` | nested `'outer: loop` + inner loop driving the iterative phi-frame walk |
| `crates/opt/src/stack_store/call_args.rs:112` | `loop { … break }` driving stack-arg collection |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:439,441,530` | three nested loops driving target enumeration |
| `crates/entity-utils/src/worklist.rs` | the existing generic `Worklist<E>` (used only by `graphwalk`) |

### Same vs different

* The optimizer-pipeline and orchestrator loops both follow the shape "step → returns enum decision → cap-bounded" but they don't share scaffolding. The orchestrator additionally tracks a stall budget; the optimizer pipeline does not.
* The cfg builder's `work_queue: Vec<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>` (cfg/builder/mod.rs:78) is a textbook drainable queue — could be `entity_utils::Worklist` if `PcodeInsnAddr` had `EntityRef`. The dedup that `Worklist` provides would be a behavioural change, however (the cfg builder relies on the `(parent_region, address)` tuple, and dedup must be by `address` only).

### Proposed API

A `FixedPointDriver` trait could replace both convergence loops:

```rust
trait FixedPointStep {
    type Decision;
    fn step(&mut self) -> Result<Self::Decision>;
    fn done(d: &Self::Decision) -> bool;
    fn stalled(d: &Self::Decision) -> bool { false }
}

fn drive<S: FixedPointStep>(s: &mut S, cap: usize, stall: usize) -> Result<()> { … }
```

### Estimated LOC removable

~30 LOC (the cap + iters scaffolding at two sites). The stall logic is bespoke enough that it's hard to share without losing its specificity.

### Risk

HIGH — both loops have hard-won correctness invariants (the orchestrator's `2 * pending + 4` bound is a published bound on legal classification transitions; the optimizer's MAX_ITERS=1024 has no analogue in the orchestrator). Unifying the cap policy could regress on either side.

**Recommendation: don't generalise.** The two loops genuinely differ; making them look the same would be visual noise.

---

## 4. Asm-fingerprint extension boilerplate

Every opt pass that creates a node from existing nodes manually calls `graph.extend_asm_fingerprint_from(new_node, contributor)` afterward. The lift-time path centralises this in `FunctionBuilder::create_node` (ir/src/builder/mod.rs:393-403); post-lift opt has no equivalent.

### Sites (all are post-lift, all are manual)

| File:line | Pattern |
|---|---|
| `crates/opt/src/pipeline.rs:54` | `OptimizationResult::after_replace` already encapsulates one variant of this (replace_all_uses + extend_asm_fingerprint_from) |
| `crates/opt/src/constant_fold/mod.rs:48` | manual after `make_*` |
| `crates/opt/src/dead_branch/mod.rs:115` | manual after a control rewrite (special-case: extends *into* a ControlState) |
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:155,171` | every `build_int_cmp` / `build_bool_neg` helper does the extend manually |
| `crates/opt/src/function_args/mod.rs:346` | manual after Truncate creation |
| `crates/opt/src/indirect_branch_resolve/inplace.rs:145,162,175` | three manual extends after splicing |
| `crates/opt/src/known_bits/mod.rs:410` | manual after make_int_const |
| `crates/opt/src/redundant_phis/mod.rs:105,116,146` | three manual extends in three different `remove_phi*` arms |
| `crates/opt/src/stack_load_forward/mod.rs:125` | manual after realize() |
| `crates/opt/src/stack_store/detect.rs:45,68` | manual after StackStore / StackStorePhi creation |

### What's shared

The contract is uniform: when pass `P` rewrites node `A` into `A'`, `A'`'s fingerprint must be a superset of `A`'s. Every site implements this with the same one-liner `graph.extend_asm_fingerprint_from(new, old)`.

### What's missing

There is **no compile-time enforcement** — a forgotten extend silently violates the superset-only contract. An audit is the only way to catch one. The Layer-C `check_asm_fingerprints` validator (ir/src/validate/mod.rs:106) is opt-in (`ValidateOptions { check_asm_fingerprints: true }`) and isn't wired in for production runs.

### Proposed API

Add a single helper on `Graph`:

```rust
impl Graph {
    /// Like `create_node`, but also unions the fingerprints of every
    /// node in `attribution_sources` into the new node.  Replaces the
    /// `create_node + extend_asm_fingerprint_from` pair at every
    /// post-lift call-site.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        outputs: impl IntoIterator<Item = NodeOutputKind>,
        attribution_sources: &[NodeId],
    ) -> NodeId { … }
}
```

This eliminates ~20 sites where the forget-the-extend bug is one line away.

### Estimated LOC removable

~25 LOC, but the *behavioural* win (compile-time-enforced attribution) is bigger than the LOC count suggests.

### Risk

LOW. Mechanical wrap of `create_node` + `extend_asm_fingerprint_from`. All existing call-sites become 1-line shorter and demonstrably correct.

---

## 5. Dedup-cache key construction

`crates/ir/src/graph/store.rs` already builds `(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>)` cache keys per `create_node`. Round 7 scale audit (B5/C4) flagged the two `Vec` allocations as the dominant secondary-memory cost. There aren't *additional* sites building similar triples — fixtures use `create_node` directly. This is a **scale concern, not a duplication concern**, and is already covered by the round7-scale review.

### Estimated LOC removable

0 (single site).

### Risk

Out of scope for "generalize". See `round7-scale.md`.

---

## 6. Capture binding / multi-occurrence-agree

### Sites

| File:line | Logic |
|---|---|
| `crates/pattern/src/matcher/bindings.rs` | `Bindings::bind` enforces same-capture-same-binding (the implementation lives there, not duplicated) |
| `crates/pattern/src/matcher/mod.rs:676-687` | `prefix_agrees` — cross-pattern shared-capture filter for `find_all_requirements` |
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:115-131` | re-extracts `(a_out, b_out)` from a successful `Match` and threads them into `build_rhs` |

### Same vs different

* `Bindings::bind` is the canonical mechanism. `prefix_agrees` is a derivative (cross-`Match` agreement).
* `flag_cmp_canonicalize::try_apply_rule` doesn't reimplement binding agreement — it *uses* the matcher correctly. The duplication called out in the audit hypothesis isn't real here.

### Verdict

Already DRY. Skip.

---

## 7. Error converter boilerplate (PyO3)

`crates/strider-py/src/errors.rs:31-62` defines five converters with the same `format!("{e:?}")` + downcast skeleton. Two of them (`into_strider_err`, `into_lift_err`) downcast for `UnknownCallOtherError` independently.

### Same vs different

* **Same.** Each has shape `format!("{e:?}")` then `<Subclass>::new_err(...)`.
* **Different.** Which subclass is the default; which inner types get downcast-promoted (only `into_strider_err` checks both `UnresolvedIndirectBranch` and `UnknownCallOtherError`; `into_lift_err` checks only the latter).

### Proposed API

```rust
fn into_typed_err<D: PyErrDefault>(e: anyhow::Error) -> PyErr {
    if e.downcast_ref::<UnresolvedIndirectBranch>().is_some() {
        return UnresolvedIndirectBranchError::new_err(format!("{e:?}"));
    }
    if e.downcast_ref::<UnknownCallOtherError>().is_some() {
        return UnknownCallOtherError::new_err(format!("{e:?}"));
    }
    D::new_err(format!("{e:?}"))
}
```

`PyErrDefault` is a tiny trait with one fn `new_err`; each of `StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError` impls it. Reduces 5 functions × ~5 lines = 25 LOC into 1 function + 5 trait impls (~10 LOC).

### Estimated LOC removable

~15 LOC.

### Risk

LOW. Mechanical refactor; adds a tiny trait but the call-sites in `strider-py/src/lib.rs` (the actual users of these converters) keep their identical surface.

---

## 8. Test fixture builders

### Sites

| File:line | Helper |
|---|---|
| `crates/ir/src/test_utils.rs:14-96` | `make_empty_fn`, `make_fn_with_var`, `make_sp_fn`, `reg_vn`, `sp_vn_x86`, `sp_vn_x86_64` (the canonical helpers) |
| `crates/strider/tests/common/indirect_resolve_helpers/classify.rs:166,278,375,466,530,596,634` | seven sites that directly call `FunctionBuilder::new_raw(...)` rather than going through the canonical helpers |
| `crates/opt/src/sp_expr.rs:498-505,517-523,558-566,588-595,610-624,648-654,675-712,733-744,775-790` | every test fn has its own `FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?; let region = b.create_region()?; b.set_entry_region(region)?; b.set_region(region);` block |
| `crates/opt/src/constant_fold/tests.rs` | similar pattern × 30+ tests (build_int_const helper used heavily, but the *function setup* is repeated) |

### Same vs different

* `ir::test_utils::make_sp_fn` already exists (test_utils.rs:85) and perfectly captures the FunctionBuilder::new_raw + region setup pattern. **It's just under-used.** The opt crate's tests reach `FunctionBuilder` directly even when `make_sp_fn` would suffice.

### Proposed action

Audit `cargo test -p opt --no-run` warnings for unused helpers and migrate `crates/opt/src/sp_expr.rs` (10 tests) and `crates/strider/tests/common/indirect_resolve_helpers/classify.rs` (7 sites) to `make_sp_fn` / `make_fn_with_var`. No new helper code; just call-site cleanup.

### Estimated LOC removable

~80 LOC of test setup boilerplate.

### Risk

LOW. Test-only refactor; failures are immediate.

---

## 9. CFG region-edge dispatch

`cfg::RegionEdgeKind` is matched in:

| File:line | Match |
|---|---|
| `crates/cfg/src/cfg/dot.rs:93-95` | style mapping (Branch / Fallthrough / If*) |
| `crates/cfg/src/cfg/query.rs:84,98,108,109` | filter by edge kind to find unique outgoing |
| `crates/cfg/src/cfg/builder/region_builder.rs:294,296,337-341,380,497,522,585` | producer (creates each kind based on terminator shape) |
| `crates/cfg/src/cfg/builder/split.rs:107` | producer (always `Fallthrough`) |
| `crates/strider/src/strider/pipeline.rs:412` | filter (skip non-Fallthrough) |

### Verdict

* The producer sites all *set* the kind and don't dispatch on it.
* The consumer sites (`dot.rs`, `query.rs`, `pipeline.rs:412`) dispatch on the kind but with *different per-arm semantics* (style string, edge-pick predicate, predicate). There's no unifying shape.

A trait method `RegionEdgeKind::dot_style()` could move dot.rs:93-95 to `RegionEdgeKind`; that's a 3-line saving and arguable readability. Skip.

### Estimated LOC removable

~3 LOC.

### Risk

LOW but not worth the file ownership shuffle.

---

## 10. Endianness handling

### Sites

| File:line | Endianness use |
|---|---|
| `crates/reader/src/elf.rs:257,277,327-341` | `is_little_endian: bool` field (set from `obj.endianness()`); switch on it for `from_le_bytes` vs `from_be_bytes` |
| `crates/reader/src/elf.rs:464` | `endian_le = matches!(obj.endianness(), object::Endianness::Little)` (relocation application) |
| `crates/reader/src/elf.rs:872` | `value.to_le_bytes()` (hard-coded LE — relocation patching, may be a latent BE bug) |
| `crates/strider-py/src/reader.rs:578` | `u64::from_le_bytes(buf)` (always LE — already flagged HIGH bug in round7-strider-target-reader.md) |
| `crates/opt/src/load_readonly/mod.rs:21-29` | docstring **delegates** endianness to the `ReadOnlyMemory` impl — pass itself stays endian-agnostic |
| `crates/pcode-lift/src/vn_io.rs:188-196` | `match self.endianness { Little => …, Big => … }` for sub-register shift direction |
| `crates/opt/src/stack_load_forward/mod.rs:355-369` | `match endianness { Little => data, Big => synthesize ShiftRight }` |
| `crates/target/src/arch.rs:1-8` | `enum Endianness { Little, Big }` — the canonical type, already exists |

### Same vs different

* The canonical `target::Endianness` type already exists.
* Sites *using* it dispatch on `Little` / `Big` for very different operations: byte-read from a buffer, IR shift-direction, IR ShiftRight synthesis.
* The unification opportunity is in the **byte-read** family: `reader::elf` and `strider-py::reader` both do "place bytes in a buffer at the LE/BE-appropriate end, then call `from_{le,be}_bytes`." The pattern is verbatim except for the bool→enum mapping.

### Proposed API

```rust
// crates/target/src/arch.rs — extend Endianness with helpers
impl Endianness {
    /// Reads `size` bytes (≤ 8) from `bytes[..size]` and zero-pads /
    /// orients them into a u64 according to this endianness.  Caller-
    /// supplied buffer must be ≥ size.
    pub fn read_u64(&self, bytes: &[u8], size: usize) -> Option<u64> {
        if size == 0 || size > 8 || bytes.len() < size { return None; }
        let mut buf = [0u8; 8];
        let slot = match self {
            Self::Little => &mut buf[..size],
            Self::Big => &mut buf[8 - size..],
        };
        slot.copy_from_slice(&bytes[..size]);
        Some(match self {
            Self::Little => u64::from_le_bytes(buf),
            Self::Big => u64::from_be_bytes(buf),
        })
    }
}
```

This consolidates `reader/src/elf.rs:325-341` (15 LOC), and replacing `strider-py/src/reader.rs:567-579` with `Endianness::Little.read_u64(&buf, size)` *also fixes the latent always-LE bug* if the Python side wires `target::Endianness` through.

### Estimated LOC removable

~20 LOC plus a HIGH-severity bug fix (the strider-py reader is currently hard-coded to LE).

### Risk

LOW for the helper itself. MED for the strider-py wiring (need to thread endianness from `MemoryMap` construction; currently the Python `MemoryMap` carries no arch metadata).

---

## Bonus categories discovered during audit

### 11. `with_built` shim pattern

`crates/opt/src/pipeline.rs:94-104` defines `with_built(graph, entry, |fg| …)` as a `&mut Graph + NodeId → &mut BuiltFunctionGraph` adapter. `crates/strider/src/rewrite.rs:130-139` reimplements the exact same `mem::take` + `BuiltFunctionGraph::from_graph_and_entry` + restore pattern inline.

Fix: re-export `opt::with_built` (or move it to `ir`) and have `GraphRewriter::apply_rule` call it.

LOC removed: ~10. Risk: LOW.

### 12. Per-CallOther varnode-vec collection

`crates/strider/src/orchestrator.rs:622-630` and `crates/strider/src/orchestrator.rs:704-714` both compute `clobber_vars: Vec<rsleigh::Vn> = graph.variables.values().copied().filter(|v| !cc.callee_saved_regs.contains(v) && Some(*v) != stack_ptr_vn).collect()` — once for `set_call_clobbered_override`, once for `clobbered_kinds.push`. Same predicate, two iterations.

Fix: factor `clobber_vars_for_cc(graph, cc, sp_vn) -> Vec<rsleigh::Vn>` and call it twice (once for the override, once with a `.map(NodeOutputType::try_from)` bolt-on for the kinds vec).

LOC removed: ~5. Risk: LOW.

### 13. `count_*_nodes` test helpers

`crates/strider/src/strider/insn/control.rs:386-414` defines three test helpers (`count_if_nodes`, `count_eq_cmps`, `count_int_consts_eq`) that all `g.preorder().filter(|n| matches!(g.graph.node_kind(n), …)).count()`.

Fix: a single generic `count_kind_matching(g, |k| matches!(...)) -> usize` in `ir::test_utils`.

LOC removed: ~12 in test_utils + ~30 across opt/strider tests. Risk: LOW.

---

## Final ranked recommendation — top 3 to ship first

Ranked by `LOC removed × inverse risk × strategic value`:

### #1 — Category 4: `Graph::create_node_attributed`

**Why first:** ~25 LOC removed across ~15 sites, but the strategic value is enormous: every new opt pass author currently has to remember to call `extend_asm_fingerprint_from` after `create_node` or silently violate the superset contract. The new helper makes the right thing the only thing. Risk is LOW (mechanical wrap). It also lets us turn on `check_asm_fingerprints: true` in production validation once every site is migrated.

### #2 — Category 10: `Endianness::read_u64` helper

**Why second:** ~20 LOC removed and **fixes a HIGH-severity latent bug** (strider-py reader is hard-coded LE; BE targets read garbage). LOW risk for the helper; MED risk for the Python wiring (needs a way to carry endianness on `MemoryMap`). Should ship together with the round7-strider-target-reader bug-fix.

### #3 — Category 1 (subset): `WorkSet::seeded_kind` + `Graph::collect_kind`

**Why third:** ~50 LOC removed across the opt crate, mechanical, LOW risk, and it consolidates the most-repeated walk pattern in the codebase (12+ sites). The deeper `drive_with_consumer_reenqueue` driver (Category 1's MED-risk piece) can wait — the simple filter-and-collect helper is tractable now and unlocks the bigger refactor later. This is the most direct response to the user's seed example.

**Skip for now:**
* Category 3 (FixedPointDriver) — HIGH risk, the two loops legitimately differ.
* Category 5 (NodeFingerprint) — already covered by round7-scale.
* Category 6 (BindingTable) — already DRY.
* Category 9 (RegionEdgeKind dispatcher) — too small to bother.
