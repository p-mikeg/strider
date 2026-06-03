# strider-pattern Tidy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five focused cleanups in `strider-pattern` (+ uv tooling) that restore node/value
symmetry, rename and tighten the predicate types, remove redundant enum state, and derive
the pattern root structurally instead of storing it.

**Architecture:** The crate models patterns/templates as a bipartite `BiGraph<N, O>` of node
vertices and output (value) vertices. The match side (`PatNode`/`PatValue`) and build side
(`TmplNode`/`TmplOutput`) must stay symmetric: node vertices carry *node* data, output
vertices carry *value* data (type/width/kind). These tasks push state to the side it belongs.

**Tech Stack:** Rust 2024, `petgraph::StableDiGraph`, `anyhow`. Tests are in-crate `#[cfg(test)]`
plus `strider-analyze` rule tests and `strider-py` pytest as integration coverage.

**Baseline:** `strider-pattern` = 302 passed, 0 failed (recorded at worktree creation).

---

## Item → Task map

| Item | Task |
|------|------|
| ④ collapse `Value(Option<ValueType>)` → `Value(ValueType)` | Task A |
| ① move `TemplateTy` from `TmplNode` to `TmplOutput` | Task B |
| ② `LocalLimit` → `NodePredicate`/`ValuePredicate`, drop `ValueType` arg | Task C |
| ③ derive root as unique sink (guard >1 sink / input≠root) | Task D |
| ⑤ uv at repo root | Task E |

Execution order: **A → B → C → D → E** (A and B both touch `OutputKindSpec`/template output
vertices, so A lands first to settle the enum; C and D are independent of A/B; E is wholly
independent). Each task: red test → green → commit, then push.

---

### Task A: collapse `OutputKindSpec::Value(Option<ValueType>)` → `Value(ValueType)`

**Why:** `OutputKindSpec` already has `AnyValue`, and `output_ok` treats `Value(None)`
*identically* to `AnyValue`. `Value(None)` is dead-equivalent state.

**Files:**
- Modify: `crates/strider-pattern/src/pattern/vertex.rs` (enum + `PatValue::value`)
- Modify: `crates/strider-pattern/src/matcher/walk.rs:333-348` (`output_ok`), `:91-101`
- Modify: `crates/strider-pattern/src/template/graph.rs:72-77` (`TmplOutput::value`)
- Modify: `crates/strider-pattern/src/template/mod.rs:215-243` (`output_kinds_for`)
- Modify: `crates/strider-pattern/src/builder/mod.rs:105-107` (`set_value_ty`)

- [ ] **Step 1: Write the failing test** in `vertex.rs` `#[cfg(test)]` (add module if absent):

```rust
#[test]
fn value_kind_has_no_optional_type() {
    // A bare value output is AnyValue; a pinned one is Value(ty).
    assert!(matches!(PatValue::value(0).kind, OutputKindSpec::AnyValue));
}
```

- [ ] **Step 2: Run** `cargo test -p strider-pattern value_kind_has_no_optional_type` — expect
  compile error (`PatValue::value` still builds `Value(None)`) or assertion fail.

- [ ] **Step 3: Change the enum variant** in `vertex.rs`:
  `Value(Option<ValueType>)` → `Value(ValueType)`. Update the doc comment.

- [ ] **Step 4: Route the old `None` constructors to `AnyValue`:**
  - `PatValue::value` → `kind: OutputKindSpec::AnyValue`
  - `TmplOutput::value` → `kind: OutputKindSpec::AnyValue`
  - `set_value_ty` → `OutputKindSpec::Value(ty)` (drop the `Some`)

- [ ] **Step 5: Update the three match sites:**
  - `walk.rs` `output_ok`: `Value(Some(ty)) => val == Some(*ty)` becomes `Value(ty) => val == Some(*ty)`;
    delete the `Value(None)` alternative from the `AnyValue | Value(None)` arm (now just `AnyValue`).
  - `walk.rs` `root_requires_value_output`: `Value(_) | AnyValue` stays valid (still both).
  - `template/mod.rs` `output_kinds_for`: `Value(_) | AnyValue | Any` stays valid.

- [ ] **Step 6: Run** `cargo test -p strider-pattern` — expect 303 passed, 0 failed.

- [ ] **Step 7: Commit & push**
```bash
git add -A && git commit -m "refactor(strider-pattern): collapse value output kind to Value(ValueType), drop redundant Value(None)"
git push origin refactor/strider-pattern-tidy
```

---

### Task B: move `TemplateTy` from `TmplNode` to `TmplOutput`

**Why:** node vertices should hold node-data only. The value output *type* belongs on the
value (output) vertex, mirroring the match side where `PatValue` carries width/type and
`PatNode` carries none.

**Files:**
- Modify: `crates/strider-pattern/src/template/graph.rs` (`TmplNode`, `TmplOutput`, ctors)
- Modify: `crates/strider-pattern/src/template/mod.rs` (`instantiate`, `output_kinds_for`)
- Modify: `crates/strider-pattern/src/template/builder.rs` (`set_value_ty`/`set_inherit_root_ty` target the output vertex)
- Check: `crates/strider-pattern/src/typed/consts.rs:280-293` (`ConstWith::compile` sets ty)

- [ ] **Step 1: Write the failing test** in `template/graph.rs` `#[cfg(test)]`:

```rust
#[test]
fn template_output_carries_ty_not_node() {
    let o = TmplOutput::value(0);
    assert!(matches!(o.ty, TemplateTy::InheritRoot));
}
```

- [ ] **Step 2: Run** the test — expect compile error (`TmplOutput` has no `ty`).

- [ ] **Step 3: Move the field.** `TmplOutput` gains `ty: TemplateTy` (default `InheritRoot`
  in `value`/`memory`/`control` ctors — memory/control ignore it). `TmplNode` drops `ty`;
  `TmplNode::buildable` no longer sets it. Update both doc comments (node = node-data only).

- [ ] **Step 4: Re-point the builder verbs.** `TemplateBuilder::set_value_ty` /
  `set_inherit_root_ty` write the **output vertex's** `ty` (via `output_weight_mut`) instead
  of the node's. Confirm `ConstWith::compile` still compiles (it calls these on the leaf's
  value output handle).

- [ ] **Step 5: Resolve ty per-output in `instantiate`.** Replace the node-level
  `let ty = match nd.ty { ... }` with a helper that resolves the **value output vertex's** ty
  against `root_ty`. `output_kinds_for` reads each output vertex's own `ty` (memory/control
  outputs keep their kind; value outputs use resolved ty). The `TemplateKind::Fn` `root_ty`
  passed to `TemplateCtx` is the resolved ty of the node's (single) value output vertex.

- [ ] **Step 6: Run** `cargo test -p strider-pattern` — expect green. Then
  `cargo test -p strider-analyze --lib opt::constant_fold` (rule RHS instantiation uses
  `int_const_with`) — expect green.

- [ ] **Step 7: Commit & push**
```bash
git add -A && git commit -m "refactor(strider-pattern): move template output type onto the output vertex"
git push origin refactor/strider-pattern-tidy
```

---

### Task C: split `LocalLimit` into `NodePredicate` / `ValuePredicate`

**Why:** one `Fn(&Matcher, NodeId, ValueType)` type backs both a node-level limit and an
output-level limit, and the `ValueType` arg is redundant (closures can derive it). Split by
the entity each constrains: node predicate keyed on `NodeId`, value predicate keyed on `ValueId`.

**Scope guard:** `when_match`/`PostMatchFn` is a *separate* post-match hook whose `ValueType`
arg **is** used by `constant_fold` rules — leave it untouched.

**Files:**
- Modify: `crates/strider-pattern/src/pattern/vertex.rs` (types + fields)
- Modify: `crates/strider-pattern/src/pattern/mod.rs:12` (re-exports)
- Modify: `crates/strider-pattern/src/matcher/walk.rs:169-200` (evaluation)
- Modify: `crates/strider-pattern/src/builder/mod.rs:142-144` (`set_node_predicate`, add `set_value_predicate`)
- Modify: `crates/strider-pattern/src/match_pat.rs:65-80,162-168` (`Limited`/`filter`)
- Modify: `crates/strider-pattern/src/typed/wildcards.rs:73-78,114` (`predicate`, `inputs_of_width`)
- Modify: `crates/strider-pattern/src/typed/consts.rs:31-40,76-78` (closures drop `ty` arg)
- Modify: `crates/strider-pattern/src/control/{phi.rs,flow.rs,memory.rs,function_arg.rs,node_pat.rs}` (closure sigs)

- [ ] **Step 1: Write the failing test** in `vertex.rs` `#[cfg(test)]`:

```rust
#[test]
fn predicate_types_drop_value_type() {
    // NodePredicate: (&Matcher, NodeId) -> bool ; ValuePredicate: (&Matcher, ValueId) -> bool
    fn _assert_node(_: &NodePredicate) {}
    fn _assert_value(_: &ValuePredicate) {}
}
```

- [ ] **Step 2: Run** — expect compile error (`NodePredicate`/`ValuePredicate` undefined).

- [ ] **Step 3: Define the split types** in `vertex.rs`, delete `LocalLimit`:
```rust
pub type NodePredicate = Box<dyn Fn(&Matcher, NodeId) -> bool>;
pub type ValuePredicate = Box<dyn Fn(&Matcher, ValueId) -> bool>;
```
  Rename `PatNode.node_limit: Option<LocalLimit>` → `node_predicate: Option<NodePredicate>`;
  `PatValue.output_limit: Option<LocalLimit>` → `value_predicate: Option<ValuePredicate>`.
  Update `pattern/mod.rs` re-export (`LocalLimit` → `NodePredicate, ValuePredicate`).

- [ ] **Step 4: Update evaluation in `walk.rs`:**
  - node predicate: `if let Some(p) = &nd.node_predicate { if !p(matcher, ir_node) { return false } }`
    (drop the `ty` derivation block).
  - value predicate: evaluate against the matched `ValueId`:
    `if let Some(p) = &ov.value_predicate { if !p(matcher, value) { return false } }`.

- [ ] **Step 5: Update builder verbs** (`builder/mod.rs`): `set_node_limit` → `set_node_predicate`
  (`Box<dyn Fn(&Matcher, NodeId) -> bool>`); add `set_value_predicate` setting the output vertex's
  field. `node_pat.rs` `with_node_limit`/`NodeLimitFactory` rename to `with_node_predicate`/
  `NodePredicateFactory`.

- [ ] **Step 6: Update closure call sites** to the `(m, node)` shape, deriving ty from the node
  where needed:
  - `consts.rs` `IntConst`: derive output ty from the node's own first value output (as
    `SignedIntConst` already does) and mask with it.
  - `consts.rs` `SignedIntConst`, `phi.rs` `phi_var_limit`, `wildcards.rs` `inputs_of_width_check`,
    `function_arg.rs`, `flow.rs`, `memory.rs`: drop the `_ty` parameter.
  - `wildcards.rs` `predicate(f)`: change `F: Fn(&Matcher, ValueType)` →
    `F: Fn(&Matcher, NodeId)`; implement as `any().filter(f)`.
  - `match_pat.rs` `Limited`/`filter`: `F: Fn(&Matcher, NodeId) -> bool`.

- [ ] **Step 7: Run** `cargo test -p strider-pattern` then `cargo test -p strider-analyze`
  then `(cd crates/strider-py && uv run pytest -q)` — all green. (If a `predicate(...)` call site
  in analyze/py used `ty`, convert it to derive ty from the node; if it can't, STOP and report.)

- [ ] **Step 8: Commit & push**
```bash
git add -A && git commit -m "refactor(strider-pattern): split node/value predicates, drop redundant ValueType arg"
git push origin refactor/strider-pattern-tidy
```

---

### Task D: derive the root as the unique structural sink

**Why:** `finish` already discards the chosen output identity and stores only the producer
*node*. The root is recoverable from the graph: the unique node none of whose produced
outputs are consumed. Storing it is redundant and lets a caller mis-root.

**Behaviour (per user):**
- Derive root = the unique sink node.
- **>1 sink → raise an error** (don't paper over it).
- **input passed at seal whose producer ≠ derived sink → bail with an error.**
- If the existing suites trip either guard on a real pattern, STOP and report.

**Files:**
- Modify: `crates/strider-pattern/src/bigraph/mod.rs` (replace `root` field + `set_root` with `derive_root`)
- Modify: `crates/strider-pattern/src/pattern/mod.rs` (`root()` derives; drop `set_root`; update tests)
- Modify: `crates/strider-pattern/src/template/graph.rs` (`Template::root()` derives)
- Modify: `crates/strider-pattern/src/builder/mod.rs` (`finish`/`finish_node` validate input vs derived)
- Modify: `crates/strider-pattern/src/template/builder.rs:170` (seal validates)

- [ ] **Step 1: Write the failing test** in `bigraph/mod.rs` `#[cfg(test)]`:

```rust
#[test]
fn derive_root_finds_unique_sink() {
    let mut g: BiGraph<&str, u32> = BiGraph::new();
    let x = g.add_node("x");  let xout = g.add_output(x, 10);
    let sum = g.add_node("sum"); g.consume(sum, 0, xout); let _ = g.add_output(sum, 12);
    assert_eq!(g.derive_root().unwrap(), sum);
}

#[test]
fn derive_root_rejects_two_sinks() {
    let mut g: BiGraph<&str, u32> = BiGraph::new();
    let a = g.add_node("a"); let _ = g.add_output(a, 1);
    let b = g.add_node("b"); let _ = g.add_output(b, 2); // two unconsumed-output nodes
    assert!(g.derive_root().is_err());
}
```

- [ ] **Step 2: Run** — expect compile error (`derive_root` undefined).

- [ ] **Step 3: Implement `derive_root`** in `bigraph/mod.rs`. A *sink* is a node vertex none of
  whose produced output vertices have an outgoing `Consumes` edge. Zero-output nodes
  (`Return`/`If`) are sinks (vacuously). Exactly one must exist:
```rust
/// The unique sink node — a node none of whose outputs are consumed.
/// Errors if there is not exactly one (0 ⇒ rootless/cyclic, >1 ⇒ multi-rooted).
pub fn derive_root(&self) -> anyhow::Result<NodeIndex> {
    let sinks: Vec<NodeIndex> = (0..)  // iterate node vertices via node_indices filter
        ./* see impl: scan inner.node_indices(), keep BiVertex::Node whose
             produced_outputs all have no Consumes consumer */;
    match sinks.as_slice() {
        [one] => Ok(*one),
        [] => Err(anyhow!("BiGraph has no sink (rootless or cyclic)")),
        many => Err(anyhow!("BiGraph has {} sinks; expected exactly one", many.len())),
    }
}
```
  (Helper: a node’s output is consumed iff `edges_directed(out, Outgoing)` has a `Consumes`.)
  Remove the `root: Option<NodeIndex>` field and `set_root`/`root` accessors.

- [ ] **Step 4: Make `Pattern::root` / `Template::root` derive.** Both return
  `self.graph.derive_root().ok()` (keep the `Option` signature the matcher/instantiate expect),
  so a malformed graph yields `None` (already handled as a non-match / rootless error).

- [ ] **Step 5: Validate at seal.** `MatcherBuilder::finish(root)` /
  `finish_node(root)` and `TemplateBuilder` seal: compute `derived = graph.derive_root()?`,
  assert `producer_of(root) == derived` (else `Err`/panic "sealed root is not the graph sink"),
  then drop the old `set_root`. Keep `assert_dag` (cycle detection) using `derived`.

- [ ] **Step 6: Update raw-construction tests** that called `set_root` (`bigraph/mod.rs` tests,
  `pattern/mod.rs` tests): delete the `set_root(...)` lines; the cycle test asserts
  `derive_root().is_err()` directly instead of `reachable_topo(g, g.root().unwrap())`.

- [ ] **Step 7: Run** `cargo test -p strider-pattern` then `-p strider-analyze` then pytest.
  If any real pattern trips ">1 sink" or "root is not sink", STOP and report the case.

- [ ] **Step 8: Commit & push**
```bash
git add -A && git commit -m "refactor(strider-pattern): derive pattern root as the unique graph sink, drop stored root"
git push origin refactor/strider-pattern-tidy
```

---

### Task E: add uv at the repo root

**Why:** the uv project currently lives in `crates/strider-py/`; the dev workflow should run
from the workspace root.

**Files:**
- Create: `pyproject.toml` (root) — maturin backend pointing at `crates/strider-py`
- Move: `crates/strider-py/{uv.lock,.python-version}` → repo root (or regenerate)
- Verify: `uv run --version`, `uv sync --group dev`, `uv run maturin develop`, `uv run pytest`

- [ ] **Step 1:** Read `crates/strider-py/pyproject.toml` to capture name, deps, dev-group,
  abi3 settings, and the maturin `[tool.maturin]` config.

- [ ] **Step 2:** Author root `pyproject.toml` mirroring it, with
  `[tool.maturin] manifest-path = "crates/strider-py/Cargo.toml"` (and `module-name`/`python-source`
  as needed) so the extension builds from the root. Decide (and document inline) whether the
  crate-local pyproject is removed or kept as a thin shim — default: move it to the root and
  delete the crate copy.

- [ ] **Step 3:** `uv sync --group dev` at the root; expect a fresh root `uv.lock`.

- [ ] **Step 4:** `uv run maturin develop` then `uv run pytest -q` from the root — expect the
  full strider-py suite green.

- [ ] **Step 5: Commit & push**
```bash
git add -A && git commit -m "build: host the uv project at the workspace root"
git push origin refactor/strider-pattern-tidy
```

---

## Final verification (before merge)

- [ ] `cargo test -p strider-pattern -p strider-analyze` green; `cargo clippy -p strider-pattern` clean.
- [ ] `(uv run pytest -q)` from root green.
- [ ] `requesting-code-review` skill on the full diff.
- [ ] Summarize for the user and **prompt before merging** `refactor/strider-pattern-tidy` → `develop`
  (another agent is active on `develop`).
