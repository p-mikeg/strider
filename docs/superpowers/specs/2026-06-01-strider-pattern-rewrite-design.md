# strider-pattern rewrite — design

Date: 2026-06-01
Status: approved-for-planning (pending user review of this doc)
Branch: `develop` → merge to `master` when green

## 1. Why

`strider-pattern` is the sea-of-nodes pattern + template crate (LHS matching,
RHS templates, rewrite rules) consumed by `strider-analyze` (optimizer passes)
and `strider-py` (Python bindings, via `strider-pattern-macros`). This is a
**complete rewrite of the pattern layer** driven by eight goals from the owner:

1. **Stronger compile-time typing.** Replace the phantom-`Role`/`Combine`
   lattice with real trait bounds: typed builder structs that `compile()`
   themselves into a graph builder, so "this pattern is buildable as a
   rewrite RHS" is a compile error when violated, not a runtime check.
2. **Pattern graph mirrors the IR graph.** The IR is `Node → NodeOutput →
   NodeInput → Node`. The pattern graph must mirror it with **real output
   vertices** (`PatNode → PatOutput → PatNode`), and the structural
   input/output predicates (output type, width) become **declarative
   constraints on those output vertices** rather than ad-hoc closures.
3. **Remove string-keyed captures from the core.** `Capture::named` / `.cap(name)`
   leave `strider-pattern`; only opaque `Capture::new()` + `.capture(c)` remain.
4. **Post-match stays per node** (revised from the original "one global
   post-match"). The binding-aware predicate stays attached to a pattern node;
   what changes is that the *simple structural* predicates move out of closures
   into declarative output-vertex constraints. (Rationale in §7.)
5. **Cast mask belongs to the pattern, not the matcher.** One cast-walk-through
   mask per whole pattern, configured on the pattern, read by the matcher.
6. **Collapse the `Pat<R>(PatGraph<R>)` duplication** into one internal type.
7. **Use the graph library's algorithms** (e.g. `petgraph::algo::toposort` /
   `DfsPostOrder`) instead of hand-rolled traversal.
8. **Remove unneeded / inefficient code** — e.g. `Bindings::restore` can be a
   pure `truncate`; sweep the `#[allow(dead_code)]` scaffolding.

## 2. Current state (ground truth)

- `Pat<R>(PatGraph<R>)` wraps a `petgraph::StableDiGraph<NodeData, EdgeData>`.
  Edges are **node→node**, carrying `EdgeData { consumer_slot,
  producer_output_slot }`; **there is no vertex for an output** — a producer's
  output is implicit in the edge.
- Per-node constraints are closures on `NodeData`: `output_ty:
  Option<NodeOutputType>`, `node_filter` (pre-match, node-only, no bindings),
  `post_match` (post-match, binding-aware). `value_of_width` /
  `inputs_of_width` are `node_filter` closures that re-derive widths from the
  matched IR node at match time.
- `CastMask` lives on `Matcher` (`MatcherOptions`), set via
  `matcher.ignore_casts_mask(...)` — global to every pattern that matcher runs.
- `Role`/`Wildcard`/`Concrete`/`Combine` is the phantom lattice encoding
  "matchable" vs "matchable + buildable".
- `Capture(u32)` from a process-wide atomic. **`strider-analyze` uses only
  `Capture::new()` + `.capture(c)`** (123 `Capture::new()` sites, 0 string
  captures). The string path (`Capture::named` / `.cap`) is used **only** by
  `strider-py`, which has its own `intern_str` string→Capture table.
- `Bindings` is an append-only `Vec<(Capture, Binding)>` **plus** an
  `FxHashMap<Capture, usize>` overlay; `restore` truncates the vec *and* walks
  the dropped tail to evict overlay entries.
- Consumers (counts from a code sweep):
  - `strider-analyze`: ~73 `rewrite_rule`, ~60 `boxed_rule`, 39 `find_all`,
    13 `when_match`, 9 `match_at`, `int/bool/float_const_with!` ×18,
    `CastMask` ×27 (all on the matcher). ConstantFold is ~65% of all usage.
  - `strider-py`: wraps everything as `Pat<Wildcard>` (stored `Rc<Pat<Wildcard>>`);
    `rewrite_rule_dynamic` for rewrites; cast mask configured **on the matcher**
    (`Graph.find_all(pat, ignore_casts=...)`); string captures via `intern_str`.
  - `strider-pattern-macros`: emits `Py*Pat` PyO3 wrappers from `*Def` structs;
    each wrapper stores per-field `Option<…>`, then `finalise()` calls the
    `strider-pattern` builder free-function and chains setters in field order,
    yielding `Pat<Wildcard>`. Retargetable to the new builder layer.

## 3. Settled design decisions

Resolved through design dialogue (all confirmed by the owner):

- **D1 — real output vertices.** The pattern graph is genuinely bipartite:
  `PatNode` vertices and `PatOutput` vertices. (point 2)
- **D2 — custom limits stay as an option on a node *or* an output vertex.**
  Simple structural constraints (output type, width) are declarative fields;
  custom *local* predicates (no cross-binding state) remain attachable to
  either vertex kind. (refines point 2)
- **D3 — `If` has two control `PatOutput` vertices** (true / false), each able
  to constrain the following control node. This is what makes a future
  branch-swap rewrite (`if(not(x)){A}{B}` → `if(x){B}{A}`) expressible. (point 2)
- **D4 — post-match stays per node.** Keeping it per node preserves *local*
  commutative re-steering (a per-node predicate failure retries that node's two
  operand orders) while staying O(n); a single global post-match would have
  forced either loss of re-steering or exponential global enumeration. (revises
  point 4)
- **D5 — no value/control type separation.** A control pattern used in value
  position stays a match-time non-match, as today (not a compile error).
- **D6 — API boundary.** Typed compile-time structs for **all value-producing
  patterns** (fixed-arity ops, runtime-variant ops, wildcards); the imperative
  builder for **all control / variadic patterns** (`If`, `Ret`, `Call`,
  `CallOther`, `Phi`/`MemPhi`/`ValuePhi`). `If` is builder-based (D3's
  representation is independent of the API layer). (point 1)
- **D7 — cast mask on the pattern** (point 5); **one internal graph type**
  (point 6); **library traversal algorithms** (point 7); **truncate-only
  `restore` + dead-code sweep** (point 8); **core captures opaque-only** (point 3).

## 4. Architecture — the bipartite pattern graph

The single representation (one type replacing **both** `Pat<R>` and
`PatGraph<R>` — point 6). It is also the one public "compiled pattern" handle,
and it carries the per-pattern cast mask (point 5), so there is no second
wrapper type:

```
Pattern  (one type, no role parameter; public + internal)
  inner: petgraph::StableDiGraph<PatVertex, PatEdge>
  root: NodeIndex            // the matched / instantiated root PatNode
  cast_mask: CastMask        // point 5: one mask for the whole pattern
```

`PatVertex` is a two-variant enum mirroring the IR's node/output split:

```
enum PatVertex {
    Node(PatNode),     // mirrors strider_ir Node
    Output(PatOutput), // mirrors strider_ir NodeOutput
}
```

`PatNode` — the kind-level match/build spec for one IR node:

```
struct PatNode {
    kind: KindSpec,                  // Any | Variant(disc) | Exact(NodeKind) | VariantWith{disc, check}
    build: Option<TemplateKind>,     // Some => buildable (Exact(NodeKind) | Fn(closure)); None => match-only
    capture: Option<Capture>,        // bind the matched node / its value output
    node_limit: Option<LocalLimit>,  // D2: optional custom local predicate on the node (no bindings)
    post_match: Option<PostMatchFn>, // D4: per-node binding-aware predicate (kept)
    force_ordered: bool,             // opt out of commutative retry
}
```

`PatOutput` — one value/control/memory output slot of a `PatNode`:

```
struct PatOutput {
    slot: usize,                     // which output of the producing node
    kind: OutputKindSpec,            // Value(Option<NodeOutputType>) | Control | Memory | PhiToken | AnyValue
    width: Option<u32>,              // declarative width constraint (replaces value_of_width)
    output_limit: Option<LocalLimit>,// D2: optional custom local predicate on this output
    capture: Option<Capture>,        // (rare) bind this specific output
}
```

Edges (`PatEdge`) come in two directions, matching the IR:

- `PatNode → PatOutput` ("this node produces this output").
- `PatOutput → PatNode` carrying `consumer_slot` ("this node consumes that
  output at input slot *i*").

So a binary op `add(L, R)` is: `Lnode → Lout → AddNode ← Rout ← Rnode`, with
`AddNode → AddOut` for the produced value. `If` is: `cond producer → condOut →
IfNode`, plus `IfNode → trueCtrlOut` and `IfNode → falseCtrlOut`, each of which
may have a consumer edge to a following-control `PatNode`.

`LocalLimit` is the surviving node-only closure (today's `node_filter`),
re-typed so it can hang off either a node or an output vertex:

```
type LocalLimit = Box<dyn Fn(&Matcher, NodeId, NodeOutputType) -> bool>;
```

`PostMatchFn` (binding-aware, per-node) is unchanged in spirit:

```
type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, NodeOutputType, &Bindings) -> bool>;
```

Width/type predicates that were closures (`value_of_width`, `inputs_of_width`,
`output_ty`) become declarative: `value_of_width(n)` sets `PatOutput.width`;
`output_ty` sets `PatOutput.kind = Value(Some(ty))`; `inputs_of_width(n, inner)`
sets `width` on each of `inner`'s consumed input `PatOutput`s. Genuinely custom
predicates use `LocalLimit` on the relevant vertex.

## 5. API layer 1 — typed builder structs (Rust production, point 1)

Two traits replace `Role`/`Combine`:

```
trait MatchPat {
    /// Lower self into the matcher builder; return the produced value output.
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef;
}

trait TemplatePat {
    /// Lower self into the template builder; return the produced value output.
    fn compile(self, b: &mut TemplateBuilder) -> TemplateOutRef;
}
```

Every value-producing pattern is a struct generic over its operands' *types*;
buildability is carried by the `TemplatePat` bound on the operands:

```
struct Add<L, R> { lhs: L, rhs: R }

impl<L: MatchPat, R: MatchPat> MatchPat for Add<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.binary(IntBinaryOp::Add, l, r)       // builder is the SSoT
    }
}
impl<L: TemplatePat, R: TemplatePat> TemplatePat for Add<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TemplateOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.binary(IntBinaryOp::Add, l, r)
    }
}
```

Consequences (the whole point of point 1):

- `Var(c)` implements **both** `MatchPat` and `TemplatePat` (a capture resolves
  to a bound output at template time). `Any` / `Predicate` / `AnyIntConst` /
  `*_any` implement **only `MatchPat`**. So any tree containing an `Any` is not
  `TemplatePat` → using it as a rewrite RHS is a **compile error**, exactly what
  the phantom lattice approximated at runtime.
- Free functions stay the ergonomic surface and just construct the struct:
  `pub fn add<L, R>(lhs: L, rhs: R) -> Add<L, R> { Add { lhs, rhs } }`. Call
  sites read identically to today (`add(var(x), int_const(1))`).
- Runtime-variant ops are typed structs with the variant as a *field*:
  `IntBinary<L, R> { op: IntBinaryOp, lhs, rhs }`, `Extend<I> { op, inner }`.
- Match-only families (`IntBinaryAny<L, R>`, `AnyIntConst`, …) impl `MatchPat`
  only.
- Per-node combinators (`.capture(c)`, `.node_limit(f)`, `.post_match(f)`,
  `.ordered()`) are provided by small wrapper structs (e.g.
  `Captured<P>`, `Limited<P>`, `Guarded<P>`) that forward `compile` to the inner
  pattern and then annotate the produced root vertex. `Captured<P>` keeps the
  inner's `TemplatePat`-ness; `Guarded<P>` / `Limited<P>` are `MatchPat`-only
  (a custom predicate has no template form) — again enforced by trait impls,
  not a phantom.

`Capture` stays an opaque `Capture(u32)` from `Capture::new()`. No strings in
the core (point 3).

## 6. API layer 2 — imperative builders (the SSoT)

`MatcherBuilder` and `TemplateBuilder` own a `PatGraph` under construction and
expose primitive operations the typed structs lower onto, **and** the public
surface for control / variadic patterns:

```
impl MatcherBuilder {
    fn leaf(&mut self, kind: KindSpec) -> PatOutRef;
    fn unary(&mut self, kind, inner: PatOutRef) -> PatOutRef;
    fn binary(&mut self, op, l: PatOutRef, r: PatOutRef) -> PatOutRef;
    fn node(&mut self, kind: KindSpec) -> PatNodeRef;          // variadic start
    fn input(&mut self, node: PatNodeRef, slot, prod: PatOutRef);
    fn output(&mut self, node: PatNodeRef, slot, kind) -> PatOutRef;
    fn control_output(&mut self, node, slot) -> PatCtrlRef;    // If true/false
    // …capture/limit/post_match annotators…
}
```

Control / variadic builders (`call()`, `call_other()`, `ret()`, `if_node()`,
`phi()`, `mem_phi()`, `value_phi()`) are thin fluent wrappers over
`MatcherBuilder` (and `TemplateBuilder` where buildable). They keep today's
slot conventions (Call `[ctrl, mem, target, args…]`, etc.) and today's
sparse-slot ergonomics (`.arg(i, p)`, `.cond(p)`, `.at(addr)`).

`If` (D3): `if_node().cond(p).with_true(q).with_false(r)` builds an `If`
`PatNode` with two control `PatOutput` vertices; `with_true`/`with_false`
attach following-control consumer edges. Branch-follow keeps today's
"single-consumer-or-no-match" semantics.

The Python FFI lowers onto these *same* builders (so the macro retargets to
`MatcherBuilder`/`TemplateBuilder` calls instead of the old chained
free-functions), producing a runtime-checked `Pattern`.

## 7. Matcher engine

`Matcher<'f>` wraps a `&Function` and a lazy kind→nodes index (unchanged). It no
longer owns a cast mask — it reads `pattern.cast_mask` per query (point 5).

Matching walks the bipartite graph in pull order from `root`:

1. At a `PatNode`: kind-check against the IR node; run the node's
   `node_limit` (D2) if any (cheap short-circuit before recursion).
2. For each consumed input `PatOutput`: resolve the IR producer output at
   `consumer_slot`; check the output vertex's declarative constraints
   (`kind`/`width`) and its `output_limit` (D2); recurse into the producer
   `PatNode`. On a producer kind-mismatch, apply `pattern.cast_mask`
   walk-through (unwrap registered casts, retry).
3. Commutative arity-2 nodes try natural order, then (unless `force_ordered`)
   the swapped order, via `Bindings::mark`/`restore`.
4. Bind the node's `capture` (capture-equality enforced by `Bindings`).
5. Run the node's `post_match` (D4) if any; failure rejects and triggers the
   node's commutative retry (local re-steering, bounded).

**Why per-node post-match (D4) over one global post-match.** A per-node
predicate failure re-tries *that node's two operand orders* — local, bounded
re-steering that callers like `and(var(x), any_int_const().capture(c))` rely on.
A single end-of-pattern predicate would run after all commutative choices
committed, so it could only accept/reject (losing re-steer) or force the matcher
to enumerate every structural assignment per root (multiplicative in nested
commutative depth — violates the O(n)/O(n log n) scalability rule). Keeping it
per node gives the re-steering power at O(n).

`find_all` / `find_first` / `match_at` / `find_joined` / `find_all_multi` keep
their signatures, except cast configuration moves from `Matcher` to `Pattern`.

## 8. Captures & Bindings (point 8)

- Core `Capture`: opaque `Capture::new()` only. Delete `Capture::named` and the
  process-wide string table. `strider-py` keeps its own `intern_str` →
  `Capture` map behind the FFI (string ergonomics survive in Python only).
- `Bindings` simplification (point 8): drop the `FxHashMap<Capture, usize>`
  overlay; store a plain `Vec<(Capture, Binding)>`. `mark` → length; `restore`
  → **`Vec::truncate`** (no tail walk, no overlay eviction). `bind_capture` /
  `get_binding` become a linear scan over the vec. Capture counts per match are
  tiny (single digits in every real pattern), so linear is faster in practice
  and far simpler; this keeps per-match work O(captures) and total matching
  O(n). Typed extractors (`get_uint`, `get_int_binary_op`, …) are unchanged.

## 9. Templates & rewrites

- `TemplatePat::compile` lowers a buildable typed struct onto `TemplateBuilder`;
  the dynamic `*_const_with!` family stays as `TemplateKind::Fn` closures
  receiving a `TemplateCtx` (captured bindings + matched root + `&mut`/`&`
  function), exactly as today.
- `instantiate` walks the pattern graph in `petgraph::algo::toposort` order
  (point 7), resolving captures through `Bindings`, materialising each buildable
  `PatNode`, wiring inputs by `consumer_slot`.
- The asm-fingerprint absorption contract is preserved verbatim: snapshot
  `next_node_id` before build, then `extend_asm_fingerprint_from` every
  freshly-allocated interior node from the rewrite root (superset-only).
- `rewrite_rule(lhs, rhs)` takes `lhs: impl MatchPat`, `rhs: impl TemplatePat`
  — buildability is now a **compile-time bound**, so `rewrite_rule_dynamic` +
  `assert_concrete_at_runtime` collapse into a single runtime-checked entry used
  only by the FFI (`rewrite_rule_runtime(lhs: Pattern, rhs: Pattern)`).
  Construction-time soundness checks (capture coverage, root-type agreement) are
  kept. `GraphRewriter` / `RewriteCtx` / `boxed_rule` / `apply_rules_in_order`
  keep their shapes so the optimizer passes need only import-path + builder-call
  changes.
- D3 keeps the door open for control-rewiring rewrites (branch swap) by modeling
  `If`'s two control outputs as vertices; **implementing** branch-swap as a rule
  is explicitly out of scope here (the representation must merely not preclude
  it).

## 10. Library algorithms (point 7)

Replace `topo_order_from_root` / `assert_dag` with `petgraph::algo::toposort`
(cycle → error) over the reachable-from-root subgraph (via
`petgraph::visit::DfsPostOrder` to scope reachability), used by both
`instantiate` and the construction-time DAG assertion. Reuse petgraph's
`edges_directed` / `EdgeRef` everywhere instead of bespoke adjacency scans.

## 11. Consumer migration

- **strider-analyze** (largest surface): ConstantFold (~73 rules), the
  indirect-branch resolver (`match_at` + `get_uint`/`output`), FlagCmpCanonicalize,
  IfCondInversion. Call sites mostly change shape minimally (free functions
  keep their names; `Capture::new()` + `.capture(c)` unchanged). The 27
  matcher-level `CastMask` configurations move onto the pattern. The 13
  `when_match` closures map to per-node `post_match` (D4) — no behavioural
  change. Width/type filters (`value_of_width`/`inputs_of_width`/`output_ty`)
  become declarative output-vertex constraints.
- **strider-py** + **strider-pattern-macros**: retarget the macro's `finalise()`
  to emit `MatcherBuilder`/`TemplateBuilder` calls; keep the per-field `Option`
  accumulation model. `Pat<Wildcard>` storage becomes `Rc<Pattern>`. String
  captures stay behind `intern_str`. Cast mask moves from the matcher call onto
  the pattern object (`pat.ignore_casts(...)` / a `CastMask` kwarg folded into
  the built `Pattern`). The flat `StriderError` mapping is unchanged.

## 12. Non-goals / out of scope

- Implementing the `If` branch-swap rewrite rule (representation only).
- Changing the IR, the optimizer pass *set*, or the orchestrator pipeline.
- Adding new pattern kinds beyond what exists today.
- Multi-threading / `Arc`/`Send`/`Sync` (workspace stays single-threaded; `Box`
  for owned closures, `Rc` only behind the Python FFI).
- Re-deriving `NodeOutputType` shape (explicitly deferred per project memory).

## 13. Testing strategy

- TDD throughout; the crate's existing `tests/` suite is the behavioural
  contract to preserve. Port every test to the new API; a test that can't be
  expressed is a design gap to resolve, not delete.
- Per-crate `cargo test -p strider-pattern`, then `-p strider-analyze`, then
  `-p strider-py` (avoid `--workspace` for signal speed).
- Regression bar (project memory): the 4 pre-existing v2 baseline failures are
  allowed; **no new failures**. ConstantFold output must be unchanged
  node-for-node on the fixtures (it dominates real-binary output).
- Add focused unit tests for: output-vertex width/type constraints, `If`
  two-control-output representation, per-pattern cast mask, compile-time
  `TemplatePat` rejection of `Any` (a `trybuild`-style compile-fail test if
  feasible, else documented), and `Bindings` truncate-restore.

## 14. Risks

- **Breadth.** This touches three crates + the macro. Mitigation: land
  `strider-pattern` internals + typed API behind the same test suite first;
  migrate consumers crate-by-crate; keep free-function names stable to minimize
  call-site churn.
- **Macro retarget.** The proc-macro must emit builder calls; the per-field
  model survives but the `finalise()` body changes. Mitigation: do one wrapper
  by hand first to pin the target shape, then retarget the macro.
- **Bipartite walk performance.** Doubling vertex count (outputs as vertices)
  must stay O(n). Mitigation: outputs are 1–2 per node; the kind index and
  reachable walk are unchanged in order; benchmark against current `find_all`.
- **Commutative + per-node post-match parity.** D4 keeps today's semantics;
  port the commutative-retry tests verbatim to lock it.
```
