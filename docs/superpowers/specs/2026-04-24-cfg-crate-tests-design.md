# CFG crate test suite — design

**Date:** 2026-04-24
**Scope:** `crates/cfg/`
**Goal:** Basic + edge-case tests for every function with logic in the `cfg` crate, with tests living in `crates/cfg/tests/` rather than inline. Matches the migration pattern recently applied to `reader`.

---

## Context

The `cfg` crate lifts a single function from a binary to a graph of basic blocks via `rsleigh`. Its layered shape:

1. **Types** (`src/cfg/types.rs`) — `MachineInsnAddr`, `PcodeInsnAddr`, `Region`, `RegionInstruction`, `RegionEdgeKind`.
2. **Options** (`src/cfg/options.rs`) — `Options`, `OptionsBuilder` (fluent).
3. **Builder** (`src/cfg/builder/{mod,region_builder,split,testing}.rs`) — work-queue driven: `Builder::build` seeds the entry address, pops items, and routes them through `explore` → `find_region_containing_addr` → either `split_region` (if address lands mid-region), `explore_new_region` (new region via `RegionBuilder`), or a direct edge to an existing region's start. `RegionBuilder` lifts pcode via `rsleigh::Sleigh::lift_one` and dispatches per opcode (`Branch`, `CondBranch`, `Return`, else continue).
4. **Cfg** (`src/cfg/mod.rs`) — finished graph. Query API: `region_insn`, `region_if`, `region_branch`, `regions`, `region_ids`, `following_regions` (internal dedup).
5. **Dot dumper** (`src/cfg/dot.rs`) — `vn_to_name` for register/const/ram/unique spaces; `CfgDotDumper` writes dot/html output.

Errors flow through `strider_error::define_error!` (location chain + backtrace).

### Current test state (before this work)

Inline `#[cfg(test)]` tests:

- `src/cfg/mod.rs` — 7 tests (addr types, RegionEdgeKind distinctness, Region::contains_addr).
- `src/cfg/options.rs` — 4 tests (OptionsBuilder combinations).
- `src/cfg/builder/mod.rs` — 7 tests (add_region happy + empty, find_region_containing_addr full grid).
- `src/cfg/builder/region_builder.rs` — 6 tests (is_branch_tail_call{,_nocheck}).
- `src/cfg/builder/split.rs` — 4 tests (split_region: no-op, split, ranges, fallthrough, rewire).
- `src/cfg/builder/testing.rs` — shared `pub(super)` helpers (addr, make_builder, make_region, fake_insn, make_region_builder, make_sleigh).

Integration tests:

- `tests/cfg_integration.rs` + `tests/helpers.rs` — arch-parametrized macro (`arch_tests!`) runs one shared structural suite against `binary_tests/out/{x86,x64,aarch64,arm}/test.elf`. Covers linear/single-conditional/nested-conditional/looping/recursive/tail-call functions, entry-address invariant, `region_if`/`region_branch` basics, global invariants. ARM is `#[ignore]` (BranchIndirect not yet a region terminator).

### Gaps

- `Builder::explore` has no direct unit test (covered only transitively).
- `RegionBuilder::decode_branch_target` — CONST-relative, default-absolute, and invalid-space paths are untested.
- `RegionBuilder::process_new_insn` / `process_insn` — no direct tests for Branch (tail + non-tail), CondBranch (both successors, insn_index-rollover vs next-machine-insn), Return, non-terminating, or fallthrough-into-existing paths.
- `RegionBuilder::finish_current_region` — empty-insns → `NoInstructionsRegionBuilder` error untested.
- `Builder::split_region` — `FailedSplitingRegion` (addr not matching any insn) untested.
- `Cfg::following_regions` — `DuplicateEdgeKind` error path untested.
- `Cfg::region_insn` — both happy path and `InvalidRegion` untested.
- `Cfg::regions` / `region_ids` — basic iteration untested.
- `Cfg::vn_to_name` — every space variant (CONST, REGISTER, RAM, UNIQUE) + `InvalidRegVn` + `UnsupportedVnSpaceDisplay` untested.
- `CfgDotDumper::dump_as_dot` — no smoke test.
- Option effects (`fn_max_size`, `allow_code_before_start_addr`) on real `Builder::build` runs untested.

---

## Design decisions

### D1 — Move tests to `tests/`, following the reader precedent

All tests live in `crates/cfg/tests/`. Inline `#[cfg(test)]` blocks in every `src/cfg/**` file are deleted. `src/cfg/builder/testing.rs` is deleted; its helpers migrate to `tests/common/synthetic.rs`.

**Why:** matches the recently-applied pattern in `reader` (commits `9c0d699`, `856cba4`, `586cba4`). Black-box-style tests; no test scaffolding in the shipping source tree except the deliberate `test_api` surface described in D2.

### D2 — Expose internal builder API via `#[doc(hidden)] pub mod test_api`

`cfg`'s internals are almost entirely `pub(super)` (`Builder` fields, `RegionBuilder` struct and methods, `add_region`, `split_region`, `find_region_containing_addr`, `is_branch_tail_call*`, `decode_branch_target`, `process_insn`, `finish_current_region`, `vn_to_name`). Integration tests cannot reach any of them.

A single `#[doc(hidden)] pub mod test_api` at the crate root re-exposes the minimum needed. It is pure forwarding — no new logic — and is module-documented as "test-only API; not covered by semver".

```rust
// crates/cfg/src/test_api.rs
//! Test-only API.  Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Do not use from downstream crates.

#[doc(hidden)]
pub use crate::cfg::builder::test_api::*;
#[doc(hidden)]
pub use crate::cfg::dot_test_api::*;
```

Inside the crate, per-module `test_api` sub-modules forward the needed items (e.g. `crates/cfg/src/cfg/builder/test_api.rs` re-exports `Builder` field accessors, `add_region`, `find_region_containing_addr`, `split_region`, `RegionBuilder`, tail-call predicates, `decode_branch_target`, `process_insn`, `finish_current_region`, and the testing helpers formerly in `testing.rs` that need crate-internal access).

**Why not a `test-util` cargo feature:** self-referencing dev-deps don't work and `cargo test -p cfg` would need `--features test-util`, breaking `cargo test --workspace` ergonomics. `#[doc(hidden)]` hides the module from rustdoc; users opening the crate docs never see it.

**Why not widen everything to `pub`:** the `pub(super)` annotations reflect a real design choice. One hidden forwarding module contains the leak.

### D3 — Synthetic binaries as raw byte arrays via `BufMemReader`

For the specific code paths that real binaries don't reach (mid-instruction CondBranch, fn_max_size boundary, InvalidTailCall trigger, allow_code_before_start_addr flip), tests assemble small x86-64 byte sequences and feed them through `rsleigh::mem_readers::BufMemReader::new(bytes, base_addr)`.

**Why not `object::write`:** `BufMemReader` accepts raw bytes; no ELF envelope is required for `Builder::build` to lift them. `object::write` was the right call in `reader` (parsing ELFs end-to-end); it would be pure overhead here.

**Arch for synthetic tests:** x86-64 only. The existing `arch_tests!` macro covers cross-arch structural shape on real binaries; synthetic tests pin specific arch-independent builder paths, so they run on whichever arch is cheapest to write bytes for.

### D4 — Keep `arch_tests!` parametrization; factor helpers into `common/`

The arch-parametrized macro in `tests/cfg_integration.rs` stays — it is the cross-arch confidence signal and is cheap to maintain. `tests/helpers.rs` is decomposed into `tests/common/{real_binary,assertions}.rs` (used by every integration file, not just `cfg_integration.rs`). `cfg_integration.rs` is trimmed of any cases duplicated by the new per-topic files so the arch matrix remains focused on cross-arch shape.

### D5 — Pin option effects on real binaries

Today, `OptionsBuilder` is unit-tested at the struct level; there is no test that verifies its flags actually change `Builder::build` output. `tests/build_end_to_end.rs` adds:

- `fn_max_size` set below a real function's inner branch target → branch classified as tail call (region has `ends_with_tail_call = true`).
- `allow_code_before_start_addr` set on a function with a branch below its entry (constructed synthetically) → branch is followed, not treated as a tail call.

---

## File layout

```
crates/cfg/tests/
├── common/
│   ├── mod.rs                        # re-exports
│   ├── real_binary.rs                # binary(arch), symbol_addr, build_cfg
│   ├── assertions.rs                 # region_count, count_edges_of_kind, has_cycle,
│   │                                 # all_conditional_regions_well_formed,
│   │                                 # assert_linear_function / single_conditional /
│   │                                 # looping / global_invariants
│   └── synthetic.rs                  # addr, make_builder, make_builder_opts, make_sleigh,
│                                     # fake_insn, make_region, make_region_builder,
│                                     # small raw-bytes x86-64 helpers (ret_only, jmp_ret, etc.)
│
├── addr_types.rs                     # MachineInsnAddr, PcodeInsnAddr ordering
├── region.rs                         # Region::contains_addr
├── options.rs                        # OptionsBuilder combinations
├── region_edge_kind.rs               # variant distinctness
│
├── builder_add_region.rs             # add_region: basic, empty → EmptyRegion, two regions
├── builder_find_region.rs            # find_region_containing_addr: empty, at-start, interior,
│                                     # last-insn, beyond-end, two-adjacent routing
├── builder_split_region.rs           # split_region: no-op-at-start, two regions + ranges,
│                                     # fallthrough edge, incoming rewiring,
│                                     # addr-not-in-insns → FailedSplitingRegion
├── region_builder_tail_call.rs       # is_branch_tail_call{,_nocheck}: below-start default,
│                                     # below-start+allow, within-fn no-limit,
│                                     # fn_max_size boundary both sides, insn_index=0 ok,
│                                     # insn_index≠0 → InvalidTailCall, within-fn any idx ok
├── region_builder_decode.rs          # decode_branch_target: CONST-relative, default-absolute,
│                                     # invalid space → InvalidBranchTargetVaErr
├── region_builder_process.rs         # process_new_insn: non-terminating,
│                                     # Branch non-tail enqueues target, Branch tail-call ends,
│                                     # CondBranch enqueues both cases, CondBranch false-case
│                                     # insn_index+1 rollover to next machine insn,
│                                     # Return ends region;
│                                     # process_insn: existing-region-at-addr → Fallthrough edge;
│                                     # finish_current_region: with+without parent edge,
│                                     # empty → NoInstructionsRegionBuilder
│
├── build_end_to_end.rs               # Builder::build via synthetic bytes + real binaries:
│                                     # split-by-back-jump, fn_max_size triggers tail-call
│                                     # and sets ends_with_tail_call, allow_code_before_start_addr
│                                     # negates tail-call classification, InvalidTailCall trigger
│
├── cfg_query.rs                      # region_insn (basic + InvalidRegion);
│                                     # regions/region_ids iteration;
│                                     # region_if (both, one-present, neither);
│                                     # region_branch (present, None);
│                                     # DuplicateEdgeKind via manually-constructed Cfg
│
├── vn_to_name.rs                     # CONST, REGISTER, RAM, UNIQUE formatting;
│                                     # InvalidRegVn, UnsupportedVnSpaceDisplay
│
├── dot_dumper.rs                     # CfgDotDumper smoke: non-empty output for real binary;
│                                     # edge style per RegionEdgeKind;
│                                     # iter_nodes covers every graph node
│
└── cfg_integration.rs                # (existing) arch_tests! macro — trimmed of duplicates,
                                       # kept as cross-arch matrix
```

Files deleted:

- `crates/cfg/src/cfg/builder/testing.rs`
- `crates/cfg/tests/helpers.rs`
- all inline `#[cfg(test)] mod tests { ... }` blocks in `src/cfg/**`
- all inline `#[cfg(test)] mod tests { ... }` blocks in `src/cfg/builder/**`

Files added:

- `crates/cfg/src/test_api.rs` (crate root; `#[doc(hidden)] pub`)
- `crates/cfg/src/cfg/builder/test_api.rs`
- `crates/cfg/src/cfg/dot_test_api.rs` (or an inline `test_api` sub-module inside `dot.rs`; chosen during implementation for the smallest diff)

---

## Coverage matrix

Every function with logic gets at least one positive test and at least one edge/error test where one exists.

| Function | Basic | Edges / Errors |
|---|---|---|
| `MachineInsnAddr::from<u64>` | round-trip | ordering |
| `PcodeInsnAddr` Ord | machine-addr primary | insn-index tiebreak, equality, antisymmetry |
| `Region::contains_addr` | start, end, interior | pcode-interior, before-start, after-end, empty-insns |
| `OptionsBuilder` | default | `set_function_max_size`, `allow_code_before_start_addr`, both set |
| `RegionEdgeKind` | — | all four variants pairwise distinct |
| `Builder::add_region` | inserts into graph + map | `EmptyRegion`; two regions preserve indices |
| `Builder::find_region_containing_addr` | at-start, interior, last-insn | empty graph → None, beyond-end → None, adjacent regions route correctly |
| `Builder::split_region` | interior split → 2 regions, ranges, fallthrough edge, parent rewiring | no-op at start, `FailedSplitingRegion` on addr-not-in-insns |
| `Builder::explore` | — | transitively covered by `region_builder_process.rs` + `build_end_to_end.rs` (new-region, existing-start, split paths) |
| `Builder::build` | linear, single-cond, loop (via real binaries) | synthetic: split-by-back-jump, fn_max_size triggers tail-call, allow_code_before_start_addr, InvalidTailCall |
| `RegionBuilder::decode_branch_target` | CONST-relative, default-absolute | invalid space → `InvalidBranchTargetVaErr` |
| `RegionBuilder::is_branch_tail_call_nocheck` | below-start default, within-fn no-limit | below-start + allow negates, fn_max_size boundary both sides |
| `RegionBuilder::is_branch_tail_call` | valid insn_index=0 tail, within-fn any idx | insn_index≠0 tail → `InvalidTailCall` |
| `RegionBuilder::process_new_insn` | non-terminating, Branch non-tail, CondBranch both cases enqueued | Branch tail-call ends, CondBranch false-case rollover to next machine insn, Return ends |
| `RegionBuilder::process_insn` | new insn → delegates | addr is existing region's start → Fallthrough edge + finish |
| `RegionBuilder::finish_current_region` | with parent edge, without parent edge | empty insns → `NoInstructionsRegionBuilder` |
| `RegionBuilder::build` | — | transitively covered by `build_end_to_end.rs` |
| `Cfg::following_regions` | unique-kinded outgoing edges | two same-kinded edges → `DuplicateEdgeKind` |
| `Cfg::region_branch` | present, absent | malformed graph (via manually-constructed Cfg) |
| `Cfg::region_if` | both successors, one present one absent, neither | malformed graph |
| `Cfg::regions` / `region_ids` | iteration count matches `graph.node_count()` | — |
| `Cfg::region_insn` | returns clone of region's insns | `InvalidRegion` for out-of-graph `NodeIndex` |
| `Cfg::vn_to_name` | CONST, REGISTER, RAM, UNIQUE | `InvalidRegVn` on bad register offset, `UnsupportedVnSpaceDisplay` on exotic space |
| `CfgDotDumper::dump_as_dot` | real-binary smoke: output non-empty, ≥1 node emitted, edge styles present per kind found in graph | — |

---

## Components

### `tests/common/real_binary.rs`

Lifted from `tests/helpers.rs` without change:

```rust
pub fn binary(arch: &str) -> std::path::PathBuf;
pub fn symbol_addr(binary_path: &str, fn_name: &str) -> u64;
pub fn build_cfg(
    binary_path: &str,
    fn_name: &str,
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
) -> cfg::Cfg<reader::ElfFileMemReader>;
```

### `tests/common/assertions.rs`

Lifted from `tests/helpers.rs` without change:

```rust
pub fn region_count<R>(cfg: &Cfg<R>) -> usize;
pub fn count_edges_of_kind<R>(cfg: &Cfg<R>, kind: RegionEdgeKind) -> usize;
pub fn outgoing_edge_kinds<R>(cfg: &Cfg<R>, id: NodeIndex) -> Vec<RegionEdgeKind>;
pub fn has_cycle<R>(cfg: &Cfg<R>) -> bool;
pub fn all_edge_endpoints_valid<R>(cfg: &Cfg<R>) -> bool;
pub fn entry_has_no_predecessors<R>(cfg: &Cfg<R>) -> bool;
pub fn all_conditional_regions_well_formed<R>(cfg: &Cfg<R>) -> bool;
pub fn assert_linear_function<R>(cfg: &Cfg<R>, name: &str);
pub fn assert_single_conditional<R>(cfg: &Cfg<R>, name: &str);
pub fn assert_looping_function<R>(cfg: &Cfg<R>, name: &str);
pub fn assert_global_invariants<R>(cfg: &Cfg<R>, name: &str);
```

### `tests/common/synthetic.rs`

Ported from `src/cfg/builder/testing.rs`, accessing crate internals through `cfg::test_api`:

```rust
pub fn addr(machine: u64, insn: u64) -> cfg::PcodeInsnAddr;
pub fn fake_insn() -> rsleigh::Insn;
pub fn make_region(addrs: &[(u64, u64)]) -> cfg::Region;
pub fn make_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>>;
pub fn make_sleigh_with_bytes(
    bytes: Vec<u8>, base: u64,
) -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>>;
pub fn make_builder(start_addr: u64) -> cfg::test_api::Builder<…>;
pub fn make_builder_opts(start_addr: u64, options: cfg::Options) -> cfg::test_api::Builder<…>;
pub fn make_region_builder<'a>(builder: &'a mut Builder, start: PcodeInsnAddr)
    -> cfg::test_api::RegionBuilder<'a, …>;

// Small x86-64 raw-bytes helpers for synthetic Builder::build tests:
pub fn ret_only() -> Vec<u8>;                    // single `ret`
pub fn jmp_ret_ret() -> Vec<u8>;                 // `jmp +1; ret; ret` — unreachable region
pub fn back_jump() -> Vec<u8>;                   // forward insn, then `jmp` back — split trigger
pub fn cond_branch_loop() -> Vec<u8>;            // small loop with `jne`
pub fn short_with_unused_tail(tail_len: u8)
    -> Vec<u8>;                                  // deterministic body + `ret` + `tail_len` nops
```

### `tests/common/mod.rs`

```rust
pub mod assertions;
pub mod real_binary;
pub mod synthetic;

pub use assertions::*;
pub use real_binary::*;
pub use synthetic::*;
```

### `tests/<topic>.rs` — per-topic files

Each integration-test file declares `mod common;` at the top (Rust's integration-test conventions resolve it to `tests/common/mod.rs` automatically) and pulls the symbols it needs. Contents are enumerated in the **Coverage matrix** and **File layout** sections.

### `src/test_api.rs` and per-module `test_api` sub-modules

`#[doc(hidden)] pub` forwarding modules. The crate root `lib.rs` adds:

```rust
#[doc(hidden)]
pub mod test_api;
```

The per-module sub-modules (`src/cfg/builder/test_api.rs`, etc.) re-export exactly what tests need and nothing more. Everything these modules export remains logically `pub(super)` / `pub(crate)` for the crate itself; the `test_api` surface is a deliberate, documented leak.

---

## Pinned contracts

Two silent behaviors become explicit tests with commentary in the test file:

1. **Commit invariant on tail-call classification** — a `Branch` whose target is below `start_addr` is a tail call iff `allow_code_before_start_addr == false`. Asserted in `region_builder_tail_call.rs` with a `// Contract:` comment.
2. **Split ownership** — after `split_region`, the *second* half keeps the original `NodeIndex`, the first half gets a fresh one. Asserted in `builder_split_region.rs` with a `// Contract:` comment explaining why (child edges + work-queue parent references must continue to resolve).

---

## Dev-dependency changes

No new dev-dependencies. The crate's current `[dev-dependencies]` (`reader`, `object`, `petgraph`) remain sufficient — raw-byte synthetic tests use `rsleigh::mem_readers::BufMemReader`, which is already in the runtime deps.

---

## Migration plan

Review-sized commits; each one leaves `cargo test --workspace` green.

1. **Common scaffold.** Create `tests/common/{mod,real_binary,assertions}.rs` by lifting verbatim from `tests/helpers.rs`. Update `tests/cfg_integration.rs` to `mod common;` and call the same helpers through the new path. Delete `tests/helpers.rs`. No behavior change; arch-param suite still passes.
2. **Pure-type migrations.** Create `tests/addr_types.rs`, `tests/region.rs`, `tests/options.rs`, `tests/region_edge_kind.rs`. Delete corresponding `#[cfg(test)]` blocks from `src/cfg/mod.rs` and `src/cfg/options.rs`. No new coverage — just a move.
3. **Add `test_api` surface.** Add `src/test_api.rs` + `src/cfg/builder/test_api.rs` + `src/cfg/dot_test_api.rs` (or an inline sub-module). `#[doc(hidden)] pub`. Forwarding-only. Crate still builds; no public API change as seen in rustdoc.
4. **Builder-unit migrations + synthetic helper move (atomic).** Port `src/cfg/builder/testing.rs` into `tests/common/synthetic.rs` using `cfg::test_api`. Create `tests/builder_add_region.rs`, `tests/builder_find_region.rs`, `tests/builder_split_region.rs`: port every existing inline test and add the `FailedSplitingRegion` case. In the **same commit**, delete the inline `mod tests` blocks from `src/cfg/builder/{mod,split}.rs` **and** delete `src/cfg/builder/testing.rs`. Done atomically because the inline tests and `testing.rs` depend on each other; splitting would leave a broken checkpoint.
5. **RegionBuilder tail-call migration.** `tests/region_builder_tail_call.rs` — port existing inline tests. Delete the inline `mod tests` block from `src/cfg/builder/region_builder.rs`.
6. **RegionBuilder new coverage.** `tests/region_builder_decode.rs` and `tests/region_builder_process.rs` — first new ground (decode_branch_target, process_new_insn/process_insn, finish_current_region).
7. **Cfg query coverage.** `tests/cfg_query.rs` — region_insn, region_if, region_branch, regions/region_ids, DuplicateEdgeKind via manually-constructed Cfg.
8. **vn_to_name coverage.** `tests/vn_to_name.rs` — every space variant + error cases.
9. **End-to-end synthetic.** `tests/build_end_to_end.rs` — split-by-back-jump, fn_max_size, allow_code_before_start_addr, InvalidTailCall.
10. **Dot dumper smoke.** `tests/dot_dumper.rs` — non-empty output, edge-style coverage.
11. **Final prune.** Trim `cfg_integration.rs` of anything now duplicated by per-topic files (it stays as the cross-arch signal, not a catch-all). Run `cargo clippy --workspace` and `cargo test --workspace`.

**Expected final count:** ~75–90 tests, up from ~34 today.

---

## Out of scope

- Property-based tests (proptest/quickcheck). Current assertions are sufficient; property tests could be layered later for `find_region_containing_addr` and `split_region`.
- Fuzzing `Builder::build` on random bytes. Worthwhile for production robustness but not a coverage tool for this task.
- Benchmarks.
- Coverage tooling (`cargo-llvm-cov`).
- ARM integration tests: remain `#[ignore]` until `BranchIndirect` is handled as a region terminator. Out of scope here.
- Rewriting `arch_tests!` macro. It is a good fit for cross-arch structural coverage and stays as-is (only its duplicated cases are trimmed).
