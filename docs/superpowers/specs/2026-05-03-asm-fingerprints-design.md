# Assembly-instruction fingerprints on IR nodes

Status: design — supersedes none, additive feature.

Date: 2026-05-03

## Motivation

When a pattern query against the IR returns a `Match`, the user needs a way to
sanity-check the result against the source binary: which assembly instructions
of the disassembled function explain the captured node?  We want a
**proof-of-correctness aid**: every captured node carries a *set of
assembly-instruction addresses* — its **fingerprint** — that names every
machine instruction whose lifting (or whose subsequent rewrite into this node)
contributed to the node's value.

## Hard correctness contract

* **Overestimation is allowed; underestimation is not.**  A fingerprint may
  list instruction addresses that did not strictly contribute (cheap
  superset).  It may *not* omit a contributing address — that would weaken the
  proof.
* **The invariant survives every optimisation pass**, including the
  destructive ones.  Passes may *grow* fingerprints; they may not shrink them
  or replace a node with one whose fingerprint omits an ancestor.
* **Two structurally identical nodes still dedup.**  Fingerprints live on a
  side-table keyed by `NodeId`, never on the dedup key.  When `create_node`
  hits the cache, the new contributors are *unioned* into the existing entry.
* **End-to-end through Python.**  `Match.asm_fingerprint(c)` works in both
  Rust and Python.

## Vocabulary

* **Asm address** — the `MachineInsnAddr.addr: u64` of the parent machine
  instruction that produced one or more pcode insns.  We track only the
  machine-level address, never the per-pcode `insn_index`.  Multiple pcode
  insns sharing one machine instruction all attribute the same address.
* **Fingerprint** — the set of asm addresses associated with a `NodeId`.
  Stored sorted-and-deduplicated for cheap union and stable iteration.
* **Contributor** — any asm address that should appear in a node's
  fingerprint.  Two ways to become a contributor:
  1. *Lift-time*: the lifter creates the node while processing a pcode insn
     whose machine-address is `A`.
  2. *Optimisation-time*: a pass rewrites a node `M` whose use is then
     redirected to this node `N`; `N` absorbs `M`'s fingerprint (and so on
     transitively, since `M` was already a fingerprint accumulator).

## Design points (with rationale)

### 1. Storage shape

`Graph::asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>`, default empty.
The `Vec<u64>` is kept **sorted ascending and deduplicated** so:

* `extend_asm_fingerprint` is `O(N+M)` worst-case (sorted-merge), `O(M)` if
  `M` is the size of the new contributor list.
* Equality / "contains" checks are cheap.
* The on-disk / display format is canonical.

`Vec<u64>` is preferred over `BTreeSet<u64>` because the typical fingerprint
is small (1–4 entries for lift-time-only nodes; bounded by a small constant
for the deepest folded chain), allocation pressure dominates, and a sorted
`Vec` outperforms a `BTreeSet` at this size.  `SmallVec<[u64; 1]>` would save
the heap allocation for the single-address case, but adds a 24-byte stack
footprint to every node *with* a fingerprint and complicates the public
slice accessor — the existing `stack_phi_offsets: SecondaryMap<NodeId,
Vec<i64>>` precedent already pays the same cost; we follow it.

Public API on `Graph`:

```rust
pub fn asm_fingerprint(&self, node_id: NodeId) -> &[u64];
pub fn set_asm_fingerprint(&mut self, node_id: NodeId, addrs: Vec<u64>);
pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]);
pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId);
```

`extend_*` perform sorted-merge into the existing entry without ever shrinking
it; `set_*` is for tests / synthetic graphs only.

### 2. Where the asm address comes from

`rsleigh::Insn` does **not** carry an address.  `cfg::RegionInstruction`
wraps each pcode insn as `(PcodeInsnAddr, rsleigh::Insn)`, and
`PcodeInsnAddr.machine_addr.addr: u64` is exactly the value we want.
`strider::Strider::process_insn` already receives `cfg::PcodeInsnAddr` (today
named `_addr`); it is the natural plumbing point.

### 3. Lift-time attribution

`pcode_lift::ValueLifter` gains an optional `current_asm_addr: Option<u64>`
field.  The strider per-region loop sets it via
`ValueLifter::with_asm_addr(addr)` immediately before each call to `lift`,
and the **same loop** sets it before the strider-side handlers
(`handle_store`, `handle_call`, `handle_call_indirect`, `handle_return`,
`handle_call_other`, `handle_branch`, `handle_cond_branch`).  Pcode
canonicalisations (`Sub→Add+Neg`, `LessEqual→BoolNeg+Less`, etc.) all
synthesise nodes inside the lifter using the same address.

Implementation strategy: rather than adding asm-address arguments to every
internal `make_*` helper, we add a single `Graph::record_contributor(node_id,
addr)` method and route attribution through a small wrapper exposed on
`FunctionBuilder`:

```rust
impl FunctionBuilder {
    pub fn set_lift_addr(&mut self, addr: Option<u64>);
    pub fn lift_addr(&self) -> Option<u64>;
}
```

`FunctionBuilder::create_node` (the central entry — every `build_*` /
`make_*` ultimately calls it) reads `self.lift_addr` and, when `Some`,
invokes `graph.extend_asm_fingerprint(node_id, &[addr])`.  Optimisation
passes do not use `lift_addr`; they call `extend_asm_fingerprint{,_from}`
directly with the correct contributor set.

When `create_node` hits the dedup cache (returns an existing `NodeId`), the
attribution call still runs, **unioning** the current address into the
existing entry — exactly matching the contract that two structurally
identical nodes share one fingerprint that is the union of every
contributor's addresses.

`vn_io.rs` (`read_vn` / `write_vn` / register-aliasing helpers) goes through
the same `create_node`, so register-extract / -insert / -shift / -mask nodes
inherit attribution automatically.  No special-case code in `vn_io.rs`.

### 4. Region / phi / initial nodes

Allowed-to-be-empty (documented exemption):

* `Entry`, `InitialMemory`, `InitialVar(_)`, `FunctionArg` — synthesised at
  function-entry, before any insn has been processed.
* `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi` —
  synthesised as part of region setup, *not* by an asm insn.

A node is **expected non-empty** if it is reachable via `walk_graph(entry)`
**and** its kind is not in the exempt set above.  The Layer-C validator
rejects an empty fingerprint on any non-exempt reachable node, but only when
fingerprint validation is opted in.

Why allow empty for phis / region nodes?  Their existence is a *structural*
consequence of the CFG, not of any particular machine instruction.  Marking
them with the address of the join-point's first contributing insn would
require ad-hoc heuristics that would lie just as readily as the empty set
does, with worse readability.  Patterns that match phis can call
`graph.asm_fingerprint(node)` and accept the empty result — captures of
*value* nodes (the common case) always have a non-empty fingerprint.

### 5. Optimisation pass invariant + helper

> **Invariant**: for every reachable node `N` after a pass `P`, `N`'s
> fingerprint contains the union of fingerprints of every node that was
> rewritten / folded into `N`'s value during `P`.

Concretely, every pass that performs a rewrite of the form "redirect uses of
old's output to new's output" must additionally call:

```rust
graph.extend_asm_fingerprint_from(new_node_id, old_node_id);
```

For chained rewrites (e.g. `(a&C1)&C2 → a&(C1&C2)` materialises a fresh
`IntConst(C1&C2)`, then a fresh `And(a, fresh)`, then redirects the outer
`(a&C1)&C2`'s use to the new outer `And`), every intermediate fresh node
absorbs the union of its source operands' fingerprints; the final outer
`And` absorbs the original outer `And`'s.  In practice the simplest pattern
is "absorb all old `NodeId`s involved in the rewrite into the surviving
`NodeId`".

To enforce no-shrink, `extend_asm_fingerprint{,_from}` use sorted-merge and
never delete entries; `set_asm_fingerprint` is **not used by passes** (and
Layer C never expects shrinkage).

### 6. Per-pass plan

| Pass | Action |
| --- | --- |
| `ConstantFold` | Every fresh constant / fresh node absorbs the fingerprint of every old node whose use it replaces.  All folding rules in `rules.rs` live behind `make_*_node` + `replace_all_uses` — we add the `extend_*` call alongside `replace_all_uses` in each rule. |
| `KnownBits` | Same as `ConstantFold`: rules synthesise a const replacement and `replace_all_uses` it.  Absorb the old node's fingerprint into the const node. |
| `IfCondInversion` | Swaps `If`'s control outputs; same `If` `NodeId` survives.  No new node ⇒ no fingerprint update needed (the `If`'s fingerprint already describes the cond's contributing insns). |
| `RedundantPhis` | Phi `N` with single reachable predecessor is replaced by that predecessor's value.  We absorb `N`'s fingerprint into the predecessor producer node before redirecting uses (in practice empty, since phis are exempt; but the helper is a no-op for empty input, so calling it always is safe). |
| `DeadBranchElimination` | Strips a control edge.  No data-flow rewrites that could lose attribution; nothing to do. |
| `CallOtherElide` | Drops a `CallOther` whose user-op is a no-op.  The call's *output* (if any) was already the input to its consumers via the IR's standard path — but elide-able call-others have no value output (their output is the memory token).  Memory token uses are redirected to the previous memory; we absorb the elided `CallOther`'s fingerprint into the surviving memory producer. |
| `LoadReadOnly` | Replaces a `Load` with an `IntConst`.  Absorb the `Load`'s fingerprint into the new const. |
| `StackStoreDetect` | Replaces `Store` with `StackStore { offset }` / `StackStorePhi`.  The replacement node is created via `set_node_kind` on the *same* `NodeId` (non-cacheable kinds), so the existing fingerprint is preserved automatically — no work needed. |
| `StackLoadForward` | Forwards stored value to a later `Load`.  Absorb the `Load`'s fingerprint into whatever value-producer node receives the redirected uses (if a fresh truncation/extension is created, it absorbs both the `Load`'s and the `Store`'s fingerprints). |
| `IndirectBranchResolve` (the classifier) | Read-only inspection; no rewrites; nothing to do. |
| `CallStackArgCollect` (post-pass) | Adds new value inputs to a `Call` node by re-pointing existing memory loads.  No fresh nodes other than the existing argument loads, which already carry their own attribution.  Nothing to do. |
| `FunctionArgDetect` (post-pass) | Replaces an `InitialVar(arg_reg)` use with a freshly-minted `FunctionArg` node.  `FunctionArg` is in the exempt-empty set, so no attribution required; existing consumers' attribution is unchanged. |

For `strider::indirect_resolve::inplace::{apply_link_register, apply_tail_call}`:

* `apply_link_register` replaces the placeholder's kind with `Return` via
  `set_node_kind`; same `NodeId` survives → fingerprint preserved.
* `apply_tail_call` synthesises a `Call` + `Return` chain.  We absorb the
  placeholder's fingerprint into both new nodes.

`GraphRewriter` / `pattern::rewrite_rule` is used by strider only after the
orchestrator finishes; it goes through `replace_all_uses` and currently
creates fresh nodes via the standard graph helpers.  We extend its facade so
each rewrite absorbs the fingerprint of every replaced root into the
substituted root.

### 7. Validator hook (Layer C, opt-in)

`ir::validate::validate(graph, entry)` adds a Layer-C check that walks the
reachable nodes via `walk_graph` and flags any **non-exempt** node with an
empty fingerprint.  Exempt kinds: `Entry`, `InitialMemory`, `InitialVar`,
`FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `StackStorePhi`,
`IfCase`.

The check is opt-in via `validate_with_options(graph, entry,
ValidateOptions { check_asm_fingerprints: true, .. })`.  The plain
`validate` function continues to run with `check_asm_fingerprints: false`,
so the existing 100+ tests that build mock graphs without attribution stay
green.  The integration test on `fixtures/out/x86/arithmetic.elf` opts in.

`opt::OptimizerPipeline::run` also keeps the default-off behaviour to avoid
breaking existing pass tests; a new
`OptimizerPipeline::with_asm_fingerprint_check(true)` flips the bit.

### 8. Pattern + Python surfaces

```rust
// pattern crate
impl Match {
    pub fn asm_fingerprint(&self, c: Capture, graph: &Graph) -> &[u64];
}
```

The accessor reads `graph.asm_fingerprint(self.node(c)?)` directly; returns
an empty slice when the capture is unbound (no successful match) or when
the capture-bound node is exempt-empty.

```python
# strider-py
class Match:
    def asm_fingerprint(self, c) -> list[int]: ...
```

The Python accessor mirrors `match.stack_phi_offsets` precisely: it reads
through the underlying Rust `Match` and converts to a `list[int]`.

### 9. Test strategy

* **`ir`** unit tests: side-table get/set/extend, sorted-merge correctness,
  dedup-cache union semantics.
* **`pcode-lift`** unit test: a synthetic insn at addr `0xABCD` produces
  nodes whose fingerprint contains `0xABCD`.
* **`opt`** per-pass tests: fingerprints survive the rewrite (one test per
  pass, building a synthetic input graph with known pre-rewrite addresses,
  asserting the post-rewrite fingerprint is a superset).
* **`strider`** integration test: lift `fixtures/out/x86/arithmetic.elf`,
  run the full pipeline with `check_asm_fingerprints: true`, walk every
  reachable node, assert the validator passes.
* **Pattern test**: capture an `Add` node from a known offset in the
  `arithmetic.elf` IR; assert the captured fingerprint contains the asm
  address of the actual `add` machine instruction.
* **Python test**: same pattern test, in pytest.

## Out of scope

* Per-pcode-insn granularity (the user explicitly asked for asm-level).
* Cross-function attribution (each function's fingerprint set is local).
* Visualisation: we do not modify `dot` output to render fingerprints in
  this slice.  A future pass can read `graph.asm_fingerprint(id)` and
  decorate node labels.
* `compute_asm_fingerprints_from_scratch` recovery path: if a pass forgets
  to absorb, we surface it via Layer C, not by recomputing.

## Risks / known gaps

1. **Pass-author discipline.**  Adding a new pass without absorbing
   fingerprints will silently produce false negatives.  The Layer-C check
   (when opted in) catches the obvious "fresh non-exempt node has empty
   fingerprint" case but cannot catch a *partial* under-attribution.
   Mitigation: documentation in `CLAUDE.md` (the worktree copy) + an
   integration test that exercises every pass.
2. **Constant-fold dedup collisions.**  Two unrelated constant folds may
   produce the same `IntConst(K)` and dedup to the same `NodeId`.  Both
   contributors' addresses are unioned — correct per-spec, but the
   resulting fingerprint may surprise users.  Documented as a feature of
   the cache, not a bug.
3. **Performance.**  `extend_asm_fingerprint` is hot (called once per
   `create_node`).  Sorted-merge on a typically-1-element vector is O(1) +
   one allocation; an `O(1)` short-circuit when `addr` is the last
   element keeps the steady-state cost to a single comparison.

## Implementation order

(Each item ends with a green workspace + a focused commit; each goes
through `feature-dev:code-reviewer` before commit.)

1. **Spec + plan doc** (this file + `2026-05-03-asm-fingerprints-plan.md`).
2. `ir`: side-table + accessors + tests.  Layer-C check, off by default.
3. `pcode-lift`: thread asm address; attribute on `create_node`.  Synthetic
   test.
4. `strider`: set the lift-addr around every `process_insn` and around
   every strider-side handler.  Update `indirect_resolve` rewrites.
5. `opt`: per-pass propagation + per-pass test.  Order:
   `ConstantFold` → `KnownBits` → `LoadReadOnly` → `StackStoreDetect` →
   `StackLoadForward` → `RedundantPhis` → `DeadBranchElimination` →
   `CallOtherElide` → `IfCondInversion` (no-op).
6. `pattern`: `Match::asm_fingerprint`.
7. `strider-py`: Python accessor + Python test.
8. Integration test on `fixtures/out/x86/arithmetic.elf` (opt in to
   Layer-C check).
9. Update worktree `CLAUDE.md`.
