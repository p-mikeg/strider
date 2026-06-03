# Self-Cleaning RewriteCtx + Shared OptCtx — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move all IR-rewrite machinery into `strider-opt` and give it a `RewriteCtx` whose `FunctionState` recursively culls dead nodes on every edit (keeping asm-fingerprints correct), with `OptCtx` as the shared home for cross-pass config + caches.

**Architecture:** A `FunctionState` (live-node set + roots + kill-queue + flags) is maintained incrementally by every `RewriteCtx` edit; the `OptimizerPipeline` populates it before the fixed-point loop and drains it after every changed pass. `Optimizer::apply(&mut RewriteCtx, &mut OptCtx)` is the only pass method; the pipeline takes `Function + OptCtx`.

**Tech Stack:** Rust workspace. `strider-ir` (sea-of-nodes IR + `Graph`/`Function` mutation primitives), `strider-pattern` (matching/templating), `strider-opt` (passes/pipeline/rewrite-after-this), `entity-utils` (`DenseEntitySet`, `Worklist`).

**Spec:** `docs/superpowers/specs/2026-06-03-rewrite-ctx-self-cleaning-design.md`.

**Working branch:** `refactor/rewrite-ctx-self-cleaning` (worktree `.worktrees/rewrite-ctx`, `fixtures/out` symlinked).

**Gate after each stage:** `cargo test --workspace` (baseline 3075) + `cargo clippy --workspace --all-targets` (0 warnings). Run `pytest` (baseline 841) at stages that touch strider-py (Stage 2, Stage 5) and before final merge. Push the branch after each stage's final commit (`git push origin refactor/rewrite-ctx-self-cleaning`).

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `crates/strider-ir/src/node/kind.rs` | `NodeKind` classification helpers | ADD `has_control_flow`, `has_side_effects` |
| `crates/strider-pattern/src/rewrite.rs` | rewrite machinery (today) | MOVE OUT (deleted here) |
| `crates/strider-pattern/src/lib.rs` | crate re-exports | REMOVE 9 rewrite re-exports |
| `crates/strider-opt/src/rewrite/mod.rs` | relocated rewrite + RewriteCtx | NEW (from strider-pattern + rewrite_ext merged) |
| `crates/strider-opt/src/rewrite/function_state.rs` | `FunctionState` (live/roots/queue/flags + cleanup) | NEW |
| `crates/strider-opt/src/rewrite_ext.rs` | opt-domain rewrite helpers | MERGE into `rewrite/mod.rs`, delete |
| `crates/strider-opt/src/pipeline.rs` | `Optimizer`, `OptCtx`, `OptimizerPipeline` | MODIFY (apply-only trait, `&mut OptCtx`, populate+drain in `run`) |
| `crates/strider-opt/src/peephole.rs` | peephole driver | MODIFY (seed from `rctx.reverse_postorder`/`postorder`) |
| `crates/strider-opt/src/{dead_branch,phi_collapse,region_collapse,load_forward,cfg_detach,indirect_branch_resolve}/…` | passes doing by-hand cleanup | MODIFY (drop manual detach; rely on auto-clean) |
| `crates/strider-opt/src/{function_args,load_readonly,stack_offset_detect}/…` | order-agnostic iteration | MODIFY (`rpo_filter` → `live_of_kind`) |
| `crates/strider-opt/src/{load_forward,function_args,call_stack_args}/…` | AliasMode consumers | MODIFY (read `octx.alias_mode`, drop field) |
| `crates/strider-opt/src/alias_mode.rs` | `AliasMode` | unchanged (just relocated usage) |
| `crates/strider-opt/tests/rewrite_build.rs` | relocated rewrite tests | NEW (moved from strider-pattern) |
| `crates/strider-orchestrator/src/orchestrator/mod.rs`, `strider/pipeline.rs` | pipeline driving + OptCtx build | MODIFY (build `OptCtx`, drop per-pass `.alias_mode`) |
| `crates/strider-py/src/function.rs` | FFI rewrite | MODIFY (`strider_opt::` rewrite imports) |

---

# Stage 1 — `NodeKind::has_side_effects`

### Task 1: Add `has_control_flow` + `has_side_effects` to `NodeKind`

**Files:**
- Modify: `crates/strider-ir/src/node/kind.rs` (next to `is_cacheable`, ~line 254)
- Test: same file, in its `#[cfg(test)] mod tests` (or add one)

- [ ] **Step 1: Write the failing test**

In `crates/strider-ir/src/node/kind.rs` test module:

```rust
#[test]
fn has_side_effects_is_control_flow_plus_memory_writes_and_opaque() {
    use NodeKind::*;
    // Control-flow nodes: side effects, and report control flow.
    for k in [Entry, Region, If, Return, Call, IndirectBranch] {
        assert!(k.has_control_flow(), "{k:?} should be control flow");
        assert!(k.has_side_effects(), "{k:?} should have side effects");
    }
    // Non-control side-effecting nodes: a memory WRITE + opaque ops.
    for k in [Store(rsleigh::VnSpace::RAM), CPoolRef, New] {
        assert!(!k.has_control_flow(), "{k:?} is not control flow");
        assert!(k.has_side_effects(), "{k:?} should have side effects");
    }
    // Pure value / read nodes: killable when unused (incl. a memory READ).
    for k in [
        IntConst(0),
        IntBinaryOp(crate::IntBinaryOp::Add),
        Load(rsleigh::VnSpace::RAM),
    ] {
        assert!(!k.has_control_flow(), "{k:?} is not control flow");
        assert!(!k.has_side_effects(), "{k:?} should NOT have side effects");
    }
}
```

- [ ] **Step 2: Run it, confirm it fails**

Run: `cargo test -p strider-ir node::kind::tests::has_side_effects_matches_control_flow`
Expected: FAIL — `no method named has_control_flow`.

- [ ] **Step 3: Implement**

Add to `impl NodeKind` (mirror the exhaustive match style of `is_cacheable`). Control-flow kinds are those carrying a `Control` input or output:

```rust
/// Whether this node participates in control flow (carries a `Control`
/// input or output): `Entry` / `Region` / `If` / `Return` / `Call` /
/// `CallOther` / `IndirectBranch`.
#[inline]
pub fn has_control_flow(&self) -> bool {
    matches!(
        self,
        NodeKind::Entry
            | NodeKind::Region
            | NodeKind::If
            | NodeKind::Return
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::IndirectBranch
    )
}

/// Whether a node may NOT be removed even when all its outputs are
/// unused.  Control-flow nodes, a memory **write** (`Store` — removing
/// it would be dead-store elimination, which needs deliberate aliasing
/// reasoning), and opaque ops (`CPoolRef` / `New`, whose resolution may
/// observe state).  Pure value nodes and memory **reads** (`Load`) /
/// joins (`MemPhi`) are NOT side-effecting and are culled when unused.
#[inline]
pub fn has_side_effects(&self) -> bool {
    self.has_control_flow()
        || matches!(self, NodeKind::Store(_) | NodeKind::CPoolRef | NodeKind::New)
}
```

Verify the exact variant names against the current `enum NodeKind` (e.g. `CallOther { user_op_id }` → `CallOther { .. }`). If `MemPhi`/`Phi` should count as control, they should NOT — they are value/phi-token joins, killable when unused.

- [ ] **Step 4: Run it, confirm pass**

Run: `cargo test -p strider-ir node::kind::tests::has_side_effects_matches_control_flow`
Expected: PASS.

- [ ] **Step 5: Gate + commit**

```bash
cargo clippy -p strider-ir --all-targets 2>&1 | grep -E "warning|error" || echo clean
git add crates/strider-ir/src/node/kind.rs
git commit -m "feat(strider-ir): add NodeKind::has_control_flow + has_side_effects"
```

---

# Stage 2 — Relocate `rewrite` into `strider-opt` (no behavior change)

> This stage is a mechanical move. The executing engineer should read the current `crates/strider-pattern/src/rewrite.rs` and `crates/strider-opt/src/rewrite_ext.rs` in full before starting. Net effect: identical behavior, new home.

### Task 2: Move the rewrite module file

**Files:**
- Move: `crates/strider-pattern/src/rewrite.rs` → `crates/strider-opt/src/rewrite/mod.rs`

- [ ] **Step 1:** Create the dir and move the file:

```bash
mkdir -p crates/strider-opt/src/rewrite
git mv crates/strider-pattern/src/rewrite.rs crates/strider-opt/src/rewrite/mod.rs
```

- [ ] **Step 2:** Fix intra-crate paths inside `rewrite/mod.rs`. It currently uses `crate::matcher::…`, `crate::template::…`, `crate::capture::…`, `crate::error::…` (strider-pattern-relative). These are now cross-crate → `strider_pattern::…`:

```bash
cd crates/strider-opt/src/rewrite
sed -i \
  -e 's#crate::matcher::match_pat::MatchPat#strider_pattern::MatchPat#g' \
  -e 's#crate::matcher::Matcher#strider_pattern::Matcher#g' \
  -e 's#crate::matcher::Pattern#strider_pattern::Pattern#g' \
  -e 's#crate::template::template_pat::TemplatePat#strider_pattern::TemplatePat#g' \
  -e 's#crate::template::{Template, instantiate}#strider_pattern::{Template, instantiate}#g' \
  -e 's#crate::capture::Capture#strider_pattern::Capture#g' \
  -e 's#crate::error::{Result, is_skip}#strider_pattern::{Result, is_skip}#g' \
  mod.rs
cd -
```
Then read `mod.rs` top-to-bottom and fix any remaining `crate::` references that pointed at strider-pattern internals (e.g. `crate::Matcher`, `crate::Pattern`) → `strider_pattern::`. Anything referring to `Function`/`Graph`/`NodeId`/`ValueId` stays `strider_ir::…` (already external). `crate::error::Result` may need `strider_pattern::Result` or `anyhow::Result` — match whichever the items resolve to.

- [ ] **Step 3:** Register the module in `crates/strider-opt/src/lib.rs`: add `pub mod rewrite;` and re-export the public surface at the crate root:

```rust
pub use rewrite::{
    BoxedRule, GraphRewriteCtxExt, GraphRewriter, RewriteCtx, RewriteCtxView,
    apply_rules_in_order, boxed_rule, rewrite_rule, rewrite_rule_runtime,
};
```

- [ ] **Step 4:** Remove from `crates/strider-pattern/src/lib.rs`: the `pub mod rewrite;` line (if present) and the `pub use rewrite::{ … }` block (the 9 names).

- [ ] **Step 5:** Build strider-opt — expect MANY errors (consumers still say `strider_pattern::RewriteCtx`). That's the next task. Just confirm `rewrite/mod.rs` itself compiles in isolation:

Run: `cargo build -p strider-opt 2>&1 | grep -E "rewrite/mod.rs" | head`
Expected: no errors pointing *inside* `rewrite/mod.rs` (errors will be at consumer call sites instead). If there are errors inside `rewrite/mod.rs`, fix the import paths from Step 2.

(No commit yet — the crate doesn't build until Task 3.)

### Task 3: Re-point all consumers; merge `rewrite_ext`

**Files:**
- Modify: every strider-opt pass file using `strider_pattern::RewriteCtx`
- Merge+delete: `crates/strider-opt/src/rewrite_ext.rs`
- Modify: `crates/strider-orchestrator/src/orchestrator/mod.rs`, `crates/strider-py/src/function.rs`

- [ ] **Step 1:** In `crates/strider-opt/src`, replace `strider_pattern::RewriteCtx` / `RewriteCtxView` / `GraphRewriter` / `rewrite_rule` / etc. with `crate::…`:

```bash
grep -rl "strider_pattern::RewriteCtx\|strider_pattern::GraphRewriter\|strider_pattern::rewrite_rule\|strider_pattern::BoxedRule\|strider_pattern::boxed_rule\|strider_pattern::apply_rules_in_order\|strider_pattern::GraphRewriteCtxExt\|strider_pattern::RewriteCtxView" crates/strider-opt/src \
 | xargs -r sed -i -E 's#strider_pattern::(RewriteCtx|RewriteCtxView|GraphRewriter|GraphRewriteCtxExt|rewrite_rule|rewrite_rule_runtime|apply_rules_in_order|boxed_rule|BoxedRule)#crate::\1#g'
```
Also fix any `use strider_pattern::{… RewriteCtx …}` brace-lists by hand (the sed above only catches `strider_pattern::Name`, not names inside a shared brace import).

- [ ] **Step 2:** Merge `rewrite_ext.rs` into `rewrite/mod.rs`. `rewrite_ext.rs` defines an extension trait (`OptRewrite`?) with `replace_value`, `redirect_input`, `remove_region_predecessors` on `RewriteCtx`, plus 3 tests. Move those methods into the inherent `impl RewriteCtx` in `rewrite/mod.rs` (they no longer need to be an extension trait — `RewriteCtx` lives in the same crate now), and move the 3 tests into `rewrite/mod.rs`'s `#[cfg(test)]`. Delete the trait and the file:

```bash
git rm crates/strider-opt/src/rewrite_ext.rs
```
Remove `pub mod rewrite_ext;` / `mod rewrite_ext;` from `crates/strider-opt/src/lib.rs` and any `use crate::rewrite_ext::OptRewrite;` from pass files (the methods are now inherent, no trait import needed).

- [ ] **Step 3:** Orchestrator: in `crates/strider-orchestrator/src/orchestrator/mod.rs`, `strider_pattern::RewriteCtxView` → `strider_opt::RewriteCtxView`, `use strider_pattern::GraphRewriteCtxExt;` → `use strider_opt::GraphRewriteCtxExt;`.

- [ ] **Step 4:** strider-py: in `crates/strider-py/src/function.rs`, `strider_pattern::{rewrite_rule_runtime, GraphRewriter, boxed_rule, BoxedRule}` → `strider_opt::…`. (strider-py already depends on strider-opt via orchestrator; confirm `strider-opt` is a direct dependency in `crates/strider-py/Cargo.toml` — add it if missing.)

- [ ] **Step 5:** Move the rewrite tests:

```bash
git mv crates/strider-pattern/tests/rewrite_build.rs crates/strider-opt/tests/rewrite_build.rs
# pattern_matching/rewrite.rs is part of the strider-pattern integration harness;
# move it into a strider-opt integration test (it may need its support helpers —
# read it first and relocate the minimal `Tb`/shapes it needs, or port to
# strider-ir-test-utils builders).
```
In the moved `tests/rewrite_build.rs`, change `use strider_pattern::{… rewrite items …}` to `use strider_opt::{…}` (the pattern items it still uses — `add`, `var`, `int_const`, `Capture`, `Matcher` — stay `strider_pattern::`).

- [ ] **Step 6:** Build + test the whole workspace:

Run: `cargo build --workspace 2>&1 | grep -E "^error" | head`
Expected: no errors. Fix any remaining `strider_pattern::` rewrite references the sed missed (brace imports, doc-links).

Run: `cargo test --workspace 2>&1 | grep -E "test result:|FAILED" | awk '/result/{p+=$4;f+=$6} END{print p" passed "f" failed"}'`
Expected: 3075 passed, 0 failed (pure move).

- [ ] **Step 7:** Gate + commit + push:

```bash
cargo clippy --workspace --all-targets 2>&1 | grep -cE "^warning|^error"   # expect 0
(cd crates/strider-py && uv run maturin develop >/dev/null 2>&1 && uv run pytest -q 2>&1 | tail -1)  # expect 841 passed
git add -A
git commit -m "refactor: relocate rewrite machinery from strider-pattern into strider-opt"
git push origin refactor/rewrite-ctx-self-cleaning
```

---

# Stage 3 — `FunctionState` + self-cleaning `RewriteCtx`

> The novel core. Build `FunctionState` test-first, then wire it into `RewriteCtx` and the pipeline. Read `crates/strider-ir/src/walk/mod.rs` (`GraphWalkInfo`, `DefUseSuccs`, `raw_def_use_succs`), `crates/strider-ir/src/graph/uses.rs` (`detach_node_inputs`, `value_uses`, `value_has_one_use`, `replace_all_uses`), and `crates/strider-ir-test-utils` (the `RegisterSet`/`FunctionBuilder` test builders) first.

### Task 4: `FunctionState` struct + `populate`

**Files:**
- Create: `crates/strider-opt/src/rewrite/function_state.rs`
- Modify: `crates/strider-opt/src/rewrite/mod.rs` (`mod function_state;`)
- Test: in `function_state.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests** (use `strider_ir_test_utils` builders to construct fixtures; a const + an Add that consumes it + an unreachable extra const):

```rust
#[test]
fn populate_seeds_live_and_roots_and_culls_dead() {
    // ret(add(k1, k2)); plus a dangling const not consumed by anything.
    let mut function = /* build via test-utils: see existing walk tests for the pattern */;
    let state = FunctionState::populate(&function);
    // roots are the input-less live nodes (Entry + the consts), no Add.
    assert!(state.roots.iter().all(|&n| function.node_inputs(n).is_empty()));
    // the dangling const is not live (unreachable from entry / unused).
    // every live node is reachable; the Add is live, its operands live.
}
```
(Construct the fixture with the same `Graph::create_node`/`add_node_input` helpers used in `crates/strider-ir/src/walk/mod.rs` tests; assert concrete node ids.)

- [ ] **Step 2:** Run, confirm fail (`FunctionState` undefined).

- [ ] **Step 3: Implement** `FunctionState`:

```rust
use entity_utils::set::DenseEntitySet;
use entity_utils::Worklist;
use cranelift_entity::SecondaryMap;
use strider_ir::{Function, node::NodeId};

bitflags::bitflags! {
    #[derive(Clone, Copy, Default)]
    pub(crate) struct NodeFlags: u8 { const ENQUEUED = 0b01; const OUTPUT_KILLED = 0b10; }
}

pub(crate) struct FunctionState {
    pub(crate) live_nodes: DenseEntitySet<NodeId>,
    pub(crate) roots: Vec<NodeId>,
    pub(crate) queue: Worklist<NodeId>,
    pub(crate) flags: SecondaryMap<NodeId, NodeFlags>,
}

impl FunctionState {
    pub(crate) fn populate(function: &Function) -> Self { /* see below */ }
}
```
`populate`: get `entry`; `let info = strider_ir::walk::GraphWalkInfo::compute_full(function.graph(), entry);` to obtain `info.live_nodes` + `info.roots`. Seed `Self { live_nodes: info.live_nodes, roots: info.roots, queue: Worklist::new(), flags: SecondaryMap::new() }`. Then cull pre-existing dead nodes: walk the raw def-use post-order (`strider_ir::walk::raw_def_use_succs` from roots — confirm the exact public entry; if only `DefUseSuccs`/`reverse_postorder` is public, expose a `pub fn` in walk for the raw postorder) and for any node NOT in `live_nodes`, kill it. (Killing here needs the mutable `RewriteCtx` — so in practice `populate` returns the state with `live_nodes`/`roots` seeded, and the *culling* of pre-existing dead nodes happens via the `RewriteCtx` built around it; do the initial cull as the first `clean()` after `RewriteCtx::new`. Keep `populate` pure-read: seed live/roots only.) Document this split.

NOTE: `GraphWalkInfo` currently exposes `pub roots` / `pub live_nodes` (confirmed). If `compute_full` is `pub`, reuse it; otherwise add a thin `pub` accessor in `strider-ir`.

- [ ] **Step 4:** Run, confirm pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/rewrite/function_state.rs crates/strider-opt/src/rewrite/mod.rs
git commit -m "feat(strider-opt): FunctionState with seeded live_nodes + roots"
```

### Task 5: `RewriteCtx` owns `FunctionState`; `kill_node` + `clean` (recursive cull)

**Files:**
- Modify: `crates/strider-opt/src/rewrite/mod.rs` (`RewriteCtx` gains `state: &mut FunctionState`)
- Test: `rewrite/mod.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests** for the recursive cull + side-effect gate:

```rust
#[test]
fn clean_recursively_culls_orphaned_operands() {
    // add(neg(k), k2): kill the add → neg loses its only use → neg culled;
    // k loses its only use → k culled; k2 likewise.  All become !live.
    let mut function = /* build add(neg(k), k2) returned by ret */;
    let mut state = FunctionState::populate(&function);
    let mut ctx = RewriteCtx::new(&mut function, &mut state);
    ctx.kill_node(add_node);
    ctx.clean();
    assert!(!ctx.is_live(neg_node));
    assert!(!ctx.is_live(k_node));
    assert!(!ctx.is_live(k2_node));
}

#[test]
fn clean_keeps_side_effect_node_with_no_uses() {
    // A Store whose memory output has NO uses still stays (Store is
    // side-effecting). Build add(load(...), k) then kill the add; the load is
    // culled (pure read), but a Store in the chain is never enqueued/culled.
}

#[test]
fn clean_keeps_shared_operand_with_another_live_use() {
    // add(k, k) then add2(k, other): killing add must NOT cull k (add2 still uses it).
}
```

- [ ] **Step 2:** Run, confirm fail.

- [ ] **Step 3: Implement** `kill_node`, `will_detach_value`, `enqueue`, `dequeue`, `is_node_dead`, `clean`, `is_live`, mirroring the reference (`crates/strider-opt/src/rewrite/mod.rs`):

```rust
impl<'g> RewriteCtx<'g> {
    pub fn new(function: &'g mut Function, state: &'g mut FunctionState) -> Self {
        let mut ctx = Self { function, state };
        ctx.cull_pre_existing_dead();   // the deferred populate cull (Task 4)
        ctx
    }

    pub fn is_live(&self, node: NodeId) -> bool { self.state.live_nodes.contains(node) }

    pub fn kill_node(&mut self, node: NodeId) {
        for input in self.function.graph().node_inputs(node) {
            self.will_detach_value(input);
        }
        self.function.graph_mut().detach_node_inputs(node);
        self.state.live_nodes.remove(node);
        // roots: remove if present (cheap retain or swap-remove)
        if let Some(pos) = self.state.roots.iter().position(|&r| r == node) {
            self.state.roots.swap_remove(pos);
        }
    }

    fn will_detach_value(&mut self, value: ValueId) {
        // If this detach removes the LAST use of `value`, its producer may be dead.
        if self.function.graph().value_uses(value).nth(1).is_none() {
            let def = self.function.graph().producer(value);
            if !self.function.node_kind(def).has_side_effects() {
                self.state.flags[def].insert(NodeFlags::OUTPUT_KILLED);
                self.enqueue(def);
            }
        }
    }

    fn enqueue(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node)
            && !self.state.flags[node].contains(NodeFlags::ENQUEUED)
        {
            self.state.flags[node].insert(NodeFlags::ENQUEUED);
            self.state.queue.enqueue(node);
        }
    }

    fn dequeue(&mut self) -> Option<NodeId> {
        while let Some(node) = self.state.queue.dequeue() {
            self.state.flags[node].remove(NodeFlags::ENQUEUED);
            if self.state.live_nodes.contains(node) { return Some(node); }
        }
        None
    }

    fn is_node_dead(&self, node: NodeId) -> bool {
        if self.function.node_kind(node).has_side_effects() { return false; }
        self.function.graph().node_outputs(node).iter()
            .all(|&o| self.function.graph().value_uses(o).next().is_none())
    }

    pub fn clean(&mut self) {
        while let Some(node) = self.dequeue() {
            let killed = self.state.flags[node].contains(NodeFlags::OUTPUT_KILLED);
            self.state.flags[node].remove(NodeFlags::OUTPUT_KILLED);
            if killed && self.is_node_dead(node) {
                self.kill_node(node);   // recurses via will_detach_value → enqueue
            }
        }
    }
}
```
Confirm exact `Graph`/`Function` method names against `crates/strider-ir/src/graph/uses.rs` + `function.rs` (`graph_mut`, `detach_node_inputs`, `value_uses`, `producer`, `node_inputs`, `node_outputs`, `node_kind`). `Worklist::dequeue` already dedups while-queued; the `ENQUEUED` flag is kept (per design) as the explicit not-already-queued guard.

- [ ] **Step 4:** Run, confirm pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/rewrite/mod.rs
git commit -m "feat(strider-opt): RewriteCtx kill_node + recursive clean via FunctionState"
```

### Task 6: Self-cleaning + live/roots maintenance on the edit verbs

**Files:**
- Modify: `crates/strider-opt/src/rewrite/mod.rs` (the existing edit methods: `create_node`, `create_node_attributed`, `add_node_input`, `update_input`, `remove_node_input`, `replace_value`, `replace_all_uses`, `remove_region_predecessors`)
- Test: `rewrite/mod.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn create_node_marks_live_and_tracks_root() {
    // creating an input-less const → live + appears in roots.
    // creating an Add over two values → live, NOT a root.
}

#[test]
fn add_node_input_drops_root_when_node_gains_input() {
    // create a Region (no inputs) → in roots; add_node_input → leaves roots.
}

#[test]
fn replace_value_enqueues_old_producer_and_clean_culls_it() {
    // replace_value(old_out, new_out); clean(); old's producer (no side effects, now unused) culled.
}
```

- [ ] **Step 2:** Run, confirm fail.

- [ ] **Step 3: Implement** the maintenance hooks in each edit verb:
  - `create_node*`: after creating `n`, `state.live_nodes.insert(n)`; `if function.graph().node_inputs(n).is_empty() { state.roots.push(n) }`.
  - `add_node_input(node, …)`: if `node` was input-less before the add (check before mutating), `swap_remove` it from `roots`.
  - `update_input` / `set_node_input` / `remove_node_input`: call `will_detach_value(displaced_old_value)` before rewiring.
  - `replace_value(old, new)`: keep existing fingerprint-absorb + `replace_all_uses`, then `enqueue_killed_def_node(old.producer)` (i.e. `will_detach_value`-style enqueue of the old producer).
  - `remove_region_predecessors`: built on `remove_node_input`, so it auto-enqueues.

  Keep all existing fingerprint logic exactly as relocated in Stage 2.

- [ ] **Step 4:** Run, confirm pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/rewrite/mod.rs
git commit -m "feat(strider-opt): edit verbs maintain live/roots + enqueue maybe-dead producers"
```

### Task 7: Cheap walks from cache — `reverse_postorder`, `postorder`, `live_of_kind`

**Files:**
- Modify: `crates/strider-opt/src/rewrite/mod.rs`
- Test: `rewrite/mod.rs` `#[cfg(test)]`

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn reverse_postorder_from_cache_matches_graph_rpo() {
    // ctx.reverse_postorder() equals Graph::reverse_postorder(entry) for a clean function.
}
#[test]
fn live_of_kind_filters_without_walking() {
    // two IntConsts + one Add: live_of_kind(IntConst) yields exactly the two consts.
}
```

- [ ] **Step 2:** Run, confirm fail.

- [ ] **Step 3: Implement** using the cached `roots`/`live_nodes`:

```rust
pub fn reverse_postorder(&self) -> Vec<NodeId> {
    let mut po = self.postorder();
    po.reverse();
    po
}
pub fn postorder(&self) -> Vec<NodeId> {
    // forward def->use post-order from cached roots, restricted to cached live_nodes.
    // Reuse strider_ir::walk::DefUseSuccs + graphwalk::PostOrder seeded with self.state.roots.
    use strider_ir::walk::{DefUseSuccs, PostOrder};
    PostOrder::new(
        DefUseSuccs::new(self.function.graph(), &self.state.live_nodes),
        self.state.roots.iter().copied(),
    ).collect()
}
pub fn live_of_kind<'a>(&'a self, pred: impl Fn(&strider_ir::node::NodeKind) -> bool + 'a)
    -> impl Iterator<Item = NodeId> + 'a
{
    self.state.live_nodes.iter().filter(move |&n| pred(self.function.node_kind(n)))
}
```
Confirm `DefUseSuccs`/`PostOrder` are `pub` in `strider_ir::walk` (they are, post the RPO refactor). Confirm `DenseEntitySet::iter()` exists.

- [ ] **Step 4:** Run, confirm pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/rewrite/mod.rs
git commit -m "feat(strider-opt): cached reverse_postorder/postorder/live_of_kind on RewriteCtx"
```

### Task 8: Pipeline drives populate + drain; `apply`-only `Optimizer`; `run(Function, &mut OptCtx)`

**Files:**
- Modify: `crates/strider-opt/src/pipeline.rs`
- Modify: `crates/strider-opt/src/peephole.rs` (seed via `rctx.reverse_postorder()`/`postorder()` instead of `rpo_filter`)
- Modify: every `impl Optimizer` (drop the `optimize` default usage; signature `apply(&self, &mut RewriteCtx, &mut OptCtx)`)
- Modify: orchestrator + tests that call `pass.optimize(&mut fn, &ctx)`

- [ ] **Step 1: Write a failing pipeline test:**

```rust
#[test]
fn pipeline_drains_dead_nodes_after_changed_pass() {
    // a one-pass pipeline whose pass replaces a value, leaving an orphan;
    // after run(), the orphan is not live (was drained).
}
```
Add a `run_one(pass, &mut function, &mut octx)` test helper in strider-opt (`#[cfg(test)]` or a small `pub(crate)`): build `FunctionState::populate`, `RewriteCtx::new`, `apply`, `clean`.

- [ ] **Step 2:** Run, confirm fail.

- [ ] **Step 3: Implement:**
  - `Optimizer` trait: keep only `fn apply(&self, rctx: &mut RewriteCtx<'_>, octx: &mut OptCtx<'_>) -> Result<OptimizationResult>;` + `name`. DELETE the `optimize(&mut Function, &OptCtx)` default.
  - `OptimizerPipeline::run(&self, function: &mut Function, octx: &mut OptCtx<'_>) -> Result<()>`: build `FunctionState::populate(function)`; `let mut rctx = RewriteCtx::new(function, &mut state);` (the `new` runs the initial cull); fixed-point loop: for each pass `if opt.apply(&mut rctx, octx)?.changed() { changed = true; rctx.clean(); }` (drain after a changed pass — or drain once after the inner loop if any pass changed; match the design's "after every pass that changed"); post-passes then `rctx.clean()`. Then `validate` + (existing) compact path.
  - `peephole.rs run_peephole`: replace the `ctx.rpo_filter(pred)` seed with `ctx.reverse_postorder()`/`ctx.postorder()` filtered by `matches_kind` (respecting the existing `SeedOrder`). Keep the worklist/consumer-reenqueue logic.
  - Update all `impl Optimizer for …`: rename `fn apply(&self, rctx, ctx)` second arg to `&mut OptCtx` (Stage 5 fills the OptCtx fields; for now just `&mut OptCtx` with the existing single `rom` field).
  - Update orchestrator `pipeline.run(&mut function, &ctx)` → `&mut ctx` (make the local `ctx` mutable). Update per-pass unit tests calling `.optimize(&mut fg, &ctx)` to the new `run_one` helper or a one-pass pipeline.

- [ ] **Step 4:** `cargo test --workspace` — expect 3075+new passing, 0 failed. Fix fallout (mostly mechanical: tests calling the removed `optimize`).

- [ ] **Step 5: Gate + commit + push:**

```bash
cargo clippy --workspace --all-targets 2>&1 | grep -cE "^warning|^error"
git add -A
git commit -m "feat(strider-opt): pipeline owns FunctionState (populate+drain); apply-only Optimizer; run(Function, &mut OptCtx)"
git push origin refactor/rewrite-ctx-self-cleaning
```

---

# Stage 4 — Migrate passes off by-hand cleanup

> For each pass, REMOVE the manual detach/use-count/dead-node code and rely on auto-clean; switch order-agnostic enumeration to `live_of_kind`. Behavior (and tests) must stay identical. Do ONE pass per task, gate after each.

### Task 9–14 (one per pass): de-manualize

For each of `dead_branch`, `phi_collapse`, `region_collapse`, `load_forward`, `cfg_detach`, `indirect_branch_resolve`:

- [ ] **Step 1:** Read the pass + its tests. Identify the manual bookkeeping (e.g. `ctx.detach_node_inputs(x)` calls that exist only to drop dead nodes, manual use-count checks, manual region-pred stripping).
- [ ] **Step 2:** Replace the rewire with the `RewriteCtx` verb that auto-enqueues (`replace_value` / `remove_region_predecessors` / `kill_node`), and DELETE the now-redundant manual detach. Where a pass detaches a node purely to kill it, call `ctx.kill_node` (or just `replace_value` and let `clean()` cull).
- [ ] **Step 3:** For `function_args`/`load_readonly`/`stack_offset_detect` (order-agnostic): replace `ctx.rpo_filter(pred)` with `ctx.live_of_kind(pred)`.
- [ ] **Step 4:** Run that pass's tests: `cargo test -p strider-opt <pass_module>` — expect unchanged pass/fail (all green).
- [ ] **Step 5:** Commit: `git commit -am "refactor(strider-opt): <pass> relies on RewriteCtx auto-cleanup"`.

After all six: full `cargo test --workspace` + clippy, then commit any leftover + push.

---

# Stage 5 — `OptCtx` shared config + caches

### Task 15: Extend `OptCtx`; make `AliasMode` global

**Files:**
- Modify: `crates/strider-opt/src/pipeline.rs` (`OptCtx`)
- Modify: `crates/strider-opt/src/{load_forward,function_args,call_stack_args}/mod.rs` (drop `alias_mode` field + setter; read `octx.alias_mode`)
- Modify: `crates/strider-orchestrator/src/strider/pipeline.rs` + `orchestrator/mod.rs` (build `OptCtx` with alias_mode; drop per-pass `.alias_mode(…)`)

- [ ] **Step 1: Write failing tests:** an `OptCtx` carrying `AliasMode::Strict` makes `LoadForward` behave strictly (port an existing alias-mode test to set it via `OptCtx` instead of the pass setter).

- [ ] **Step 2:** Run, confirm fail.

- [ ] **Step 3: Implement:**
```rust
pub struct OptCtx<'mem> {
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
    pub alias_mode: crate::AliasMode,
    pub call_clobbers_args: bool,
    pub sp_memo: crate::sp_expr::SpExprMemo,
    pub arg_layout: Option<strider_target::PositionalArgLayout>,  // built from the function's CC when known
}
```
Update `empty()`/`with_rom()` (and add a builder `OptCtx::for_run(rom, alias_mode, call_clobbers_args)` + a way to set `arg_layout`/clear `sp_memo`). `LoadForward`/`FunctionArgDetect`/`CallStackArgCollect`: delete their `alias_mode` field + `.alias_mode()` setter; read `octx.alias_mode`. `FunctionArgDetect`: read `octx.call_clobbers_args` + `octx.arg_layout`. Passes using `SpExprMemo`: take `&mut octx.sp_memo` instead of building a fresh local. In `OptimizerPipeline::run`, **clear `octx.sp_memo` at each drain point** (right where `rctx.clean()` runs).

- [ ] **Step 4:** Orchestrator: construct `OptCtx` once per run from `rom` + `lift_driver.alias_mode` + CC-derived `arg_layout`; remove the per-pass `.alias_mode(self.alias_mode)` calls in `build_*_optimizer_pipeline`.

- [ ] **Step 5:** `cargo test --workspace` — expect all green (3075+). Fix fallout.

- [ ] **Step 6: Gate + commit + push:**
```bash
cargo clippy --workspace --all-targets 2>&1 | grep -cE "^warning|^error"
(cd crates/strider-py && uv run maturin develop >/dev/null 2>&1 && uv run pytest -q 2>&1 | tail -1)
git add -A
git commit -m "feat(strider-opt): OptCtx holds AliasMode (global) + call_clobbers_args + sp_memo + arg_layout"
git push origin refactor/rewrite-ctx-self-cleaning
```

---

# Final: review + merge

- [ ] Full gate: `cargo test --workspace` (≥3075, 0 fail) + `cargo clippy --workspace --all-targets` (0) + `pytest` (841).
- [ ] Dispatch a code reviewer (pr-review-toolkit:code-reviewer) over the branch diff; fix Critical/Important.
- [ ] Prompt the user before merging (per their standing instruction), then merge `--no-ff` into `develop`, push, remove worktree + branch.

---

## Self-Review (plan vs. spec)

- **Part 1 (relocate):** Stage 2 (Tasks 2–3). ✓ incl. tests move + rewrite_ext merge + py/orchestrator re-point.
- **Part 2 (FunctionState/RewriteCtx):** Stage 3 (Tasks 4–8) + Stage 4 (migration). ✓ live_nodes+roots+queue+flags, populate, edit verbs, recursive clean, has_side_effects gate, cheap walks, pipeline populate+drain.
- **Part 3 (OptCtx):** Stage 5 (Task 15). ✓ AliasMode global, call_clobbers_args, sp_memo (cleared on drain), arg_layout, &mut OptCtx.
- **Part 4 (testing):** TDD steps throughout + full gate per stage. ✓
- **Part 5 (build order):** Stages 1–5 match the spec's order. ✓
- **has_side_effects:** Stage 1. `= has_control_flow() || Store | CPoolRef | New`. Matches the spec's "Store/Call/Return/… never auto-killed"; only pure value cones and memory *reads* (`Load`/`MemPhi`) are culled. ✓
- **Type consistency:** `FunctionState`, `RewriteCtx::new`, `clean`, `kill_node`, `live_of_kind`, `reverse_postorder`/`postorder`, `OptCtx` fields used consistently across tasks. ✓
- **Known soft spots the executor must resolve against real code:** exact `strider_ir::walk` raw-postorder public entry for `populate`'s cull; exact `Graph::graph_mut`/`detach_node_inputs` names; the `pattern_matching/rewrite.rs` test relocation (needs its support helpers). Each task flags "confirm against current code."
