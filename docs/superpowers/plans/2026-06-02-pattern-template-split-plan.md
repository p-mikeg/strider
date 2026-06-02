# Pattern / Template split — implementation plan

> Executes `docs/superpowers/specs/2026-06-02-pattern-template-split-design.md`. Runs on `develop`
> after Phase 8 (strider-pattern core + strider-analyze green). Then Phase 9 (Python) lands against
> the clean two-type API, then the dead-code sweep + merge.

**Goal:** split the one `Pattern` type (used for both matching and templating) into two distinct types
over a generic `BiGraph<N, O>`, with purpose-built builders and no repetitive hand-written code.

**Green-keeping strategy:** each phase is a coherent compiling+green milestone. The match side stays
green throughout; strider-analyze stays green because it uses the compile-time
`rewrite_rule<L: MatchPat, T: TemplatePat>` (the typed-struct call sites don't change — only
`into_template()`'s return type does, behind the trait). Only the strider-pattern tests + the FFI
`rewrite_rule_runtime` signature change. Push after every commit. NO plan/Phase identifiers in code or
commit messages. Single-threaded; no `Arc`/`Rc`/`Send`/`Sync`.

---

## S1 — Generic `BiGraph<N, O>`; re-express `Pattern` over it

**Outcome:** a generic bipartite graph owns the shared mechanics; `Pattern` is a thin instantiation.
Pure refactor — the whole strider-pattern + strider-analyze suites stay green.

- Create `crates/strider-pattern/src/bigraph/mod.rs`:
  ```
  pub enum BiVertex<N, O> { Node(N), Output(O) }
  #[derive(Clone, Copy, Debug)] pub enum BiEdge { Produces, Consumes { slot: usize } }
  pub struct BiGraph<N, O> { inner: StableDiGraph<BiVertex<N,O>, BiEdge>, root: Option<NodeIndex> }
  impl<N, O> BiGraph<N, O> {
      new/default, add_node(N)->NodeIndex, add_output(producer, O)->NodeIndex,
      consume(consumer, slot, output), set_root/root,
      node_count/output_count (test helpers),
      // generic accessors the match + template sides both need:
      producer_of(output)->NodeIndex, node_weight/_mut, output_weight/_mut,
      inputs_of(node)-> iterator of (slot, output_vertex), produced_outputs_of(node),
  }
  // generic reachable-topo (moved from pattern/topo.rs, parameterised over BiVertex<N,O>):
  pub fn reachable_topo<N,O>(g, root) -> Result<Vec<NodeIndex>>; pub fn assert_dag<N,O>(g, root);
  ```
- Re-express `Pattern` as a thin wrapper carrying `BiGraph<PatNode, PatOutput>` + `cast_mask` (keep the
  current `PatNode`/`PatOutput` payloads unchanged for now, including the `build`/`build_ty` fields —
  those move out in S2). Delete the bespoke `pattern/{topo}.rs` graph code superseded by the generic.
- `MatcherBuilder` (still over `BiGraph<PatNode, PatOutput>`) and the matcher `walk` use the generic
  accessors. `instantiate` still walks the `Pattern` (unchanged this phase).
- **Gate:** `cargo test -p strider-pattern` green; `cargo test -p strider-analyze` green; clippy
  `--all-targets` clean on both. Add a `BiGraph` generic unit test (build a 2-node bipartite shape over
  a dummy payload, assert topo order + accessors).
- Commit: `refactor(strider-pattern): extract generic BiGraph; re-express Pattern over it`.

## S2 — Distinct `Template` type + `TemplateBuilder` + `instantiate(Template)`

**Outcome:** matching and templating are separate types; no dead cross-fields; `rewrite_rule_runtime`
is type-honest.

- New `template/graph.rs`: `TmplNode { kind: TemplateKind, ty: TemplateTy, capture: Option<Capture> }`,
  `TmplOutput { slot, kind: OutputKindSpec }`; `pub struct Template(BiGraph<TmplNode, TmplOutput>)`.
- New `TemplateBuilder` over `BiGraph<TmplNode, TmplOutput>` — exposes ONLY construction verbs
  (`leaf`/`unary`/`binary`/`node`/`input`/`value_output`/`memory_output`/`phi_token_output` +
  capture + the dynamic-kind `TemplateKind::Fn` setter). NO match verbs (`set_node_limit`/
  `set_output_width`/`set_force_ordered`/predicate kindspecs). Multi-output interior nodes supported via
  `TmplOutput` slots.
- `TemplatePat::compile(self, &mut TemplateBuilder) -> TmplOutRef`; `into_template(self) -> Template`.
- Rewrite `instantiate(&Template, &mut Function, &Bindings, lhs_root, root_ty) -> NodeOutputId`: walk
  `reachable_topo`, resolve capture-only nodes via `Bindings::get_output`, materialise each `TmplNode`
  via `function.create_node(kind, inputs_by_slot, output_kinds_from(TmplOutputs))`. Root yields the
  single value output (unchanged contract). Reads `TmplOutput`s for the full output signature.
- **Drop the build machinery from the match side:** remove `build`/`build_ty` from `PatNode`; remove the
  `template: bool` flag + `stamp_build`/`build_spec_for` from the (now match-only) builder core; the two
  builders no longer share a `template` flag (they may still share a private generic wiring helper over
  `BiGraph`, but with no match/template branch).
- `rewrite_rule_runtime(lhs: Pattern, rhs: Template) -> Result<...>`; `assert_buildable` operates on a
  `Template` (every `TmplNode` has a `kind`/`Fn` or a capture — by construction a `Template` is buildable,
  so `assert_buildable` reduces to "every capture-only node's capture is LHS-bound", folded into
  `check_capture_coverage`). The compile-time `rewrite_rule<L: MatchPat, T: TemplatePat>` calls
  `rhs.into_template()` internally — **strider-analyze call sites are unchanged**.
- Update strider-pattern tests: `rewrite_build.rs` RHS finalisation → `Template`; the `*_const_with!`
  macros now build a `Template` leaf (`ConstWith: TemplatePat`).
- **Gate:** `cargo test -p strider-pattern` green; `cargo test -p strider-analyze` green (the compile-time
  rewrite path is unchanged); clippy clean. Add a `Template` test: a multi-output interior node (build a
  node consuming a prior node's memory output; root yields a value) + a compile-fail confirming a
  `Pattern` can't be passed where a `Template` is required (e.g. `rewrite_rule_runtime(p, p)` fails to
  compile / a `Pattern`→`Template` mismatch).
- Commit(s): `refactor(strider-pattern): distinct Template type + TemplateBuilder + typed instantiate`,
  `refactor(strider-pattern): drop build fields from match side; rewrite_rule_runtime takes Template`.

## S3 — DRY pass: macro-generate the repetitive surface (owner mandate)

**Outcome:** adding an op or a builder verb is one macro line; no hand-duplicated families.

- Audit for repetition: the ~86 op factory/struct families and their `MatchPat`/`TemplatePat` impls
  (`typed/value_ops.rs`), and the two builders' verb surfaces. Where the current per-family `macro_rules!`
  (`int_binary_op!`/`cast_op!`/…) don't yet cover a family (e.g. the `*Any` structs, the value-builder
  `MatchPat` impls), extend the macros so each family is one invocation. Generate the `MatchPat` +
  `TemplatePat` impls together from one macro where the lowering is shared.
- Confirm the two builders' shared wiring is one generic `BiGraph` helper (no parallel copies).
- **Gate:** `cargo test -p strider-pattern` + `-p strider-analyze` green; clippy clean; net LOC down or
  flat with the duplication gone (report the delta). No behaviour change.
- Commit: `refactor(strider-pattern): macro-generate op families and builder verbs`.

---

## After the split

- **Phase 9 — strider-py + macro**, once, against the clean API: Python LHS builds a `Pattern`, RHS
  builds a `Template`; the proc-macro emits the two finalise paths; `rewrite_rule_runtime(Pattern,
  Template)`; cast mask on the `Pattern`; string captures stay in strider-py's `intern_str`. Gate:
  `cargo build -p strider-py` + `uv run maturin develop` + `uv run pytest` green.
- **Dead-code sweep** (the deferred Phase 6): remove the `#![allow(dead_code)]`s with full consumer
  visibility; finalise the public surface. Gate: clippy `--all-targets` clean across the workspace.
- **Merge** (Phase 10): workspace build/clippy/per-crate test sweep + orchestrator demo + review +
  `develop`→`master`; update CLAUDE.md.
