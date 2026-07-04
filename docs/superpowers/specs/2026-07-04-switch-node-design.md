# Switch IR Node + Control-Node Patterns — Design

**Date:** 2026-07-04
**Branch:** `feat/2026-07-04-switch-node`

## Goal

Replace the switch/indirect-branch **if-ladder lowering** (`build_switch_if_ladder`
in `strider-lift`, the last production caller of the eager
`FunctionBuilder::create_region`) with a real `Switch` IR node, add its collapse
to `DeadBranchElimination`, and add matcher patterns for `Switch`,
`IndirectBranch`, and `Unreachable`. After this, production lifting is 100%
pruned-SSA and the eager `create_region` path is test-only.

## Motivation

- A resolved jump table (`ResolvedTargets::Multiple`) is lowered today by
  `handle_switch` → `build_switch_if_ladder`, which synthesizes N−2 intermediate
  "dispatcher" regions via eager `create_region` (a `Phi` per tracked varnode
  per dispatcher). Those phis are all redundant (each dispatcher is
  single-predecessor) and get collapsed by `RedundantPhis` — transient waste,
  and the only production use of eager `create_region`.
- The CFG already models `dispatch_region → N target regions` **directly** (the
  cfg builder emits one `Branch` edge per target; there are NO dispatcher
  regions in the CFG). The if-ladder therefore introduces **IR-only regions that
  diverge from the CFG** — the pruned-SSA IDF is computed on the CFG's
  `dispatch → targets` shape while the IR routes through extra dispatchers. A
  `Switch` node makes the IR match the CFG.

## Decisions (approved)

- **Selector = the dispatch address** (`Switch.inputs[1]`), the same value
  `handle_switch` reads today (`RegionTerminator::Switch.target_vn`). No 0..N-1
  index extraction / plumbing.
- **Case addresses in a side table**, not graph inputs: `NodeId → Vec<u64>`,
  positional (control output `i` ↔ `cases[i]`).
- **No explicit default output** — the resolved jump table is exhaustive
  (`address` is guaranteed to be one of the targets), so N outputs = N cases.

## Design

### 1. The `Switch` IR node (`strider-ir`)

- `NodeKind::Switch` — unit variant, **non-cacheable** (like `If` / `Region` /
  `Call`; `NodeKind::is_cacheable` returns false).
- Signature (`node_signature::expected_signature`):
  `inputs: [CTRL, VALUE]` (control edge + dispatch-address value);
  `outputs: []; out_tail: CTRL` — N variadic control outputs, one per target, in
  target order. Output `i` is taken when `address == cases[i]`.
- Side table `Function::switch_targets: SecondaryMap<NodeId, Vec<u64>>` —
  `switch_targets[switch_node][i]` = the target machine address for control
  output `i`. Registered in the side-table registry so `Function::compact`
  remaps it and `Function::retain_reachable` drops culled entries, exactly like
  `stack_offsets` / `call_other_names`. Accessors:
  `switch_targets(node) -> &[u64]`, `set_switch_targets(node, Vec<u64>)`.
- **Validation** (`validate/graph_invariants.rs`): every reachable `Switch`
  node's `switch_targets` length equals its control-output count (one address
  per output), and it has ≥1 output.

### 2. Builder verb (`strider-ir`)

`FunctionBuilder::build_switch(address: ValueId, arms: &[(RegionId, u64)]) -> Result<()>`:

- Terminates the current region (snapshots `ctrl`/`mem`).
- Requires `address` is a value edge; requires ≥1 arm.
- Creates a `Switch` node with inputs `[ctrl, address]` and `arms.len()` control
  outputs.
- Links each control output `i` to `arms[i].0` (the target IR region), in order,
  via the existing `link_region` machinery — so each target `Region`'s control
  input list gets the switch output as a predecessor edge (matching the CFG's
  per-target `Branch` edge).
- Records `arms.iter().map(|(_, addr)| *addr)` into `switch_targets`.

### 3. Lifter: `handle_switch` → `Switch` (`strider-lift`)

- `handle_switch` keeps building `targets_and_regions: Vec<(u64, RegionId)>` and
  reading `idx = read_vn(target_vn)` (the address) as it does today.
- Replace the `build_switch_if_ladder(...)` call with
  `self.builder.build_switch(idx, &arms)` where `arms[i] = (region_i, target_i)`.
- The `n == 1` degenerate case stays a plain `build_branch` (unchanged behavior).
- **Delete** `build_switch_if_ladder` and its unit tests; replace with
  `build_switch` unit tests.

### 4. `DeadBranchElimination`: switch collapse (`strider-opt`)

Extend the existing peephole (it folds `If(const)`):

- `matches_kind` also returns true for `NodeKind::Switch`.
- On a `Switch` whose **address input folds to a constant `K`**: find the output
  `i` with `switch_targets[node][i] == K`, replace that control output with the
  switch's `ctrl_in` (so the live target receives control directly), and
  **kill** the `Switch` (absorbing the address cone's asm-fingerprint into the
  surviving edge, mirroring how the `If` fold absorbs the condition). `CfgDetach`
  + `PhiCollapse` finish the teardown of the now-dead arms.
- `K` matching no case (should not happen for an exhaustive table) → no fold
  (leave the node; conservative).

### 5. Patterns (`strider-pattern` + `strider-py`)

- Rust `strider-pattern` builders:
  - `switch(...)` — matches a `Switch` node; can capture/constrain the address
    input.
  - `indirect_branch(...)` — matches `IndirectBranch[ctrl, mem, target]`; can
    capture the `target` input.
  - `unreachable()` — matches an `Unreachable[ctrl]` node.
  These are node-rooted with no value output (same shape as the existing `ret`
  builder).
- Python `Py*Pat` mirror via the in-crate `node_builder!` macro in
  `strider-py/src/pattern.rs`, plus `strider.pattern` free-function exposure.

### 6. Dot rendering (`strider-ir`)

`function::dot` renders a `Switch` node with its case addresses inline (e.g.
`switch → [0x401000, 0x401020, …]`), mirroring how `If` shows its two arms.

### 7. Remove the now-unneeded eager `create_region` flow

After §3 deletes `build_switch_if_ladder`, eager `create_region` has **no
production callers** (only tests + `strider-ir-test-utils`). Gate it test-only:

- Mark `FunctionBuilder::create_region` (and the `seed_current == true` branch of
  `create_region_with`, if cleanly separable) `#[cfg(any(test, feature =
  "test-util"))]`, matching the existing `record_register_arg_carriers` pattern.
  Production (`strider-lift`) uses only `create_region_pruned`.
- If the `seed_current` flag becomes single-valued in non-test builds, simplify
  `create_region_with` accordingly (pruned path only) — but keep it behavior-
  preserving for tests.
- Add a guard: a test (or a `#[cfg(not(test))]` compile check) asserting no
  production path constructs an eager region.

## Testing (TDD — write the failing test first for each unit)

- **Unit (`strider-ir`):** `build_switch` produces a `Switch` with N control
  outputs, `[ctrl, address]` inputs, and `switch_targets` of length N; validation
  rejects a `switch_targets`/output-count mismatch; dot renders the addresses.
- **Unit (`strider-opt`):** switch collapse — a `Switch` with a constant address
  input folds to the single matching arm and the `Switch` is killed; a
  non-constant address is left untouched.
- **Unit (`strider-pattern` / `strider-py`):** each new pattern matches its node
  kind and rejects others; capture of the address / target input works.
- **Integration (`strider-lift` / orchestrator):** a function with a resolved
  jump table lifts to a `Switch` node (assert no if-ladder / no dispatcher
  regions); `analyze()` resolves it; the 6 kernel functions + pytest suite are
  unaffected (behavior-preserving end to end).
- **Guard:** no production caller of eager `create_region` remains.

## Non-goals

- Extracting a normalized 0..N-1 index (the address selector is used as-is).
- An explicit `default` arm (tables are exhaustive).
- Changing the CFG `Switch` terminator representation (`target_vn` + `targets`
  are consumed as-is).
- Migrating the hundreds of test `create_region` call sites to pruned SSA
  (eager stays available to tests).
