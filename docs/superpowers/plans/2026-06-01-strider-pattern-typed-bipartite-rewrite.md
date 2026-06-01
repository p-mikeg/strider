# strider-pattern typed-bipartite rewrite — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the `strider-pattern` crate to a bipartite IR-mirroring pattern graph with a compile-time-typed `MatchPat`/`TemplatePat` builder API, then migrate `strider-analyze`, `strider-py`, and `strider-pattern-macros` onto it.

**Architecture:** One internal `Pattern` type backed by `petgraph::StableDiGraph<PatVertex, PatEdge>` where `PatVertex` is `Node | Output` (mirroring IR `Node → NodeOutput → Node`). Two API layers lower onto one imperative `MatcherBuilder`/`TemplateBuilder` SSoT: compile-time-typed structs (`Add<L,R>`, `Var`, …) for value-producing patterns, and fluent builders for variadic/control patterns (`call`, `if_node`, `phi`, …). Buildability of a rewrite RHS is a compile-time trait bound (`TemplatePat`), not a runtime check.

**Tech Stack:** Rust, `petgraph` 0.8, `strider-ir`, `cranelift-entity`, `rustc-hash`; PyO3/maturin for `strider-py`; `proc-macro2`/`quote`/`syn` for the macro.

**Design spec:** `docs/superpowers/specs/2026-06-01-strider-pattern-rewrite-design.md` (read it first).

**Note on the sibling file** `2026-06-01-strider-pattern-rewrite.md`: that is the *original* plan that built today's crate. This plan rewrites what that one built; keep both.

---

## Execution strategy & invariants

- **Branch:** all work on `develop`; push after every commit (`git push origin develop`). Merge to `master` only at Phase 10.
- **Green-per-crate, not green-per-workspace mid-flight.** Phases 1–7 bring `strider-pattern` to green (`cargo test -p strider-pattern`). `strider-analyze`/`strider-py` will not compile until Phases 8–9 — expected for a rename-shaped rewrite. Don't run `cargo test --workspace` for signal until Phase 10 (project convention: per-crate testing for fast signal).
- **Free-function names stay stable** (`add`, `var`, `int_const`, `truncate`, `rewrite_rule`, `find_all`, …) to minimize consumer churn. Signatures change; names do not.
- **The existing `crates/strider-pattern/tests/` suite is the behavioural contract.** Phase 7 ports it; a test that cannot be expressed in the new API is a design gap to escalate, not delete.
- **No `Arc`/`Send`/`Sync`** in the core (`Box` for owned closures; `Rc` only behind the Python FFI).
- **TDD always:** failing test → run-fail → minimal impl → run-pass → commit.
- **No plan/Phase/Task identifiers in code or commit messages** (project convention).
- **Regression bar:** the 4 pre-existing v2 baseline failures are allowed; **no new failures**. ConstantFold fixture output must be unchanged node-for-node.

The typed-trait mechanism (`MatchPat`/`TemplatePat`, free-fn ergonomics, combinator wrappers, compile-time rejection of a wildcard RHS) was de-risked with a throwaway `rustc` spike before this plan was written — both the buildable-RHS-compiles and wildcard-RHS-is-a-compile-error halves were confirmed.

---

## File structure (new `strider-pattern/src`)

```
lib.rs                      # re-exports; crate docs
capture.rs                  # opaque Capture only (delete named/string table)
bindings.rs                 # Vec-backed, truncate-only restore
error.rs                    # unchanged (RewriteSkip/PatternBuildError sentinels)
pattern/
  mod.rs                    # `Pattern` type (the one graph type) + petgraph store
  vertex.rs                 # PatVertex, PatNode, PatOutput, KindSpec, OutputKindSpec
  edge.rs                   # PatEdge (produces / consumes-at-slot)
  topo.rs                   # petgraph::algo::toposort + DfsPostOrder reachability
builder/
  mod.rs                    # MatcherBuilder + TemplateBuilder (the SSoT)
  refs.rs                   # PatOutRef / PatNodeRef handles
match_pat.rs                # MatchPat trait + combinator wrappers (Captured/Guarded/Limited/Ordered)
template_pat.rs             # TemplatePat trait
typed/
  mod.rs                    # free functions returning typed structs
  value_ops.rs              # Add/Mul/.../Truncate/Extend/... structs + free fns
  consts.rs                 # IntConst/FloatConst/AnyIntConst/... structs
  wildcards.rs              # Any/Var/Predicate/value_of_width/... structs
control/
  mod.rs                    # call/call_other/ret/if_node/phi/... (builder-based)
matcher/
  mod.rs                    # Matcher<'f> (kind index, find_all/first/joined/match_at)
  walk.rs                   # bipartite recursive match + commutative retry + cast walk-through
template/
  mod.rs                    # instantiate (toposort) + TemplateCtx + TemplateKind
rewrite/
  mod.rs                    # rewrite_rule (compile-time) + rewrite_rule_runtime (FFI) +
                            # GraphRewriter/RewriteCtx/BoxedRule/apply_rules_in_order + fingerprints
```

Delete old modules (`pat_graph/`, `builders/`, old `matcher/try_match.rs`) in the commit that supersedes them so the crate never carries two parallel implementations.

---

## Phase 1 — Representation + Bindings

### Task 1.1: `Capture` — opaque only

**Files:**
- Modify: `crates/strider-pattern/src/capture.rs`
- Test: inline `#[cfg(test)]` in `capture.rs`

- [ ] **Step 1: Write the failing test** (replace `named`-based assumptions; keep uniqueness)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_ids_are_globally_unique() {
        let n = 256;
        let ids: std::collections::HashSet<u32> = (0..n).map(|_| Capture::new().id()).collect();
        assert_eq!(ids.len(), n);
    }
}
```

- [ ] **Step 2: Run** `cargo test -p strider-pattern --lib capture 2>&1 | tail -5` — confirms it compiles against `new()`/`id()`.
- [ ] **Step 3: Delete the string path** — remove `Capture::named` and the `OnceLock<Mutex<HashMap<String, Capture>>>` table. Keep `new()`, `id()`, `Default`, derives, atomic counter. Other modules still referencing `named`/`.cap` are fine to defer; record them: `git grep -n 'Capture::named'`.
- [ ] **Step 4: Run** `cargo test -p strider-pattern --lib capture 2>&1 | tail -5` → PASS (once crate compiles).
- [ ] **Step 5: Commit**

```bash
git add crates/strider-pattern/src/capture.rs
git commit -m "refactor(strider-pattern): drop string-keyed Capture from core"
git push origin develop
```

### Task 1.2: `Bindings` — Vec-backed, truncate-only restore

**Files:**
- Modify: `crates/strider-pattern/src/bindings.rs`
- Test: inline tests in `bindings.rs` (preserve the existing suite)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn restore_is_pure_truncate_and_rebind_succeeds() {
    let function = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::I64)).unwrap();
    let n = function.walk().find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_))).unwrap();
    let mut b = Bindings::default();
    let (kept, dropped) = (Capture::new(), Capture::new());
    assert!(b.bind_capture(kept, Binding::Node(n)));
    let mark = b.mark();
    assert!(b.bind_capture(dropped, Binding::Node(n)));
    b.restore(mark);
    assert!(b.get_binding(kept).is_some());
    assert!(b.get_binding(dropped).is_none());
    assert!(b.bind_capture(dropped, Binding::Node(n))); // rebinds clean
}
```

- [ ] **Step 2: Run** `cargo test -p strider-pattern --lib bindings 2>&1 | tail -5` → PASS against current overlay impl (behaviour identical; we're simplifying internals).
- [ ] **Step 3: Simplify the implementation**

```rust
#[derive(Clone, Default)]
pub struct Bindings { entries: Vec<(Capture, Binding)> }
pub struct BindingsMark(usize);

impl Bindings {
    pub(crate) fn mark(&self) -> BindingsMark { BindingsMark(self.entries.len()) }
    pub(crate) fn restore(&mut self, m: BindingsMark) { self.entries.truncate(m.0); }
    pub(crate) fn bind_capture(&mut self, c: Capture, b: Binding) -> bool {
        if let Some((_, existing)) = self.entries.iter().find(|(k, _)| *k == c) { return *existing == b; }
        self.entries.push((c, b)); true
    }
    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.entries.iter().rev().find(|(k, _)| *k == c).map(|(_, b)| *b)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Capture, Binding)> + '_ { self.entries.iter().copied() }
    pub fn is_bound(&self, c: Capture) -> bool { self.entries.iter().any(|(k, _)| *k == c) }
    // get_output/get/get_node + typed extractors: unchanged bodies (they call get_binding)
}
```

Drop the `rustc_hash` import if now unused here.

- [ ] **Step 4: Run** `cargo test -p strider-pattern --lib bindings 2>&1 | tail -10` → PASS (existing + new).
- [ ] **Step 5: Commit**

```bash
git add crates/strider-pattern/src/bindings.rs
git commit -m "perf(strider-pattern): Vec-backed Bindings with truncate-only restore"
git push origin develop
```

### Task 1.3: Vertex & edge types + `Pattern`

**Files:**
- Create: `crates/strider-pattern/src/pattern/{vertex,edge,mod}.rs`
- Test: inline in `pattern/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::NodeKind;
    #[test]
    fn builds_bipartite_add_shape() {
        let mut p = Pattern::new();
        let kx = p.add_node(PatNode::wildcard());
        let xout = p.add_output(kx, PatOutput::value(0));
        let kk = p.add_node(PatNode::exact(NodeKind::IntConst(1)));
        let kout = p.add_output(kk, PatOutput::value(0));
        let add = p.add_node(PatNode::exact(NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)));
        p.consume(add, 0, xout);
        p.consume(add, 1, kout);
        let _addout = p.add_output(add, PatOutput::value(0));
        p.set_root(add);
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
        assert!(p.root().is_some());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p strider-pattern --lib pattern::tests::builds_bipartite_add_shape 2>&1 | tail -5` → FAIL (undefined types).
- [ ] **Step 3: Implement `vertex.rs`**

```rust
use std::mem::Discriminant;
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};
use crate::matcher::Matcher;

pub enum KindSpec {
    Any, Variant(Discriminant<NodeKind>), Exact(NodeKind),
    VariantWith { discriminant: Discriminant<NodeKind>, check: Box<dyn Fn(&NodeKind) -> bool> },
}
impl KindSpec {
    pub fn discriminant(&self) -> Option<Discriminant<NodeKind>> {
        match self {
            Self::Any => None,
            Self::Variant(d) | Self::VariantWith { discriminant: d, .. } => Some(*d),
            Self::Exact(k) => Some(std::mem::discriminant(k)),
        }
    }
    pub fn matches(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Variant(d) => *d == std::mem::discriminant(kind),
            Self::Exact(k) => k == kind,
            Self::VariantWith { discriminant, check } => *discriminant == std::mem::discriminant(kind) && check(kind),
        }
    }
}

pub type LocalLimit = Box<dyn Fn(&Matcher, NodeId, NodeOutputType) -> bool>;
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, NodeOutputType, &crate::bindings::Bindings) -> bool>;

pub struct PatNode {
    pub kind: KindSpec,
    pub build: Option<crate::template::TemplateKind>,
    pub capture: Option<crate::capture::Capture>,
    pub node_limit: Option<LocalLimit>,
    pub post_match: Option<PostMatchFn>,
    pub force_ordered: bool,
}
impl PatNode {
    pub fn wildcard() -> Self { Self::from_kind(KindSpec::Any) }
    pub fn exact(k: NodeKind) -> Self { Self::from_kind(KindSpec::Exact(k)) }
    pub fn from_kind(kind: KindSpec) -> Self {
        Self { kind, build: None, capture: None, node_limit: None, post_match: None, force_ordered: false }
    }
}

pub enum OutputKindSpec { AnyValue, Value(Option<NodeOutputType>), Control, Memory, PhiToken }
pub struct PatOutput {
    pub slot: usize, pub kind: OutputKindSpec, pub width: Option<u32>,
    pub output_limit: Option<LocalLimit>, pub capture: Option<crate::capture::Capture>,
}
impl PatOutput {
    pub fn value(slot: usize) -> Self { Self { slot, kind: OutputKindSpec::Value(None), width: None, output_limit: None, capture: None } }
    pub fn control(slot: usize) -> Self { Self { slot, kind: OutputKindSpec::Control, width: None, output_limit: None, capture: None } }
}
```

Add a stub in `template/mod.rs` so `TemplateKind` resolves now (extended in Task 4.1):
```rust
pub enum TemplateKind { Exact(strider_ir::node::NodeKind) }
```

- [ ] **Step 4: Implement `edge.rs`**

```rust
#[derive(Clone, Copy, Debug)]
pub enum PatEdge { Produces, Consumes { slot: usize } }
```

- [ ] **Step 5: Implement `pattern/mod.rs`**

```rust
mod vertex; mod edge; mod topo;
pub use vertex::{KindSpec, LocalLimit, OutputKindSpec, PatNode, PatOutput, PostMatchFn};
pub use edge::PatEdge;
pub(crate) use topo::{reachable_topo, assert_dag};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use crate::matcher::CastMask;

pub enum PatVertex { Node(PatNode), Output(PatOutput) }

pub struct Pattern {
    pub(crate) inner: StableDiGraph<PatVertex, PatEdge>,
    pub(crate) root: Option<NodeIndex>,
    pub(crate) cast_mask: CastMask,
}
impl Default for Pattern { fn default() -> Self { Self::new() } }
impl Pattern {
    pub fn new() -> Self { Self { inner: StableDiGraph::new(), root: None, cast_mask: CastMask::empty() } }
    pub fn add_node(&mut self, n: PatNode) -> NodeIndex { self.inner.add_node(PatVertex::Node(n)) }
    pub fn add_output(&mut self, producer: NodeIndex, o: PatOutput) -> NodeIndex {
        let idx = self.inner.add_node(PatVertex::Output(o));
        self.inner.add_edge(producer, idx, PatEdge::Produces); idx
    }
    pub fn consume(&mut self, consumer: NodeIndex, slot: usize, output: NodeIndex) {
        self.inner.add_edge(output, consumer, PatEdge::Consumes { slot });
    }
    pub fn set_root(&mut self, n: NodeIndex) { self.root = Some(n); }
    pub fn root(&self) -> Option<NodeIndex> { self.root }
    #[must_use] pub fn ignore_casts_mask(mut self, m: CastMask) -> Self { self.cast_mask |= m; self }
    #[must_use] pub fn ignore_casts(self) -> Self { self.ignore_casts_mask(CastMask::all()) }
    pub fn node_count(&self) -> usize { self.inner.node_weights().filter(|v| matches!(v, PatVertex::Node(_))).count() }
    pub fn output_count(&self) -> usize { self.inner.node_weights().filter(|v| matches!(v, PatVertex::Output(_))).count() }
}
```

Add minimal skeletons so imports resolve: `matcher/mod.rs` with `pub use strider_ir::walk::CastMask;` and an empty `Matcher` placeholder (fleshed in Phase 2), and `pub mod template;` with the `TemplateKind` stub. In `lib.rs`: `pub mod pattern; pub mod matcher; pub mod template; pub mod bindings; pub mod capture;`.

- [ ] **Step 6: Run** `cargo test -p strider-pattern --lib pattern 2>&1 | tail -10` → PASS.
- [ ] **Step 7: Commit**

```bash
git add crates/strider-pattern/src/pattern crates/strider-pattern/src/lib.rs crates/strider-pattern/src/matcher crates/strider-pattern/src/template
git commit -m "feat(strider-pattern): bipartite Pattern representation (node/output vertices)"
git push origin develop
```

### Task 1.4: topo via library algorithm

**Files:**
- Create: `crates/strider-pattern/src/pattern/topo.rs`
- Test: inline

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{Pattern, PatNode, PatOutput};
    #[test]
    fn reachable_topo_orders_producers_before_consumers() {
        let mut p = Pattern::new();
        let a = p.add_node(PatNode::wildcard());
        let ao = p.add_output(a, PatOutput::value(0));
        let b = p.add_node(PatNode::wildcard());
        p.consume(b, 0, ao);
        p.set_root(b);
        let order = reachable_topo(&p.inner, p.root.unwrap()).unwrap();
        let pa = order.iter().position(|&n| n == a).unwrap();
        let pb = order.iter().position(|&n| n == b).unwrap();
        assert!(pa < pb);
    }
}
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement using petgraph algorithms**

```rust
use anyhow::anyhow;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::{DfsPostOrder, Reversed, Walker};
use crate::pattern::{PatEdge, PatVertex};

pub(crate) fn reachable_topo(g: &StableDiGraph<PatVertex, PatEdge>, root: NodeIndex) -> anyhow::Result<Vec<NodeIndex>> {
    let reachable: std::collections::HashSet<NodeIndex> =
        DfsPostOrder::new(Reversed(g), root).iter(Reversed(g)).collect();
    let sorted = petgraph::algo::toposort(g, None)
        .map_err(|c| anyhow!("PatGraph cycle at {:?}", c.node_id()))?;
    Ok(sorted.into_iter().filter(|n| reachable.contains(n)).collect())
}
pub(crate) fn assert_dag(g: &StableDiGraph<PatVertex, PatEdge>, root: NodeIndex) -> anyhow::Result<()> {
    reachable_topo(g, root).map(|_| ())
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): reachable topo via petgraph::algo::toposort"` + push.

---

## Phase 2 — Imperative builder + bipartite matcher engine

### Task 2.1: `MatcherBuilder` refs + primitives

**Files:**
- Create: `crates/strider-pattern/src/builder/{refs,mod}.rs`
- Test: inline in `builder/mod.rs`

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::{IntBinaryOp, node::NodeKind};
    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::pattern::KindSpec::Any);
        let k = b.leaf(crate::pattern::KindSpec::Exact(NodeKind::IntConst(1)));
        let sum = b.binary(IntBinaryOp::Add, x, k);
        let p = b.finish(sum);
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
    }
}
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement `refs.rs`**

```rust
use petgraph::stable_graph::NodeIndex;
#[derive(Clone, Copy)] pub struct PatOutRef(pub(crate) NodeIndex);
#[derive(Clone, Copy)] pub struct PatNodeRef(pub(crate) NodeIndex);
```

- [ ] **Step 4: Implement `builder/mod.rs`** (MatcherBuilder primitives + annotators; full body)

```rust
mod refs;
pub use refs::{PatNodeRef, PatOutRef};

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, NodeOutputType};
use crate::pattern::{KindSpec, OutputKindSpec, PatEdge, PatNode, PatOutput, PatVertex, Pattern};

pub struct MatcherBuilder { pub(crate) p: Pattern }
impl Default for MatcherBuilder { fn default() -> Self { Self::new() } }
impl MatcherBuilder {
    pub fn new() -> Self { Self { p: Pattern::new() } }
    pub fn leaf(&mut self, kind: KindSpec) -> PatOutRef {
        let n = self.p.add_node(PatNode::from_kind(kind));
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }
    pub fn unary(&mut self, kind: KindSpec, inner: PatOutRef) -> PatOutRef {
        let n = self.p.add_node(PatNode::from_kind(kind));
        self.p.consume(n, 0, inner.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }
    pub fn binary(&mut self, op: IntBinaryOp, l: PatOutRef, r: PatOutRef) -> PatOutRef {
        let n = self.p.add_node(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.p.consume(n, 0, l.0); self.p.consume(n, 1, r.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef { PatNodeRef(self.p.add_node(PatNode::from_kind(kind))) }
    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatOutRef) { self.p.consume(node.0, slot, prod.0); }
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::value(slot)))
    }
    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::control(slot)))
    }
    // annotators
    fn producing_node_idx(&self, out: NodeIndex) -> NodeIndex {
        self.p.inner.edges_directed(out, petgraph::Incoming)
            .find(|e| matches!(e.weight(), PatEdge::Produces)).map(|e| e.source()).expect("output has a producer")
    }
    pub(crate) fn node_of(&mut self, out: PatOutRef) -> &mut PatNode {
        let pi = self.producing_node_idx(out.0);
        match self.p.inner.node_weight_mut(pi) { Some(PatVertex::Node(n)) => n, _ => unreachable!() }
    }
    pub(crate) fn out_of(&mut self, out: PatOutRef) -> &mut PatOutput {
        match self.p.inner.node_weight_mut(out.0) { Some(PatVertex::Output(o)) => o, _ => unreachable!() }
    }
    pub fn set_output_ty(&mut self, out: PatOutRef, ty: NodeOutputType) { self.out_of(out).kind = OutputKindSpec::Value(Some(ty)); }
    pub fn set_output_width(&mut self, out: PatOutRef, bits: u32) { self.out_of(out).width = Some(bits); }
    pub fn capture_node(&mut self, out: PatOutRef, c: crate::capture::Capture) { self.node_of(out).capture = Some(c); }
    pub fn set_node_limit(&mut self, out: PatOutRef, f: crate::pattern::LocalLimit) { self.node_of(out).node_limit = Some(f); }
    pub fn set_post_match(&mut self, out: PatOutRef, f: crate::pattern::PostMatchFn) { self.node_of(out).post_match = Some(f); }
    pub fn set_force_ordered(&mut self, out: PatOutRef) { self.node_of(out).force_ordered = true; }
    /// Set width on every value input consumed by `out`'s producing node (inputs_of_width).
    pub fn constrain_input_widths(&mut self, out: PatOutRef, bits: u32) {
        let node = self.producing_node_idx(out.0);
        let input_outputs: Vec<NodeIndex> = self.p.inner.edges_directed(node, petgraph::Incoming)
            .filter(|e| matches!(e.weight(), PatEdge::Consumes { .. })).map(|e| e.source()).collect();
        for io in input_outputs {
            if let Some(PatVertex::Output(o)) = self.p.inner.node_weight_mut(io) { o.width = Some(bits); }
        }
    }
    pub fn finish(mut self, root: PatOutRef) -> Pattern {
        let producer = self.producing_node_idx(root.0);
        self.p.set_root(producer);
        crate::pattern::assert_dag(&self.p.inner, producer).expect("builder produced a DAG");
        self.p
    }
    pub fn finish_node(mut self, root: PatNodeRef) -> Pattern {
        self.p.set_root(root.0);
        crate::pattern::assert_dag(&self.p.inner, root.0).expect("builder produced a DAG");
        self.p
    }
}
```

- [ ] **Step 5: Run** `cargo test -p strider-pattern --lib builder 2>&1 | tail -5` → PASS.
- [ ] **Step 6: Commit** `git commit -am "feat(strider-pattern): MatcherBuilder primitives over bipartite graph"` + push.

### Task 2.2: Matcher skeleton + kind index

**Files:**
- Modify: `crates/strider-pattern/src/matcher/mod.rs`

- [ ] **Step 1: Implement the matcher skeleton** — port `Matcher<'f>`, `KindIndex`, `try_new`, `entry`, `function`, `find_all`/`find_first`/`match_at`/`find_joined`/`find_all_multi`, `function_arg*`, `ArgSource`, `FunctionArgHandle`, `prefix_agrees` **verbatim** from the current `crates/strider-pattern/src/matcher/mod.rs`, with these changes:
  1. Delete `MatcherOptions`, `ignore_casts`, `ignore_casts_mask`, the `options` field/accessor. The cast mask now lives on `Pattern`.
  2. The `Pattern` trait and `&dyn Pattern` parameters → take `&crate::pattern::Pattern` (the concrete type). `find_joined`/`find_all_multi` take `&[&crate::pattern::Pattern]`.
  3. `find_*` dispatch on `pat.root_kind_discriminant()` — add a free helper `fn root_kind_discriminant(p: &Pattern) -> Option<Discriminant<NodeKind>>` reading the root `PatNode`'s `kind.discriminant()`.
  4. The per-node match calls become `walk::try_match(self, pat, out_id, &mut bindings)` / `walk::try_match_node(...)` (Task 2.3).
- [ ] **Step 2: Run** `cargo build -p strider-pattern 2>&1 | tail -15` — errors only about not-yet-written `walk` / `Match`. Proceed to 2.3 (no commit yet — checkpoint).

### Task 2.3: `Match` result + bipartite walk engine

**Files:**
- Create: `crates/strider-pattern/src/match_result.rs`
- Create: `crates/strider-pattern/src/matcher/walk.rs`
- Test: `crates/strider-pattern/tests/engine_smoke.rs`

- [ ] **Step 1: Failing integration test**

```rust
use strider_ir::{IntBinaryOp, node::NodeOutputType};
use strider_ir_test_utils::make_empty_fn;
use strider_pattern::{builder::MatcherBuilder, pattern::KindSpec, Matcher};

#[test]
fn matches_add_const_via_builder() {
    let f = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, NodeOutputType::I64)?;
        let k = b.build_int_const(1u64, NodeOutputType::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, NodeOutputType::I64)
    }).unwrap();
    let mut mb = MatcherBuilder::new();
    let x = mb.leaf(KindSpec::Any);
    let k = mb.leaf(KindSpec::Any);
    let sum = mb.binary(IntBinaryOp::Add, x, k);
    let pat = mb.finish(sum);
    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).len(), 1);
}
```

- [ ] **Step 2: Run** `cargo test -p strider-pattern --test engine_smoke 2>&1 | tail -10` → FAIL.
- [ ] **Step 3: Implement `match_result.rs`** — port `Match` verbatim from the current `crates/strider-pattern/src/match_result.rs` (public `node`/`output`/`asm_fingerprint`/typed extractors are consumer API; don't change shape).
- [ ] **Step 4: Implement `walk.rs`** — bipartite recursive matcher. Port the algorithm from current `matcher/try_match.rs`, adapting node→node to node→output→node. Key helpers + the constraint check:

```rust
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::node::{NodeId, NodeOutputId, NodeOutputKind, NodeOutputType};
use strider_ir::Function;
use crate::bindings::{Binding, Bindings};
use crate::matcher::{Matcher, skip_casts};
use crate::pattern::{OutputKindSpec, PatEdge, PatOutput, PatVertex, Pattern};

fn output_ok(o: &PatOutput, f: &Function, out: NodeOutputId) -> bool {
    let val = f.output_kind(out).as_value();
    let kind_ok = match &o.kind {
        OutputKindSpec::AnyValue => val.is_some(),
        OutputKindSpec::Value(Some(ty)) => val == Some(*ty),
        OutputKindSpec::Value(None) => val.is_some(),
        OutputKindSpec::Control => matches!(f.output_kind(out), NodeOutputKind::Control),
        OutputKindSpec::Memory => matches!(f.output_kind(out), NodeOutputKind::Memory),
        OutputKindSpec::PhiToken => matches!(f.output_kind(out), NodeOutputKind::PhiToken),
    };
    kind_ok && o.width.map_or(true, |w| val.map_or(false, |t| t.bit_width() == w as usize))
}

// For a consumer PatNode index, collect (consumer_slot, producer_output_vertex, producer_node).
// Incoming Consumes edges: source = a PatOutput vertex; that vertex's Incoming Produces edge
//   source = the producer PatNode (the sub-pattern root).
// try_match(pat, pat_node_idx, matcher, ir_node, root_out, bindings) mirrors current try_match_at:
//   1. kind.matches; 2. node_limit; 3. for each input: output_ok on the IR producer output +
//      output_limit, then recurse into producer PatNode; commutative retry via mark/restore;
//      cast walk-through via skip_casts(matcher, producer_out, pat.cast_mask) on mismatch;
//   4. bind node.capture; 5. node.post_match. Return bool.
```

Implement `pub(crate) fn try_match(matcher, pat, root_out, bindings) -> bool` and `try_match_node(...)` as the engine entries called by `Matcher::find_*`. Port `skip_casts` from current `matcher/cast_walk_through.rs` into `matcher/` (reading the mask passed in, not from the matcher).

- [ ] **Step 5: Run** `cargo test -p strider-pattern --test engine_smoke 2>&1 | tail -10` → PASS.
- [ ] **Step 6: Commit**

```bash
git add crates/strider-pattern/src/match_result.rs crates/strider-pattern/src/matcher crates/strider-pattern/tests/engine_smoke.rs crates/strider-pattern/src/lib.rs
git commit -m "feat(strider-pattern): bipartite match engine + Match result"
git push origin develop
```

### Task 2.4: commutative + cast walk-through parity

**Files:**
- Test: `crates/strider-pattern/tests/engine_smoke.rs` (extend)

- [ ] **Step 1: Add tests** for (a) `add(any, const)` matching IR `Add(const, any)` via commutative swap, (b) cast walk-through when `pattern.ignore_casts_mask(EXTEND)` set, (c) `force_ordered` disabling swap — mirroring current `tests/pattern_matching/commutativity.rs` and `tests/cast_walk_through.rs` assertions, built via `MatcherBuilder` + annotators.
- [ ] **Step 2: Run** → FAIL where unhandled.
- [ ] **Step 3: Fix engine** until green.
- [ ] **Step 4: Commit** `git commit -am "test(strider-pattern): commutative + cast walk-through engine parity"` + push.

---

## Phase 3 — Typed builder structs (MatchPat) + free functions

### Task 3.1: `MatchPat` trait + combinators + first structs

**Files:**
- Create: `crates/strider-pattern/src/match_pat.rs`
- Create: `crates/strider-pattern/src/typed/{mod,value_ops,consts,wildcards}.rs`
- Test: `crates/strider-pattern/tests/typed_match.rs`

- [ ] **Step 1: Failing test** (mirrors the verified spike)

```rust
use strider_pattern::{add, var, int_const, Capture, Matcher, CaptureExt};
use strider_ir::{IntBinaryOp, node::NodeOutputType};
use strider_ir_test_utils::make_empty_fn;
#[test]
fn typed_add_matches_and_captures() {
    let f = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, NodeOutputType::I64)?;
        let k = b.build_int_const(1u64, NodeOutputType::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, NodeOutputType::I64)
    }).unwrap();
    let c = Capture::new();
    let pat = add(var(c), int_const(1u128)).into_pattern();
    let hits = Matcher::try_new(&f).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement `match_pat.rs`** (trait + combinators)

```rust
use crate::builder::{MatcherBuilder, PatOutRef};
use crate::pattern::Pattern;

pub trait MatchPat: Sized {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef;
    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let root = self.compile(&mut b);
        b.finish(root)
    }
}

pub struct Captured<P> { pub(crate) inner: P, pub(crate) cap: crate::capture::Capture }
impl<P: MatchPat> MatchPat for Captured<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef { let o = self.inner.compile(b); b.capture_node(o, self.cap); o }
}
pub struct Limited<P, F> { pub(crate) inner: P, pub(crate) f: F }
impl<P: MatchPat, F> MatchPat for Limited<P, F>
where F: Fn(&crate::Matcher, strider_ir::node::NodeId, strider_ir::node::NodeOutputType) -> bool + 'static {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef { let o = self.inner.compile(b); b.set_node_limit(o, Box::new(self.f)); o }
}
pub struct Guarded<P, F> { pub(crate) inner: P, pub(crate) f: F }
impl<P: MatchPat, F> MatchPat for Guarded<P, F>
where F: Fn(&crate::Matcher, strider_ir::node::NodeOutputType, &crate::Bindings) -> bool + 'static {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let o = self.inner.compile(b);
        b.set_post_match(o, Box::new(move |m, _node, ty, bnd| (self.f)(m, ty, bnd))); o
    }
}
pub struct Ordered<P> { pub(crate) inner: P }
impl<P: MatchPat> MatchPat for Ordered<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef { let o = self.inner.compile(b); b.set_force_ordered(o); o }
}

pub trait CaptureExt: MatchPat {
    fn capture(self, c: crate::capture::Capture) -> Captured<Self> { Captured { inner: self, cap: c } }
    fn when_match<F>(self, f: F) -> Guarded<Self, F>
        where F: Fn(&crate::Matcher, strider_ir::node::NodeOutputType, &crate::Bindings) -> bool + 'static { Guarded { inner: self, f } }
    fn filter<F>(self, f: F) -> Limited<Self, F>
        where F: Fn(&crate::Matcher, strider_ir::node::NodeId, strider_ir::node::NodeOutputType) -> bool + 'static { Limited { inner: self, f } }
    fn ordered(self) -> Ordered<Self> { Ordered { inner: self } }
}
impl<P: MatchPat> CaptureExt for P {}
```

- [ ] **Step 4: Implement first structs** in `typed/` — `Any`, `Var`, `IntConst`, `Add` (exact code in spec §5 / the spike). Free fns `any()`, `var(c)`, `int_const(v: impl Into<u128>)`, `add(l, r)`.
- [ ] **Step 5: Run** `cargo test -p strider-pattern --test typed_match 2>&1 | tail -10` → PASS.
- [ ] **Step 6: Commit** `git commit -am "feat(strider-pattern): MatchPat typed structs + combinators (Add/Var/Any/IntConst)"` + push.

### Task 3.2: Full value-op / cast / cmp / unary / const / wildcard family

**Files:**
- Modify: `crates/strider-pattern/src/typed/*.rs`
- Test: `crates/strider-pattern/tests/typed_match.rs` (extend)

- [ ] **Step 1: Write tests** (≈6 patterns per test fn) covering the full consumer surface — list (cross-checked against the consumer inventory):
  - binary int: `mul, sub, div, sdiv, rem, srem, and, or, xor, shl, shr, sshr`
  - unary: `neg, popcount, lzcount`, `bit_not`, `not_`
  - cast: `truncate, zero_extend, sign_extend, int_to_float, float_to_int, int_bits_to_float, float_bits_to_int, float_to_float, extend(op,_)`
  - int cmp: `int_eq, int_lt, int_slt, int_sborrow, int_carry, int_scarry, int_cmp(op,_,_)`
  - float: `float_add, float_sub, float_mul, float_div, float_neg, float_eq, float_lt, float_is_nan, float_cmp(op,_,_)`
  - bool: `bool_and, bool_or, bool_xor, bool_not, bool_binary(op,_,_)`
  - consts: `signed_int_const, int_const_any_of, bool_const, float_const, any_int_const(c), any_bool_const(c), any_float_const(c)`
  - variant-agnostic: `int_binary(op,_,_), int_binary_any, int_unary_any, int_cmp_any, float_binary_any, float_unary_any, float_cmp_any`
  - wildcards: `predicate, value_of_width, bool_value, inputs_of_width, bool_inputs, initial_var, initial_var_for`
- [ ] **Step 2: Run** → FAIL (most undefined).
- [ ] **Step 3: Implement** via mechanical transformation from the old builders (read `crates/strider-pattern/src/builders/*.rs` for each one's exact `KindSpec`/lowering):
  - Binary op `name`: `struct Name<L,R>{lhs,rhs}` + `impl MatchPat where L,R: MatchPat` → `b.binary(IntBinaryOp::Variant, l, r)`; free fn `name(l,r)`. `sub` builds `add(l, neg(r))`.
  - `int_binary(op,l,r)`: `IntBinary<L,R>{op,lhs,rhs}`.
  - `*_any`: `MatchPat`-only structs using `KindSpec::Variant(disc)`, no build path; variant recovered via `Match::get_*` (unchanged).
  - Unary/cast: `Name<I>{inner}` + `impl where I: MatchPat` → `b.unary(KindSpec::Exact(kind), inner)`.
  - Cmp: like binary, `KindSpec::Exact(NodeKind::IntCmpOp(op))` etc.; commutativity stays data-driven.
  - Consts: leaf structs (`KindSpec::Exact`/`Variant`); `any_*` add a capture.
  - `value_of_width(n)`/`bool_value()`: leaf `Any` whose **output vertex** gets `set_output_width(out, n)` — declarative, not a closure.
  - `inputs_of_width(n, inner)`/`bool_inputs(inner)`: wrap `inner`, call `constrain_input_widths(out, n)` (Task 2.1 helper). Preserve old semantics (every value input width == n, ≥1 value input, root has a value output) — add a `node_limit` checking "≥1 value input & has value output" alongside the declarative width.
  - `predicate(f)`: `any().when_match(move |m, ty, _| f(m, ty))`.
  - `bool_*` ops pin output to `I1`: after `b.binary(...)`, `set_output_ty(out, I1)` + a node_limit guarding I1 (mirror old `bool_ops.rs`).
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): full typed value-op/cast/cmp/const/wildcard family"` + push.

---

## Phase 4 — Templates, TemplatePat, rewrite

### Task 4.1: `TemplateKind` + `TemplateBuilder` + `TemplatePat` + `instantiate`

**Files:**
- Modify: `crates/strider-pattern/src/template/mod.rs`, `builder/mod.rs`
- Create: `crates/strider-pattern/src/template_pat.rs`
- Test: `crates/strider-pattern/tests/template_build.rs`

- [ ] **Step 1: Failing test** — match `add(var(x), int_const(1))`, instantiate `add(var(x), int_const(2))` as fresh IR, assert a new Add output exists redirecting correctly. (Mirror old `tests/template_basics.rs`.)
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Extend `TemplateKind`** + port `TemplateCtx` (from current `template/ctx.rs`) + add `TemplateTy { InheritRoot, Fixed(NodeOutputType) }`.
- [ ] **Step 4: Implement `TemplateBuilder`** in `builder/mod.rs`: same primitive surface as `MatcherBuilder`, but populating `PatNode.build = Some(TemplateKind::Exact(kind))` + recording `TemplateTy`. Factor shared graph-wiring into a private helper both builders call (DRY) — e.g. an inner `fn binary_raw(&mut p, op, l, r, build: Option<TemplateKind>)`.
- [ ] **Step 5: Implement `template_pat.rs`**

```rust
pub trait TemplatePat: Sized {
    fn compile(self, b: &mut crate::builder::TemplateBuilder) -> crate::builder::PatOutRef;
    fn into_template(self) -> crate::pattern::Pattern {
        let mut b = crate::builder::TemplateBuilder::new();
        let root = self.compile(&mut b); b.finish(root)
    }
}
// impl for Var (capture resolves at instantiate), IntConst, FloatConst, BoolConst, and the value-op
// structs where their operands: TemplatePat. Captured<P>: TemplatePat where P: TemplatePat.
// Guarded<P,F> / Limited<P,F> / Any / *_any: do NOT impl TemplatePat.
```

- [ ] **Step 6: Implement `instantiate`** in `template/mod.rs` — port `instantiate_pat_graph` from current code, adapted to the bipartite store and `reachable_topo`. Capture-only nodes resolve via `Bindings::get_output`; buildable nodes call `function.create_node(kind, inputs_by_slot, [OutputType(ty)])`, collecting inputs by walking the node's `Consumes` edges and reading the materialised producer output of each input vertex's producer node.
- [ ] **Step 7: Run** → PASS.
- [ ] **Step 8: Commit** `git commit -am "feat(strider-pattern): TemplateBuilder + TemplatePat + instantiate (toposort)"` + push.

### Task 4.2: `rewrite_rule` + runtime variant + RewriteCtx/GraphRewriter + fingerprints

**Files:**
- Create: `crates/strider-pattern/src/rewrite/mod.rs`
- Test: `crates/strider-pattern/tests/rewrite_build.rs`

- [ ] **Step 1: Failing tests** — (a) `rewrite_rule(add(var(x), int_const(0)), var(x))` fires & redirects uses; (b) buildable-RHS path compiles (the spike already proved a wildcard RHS fails to compile; document with a `// compile-fail:` comment, plus a `trybuild` ui test if the harness is added).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement `rewrite/mod.rs`** — port `RewriteCtx`/`RewriteCtxView`/`GraphRewriter`/`BoxedRule`/`boxed_rule`/`apply_rules_in_order`/`GraphRewriteCtxExt`/`absorb_fingerprints_into_fresh_subtree` **verbatim** from current `rewrite/mod.rs`. Replace the rule constructors:

```rust
pub fn rewrite_rule<L: MatchPat + 'static, T: TemplatePat + 'static>(lhs: L, rhs: T)
    -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static {
    let lhs_pat = lhs.into_pattern();
    let rhs_tpl = rhs.into_template();
    check_capture_coverage(&lhs_pat, &rhs_tpl).expect("rewrite_rule: RHS capture not bound by LHS");
    rewrite_rule_impl(lhs_pat, rhs_tpl)
}
pub fn rewrite_rule_runtime(lhs: Pattern, rhs: Pattern)
    -> Result<impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static> {
    assert_buildable(&rhs)?;           // every reachable PatNode: build.is_some() || capture.is_some()
    check_capture_coverage(&lhs, &rhs)?;
    Ok(rewrite_rule_impl(lhs, rhs))
}
```

`rewrite_rule_impl(lhs: Pattern, rhs: Pattern)` = shared body (match → instantiate → fingerprint absorb → `replace_all_uses`), with `RewriteSkip` handling preserved. `check_capture_coverage`/`assert_buildable` walk the bipartite store's `PatNode` weights.

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): compile-time rewrite_rule + runtime FFI variant + fingerprint absorption"` + push.

### Task 4.3: `*_const_with!` macros

**Files:**
- Modify: `crates/strider-pattern/src/macros_impl.rs`
- Test: `crates/strider-pattern/tests/rewrite_build.rs` (extend)

- [ ] **Step 1: Failing test** — `rewrite_rule(add(any_int_const().capture(c1), any_int_const().capture(c2)), int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)))` folds two constants.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Port the macros** — expand to a `ConstWith` struct carrying a `TemplateKind::Fn` closure that reads captures from `ctx.bindings` and returns `IntConst`/`FloatConst`. `ConstWith: TemplatePat`. Grammar `([c: uint, ...] => expr)` unchanged. Provide `int_const_with_fn`/`bool_const_with_fn`/`float_const_with_fn` constructors.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): port *_const_with! macros to typed templates"` + push.

---

## Phase 5 — Control & variadic builders

### Task 5.1: `call` / `call_other` / `ret`

**Files:**
- Create: `crates/strider-pattern/src/control/mod.rs`
- Test: `crates/strider-pattern/tests/control_build.rs`

- [ ] **Step 1: Failing tests** — `call().at(addr)`, `call().arg(0, var(x))`, `ret().ret_val(0, var(x))`, `call_other().name("foo")` each `find_all` to the expected count (mirror current control-flow tests).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — fluent builders over `MatcherBuilder::node`/`input`, keeping the **exact slot conventions + hook routing** from current `builders/control.rs`: Call `[ctrl,mem,target,args+3]`; CallOther raw slots + `.name` via node-local `LocalLimit` (`call_other_name`); Ret `[ctrl,mem,retval+2]`. Zero-value-output roots → `MatcherBuilder::finish_node`. Replace `From<…> for Pat<Wildcard>` with `.build() -> Pattern`; `.capture(c)` sets the node capture before finishing. These return `Pattern` directly (control patterns are builder-only per the design boundary).
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): call/call_other/ret control builders"` + push.

### Task 5.2: `if_node` with two control outputs

**Files:**
- Modify: `crates/strider-pattern/src/control/mod.rs`
- Test: `crates/strider-pattern/tests/control_build.rs` (extend)

- [ ] **Step 1: Failing tests** — `if_node().cond(var(c))` matches an `If`; `if_node().cond(_).with_true(ret())` matches only when the true-branch's single consumer is a `Return`. White-box assert the `If` node carries **two control output vertices** (representation invariant). Mirror current `tests/pattern_matching/control_flow.rs`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `IfPat`: create the `If` node; add `control_output(node, 0)` (true) + `control_output(node, 1)` (false) as real vertices. `.cond(p)` wires input slot 1. `.with_true(q)`/`.with_false(r)` attach a following-control consumer to the respective control output vertex; matching follows the IR If's Control output to its single consumer (port `match_branch_consumer`), now anchored on the control output vertex. Keep "single-consumer-or-no-match".
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(strider-pattern): if_node with first-class true/false control output vertices"` + push.

### Task 5.3: `phi` / `mem_phi` / `value_phi`

- [ ] **Steps 1–5:** port from current `builders/phi.rs` (`phi().for_vn(vn).input(i, p)`, `mem_phi`, `value_phi`) over `MatcherBuilder::node`/`input`, preserving phi slot/tag semantics. TDD (`tests/control_build.rs`) + commit + push.

### Task 5.4: `function_arg` family

- [ ] **Steps 1–5:** port `function_arg`/`function_arg_any`/`function_arg_reg`/`function_arg_stack` from current `builders/function_arg.rs`, preserving `arg_index_to_nodes` side-table semantics + the Register/Stack enum-dispatch source. TDD + commit + push.

---

## Phase 6 — lib surface + dead-code sweep

### Task 6.1: `lib.rs` public surface + delete superseded modules

**Files:**
- Modify: `crates/strider-pattern/src/lib.rs`; delete old modules.

- [ ] **Step 1:** Re-export the new API: `add`, `var`, `int_const`, … (all free fns), `Matcher`, `Match`, `Pattern`, `Capture`, `Bindings`, `MatchPat`, `TemplatePat`, `CaptureExt`, builder types (`MatcherBuilder`, `TemplateBuilder`, `PatOutRef`, `PatNodeRef`), control builders, `rewrite_rule`, `rewrite_rule_runtime`, `GraphRewriter`, `RewriteCtx`, `RewriteCtxView`, `BoxedRule`, `boxed_rule`, `apply_rules_in_order`, `GraphRewriteCtxExt`, `CastMask`, the IR op enums, the `*_const_with!` macros. Remove from the surface: `rewrite_rule_dynamic`, `Role`/`Wildcard`/`Concrete`/`Combine`, `Pat`, `IntoPat`, `Capture::named`/`.cap`.
- [ ] **Step 2:** Delete superseded modules: `pat_graph/`, `builders/`, the old `matcher/try_match.rs`, `matcher/cast_walk_through.rs` (if its logic moved), any `#[allow(dead_code)]`/`#[allow(unused_imports)]` scaffolds no longer needed.
- [ ] **Step 3: Run** `cargo build -p strider-pattern 2>&1 | tail -20` + `cargo clippy -p strider-pattern --all-targets 2>&1 | tail -20` → clean (workspace lints are `-D warnings`).
- [ ] **Step 4: Commit** `git commit -am "refactor(strider-pattern): finalize public surface; drop superseded modules + dead-code allows"` + push.

---

## Phase 7 — Port the strider-pattern test suite (the contract)

### Task 7.1: migrate `tests/` to the new API

**Files:**
- Modify: every file under `crates/strider-pattern/tests/` (read each first).

- [ ] **Step 1:** Rewrite pattern construction to the new typed/builder API per file (names mostly identical; drop `Pat<Wildcard>` annotations; cast mask moves from `Matcher::ignore_casts*` to `pattern.ignore_casts_mask`). Keep every assertion. A test that cannot be expressed → STOP and escalate (design gap), don't delete.
- [ ] **Step 2: Run** `cargo test -p strider-pattern 2>&1 | tail -25` → ALL PASS (lib + every integration test).
- [ ] **Step 3:** `cargo clippy -p strider-pattern --all-targets 2>&1 | tail -10` → clean.
- [ ] **Step 4: Commit** `git commit -am "test(strider-pattern): port full suite to new typed API"` + push.

**Phase 7 gate:** `strider-pattern` fully green and self-contained.

---

## Phase 8 — Migrate strider-analyze

> Build/test only `-p strider-analyze` for signal. Free-function names are stable, so most call sites change only where they used `Matcher::ignore_casts*`, `when_match` signatures, or `rewrite_rule_dynamic`.

### Task 8.1: ConstantFold rules

**Files:**
- Modify: `crates/strider-analyze/src/opt/constant_fold/rules.rs` (+ submodules)
- Test: `cargo test -p strider-analyze constant_fold`

- [ ] **Step 1:** Apply mechanical transforms:
  - builder calls (`add`, `any_int_const().capture(c)`, …) — unchanged (names stable).
  - `.when_match(move |ctx, ty, b| …)` — closure now receives `(&Matcher, NodeOutputType, &Bindings)`; bodies using `b.get(x)` / `ctx.function()` are unchanged (the `Matcher` *is* the data-access handle). Rename the first param if it was typed as a view.
  - `rewrite_rule(lhs, rhs)` — same shape; RHS must be `TemplatePat` (these RHSs already are concrete). A wildcard RHS now fails to compile (desired).
  - `int/bool/float_const_with!`, `boxed_rule`, `apply_rules_in_order` — unchanged.
- [ ] **Step 2: Build** `cargo build -p strider-analyze 2>&1 | tail -30` — fix errors file-by-file.
- [ ] **Step 3: Run** `cargo test -p strider-analyze constant_fold 2>&1 | tail -20` → PASS.
- [ ] **Step 4: Verify fixture output unchanged** — the constant_fold unit tests assert specific folded shapes; they must pass unchanged. If a golden node-count exists, diff it.
- [ ] **Step 5: Commit** `git commit -am "refactor(strider-analyze): migrate ConstantFold to new pattern API"` + push.

### Task 8.2: indirect-branch resolver

**Files:**
- Modify: `crates/strider-analyze/src/opt/indirect_branch_resolve/{jump_table,stack_array,inplace}.rs`
- Test: `cargo test -p strider-analyze indirect`

- [ ] **Step 1:** Migrate `match_at` + `get_uint`/`output`/`get_int_cmp_op` sites (names stable); move `Matcher::ignore_casts*` onto the pattern.
- [ ] **Step 2: Build + Run** `cargo test -p strider-analyze indirect 2>&1 | tail -20` → PASS.
- [ ] **Step 3: Commit** + push.

### Task 8.3: FlagCmpCanonicalize + IfCondInversion + remaining

**Files:**
- Modify: `crates/strider-analyze/src/opt/flag_cmp_canonicalize/`, `if_cond_inversion/`, any other pattern users.
- Test: `cargo test -p strider-analyze`

- [ ] **Step 1:** Migrate the ~12 flag-cmp `rewrite_rule`s + if-cond-inversion `match_at`/`Pat<Concrete>` usage (drop explicit role types; the typed struct carries buildability).
- [ ] **Step 2: Build + Run** `cargo test -p strider-analyze 2>&1 | tail -25` → PASS (4 known baseline failures allowed; no new).
- [ ] **Step 3:** `cargo clippy -p strider-analyze --all-targets 2>&1 | tail -15` → clean.
- [ ] **Step 4: Commit** `git commit -am "refactor(strider-analyze): migrate remaining passes + tests to new pattern API"` + push.

**Phase 8 gate:** `cargo test -p strider-analyze` green (modulo known baseline).

---

## Phase 9 — Migrate strider-py + the proc-macro

### Task 9.1: retarget `strider-pattern-macros`

**Files:**
- Modify: `crates/strider-pattern-macros/src/lib.rs`

- [ ] **Step 1:** Change generated `finalise()` to lower onto the new builder layer: call the control/variadic builder or typed free fn, then `.build()`/`.into_pattern()` to a `Pattern`. Per-field `Option` accumulation model unchanged; only `finalise` body + return type (`Pattern`) change. `capture`/`cap` fields resolve to `strider_pattern::Capture` (Python interns strings → Capture *before* calling — see 9.2). Add the cast-mask setter call in `finalise` when configured.
- [ ] **Step 2: Build** `cargo build -p strider-pattern-macros 2>&1 | tail -15` → compiles.
- [ ] **Step 3: Commit** with 9.2 (the macro is exercised only through strider-py).

### Task 9.2: retarget `strider-py`

**Files:**
- Modify: `crates/strider-py/src/pattern.rs`, `function.rs`
- Test: `uv run pytest`

- [ ] **Step 1:**
  - `PyPat` stores `Rc<Pattern>` (was `Rc<Pat<Wildcard>>`).
  - Keep `intern_str` (it now owns the only `String → Capture` table).
  - Cast mask: fold onto the built `Pattern` (`pat.ignore_casts_mask(...)`) before `find_all`; `Matcher` no longer takes a mask.
  - Rewrite: `rewrite_rule_dynamic` → `rewrite_rule_runtime(lhs_pattern, rhs_pattern)`.
  - `wrap_when` → `when_match` with `(&Matcher, NodeOutputType, &Bindings)`; the `PyPartialMatch` proxy is unchanged.
- [ ] **Step 2: Build** `cargo build -p strider-py 2>&1 | tail -30` → fix errors.
- [ ] **Step 3: Wheel + Python tests**

```bash
cd crates/strider-py && uv sync --group dev && uv run maturin develop 2>&1 | tail -5 && uv run pytest 2>&1 | tail -25
```
Expected: PASS.

- [ ] **Step 4: Commit** `git commit -am "refactor(strider-py,macros): retarget bindings to new pattern API"` + push.

---

## Phase 10 — Workspace green + merge

### Task 10.1: full verification

- [ ] **Step 1:** `cargo build --workspace 2>&1 | tail -15` → clean.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets 2>&1 | tail -20` → clean.
- [ ] **Step 3:** Per-crate test sweep (avoid one giant `--workspace` run for signal clarity):
  `cargo test -p strider-pattern 2>&1 | tail -5`;
  `cargo test -p strider-analyze 2>&1 | tail -5`;
  `cargo test -p strider-py 2>&1 | tail -5`;
  `cd crates/strider-py && uv run pytest 2>&1 | tail -5`.
  Expected: green (4 known baseline failures allowed; **no new**).
- [ ] **Step 4:** End-to-end: `cargo run -p strider-analyze --example orchestrator_demo 2>&1 | tail -5` (needs `fixtures/out/x86/arithmetic.elf` per `fixtures/Makefile`).
- [ ] **Step 5: Commit** any final cleanup + push.

### Task 10.2: review, then merge to master

- [ ] **Step 1:** Use `superpowers:requesting-code-review` on the full `develop`-vs-`master` diff.
- [ ] **Step 2:** Address findings (TDD), commit, push.
- [ ] **Step 3:** Merge:

```bash
git checkout master && git merge --ff-only develop && git push origin master && git checkout develop
```
(If `--ff-only` fails because `master` advanced: rebase `develop` on `master`, re-run the per-crate sweep, then merge.)

- [ ] **Step 4:** Update `CLAUDE.md`'s strider-pattern section for the new bipartite/typed API (via `claude-md-management:revise-claude-md`).

---

## Self-review — spec coverage

- Spec §4 (bipartite graph) → Phase 1.3–1.4, Phase 2.
- Spec §5 (typed MatchPat/TemplatePat) → Phase 3, Phase 4 (verified by the rustc spike).
- Spec §6 (imperative builders / SSoT) → Phase 2.1, Phase 4.1, Phase 5.
- Spec §7 (engine, per-node post-match, cast mask from pattern) → Phase 2.3–2.4, Phase 1.3 (`Pattern::ignore_casts_mask`).
- Spec §8 (captures opaque + Bindings truncate) → Phase 1.1–1.2.
- Spec §9 (templates/rewrites + fingerprints + rewrite_rule split) → Phase 4.
- Spec §10 (library algorithms) → Phase 1.4, Phase 4.1 instantiate.
- Spec §11 (consumer migration) → Phases 8–9.
- Spec §12 non-goals respected (no branch-swap rule; representation only) → Phase 5.2.
- Spec §13 testing strategy → Phase 7 (contract) + per-crate gates throughout.

Type-consistency check: builder methods (`leaf`/`unary`/`binary`/`node`/`input`/`value_output`/`control_output`/`set_output_ty`/`set_output_width`/`capture_node`/`set_node_limit`/`set_post_match`/`set_force_ordered`/`constrain_input_widths`/`finish`/`finish_node`) are named identically wherever referenced across Tasks 2.1, 3.1, 3.2, 5.x. `Pattern::ignore_casts_mask` (Task 1.3) is the same name used in Phases 6–8. `into_pattern`/`into_template`/`build` finalisers are consistent.
