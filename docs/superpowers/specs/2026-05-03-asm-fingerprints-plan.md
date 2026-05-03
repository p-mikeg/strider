# Asm-fingerprints — implementation plan

Companion to `2026-05-03-asm-fingerprints-design.md`.  Each phase is a
focused commit; every functional slice is built TDD-first (failing test →
implementation → green).

## Phase 0 — spec + plan (this commit)

* Spec + plan in `docs/superpowers/specs/`.
* No code changes.
* Commit: `docs: asm-fingerprints design + implementation plan`.

## Phase 1 — `ir` side-table + accessors

* Add `asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>` to `Graph`.
* Add `Graph::asm_fingerprint`, `set_asm_fingerprint`,
  `extend_asm_fingerprint`, `extend_asm_fingerprint_from`.
* Add `validate::ValidateOptions { check_asm_fingerprints: bool }` and
  `validate_with_options(graph, entry, opts)`.  `validate(graph, entry)`
  becomes `validate_with_options(graph, entry, ValidateOptions::default())`.
* Layer-C exemption list:
  `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`,
  `MemPhi`, `VarPhi`, `StackStorePhi`, `IfCase`.
* Tests in `crates/ir/src/graph/tests.rs`:
  * unset → empty slice;
  * set + read;
  * extend dedup-and-sort behaviour;
  * extend_from copies the source's slice into the destination, sorted;
  * dedup-cache hit unions both contributors;
  * Layer-C with check on, exempt node OK, non-exempt empty fails.
* Commit: `ir: asm-fingerprint side-table + opt-in Layer-C check`.

## Phase 2 — `pcode-lift` lift-time attribution

* Add `current_asm_addr: Option<u64>` to `ValueLifter`, plus
  `with_asm_addr` setter.
* Add `FunctionBuilder::lift_addr` / `set_lift_addr` and route
  attribution through `FunctionBuilder::create_node` so every helper
  (`build_*`, `make_*`, `vn_io.rs`) automatically records the address.
* Update `ValueLifter` so before each `lift` call (in tests this is
  already a single-shot construction; in production it'll be set from
  strider) the inner builder's `lift_addr` matches.
* Test: synthetic `IntAdd` insn at addr `0x42`; build a tiny graph;
  assert the produced `Add` node's fingerprint contains `0x42`.
* Test: dedup — two insns at `0x42` and `0x84` both produce
  `Add(IntConst(1), IntConst(2))`; the cached node carries both.
* Commit: `pcode-lift: thread current asm address; attribute on create_node`.

## Phase 3 — `strider` per-insn drive

* In `IrStrider::process_insn`, drop the `_addr` underscore; before each
  arm (value-lifter `lift`, every handler, every special terminator)
  call `self.builder.set_lift_addr(Some(addr.machine_addr.addr))`.
* In `pipeline.rs`'s special-terminator handlers, set the lift addr to
  the terminator pcode's machine address before invoking
  `handle_unresolved_indirect_branch`, `handle_switch`, `handle_tail_call`.
* `indirect_resolve::inplace::apply_link_register` — `set_node_kind` keeps
  the same `NodeId`; nothing to do.
* `indirect_resolve::inplace::apply_tail_call` — synthesise the new
  `Call` and `Return` with the placeholder node's fingerprint absorbed.
* Existing `cargo run -p strider --example strider` still succeeds.
* Test: build a region with two known-address insns; assert fingerprints
  of every produced value node are non-empty and equal to the
  contributing addresses.
* Commit: `strider: set lift addr around every per-insn dispatch`.

## Phase 4 — `opt::ConstantFold` propagation + tests

* In every rewrite rule, after `replace_all_uses`, call
  `extend_asm_fingerprint_from` to absorb the rewritten node's
  fingerprint into the survivor.
* Test per major rule (`x+0→x`, `x^x→0`, AND-mask merge, all-const eval,
  bool/float subset).
* Commit: `opt::ConstantFold: absorb fingerprints across rewrites`.

## Phase 5 — `opt::KnownBits` + `opt::LoadReadOnly`

* Same pattern: absorb on every replacement.
* Per-pass test.
* Commit: `opt: KnownBits + LoadReadOnly preserve asm-fingerprints`.

## Phase 6 — stack passes

* `StackStoreDetect`: same `NodeId` (uses `set_node_kind`); no work.
* `StackLoadForward`: absorb both `Load`'s and forwarded `Store`'s
  fingerprints into the survivor.
* Per-pass test.
* Commit: `opt: stack passes preserve asm-fingerprints`.

## Phase 7 — phi / branch / call-other passes

* `RedundantPhis`, `DeadBranchElimination`, `CallOtherElide`,
  `IfCondInversion`.  All but `RedundantPhis` and `CallOtherElide` are
  no-op for fingerprints; absorb on the two that redirect.
* Per-pass test.
* Commit: `opt: redundant-phis / call-other / dead-branch preserve asm-fingerprints`.

## Phase 8 — `pattern::Match::asm_fingerprint`

* Add accessor; tests with a synthetic graph.
* Commit: `pattern: Match::asm_fingerprint(c, graph)`.

## Phase 9 — `strider-py` surface

* `PyMatch.asm_fingerprint(c)` — convert `&[u64]` to `list[int]`.
* Python test in `crates/strider-py/tests/python/`.
* Commit: `strider-py: Match.asm_fingerprint(c) -> list[int]`.

## Phase 10 — end-to-end integration test

* Build `fixtures/out/x86/arithmetic.elf` if not present (the
  workspace's `make -C fixtures ARCH=x86 CASE=arithmetic` target).
* Test runs `strider::run` with the asm-fingerprint Layer-C check
  enabled; walks every reachable node; asserts each non-exempt one has
  a non-empty fingerprint.
* Test captures a known `Add` from a hand-picked function and asserts
  the fingerprint contains the actual `add` instruction's asm address.
* Commit: `strider: end-to-end asm-fingerprint integration test`.

## Phase 11 — CLAUDE.md update

* Document the side-table and the per-pass invariant in the worktree's
  `CLAUDE.md`, mirroring the `stack_phi_offsets` / `call_other_names`
  precedent.
* Commit: `docs: CLAUDE.md — document asm-fingerprint side-table`.

## Verification gate

Before declaring done:

* `cargo build --workspace`
* `cargo clippy --workspace -- -D warnings`
* `cargo test --workspace`
* `cd crates/strider-py && uv run pytest`

Raw output captured in the final report.
