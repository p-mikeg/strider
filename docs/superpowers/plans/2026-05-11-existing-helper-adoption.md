# Round 15 — Existing-helper adoption plan

> Audit date: 2026-05-11.  Branch: `simplification/ai1` (HEAD `5b0f7a8`).
> Predecessor: round 14 generalization plan + W1/W2 commits.

## Question being asked

Where in the workspace does code hand-roll something that a helper already in the workspace would do better? (Not "add a new helper" — strictly "use the existing one.")

## TL;DR

Six parallel read-only audits surveyed the entire workspace:
- `ir` + `pcode-lift` (graph primitive helpers)
- `opt` (passes + `rewrite_rule` / `WorkSet` / `after_replace` helpers)
- `pattern` + every consumer (Matcher / Pat / rewrite_rule)
- `cfg` + `strider` + `strider-py` (integration glue)
- `target` + `reader` + `entity-utils` + `graphwalk` + `dot` (support crates)
- tests / examples / benches / docs / Python (long tail)

**Total concrete findings: 6 items, ~ −19 LOC.**  Five items are mechanical refactors against helpers added in rounds 11–14; one is a stale README hunk.  No semantic-level work, no architectural restructuring.

Reasonable interpretation: the codebase has been refactored hard enough that the "use existing helper" surface is genuinely small.  The honest plan is short.  After landing these, the next-most-promising direction is *not* more existing-helper adoption — it's design-level changes that the round-14 plan covered (mostly deferred there for good reason).

---

## Findings

### F-1 — `build_single_output_pure` should use typed slot extraction

- **Site:** `crates/ir/src/builder/mod.rs:465`
- **Current:**
  ```rust
  pub(super) fn build_single_output_pure(
      &mut self,
      kind: NodeKind,
      inputs: impl IntoIterator<Item = NodeOutputId>,
      output_type: NodeOutputType,
  ) -> NodeOutputId {
      let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
      self.graph().node_outputs(node)[0]
  }
  ```
- **Helper to use:** `Graph::node_outputs_exact::<1>(node) -> Result<[NodeOutputId; 1]>`
- **Why correct:** the node is created with exactly one output kind (single-element output_kinds slice).  `node_outputs_exact::<1>` returns the typed array, surfacing any future shape divergence as a typed error rather than a silent index-out-of-bounds.
- **Migration:** caller must change return type to `Result<NodeOutputId>` and add `?`. Inspect every call site of `build_single_output_pure` (a few dozen — `crates/ir/src/builder/nodes.rs` is the main consumer) to thread the `?`.
- **LOC delta:** ~0 (1 line shorter at the helper; `?` added at each caller).
- **Risk:** trivial — the result is already by-construction correct, so this only surfaces invariant violations as typed errors instead of panics.
- **Foot-gun mitigation:** low (replaces an index that can't actually be out-of-bounds with a typed Result).

### F-2 — `validate::mod.rs` Layer A reachability filter

- **Site:** `crates/ir/src/validate/mod.rs:96-99`
- **Current:**
  ```rust
  for node in graph.nodes.keys() {
      if !reachable.contains(node) {
          continue;
      }
      check_layer_a(graph, node, &mut errs);
  }
  ```
- **Helper to use:** `validate::layer_c::reachable_nodes(graph, reachable) -> impl Iterator<Item = (NodeId, &NodeKind)>` (added in simplification W1).
- **Why correct:** same reachability-scoping semantics; Layer A only needs the `NodeId` so the kind half of the tuple is discarded.  Needs `reachable_nodes` to be re-exported as `validate::reachable_nodes` (currently `pub(super)` inside layer_c — promote to `pub(super) use` at `validate::mod` or move the helper up one module).
- **LOC delta:** −2
- **Risk:** low (one-file refactor; visibility tightening).

### F-3 — `validate::layer_b.rs` reachability filter

- **Site:** `crates/ir/src/validate/layer_b.rs:63-65`
- **Current:** identical shape to F-2.
- **Helper to use:** same `reachable_nodes` helper.
- **LOC delta:** −2
- **Risk:** low.

### F-4 — `dot/{render,label}.rs` should use BFG accessors

- **Sites:**
  - `crates/ir/src/dot/render.rs:146` — `match self.call_clobbered.get(i) { … }`
  - `crates/ir/src/dot/label.rs:341` — `let Some(vn) = self.ret_val_regs.get(i) else { … }`
- **Helpers to use:** `BuiltFunctionGraph::call_clobbered_regs()` / direct slice already used elsewhere — but the dumper holds its CC fields as `&[Vn]` references via constructor injection (see `BuiltFunctionGraph::dot_dumper`).  The direct-slice path is correct **at the call site**; the dumper doesn't have access to the BFG.
- **Verdict on F-4:** **withdraw**.  After re-reading the dumper code, the dumper takes `&[Vn]` slices in its constructor (not the BFG), so the field-vs-accessor question doesn't apply here.  Original audit finding was based on the BFG-method symbol, not the dumper's slice arg.  **No change recommended.**

### F-5 — `apply_tail_call`: three `create_node + extend_asm_fingerprint` triplets

- **Site:** `crates/opt/src/indirect_branch_resolve/inplace.rs:159–194` (three creations: `IntConst` target literal at 159, `Call` node at 180–181, `Return` node at 193–194)
- **Current:**
  ```rust
  let int_const = graph.create_node(NodeKind::IntConst(masked_target), [], [...]);
  graph.extend_asm_fingerprint(int_const, &placeholder_fingerprint);
  // ... similar pattern for Call and Return
  ```
- **Helper to use:** `Graph::create_node_attributed(kind, inputs, output_kinds, contributors: &[NodeId])` — atomic create + fingerprint absorb.  But here the contributor is a *fingerprint slice* (`&[u64]`), not a `NodeId`.  Need to verify: does `placeholder_fingerprint` come from a node we can pass as the contributor?  Probably yes (the placeholder was the `IndirectBranch` node being rewritten).
- **Why correct (pending verification):** if the placeholder is still a live `NodeId` at this point (it is — `apply_tail_call` rewrites it in-place), we can pass `&[placeholder]` and `create_node_attributed` will union the placeholder's own fingerprint.  Equivalent.
- **LOC delta:** ~−6 (three sites × 2 lines saved).
- **Risk:** trivial — semantically identical; just folds the absorption into the constructor.
- **Verification needed before coding:** confirm that `placeholder_fingerprint` is exactly `graph.asm_fingerprint(placeholder)`.  If it's a hand-curated slice, this finding is invalid and we should leave it alone.

### F-6 — `cfg/README.md` references deleted `Builder::new`

- **Site:** `crates/cfg/README.md:93-97`
- **Current:** "Gotchas" section warns against `Builder::new`'s silent-default behaviour — but `Builder::new` was deleted in round 12.  Lines 17–20 already document the deletion + replacement.
- **Helper to use:** N/A — purely doc cleanup.  Delete the redundant block.
- **LOC delta:** −5
- **Risk:** trivial.

---

## Categories verified clean

These were investigated and report zero findings:

1. **Hand-rolled `match_at + manual_build + replace_all_uses`** outside the W2-retired `flag_cmp_canonicalize`.  Every rule-driven pass uses `rewrite_rule(lhs, rhs)`.  Cases that genuinely can't use it (`IfCondInversion`'s consumer swap, `DeadBranchElimination`'s control-edge surgery, `apply_link_register`/`apply_tail_call`'s placeholder rewriting) document why in headers.

2. **Hand-rolled register-aliasing** outside `pcode-lift::vn_io.rs`.  No reimplementations of `find_largest_fitting_register` or shift math anywhere.

3. **Hand-rolled CallOther name dispatch.**  All sites route through `target::call_other_abi::classify(preset, name)`.

4. **Hand-rolled lock guards on `PyGraph::inner`.**  28+ call-sites consistently use `read_inner()` / `try_write_inner()`.  No `inner.read()` / `inner.write()` leaks since round 13 W3.

5. **Hand-rolled error PyErr construction.**  All errors flow through `into_strider_err` / `into_lift_err` / `into_reader_err` / `into_pattern_err` / `into_rewrite_err`.  No leftover `PyValueError::new_err(...)` in live paths (the only raw uses are intentional `function_max_size=0` rejection at the Python boundary).

6. **Hand-rolled DFS/BFS over region graphs.**  `cfg::Cfg` implements `graphwalk::GraphRef` (W1), and generic traversal is available; no current consumer needs it yet.  No bespoke walks found.

7. **Stale `Builder::new` / `Builder::with_endianness` calls** in tests / examples / benches.  Zero leaks.

8. **Manual `Worklist<NodeId>` reimplementation** outside `entity-utils::Worklist` / `opt::WorkSet`.  No `VecDeque<NodeId>` + `HashSet<NodeId>` shapes found.

9. **Manual ELF relocation walks** outside `reader::apply_elf_relocations[_autoload]`.  Reader owns this completely.

10. **Test fixtures hand-rolling synthetic graphs.**  All synthetic graphs go through `ir::FunctionBuilder::empty()` or test-only ctors documented in module headers.

11. **Benchmarks bypassing `strider::run`.**  Each bench documents its stage-isolation rationale (`crates/strider/benches/scaling.rs:89-91` is the model).

12. **Examples not using canonical entry.**  `crates/strider/examples/strider.rs` and the 4 pattern/cfg examples all use `Builder::for_arch` + `strider::run` or `FunctionBuilder::empty()` (for synthetic graphs) appropriately.

13. **Python tests hand-rolling pipelines.**  All Python tests use `strider.run(...)` for end-to-end; the few that don't (e.g. pattern unit tests) operate on synthetic graphs intentionally.

14. **Stale deleted-API references in doc comments** (`CallOtherElide`, `NO_OP_USER_OPS`, `analyze_binary`, `graphmock` crate).  All zero (round 13 W2 swept these).

---

## Phasing recommendation

Phase 1 (mechanical, ~30 minutes):
- F-1: `build_single_output_pure` → `node_outputs_exact::<1>`
- F-2 + F-3: validate Layer A + B reachability filters → `reachable_nodes` helper
- F-6: cfg/README.md stale `Builder::new` block

Phase 2 (1 verification + ~15 minutes):
- F-5: verify `placeholder_fingerprint` is reachable from a `NodeId`, then migrate `apply_tail_call`'s three create-and-extend triplets to `create_node_attributed`.

Withdrawn:
- F-4 (dot dumper field access) — re-reading the dumper API showed the audit finding was incorrect.

**Total LOC delta after both phases: ≈ −15.**

---

## Honest assessment

This is genuinely a thin tail.  After rounds 11–14 plus the W1+W2 changes on `simplification/ai1`, the workspace's "use the helper that exists" debt is essentially paid off.  The six findings here are real but small; landing them in one wave is the natural ceiling.

The next round of *meaningful* simplification would have to come from one of:

1. **Design-level consolidation** of the per-iteration handle types (`RegionLiftHandles`, `RegionIndex`, `unresolved_branches`) — deferred in round 14 because the in-flight incremental-indirect-resolve plan is the right driver.
2. **D-1 / D-2 from round 14** (NodeKind ↔ PatKind proc-macro mirror; typed error hierarchy) — major-scope items deferred for good reason.
3. **Test-coverage growth** for the asm-fingerprint contract under `check_asm_fingerprints: true`, since that invariant is the most expensive to verify by hand.

None of these are "use existing helper" work.  They're new-design work, and the round-14 plan documents the tradeoffs.

---

## Artifacts

- `/tmp/round15-audit-ir.md` (ir + pcode-lift; 4 concrete findings)
- `/tmp/round15-audit-opt.md` (opt crate; 1 concrete finding)
- `/tmp/round15-audit-pattern.md` (pattern + consumers; 0 findings, clean bill)
- `/tmp/round15-audit-integration.md` (cfg + strider + strider-py; 0 findings, clean bill)
- `/tmp/round15-audit-support.md` (target/reader/utility; 0 findings, clean bill)
- `/tmp/round15-audit-glue.md` (tests / examples / benches / docs; 1 concrete finding — cfg README)
