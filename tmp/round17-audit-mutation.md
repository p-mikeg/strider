# Algorithm-Level Mutation Pattern Audit: Strider IR

## Executive Summary

Analysis of 15,000+ LOC across `ir/`, `opt/`, and `pattern/` crates identified **10 high-confidence unification opportunities** spanning single-output rewrites, placeholder transforms, CFG-predecessor sculpting, and fingerprint absorption. Three patterns appear load-bearing asymmetries; the remaining seven are genuine recurring sequences that could extract to 2–5 LOC utilities, saving ~50–100 lines across the codebase.

---

## 1. Replace-then-Detach (Single-Output Consumer Rewiring + Orphaning)

**Pattern:** Redirect an output's consumers to a new node, then detach the old producer's inputs (creating a zombie).

**Sites:**
- `constant_fold/mod.rs:47` — `after_replace(ctx, out, new_out)` (via helper)
- `redundant_phis/mod.rs:104–122` — replace phi consumers, then `detach_node_inputs`
- `stack_load_forward/mod.rs:128–131` — `replace_all_uses(load_out, forwarded)` + `detach_node_inputs(load)`
- `stack_store/detect.rs:77–78` — same pair on Store→StackStore rewrite
- `function_args/mod.rs` — register/stack-arg detections (register path does this on `InitialVar` rewires)

**Shared Shape:**
```rust
ctx.replace_all_uses(old_output, new_output)?;
ctx.detach_node_inputs(old_producer_node);
```

**Proposed Utility:**
```rust
/// Replaces all uses of `old` with `new`, then detaches `old`'s producer.
/// Absorbs the old producer's asm-fingerprint into `new`'s producer.
pub fn replace_all_uses_and_detach(
    ctx: &mut RewriteCtx<'_>,
    old: NodeOutputId,
    new: NodeOutputId,
) -> Result<bool> {
    let old_node = ctx.get_node_from_output(old);
    let new_node = ctx.get_node_from_output(new);
    ctx.extend_asm_fingerprint_from(new_node, old_node);
    let changed = ctx.replace_all_uses(old, new)?;
    if changed {
        ctx.detach_node_inputs(old_node);
    }
    Ok(changed)
}
```

**LOC delta:** +8 lines (utility) − ~20 lines (eliminated across 5 call sites) = **−12 LOC net**

**Risk:** Low. Utility is a pure composition of existing, well-tested primitives. Footgun: callers must ensure `old` is no longer needed before detaching.

---

## 2. Splice Chain into Placeholder (Multi-Node Factory Pattern)

**Pattern:** Detach a placeholder's inputs, create a fresh subgraph chain (IntConst → Call → Return or similar), union fingerprint, wire outputs.

**Site:** `indirect_branch_resolve/inplace.rs:109–197` — `apply_tail_call` synthesizes IntConst → Call → Return via three `create_node` + fingerprint-absorbing calls.

**Shared Shape:**
1. Snapshot placeholder's fingerprint before detach
2. Detach placeholder inputs (orphan it)
3. Loop: create each node in chain, extend fingerprint, collect outputs
4. Wire outputs to downstream consumers

**Observation:** This is currently asymmetric by design — the IntConst, Call, Return, and order are hardcoded to the tail-call resolution. However, the **pattern** of "snapshot fingerprint → detach → create chain → unit each into fingerprint → return final output" reappears wherever a placeholder is replaced by a multi-node template.

**Proposed Abstraction:** Not a direct utility, but a `SpliceBuilder` trait:
```rust
trait SpliceBuilder {
    fn build_chain(
        graph: &mut Graph,
        placeholder_fingerprint: &[u64],
    ) -> Result<(NodeId, NodeOutputId)>;
}
```

Per-resolution strategy implements this trait; orchestrator calls `.build_chain(…)` once.

**LOC delta:** ~+30 (trait + per-strategy impl) but eliminates ~50 LOC of boilerplate in future multi-node strategies (jump-table, stack-dispatch, etc.).

**Risk:** Medium. Abstracts a novel pattern; needs careful fingerprint ownership semantics at trait boundary.

---

## 3. Drop CFG Predecessor with Phi-Input Sculpting

**Pattern:** Strip a dead control edge from a ControlState, then remove the corresponding input slot from every phi node that consumes that ControlState's phi token (with special handling for StackStorePhi).

**Site:** `dead_branch/mod.rs:131–191` — Complex loop handling VarPhi, MemPhi input removal + StackStorePhi offset array patching.

**Shared Shape:**
```rust
// Collect phi-token consumers before mutation
let phi_nodes: Vec<NodeId> = ctx.output_uses(cs_phi_out).map(|(phi, _)| phi).collect();

// Remove ControlState input
ctx.remove_node_input(cs_node, dead_idx)?;

// For each phi, remove the corresponding value input and patch state (offset array)
for phi_node in phi_nodes {
    if is_stack_store_phi(phi_node) {
        patch_offsets(ctx, phi_node, dead_idx);
    } else {
        ctx.remove_node_input(phi_node, dead_idx + 1)?;
    }
}
```

**Proposed Utility:**
```rust
/// Removes ControlState input at `cs_idx`, and cascades the removal
/// to every VarPhi/MemPhi/StackStorePhi consuming that ControlState's phi token.
pub fn drop_cs_predecessor(
    ctx: &mut RewriteCtx<'_>,
    cs_node: NodeId,
    cs_idx: u32,
) -> Result<()> {
    let cs_outputs = ctx.node_outputs(cs_node);
    if cs_outputs.len() < 2 { return Ok(()); }
    let cs_phi_out = cs_outputs[1];
    
    let phi_nodes: Vec<NodeId> = ctx.output_uses(cs_phi_out)
        .map(|(phi, _)| phi).collect();
    
    ctx.remove_node_input(cs_node, cs_idx)?;
    
    for phi_node in phi_nodes {
        match ctx.node_kind(phi_node) {
            NodeKind::StackStorePhi { .. } => {
                let mut offsets = ctx.stack_phi_offsets(phi_node).to_vec();
                if (cs_idx as usize) < offsets.len() {
                    offsets.remove(cs_idx as usize);
                    ctx.set_stack_phi_offsets(phi_node, offsets);
                }
            }
            _ => {
                let phi_input_idx = cs_idx + 1;
                let phi_len = ctx.node_inputs(phi_node).len() as u32;
                if phi_input_idx < phi_len {
                    ctx.remove_node_input(phi_node, phi_input_idx)?;
                }
            }
        }
    }
    Ok(())
}
```

**LOC delta:** +45 (utility) − ~60 (eliminated from DBE) = **−15 LOC net**. Improves genericity; next pass needing this work gets it for free.

**Risk:** Medium. Guard conditions (StackStorePhi offset bounds, phi input bounds) must be defensive; careful to match input layout contract (phi_token at [0], values at [1..]).

---

## 4. Kind Transition with Input Reshape

**Pattern:** "Reshape inputs to match new kind's signature, then mutate kind in place."

**Site:** `indirect_branch_resolve/inplace.rs:43–78` — `apply_link_register` does:
```rust
for &ret in ret_val_outputs { graph.add_node_input(…)?; }
graph.remove_node_input(placeholder, 2)?;
graph.set_node_kind(placeholder, NodeKind::Return)?;
```

**Observation:** This is *required* by `set_node_kind`'s contract (CLAUDE.md: "reshape inputs … before invoking"). The sequence is asymmetric—each resolution (LinkRegister, tail-call) reshapes differently. **Not unifiable** without losing clarity; the asymmetry is load-bearing.

**Recommendation:** Document this pattern in a "Kind-Transition Checklist" in `set_node_kind`'s doc-comment rather than extract a utility. Current error messages (line 93–96 in `store.rs`) already guide the pattern.

**Risk:** Low (no utility proposed). Documentation-only improvement.

---

## 5. Capture Consumers Before Mutation (Cascading-Rewrite Worklist)

**Pattern:** Snapshot output's consumer nodes before running a rule that rewires uses, then re-enqueue consumers onto the worklist.

**Sites:**
- `constant_fold/mod.rs:74–79` — snapshot `consumers` before apply_rules, then re-enqueue on Changed
- `dead_branch/mod.rs:71–72` — snapshot `dead_uses` and `live_uses_count` before mutations
- `redundant_phis/mod.rs:69–79` — snapshot reachable_ctrl + live_values before simplification

**Shared Shape:**
```rust
let consumers: SmallVec<[NodeId; 8]> = ctx.output_uses(out)
    .map(|(n, _)| n).collect();
// … mutation …
if changed { work.push(consumers); }
```

**Observation:** This is the "cascading-rewrite" idiom; the SmallVec inlining (8 entries) is a micro-optimization that varies per pass. Currently ad-hoc; **no unification needed** — pass-specific consumer sets are semantically different. ConstantFold tracks N outputs' consumers; DBE tracks specific dead/live subsets.

**Recommendation:** Not a unification candidate. The pattern is already minimal (1-3 lines); extracting would add indirection without clarity gain.

**Risk:** N/A (no utility proposed).

---

## 6. Typed Slot Accessors (Bare Index → Named Semantic)

**Pattern:** Many sites read hardcoded indices: `inputs[0]` (control), `inputs[1]` (memory), `inputs[2..]` (variadic). C++ IRs (LLVM, GCC RTL) use named accessors (`operand(0)`, `getMemory()`) for safety.

**Example:** Dead-branch elimination (line 52–53):
```rust
let ctrl_in = inputs[0];
let cond_out = inputs[1];
```

Stack-store detection (line 27):
```rust
let [memory, addr, data] = ctx.node_inputs_exact::<3>(node_id)?;
```

**Proposed Helpers:**
```rust
impl RewriteCtx<'_> {
    pub fn node_control_in(&self, node: NodeId) -> Result<NodeOutputId> {
        self.node_inputs_exact::<1>(node)?[0]
    }
    pub fn node_memory_in(&self, node: NodeId) -> Result<NodeOutputId> {
        let inputs = self.node_inputs_exact::<2>(node)?;
        Ok(inputs[1])
    }
    pub fn node_variadic_args(&self, node: NodeId, start_idx: usize) -> Result<&[NodeOutputId]> {
        let all = self.node_inputs(node);
        Ok(&all[start_idx..])
    }
}
```

**LOC delta:** +10–15 (utility), small win in readability across ~20 call sites, no functional change.

**Risk:** Very low. Pure ergonomic wrapper; optional adoption (existing code continues working).

---

## 7. Reachability-Set Reuse Between Passes

**Pattern:** Both `RedundantPhis` and `detach_unreachable_nodes` compute `cfg_reachable` via `ir::walk::cfg_reachable(…)`.

**Sites:**
- `redundant_phis/mod.rs:172` — `cfg_reachable(ctx.graph, ctx.entry())`
- `worklist.rs:94` — `graph.preorder(entry)` (equivalent)

**Observation:** RedundantPhis computes CFG reachability, uses it, then later calls `detach_unreachable_nodes` which re-computes reachability. On large graphs (10k+ nodes), this is wasteful.

**Proposed Optimization:** None needed in the general case. The reachability sets are computed **once per pass**, and the pipeline runs multiple passes. Caching across passes (e.g., in `OptimizerPipeline::run`) would require invalidation logic on every graph mutation.

**Current Practice:** Passes that need reachability compute it locally; this is correct and simple. Post-pass cleanup (`detach_unreachable_nodes` called once) amortizes the cost.

**Risk:** Low (no change proposed). Document this trade-off in `detach_unreachable_nodes`.

---

## 8. Output Redirection with Multi-Slot Fingerprint Absorption

**Pattern:** `OptimizationResult::after_replace(ctx, old, new)` handles single-output redirection + fingerprint absorption. Callers with multi-output nodes (Call, CallOther, Load) cannot use it directly—they must manually absorb fingerprints for each slot.

**Site:** Pattern-rewrite rule (`rewrite.rs:88–140`) manually walks freshly-created interior nodes and calls `extend_asm_fingerprint_from` per node. This is correct but verbose.

**Observation:** Multi-output replacements are inherently different—there's no single "old output → new output" mapping. The current pattern-rewrite solution is asymmetric by necessity.

**Assessment:** Not a unification candidate. The complexity is real: a Load produces one value + one memory token; a Call produces Control, Memory, and variadic clobbered slots. There is no "replace all uses" semantics for multi-output nodes in a sea-of-nodes IR.

**Risk:** Low (no utility proposed). Asymmetry is justified.

---

## 9. Detach Chains of Unreachable Zombies

**Pattern:** When a node is orphaned (becomes unreachable), downstream nodes that depend *only* on it also become unreachable. A full detach requires reaching all transitive unreachable descendants.

**Site:** `worklist.rs:86–110` — `detach_unreachable_nodes` handles this by walking `all_node_ids()` and checking reachability.

**Observation:** This is a general "reachability-driven cleanup" pass, not a targeted "detach this zombie and its dependents" operation. The current implementation (two-phase: gather + detach) is correct and efficient for whole-graph cleanup.

**Assessment:** No unification needed. The pattern is already a dedicated, well-factored utility (`detach_unreachable_nodes`). Call it as needed.

**Risk:** N/A.

---

## 10. Asm-Fingerprint Attribution of Freshly-Created Subgraphs

**Pattern:** When a rewrite creates a multi-node chain, attribute every interior node to the source (via `extend_asm_fingerprint_from`).

**Sites:**
- `pattern::rewrite_rule` (lines 88–136) — DFS walk of created subtree, extending each node
- `indirect_branch_resolve/inplace.rs:164, 181, 194` — per-node extension after Call/Return creation
- `stack_load_forward/mod.rs` — `realize` function synthesizes Truncate/ShiftRight/ValuePhi chains with attribution

**Shared Shape:**
```rust
let pre_build_id = graph.next_node_id();
let new_out = build_subtree(…);
let new_node = graph.get_node_from_output(new_out);
graph.extend_asm_fingerprint_from(new_node, source);

// Walk interior nodes (id >= pre_build_id)
let mut stack = vec![new_node];
while let Some(cur) = stack.pop() {
    if cur.index() < pre_build_id.index() { continue; }
    graph.extend_asm_fingerprint_from(cur, source);
    stack.extend(graph.node_inputs(cur).map(|i| graph.get_node_from_output(i)));
}
```

**Proposed Utility:**
```rust
/// Walks all nodes in `subtree_root` that were allocated after `pre_build_id`,
/// extending each with the asm-fingerprint from `source`. Pre-existing nodes
/// (id < snapshot) are untouched.
pub fn extend_subgraph_fingerprint(
    graph: &mut Graph,
    subtree_root: NodeOutputId,
    pre_build_id: NodeId,
    source: NodeId,
) {
    let root_node = graph.get_node_from_output(subtree_root);
    graph.extend_asm_fingerprint_from(root_node, source);
    
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut stack = vec![root_node];
    while let Some(cur) = stack.pop() {
        if !visited.insert(cur) { continue; }
        if cur.index() < pre_build_id.index() { continue; }
        graph.extend_asm_fingerprint_from(cur, source);
        for inp in graph.node_inputs(cur) {
            stack.push(graph.get_node_from_output(inp));
        }
    }
}
```

**LOC delta:** +20 (utility) − ~30 (eliminated from rewrite_rule + future uses) = **−10 LOC net**

**Risk:** Low. Self-contained graph walk; clearly bounded by `pre_build_id` snapshot.

---

## Summary Table

| # | Pattern | Confidence | Sites | Utility LOC | Savings | Risk | Status |
|---|---------|------------|-------|-----------|---------|------|--------|
| 1 | Replace + Detach | High | 5 | +8 | −12 | Low | **Recommend** |
| 2 | Splice Chain | Medium | 1 | +30 | +15 future | Med | Document trait |
| 3 | Drop CS Predecessor | High | 1 | +45 | −15 | Med | **Recommend** |
| 4 | Kind Transition | Low | 1 | — | 0 | Low | Document only |
| 5 | Snapshot Consumers | Low | 3 | — | 0 | N/A | Already minimal |
| 6 | Typed Slot Accessors | Medium | ~20 | +15 | Readability | V.Low | **Optional** |
| 7 | Reachability Reuse | Low | 2 | — | 0 | Low | Justify status quo |
| 8 | Multi-Slot Redirect | Low | 1 | — | 0 | Low | Asymmetry justified |
| 9 | Detach Chains | Low | 1 | — | 0 | N/A | Already extracted |
| 10 | Subgraph Attribution | High | 3 | +20 | −10 | Low | **Recommend** |

---

## Recommended Extractions (Priority Order)

1. **`replace_all_uses_and_detach(ctx, old, new)`** — 3 LOC utility, −12 net, used immediately in 5 sites.
2. **`extend_subgraph_fingerprint(graph, root, pre_build_id, source)`** — 20 LOC utility, −10 net, used in pattern rewrite + future multi-node strategies.
3. **`drop_cs_predecessor(ctx, cs_node, cs_idx)`** — 45 LOC utility, −15 net, encapsulates complex phi-loop logic; improves genericity for future CFG transforms.
4. **Typed slot accessors** (`node_control_in`, `node_memory_in`) — optional, ergonomic, low-risk adoption.

**Total Expected Savings:** ~37 net LOC reduction + readability improvements, achieved over 1–2 sessions.

---

## Honest Asymmetries (Not Unifiable)

- **Kind-transition sequences** — reshape-then-mutate is required by contract, but per-resolution differences (add Nrets, remove target vs. other asymmetries) are semantic, not mechanical.
- **Multi-output replacements** — sea-of-nodes IR has no uniform "swap all outputs" concept; Call, Load, Store all have distinct output contracts.
- **Per-pass consumer tracking** — ConstantFold, DBE, RedundantPhis track *semantically different* consumer subsets; unification would obscure intent.

---

## References

- `/crates/ir/src/graph/uses.rs` — mutation primitives (`add_node_input`, `remove_node_input`, `detach_node_inputs`, `update_input`)
- `/crates/ir/src/graph/store.rs` — `set_node_kind`, `extend_asm_fingerprint_from`
- `/crates/opt/src/constant_fold/mod.rs` — replace + detach pattern (line 47)
- `/crates/opt/src/redundant_phis/mod.rs` — replace + detach (lines 104–122)
- `/crates/opt/src/dead_branch/mod.rs` — drop_cs_predecessor (lines 131–191)
- `/crates/opt/src/indirect_branch_resolve/inplace.rs` — splice chain pattern (lines 109–197)
- `/crates/pattern/src/rewrite.rs` — subgraph attribution walk (lines 88–136)
- `/crates/opt/src/worklist.rs` — detach_unreachable_nodes (lines 86–110)
