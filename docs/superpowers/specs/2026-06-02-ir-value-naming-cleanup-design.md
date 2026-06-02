# IR value-naming + cleanup — design

Date: 2026-06-02
Branch: `refactor/ir-value-naming` (forked from `develop` @ `ef162133`)

## Motivation

The IR's `NodeOutputId` / `NodeInputId` vocabulary describes graph edges in
terms of a node's *slots* ("output", "input"). The more correct domain
terminology is **value** (what a node produces) and **use** (a reference to a
value consumed by a node). This is a large, behavior-preserving rename plus
three structural cleanups that make the IR API more correct and more explicit.

Four independent workstreams, one branch, merged to `develop` after a final
prompt, then the branch is deleted.

## Scope (the four workstreams)

### WS1 — Rename to value/use vocabulary (behavior-preserving)

Curated identifier→identifier rename. **Rule:** node-level slot accessors keep
`inputs`/`outputs`; everything keyed by a value or a use becomes `value`/`uses`.
This is NOT a blanket `output→value` substitution — `node_outputs` is *kept*.

| current | new | category |
|---|---|---|
| `NodeOutputId` | `ValueId` | type |
| `NodeInputId` | `UseId` | type |
| `NodeOutputKind` | `ValueKind` | type |
| `NodeOutputType` | `ValueType` | type |
| `NodeOutputKind::OutputType(t)` | `ValueKind::Typed(ValueType)` | variant |
| `NodeOutputIdList` (and similar internal aliases) | `ValueIdList` | type alias |
| `node_signature::ExpectedOutputKind` | `ExpectedValueKind` | type (consistency with `ValueKind`) |
| `node_outputs`, `node_inputs` (+ `_exact`, `_id_at`) | **kept** | node-level slot accessors |
| `add_node_input` / `remove_node_input` / `detach_node_inputs` / `update_input` | **kept** | node-level edge ops |
| `replace_all_uses` | **kept** | already use-keyed |
| `output_kind(v)` | `value_kind(v)` | value-keyed |
| `kind_of_output(v)` | `kind_of_value(v)` | value-keyed |
| `node_for_output(v)` | `producer(v)` | value-keyed |
| `output_uses(v)` | `value_uses(v)` | value/use-keyed |
| `PatOutput` / `PatOutRef` | `PatValue` / `PatValueRef` | pattern value-vertex |
| pattern value-vertex helpers (`*_out`, e.g. `mem_out`) | value vocab (`mem_value`, etc.) | pattern value-vertex |

The long tail of residual identifiers (macro-generated `Py*` names, `.pyi`
stubs, doc-comment references, local variable names like `out`/`output`) is
resolved compiler-first during execution **per this rule**; the table above
fixes every case that is load-bearing or could be interpreted two ways.

Variable/parameter names follow the same rule: a binding holding a `ValueId`
is named `value`/`val`/`v` (not `output`/`out`); a binding holding a `UseId`
is named `use_id`/`u`.

### WS2 — Remove all `Deref` / `DerefMut`

Four impls are removed so call sites are explicit about which layer
(`Graph` / `Function` / rewrite ctx) they operate on:

- `impl Deref/DerefMut for Function` (Target = `Graph`)
- `impl Deref for RewriteCtx` (Target = `Function`)
- `impl Deref for RewriteCtxView` (Target = `Graph`)

Replacement: explicit accessors already largely exist (`graph()`,
`graph_mut()`, `function_ref()`, …). Every implicit-deref call site
(`function.node_kind(..)` relying on `Function: Deref<Graph>`,
`ctx.node_inputs(..)` relying on `RewriteCtx: Deref<Function>`, etc.) becomes
an explicit `.graph()` / `.function_ref()` / `.graph_mut()` hop, OR the needed
method is forwarded as an inherent method on the outer type where that reads
better. Decision rule: forward the small, frequently-used read accessors as
inherent methods on `Function`/`RewriteCtx`; require an explicit `.graph()` /
`.function_ref()` hop for the long tail. The exact forwarding set is pinned in
the plan after a call-site census. Largest manual effort; subagented per crate.

### WS3 — `var_table` (VarId ↔ Vn) → `ValueId → Vn` map

`CcMetadata::var_table` is currently a bidirectional `VarId ↔ Vn` table backed
by a Vec indexed by `VarId`. Replace the stored representation with a
`ValueId → Vn` map keyed by the producing value, eliminating the Vec. `VarId`
threads through SSA construction in `builder/` and `region.rs`, so the exact
mechanics (whether `VarId` survives as a build-time-only index or is removed
entirely) are pinned by a focused read-only spike at the start of this WS in
the plan. Isolated to `strider-ir`.

### WS4 — Conservative panic-on-invariants

Convert only the clearest **non-user-facing** "can't happen" error paths to
`panic!`/`expect`/`debug_assert!`/`unreachable!`: validator-guaranteed
structural invariants (e.g. arity the validator already enforces, signature-
guaranteed slot kinds). Leave `Result` wherever there is any doubt, or where
the path can be reached by user/binary input. This aligns with the existing
"panic on validator-guaranteed structural invariants" policy in the codebase.

## Approach

Mechanical-first, compiler-driven, single branch.

- rust-analyzer is unavailable here (crashes; and the LSP tool exposes no
  rename op), so renames are curated word-boundary token sweeps from an
  explicit mapping, followed by `cargo check` per crate in dependency order
  (`strider-target → strider-ir → strider-reader → strider-lift →
  strider-analyze / strider-pattern → strider-py`) to mop up residuals
  (macro-generated names, `.pyi` stubs, doc links, string literals).
- Structural workstreams (WS2–WS4) land after WS1 so they operate on the new
  vocabulary.
- Subagents parallelize independent per-crate fixups (WS2 call-site edits,
  WS4 per-crate panic sweep). Each subagent is given an explicit, bounded task
  and the dependency-order constraint.

## Sequencing

1. WS1 renames (types → methods → pattern vocab), one logical commit group.
2. WS2 deref removal.
3. WS3 var_table → ValueId→Vn.
4. WS4 conservative panic pass.

Each workstream: full `cargo test --workspace` + `cargo clippy --workspace`
green before moving on; commit(s) per workstream, pushed to
`origin/refactor/ir-value-naming` after each.

## Verification

- `cargo test --workspace` (baseline 3037 Rust tests) + clippy clean after
  **every** workstream.
- strider-py pytest (baseline 841) after WS1 (it touches `Py*`/`.pyi`), after
  WS2, and at the end.
- WS1 is behavior-preserving → the existing suite is the safety net; no new
  tests. WS2/WS3/WS4 get TDD where behavior could shift (new accessors, the
  var-table representation change). WS4 conversions must keep the suite green.

## Risks / mitigations

- **Substring over-replacement** (`node_outputs` wrongly hit by an
  `output→value` rule): mitigated by exact-identifier, word-boundary sweeps
  from an explicit mapping — never a blanket substring replace.
- **Deref removal scope creep**: a call-site census fixes the forwarding set
  before editing; subagents work from that fixed set.
- **WS3 unknowns**: gated behind a read-only spike before any edit.
- **Over-converting `Result`→panic** (WS4): conservative policy; when in doubt,
  leave `Result`.

## Out of scope

No behavioral/optimizer changes. No unrelated refactoring. The
`NodeOutputType` reshape to `Bool/Int(IntWidth)/Float(FloatKind)` (separately
deferred) is NOT part of this — only the rename of the existing flat enum.
