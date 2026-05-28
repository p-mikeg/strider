# Base-aware stack offsets

> Implement test-first; commit per part; push after each.

**Goal:** Make the stack-slot identity `(base, offset)` everywhere instead of
a base-less `offset`, where `base` is the SP-derived terminal node
`decompose_sp` already returns (`InitialVar(sp)` *or* an alignment-masked
`And/Or(sp, mask)`). Fixes two latent issues with one root cause:

- **`LoadForward` soundness**: it discards the base (`SpRooted { offset }`)
  and matches stores/loads by offset alone, so a `store@(initialSP, K)` and a
  `load@(alignedSP, K)` on one memory chain are forwarded — wrong value.
- **Side-table coverage**: `StackOffsetDetect` only stamps `InitialVar(sp)`,
  so aligned-frame (`and rsp,-16`) accesses are invisible to `call_stack_args`,
  the `LoadPat`/`StorePat` `stack_only`/`offset_capture`, and dot rendering.

`decompose_sp` already returns the base; every consumer throws it away. The
fix threads it through. Two accesses are the same slot iff **same base node
AND same offset**; different bases can't be related (gap is `initial_sp mod
align`, caller-dependent) → conservatively `MayAlias` / no match.

## Part 1 — LoadForward (soundness; self-contained)

File: `crates/strider-analyze/src/opt/load_forward/mod.rs` (+ tests.rs).

- `AddrClass::SpRooted { offset }` → `SpRooted { base: NodeOutputId, offset }`.
- `classify_addr`: `Terminal { base, offset }` → `SpRooted { base, offset }`.
- `alias_verdict`: the `(SpRooted, SpRooted)` arm matches/disjoints only when
  `base == base`; different base → `MayAlias`. (`Constant` arm unchanged.)

TDD: (a) RED — store on `InitialVar(sp)+K`, load on `And(sp,-16)+K`, one mem
chain: currently forwarded; after fix, NOT forwarded. (b) within a single
aligned base, store→reload at same offset still forwards (no regression).

## Part 2 — side-table coverage

Files: `strider-ir` (`Function::stack_offsets` + accessors + compact remap),
`strider-analyze` (`StackOffsetDetect`, `call_stack_args`), dot renderer.

- `stack_offsets: SecondaryMap<NodeId, Option<(NodeOutputId, i64)>>` — value
  is `(base, offset)` where `base` is the SP-derived terminal *output*
  (matching `decompose_sp`'s `Terminal.base`). `stack_offset(id) ->
  Option<(NodeOutputId, i64)>`; `set_stack_offset(id, base, offset)`.
- **compact** must remap the value's `base` `NodeOutputId` via
  `NodeIdRemap::output_old_to_new` (un-gated from test-only) alongside the
  key remap — the base output survives compaction as a data input to the
  live Load/Store.
- `StackOffsetDetect`: stamp `(base_node, offset)` for any SP base (drop the
  `InitialVar(sp)`-only filter).
- `call_stack_args`: compare `(base, offset)` (only match stores on the
  call-site SP base it expects).
- dot renderer: keep suppressing the addr edge / labelling with the offset;
  base optional in the label.

TDD: `StackOffsetDetect` stamps an `And(sp,-16)+K` store with its aligned base.

## Part 3 — pattern layer

Files: `pattern/var.rs`, `pattern/matcher/bindings.rs`, `pattern/pat/builders/
memory.rs`, `pattern/matcher/match_result.rs`, `strider-py` mirrors.

- `OffsetCapture` binds `(base, offset)`: `bind_offset(oc, base, offset)`; the
  join requires both equal.
- `captured_offset(c) -> Option<i64>` unchanged (offset; base pinned by join).
- `stack_only` stays base-agnostic; `stack_offset_filter(K)` stays
  base-agnostic (documented "offset within its own frame").
- User-facing API unchanged: `offset_capture(c)` + same-capture join now
  sound across bases and covers aligned frames.

TDD: an `offset_capture` join must NOT unify a store/load at equal offset on
different SP bases; must unify within one aligned base.

## Final
Full workspace tests + clippy + docs + pytest; code-review; merge.
