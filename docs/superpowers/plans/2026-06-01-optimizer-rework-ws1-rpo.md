# Optimizer Rework — Workstream 1: RPO foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reverse-post-order (`rpo`) data-cone traversal to `strider-ir` that yields every operand before the node that consumes it, then rewrite `decompose_sp` as a flat single-sweep map-build over it.

**Architecture:** `Graph::rpo(seed)` wraps `graphwalk::PostOrder` over an inputs-only `GraphRef`, so a node is emitted after all its input producers (defs-before-uses). `decompose_sp` stops recursing/memoising and instead fills an `SpExpr` map in one `rpo` sweep, looking up already-classified operands. The 13 existing `decompose.rs` characterization tests are the behaviour gate — the rewrite must keep them all green with zero changes.

**Tech Stack:** Rust, `graphwalk` crate (`PostOrder`, `GraphRef`), `entity-utils` (`DenseEntitySet`, `SecondaryMap`), `cranelift-entity`.

**Spec:** `docs/superpowers/specs/2026-06-01-optimizer-rework-design.md` (D3, items 1 + 7).

---

## File structure

- `crates/strider-ir/src/walk/mod.rs` — add `InputSuccs<'a>` (`GraphRef` over input producers) next to `GraphWalkSuccs`; add `rpo_walk(graph, seed)` helper.
- `crates/strider-ir/src/graph/mod.rs` — add the public `Graph::rpo(seed)` method next to `walk_from`.
- `crates/strider-analyze/src/opt/sp_expr/decompose.rs` — rewrite `decompose_sp` body to the `rpo`-map form; keep the public signature, `SpExpr`, `SpExprMemo`, and `int_const_signed` unchanged.

No behaviour change is intended anywhere: this is a pure refactor gated by existing tests.

---

### Task 1: `rpo` ordering primitive on `Graph`

**Files:**
- Modify: `crates/strider-ir/src/walk/mod.rs` (add `InputSuccs` + `rpo_walk` after `GraphWalkSuccs`, around line 148)
- Modify: `crates/strider-ir/src/graph/mod.rs` (add `Graph::rpo` next to `walk_from`, ~line 247)
- Test: `crates/strider-ir/src/walk/mod.rs` `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/strider-ir/src/walk/mod.rs`:

```rust
// ── rpo (defs-before-uses data-cone walk) ─────────────────────────────────

/// `rpo` over `Add(InitialVar, IntConst)` must emit BOTH operands before
/// the `Add` that consumes them (defs-before-uses). The seed node is last.
#[test]
fn rpo_emits_operands_before_consumer() {
    let mut graph = Graph::new();
    let a = graph.create_node(
        NodeKind::InitialVar(rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    let [a_out] = graph.node_outputs_exact::<1>(a).unwrap();
    let c = graph.create_node(
        NodeKind::IntConst(4),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
    let add = graph.create_node(
        NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
        [a_out, c_out],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    let [add_out] = graph.node_outputs_exact::<1>(add).unwrap();

    let order: Vec<NodeId> = graph.rpo(add_out).collect();

    // All three appear exactly once.
    assert_eq!(order.len(), 3, "rpo must visit each cone node once: {order:?}");
    // The consumer (Add) is emitted AFTER both operands.
    let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
    assert!(pos(a) < pos(add), "InitialVar must precede Add");
    assert!(pos(c) < pos(add), "IntConst must precede Add");
    assert_eq!(order[2], add, "seed (Add) is emitted last");
}

/// `rpo` follows only data inputs — it must not fan out through control
/// successors, and must terminate on a graph with shared operands.
#[test]
fn rpo_visits_shared_operand_once() {
    let mut graph = Graph::new();
    let c = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
    // Add(c, c): the same operand drives both inputs.
    let add = graph.create_node(
        NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
        [c_out, c_out],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    let [add_out] = graph.node_outputs_exact::<1>(add).unwrap();

    let order: Vec<NodeId> = graph.rpo(add_out).collect();
    assert_eq!(order, vec![c, add], "shared operand visited once, before Add");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p strider-ir walk::tests::rpo_ -- --nocapture`
Expected: FAIL to compile — `no method named 'rpo' found for struct 'Graph'`.

- [ ] **Step 3: Add the inputs-only `GraphRef` and walk helper**

In `crates/strider-ir/src/walk/mod.rs`, after the `GraphWalk` type alias (around line 151), add:

```rust
/// A [`graphwalk::GraphRef`] whose successors are a node's **data-input
/// producers only** (no forward control edges).  Driving a post-order walk
/// with this relation yields every producer before the node that consumes
/// it — the defs-before-uses order used by value-cone analyses such as
/// `decompose_sp`.
#[derive(Clone, Copy)]
pub struct InputSuccs<'a>(&'a Graph);

impl graphwalk::GraphRef for InputSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.0
            .node_inputs(node)
            .into_iter()
            .map(|input| self.0.output_definition(input).0)
            .try_for_each(f)
    }
}

/// The concrete post-order walk type backing [`Graph::rpo`].
pub type RpoWalk<'a> = graphwalk::PostOrder<InputSuccs<'a>, DenseEntitySet<NodeId>>;

/// Walks the data-input cone of `seed`'s producer in defs-before-uses order:
/// every producer is yielded before the node consuming it, and the seed's
/// producer is yielded last.  Follows only data inputs (value, memory,
/// dispatch) — never forward control edges.
///
/// Cyclic data edges (a loop-carried `Phi` back-edge) are visited once via
/// the `DenseEntitySet` tracker; the defs-before-uses guarantee holds for
/// every acyclic edge and a back-edge's producer is simply visited in
/// whatever order the DFS reaches it (callers that care about cycles —
/// `decompose_sp` — handle the back-edge explicitly).
#[must_use]
pub(crate) fn rpo_walk(graph: &Graph, seed: NodeOutputId) -> RpoWalk<'_> {
    let seed_node = graph.node_for_output(seed);
    graphwalk::PostOrder::new(InputSuccs(graph), iter::once(seed_node))
}
```

- [ ] **Step 4: Expose `Graph::rpo`**

In `crates/strider-ir/src/graph/mod.rs`, next to `walk_from` (around line 247), add:

```rust
/// Reverse-post-order walk of the data-input cone reachable from `seed`.
///
/// Yields every producer node before the node that consumes it
/// (defs-before-uses); the producer of `seed` is yielded last.  Follows
/// only data inputs — see [`crate::walk::rpo_walk`].  Used by value-cone
/// analyses (e.g. SP-expression decomposition) that need each operand
/// classified before the node that uses it.
#[must_use]
pub fn rpo(&self, seed: crate::node::NodeOutputId) -> crate::walk::RpoWalk<'_> {
    crate::walk::rpo_walk(self, seed)
}
```

Ensure `RpoWalk` is re-exported: confirm `crate::walk` already has `pub use` for its public types, or add `pub use walk::RpoWalk;` wherever `GraphWalk` is re-exported (search `GraphWalk` in `crates/strider-ir/src/lib.rs` and mirror it).

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p strider-ir walk::tests::rpo_ -- --nocapture`
Expected: PASS (both `rpo_emits_operands_before_consumer` and `rpo_visits_shared_operand_once`).

- [ ] **Step 6: Confirm the whole IR crate still builds and tests green**

Run: `cargo test -p strider-ir`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir/src/walk/mod.rs crates/strider-ir/src/graph/mod.rs crates/strider-ir/src/lib.rs
git commit -m "feat(strider-ir): add rpo data-cone walk (defs-before-uses)"
```

---

### Task 2: Lock `decompose_sp` behaviour with its existing characterization tests

**Files:**
- Test: `crates/strider-analyze/src/opt/sp_expr/decompose.rs` (existing `#[cfg(test)] mod tests`, 13 tests)

No new test code — the file already ships the full behaviour net:
`decompose_sp_initial_var`, `_sub_constant`, `_add_negative_unsigned`,
`_memo_hit_returns_same_result`, `_non_sp_returns_none`,
`_memo_caches_intermediate_results`, `_does_not_cache_none_results`,
`_phi_with_non_sp_pred_returns_none`,
`_and_with_alignment_mask_yields_opaque_base`,
`_sub_after_and_chains_offset_through_opaque_base`,
`_budget_kicks_in_on_deep_and_chain`,
`_does_not_stack_overflow_on_deep_chain`, plus the `int_const_signed_*` trio.

- [ ] **Step 1: Run the existing tests and record the baseline**

Run: `cargo test -p strider-analyze sp_expr::decompose::tests`
Expected: PASS (all 13+ green). This is the exact set the Task 3 rewrite must keep green.

- [ ] **Step 2: Note the two semantics these tests pin** (no code change)

Record in the task log, because Task 3 must preserve both:
1. **`None` is never memoised** (`_does_not_cache_none_results`) — distinguishes "not SP-rooted" from "cycle-truncated."
2. **Phi requires every predecessor to be a `Terminal`**; a mixed/loop-carried phi yields `None` or a multi-offset `SpExpr::Phi`, never a fabricated `offset = 0` (`_phi_with_non_sp_pred_returns_none`).

---

### Task 3: Rewrite `decompose_sp` as a flat `rpo` map-build

**Files:**
- Modify: `crates/strider-analyze/src/opt/sp_expr/decompose.rs:134-344` (replace `decompose_sp`, `decompose_sp_inner`, `decompose_sp_phi`, `commit_spine_to_memo`; keep `SpExpr`, `SpExpr::shifted`, `int_const_signed`, `SpExprMemo`, `MAX_DECOMPOSE_DEPTH` unchanged)

**Approach:** Build an `SpExpr` map over the seed's data cone in one `rpo` sweep.
Because `rpo` yields operands before consumers, each node's operand lookups
hit the partial map directly — no spine, no per-level memo rewind, no manual
recursion. The `SpExprMemo` (a `FxHashMap<NodeOutputId, Option<SpExpr>>`)
remains the cross-call cache and the per-sweep working map at once.

- [ ] **Step 1: Replace the decomposition body**

Replace the block from `pub fn decompose_sp` (line 134) through the end of
`decompose_sp_phi` (line 344) with:

```rust
/// Decomposes `out` into `InitialVar(sp) + K` (or per-branch equivalent),
/// caching definitive results in `memo`.
///
/// Implemented as a single defs-before-uses (`Graph::rpo`) sweep over the
/// address cone: because every operand is classified before the node that
/// consumes it, each arm is a local map lookup. Cyclic `Phi(sp)` back-edges
/// are the only non-DAG edge; a back-edge whose source is not yet classified
/// when the phi is processed is treated as "unknown," which collapses the
/// phi to `None` unless every predecessor independently resolves to the same
/// `Terminal` (matching the prior recursive contract).
pub fn decompose_sp(
    function: &Function,
    out: NodeOutputId,
    stack_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Option<SpExpr> {
    if let Some(cached) = memo.get(&out) {
        return cached.clone();
    }
    for node in function.graph().rpo(out) {
        // One value output per node in the address cone; skip nodes without
        // a single value output (none arise in a well-formed address cone,
        // but stay defensive).
        let Ok([node_out]) = function.node_outputs_exact::<1>(node) else {
            continue;
        };
        if memo.contains_key(&node_out) {
            continue;
        }
        let expr = classify_sp_node(function, node, node_out, stack_vn, memo);
        // Mirror the legacy contract: never cache a `None` verdict (a
        // cycle-truncated branch may resolve on a different call path).
        if expr.is_some() {
            memo.insert(node_out, expr);
        }
    }
    memo.get(&out).cloned().flatten()
}

/// Classifies a single node in the address cone given that all of its
/// operands have already been classified into `memo` (guaranteed by the
/// defs-before-uses `rpo` order, except for `Phi` back-edges which are
/// handled by reading whatever the map currently holds).
fn classify_sp_node(
    function: &Function,
    node: NodeId,
    node_out: NodeOutputId,
    stack_vn: rsleigh::Vn,
    memo: &SpExprMemo,
) -> Option<SpExpr> {
    match *function.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == stack_vn => Some(SpExpr::Terminal {
            base: node_out,
            offset: 0,
        }),
        NodeKind::Phi if function.phi_var_tag(node) == Some(stack_vn) => {
            classify_sp_phi(function, node, memo)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = function.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let (l, r) = (inputs[0], inputs[1]);
            // Add of a base and a constant: shift the base's offset.
            if let Some(c) = int_const_signed(function, r) {
                return memo.get(&l).cloned().flatten().map(|e| e.shifted(c));
            }
            if let Some(c) = int_const_signed(function, l) {
                return memo.get(&r).cloned().flatten().map(|e| e.shifted(c));
            }
            None
        }
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            // Alignment dance (`sp & mask`): the And output is a fresh opaque
            // base (offset 0) iff the non-mask operand is itself SP-rooted.
            let inputs = function.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let (l, r) = (inputs[0], inputs[1]);
            let sp_input = if int_const_signed(function, r).is_some() {
                l
            } else if int_const_signed(function, l).is_some() {
                r
            } else {
                return None;
            };
            // Discard the inner decomposition's offset — the And output is a
            // new opaque base.
            memo.get(&sp_input)
                .cloned()
                .flatten()
                .map(|_| SpExpr::Terminal {
                    base: node_out,
                    offset: 0,
                })
        }
        _ => None,
    }
}

/// Classifies a `Phi(sp)` from its already-classified predecessor values.
/// Every predecessor must resolve to a `Terminal`; a predecessor still
/// unclassified (loop back-edge) or non-`Terminal` collapses the phi to
/// `None`, preserving the legacy "never fabricate offset 0" contract.
fn classify_sp_phi(function: &Function, node: NodeId, memo: &SpExprMemo) -> Option<SpExpr> {
    let inputs = function.node_inputs(node);
    // inputs[0] = dispatch token; inputs[1..] = per-predecessor values.
    if inputs.len() < 2 {
        return None;
    }
    let mut bases = Vec::with_capacity(inputs.len() - 1);
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    for pred in inputs.into_iter().skip(1) {
        let Some(SpExpr::Terminal { base, offset }) = memo.get(&pred).cloned().flatten() else {
            return None;
        };
        bases.push(base);
        offsets.push(offset);
    }
    if bases.iter().all(|&b| b == bases[0]) && offsets.iter().all(|&o| o == offsets[0]) {
        Some(SpExpr::Terminal {
            base: bases[0],
            offset: offsets[0],
        })
    } else {
        Some(SpExpr::Phi {
            phi_node: node,
            offsets,
        })
    }
}
```

Delete `commit_spine_to_memo`, `decompose_sp_inner`, and `decompose_sp_phi`
(the old recursive forms). Keep `MAX_DECOMPOSE_DEPTH` only if still
referenced; the `rpo` sweep is iterative and bounded by the cone size, so the
deep-And and deep-chain budget is now structural — if `MAX_DECOMPOSE_DEPTH`
becomes dead, remove it and update the two tests that reference
`super::MAX_DECOMPOSE_DEPTH` to assert termination via cone size instead (see
Step 3).

- [ ] **Step 2: Confirm `Function::graph()` exists**

Run: `grep -n "pub fn graph(" crates/strider-ir/src/function.rs`
Expected: a `pub fn graph(&self) -> &Graph` accessor. If absent, add it next
to the other `Function` accessors:

```rust
/// Borrows the underlying IR graph.
#[must_use]
pub fn graph(&self) -> &crate::graph::Graph {
    &self.graph
}
```

- [ ] **Step 3: Run the characterization tests**

Run: `cargo test -p strider-analyze sp_expr::decompose::tests`
Expected: PASS for all behavioural tests.

Two tests reference `super::MAX_DECOMPOSE_DEPTH`
(`_budget_kicks_in_on_deep_and_chain`) and the 5000-node chain
(`_does_not_stack_overflow_on_deep_chain`). The `rpo` form cannot stack
overflow (the walk is heap-stack iterative). If `_budget_kicks_in_on_deep_and_chain`
no longer applies (no recursion to bound), change it to assert the deep
nested-And chain still returns a `Terminal` opaque base without panicking:

```rust
#[test]
fn decompose_sp_deep_and_chain_terminates_without_overflow() -> crate::opt::Result<()> {
    let sp = sp();
    let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
    let mut current = b.read_variable(&sp)?;
    let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::I32)?;
    const N: usize = 6000;
    for _ in 0..N {
        current = b.build_int_binary_operation(
            current, mask, IntBinaryOp::And, NodeOutputType::I32)?;
    }
    b.build_return(Some(current), &[])?;
    b.set_lift_addr(None);
    let fg = b.build()?;
    let mut memo = SpExprMemo::default();
    // Iterative rpo sweep: the deep And chain resolves to an opaque base
    // (each And re-bases) without recursion, so no stack overflow.
    let r = decompose_sp(&fg, current, sp, &mut memo);
    assert!(matches!(r, Some(SpExpr::Terminal { offset: 0, .. })));
    Ok(())
}
```

Keep `_does_not_stack_overflow_on_deep_chain` as-is — it must still pass
(the rpo sweep handles a 5000-deep Add chain and returns offset `N`).

- [ ] **Step 4: Run the SP-dependent pass tests**

Run: `cargo test -p strider-analyze stack_offset_detect`
Then: `cargo test -p strider-analyze load_forward call_stack_args function_args`
Expected: PASS — `StackOffsetDetect`, `LoadForward`, `CallStackArgCollect`,
and `function_args` all consume `decompose_sp` and must be unaffected.

- [ ] **Step 5: Run the full analyze crate + clippy**

Run: `cargo test -p strider-analyze`
Then: `cargo clippy -p strider-analyze --no-deps`
Expected: PASS, no new clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-analyze/src/opt/sp_expr/decompose.rs
git commit -m "refactor(strider-analyze): rebuild decompose_sp on the rpo data-cone walk"
```

---

### Task 4: Regression backstop on real fixtures

**Files:** none (gate only)

- [ ] **Step 1: Run the workspace test suite**

Run: `cargo test --workspace`
Expected: No NEW failures versus the known baseline (per project memory, a
small set of pre-existing fixture failures is expected; the criterion is "no
new failures").

- [ ] **Step 2: Run the orchestrator demo as a smoke test**

Run: `cargo run -p strider-analyze --example orchestrator_demo`
Expected: Completes, emitting `cfg.html`, `graph.html`, `graph-opt.html` in
the workspace root with no panic.

- [ ] **Step 3: Final commit if any incidental fixes were needed**

```bash
git add -A
git commit -m "test: workspace regression backstop for rpo + decompose_sp rework"
```

---

## Self-review

**Spec coverage (D3 / items 1, 7):**
- Item 1 (`rpo` primitive on IR, used for iteration) — Task 1 adds `Graph::rpo`.
  Migrating the worklist analysis passes' *seed order* to `rpo` is deferred: they
  are fixpoint worklists whose result is order-independent, so churning them in
  this workstream adds risk without behaviour benefit. Tracked as a follow-up in
  the Workstream-2 plan where the Rewrite layer touches those passes anyway.
- Item 7 (`decompose_sp` as a flat Add/InitialVar/And loop) — Task 3.

**Placeholder scan:** none — every step carries real code or an exact command.

**Type consistency:** `rpo(seed: NodeOutputId) -> RpoWalk`, `RpoWalk` =
`PostOrder<InputSuccs, DenseEntitySet<NodeId>>`, `rpo_walk` helper, `InputSuccs`
GraphRef — names consistent across Tasks 1 and 3. `decompose_sp` keeps its
4-arg signature and `SpExpr` shape, so all four consumer passes compile unchanged.

**Risk note:** the only behavioural subtlety is the `Phi(sp)` loop back-edge
(Task 3, `classify_sp_phi`). The existing `_phi_with_non_sp_pred_returns_none`
test is the gate; if a loop-carried SP phi regresses, the fix is to seed the
phi's own output as "unknown" before the sweep reaches its back-edge
predecessor (already the effective behaviour, since an unclassified predecessor
reads as `None` from the map).
