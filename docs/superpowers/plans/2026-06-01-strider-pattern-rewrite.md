# strider-pattern: graph-based pattern crate replacing strider-analyze::pattern

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a new `strider-pattern` crate whose internal pattern representation is a sea-of-nodes DAG backed by `petgraph::StableDiGraph`, with `Pattern` (LHS / matchable) and `Template` (RHS / instantiable) traits both implemented by the same `PatGraph` value. Replace `strider-analyze::pattern` entirely. Today's chained builder API (`add(int_const(5), var(x))`) is preserved verbatim so the ~100 existing rewrite rules and ~20 matcher consumers don't change.

**Architecture:**

- `PatGraph` = `StableDiGraph<NodeData, EdgeData>` + a `root: Option<NodeIndex>` + a `captures: Vec<Capture>` table. `NodeData` carries `{ kind_spec, output_ty, capture: Option<CaptureRef>, post_match: Option<PostMatchFn>, build_spec: Option<BuildSpec> }`. `EdgeData` carries `{ consumer_slot, producer_output_slot }` so the typed-slot semantics that today live in `Vec<NodeInputId>` survive the petgraph encoding.
- **Ownership model:** `PatGraph` is owned by value — no `Arc`, no `Rc`, no `Send`/`Sync` bounds anywhere (per the C8 cleanup that established single-threaded ownership as the project default). Closures inside `NodeData` are `Box<dyn Fn>` (move-only). `Pat` is `struct Pat(PatGraph)` — a thin newtype around the graph, also move-only. Chained builders consume their children by value (`add` takes ownership of its sub-patterns and merges them into the parent). Pattern values can be moved into a `Rewrite` or borrowed by `Matcher::find_all(&pat)`, but never refcount-shared. If a future need arises to share one pattern across multiple rules, opt in to `Rc<PatGraph>` at that callsite rather than baking it into the crate.
- `Pattern` trait: `try_match(&self, function, root_out, bindings) -> bool` + `root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>>`. Implemented by `PatGraph<R>` for every role `R`.
- `Template` trait: `instantiate(&self, function, bindings, root_ty) -> Result<NodeOutputId>`. Implemented ONLY by `PatGraph<Concrete>` — so patterns that contain a kind-`Any` or a custom predicate (role `Wildcard`) are statically rejected when used as a template.
- **Role markers (phantom types):** `Wildcard` and `Concrete`. `PatGraph<R>` and `Pat<R>` carry the role as a phantom-type parameter. Leaf builders fix the role (`any()` → `Pat<Wildcard>`, `int_const(5)` → `Pat<Concrete>`, `var(c)` → `Pat<Concrete>`). Composing builders combine via a `Combine<Other>` trait whose meet table is: `Concrete + Concrete = Concrete`; anything-with-Wildcard = Wildcard. The role is purely zero-cost type-level bookkeeping; the runtime representation is unchanged.
- `Rewrite<L, T>::new(lhs: L, rhs: T)` where `L: Pattern` and `T: Template`. Because `Template` is implemented only for `Pat<Concrete>`, passing an `any()`-rooted pattern as RHS fails *at compile time* with a clean `the trait Template is not implemented for Pat<Wildcard>` error. Runtime checks at `new` time: root output types agree + every Capture referenced in the RHS appears in the LHS's bound-captures set.
- Chained builders (`add`, `int_const`, `var`, …) construct one-node `PatGraph`s; the binary-op / call / etc. wrappers merge child sub-graphs into the parent via a `merge_subgraph` helper that remaps `NodeIndex`. From the user's perspective: identical to today.
- Matching is a rooted topological match (NOT full VF2). Pattern is asserted DAG; matcher walks the pattern bottom-up from leaves to root, threading captured `NodeOutputId`s through `Bindings`, and short-circuits on first inconsistency. Capture-equality forces "same IR node both slots" without needing structural pattern-graph sharing.

**Tech Stack:** Rust 2024 edition, `petgraph` 0.8 (already a workspace dep — used by `strider-lift`'s cfg builder), `cranelift-entity` (for any small NodeId-keyed maps), `entity-utils::DenseEntitySet` (for matcher's visited sets — O(n) bookkeeping, per the workspace's "no std HashSet/HashMap for NodeId" convention), `rsleigh::Vn`, `strider-ir` IR types. Python mirror unchanged in shape — strider-pattern-macros emits PyO3 wrappers from the same `*Def` structs, just with re-pointed import paths.

---

## Design Decisions Locked

1. **Graph storage:** `petgraph::StableDiGraph<NodeData, EdgeData>`. Slot semantics live on edges (`consumer_slot`, `producer_output_slot`).
2. **Pattern / Template:** One graph type (`PatGraph<R>`) parametrised by a phantom role marker. `Pattern` is implemented for all `R`; `Template` only for `R = Concrete`. Compile-time rejection of `any()`-in-RHS via the type system.
3. **Role markers:** `Wildcard` (contains a kind-Any or custom predicate — only matchable) vs `Concrete` (every node has a build spec or capture — matchable AND buildable). Combined via the `Combine<Other>` trait; weaker role wins.
4. **Construction API:** Today's chained builder preserved. Internal graph storage; users never construct `PatGraph` directly. Builders return `Pat<R>` directly — today's `IntoPat` conversion trait is no longer needed.
5. **Ownership:** Single-threaded throughout — no `Arc`, no `Send`/`Sync`, no `Rc` in core types. Closures are `Box<dyn Fn>` (move-only). `Pat<R>` owns its `PatGraph<R>` by value. Builders consume sub-patterns by value (move) and merge into the parent. If callers later need to reuse a pattern across multiple rules they can opt in to `Rc<PatGraph<R>>` at their call site.
6. **Walk-through:** The matcher transparently walks through casts (`Truncate` / `Extend` / `ZeroExtend` / `SignExtend` / `IntBitsToFloat` / `FloatBitsToInt` / `FloatToFloat`) when the per-matcher `CastMask` opts the relevant cast in. The matcher does NOT walk through `Region` nodes — patterns that cross region boundaries must include the `Region` explicitly. `MatcherOptions::ignore_regions` and the Region-walk-through code path don't exist in this crate.
7. **Python API:** Mirror Rust 1:1 (strider-pattern-macros emits the mirror). Phantom roles disappear at the FFI boundary — the Python `Pat` class is monomorphic, runtime check at `Rewrite.__init__` catches `any()` on the RHS for Python callers.
8. **Pattern is DAG:** No cycles. `assert_dag` runs at `Pat::from_graph` time. Topological-walk match, not VF2.
9. **Branch:** `feature/strider-pattern-crate` off `rewrite/strider`. Each task = one commit. Final step ff-merges into `rewrite/strider`, deletes the branch local + remote. No plan/task identifiers in commit messages (per `feedback_no_plan_identifiers_in_code`).

---

## File Structure

```
crates/strider-pattern/                    NEW CRATE
├── Cargo.toml
├── src/
│   ├── lib.rs                              public re-exports
│   ├── capture.rs                          Capture, Bindings, Match
│   ├── pat_graph/
│   │   ├── mod.rs                          PatGraph<R> + traits-impl
│   │   ├── node_data.rs                    NodeData, EdgeData, KindSpec
│   │   ├── role.rs                         Wildcard / Concrete / Role / Combine
│   │   ├── topo.rs                         topological iteration + DAG-cycle assertion
│   │   └── merge.rs                        merge_subgraph (used by binary builders)
│   ├── matcher/
│   │   ├── mod.rs                          Matcher::try_new / find_all / match_at
│   │   ├── ctx.rs                          MatchCtx, BuildCtx
│   │   ├── try_match.rs                    PatGraph::try_match impl
│   │   ├── cast_walk_through.rs            cast walk-through only (no Region)
│   │   └── cast_mask.rs                    CastMask (ported)
│   ├── template/
│   │   └── mod.rs                          PatGraph::instantiate impl
│   ├── rewrite/
│   │   ├── mod.rs                          Rewrite, GraphRewriter, RewriteCtx
│   │   └── asm_fingerprint.rs              asm-fingerprint absorption (ported)
│   ├── builders/
│   │   ├── mod.rs                          re-exports of all chained builders
│   │   ├── int_ops.rs                      add / sub / mul / and / or / xor / cmps
│   │   ├── float_ops.rs                    float_add / float_mul / ...
│   │   ├── bool_ops.rs                     bool_and / bool_or / ...
│   │   ├── casts.rs                        truncate / sign_extend / zero_extend / ...
│   │   ├── consts.rs                       int_const / float_const / ...
│   │   ├── memory.rs                       load / store
│   │   ├── phi.rs                          phi / value_phi / mem_phi
│   │   ├── control.rs                      call / call_other / ret / if_node
│   │   ├── function_arg.rs                 function_arg / function_arg_reg / ...
│   │   ├── wildcards.rs                    any / var / value_of_width / inputs_of_width
│   │   └── predicate.rs                    when / ordered guards
│   └── error.rs                            Result + RewriteSkip sentinel
└── tests/
    ├── basic_match.rs                      single-node + simple binary match
    ├── capture_equality.rs                 add(var(x), var(x)) semantics
    ├── walk_through.rs                     cast + Region walk-through
    ├── rewrite_basics.rs                   apply a rewrite, verify asm-fingerprint
    ├── builders_smoke.rs                   one construction per builder family
    └── differential.rs                     same fixture, old vs new matcher → same Matches
```

`strider-analyze/src/pattern/` is deleted in Task 16. Its module re-exports get replaced with `pub use strider_pattern::*;` (or fully removed and consumers updated to use `strider_pattern::` directly — decided in Task 16 based on what produces the cleanest diff).

---

## Branch + Commit Conventions

```bash
git checkout -b feature/strider-pattern-crate rewrite/strider
```

Commit message style (no plan/task/phase identifiers — per `feedback_no_plan_identifiers_in_code`): lowercase imperative subject, focus on the WHY where non-obvious, trailing `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`. After each task: stage explicit paths only (never `git add -A`), commit, `git push origin feature/strider-pattern-crate`.

Per-task gates: `cargo build --workspace --release`, `cargo test --workspace --release` (zero failures), `cargo clippy --workspace --all-targets --release -- -D warnings`, and (from Task 18 on) `cd crates/strider-py && uv run maturin develop --release && uv run pytest -q`.

---

## Task 1: Bootstrap strider-pattern crate

**Files:**
- Create: `crates/strider-pattern/Cargo.toml`
- Create: `crates/strider-pattern/src/lib.rs`
- Modify: `Cargo.toml` (workspace root — add `strider-pattern` to `workspace.dependencies`)

- [ ] **Step 1: Create the crate directory + manifest**

```toml
# crates/strider-pattern/Cargo.toml
[package]
name = "strider-pattern"
version = "0.1.0"
edition = { workspace = true }

[dependencies]
strider-ir = { workspace = true }
strider-target = { workspace = true }
entity-utils = { workspace = true }
rsleigh = { workspace = true }
petgraph = { workspace = true }
cranelift-entity = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
smallvec = { workspace = true }

[dev-dependencies]
strider-ir-test-utils = { path = "../strider-ir-test-utils" }
expect-test = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Create the empty lib.rs**

```rust
// crates/strider-pattern/src/lib.rs
//! Sea-of-nodes pattern + template crate. Replaces `strider-analyze::pattern`.
//!
//! Internal representation: `PatGraph` backed by `petgraph::StableDiGraph`.
//! Public surface: chained builder free-functions (`add`, `int_const`, `var`,
//! …) plus the `Pattern` and `Template` traits implemented by `PatGraph`.

// Modules added in subsequent tasks.
```

- [ ] **Step 3: Register the crate in the workspace**

In root `Cargo.toml`, add to `[workspace.dependencies]` (alphabetical order):

```toml
strider-pattern = { path = "crates/strider-pattern" }
```

- [ ] **Step 4: Verify build + workspace tests still green**

```bash
cargo build --workspace --release
cargo test --workspace --release 2>&1 | grep -c FAILED  # expect: 0
cargo clippy --workspace --all-targets --release -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/strider-pattern/Cargo.toml crates/strider-pattern/src/lib.rs Cargo.toml
git commit -m "$(cat <<'EOF'
feat(strider-pattern): bootstrap empty crate

Add the new `strider-pattern` crate skeleton.  Subsequent commits land
PatGraph, the Pattern/Template traits, and the chained builders.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 2: PatGraph storage + DAG-aware topological iteration

**Files:**
- Create: `crates/strider-pattern/src/pat_graph/mod.rs`
- Create: `crates/strider-pattern/src/pat_graph/node_data.rs`
- Create: `crates/strider-pattern/src/pat_graph/topo.rs`
- Create: `crates/strider-pattern/src/pat_graph/merge.rs`
- Modify: `crates/strider-pattern/src/lib.rs` (add `mod pat_graph;`)

- [ ] **Step 1: Define NodeData / EdgeData / KindSpec**

`crates/strider-pattern/src/pat_graph/node_data.rs`:

```rust
use std::mem::Discriminant;
use strider_ir::node::{NodeKind, NodeOutputType};

/// Kind-level constraint on a pattern node (ported from
/// `strider_analyze::pattern::pat::node_pat::KindSpec`).
///
/// Closures are `Box<dyn Fn>` (move-only, single-threaded) per the
/// project's no-Send/Sync, no-Arc convention established in C8.
pub enum KindSpec {
    Any,
    Variant(Discriminant<NodeKind>),
    Exact(NodeKind),
    VariantWith {
        discriminant: Discriminant<NodeKind>,
        check: Box<dyn Fn(&NodeKind) -> bool>,
    },
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
            Self::VariantWith { discriminant, check } => {
                *discriminant == std::mem::discriminant(kind) && check(kind)
            }
        }
    }
}

/// Per-node payload stored as `StableDiGraph` node weight.
///
/// `NodeData` is move-only — the `Box<dyn Fn>` closures inside aren't
/// `Clone`.  `merge_subgraph` consumes a child graph by value via
/// `StableDiGraph::into_nodes_edges()` so cloning is never needed.
pub struct NodeData {
    pub kind: KindSpec,
    pub output_ty: Option<NodeOutputType>,
    pub capture: Option<crate::capture::CaptureRef>,
    pub post_match: Option<PostMatchFn>,
    pub build_spec: Option<BuildSpec>,
}

/// Per-edge payload — typed slot indices recovering today's
/// `node_inputs(node)[i]` semantics on top of petgraph.
#[derive(Clone, Copy, Debug)]
pub struct EdgeData {
    pub consumer_slot: usize,
    pub producer_output_slot: usize,
}

pub type PostMatchFn = Box<
    dyn Fn(&crate::matcher::MatchCtx, strider_ir::node::NodeId, &mut crate::capture::Bindings) -> bool,
>;

pub struct BuildSpec {
    pub kind: BuildKind,
    pub ty: BuildTy,
}
pub enum BuildKind {
    Exact(NodeKind),
    Fn(Box<dyn Fn(&crate::matcher::BuildCtx<'_>) -> anyhow::Result<NodeKind>>),
}
pub enum BuildTy {
    InheritRoot,
    Fixed(NodeOutputType),
}
```

- [ ] **Step 2: Define PatGraph + role markers + Combine trait**

`crates/strider-pattern/src/pat_graph/mod.rs`:

```rust
mod node_data;
mod role;
mod topo;
mod merge;

pub use node_data::{KindSpec, NodeData, EdgeData, PostMatchFn, BuildSpec, BuildKind, BuildTy};
pub use role::{Wildcard, Concrete, Combine, Role};
pub(crate) use merge::merge_subgraph;
pub(crate) use topo::{topo_order_from_root, assert_dag};

use std::marker::PhantomData;
use petgraph::stable_graph::{StableDiGraph, NodeIndex};

/// Pattern graph parametrised by a role marker.
///
/// `R = Wildcard` — the graph contains at least one node whose kind is
/// `Any` or carries a custom predicate.  Matchable; NOT a Template.
///
/// `R = Concrete` — every node has either a concrete `NodeKind`
/// (kind-Exact / kind-VariantWith) or a `Capture` (resolvable at build
/// time via Bindings).  Both Matchable and Template.
///
/// The role parameter is purely a type-level marker; the runtime
/// representation is identical regardless of `R`.
pub struct PatGraph<R> {
    pub(crate) inner: StableDiGraph<NodeData, EdgeData>,
    pub(crate) root: Option<NodeIndex>,
    pub(crate) captures: Vec<crate::capture::Capture>,
    pub(crate) _role: PhantomData<R>,
}

impl<R> PatGraph<R> {
    pub fn new() -> Self {
        Self {
            inner: StableDiGraph::new(),
            root: None,
            captures: Vec::new(),
            _role: PhantomData,
        }
    }
    pub(crate) fn add_node(&mut self, data: NodeData) -> NodeIndex {
        self.inner.add_node(data)
    }
    pub(crate) fn add_edge(
        &mut self,
        producer: NodeIndex,
        consumer: NodeIndex,
        data: EdgeData,
    ) {
        self.inner.add_edge(producer, consumer, data);
    }
    pub(crate) fn set_root(&mut self, n: NodeIndex) {
        self.root = Some(n);
    }
    pub(crate) fn root(&self) -> Option<NodeIndex> {
        self.root
    }

    /// Cast this graph's role marker.  Always safe to widen
    /// `Concrete → Wildcard` (you can use any Concrete pattern in a
    /// Wildcard context).  The reverse direction is `pub(crate)` only
    /// and used internally by leaf builders that know their construction
    /// is fully buildable.
    pub fn into_wildcard(self) -> PatGraph<Wildcard> {
        PatGraph {
            inner: self.inner,
            root: self.root,
            captures: self.captures,
            _role: PhantomData,
        }
    }
}

impl<R> Default for PatGraph<R> {
    fn default() -> Self { Self::new() }
}
```

`crates/strider-pattern/src/pat_graph/role.rs`:

```rust
//! Phantom role markers + their composition rule.

/// Sealed marker trait — only `Wildcard` and `Concrete` implement it.
pub trait Role: sealed::Sealed {}
mod sealed { pub trait Sealed {} }

/// The pattern contains at least one node that cannot be instantiated
/// (kind-Any or a custom predicate without a build spec).  Patterns
/// in this role are **matchable only** — using one as a rewrite RHS
/// fails at compile time.
pub struct Wildcard;
impl sealed::Sealed for Wildcard {}
impl Role for Wildcard {}

/// Every node in the pattern has a build path: either a concrete
/// `NodeKind` (kind-Exact / kind-VariantWith) or a `Capture` (resolved
/// at template time through `Bindings`).  Patterns in this role are
/// **matchable + buildable**.
pub struct Concrete;
impl sealed::Sealed for Concrete {}
impl Role for Concrete {}

/// Compose two roles.  Weaker role wins (Wildcard absorbs Concrete).
///
/// Implemented by exactly four impls — `Wildcard×Wildcard`,
/// `Wildcard×Concrete`, `Concrete×Wildcard`, `Concrete×Concrete`.
/// No other roles are reachable thanks to the sealed `Role` trait.
pub trait Combine<Other: Role>: Role {
    type Output: Role;
}

impl Combine<Wildcard> for Wildcard { type Output = Wildcard; }
impl Combine<Wildcard> for Concrete { type Output = Wildcard; }
impl Combine<Concrete> for Wildcard { type Output = Wildcard; }
impl Combine<Concrete> for Concrete { type Output = Concrete; }
```

- [ ] **Step 3: Define topological iteration + DAG assertion**

`crates/strider-pattern/src/pat_graph/topo.rs`:

```rust
//! Topological iteration over a `PatGraph` rooted at `root`.
//!
//! The user's contract guarantees DAG (no cycles); we assert it at
//! pattern-finalisation time so a builder bug surfaces immediately.

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use entity_utils::DenseEntitySet;
use anyhow::anyhow;

use super::node_data::{NodeData, EdgeData};

/// Returns nodes reachable from `root` in reverse topological order
/// (leaves first, root last).  Errors if a cycle is detected.
pub(crate) fn topo_order_from_root(
    g: &StableDiGraph<NodeData, EdgeData>,
    root: NodeIndex,
) -> anyhow::Result<Vec<NodeIndex>> {
    // Iterative postorder DFS via an explicit stack, with a visited set
    // and an on-stack set to detect cycles.
    let mut order = Vec::new();
    let mut visited = std::collections::HashSet::<NodeIndex>::new();
    let mut on_stack = std::collections::HashSet::<NodeIndex>::new();
    let mut stack: Vec<(NodeIndex, bool)> = vec![(root, false)];
    while let Some((n, expanded)) = stack.pop() {
        if expanded {
            on_stack.remove(&n);
            order.push(n);
            continue;
        }
        if !visited.insert(n) {
            continue;
        }
        on_stack.insert(n);
        stack.push((n, true));
        // Pattern edges go producer → consumer; iterate incoming edges
        // (producers feeding `n`) to schedule them before `n`.
        for edge in g.edges_directed(n, petgraph::Incoming) {
            let producer = edge.source();
            if on_stack.contains(&producer) {
                return Err(anyhow!(
                    "PatGraph cycle detected: {:?} reachable from itself",
                    producer
                ));
            }
            stack.push((producer, false));
        }
    }
    Ok(order)
}

/// Asserts that the graph rooted at `root` is a DAG.  Used at
/// pattern-finalisation (`into_pat`) time.
pub(crate) fn assert_dag(
    g: &StableDiGraph<NodeData, EdgeData>,
    root: NodeIndex,
) -> anyhow::Result<()> {
    topo_order_from_root(g, root).map(|_| ())
}
```

Note: `DenseEntitySet` is the project preference for NodeId-keyed sets (per `project_prefer_entity_utils`); here we're keying by `petgraph::NodeIndex`, which is not entity-utils-friendly. `HashSet<NodeIndex>` is acceptable inside an O(pattern size) operation — pattern graphs are tiny (≤ ~30 nodes). For the matcher's IR-side visited set (Task 5), we'll use `DenseEntitySet<NodeId>`.

- [ ] **Step 4: Define merge_subgraph**

`crates/strider-pattern/src/pat_graph/merge.rs`:

```rust
//! Merge a child `PatGraph` (built by a sub-expression) into a parent
//! `PatGraph` (built by a binary / unary / call constructor).  Used by
//! every chained builder to absorb child graphs.
//!
//! The child graph is *consumed by value* — `NodeData` is move-only
//! (closures inside are `Box<dyn Fn>`, not Clone) so we destructure
//! the child instead of cloning it.

use petgraph::stable_graph::NodeIndex;
use std::collections::HashMap;

use super::PatGraph;

/// Merges `child` into `parent`.  Returns the parent-side `NodeIndex`
/// corresponding to `child.root()`.  All child nodes / edges are
/// *moved* (not cloned) into the parent with remapped indices.
/// `child.captures` are appended to `parent.captures`.
pub(crate) fn merge_subgraph<R1, R2, RO>(
    parent: &mut PatGraph<RO>,
    child: PatGraph<R2>,
) -> NodeIndex
where
    R1: crate::pat_graph::Role,
    R2: crate::pat_graph::Role,
    RO: crate::pat_graph::Role,
{
    let PatGraph { inner: child_inner, root: child_root, captures: child_captures, _role: _ } = child;
    let child_root = child_root.expect("merge_subgraph called on rootless child");

    // petgraph's StableDiGraph exposes `into_nodes_edges_iters` (NOT
    // `into_nodes_edges` — that's on the inner Graph only).  Each
    // iterator yields owned StableGraphNode/Edge entries which carry
    // their own NodeIndex; enumerate-derived indices are wrong because
    // a StableGraph may contain holes from earlier removals (we don't
    // remove during construction, but use the API correctly anyway).
    let (node_iter, edge_iter) = child_inner.into_nodes_edges_iters();
    let mut remap: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    for stable_node in node_iter {
        // `StableGraphNode { idx, weight }` (private fields — use
        // accessors); `idx()` returns the original NodeIndex, `weight`
        // is the owned NodeData.
        let old_idx = stable_node.idx();
        let new_idx = parent.inner.add_node(stable_node.weight);
        remap.insert(old_idx, new_idx);
    }
    for stable_edge in edge_iter {
        // `StableGraphEdge { source(), target(), weight }`.
        let new_src = remap[&stable_edge.source()];
        let new_dst = remap[&stable_edge.target()];
        parent.inner.add_edge(new_src, new_dst, stable_edge.weight);
    }
    parent.captures.extend(child_captures);
    remap[&child_root]
}
```

`NodeData` does **not** derive `Clone` (closures inside are `Box<dyn Fn>`, which is move-only). `merge_subgraph` consumes the child graph by value via `StableDiGraph::into_nodes_edges()`, so cloning is never needed. This drops the Send/Sync/Arc baggage that the prior draft of the plan inherited from the existing `strider-analyze::pattern` shape.

- [ ] **Step 5: Unit test the graph storage + topo**

`crates/strider-pattern/src/pat_graph/topo.rs` — add a `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pat_graph::PatGraph;
    use crate::pat_graph::node_data::*;
    use strider_ir::node::{NodeKind, NodeOutputType};

    fn dummy_node() -> NodeData {
        NodeData {
            kind: KindSpec::Any,
            output_ty: Some(NodeOutputType::I64),
            capture: None,
            post_match: None,
            build_spec: None,
        }
    }

    #[test]
    fn topo_chain_returns_leaves_first() {
        let mut g = PatGraph::new();
        let a = g.add_node(dummy_node());
        let b = g.add_node(dummy_node());
        let c = g.add_node(dummy_node());
        g.add_edge(a, b, EdgeData { consumer_slot: 0, producer_output_slot: 0 });
        g.add_edge(b, c, EdgeData { consumer_slot: 0, producer_output_slot: 0 });
        g.set_root(c);
        let order = topo_order_from_root(&g.inner, c).unwrap();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn topo_detects_cycle() {
        let mut g = PatGraph::new();
        let a = g.add_node(dummy_node());
        let b = g.add_node(dummy_node());
        g.add_edge(a, b, EdgeData { consumer_slot: 0, producer_output_slot: 0 });
        g.add_edge(b, a, EdgeData { consumer_slot: 0, producer_output_slot: 0 });
        let err = topo_order_from_root(&g.inner, a).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }
}
```

- [ ] **Step 6: Verify + commit**

```bash
cargo test -p strider-pattern --release 2>&1 | tail -5
cargo clippy -p strider-pattern --all-targets --release -- -D warnings
```

Commit (stage all created files explicitly):

```bash
git add crates/strider-pattern/src/pat_graph/ crates/strider-pattern/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): add PatGraph storage + topological traversal

PatGraph wraps petgraph::StableDiGraph and recovers typed slot
semantics via EdgeData (consumer_slot, producer_output_slot).  The
topological-walk helper asserts DAG (the pattern crate's contract) so
a downstream builder bug surfaces at finalisation time, not at match
time.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 3: Capture, Bindings, Match (ported from strider-analyze)

**Files:**
- Create: `crates/strider-pattern/src/capture.rs`
- Modify: `crates/strider-pattern/src/lib.rs`

Source files in strider-analyze to port verbatim, adapting only the module path:

- `strider-analyze/src/pattern/var.rs` → `Capture`, `CaptureRef`
- `strider-analyze/src/pattern/matcher/bindings.rs` → `Bindings`, `BindingsMark`
- `strider-analyze/src/pattern/matcher/match_result.rs` → `Match`

- [ ] **Step 1: Port var.rs as `capture.rs`**

Read `crates/strider-analyze/src/pattern/var.rs` and copy the `Capture` / `CaptureRef` types into `crates/strider-pattern/src/capture.rs`. Adjust visibility — these are public API.

- [ ] **Step 2: Inline `Bindings` and `BindingsMark` into the same file**

Read `crates/strider-analyze/src/pattern/matcher/bindings.rs` and append into `capture.rs`. The `Bindings` enum (`Node(NodeId)` / `Output(NodeOutputId)`) introduced in C4 of the prior cleanup is the storage shape we want.

- [ ] **Step 3: Inline `Match`**

Read `crates/strider-analyze/src/pattern/matcher/match_result.rs` and append. `Match` exposes `node(c, function)`, `output(c)`, `get_vn(c, function)`, `root()`, `bindings_clone()`. Port verbatim.

- [ ] **Step 4: Add `pub mod capture;` to lib.rs and re-export the public types**

```rust
// crates/strider-pattern/src/lib.rs
pub mod capture;
pub mod pat_graph;

pub use capture::{Capture, CaptureRef, Bindings, BindingsMark, Match};
pub use pat_graph::{PatGraph, KindSpec, NodeData, EdgeData};
```

- [ ] **Step 5: Port the unit tests from bindings.rs and match_result.rs**

Bindings has a small test module in strider-analyze; copy the tests across to maintain coverage.

- [ ] **Step 6: Verify + commit**

```bash
cargo test -p strider-pattern --release
cargo clippy -p strider-pattern --all-targets --release -- -D warnings
git add crates/strider-pattern/src/capture.rs crates/strider-pattern/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): port Capture / Bindings / Match from strider-analyze

Same semantics as strider-analyze::pattern (Bindings enum with
Node(NodeId) / Output(NodeOutputId) arms after C4).  No behavioural
changes; the move lets the new matcher (next commit) consume these
types directly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 4: Pattern trait + Matcher scaffolding

**Files:**
- Create: `crates/strider-pattern/src/matcher/mod.rs`
- Create: `crates/strider-pattern/src/matcher/ctx.rs`
- Modify: `crates/strider-pattern/src/lib.rs`

- [ ] **Step 1: Define the Pattern trait**

`crates/strider-pattern/src/matcher/mod.rs`:

```rust
//! Matcher: scans an IR graph for nodes matching a `PatGraph`.

mod ctx;

pub use ctx::{MatchCtx, BuildCtx};

use std::mem::Discriminant;
use strider_ir::{Function, node::{NodeId, NodeOutputId, NodeKind}};
use crate::capture::{Bindings, Match};
use crate::pat_graph::PatGraph;

/// LHS of a rewrite or query.  A `Pattern` can be matched against an
/// IR node-output to attempt to bind captures.
pub trait Pattern {
    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool;

    /// Discriminant of the root node's `NodeKind`, used by `find_all` to
    /// scan the IR's kind index directly instead of walking every node.
    /// Returns `None` for kind-`Any` roots (rare).
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>>;
}

/// Top-level matcher.  Owns no per-match state — `try_new` is the
/// validation gate (today's matcher invokes `validate::validate` once
/// up-front).
pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
}

impl<'f> Matcher<'f> {
    pub fn try_new(function: &'f Function) -> anyhow::Result<Self> {
        // Validate once; today's matcher does this in strider-analyze.
        // Keep the same contract.
        let entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("Function has no entry"))?;
        strider_ir::validate::validate(function, entry)
            .map_err(|errs| anyhow::anyhow!("validation: {errs:?}"))?;
        Ok(Self { function })
    }

    pub fn find_all<P: Pattern>(&self, pat: &P) -> Vec<Match> {
        let mut out = Vec::new();
        let ctx = MatchCtx { matcher: self, function: self.function };
        match pat.root_kind_discriminant() {
            Some(d) => {
                // Iterate only IR nodes whose kind matches the
                // discriminant.  Today this consults a lazy kind index
                // on Matcher.  For the MVP we iterate all reachable
                // nodes and filter; optimise in a follow-up.
                for node in self.function.walk() {
                    if std::mem::discriminant(self.function.node_kind(node)) != d {
                        continue;
                    }
                    self.try_at_node(node, pat, &ctx, &mut out);
                }
            }
            None => {
                for node in self.function.walk() {
                    self.try_at_node(node, pat, &ctx, &mut out);
                }
            }
        }
        out
    }

    fn try_at_node<P: Pattern>(
        &self,
        node: NodeId,
        pat: &P,
        ctx: &MatchCtx,
        out: &mut Vec<Match>,
    ) {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::new();
            if pat.try_match_node_id(ctx, node, &mut bindings) {
                out.push(Match::from_root(node, bindings));
            }
            return;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::new();
            if pat.try_match(ctx, out_id, &mut bindings) {
                out.push(Match::from_root(node, bindings));
                break;
            }
        }
    }

    pub fn match_at<P: Pattern>(
        &self,
        node: NodeId,
        pat: &P,
    ) -> Option<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::new();
            if pat.try_match_node_id(&ctx, node, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
            return None;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::new();
            if pat.try_match(&ctx, out_id, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
        }
        None
    }
}

// Extension: try_match_node_id is for zero-output kinds (Return).
// We add it via a default method on Pattern + override in PatGraph.
pub trait PatternExt {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool;
}
impl<T: Pattern + ?Sized> PatternExt for T {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool {
        // Default: try every output, then short-circuit.
        let outputs = ctx.function.node_outputs(node);
        if outputs.is_empty() {
            return false;
        }
        for &out in outputs {
            let mark = bindings.mark();
            if self.try_match(ctx, out, bindings) {
                return true;
            }
            bindings.restore(mark);
        }
        false
    }
}
```

`crates/strider-pattern/src/matcher/ctx.rs`:

```rust
use strider_ir::Function;
use super::Matcher;

#[derive(Clone, Copy)]
pub struct MatchCtx<'a> {
    pub matcher: &'a Matcher<'a>,
    pub function: &'a Function,
}

pub struct BuildCtx<'a> {
    pub function: &'a mut Function,
    pub bindings: &'a crate::capture::Bindings,
    pub root: strider_ir::node::NodeId,
    pub root_ty: strider_ir::node::NodeOutputType,
}
```

- [ ] **Step 2: Re-export from lib.rs**

```rust
// add to lib.rs
pub mod matcher;
pub use matcher::{Matcher, Pattern, MatchCtx, BuildCtx};
```

- [ ] **Step 3: Verify + commit**

```bash
cargo build -p strider-pattern --release
cargo clippy -p strider-pattern --all-targets --release -- -D warnings
git add crates/strider-pattern/src/matcher/ crates/strider-pattern/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): add Pattern trait + Matcher scaffolding

The Pattern trait + Matcher::find_all / match_at give the same public
surface as strider-analyze::pattern::Matcher.  PatGraph's Pattern
impl lands in the next commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 5: PatGraph implements Pattern (kind-only single-node case)

**Files:**
- Create: `crates/strider-pattern/src/matcher/try_match.rs`
- Modify: `crates/strider-pattern/src/pat_graph/mod.rs`

- [ ] **Step 1: Write the failing test first**

`crates/strider-pattern/tests/basic_match.rs`:

```rust
use strider_pattern::pat_graph::{PatGraph, KindSpec, NodeData};
use strider_pattern::Matcher;
use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::RegisterSet;

#[test]
fn matches_int_const_5() {
    // Build IR: a function with a single IntConst(5):I64.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let _five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();

    // Build PatGraph: a single IntConst(5) pattern node.
    let mut g = PatGraph::new();
    let root = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(5)),
        output_ty: Some(NodeOutputType::I64),
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.set_root(root);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert_eq!(hits.len(), 1, "expected exactly one IntConst(5) match");
}
```

Run: `cargo test -p strider-pattern --test basic_match` → FAIL (PatGraph doesn't impl Pattern yet).

- [ ] **Step 2: Implement Pattern for PatGraph (single-node case)**

`crates/strider-pattern/src/matcher/try_match.rs`:

```rust
//! `impl Pattern for PatGraph` — rooted DAG topological matcher.

use std::mem::Discriminant;
use strider_ir::node::{NodeId, NodeOutputId, NodeKind};
use crate::capture::Bindings;
use crate::matcher::{MatchCtx, Pattern};
use crate::pat_graph::PatGraph;

impl Pattern for PatGraph {
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>> {
        let root = self.root?;
        self.inner.node_weight(root)?.kind.discriminant()
    }

    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool {
        let Some(root) = self.root else {
            return false;
        };
        // Single-node case (kind check + capture + post_match);
        // Task 6 adds the recursive input walk.
        let root_node = ctx.function.node_for_output(root_out);
        let nd = self.inner.node_weight(root).expect("root must exist");
        if !nd.kind.matches(ctx.function.node_kind(root_node)) {
            return false;
        }
        // Bind capture if requested.
        if let Some(cap) = &nd.capture
            && !bindings.bind_output(*cap, root_out, ctx.function)
        {
            return false;
        }
        if let Some(pm) = &nd.post_match
            && !pm(ctx, root_node, bindings)
        {
            return false;
        }
        true
    }
}
```

Wire it from `matcher/mod.rs` (one line; the impl block is compiled as part of the matcher module):

```rust
// crates/strider-pattern/src/matcher/mod.rs — add at top
mod try_match;
```

- [ ] **Step 3: Run test → PASS**

```bash
cargo test -p strider-pattern --test basic_match --release
# expect: 1 passed
```

- [ ] **Step 4: Commit**

```bash
git add crates/strider-pattern/src/matcher/try_match.rs crates/strider-pattern/src/matcher/mod.rs crates/strider-pattern/tests/basic_match.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): PatGraph::try_match — single-node case

Kind check + optional capture + optional post_match.  Recursive input
walk lands in the next commit (Task 6).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 6: Recursive matcher — DAG topological walk with input edges

**Files:**
- Modify: `crates/strider-pattern/src/matcher/try_match.rs`
- Modify: `crates/strider-pattern/tests/basic_match.rs` (add tests)
- Create: `crates/strider-pattern/tests/capture_equality.rs`

- [ ] **Step 1: Write failing tests**

Add to `basic_match.rs`:

```rust
#[test]
fn matches_add_int_const_5_var() {
    // IR: x = read_var(rax); add(5, x).  Pattern: add(int_const(5), var(c)).
    // Match should bind c to x's output.
    // ...
    todo!("write test against fixture")
}
```

`tests/capture_equality.rs`:

```rust
#[test]
fn add_x_x_matches_when_both_inputs_are_same_node() {
    // Build IR with add(N, N) where N is a Load.
    // Pattern: add(var(x), var(x))
    // Expect: 1 hit, captures bind to the same NodeOutputId.
    todo!("write test")
}

#[test]
fn add_x_x_rejects_when_inputs_differ() {
    // IR: add(N1, N2) where N1 != N2.
    // Pattern: add(var(x), var(x))
    // Expect: 0 hits (capture-equality enforced).
    todo!("write test")
}
```

Run → FAIL.

- [ ] **Step 2: Extend try_match with recursive input walk**

Replace the single-node body in `try_match.rs` with the topo-walking version. Algorithm: starting from the root pattern node, for each producer (incoming edge) recursively try-match against the IR node that feeds the corresponding IR input slot. Use a `DenseEntitySet<NodeId>` for the IR-side visited set (per `project_prefer_entity_utils`).

**IR API names** — verified against `crates/strider-ir/src/graph/access.rs`:
- `Function` derefs to `Graph`; the methods below all live on `Graph`.
- `function.node_kind(node_id) -> &NodeKind`.
- `function.node_inputs(node_id) -> Inputs<'_>` (an iterator-ish view).
- `function.node_outputs(node_id) -> &[NodeOutputId]`.
- `function.node_for_output(output_id) -> NodeId` — IR-side producer lookup.
- `function.input_output_id(input_id) -> NodeOutputId` — given a `NodeInputId`, returns the `NodeOutputId` it consumes. (There is **no** `node_for_input`; chain `input_output_id → node_for_output`.)
- For `Inputs<'_>`, iterate or index — the exact accessor pattern is what today's matcher uses; mirror it.

Pseudocode (corrected):

```rust
fn try_match_at<R>(
    pat: &PatGraph<R>,
    pat_node: NodeIndex,
    ctx: &MatchCtx,
    ir_node: NodeId,
    bindings: &mut Bindings,
) -> bool {
    let nd = pat.inner.node_weight(pat_node).unwrap();
    if !nd.kind.matches(ctx.function.node_kind(ir_node)) {
        return false;
    }
    // Collect this pat node's incoming edges keyed by consumer_slot.
    let mut by_slot: std::collections::HashMap<usize, (NodeIndex, usize)> =
        Default::default();
    for edge in pat.inner.edges_directed(pat_node, petgraph::Incoming) {
        let ed = edge.weight();
        by_slot.insert(ed.consumer_slot, (edge.source(), ed.producer_output_slot));
    }
    // Index the IR node's input outputs by slot.  Today's matcher uses
    // `node_inputs(ir_node)` then resolves each NodeInputId via
    // `input_output_id` → NodeOutputId → `node_for_output` if we need
    // the producer node.
    let ir_inputs = ctx.function.node_inputs(ir_node);  // Inputs<'_>
    for (slot, (producer_pat, _producer_output_slot)) in by_slot {
        let Some(ir_input_id) = ir_inputs.get(slot) else { return false; };
        let producer_out = ctx.function.input_output_id(ir_input_id);
        let producer_ir = ctx.function.node_for_output(producer_out);
        let mark = bindings.mark();
        if !try_match_at(pat, producer_pat, ctx, producer_ir, bindings) {
            bindings.restore(mark);
            return false;
        }
    }
    // Capture-binding: bind the IR node's output that this pat_node
    // matches against.  The exact semantics (Bindings::bind_output /
    // bind_node with equality check) are ported from
    // `strider-analyze/src/pattern/matcher/mod.rs` — read it for the
    // detail; do not rewrite from scratch.
    // …
    // post_match runs after children matched — uses the new
    // `.when_match` signature (ctx, ty, bindings):
    if let Some(pm) = &nd.post_match {
        // pm: Box<dyn Fn(&MatchCtx, NodeOutputType, &mut Bindings) -> bool>
        let ty = /* infer from pat_node.output_ty or from the matched
                   IR output's type — port from existing matcher */;
        if !pm(ctx, ty, bindings) {
            return false;
        }
    }
    true
}
```

Detail: capture-binding semantics + the exact `Inputs<'_>::get(slot)` accessor are ported verbatim from `strider-analyze::pattern::matcher`. Do not redesign — translate.

- [ ] **Step 2.5: Commutative-retry path**

For commutative ops (`NodeKind::is_commutative()` returns true), the matcher must try operand swap when the first ordering fails. Today this lives in `strider-analyze/src/pattern/pat/node_pat.rs::try_match_common` — the snapshot-restore + retry-with-swapped-slot pattern. Port that logic into `try_match_at` for the arity-2 `Indexed` case where `is_commutative(nd.kind)` holds: snapshot bindings via `bindings.mark()`, attempt slots 0/1 as written; if that fails, restore and attempt with slots 0/1 swapped.

This step lands before Task 8 (binary-op builders) so commutative `xor` / `add` tests pass.

- [ ] **Step 3: Tests pass**

```bash
cargo test -p strider-pattern --release
```

- [ ] **Step 4: Commit**

```bash
git add crates/strider-pattern/src/matcher/try_match.rs crates/strider-pattern/tests/basic_match.rs crates/strider-pattern/tests/capture_equality.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): recursive DAG match via topological walk

PatGraph::try_match now recurses through input edges.  Capture-equality
on Bindings forces the same IR node into both slots when a capture is
re-used (add(var(x), var(x)) semantics preserved).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 7: Chained builder facade — wildcards, captures, constants

**Files:**
- Create: `crates/strider-pattern/src/builders/mod.rs`
- Create: `crates/strider-pattern/src/builders/wildcards.rs`
- Create: `crates/strider-pattern/src/builders/consts.rs`
- Modify: `crates/strider-pattern/src/lib.rs`

These are the leaf builders — they don't have child patterns, just build a one-node `PatGraph`. Reading `strider-analyze/src/pattern/pat/ctor/wildcards.rs` and `consts.rs` gives the exact spec.

- [ ] **Step 1: Define the `Pat` type alias and `IntoPat` trait**

`crates/strider-pattern/src/builders/mod.rs`:

```rust
pub mod wildcards;
pub mod consts;

pub use wildcards::{any, var, value_of_width, inputs_of_width, bool_value, bool_inputs, predicate};
pub use consts::{int_const, int_const_any_of, bool_const, float_const, any_int_const, any_bool_const, any_float_const, signed_int_const};

// `Pat<R>` is a thin newtype around an owned PatGraph<R>.  Move-only
// (no Clone, no Arc, no Rc) per the no-Send/Sync convention.  The
// role parameter `R` is `Wildcard` or `Concrete`; the type system
// enforces that `Pat<Wildcard>` cannot be used where a Template is
// expected.
//
// Chained builders consume sub-Pats by value; the matcher borrows
// (`&pat`).  To reuse a pattern across multiple rules, wrap in
// `Rc<PatGraph<R>>` at the callsite — the crate itself doesn't bake
// refcounting in.
use crate::pat_graph::{PatGraph, Wildcard, Concrete};

pub struct Pat<R>(pub(crate) PatGraph<R>);

impl<R> Pat<R> {
    pub fn from_graph(g: PatGraph<R>) -> Self {
        // Assert DAG at construction time so a bug surfaces here, not at match.
        if let Some(root) = g.root() {
            crate::pat_graph::assert_dag(&g.inner, root)
                .expect("PatGraph passed to Pat::from_graph contains a cycle");
        }
        Pat(g)
    }

    /// Always-safe role widening (Concrete → Wildcard).
    pub fn into_wildcard(self) -> Pat<Wildcard> {
        Pat(self.0.into_wildcard())
    }

    // Universal chainers ported from strider-analyze.  `capture` and
    // `cap` preserve the role (binding a capture doesn't change
    // buildability).  `when_match` *moves* the role to Wildcard
    // because a post-match predicate has no template counterpart.
    //
    // Signature note: the real existing predicate signature (see
    // `strider-analyze/src/opt/constant_fold/rules.rs:166`,
    // `strider-analyze/src/opt/flag_cmp_canonicalize/mod.rs:219`) is
    // `Fn(&MatchCtx, NodeOutputType, &Bindings) -> bool` — three
    // arguments, NOT `&PartialMatch`.  Port that signature verbatim.
    pub fn capture(self, c: crate::capture::Capture) -> Self {
        todo!("port from var.rs / IntoPat::capture — preserves role")
    }
    pub fn cap(self, name: impl Into<String>) -> Self {
        todo!("port — preserves role")
    }
    pub fn when_match<F>(self, f: F) -> Pat<Wildcard>
    where F: Fn(&crate::MatchCtx, strider_ir::node::NodeOutputType, &crate::Bindings) -> bool + 'static
    {
        todo!("port — coerces to Wildcard since predicates aren't buildable")
    }
}

// LHS-vs-RHS independence note for the role marker:
//
// `Rewrite<L: Pattern, T: Template>::new(lhs, rhs)` takes LHS and RHS
// as *separate* type parameters.  A predicate-decorated LHS sub-tree
// has role `Wildcard` (because of `.when_match`), but this only
// affects the LHS type parameter; the RHS is its own expression tree
// and is typed independently.  So:
//
//     Rewrite::new(
//         truncate(zero_extend(var(x))).when_match(|_,_,_| true),  // Pat<Wildcard> — OK as LHS
//         var(x),                                                  // Pat<Concrete>  — OK as RHS
//     )
//
// compiles.  Only embedding a `Pat<Wildcard>` inside the RHS subtree
// would fail (which is the whole point of the type-level check).

// Pattern impl for Pat<R> — implemented for every role.
impl<R> crate::Pattern for Pat<R> {
    fn try_match(&self, ctx: &crate::MatchCtx, root_out: strider_ir::node::NodeOutputId, b: &mut crate::Bindings) -> bool {
        self.0.try_match(ctx, root_out, b)
    }
    fn root_kind_discriminant(&self) -> Option<std::mem::Discriminant<strider_ir::node::NodeKind>> {
        self.0.root_kind_discriminant()
    }
}

// Template impl for Pat<Concrete> ONLY.  This is the compile-time
// gate: passing a `Pat<Wildcard>` where a Template is expected fails
// with `the trait Template is not implemented for Pat<Wildcard>`.
impl crate::Template for Pat<Concrete> {
    fn instantiate(&self, function: &mut strider_ir::Function, bindings: &crate::Bindings, root_ty: strider_ir::node::NodeOutputType) -> anyhow::Result<strider_ir::node::NodeOutputId> {
        self.0.instantiate(function, bindings, root_ty)
    }
}
```

- [ ] **Step 2: Port wildcards**

`crates/strider-pattern/src/builders/wildcards.rs` — for each builder, construct a one-node `PatGraph<R>` with the appropriate `KindSpec` and return via `Pat::from_graph`. Role is determined by buildability: kind-Any with no capture → `Wildcard`; kind-Any with a Capture → `Concrete` (resolvable at build time via Bindings).

```rust
use std::marker::PhantomData;
use crate::pat_graph::{PatGraph, NodeData, KindSpec, Wildcard, Concrete};
use crate::builders::Pat;

pub fn any() -> Pat<Wildcard> {
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

pub fn var(c: crate::capture::Capture) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(c.as_ref()),
        post_match: None,
        build_spec: None,   // capture is the build path: resolved through Bindings at instantiate time
    });
    g.captures.push(c);
    g.set_root(n);
    Pat::from_graph(g)
}

pub fn predicate<F>(f: F) -> Pat<Wildcard>
where F: Fn(&crate::PartialMatch) -> bool + 'static
{
    // … kind-Any + post_match closure; no build path → Wildcard role.
    todo!("port from strider-analyze; role is Wildcard")
}
```

- [ ] **Step 3: Port consts**

Same pattern. `int_const(v)` builds `Pat<Concrete>` (concrete `NodeKind::IntConst(v)` → buildable). `any_int_const()` builds `Pat<Wildcard>` (kind-VariantWith without a fixed value, no build spec).

- [ ] **Step 4: Smoke-test**

`crates/strider-pattern/tests/builders_smoke.rs`:

```rust
use strider_pattern::{int_const, var, any, Capture, Matcher};
use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;

#[test]
fn int_const_5_matches_via_builder() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let _ = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let pat = int_const(5u64);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}
```

- [ ] **Step 5: Verify + commit**

```bash
cargo test -p strider-pattern --release
git add crates/strider-pattern/src/builders/ crates/strider-pattern/src/lib.rs crates/strider-pattern/tests/builders_smoke.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): chained builders — wildcards, captures, constants

The leaf builders (any, var, int_const, …) wrap a one-node PatGraph
behind the same `Pat` newtype the rest of the crate consumes.  Same
external API as strider-analyze::pattern; internal storage changed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 8: Chained builder facade — binary ops (add, sub, mul, and/or/xor, shifts)

**Files:**
- Create: `crates/strider-pattern/src/builders/int_ops.rs`
- Modify: `crates/strider-pattern/src/builders/mod.rs`

Spec: read `strider-analyze/src/pattern/pat/builders/binary_op.rs` and `strider-analyze/src/pattern/pat/ctor/int.rs`. Two-pat children → merged into one parent PatGraph via `merge_subgraph`. The commutative flag is preserved (today's `is_commutative` table on `NodeKind` is still the SSoT — read it through `strider_ir::NodeKind::is_commutative()`).

- [ ] **Step 1: Write failing tests for `add` and `xor`**

```rust
#[test]
fn add_int_const_5_var_x_matches_and_captures() { /* ... */ }
#[test]
fn xor_is_commutative() { /* ... */ }
```

- [ ] **Step 2: Implement the binary-op factory**

Binary builders are generic over both operand roles and produce the
*combined* role (Wildcard absorbs Concrete via the `Combine` trait).

```rust
use crate::pat_graph::{Role, Combine, Wildcard, Concrete};
use std::marker::PhantomData;

fn binary_op_pat<R1, R2>(
    op: NodeKind,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
{
    // Consume both sub-Pats by value; merge_subgraph moves the
    // underlying NodeData out of the child graphs.  No Arc, no Clone.
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Exact(op),
        output_ty: None,                       // inherits at match time
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(op),
            ty: BuildTy::InheritRoot,
        }),
    });
    parent.add_edge(lhs_root, root, EdgeData { consumer_slot: 0, producer_output_slot: 0 });
    parent.add_edge(rhs_root, root, EdgeData { consumer_slot: 1, producer_output_slot: 0 });
    parent.set_root(root);
    Pat::from_graph(parent)
}

pub fn add<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where R1: Combine<R2>,
{
    binary_op_pat(NodeKind::IntBinaryOp(strider_ir::node::IntBinaryOp::Add), lhs, rhs)
}
// … sub, mul, and, or, xor, shifts ….
```

**Note on `merge_subgraph` and roles.** `merge_subgraph` operates on
the structural `StableDiGraph` inside the PatGraph and is agnostic to
the role parameter; the caller (here `binary_op_pat`) constructs the
parent PatGraph with the combined role and trusts the `where` bound to
ensure soundness.

**Note on `Pat: !Clone`.** Builders consume by value, so cloning is
never required. `merge_subgraph` moves children's `NodeData` into the
parent via `StableDiGraph::into_nodes_edges()`.

**Commutative matching:** today's matcher tries operand swap when the kind is commutative. Inside `try_match_at`, after the first-order match fails, retry with `consumer_slot` 0 and 1 swapped — only when the pattern node is `is_commutative()`. Implement in Task 6's `try_match` (revise if needed).

- [ ] **Step 3: Iterate through every IntBinaryOp variant**

For each: `add`, `sub` (lift-time canonicalised — match as `Add(_, Neg(_))`), `mul`, `div`, `sdiv`, `rem`, `srem`, `and`, `or`, `xor`, `shl`, `shr`, `sshr`. Cross-reference `strider-ir::node::IntBinaryOp` for the canonical list.

`sub` is special: it doesn't exist as an `IntBinaryOp` variant — it's lowered to `Add(_, Neg(_))`. The builder `sub(a, b)` returns the pattern `add(a, neg(b))`. Same applies to `float_sub` later.

- [ ] **Step 4: Tests + commit**

```bash
cargo test -p strider-pattern --release
git add crates/strider-pattern/src/builders/int_ops.rs crates/strider-pattern/src/builders/mod.rs
git commit -m "$(cat <<'EOF'
feat(strider-pattern): chained builders — integer binary ops

add / sub / mul / div / and / or / xor / shifts.  Commutative-retry
preserved at the matcher level via NodeKind::is_commutative.  `sub`
remains the lift-time-lowered `Add(_, Neg(_))` shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push origin feature/strider-pattern-crate
```

---

## Task 9: Chained builder facade — comparisons, casts, unary ops, floats, bools

**Files:**
- Create: `crates/strider-pattern/src/builders/cmps.rs`
- Create: `crates/strider-pattern/src/builders/casts.rs`
- Create: `crates/strider-pattern/src/builders/unary_ops.rs`
- Create: `crates/strider-pattern/src/builders/float_ops.rs`
- Create: `crates/strider-pattern/src/builders/bool_ops.rs`

For each, the porting recipe is the same as Task 8 — build a `PatGraph` with the right `NodeKind` and slot wiring, merge children, set root. Source files:

- `strider-analyze/src/pattern/pat/ctor/int.rs` (cmps: `int_eq`, `int_lt`, `int_le`, `int_sle`, …)
- `strider-analyze/src/pattern/pat/ctor/casts.rs` (truncate, sign_extend, zero_extend, extend, int_to_float, float_to_int, int_bits_to_float, float_bits_to_int, float_to_float)
- `strider-analyze/src/pattern/pat/builders/unary_op.rs` + `cmp_op.rs`
- `strider-analyze/src/pattern/pat/ctor/float.rs`
- `strider-analyze/src/pattern/pat/ctor/bool_.rs`

The lift-time canonicalisations carry over verbatim — `int_le(a, b)` is `xor(int_lt(b, a), int_const(1)):I1`, `float_le(a, b)` is `or(float_lt(a, b), float_eq(a, b)):I1`, etc.

- [ ] **Step 1-4: One commit per family.** Each commit has its own `cargo test --release` + clippy gates. Each commit pushes.

Suggested commit subjects:
- `feat(strider-pattern): chained builders — integer comparisons`
- `feat(strider-pattern): chained builders — int casts + unary ops`
- `feat(strider-pattern): chained builders — float ops`
- `feat(strider-pattern): chained builders — bool ops`

Each commit includes one `tests/builders_smoke.rs` test per family.

---

## Task 10: Chained builder facade — memory, phi, control

**Files:**
- Create: `crates/strider-pattern/src/builders/memory.rs`
- Create: `crates/strider-pattern/src/builders/phi.rs`
- Create: `crates/strider-pattern/src/builders/control.rs`
- Create: `crates/strider-pattern/src/builders/function_arg.rs`

Source: `strider-analyze/src/pattern/pat/builders/{call,memory,phi,branch,ret,function_arg}.rs` + the corresponding `pat/ctor/control.rs`.

Indexed-input matching is the hard part here. `LoadPat::space`, `CallPat::arg(idx, p)`, etc., set per-slot sub-patterns. In the new world these become edges into the parent pat node with explicit `consumer_slot` indices on `EdgeData`. The matcher's input-slot iteration already handles sparse indexed cases (Task 6).

After C5 (consumer-walk deletion in the prior cleanup) and the recent OutputsSpec/ret_output removal, **only** input-side constraints exist. CallPat has `target` (slot 2) + `arg(i)` (slots 3..). CallOtherPat has `arg(i)` for raw `inputs[i]` plus `ctrl` (alias for `arg(0)`) and `mem` (alias for `arg(1)`).

Each builder lands as its own commit. Suggested subjects:
- `feat(strider-pattern): chained builders — memory ops (load, store)`
- `feat(strider-pattern): chained builders — phi / mem_phi / value_phi`
- `feat(strider-pattern): chained builders — control ops (call, call_other, ret, if)`
- `feat(strider-pattern): chained builders — function_arg`

Per commit: one smoke test in `tests/builders_smoke.rs`, gates pass, push.

---

## Task 11: Cast walk-through + predicate guards (no Region walk-through)

**Files:**
- Create: `crates/strider-pattern/src/matcher/cast_walk_through.rs`
- Create: `crates/strider-pattern/src/matcher/cast_mask.rs`
- Modify: `crates/strider-pattern/src/builders/wildcards.rs` (or a new `predicate.rs`)

Source: `strider-analyze/src/pattern/matcher/{walk_through,cast_mask}.rs` (the cast-handling portion only) + `strider-analyze/src/pattern/pat/guards.rs`.

**Cast walk-through (kept):** lets a pattern match `add(var(x), int_const(5))` against IR `Add(SignExtend(N), IntConst(5))` by transparently traversing through the cast. The set of casts to walk through is per-matcher via `MatcherOptions::cast_mask: CastMask`. Port verbatim from strider-analyze; rename `walk_through.rs` → `cast_walk_through.rs` since it now handles only casts.

**Region walk-through (dropped — explicit decision):** the new matcher does NOT walk through `Region` nodes. Any pattern that needs to cross a region boundary must explicitly include the `Region` (or whatever shape the IR has at that point). `MatcherOptions::ignore_regions` does not exist; the related code paths in `walk_through.rs` are NOT ported. This is a deliberate simplification — Region walk-through was the messier of the two behaviours, semantically muddying control-flow with data-flow; cast walk-through is bounded (a finite set of typed-conversion ops) and cleaner to reason about.

**Predicate guards (`when`, `ordered`):** chainable on any `Pat<R>`. Per the role rules (Task 7): `.capture(c)` and `.cap(name)` preserve `R`; `.when(predicate)` coerces the result to `Pat<Wildcard>` because a post-match predicate has no template counterpart.

**Migration impact (calls out for Task 15 + Task 17):** rules in `strider-analyze/src/opt/` that currently rely on Region walk-through (via `MatcherOptions::ignore_regions = true`) must be updated to include explicit `Region` shape in their patterns. Audit during Task 15:

```bash
rg -n 'ignore_regions' crates/strider-analyze/src/ crates/strider-analyze/tests/
rg -n 'ignore_regions' crates/strider-py/src/ crates/strider-py/strider/ crates/strider-py/tests/python/
```

Each call site either:
- Was set defensively (no region between root and inputs at match sites) → drop the option, pattern works as-is.
- Genuinely crosses a Region → rewrite the pattern to include the Region explicitly.

**Python-side breakage (confirmed during review):** `crates/strider-py/src/function.rs` exposes `ignore_regions=False` in three Py-method `signature` annotations (`find_all`, `find`, `find_joined` — confirmed at lines 391, 453, 520). `crates/strider-py/strider/__init__.pyi` declares the kwarg in three places (lines 323, 330, 340). And `crates/strider-py/strider/_api.py` references it. Task 17 must:
- Delete the `ignore_regions` parameter from all three Py method signatures.
- Delete the corresponding `.pyi` declarations.
- Update `_api.py` docstrings.
- Update any Python tests that pass `ignore_regions=True`.

This is part of the Task 17 commit, not a separate cleanup.

Commit subjects:
- `feat(strider-pattern): port cast walk-through (region walk-through dropped)`
- `feat(strider-pattern): port predicate guards (when, ordered, cap)`

---

## Task 12: PatGraph implements Template (instantiation)

**Files:**
- Create: `crates/strider-pattern/src/template/mod.rs`
- Modify: `crates/strider-pattern/src/lib.rs`

The Template trait + impl let a `PatGraph` materialise itself as IR sub-graph rooted at a new `NodeOutputId`. Today's `Pattern::try_build` (in `strider-analyze`) does this; we move it onto a separate `Template` trait so a rewrite-rule type can express the LHS-vs-RHS distinction cleanly.

- [ ] **Step 1: Define the Template trait**

```rust
// crates/strider-pattern/src/template/mod.rs

use crate::capture::Bindings;
use crate::matcher::BuildCtx;
use crate::pat_graph::PatGraph;
use strider_ir::Function;
use strider_ir::node::{NodeOutputId, NodeOutputType};

pub trait Template {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId>;
}
```

- [ ] **Step 2: Implement Template for PatGraph<Concrete>**

Topo-walk from leaves to root.  For each pattern node:

- Wildcard with capture: resolve the capture from `bindings` → `NodeOutputId`. Reuse.
- Concrete `BuildSpec`: create a fresh IR node. **API note:** the existing `strider-analyze::pattern::rewrite::rewrite_rule` uses `Function`'s node-construction surface — specifically `Graph::create_node(kind, inputs, output_kinds)` (`crates/strider-ir/src/graph/store.rs:58`) plus the `FunctionBuilder::build_*` methods for specialised kinds. `Function::make_value_node` is NOT in the IR — the plan's earlier pseudocode used it as a placeholder; **port from the real `rewrite_rule` impl** in strider-analyze rather than the snippet below.

```rust
impl Template for PatGraph {
    fn instantiate(&self, function: &mut Function, bindings: &Bindings, root_ty: NodeOutputType) -> anyhow::Result<NodeOutputId> {
        let Some(root) = self.root else { anyhow::bail!("rootless PatGraph") };
        let order = crate::pat_graph::topo_order_from_root(&self.inner, root)?;
        // Pat-node-index → output of instantiated IR node.
        let mut materialised: std::collections::HashMap<petgraph::stable_graph::NodeIndex, NodeOutputId> = Default::default();
        for pn in order {
            let nd = self.inner.node_weight(pn).unwrap();
            // 1. If this pattern node is a wildcard with a capture, resolve.
            if let Some(cap) = &nd.capture
                && nd.build_spec.is_none()
            {
                let out = bindings.output_for(*cap)
                    .ok_or_else(|| anyhow::anyhow!("capture {:?} unbound", cap))?;
                materialised.insert(pn, out);
                continue;
            }
            // 2. Otherwise instantiate via BuildSpec.
            let bs = nd.build_spec.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Template pat node has no BuildSpec and no capture"))?;
            let kind = match &bs.kind {
                crate::pat_graph::BuildKind::Exact(k) => k.clone(),
                crate::pat_graph::BuildKind::Fn(f) => {
                    let ctx = BuildCtx { function, bindings, root: /* root NodeId */ todo!(), root_ty };
                    f(&ctx)?
                }
            };
            let ty = match bs.ty {
                crate::pat_graph::BuildTy::InheritRoot => root_ty,
                crate::pat_graph::BuildTy::Fixed(t) => t,
            };
            // Collect input outputs in slot order.
            let mut inputs_by_slot: std::collections::BTreeMap<usize, NodeOutputId> = Default::default();
            for edge in self.inner.edges_directed(pn, petgraph::Incoming) {
                let producer = edge.source();
                let p_out = materialised.get(&producer).copied()
                    .ok_or_else(|| anyhow::anyhow!("producer not materialised — topo bug"))?;
                inputs_by_slot.insert(edge.weight().consumer_slot, p_out);
            }
            let inputs: Vec<NodeOutputId> = inputs_by_slot.into_values().collect();
            // PLACEHOLDER — real call is what `strider-analyze::pattern::rewrite::rewrite_rule`
            // does: `Graph::create_node(kind, inputs, [NodeOutputKind::OutputType(ty)])`
            // and then take the resulting node's single output via
            // `node_outputs_exact::<1>`.  Port that exact shape.
            let new_out = todo!("port from rewrite_rule's instantiate path; create_node + node_outputs_exact");
            materialised.insert(pn, new_out);
        }
        Ok(materialised[&root])
    }
}
```

- [ ] **Step 3: Test instantiation**

```rust
// crates/strider-pattern/tests/template_basics.rs
// Build an IR with Add(IntConst(5), IntConst(3)).
// Build a template PatGraph for Add(IntConst(8), int_const(0)).
// Bindings empty (template has no captures).
// instantiate(&mut function, bindings, I64) should return a NodeOutputId.
// Assert the IR now contains Add(IntConst(8), IntConst(0)):I64.
```

- [ ] **Step 4: Verify + commit**

Commit subject: `feat(strider-pattern): PatGraph implements Template (instantiation)`

---

## Task 13: Rewrite type + GraphRewriter (with soundness check)

**Files:**
- Create: `crates/strider-pattern/src/rewrite/mod.rs`
- Create: `crates/strider-pattern/src/rewrite/asm_fingerprint.rs`
- Modify: `crates/strider-pattern/src/lib.rs`

Source: `strider-analyze/src/pattern/rewrite.rs`. Port the file. Two API changes:

1. `rewrite_rule(lhs, rhs) -> impl Fn(...)` becomes `Rewrite::new(lhs, rhs) -> Result<Rewrite>` + `Rewrite::apply(&mut RewriteCtx, NodeId) -> Result<bool>`. The shape is the same; the change is that the construction-time soundness check lives in `Rewrite::new` and produces a typed error if LHS-root-output-type ≠ RHS-root-output-type.

2. The closure-based `BoxedRule` interface stays for `apply_rules_in_order` callers; we add a `From<Rewrite> for BoxedRule` impl so existing callsites read `BoxedRule::from(Rewrite::new(lhs, rhs)?)` (one extra `?`).

```rust
pub struct Rewrite<P: Pattern, T: Template> {
    pub lhs: P,
    pub rhs: T,
}

impl<P: Pattern, T: Template> Rewrite<P, T> {
    pub fn new(lhs: P, rhs: T) -> anyhow::Result<Self> {
        // Soundness checks (all O(pattern size)):
        //
        // 1. Root output types must agree.  Read `self.root`'s
        //    `NodeData.output_ty` on each side; when either is `None`
        //    (means "inherit from root"), accept and defer to apply.
        //
        // 2. Capture coverage: every Capture referenced by an RHS
        //    pattern node must appear in the LHS's `captures` set.
        //    Catches "the RHS uses a Capture the LHS never bound" at
        //    construction time, not at first-fire.  Implementation
        //    detail: walk `rhs.captures`, fail if any is absent from
        //    `lhs.captures`.
        //
        // Both impls today are PatGraph, so we have direct access to
        // root + captures.  When P / T diverge in a future refactor,
        // expose these via trait methods (e.g. `Pattern::captures()`
        // returning an iterator) so the check stays generic.
        Ok(Self { lhs, rhs })
    }
    pub fn apply(&self, ctx: &mut RewriteCtx<'_>, node: NodeId) -> anyhow::Result<bool> {
        // Mirror rewrite.rs's logic: match LHS → bind → instantiate RHS
        // → replace_all_uses → asm-fingerprint absorb interior nodes.
        // …
    }
}

pub struct GraphRewriter { /* … */ }
pub struct RewriteCtx<'g> { pub function: &'g mut strider_ir::Function }
// … apply_rules_in_order, BoxedRule, boxed_rule …
```

The asm-fingerprint absorption logic (the `pre_build_node_id` + walk-fresh-interior-nodes pass) is delicate. Port the existing logic verbatim into `rewrite/asm_fingerprint.rs` and have `Rewrite::apply` call it.

- [ ] **Step 1-4: TDD cycle.** Port the existing tests in `strider-analyze/tests/pattern_matching/` that exercise the rewriter.

Commit subject: `feat(strider-pattern): Rewrite + GraphRewriter ported from strider-analyze`

---

## Task 14: Cross-pattern joins (find_joined)

**Files:**
- Modify: `crates/strider-pattern/src/matcher/mod.rs`

Source: `strider-analyze::pattern::Matcher::find_joined`. Port verbatim.

Commit subject: `feat(strider-pattern): port Matcher::find_joined`

---

## Task 15: Switch strider-analyze consumers to strider-pattern

**Files:**
- Modify: `crates/strider-analyze/Cargo.toml` (add `strider-pattern` dependency)
- Modify: every `use crate::pattern::…` in `crates/strider-analyze/src/` → `use strider_pattern::…`
- Delete: `crates/strider-analyze/src/pattern/` (entirely)
- Modify: `crates/strider-analyze/src/lib.rs` (drop `pub mod pattern;` + `pub use pattern::*;`; either re-export from strider-pattern or remove the alias entirely depending on what produces the cleanest diff — see Task 15's decision below)

- [ ] **Step 1: Make sure strider-pattern feature parity is complete**

Run the strider-analyze test suite via a temporary shim: in `strider-analyze/src/pattern.rs`, replace the module with `pub use strider_pattern::*;`. Build → identify any missing re-exports (extension methods, type aliases, trait re-exports). Add them to `strider-pattern/src/lib.rs`. Loop until `cargo build -p strider-analyze` succeeds.

- [ ] **Step 2: Direct-use migration (no shim)**

Replace every `use crate::pattern::…` / `use strider_analyze::pattern::…` path with `use strider_pattern::…` directly. Per the user's stated end-state ("replace all strider-analyze/pattern"): no backward-compat shim, no `pub use` re-export bridge. Expect ~50 distinct `crate::pattern::*` sites in `opt/` (per the review's grep) plus a handful in the orchestrator and tests — all small mechanical renames. Each consumer file is its own diff inside this commit.

- [ ] **Step 3: Run all tests**

```bash
cargo test --workspace --release 2>&1 | grep -c FAILED  # must be 0
cargo clippy --workspace --all-targets --release -- -D warnings
```

- [ ] **Step 4: Commit + push**

Commit subject: `refactor(strider-analyze): swap pattern module for strider-pattern dependency`

---

## Task 16: Update strider-pattern-macros

**Files:**
- Modify: `crates/strider-pattern-macros/src/*` (find the path-emission constants for the macro target)

The proc-macro emits PyO3 mirror impls that reference `strider_analyze::pattern::*Pat` types. These now live in `strider_pattern`. Find every occurrence of `strider_analyze :: pattern` in the macro output and replace with `strider_pattern`.

Check `crates/strider-pattern-macros/EMISSION_SPEC.md` to confirm the contract.

Commit subject: `refactor(strider-pattern-macros): emit references to strider-pattern instead of strider-analyze::pattern`

---

## Task 17: Update strider-py

**Files:**
- Modify: `crates/strider-py/Cargo.toml` (add `strider-pattern` dep; drop if `strider-analyze` still re-exports as a shim)
- Modify: `crates/strider-py/src/pattern.rs` (every `strider_analyze::pattern::*` → `strider_pattern::*`)
- Verify: `.pyi` stubs regenerate correctly

```bash
cd crates/strider-py
uv run maturin develop --release
uv run pytest -q 2>&1 | tail -5    # expect: ≥837 passed, 0 failed
```

If `.pyi` content drifts, regenerate via the existing `stub_gen.py` workflow and commit alongside.

Commit subject: `refactor(strider-py): point pattern bindings at strider-pattern`

---

## Task 18: Documentation updates

**Files:**
- Modify: `CLAUDE.md` (Crate Inventory section + Pattern DSL section)
- Modify: `README.md` (if it references the pattern crate)

Update the "Crate Inventory" section in CLAUDE.md to list `strider-pattern` and re-describe `strider-analyze`'s role (no longer hosts the pattern subsystem). Update the dependency graph diagram. Update the Pattern DSL section's prose to point at `strider_pattern::pattern` instead of `strider_analyze::pattern`.

Commit subject: `docs: document the new strider-pattern crate`

---

## Task 19: Differential test pass (REQUIRED)

**Files:**
- Create: `crates/strider-pattern/tests/differential.rs`

Pick a handful of real fixtures from `fixtures/` and run a side-by-side comparison: for each fixture, build it with both the old matcher (snapshotted from `rewrite/strider` tip via a temporary git worktree) and the new matcher. Verify match counts and captured `NodeOutputId`s agree.

This task is **required**, not optional. Reason: ~100 rewrite rules depend on the matcher's exact behaviour (commutative retry, capture rebinding under cast walk-through, the `RewriteSkip` semantics, Region walk-through's removal). Python tests exercise the IR shapes the optimiser produced *with the old matcher* — that's the wrong baseline for verifying matcher equivalence. Differential testing against a frozen old-matcher snapshot is the only way to catch silent drift.

Minimum-viable differential:
- 3-5 representative fixtures from `fixtures/cases/` (including the switch fixture, a tight-loop, an arithmetic-heavy one).
- For each, lift via the orchestrator (same options on both sides), then run a representative pattern set from `strider-analyze/src/opt/constant_fold/rules.rs`.
- Compare match-count and per-match captured `NodeOutputId` sets.
- Diff > 0 fails the test with the offending pattern + fixture.

Commit subject: `test(strider-pattern): differential check vs prior matcher`

---

## Task 20: Final review + ff-merge into rewrite/strider

- [ ] **Step 1: Confirm everything green at the branch tip**

```bash
cargo test --workspace --release 2>&1 | grep -c FAILED       # 0
cargo clippy --workspace --all-targets --release -- -D warnings
cd crates/strider-py && uv run maturin develop --release && uv run pytest -q
cd ../..
```

- [ ] **Step 2: Final code review via subagent (per `feedback_skills_always`)**

Dispatch a `pr-review-toolkit:code-reviewer` subagent on the diff `git diff rewrite/strider..feature/strider-pattern-crate`. Address any P0/P1 findings.

- [ ] **Step 3: ff-merge**

```bash
git checkout rewrite/strider
git pull origin rewrite/strider
git merge --ff-only feature/strider-pattern-crate
cargo test --workspace --release 2>&1 | grep -c FAILED       # 0 (post-merge gate)
git push origin rewrite/strider
```

- [ ] **Step 4: Delete the feature branch (local + remote)**

```bash
git branch -d feature/strider-pattern-crate
git push origin --delete feature/strider-pattern-crate
```

- [ ] **Step 5: Verify final state**

```bash
git log --oneline -10
git branch
```

The branch list should be `feature/ai` (main) + `rewrite/strider` (development) only.

---

## Risks + Mitigations

| Risk | Mitigation |
|------|------------|
| `merge_subgraph` cost per rule | `NodeData` is move-only — `merge_subgraph` consumes the child graph via `into_nodes_edges_iters`, moving each NodeData into the parent.  No clones, no Arc traffic.  Allocation pressure is `O(rule_size)` Vec growth, identical to today's tree-based Pat construction. |
| Move-only `Pat<R>` blocks shared sub-builders in `LazyLock<Vec<BoxedRule>>` | Existing rule registration in `opt/constant_fold/rules.rs` etc. builds each rule once inside the LazyLock initialiser; there's no cross-rule Pat re-use that requires Clone.  If a callsite genuinely needs sharing later, add a deliberate `Pat::deep_copy() -> Pat<R>` method (one-line wrapper around `merge_subgraph` into a fresh parent) and call it explicitly.  Do NOT bake Arc/Rc into the type. |
| `IntoPat` trait dropped — today's builders take `impl Into<Pat>` for their args | After the swap, every builder takes `Pat<R>` directly (where `R` is a generic parameter, propagated via `Combine`).  Existing call sites that today pass a builder struct (e.g. `mul_pat.into()`) become direct passes since builder structs are gone.  String-keyed captures (the `"_"` / `"any_"` reserved names) still work via the explicit `cap(name)` chainer. |
| Kind-index optimisation deferred | The MVP scans all reachable IR nodes per `find_all`, filtered by discriminant.  Per `project_phase6_perf_reality` this could matter.  Budget the lazy `FxHashMap<Discriminant<NodeKind>, Vec<NodeId>>` kind index as an explicit follow-on in Task 14 (cross-pattern joins) rather than a Task 20 afterthought — landing it next to find_joined where the perf cost is most visible. |
| Soundness check at `Rewrite::new` time can't see runtime `BuildTy::InheritRoot` since root_ty is determined at apply time | Defer the equality check to `Rewrite::apply` for the `InheritRoot`-on-both-sides case. Document in the type's rustdoc. |
| Cast walk-through cast-mask semantics are subtle (today's logic handles Trunc/Ext interleaving) | Port the cast-side logic from `strider-analyze/src/pattern/matcher/walk_through.rs` verbatim. The Region-side logic is deliberately not ported (per the design decision in Task 11). |
| Some existing rules / pass code rely on `ignore_regions = true` for Region walk-through | During Task 15, grep for `ignore_regions` callers; each needs the underlying pattern updated to include the Region explicitly. Failing tests after the swap surface any that were missed. |
| `petgraph::StableDiGraph` indices don't compact automatically (deleted nodes leave holes); pattern graphs never delete nodes, but if a builder accidentally does, indices stay valid | No-op for now (we never delete). Add a debug-assert in `Pat::from_graph` if needed. |
| 100+ existing rewrite rules need to keep working — any subtle behaviour change breaks the optimiser | Each task's gate is the *full* `cargo test --workspace`; tests will catch divergence. Plus differential test in Task 19. |
| `find_all`'s "iterate all reachable nodes and filter by discriminant" replacement for the kind index is `O(n)` per query; today's kind index is `O(matches)` after the index build | Acceptable for the MVP since pattern queries today already iterate. Land an internal kind index (lazy `FxHashMap<Discriminant<NodeKind>, Vec<NodeId>>`) as a follow-up only if the post-Task-17 benchmark shows regression. |

---

## Self-Review

(Run after the plan is written, before handoff to the user.)

- **Spec coverage:** Every item from the user's prompt — graph storage (Task 2), Pattern/Template traits (Tasks 4 + 12 + 13), chained-builder migration (Tasks 7-11), Python API (Task 17), rewrite/build/query support (Tasks 13 + 12 + 6), new crate `strider-pattern` (Task 1), replace strider-analyze::pattern (Task 15), DAG assumption (Task 2's `assert_dag` + Task 6's topological walk), branch + merge workflow (Task 20) — has a task that addresses it.
- **Placeholder scan:** Tasks 6, 12, 13 have `todo!()` / "port verbatim" instructions rather than full code. These reference an existing file in strider-analyze as the SPEC for the port — the implementer reads the source file and translates the API. This is deliberate (porting hundreds of lines verbatim into the plan doc would balloon it past readability) and matches the writing-plans skill's "complete code in every step — if a step changes code, show the code" — for *novel* code we show full snippets; for *ports*, we point at the canonical source.
- **Type consistency:** `Pat`, `PatGraph`, `Pattern`, `Template`, `Bindings`, `Capture`, `Match`, `Rewrite`, `MatchCtx`, `BuildCtx`, `KindSpec`, `NodeData`, `EdgeData`, `BuildSpec`, `BuildKind`, `BuildTy` — names used consistently throughout. `Pat` is the public newtype `Arc<PatGraph>`; `PatGraph` is the internal storage; the chained builders return `Pat`.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-01-strider-pattern-rewrite.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, two-stage review (spec compliance + code quality) between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints for review.

Which approach?
