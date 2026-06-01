# Optimizer rework — design spec

Date: 2026-06-01
Status: draft (awaiting user review)

## Goal

Rework how `strider-analyze` optimization passes traverse, mutate, and reason
about the IR so that:

- every pass iterates in one well-defined order (RPO),
- the act of "changing the function" is funnelled through a single Rewrite
  abstraction (so asm-fingerprint propagation has exactly one implementation),
- CFG edge detachment is its own concern, separate from dead-branch / phi
  reasoning,
- the two memory-chain analyses (function-args, load-forward) share one
  Memory-SSA walker parameterised by an aliasing predicate,
- passes are *constructed* with the data they need instead of reaching for
  `thread_local!` / lazy statics,
- ROM access matches the std `Read` shape and the optimizer is endianness-aware,
- KnownBits drops its `merge` step and rewrites by iterating the analysed map.

This is a large change. It is decomposed into six sequenced workstreams below;
each is independently testable and lands behind green gates before the next.

## Scope reflected from the request (12 items)

1. All passes iterate in RPO order; expose an `rpo` walk on the IR and route
   iteration through it.
2. Split "detach CFG edges" into its own pass, separate from dead-branch / phi
   elimination. The detach pass performs *only* CFG edge removal, using the CFG
   traversal already in `walk.rs`.
3. Rewrite is the only thing that may change the function — enforce it; no
   scattered manual `extend_asm_fingerprint` single-source-of-truth.
4. ROM Rust API becomes `fn read(&self, addr: u64, buf: &mut [u8]) -> Result<()>`
   (fill-all-or-error; matches the std `Read` *shape*, see decisions).
5. Optimizations are context-aware — e.g. little/big-endian for ROM parsing.
6. Add a `MemorySSAWalker` trait (template below) with a pluggable `may_alias`,
   and refactor function-args + load-forward onto it.
7. `decompose_sp` becomes a flat RPO loop over Add / InitialVar / And cases.
8. The user may choose whether `Call` / `CallOther` clobbers a function-arg
   load. **Default: assume it does NOT clobber** (aggressive arg detection),
   with an option to flip to conservative clobbering.
9. Rename `AssumeStackConstDisjoint` → `AssumeStackGlobalDisjoint`; make it the
   default alias mode.
10. Construct optimizers with their data (kill `thread_local!` / lazy-lock
    pattern caches and CC layouts).
11. KnownBits no longer needs `merge` (we always merge into "unknown").
12. After analysing KnownBits, rewrite by iterating the known-bits map; write a
    detailed sub-plan for this (below).

## Key decisions (resolved)

### D1 — `read` return type (item 4)

`fn read(&self, addr: u64, buf: &mut [u8]) -> Result<()>`. Fill-all-or-error:
every optimizer call site (`LoadReadOnly`, jump-table resolver) needs exactly N
bytes or nothing, so a short-read `usize` would force loops at every site with
no benefit. The signature matches the std `Read` *shape* (explicit buffer) while
keeping the all-or-nothing contract the optimizer actually wants. `Result` is
`anyhow::Result` to stay consistent with the crate.

The implementor no longer byte-swaps. It copies raw mapped bytes into `buf` (or
errors if any byte in the range is unmapped). Endianness decoding moves to the
caller (item 5).

### D2 — endianness moves into `OptCtx` (items 4, 5)

`OptCtx` gains an `endianness: strider_target::Endianness` field (and keeps the
borrowed `rom`). `LoadReadOnly` and the jump-table resolver read raw bytes via
the new `read` and decode with `Endianness::read_u{8,16,32,64}` from the
context. This is what "optimizations should be context-aware" means in
practice: arch-derived facts (endianness today; room for more later) live on the
per-run context rather than being implicit in the reader.

The orchestrator already knows the `SleighArch` (hence `Endianness`); it
populates `OptCtx` when it builds the run context.

### D3 — RPO semantics (items 1, 7); memory walk is separate (item 6)

Add to `strider-ir`:

```rust
/// Reverse-post-order over the data-dependency cone reachable from `seed`,
/// yielding every producer before the consumer that depends on it
/// (defs-before-uses). Built on graphwalk's PostOrder over the
/// "successors = data inputs" relation.
pub fn rpo(&self, seed: NodeOutputId) -> impl Iterator<Item = NodeId> + '_;
```

and a global form `rpo_from_entry()` (≡ `rpo` seeded at the entry cone plus
control reachability) used by passes that today call `function.walk()`.

`rpo` drives items 1 and 7 and the analysis passes. The `MemorySSAWalker`
(item 6) is **not** an `rpo` consumer: it walks backward along **memory-token
edges** with its own cursor (confirmed). The two traversals are deliberately
distinct:

- `rpo` / `decompose_sp` (item 7) is **leaves-first** over data inputs: classify
  `InitialVar(sp)` and the `IntConst` offset *before* the `Add`/`And` that
  consume them, so each case is a local map lookup. That is defs-before-uses.
- The `MemorySSAWalker` (item 6) starts at the load and follows the memory input
  chain backward, stopping at the *nearest* clobbering store and recursing
  per-branch at `MemPhi`. It does not consume the `rpo` iterator; the request's
  `graph.rpo(load)` pseudocode is shorthand for "walk the memory defs," and the
  real implementation is the memory-edge cursor described in D6.

Implementation note: the exact PostOrder-vs-reverse direction will be pinned by
a TDD test (`Add(InitialVar, IntConst)` must emit the two operands before the
`Add`) before any pass is migrated, because the existing `walk_graph` comment
("producer before consumer") and graphwalk's PreOrder/PostOrder need to be
reconciled against a concrete fixture rather than by prose.

### D4 — Rewrite is the only mutator: three tiers (items 2, 3)

- **Tier 1 — graph structure → Rewrite only.** Node creation, `replace_all_uses`,
  `update_input`, `add_node_input`, `remove_node_input`, and the new structural
  ops below all go through the Rewrite layer, which is the *single*
  implementation of asm-fingerprint propagation (superset-only). No optimization
  pass calls `extend_asm_fingerprint_from` / `create_node_attributed` directly
  anymore.
- **Tier 2 — non-progressive orphan detach.** Detaching an already-dead node's
  inputs is graph hygiene; it never escalates to `Changed`. This is the only
  structural edit allowed outside a Rewrite, and it has no fingerprint work
  (orphans are exempt from the non-empty check).
- **Tier 3 — side tables exempt.** `stack_offsets`, `arg_index_to_nodes`,
  `call_other_names`, etc. are derived overlays, not graph structure. Analysis
  passes write them directly; they don't touch reachability or fingerprints.

To bring today's structural edits under Tier 1, the Rewrite layer grows beyond
single-value-output replacement. Proposed op set (exact home — `strider-pattern`
`rewrite` module — TBD in plan):

```rust
pub enum RewriteOp {
    /// Redirect all consumers of `root_out` to `new_out`; absorb root's
    /// fingerprint into the new subtree (today's rewrite_rule behaviour).
    ReplaceValue { root_out: NodeOutputId, new_out: NodeOutputId },
    /// Remove a Region's control predecessor `pred_index` and the matching
    /// input slot on every Phi/MemPhi owned by that Region. Fingerprint-exempt.
    RemoveRegionPred { region: NodeId, pred_index: u32 },
    /// Swap the consumer use-lists of two outputs of one node (If true/false).
    /// No new value; fingerprints unchanged.
    SwapOutputConsumers { a: NodeOutputId, b: NodeOutputId },
}
```

Pass-by-pass landing (from the spike):

| Pass | Today | After |
|------|-------|-------|
| LoadReadOnly | manual extend + replace | `ReplaceValue` rewrite (zero behaviour change) |
| RedundantPhis (value arms) | manual extend + replace | `ReplaceValue` rewrite |
| RedundantPhis (orphan detach) | detach | Tier 2 |
| LoadForward (forward) | manual extend + replace | `ReplaceValue` rewrite |
| LoadForward (orphan detach) | detach | Tier 2 |
| DeadBranchElimination (redirect) | manual extend + replace | `ReplaceValue` rewrite |
| DeadBranchElimination (region strip) | remove_node_input ×N | `RemoveRegionPred` — **moves to the new detach pass (item 2)** |
| IfCondInversion (cond redirect) | update_input | match `If(Xor(x,1))` → `ReplaceValue` of the cond edge |
| IfCondInversion (branch swap) | update_input ×2 | `SwapOutputConsumers` |
| CallStackArgCollect (append args) | add_node_input | `RewriteOp` append variant *or* documented Tier-1 helper |
| StackOffsetDetect / FunctionArgDetect | side-table writes | Tier 3 (unchanged) |

Enforcement: after migration, grep-level/CI check that `opt/` contains no direct
`extend_asm_fingerprint_from` / `create_node_attributed` / `set_asm_fingerprint`
calls (the rewrite layer and test utils are the only allowed callers).

### D5 — CFG-detach pass split (item 2)

Today `DeadBranchElimination` does three things at once: (a) recognise
`If(const)`, (b) strip the dead Region predecessor + phi slots, (c) absorb
fingerprints. Split into:

- **DeadBranchElimination (reasoning):** recognises `If(const)` and emits a
  `ReplaceValue` rewrite redirecting live control past the `If`. It records
  which Region predecessors became dead (a worklist / set), but does not edit
  Region/phi structure.
- **CfgDetach (new, mechanical):** consumes the dead-predecessor set, walks the
  CFG via `walk.rs` (`cfg_reachable` / `region_predecessors`), and applies
  `RemoveRegionPred` for each. This is the only place region-predecessor removal
  lives; `RedundantPhis` keeps its single-pred collapse but no longer races with
  branch elimination.

The two communicate through an explicit structure (dead-edge set on the run
context), not by interleaved in-place edits.

### D6 — MemorySSAWalker (items 6, 8, 9)

```rust
pub trait MemorySSAWalker {
    /// May the memory written by `mem_def` alias the location read by `load`?
    fn may_alias(&self, load: NodeOutputId, mem_def: NodeOutputId) -> bool;
}

/// Find the nearest memory definition that may alias `load`, advancing the
/// load's memory cursor past provably-non-aliasing defs, recursing at MemPhi.
/// Returns the clobbering memory output, or None if the chain reaches
/// InitialMemory with no alias.
fn walk(&self, walker: &impl MemorySSAWalker, load: NodeOutputId)
    -> Option<NodeOutputId>;
```

Semantics (refined from the request's template):

- iterate the memory-token chain backward from `load`'s memory input;
- skip non-memory nodes;
- if a def `may_alias` the load → return it (nearest clobber);
- if it does not alias and is not a phi → advance the cursor to that def's own
  memory input and continue (this is "move the load's memory output to point at
  the node's memory");
- at a `MemPhi` → recurse into each predecessor's memory input; if all branches
  agree on a common reachable def, continue from there; otherwise the phi itself
  is the clobber boundary.

Two consumers, two `may_alias` impls:

- **load-forward** alias predicate: SP-rooted vs SP-rooted range overlap;
  constant vs constant range overlap; anchor identity; cross-class governed by
  the alias mode (D7).
- **function-args** alias predicate: does any store/call shadow the stack-arg
  slot range? The Call/CallOther clobber behaviour is a flag on this predicate
  (D8).

This replaces the bespoke `mem_walk.rs` `MemChainStep` machinery; the cycle
guards (phi-only vs every-node) become an internal detail of `walk`.

### D7 — alias-mode rename + default (item 9)

`AliasMode::AssumeStackConstDisjoint` → `AliasMode::AssumeStackGlobalDisjoint`
(SP-rooted addresses are assumed disjoint from global/constant addresses). Make
it the **default** (today `Strict` is default). `Strict` remains available for
callers that want the conservative behaviour. Wire the default through
`FunctionArgDetect`, `LoadForward`, and `CallStackArgCollect` constructors.

### D8 — Call/CallOther clobber toggle (item 8)

Add to the function-args alias predicate a `call_clobbers_args: bool`, **default
`false`** (a `Call`/`CallOther` on the memory chain does *not* shadow a stack-arg
slot — aggressive detection). When `true`, a call on the chain marks the slot
dirty (conservative). Surfaced as a constructor option on the relevant
pass(es) and threaded to Python as an analyze-option (low priority; Rust first).

### D9 — constructed optimizers (item 10)

Replace `thread_local!` pattern caches and lazy CC-layout lookups with fields
built in each pass's constructor:

- `FlagCmpCanonicalize { rules: Vec<BoxedRule> }` — built once in `new()`.
- `IfCondInversion { inner_pat: Pat<Concrete>, inner_capture: Capture }` — built
  in `new()`.
- `ConstantFold` rules likewise if they use a `thread_local!`.
- `FunctionArgDetect` / `CallStackArgCollect` / `LoadForward` already take a
  `PositionalArgLayout` at construction; extend that pattern to the alias mode
  and clobber toggle.

Because the workspace is single-threaded and `Pat`/`BoxedRule` are `!Send`, the
pipeline holds passes by value and there is no cross-thread concern. The
pipeline construction APIs (`build_*_optimizer_pipeline`) pass the needed data
(CC, endianness, alias mode) into the constructors.

### D10 — KnownBits: drop merge, iterate the map (items 11, 12)

See the dedicated sub-plan below.

## Detailed sub-plan: KnownBits rework (item 12, expands items 11 + 12)

### Current shape (from the spike)

- `analyze()` seeds a worklist with all reachable nodes; for each it computes
  `(output, KnownBitsFacts)` and calls `known[out].merge(kb)`; on change it
  re-enqueues consumers; iterates to fixpoint.
- `merge` unions ones/zeros and errors on contradiction
  (`ones & other.zeros != 0 || zeros & other.ones != 0`).
- A separate rewrite phase walks reachable nodes and, for any fully-known
  output, replaces it with an `IntConst`.

### Why `merge` is unnecessary (item 11)

Each output's facts are *recomputed from scratch* from its inputs' current facts
every time the node is processed; the recomputed value is monotonically more
precise than the stored one, and the stored one starts at "unknown" (all bits
unknown = `ones=0, zeros=0`). Since we always replace the stored facts with the
freshly recomputed facts (and only enqueue consumers when the recompute changed
something), the union-with-previous is a no-op against "unknown" on first visit
and against an equal-or-less-precise prior on later visits. So:

- store `known[out] = recomputed` directly (overwrite, not union);
- "changed?" is `recomputed != previous`;
- drop the contradiction check (it can only fire if the recompute itself is
  inconsistent, which is a lattice/transfer-function bug, not a merge concern —
  if we want the assertion we keep it as a `debug_assert` inside the transfer
  function, not a `merge` API).

This removes `KnownBitsFacts::merge` and its `Result`.

### Iterate the map to rewrite (item 12)

The current rewrite phase re-walks the graph and re-checks each node. Instead,
after `analyze()` returns the `KnownBitsMap` (a `SecondaryMap<NodeOutputId,
KnownBitsFacts>`), iterate the map directly:

1. `analyze()` returns the populated map (already does).
2. Iterate `(out, facts)` over the map's populated entries. For each:
   - skip if not fully known (`ones | zeros != type_mask`);
   - skip if `out`'s producer is already an `IntConst` (no-op);
   - skip outputs whose kind is not an integer value (control/memory/phi-token);
   - emit a `ReplaceValue` rewrite: `out` → `IntConst(ones, ty)`.
3. The rewrite layer handles fingerprint absorption (Tier 1); the pass reports
   `Changed` iff any replacement fired.

Edge cases this "iterate the map" approach resolves vs the while/for re-walk:

- **No double-processing.** The map has one entry per output; we visit each
  once, instead of re-walking the graph and re-deriving facts that
  `analyze()` already computed.
- **Stale/again-reachable nodes.** We only iterate outputs the analysis actually
  populated; detached/zombie outputs never entered the map, so they can't be
  spuriously folded.
- **Ordering independence.** Replacement is a pure per-output decision from the
  final fixed-point map, so it does not matter in which order we apply them —
  no consumer re-enqueue is needed during rewrite (the fixpoint already
  happened in `analyze`). This is the core simplification: the rewrite phase is
  a flat pass over a finished map, not a second worklist loop.
- **I1 / boolean outputs.** `ones | zeros == type_mask` with `bit_width(I1)==1`
  folds a fully-known boolean to `IntConst(0|1):I1` uniformly with wider ints.

Open question for the plan (not blocking): whether to also iterate the map to
fold *partially*-known shifts/masks (e.g. an AND whose result is known-zero in
all bits). For now we keep parity with today's "fully-known → const" only;
broader folds are out of scope.

Implementation order for this workstream:
1. Test pinning current KnownBits folding behaviour (regression net).
2. Replace `merge` with overwrite + `!=` change detection; delete `merge`.
3. Switch rewrite phase to map iteration emitting `ReplaceValue` rewrites.
4. Confirm gate parity (no new folds, no lost folds) on fixtures.

## Workstream sequencing

The six workstreams land in this order (each green before the next):

1. **RPO foundation** (D3, item 1 + 7). Add `rpo` + test; migrate `decompose_sp`
   to the flat loop; migrate the analysis passes' iteration. Lowest-level, most
   widely depended-on.
2. **Rewrite layer** (D4, item 3). Add `RewriteOp` variants; migrate the easy
   value-rewrite passes (LoadReadOnly, RedundantPhis value arms, LoadForward
   forward, DeadBranch redirect). Establish the "no manual fingerprint" CI check
   incrementally.
3. **CFG-detach split** (D5, item 2). Extract `CfgDetach`; move region-pred
   removal there via `RemoveRegionPred`; wire the dead-edge handshake.
4. **Memory-SSA unification** (D6/D7/D8, items 6 + 8 + 9). Introduce
   `MemorySSAWalker`; refactor function-args + load-forward; rename + redefault
   alias mode; add clobber toggle.
5. **ROM + context** (D1/D2, items 4 + 5). Change `read` signature + all impls
   and call sites; add endianness to `OptCtx`; move decode into the optimizer.
6. **KnownBits** (D10, items 11 + 12) and **constructed optimizers** (D9, item
   10). Independent of the others; can land last (or in parallel by a separate
   session).

Workstreams 1–4 are interdependent (RPO → rewrite → detach → mem-SSA). 5 and 6
are largely independent and could be parallelised, but to keep gates clean we
land them after 4.

## Testing strategy

- Every workstream is TDD: a failing test pinning the new behaviour (or the
  current behaviour as a regression net) before implementation.
- The existing fixture-based gates remain the integration backstop; the
  regression criterion stays "no NEW failures" against the known baseline.
- The always-on asm-fingerprint validator is the safety net for the
  Rewrite-only migration: any missed propagation surfaces as a validation error,
  not silent fingerprint loss.
- A small CI/grep check enforces "no direct fingerprint mutation in `opt/`."

## Risks / things to watch

- **RPO direction** (D3): pinned by a fixture test before migration. If the
  intended semantics differ from defs-before-uses, this is the redirect point.
- **Rewrite generality** (D4): `RemoveRegionPred` and `SwapOutputConsumers` are
  new structural ops; their fingerprint-exempt status must be justified against
  the superset-only contract (Region/Phi/If outputs involved are exempt or carry
  no new value).
- **Mem-SSA parity** (D6): the egraph/aliasing parity gap already noted in
  project memory means the refactor must be measured for node-count parity, not
  just correctness — a regression here is easy to miss.
- **Behaviour-preserving by construction**: workstreams 1–3 should be pure
  refactors (no fold/output changes); 4 changes the *default* alias mode (D7)
  and so is an intentional behaviour change to validate on fixtures.

## Out of scope

- Reshaping `NodeOutputType` (deferred per project memory).
- Broadening KnownBits to partial folds.
- Python-surface changes beyond threading the new options (kept minimal,
  Rust-first).
