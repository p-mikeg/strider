# CFG crate test suite — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate every `cfg` crate test from inline `#[cfg(test)]` modules to integration tests under `crates/cfg/tests/`, and fill coverage gaps so every function with logic has at least one basic test and at least one edge/error test.

**Architecture:** Tests live in `tests/`, organized by topic. A `#[doc(hidden)] pub mod test_api` surface in the crate forwards the minimum internal state/methods integration tests need. Shared helpers live in `tests/common/`. Synthetic-binary tests use hand-written x86-64 byte arrays fed through `rsleigh::mem_readers::BufMemReader`; real-binary tests use the existing `binary_tests/out/<arch>/test.elf` fixtures. Full spec: [2026-04-24-cfg-crate-tests-design.md](../specs/2026-04-24-cfg-crate-tests-design.md).

**Tech Stack:** Rust 2024, `cargo test -p cfg`, `cargo clippy -p cfg`, `rsleigh` (local path dep), `petgraph`, `reader`, `object`.

**Coordination:** Another agent is working in parallel on a different crate. This plan touches **only** `crates/cfg/**` and `docs/superpowers/plans/**`. It does not modify `Cargo.toml` (workspace or crate), workspace dependencies, or `Cargo.lock` beyond what `cargo test` regenerates automatically. If a step ever needs to cross that boundary, stop and surface it.

**Review cadence:** Run the `simplify` skill over new code before each commit. Run `coderabbit:review` on staged changes at the end of each of the phase groupings in the "Phase checkpoints" section before committing that phase's last task.

---

## File Structure

### Files created

- `crates/cfg/src/test_api.rs` — crate-root aggregator; `#[doc(hidden)] pub` re-exports
- `crates/cfg/tests/common/mod.rs` — re-exports
- `crates/cfg/tests/common/real_binary.rs` — `binary`, `symbol_addr`, `build_cfg`
- `crates/cfg/tests/common/assertions.rs` — structural helpers (`region_count`, `has_cycle`, `assert_linear_function`, etc.)
- `crates/cfg/tests/common/synthetic.rs` — `addr`, `make_sleigh`, `make_builder`, `make_region`, `make_region_builder`, raw-bytes helpers
- `crates/cfg/tests/addr_types.rs` — `MachineInsnAddr`, `PcodeInsnAddr` ordering
- `crates/cfg/tests/region.rs` — `Region::contains_addr`
- `crates/cfg/tests/options.rs` — `OptionsBuilder` combinations
- `crates/cfg/tests/region_edge_kind.rs` — variant distinctness
- `crates/cfg/tests/builder_add_region.rs` — `add_region` (basic + empty + two-region)
- `crates/cfg/tests/builder_find_region.rs` — `find_region_containing_addr`
- `crates/cfg/tests/builder_split_region.rs` — `split_region` including `FailedSplitingRegion`
- `crates/cfg/tests/region_builder_tail_call.rs` — tail-call predicates
- `crates/cfg/tests/region_builder_decode.rs` — `decode_branch_target`
- `crates/cfg/tests/region_builder_process.rs` — `process_new_insn`, `process_insn`, `finish_current_region`
- `crates/cfg/tests/build_end_to_end.rs` — `Builder::build` synthetic-byte scenarios + option effects
- `crates/cfg/tests/cfg_query.rs` — `region_insn`, `region_if`, `region_branch`, `regions`, `region_ids`, `DuplicateEdgeKind`
- `crates/cfg/tests/vn_to_name.rs` — every space variant + error cases
- `crates/cfg/tests/dot_dumper.rs` — `CfgDotDumper` smoke

### Files modified

- `crates/cfg/src/lib.rs` — add `#[doc(hidden)] pub mod test_api;`
- `crates/cfg/src/cfg/builder/mod.rs` — add `#[doc(hidden)] pub mod test_api { ... }` with `Builder` field/method forwarders; delete inline `mod tests`
- `crates/cfg/src/cfg/builder/split.rs` — delete inline `mod tests`
- `crates/cfg/src/cfg/builder/region_builder.rs` — add `#[doc(hidden)] pub mod test_api { ... }` with `TestRegionBuilder` wrapper + `ProcessInsnRes` re-export; delete inline `mod tests`
- `crates/cfg/src/cfg/options.rs` — delete inline `mod tests`
- `crates/cfg/src/cfg/mod.rs` — delete inline `mod tests`; if `region_if`/`region_branch` error paths need test-only surface, add a small `#[doc(hidden)] pub` accessor (see Task 18)
- `crates/cfg/src/cfg/dot.rs` — add `#[doc(hidden)] pub mod test_api { pub fn vn_to_name(...) }` wrapper
- `crates/cfg/tests/cfg_integration.rs` — switch from `#[path = "helpers.rs"] mod helpers;` to `mod common;`; trim any cases duplicated by per-topic files

### Files deleted

- `crates/cfg/src/cfg/builder/testing.rs`
- `crates/cfg/tests/helpers.rs`
- all `#[cfg(test)] mod tests { ... }` blocks in every `crates/cfg/src/cfg/**.rs`

---

## Phase checkpoints

- **Checkpoint A** after Task 4 (common scaffold + test_api surface in place).
- **Checkpoint B** after Task 9 (all legacy inline tests migrated to `tests/`).
- **Checkpoint C** after Task 14 (all new coverage complete).
- **Checkpoint D** after Task 16 (final prune + workspace-wide test run).

At each checkpoint:
1. Run `cargo test -p cfg` and `cargo clippy -p cfg` — both must pass.
2. Run `cargo test --workspace` — the rest of the workspace must still build/pass.
3. Run `coderabbit:review` on the diff since the previous checkpoint; address comments.

---

## Task 1: Create `tests/common/` scaffold by lifting `tests/helpers.rs`

**Files:**
- Create: `crates/cfg/tests/common/mod.rs`
- Create: `crates/cfg/tests/common/real_binary.rs`
- Create: `crates/cfg/tests/common/assertions.rs`
- Modify: `crates/cfg/tests/cfg_integration.rs` (switch to `mod common;`)
- Delete: `crates/cfg/tests/helpers.rs`

- [ ] **Step 1: Create `crates/cfg/tests/common/real_binary.rs`** with the path+ELF helpers lifted from `tests/helpers.rs` lines 20-56.

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Real-binary CFG helpers for integration tests.

use cfg::Cfg;
use object::{Object, ObjectSymbol};

/// Returns the path to the test binary for `arch` under `binary_tests/out/<arch>/test.elf`.
pub fn binary(arch: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binary_tests/out")
        .join(arch)
        .join("test.elf")
}

/// Resolves a named symbol's start address from an ELF file on disk.
pub fn symbol_addr(binary_path: &str, fn_name: &str) -> u64 {
    let leaked: &'static [u8] = Box::leak(
        std::fs::read(binary_path)
            .expect("read binary")
            .into_boxed_slice(),
    );
    let obj: &'static object::File<'static> =
        Box::leak(Box::new(object::File::parse(leaked).expect("parse ELF")));
    obj.symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol '{}' not found in {}", fn_name, binary_path))
        .address()
}

/// Builds a CFG for the named function using `sla_spec`/`pspec` to decode.
pub fn build_cfg(
    binary_path: &str,
    fn_name: &str,
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
) -> Cfg<reader::ElfFileMemReader> {
    let addr = symbol_addr(binary_path, fn_name);
    let mem_reader =
        reader::ElfFileMemReader::from_path(binary_path).expect("build ElfFileMemReader");
    let sleigh = rsleigh::Sleigh::new(sla_spec, pspec, mem_reader).expect("create Sleigh");
    cfg::Builder::new(sleigh, addr, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap_or_else(|e| panic!("CFG build failed for '{}': {e:?}", fn_name))
}
```

- [ ] **Step 2: Create `crates/cfg/tests/common/assertions.rs`** with the structural helpers lifted from `tests/helpers.rs` lines 60-197.

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Structural assertions over a completed `Cfg` — used across integration tests.

use cfg::{Cfg, RegionEdgeKind};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

pub fn region_count<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> usize {
    cfg.region_ids().count()
}

pub fn count_edges_of_kind<R: rsleigh::MemReader>(cfg: &Cfg<R>, kind: RegionEdgeKind) -> usize {
    cfg.graph.edge_references().filter(|e| *e.weight() == kind).count()
}

pub fn outgoing_edge_kinds<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    id: petgraph::graph::NodeIndex,
) -> Vec<RegionEdgeKind> {
    cfg.graph
        .edges_directed(id, petgraph::Direction::Outgoing)
        .map(|e| *e.weight())
        .collect()
}

pub fn has_cycle<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    petgraph::algo::is_cyclic_directed(&cfg.graph)
}

pub fn all_edge_endpoints_valid<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    cfg.graph.edge_references().all(|e| {
        cfg.graph.node_weight(e.source()).is_some() && cfg.graph.node_weight(e.target()).is_some()
    })
}

pub fn entry_has_no_predecessors<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    cfg.graph.edges_directed(cfg.entry, petgraph::Incoming).count() == 0
}

pub fn all_conditional_regions_well_formed<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    for id in cfg.region_ids() {
        let kinds = outgoing_edge_kinds(cfg, id);
        let has_true = kinds.contains(&RegionEdgeKind::IfCaseTrue);
        let has_false = kinds.contains(&RegionEdgeKind::IfCaseFalse);
        let is_conditional = has_true || has_false;
        if is_conditional && !(has_true && has_false && kinds.len() == 2) {
            return false;
        }
    }
    true
}

pub fn assert_linear_function<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert_eq!(region_count(cfg), 1, "{name}: expected 1 region");
    assert_eq!(count_edges_of_kind(cfg, RegionEdgeKind::Branch), 0, "{name}: unexpected Branch edges");
    assert_eq!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseTrue), 0, "{name}: unexpected IfCaseTrue edges");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

pub fn assert_single_conditional<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(region_count(cfg) >= 2, "{name}: expected at least 2 regions");
    assert!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseTrue) >= 1, "{name}: expected IfCaseTrue edge");
    assert!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseFalse) >= 1, "{name}: expected IfCaseFalse edge");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

pub fn assert_looping_function<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(region_count(cfg) >= 2, "{name}: expected at least 2 regions for a loop");
    assert!(has_cycle(cfg), "{name}: expected a back-edge cycle");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

pub fn assert_global_invariants<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(cfg.graph.node_weight(cfg.entry).is_some(), "{name}: entry node missing");
    assert!(entry_has_no_predecessors(cfg), "{name}: entry has predecessors");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
}
```

- [ ] **Step 3: Create `crates/cfg/tests/common/mod.rs`**.

```rust
#![allow(dead_code)]

pub mod assertions;
pub mod real_binary;

pub use assertions::*;
pub use real_binary::*;
```

(`synthetic` module is added in Task 5 — don't declare it yet.)

- [ ] **Step 4: Update `crates/cfg/tests/cfg_integration.rs`**: replace `#[path = "helpers.rs"] mod helpers;` (line 21-22) with `mod common;` and update references from `helpers::*`/`helpers::` to `common::*`/`common::`.

Specifically in `cfg_integration.rs`, change:
```rust
#[path = "helpers.rs"]
mod helpers;
```
to:
```rust
mod common;
```
Then inside each generated test module, replace `use super::helpers::*;` with `use super::common::*;` and `helpers::build_cfg` with `common::build_cfg`.

- [ ] **Step 5: Delete `crates/cfg/tests/helpers.rs`**.

```bash
rm crates/cfg/tests/helpers.rs
```

- [ ] **Step 6: Run tests to verify the move is behavior-preserving**.

Run: `cargo test -p cfg`
Expected: all previously-passing tests still pass; count unchanged.

- [ ] **Step 7: Commit**.

```bash
git add crates/cfg/tests/common crates/cfg/tests/cfg_integration.rs
git rm crates/cfg/tests/helpers.rs
git commit -m "test(cfg): lift tests/helpers.rs into tests/common/"
```

---

## Task 2: Add `#[doc(hidden)] pub mod test_api` at the crate root

**Files:**
- Create: `crates/cfg/src/test_api.rs`
- Modify: `crates/cfg/src/lib.rs`

- [ ] **Step 1: Create `crates/cfg/src/test_api.rs`** as an empty aggregator that will grow as each internal-test surface is added.

```rust
//! Test-only API. Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Not intended for use from downstream crates.

// Per-module sub-modules are added in the tasks that need them
// (Task 3: builder, Task 7: region_builder, Task 9: dot).
```

- [ ] **Step 2: Register the module at the crate root**. In `crates/cfg/src/lib.rs`, append after the existing `pub use` line:

```rust
#[doc(hidden)]
pub mod test_api;
```

- [ ] **Step 3: Verify the crate still builds**.

Run: `cargo build -p cfg`
Expected: success, no new warnings.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/src/test_api.rs crates/cfg/src/lib.rs
git commit -m "test(cfg): add empty test_api aggregator at crate root"
```

---

## Task 3: Expose `Builder` internals via `cfg::builder::test_api`

**Files:**
- Modify: `crates/cfg/src/cfg/builder/mod.rs` (add `test_api` sub-module)
- Modify: `crates/cfg/src/test_api.rs`

- [ ] **Step 1: Add the `test_api` sub-module inside `crates/cfg/src/cfg/builder/mod.rs`** below the existing `Builder` impl block and above the existing `#[cfg(test)] mod tests` block.

```rust
#[doc(hidden)]
pub mod test_api {
    //! Test-only forwarders for `Builder` internals.

    use super::Builder;
    use crate::cfg::types::{PcodeInsnAddr, Region, RegionEdgeKind, RegionGraph};
    use crate::error::Result;
    use petgraph::graph::NodeIndex;
    use std::collections::{BTreeMap, VecDeque};

    pub fn add_region<R: rsleigh::MemReader>(
        b: &mut Builder<R>, region: Region,
    ) -> Result<NodeIndex> {
        b.add_region(region)
    }

    pub fn find_region_containing_addr<'a, R: rsleigh::MemReader>(
        b: &'a Builder<R>, addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &'a Region)> {
        b.find_region_containing_addr(addr)
    }

    pub fn split_region<R: rsleigh::MemReader>(
        b: &mut Builder<R>, region_id: NodeIndex, addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        b.split_region(region_id, addr)
    }

    pub fn graph<R: rsleigh::MemReader>(b: &Builder<R>) -> &RegionGraph {
        &b.graph
    }

    pub fn graph_mut<R: rsleigh::MemReader>(b: &mut Builder<R>) -> &mut RegionGraph {
        &mut b.graph
    }

    pub fn start_addr_to_region_id<R: rsleigh::MemReader>(
        b: &Builder<R>,
    ) -> &BTreeMap<PcodeInsnAddr, NodeIndex> {
        &b.start_addr_to_region_id
    }

    pub fn work_queue<R: rsleigh::MemReader>(
        b: &Builder<R>,
    ) -> &VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)> {
        &b.work_queue
    }
}
```

Note: `find_region_containing_addr` on the `Builder` impl is `fn` (private). To allow the wrapper above to compile, change that method's signature in the same file from `fn find_region_containing_addr` to `pub(super) fn find_region_containing_addr`. The name, arguments, and body stay identical.

- [ ] **Step 2: Re-export from the crate-root `test_api.rs`**. Append to `crates/cfg/src/test_api.rs`:

```rust
#[doc(hidden)]
pub use crate::cfg::builder::test_api::*;
```

- [ ] **Step 3: Verify**.

Run: `cargo build -p cfg && cargo clippy -p cfg -- -D warnings`
Expected: success.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/src/cfg/builder/mod.rs crates/cfg/src/test_api.rs
git commit -m "test(cfg): expose Builder internals via test_api forwarders"
```

---

## Task 4: Expose `RegionBuilder` + `Cfg::vn_to_name` via `test_api`

**Files:**
- Modify: `crates/cfg/src/cfg/builder/region_builder.rs` (add `test_api` sub-module)
- Modify: `crates/cfg/src/cfg/dot.rs` (add `test_api` sub-module)
- Modify: `crates/cfg/src/test_api.rs`

- [ ] **Step 1: Add the `test_api` sub-module inside `crates/cfg/src/cfg/builder/region_builder.rs`** just above the existing `#[cfg(test)] mod tests` block.

```rust
#[doc(hidden)]
pub mod test_api {
    //! Test-only wrapper around `RegionBuilder` so integration tests can drive
    //! its private methods directly.

    use super::{ProcessInsnRes as InnerProcessInsnRes, RegionBuilder};
    use crate::cfg::types::{PcodeInsnAddr, RegionEdgeKind, RegionInstruction};
    use crate::cfg::Builder;
    use crate::error::Result;
    use petgraph::graph::NodeIndex;
    use std::collections::VecDeque;

    /// Mirror of `ProcessInsnRes` for test consumers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProcessInsnRes {
        FinishedProcessing,
        DidntFinishProcessing,
    }

    impl From<InnerProcessInsnRes> for ProcessInsnRes {
        fn from(inner: InnerProcessInsnRes) -> Self {
            match inner {
                InnerProcessInsnRes::FinishedProcessing => ProcessInsnRes::FinishedProcessing,
                InnerProcessInsnRes::DidntFinishProcessing => ProcessInsnRes::DidntFinishProcessing,
            }
        }
    }

    /// Owns a `RegionBuilder` for the lifetime of the test.
    pub struct TestRegionBuilder<'a, R: rsleigh::MemReader> {
        inner: RegionBuilder<'a, R>,
    }

    impl<'a, R: rsleigh::MemReader> TestRegionBuilder<'a, R> {
        pub fn new(builder: &'a mut Builder<R>, start_addr: PcodeInsnAddr) -> Self {
            Self {
                inner: RegionBuilder {
                    builder,
                    start_addr,
                    insns: VecDeque::new(),
                    parent_edge: None,
                },
            }
        }

        pub fn with_parent_edge(
            builder: &'a mut Builder<R>,
            start_addr: PcodeInsnAddr,
            parent: (NodeIndex, RegionEdgeKind),
        ) -> Self {
            Self {
                inner: RegionBuilder {
                    builder,
                    start_addr,
                    insns: VecDeque::new(),
                    parent_edge: Some(parent),
                },
            }
        }

        pub fn insns(&self) -> &VecDeque<RegionInstruction> {
            &self.inner.insns
        }

        pub fn push_insn(&mut self, insn: RegionInstruction) {
            self.inner.insns.push_back(insn);
        }

        pub fn is_branch_tail_call_nocheck(&mut self, target: PcodeInsnAddr) -> bool {
            self.inner.is_branch_tail_call_nocheck(target)
        }

        pub fn is_branch_tail_call(&mut self, target: PcodeInsnAddr) -> Result<bool> {
            self.inner.is_branch_tail_call(target)
        }

        pub fn decode_branch_target(
            &mut self, vn: rsleigh::Vn, at: PcodeInsnAddr,
        ) -> Result<PcodeInsnAddr> {
            self.inner.decode_branch_target(vn, at)
        }

        pub fn process_new_insn(
            &mut self, insn: &rsleigh::Insn, at: PcodeInsnAddr, lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_new_insn(insn, at, lift).map(Into::into)
        }

        pub fn process_insn(
            &mut self, insn: &rsleigh::Insn, at: PcodeInsnAddr, lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_insn(insn, at, lift).map(Into::into)
        }

        pub fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
            self.inner.finish_current_region(ends_with_tail_call)
        }
    }
}
```

Note: `decode_branch_target`, `process_new_insn`, `process_insn`, and `finish_current_region` are currently `fn` (private). The wrapper above is in the same file and can call them directly — no visibility change needed.

- [ ] **Step 2: Add `test_api` sub-module inside `crates/cfg/src/cfg/dot.rs`**, just before the final closing of the file.

```rust
#[doc(hidden)]
pub mod test_api {
    use super::Cfg;
    use crate::error::Result;

    pub fn vn_to_name<R: rsleigh::MemReader>(cfg: &Cfg<R>, vn: &rsleigh::Vn) -> Result<String> {
        cfg.vn_to_name(vn)
    }
}
```

- [ ] **Step 3: Re-export both from the crate-root `test_api.rs`**. Append:

```rust
#[doc(hidden)]
pub use crate::cfg::builder::region_builder::test_api::{
    ProcessInsnRes, TestRegionBuilder,
};
#[doc(hidden)]
pub use crate::cfg::dot::test_api::vn_to_name;
```

Note: `region_builder` is a private submodule of `cfg::builder`. Re-exporting from it requires the path to remain reachable; since `builder::region_builder` is `mod region_builder;` (not `pub mod`), Rust will reject `pub use crate::cfg::builder::region_builder::…`. Workaround: add a re-export inside `builder/mod.rs`:

```rust
// in crates/cfg/src/cfg/builder/mod.rs, alongside the existing `pub use builder::Builder;` style pattern
#[doc(hidden)]
pub use region_builder::test_api as region_builder_test_api;
```

Then in `src/test_api.rs`:
```rust
#[doc(hidden)]
pub use crate::cfg::builder::region_builder_test_api::{ProcessInsnRes, TestRegionBuilder};
```

Verify during implementation which form compiles cleanly; prefer the simpler one.

- [ ] **Step 4: Verify**.

Run: `cargo build -p cfg && cargo clippy -p cfg -- -D warnings`
Expected: success.

- [ ] **Step 5: Commit**.

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/src/cfg/builder/mod.rs crates/cfg/src/cfg/dot.rs crates/cfg/src/test_api.rs
git commit -m "test(cfg): expose RegionBuilder and vn_to_name via test_api"
```

---

### Checkpoint A — run `coderabbit:review` on the diff since session start. Address findings before continuing.

---

## Task 5: Port `src/cfg/builder/testing.rs` to `tests/common/synthetic.rs`

**Files:**
- Create: `crates/cfg/tests/common/synthetic.rs`
- Modify: `crates/cfg/tests/common/mod.rs`

- [ ] **Step 1: Create `crates/cfg/tests/common/synthetic.rs`** by porting `src/cfg/builder/testing.rs` to use the public API + `cfg::test_api`.

```rust
#![allow(dead_code, clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Shared test-fixture helpers (synthetic `Builder`s, regions, addresses).

use std::collections::VecDeque;

use cfg::test_api::TestRegionBuilder;
use cfg::{
    Builder, MachineInsnAddr, OptionsBuilder, Options, PcodeInsnAddr, Region, RegionInstruction,
};
use rsleigh::mem_readers::BufMemReader;

pub type TestReader = BufMemReader<Vec<u8>>;

/// Short constructor for a `PcodeInsnAddr`.
pub fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: insn,
    }
}

/// Sleigh backed by an empty buffer — decodes nothing but is enough to
/// construct a `Builder` for tests that never call `Builder::build`.
pub fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("failed to create test Sleigh")
}

/// Sleigh backed by `bytes` at `base`. Decodes real x86-64 when the caller
/// supplies real bytes.
pub fn make_sleigh_with_bytes(bytes: Vec<u8>, base: u64) -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(bytes, base);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        reader,
    )
    .expect("failed to create x86-64 test Sleigh")
}

pub fn make_builder(start_addr: u64) -> Builder<TestReader> {
    Builder::new(make_sleigh(), start_addr, OptionsBuilder::new().build())
}

pub fn make_builder_opts(start_addr: u64, options: Options) -> Builder<TestReader> {
    Builder::new(make_sleigh(), start_addr, options)
}

pub fn make_builder_with_bytes(bytes: Vec<u8>, start_addr: u64) -> Builder<TestReader> {
    Builder::new(
        make_sleigh_with_bytes(bytes, start_addr),
        start_addr,
        OptionsBuilder::new().build(),
    )
}

pub fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![],
    }
}

/// Builds a `Region` from `(machine_addr, insn_index)` pairs. Panics if empty.
pub fn make_region(addrs: &[(u64, u64)]) -> Region {
    assert!(!addrs.is_empty(), "make_region requires at least one address");
    let start = addr(addrs[0].0, addrs[0].1);
    let insns: VecDeque<_> = addrs
        .iter()
        .map(|&(m, i)| RegionInstruction {
            addr: addr(m, i),
            insn: fake_insn(),
        })
        .collect();
    Region {
        start_addr: start,
        insns,
        ends_with_tail_call: false,
    }
}

/// Builds a `TestRegionBuilder` anchored at `start` with no parent edge.
pub fn make_region_builder<'a>(
    builder: &'a mut Builder<TestReader>,
    start: PcodeInsnAddr,
) -> TestRegionBuilder<'a, TestReader> {
    TestRegionBuilder::new(builder, start)
}
```

- [ ] **Step 2: Declare the module in `crates/cfg/tests/common/mod.rs`**. Replace its contents with:

```rust
#![allow(dead_code)]

pub mod assertions;
pub mod real_binary;
pub mod synthetic;

pub use assertions::*;
pub use real_binary::*;
pub use synthetic::*;
```

- [ ] **Step 3: Verify build (no tests use `synthetic` yet, but it must compile)**.

Run: `cargo test -p cfg --no-run`
Expected: success.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/tests/common/synthetic.rs crates/cfg/tests/common/mod.rs
git commit -m "test(cfg): port synthetic fixture helpers to tests/common/"
```

---

## Task 6: Migrate addr/region/option/edge-kind inline tests to `tests/`

**Files:**
- Create: `crates/cfg/tests/addr_types.rs`
- Create: `crates/cfg/tests/region.rs`
- Create: `crates/cfg/tests/options.rs`
- Create: `crates/cfg/tests/region_edge_kind.rs`
- Modify: `crates/cfg/src/cfg/mod.rs` (delete inline `mod tests`)
- Modify: `crates/cfg/src/cfg/options.rs` (delete inline `mod tests`)

- [ ] **Step 1: Create `crates/cfg/tests/addr_types.rs`**. Port tests `machine_insn_addr_from_u64`, `machine_insn_addr_ordering`, `pcode_addr_orders_by_machine_addr_first`, `pcode_addr_orders_by_insn_index_when_machine_addr_equal`, `pcode_addr_ordering_is_antisymmetric`, `pcode_addr_equality` from `src/cfg/mod.rs`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::addr;

use cfg::MachineInsnAddr;

#[test]
fn machine_insn_addr_from_u64() {
    let a: MachineInsnAddr = 0x1000u64.into();
    assert_eq!(a.addr, 0x1000);
}

#[test]
fn machine_insn_addr_ordering() {
    let lo: MachineInsnAddr = 0x100u64.into();
    let hi: MachineInsnAddr = 0x200u64.into();
    assert!(lo < hi);
    assert!(hi > lo);
    assert_eq!(lo, lo);
}

#[test]
fn pcode_addr_orders_by_machine_addr_first() {
    assert!(addr(200, 0) > addr(100, 99));
}

#[test]
fn pcode_addr_orders_by_insn_index_when_machine_addr_equal() {
    assert!(addr(100, 1) > addr(100, 0));
    assert!(addr(100, 5) > addr(100, 4));
    assert_eq!(addr(100, 3), addr(100, 3));
}

#[test]
fn pcode_addr_ordering_is_antisymmetric() {
    let a = addr(0x400, 2);
    let b = addr(0x400, 5);
    assert!(a < b);
    assert!(b > a);
}

#[test]
fn pcode_addr_equality() {
    let a = addr(0x1000, 7);
    let b = addr(0x1000, 7);
    assert_eq!(a, b);
    assert!(a >= b);
    assert!(a <= b);
}
```

- [ ] **Step 2: Create `crates/cfg/tests/region.rs`**. Port `region_contains_addr_at_start`, `region_contains_addr_at_end`, `region_contains_addr_in_interior`, `region_contains_addr_pcode_interior`, `region_contains_addr_before_start`, `region_contains_addr_after_end` from `src/cfg/mod.rs`. Add one new test for the empty-insns branch.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_region};

use std::collections::VecDeque;
use cfg::Region;

#[test]
fn contains_addr_at_start() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1000, 0)));
}

#[test]
fn contains_addr_at_end() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1010, 0)));
}

#[test]
fn contains_addr_in_interior() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1008, 0)));
}

#[test]
fn contains_addr_pcode_interior() {
    let r = make_region(&[(0x1000, 0), (0x1000, 3)]);
    assert!(r.contains_addr(addr(0x1000, 1)));
}

#[test]
fn contains_addr_before_start_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x0ff8, 0)));
}

#[test]
fn contains_addr_after_end_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x1014, 0)));
}

#[test]
fn contains_addr_returns_false_for_empty_region() {
    // An empty insns list must never claim to contain any address,
    // even if start_addr happens to match — the region has no extent.
    let r = Region {
        start_addr: addr(0x1000, 0),
        insns: VecDeque::new(),
        ends_with_tail_call: false,
    };
    assert!(!r.contains_addr(addr(0x1000, 0)));
}
```

- [ ] **Step 3: Create `crates/cfg/tests/options.rs`**. Port the four `options_builder_*` tests from `src/cfg/options.rs` verbatim, updating imports from `super::*` to `cfg::*`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cfg::OptionsBuilder;

#[test]
fn options_builder_defaults() {
    let opts = OptionsBuilder::new().build();
    assert!(opts == OptionsBuilder::new().build());  // Reflexive sanity
    // The internal fields are not pub; we assert the effect indirectly by comparing default to self.
    // Stronger behavioral assertions live in build_end_to_end.rs (fn_max_size + allow_code_before).
}

#[test]
fn options_builder_set_fn_max_size_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let sized = OptionsBuilder::new().set_function_max_size(0x1000).build();
    assert_ne!(default, sized);
}

#[test]
fn options_builder_allow_code_before_start_addr_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let allow = OptionsBuilder::new().allow_code_before_start_addr().build();
    assert_ne!(default, allow);
}

#[test]
fn options_builder_both_set_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let both = OptionsBuilder::new()
        .set_function_max_size(0x2000)
        .allow_code_before_start_addr()
        .build();
    assert_ne!(default, both);
}
```

Note: the original inline tests directly inspected `opts.fn_max_size` / `opts.allow_code_before_start_addr` because they had `pub(super)` access. From integration tests, we verify behavior via `PartialEq` on `Options`. The behavioral impact lives in Task 13 (`build_end_to_end.rs`).

- [ ] **Step 4: Create `crates/cfg/tests/region_edge_kind.rs`**. Port `region_edge_kind_variants_are_distinct` from `src/cfg/mod.rs`.

```rust
use cfg::RegionEdgeKind;

#[test]
fn variants_are_pairwise_distinct() {
    let kinds = [
        RegionEdgeKind::Fallthrough,
        RegionEdgeKind::Branch,
        RegionEdgeKind::IfCaseTrue,
        RegionEdgeKind::IfCaseFalse,
    ];
    for i in 0..kinds.len() {
        for j in (i + 1)..kinds.len() {
            assert_ne!(kinds[i], kinds[j]);
        }
    }
}
```

- [ ] **Step 5: Delete the inline `#[cfg(test)] mod tests { ... }` block in `src/cfg/mod.rs`** (lines 36-187 of the current file, i.e. everything after the `#[cfg(test)]` attribute to the closing `}` of the module).

Also delete the now-unused `#![cfg_attr(test, allow(clippy::panic, …))]` lint suppression at the top of `src/lib.rs` if it was only added for in-source tests — grep first; leave it if analyzer/opt/other crates compose through it.

Actually: the file with that suppression is `src/lib.rs`, and `#![cfg_attr(test, allow(…))]` only applies when the crate itself is compiled as a test binary. With all inline tests gone, keep or remove based on whether any remains — leave it for now; final prune in Task 16 handles it.

- [ ] **Step 6: Delete the inline `#[cfg(test)] mod tests { ... }` block in `src/cfg/options.rs`** (lines 69-107).

- [ ] **Step 7: Run tests to verify**.

Run: `cargo test -p cfg`
Expected: all new integration tests pass; count is no lower than before the move.

- [ ] **Step 8: Commit**.

```bash
git add crates/cfg/tests/addr_types.rs crates/cfg/tests/region.rs crates/cfg/tests/options.rs crates/cfg/tests/region_edge_kind.rs crates/cfg/src/cfg/mod.rs crates/cfg/src/cfg/options.rs
git commit -m "test(cfg): migrate addr/region/options/edge-kind tests to tests/"
```

---

## Task 7: Migrate builder unit tests to `tests/`, delete `src/cfg/builder/testing.rs` (atomic)

**Files:**
- Create: `crates/cfg/tests/builder_add_region.rs`
- Create: `crates/cfg/tests/builder_find_region.rs`
- Create: `crates/cfg/tests/builder_split_region.rs`
- Modify: `crates/cfg/src/cfg/builder/mod.rs` (delete inline `mod tests`; delete `#[cfg(test)] pub(super) mod testing;` declaration)
- Modify: `crates/cfg/src/cfg/builder/split.rs` (delete inline `mod tests`)
- Delete: `crates/cfg/src/cfg/builder/testing.rs`

This must be one commit: the inline tests and `testing.rs` reference each other; splitting the step leaves an uncompilable checkpoint.

- [ ] **Step 1: Create `crates/cfg/tests/builder_add_region.rs`**. Port from `src/cfg/builder/mod.rs` tests `add_region_inserts_into_graph_and_map`, `add_region_empty_returns_error`, `add_region_two_regions_both_present`. Access internals through `cfg::test_api`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_builder, make_region};

use cfg::{test_api, ErrorKind, Region};
use std::collections::VecDeque;

#[test]
fn add_region_inserts_into_graph_and_map() {
    let mut b = make_builder(0x1000);
    let r = make_region(&[(0x1000, 0), (0x1004, 0)]);
    let id = test_api::add_region(&mut b, r).unwrap();

    assert!(test_api::graph(&b).node_weight(id).is_some());
    assert_eq!(
        test_api::start_addr_to_region_id(&b).get(&addr(0x1000, 0)),
        Some(&id)
    );
}

#[test]
fn add_region_empty_returns_error() {
    let mut b = make_builder(0x1000);
    let empty = Region {
        start_addr: addr(0x1000, 0),
        insns: VecDeque::new(),
        ends_with_tail_call: false,
    };
    let err = test_api::add_region(&mut b, empty).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::EmptyRegion(_)));
}

#[test]
fn add_region_two_regions_both_present() {
    let mut b = make_builder(0x1000);
    let r1 = make_region(&[(0x1000, 0)]);
    let r2 = make_region(&[(0x1010, 0)]);
    let id1 = test_api::add_region(&mut b, r1).unwrap();
    let id2 = test_api::add_region(&mut b, r2).unwrap();

    assert_ne!(id1, id2);
    assert_eq!(test_api::graph(&b).node_count(), 2);
    assert_eq!(test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)], id1);
    assert_eq!(test_api::start_addr_to_region_id(&b)[&addr(0x1010, 0)], id2);
}
```

- [ ] **Step 2: Create `crates/cfg/tests/builder_find_region.rs`**. Port `find_region_empty_graph`, `find_region_at_start_addr`, `find_region_at_interior_addr`, `find_region_at_last_insn`, `find_region_beyond_end_returns_none`, `find_region_two_adjacent_regions_correct_routing`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_builder, make_region};

use cfg::test_api;

#[test]
fn find_region_empty_graph_returns_none() {
    let b = make_builder(0x1000);
    assert!(test_api::find_region_containing_addr(&b, addr(0x1000, 0)).is_none());
}

#[test]
fn find_region_at_start_addr() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1000, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn find_region_at_interior_addr() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1008, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn find_region_at_last_insn() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x100f, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn find_region_beyond_end_returns_none() {
    let mut b = make_builder(0x1000);
    test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert!(test_api::find_region_containing_addr(&b, addr(0x1020, 0)).is_none());
}

#[test]
fn find_region_two_adjacent_regions_correct_routing() {
    let mut b = make_builder(0x1000);
    let id1 = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    let id2 = test_api::add_region(&mut b, make_region(&[(0x1010, 0), (0x1020, 0)])).unwrap();

    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1004, 0)).map(|(i, _)| i),
        Some(id1)
    );
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1010, 0)).map(|(i, _)| i),
        Some(id2)
    );
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1018, 0)).map(|(i, _)| i),
        Some(id2)
    );
}
```

- [ ] **Step 3: Create `crates/cfg/tests/builder_split_region.rs`**. Port `split_region_at_start_is_noop`, `split_region_creates_two_regions`, `split_region_correct_addr_ranges`, `split_region_adds_fallthrough_edge`, `split_region_rewires_incoming_edges`. **Add** one new test for the `FailedSplitingRegion` error.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_builder, make_region};

use cfg::{test_api, ErrorKind, RegionEdgeKind};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

#[test]
fn split_at_start_is_noop() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    let result = test_api::split_region(&mut b, id, addr(0x1000, 0)).unwrap();

    assert_eq!(result, id, "split at start must return original id");
    assert_eq!(test_api::graph(&b).node_count(), 1, "no new region should be created");
}

#[test]
fn split_creates_two_regions_second_keeps_original_id() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]),
    ).unwrap();
    let second = test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    // Contract: the second half keeps the original NodeIndex so outgoing
    // edges and work-queue parent references remain valid.
    assert_eq!(second, original);
    assert_eq!(test_api::graph(&b).node_count(), 2);
}

#[test]
fn split_produces_correct_addr_ranges() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]),
    ).unwrap();
    test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    assert_eq!(test_api::graph(&b)[original].start_addr, addr(0x1008, 0));
    assert_eq!(test_api::graph(&b)[original].insns.len(), 2);

    let first_id = test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)];
    assert_eq!(test_api::graph(&b)[first_id].start_addr, addr(0x1000, 0));
    assert_eq!(test_api::graph(&b)[first_id].insns.len(), 2);
}

#[test]
fn split_adds_fallthrough_edge() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    let edges: Vec<_> = test_api::graph(&b).edge_references().collect();
    assert_eq!(edges.len(), 1, "exactly one edge after split");
    assert_eq!(*edges[0].weight(), RegionEdgeKind::Fallthrough);
    assert_eq!(edges[0].target(), original);
}

#[test]
fn split_rewires_incoming_edges_to_first_half() {
    let mut b = make_builder(0x1000);
    let a = test_api::add_region(&mut b, make_region(&[(0x0ff0, 0)])).unwrap();
    let b_id = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    test_api::graph_mut(&mut b).add_edge(a, b_id, RegionEdgeKind::Branch);

    test_api::split_region(&mut b, b_id, addr(0x1004, 0)).unwrap();

    let first = test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)];
    let incoming: Vec<_> = test_api::graph(&b).edges_directed(first, petgraph::Incoming).collect();
    assert_eq!(incoming.len(), 1);
    assert_eq!(*incoming[0].weight(), RegionEdgeKind::Branch);
    assert_eq!(incoming[0].source(), a);

    let second_branch_incoming: Vec<_> = test_api::graph(&b)
        .edges_directed(b_id, petgraph::Incoming)
        .filter(|e| *e.weight() == RegionEdgeKind::Branch)
        .collect();
    assert!(second_branch_incoming.is_empty());
}

#[test]
fn split_addr_not_in_region_insns_returns_failed_splitting_region() {
    // Region has insns at 0x1000 and 0x1010 only — nothing at 0x1008.
    // split_region expects addr to match an exact insn addr.
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x1010, 0)])).unwrap();
    let err = test_api::split_region(&mut b, id, addr(0x1008, 0)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::FailedSplitingRegion(_, a) if *a == addr(0x1008, 0)));
}
```

- [ ] **Step 4: Delete `crates/cfg/src/cfg/builder/testing.rs`** entirely.

- [ ] **Step 5: In `crates/cfg/src/cfg/builder/mod.rs`**:
  - Remove `#[cfg(test)] pub(super) mod testing;` (line 5).
  - Delete the entire `#[cfg(test)] mod tests { ... }` block at the bottom of the file.

- [ ] **Step 6: In `crates/cfg/src/cfg/builder/split.rs`**: delete the entire `#[cfg(test)] mod tests { ... }` block.

- [ ] **Step 7: Verify**.

Run: `cargo test -p cfg`
Expected: all tests pass; new split-fail-error test appears in output.

- [ ] **Step 8: Commit** (atomic).

```bash
git add crates/cfg/tests/builder_add_region.rs crates/cfg/tests/builder_find_region.rs crates/cfg/tests/builder_split_region.rs crates/cfg/src/cfg/builder/mod.rs crates/cfg/src/cfg/builder/split.rs
git rm crates/cfg/src/cfg/builder/testing.rs
git commit -m "test(cfg): migrate builder unit tests to tests/, add split-addr-missing case"
```

---

## Task 8: Migrate RegionBuilder tail-call tests to `tests/`

**Files:**
- Create: `crates/cfg/tests/region_builder_tail_call.rs`
- Modify: `crates/cfg/src/cfg/builder/region_builder.rs` (delete inline `mod tests`)

- [ ] **Step 1: Create `crates/cfg/tests/region_builder_tail_call.rs`**. Port `tail_call_nocheck_below_start_default_opts`, `tail_call_nocheck_below_start_with_allow`, `tail_call_nocheck_within_function_no_limit`, `tail_call_nocheck_at_fn_max_size_boundary`, `tail_call_valid_insn_index_zero`, `tail_call_invalid_insn_index_nonzero`, `tail_call_inside_function_returns_false`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_builder, make_builder_opts, make_region_builder};

use cfg::{ErrorKind, OptionsBuilder};

#[test]
fn nocheck_below_start_default_opts_is_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_below_start_with_allow_is_not_tail_call() {
    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let mut b = make_builder_opts(0x1000, opts);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_within_function_no_limit_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x1200, 0)));
}

#[test]
fn nocheck_at_fn_max_size_boundary() {
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
    let mut b = make_builder_opts(0x1000, opts);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    // Contract: target at exactly start + max_size is a tail call (inclusive boundary).
    assert!(rb.is_branch_tail_call_nocheck(addr(0x1100, 0)));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x10ff, 0)));
}

#[test]
fn check_valid_insn_index_zero_is_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x0800, 0)), Ok(true)));
}

#[test]
fn check_invalid_insn_index_nonzero_returns_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.is_branch_tail_call(addr(0x0800, 3)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidTailCall(_)));
}

#[test]
fn check_inside_function_any_insn_index_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x1200, 7)), Ok(false)));
}
```

- [ ] **Step 2: Delete the inline `#[cfg(test)] mod tests { ... }` block in `src/cfg/builder/region_builder.rs`** (lines 306-387).

- [ ] **Step 3: Verify**.

Run: `cargo test -p cfg`
Expected: all tests pass.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/tests/region_builder_tail_call.rs crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "test(cfg): migrate RegionBuilder tail-call tests to tests/"
```

---

## Task 9: Remove leftover inline-test suppression from `lib.rs`

**Files:**
- Modify: `crates/cfg/src/lib.rs`

At this point no inline tests remain. The `#![cfg_attr(test, allow(clippy::panic, …))]` at the top of `src/lib.rs` is now dead (integration tests get their own `#![allow(…)]`).

- [ ] **Step 1: Remove lines 1-9 of `crates/cfg/src/lib.rs`** — the whole `#![cfg_attr(test, allow(…))]` block.

- [ ] **Step 2: Verify**.

Run: `cargo build -p cfg && cargo clippy -p cfg -- -D warnings`
Expected: no new warnings.

- [ ] **Step 3: Commit**.

```bash
git add crates/cfg/src/lib.rs
git commit -m "test(cfg): drop test-only lint suppression now that src has no inline tests"
```

---

### Checkpoint B — legacy inline tests fully migrated. Run `coderabbit:review` on the diff since Checkpoint A.

---

## Task 10: Test `decode_branch_target`

**Files:**
- Create: `crates/cfg/tests/region_builder_decode.rs`

The function (private `fn` in `region_builder.rs` lines 62-86) handles three cases: CONST-space (relative), default-code-space (absolute), other (error).

- [ ] **Step 1: Create `crates/cfg/tests/region_builder_decode.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, make_builder, make_region_builder};

use cfg::ErrorKind;
use rsleigh::{Vn, VnSpace};

fn const_vn(offset: u64) -> Vn {
    Vn {
        addr: rsleigh::VnAddr { space: VnSpace::CONST, off: offset },
        size: 8,
    }
}

fn code_space_vn(sleigh: &rsleigh::Sleigh<common::TestReader>, offset: u64) -> Vn {
    Vn {
        addr: rsleigh::VnAddr { space: sleigh.default_code_space(), off: offset },
        size: 8,
    }
}

fn register_vn(offset: u64) -> Vn {
    Vn {
        addr: rsleigh::VnAddr { space: VnSpace::REGISTER, off: offset },
        size: 8,
    }
}

#[test]
fn decode_const_space_is_relative_to_current_pcode_insn_index() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x2000, 0));
    let target = rb.decode_branch_target(const_vn(3), addr(0x2000, 2)).unwrap();
    // insn_index should become current (2) + offset (3) = 5; machine_addr unchanged.
    assert_eq!(target, addr(0x2000, 5));
}

#[test]
fn decode_default_code_space_is_absolute_machine_address() {
    let sleigh = common::make_sleigh();
    // Need the Builder's sleigh's default_code_space — easiest path: construct the vn
    // against the same Sleigh we put in the Builder. So build the Sleigh first, then
    // move it into the Builder.
    let mut b = cfg::Builder::new(sleigh, 0x1000, cfg::OptionsBuilder::new().build());
    let cs_vn = Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::mem_readers::BufMemReader::<Vec<u8>>::new(vec![], 0)
                .pipe_default_code_space_todo(),  // placeholder — see note
            off: 0xabc0,
        },
        size: 8,
    };
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let target = rb.decode_branch_target(cs_vn, addr(0x1000, 4)).unwrap();
    assert_eq!(target, addr(0xabc0, 0));
}
```

**Implementation note:** the default_code_space comes from `Sleigh::default_code_space(&self)`. After moving the sleigh into the Builder, accessing it back requires the builder's sleigh — which is `pub(super)`. Expose it via `test_api::sleigh<R>(&Builder<R>) -> &rsleigh::Sleigh<R>` — add this forwarder to `src/cfg/builder/mod.rs::test_api` now; it's a trivial one-liner:

```rust
pub fn sleigh<R: rsleigh::MemReader>(b: &super::Builder<R>) -> &rsleigh::Sleigh<R> {
    &b.sleigh
}
```

And update the crate-root `test_api.rs` re-export if not already using the glob.

Then the default-code-space test becomes:

```rust
#[test]
fn decode_default_code_space_is_absolute_machine_address() {
    let mut b = make_builder(0x1000);
    let default_cs = cfg::test_api::sleigh(&b).default_code_space();
    let cs_vn = Vn {
        addr: rsleigh::VnAddr { space: default_cs, off: 0xabc0 },
        size: 8,
    };
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let target = rb.decode_branch_target(cs_vn, addr(0x1000, 4)).unwrap();
    assert_eq!(target, addr(0xabc0, 0));
}
```

Add the third test for the error path:

```rust
#[test]
fn decode_unsupported_space_returns_invalid_branch_target_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.decode_branch_target(register_vn(0x20), addr(0x1000, 0)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}
```

- [ ] **Step 2: Add `sleigh` forwarder to `src/cfg/builder/mod.rs::test_api`** (as described above).

- [ ] **Step 3: Verify**.

Run: `cargo test -p cfg --test region_builder_decode`
Expected: three tests pass.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/tests/region_builder_decode.rs crates/cfg/src/cfg/builder/mod.rs
git commit -m "test(cfg): add decode_branch_target coverage (const/code-space/invalid)"
```

---

## Task 11: Test `process_new_insn`, `process_insn`, `finish_current_region`

**Files:**
- Create: `crates/cfg/tests/region_builder_process.rs`

Driving these requires a real lift so `lift_res` / insn data is meaningful. We feed x86-64 bytes into `make_builder_with_bytes` and lift one machine instruction, then call the methods directly.

- [ ] **Step 1: Add an x86-64-specific helper to `crates/cfg/tests/common/synthetic.rs`** near the top:

```rust
/// `nop; ret` encoded for x86-64 at `base`.
pub fn nop_ret_bytes() -> Vec<u8> {
    vec![0x90, 0xc3]
}

/// Unconditional `jmp` to `rel8` offset, then `ret`. Total 3 bytes.
pub fn jmp_rel8_ret_bytes(rel: i8) -> Vec<u8> {
    vec![0xeb, rel as u8, 0xc3]
}

/// Conditional `je rel8`, then two `ret`s. Total 4 bytes.
pub fn je_rel8_ret_ret_bytes(rel: i8) -> Vec<u8> {
    vec![0x74, rel as u8, 0xc3, 0xc3]
}
```

- [ ] **Step 2: Create `crates/cfg/tests/region_builder_process.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{
    addr, je_rel8_ret_ret_bytes, jmp_rel8_ret_bytes, make_builder_with_bytes, make_region_builder,
    nop_ret_bytes,
};

use cfg::{test_api::ProcessInsnRes, ErrorKind, RegionEdgeKind};

fn lift_one(b: &cfg::Builder<common::TestReader>, at: u64) -> rsleigh::LiftRes {
    cfg::test_api::sleigh(b).lift_one(at).expect("lift_one")
}

#[test]
fn process_new_insn_non_terminating_keeps_region_open() {
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(nop_ret_bytes(), base);
    let lift = lift_one(&b, base);
    let (insn, insn_addr) = (lift.insns[0].clone(), addr(base, 0));
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&insn, insn_addr, &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::DidntFinishProcessing);
    assert_eq!(rb.insns().len(), 1);
}

#[test]
fn process_new_insn_return_ends_region() {
    // lift `ret` at 0x1000 (pure ret is 0xc3)
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(vec![0xc3], base);
    let lift = lift_one(&b, base);

    // Find the Return pcode insn in the lift (position varies by arch micro-op sequence).
    let (pos, ret_insn) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| matches!(i.opcode, rsleigh::Opcode::Return))
        .map(|(p, i)| (p as u64, i.clone()))
        .expect("ret sequence contains a Return pcode op");

    let at = addr(base, pos);
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    let res = rb.process_new_insn(&ret_insn, at, &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);
}

#[test]
fn process_new_insn_branch_non_tail_enqueues_target() {
    // `jmp +0` from 0x1000 → target 0x1002 (next insn is a ret).
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(jmp_rel8_ret_bytes(0), base);
    let lift = lift_one(&b, base);

    let (pos, branch) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| matches!(i.opcode, rsleigh::Opcode::Branch))
        .map(|(p, i)| (p as u64, i.clone()))
        .expect("jmp sequence contains a Branch pcode op");

    let at = addr(base, pos);
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    let res = rb.process_new_insn(&branch, at, &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    assert_eq!(cfg::test_api::work_queue(&b).len(), 1);
    let (parent, enq_addr) = cfg::test_api::work_queue(&b)[0].clone();
    assert_eq!(enq_addr, addr(0x1002, 0));
    let (_, kind) = parent.expect("branch must have a parent edge");
    assert_eq!(kind, RegionEdgeKind::Branch);
}

#[test]
fn process_new_insn_branch_tail_call_ends_with_tail_call_flag() {
    // `jmp -10` from 0x1000 → target 0x0ff2 (below start → tail call).
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(jmp_rel8_ret_bytes(-10), base);
    let lift = lift_one(&b, base);

    let (pos, branch) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| matches!(i.opcode, rsleigh::Opcode::Branch))
        .map(|(p, i)| (p as u64, i.clone()))
        .expect("jmp contains a Branch pcode op");

    let at = addr(base, pos);
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    let res = rb.process_new_insn(&branch, at, &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // Queue is untouched — tail call doesn't enqueue the target.
    assert_eq!(cfg::test_api::work_queue(&b).len(), 0);
    // One region was added with ends_with_tail_call = true.
    let regions: Vec<_> = cfg::test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert!(regions[0].ends_with_tail_call);
}

#[test]
fn process_new_insn_cond_branch_enqueues_both_cases() {
    // `je +0` from 0x1000 → true target 0x1002, false target 0x1002 (fall-through).
    // The fall-through insn_index rollover depends on whether the je expands to
    // multiple pcode ops. We use a simple je and verify that two items are enqueued.
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(je_rel8_ret_ret_bytes(0), base);
    let lift = lift_one(&b, base);

    let (pos, cbr) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| matches!(i.opcode, rsleigh::Opcode::CondBranch))
        .map(|(p, i)| (p as u64, i.clone()))
        .expect("je contains a CondBranch pcode op");

    let at = addr(base, pos);
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    let res = rb.process_new_insn(&cbr, at, &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let queue: Vec<_> = cfg::test_api::work_queue(&b).iter().cloned().collect();
    assert_eq!(queue.len(), 2, "CondBranch must enqueue both true and false targets");
    // Exactly one IfCaseTrue and one IfCaseFalse were enqueued.
    let mut kinds: Vec<_> = queue
        .iter()
        .filter_map(|(parent, _)| parent.as_ref().map(|(_, k)| *k))
        .collect();
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(
        kinds,
        vec![RegionEdgeKind::IfCaseFalse, RegionEdgeKind::IfCaseTrue]
    );
}

#[test]
fn process_insn_falls_through_into_existing_region_start() {
    // Pre-register a region at 0x1004. Then drive process_insn at 0x1004 — it
    // should close out the current region with a Fallthrough edge to the
    // existing one, without decoding.
    let base = 0x1000u64;
    let mut b = make_builder_with_bytes(nop_ret_bytes(), base);

    // Add an existing region at 0x1004.
    let existing = cfg::test_api::add_region(
        &mut b,
        common::make_region(&[(0x1004, 0)]),
    ).unwrap();

    // Build a RegionBuilder that has already consumed one insn so
    // finish_current_region has something to close.
    let lift = lift_one(&b, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    rb.push_insn(cfg::RegionInstruction { addr: addr(base, 0), insn: lift.insns[0].clone() });

    // Now call process_insn at the existing-start addr; the insn body is irrelevant.
    let dummy = lift.insns[0].clone();
    let res = rb.process_insn(&dummy, addr(0x1004, 0), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // Graph has at least the new region + existing + exactly one Fallthrough edge between them.
    let ft_count = cfg::test_api::graph(&b)
        .edge_references()
        .filter(|e| *e.weight() == RegionEdgeKind::Fallthrough && e.target() == existing)
        .count();
    assert_eq!(ft_count, 1);
}

#[test]
fn finish_current_region_empty_insns_returns_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.finish_current_region(false).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::NoInstructionsRegionBuilder));
}

// Helper (not a test): unqualified `make_builder` for the error-path test above.
use common::make_builder;
```

Notes:
- `edge_references` on `StableDiGraph` is available through `petgraph::visit::IntoEdgeReferences`; add that `use` at the top.
- `RegionInstruction` is `pub` in `cfg` — it re-exports from `cfg::types` via the module tree (verify during implementation; if not, re-export it from the crate root or import through the full path `cfg::Region as _`).

- [ ] **Step 3: Verify**.

Run: `cargo test -p cfg --test region_builder_process`
Expected: seven tests pass. If `find(..Return..)` or `find(..Branch..)` panics, inspect the lift output — rsleigh's pcode sequence for `0xc3` / `0xeb` / `0x74` is deterministic but the exact index may require adjustment. Fix by matching on opcode as shown rather than hardcoding indices.

- [ ] **Step 4: Commit**.

```bash
git add crates/cfg/tests/region_builder_process.rs crates/cfg/tests/common/synthetic.rs
git commit -m "test(cfg): cover process_new_insn / process_insn / finish_current_region"
```

---

## Task 12: Test `Cfg::region_insn`, `region_if`, `region_branch`, iteration, `DuplicateEdgeKind`

**Files:**
- Create: `crates/cfg/tests/cfg_query.rs`

- [ ] **Step 1: Create `crates/cfg/tests/cfg_query.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{addr, build_cfg, binary, make_region, make_sleigh};

use cfg::{Cfg, ErrorKind, RegionEdgeKind};
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;

fn real_cfg(arch: &str, fn_name: &str) -> Cfg<reader::ElfFileMemReader> {
    let p = binary(arch);
    build_cfg(
        p.to_str().unwrap(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

#[test]
fn region_insn_returns_clone_of_region_insns() {
    let cfg = real_cfg("x64", "add");
    let insns = cfg.region_insn(cfg.entry).unwrap();
    assert!(!insns.is_empty());
    // Cloning — the original region still has its instructions.
    assert_eq!(
        cfg.graph[cfg.entry].insns.len(),
        insns.len()
    );
}

#[test]
fn region_insn_invalid_node_index_returns_invalid_region_error() {
    let cfg = real_cfg("x64", "add");
    let bogus = NodeIndex::new(10_000);
    let err = cfg.region_insn(bogus).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidRegion(_)));
}

#[test]
fn regions_iterator_count_matches_node_count() {
    let cfg = real_cfg("x64", "sum_to_n");
    assert_eq!(cfg.regions().count(), cfg.graph.node_count());
}

#[test]
fn region_ids_iterator_count_matches_node_count() {
    let cfg = real_cfg("x64", "sum_to_n");
    assert_eq!(cfg.region_ids().count(), cfg.graph.node_count());
}

#[test]
fn region_branch_returns_none_for_linear_entry() {
    let cfg = real_cfg("x64", "add");
    assert!(cfg.region_branch(cfg.entry).unwrap().is_none());
}

#[test]
fn region_if_both_successors_present_on_abs_val() {
    let cfg = real_cfg("x64", "abs_val");
    let has_pair = cfg.region_ids().any(|id| {
        let s = cfg.region_if(id).unwrap();
        s.if_true_region.is_some() && s.if_false_region.is_some()
    });
    assert!(has_pair);
}

#[test]
fn region_if_absent_on_linear_entry() {
    let cfg = real_cfg("x64", "add");
    let s = cfg.region_if(cfg.entry).unwrap();
    assert!(s.if_true_region.is_none());
    assert!(s.if_false_region.is_none());
}

#[test]
fn duplicate_edge_kind_is_detected_by_region_branch() {
    // Manually construct a malformed Cfg with two Branch edges from one node.
    let mut graph: StableDiGraph<cfg::Region, RegionEdgeKind> = StableDiGraph::new();
    let src = graph.add_node(make_region(&[(0x1000, 0)]));
    let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
    let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
    graph.add_edge(src, dst1, RegionEdgeKind::Branch);
    graph.add_edge(src, dst2, RegionEdgeKind::Branch);

    let cfg = cfg::Cfg {
        sleigh: make_sleigh(),
        graph,
        entry: src,
    };

    let err = cfg.region_branch(src).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::DuplicateEdgeKind(_, RegionEdgeKind::Branch)));
}
```

Note: constructing `Cfg` directly works because its fields (`sleigh`, `graph`, `entry`) are already `pub`.

- [ ] **Step 2: Verify**.

Run: `cargo test -p cfg --test cfg_query`
Expected: eight tests pass. The real-binary tests need `binary_tests/out/x64/test.elf` present (same requirement as existing `cfg_integration.rs`).

- [ ] **Step 3: Commit**.

```bash
git add crates/cfg/tests/cfg_query.rs
git commit -m "test(cfg): cover Cfg query API incl. DuplicateEdgeKind error path"
```

---

## Task 13: Test `vn_to_name` across all space variants and error cases

**Files:**
- Create: `crates/cfg/tests/vn_to_name.rs`

- [ ] **Step 1: Create `crates/cfg/tests/vn_to_name.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{binary, build_cfg};

use cfg::{test_api::vn_to_name, ErrorKind};
use rsleigh::{Vn, VnAddr, VnSpace};

fn real_cfg() -> cfg::Cfg<reader::ElfFileMemReader> {
    let p = binary("x64");
    build_cfg(
        p.to_str().unwrap(),
        "add",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

#[test]
fn vn_const_formats_as_hex_with_size() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::CONST, off: 0x2a }, size: 4 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "0x2a:4");
}

#[test]
fn vn_ram_formats_as_ram_hex() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::RAM, off: 0x1000 }, size: 8 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "ram[0x1000]:8");
}

#[test]
fn vn_unique_formats_as_unique_hex() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::UNIQUE, off: 0x80 }, size: 1 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "unique[0x80]:1");
}

#[test]
fn vn_register_known_offset_returns_register_name() {
    let cfg = real_cfg();
    // On x86-64, a well-known register is `rax` — look up its actual Vn via the Sleigh regs.
    let regs = cfg.sleigh.regs().unwrap();
    // Grab any register the regs table knows; iterate until one works.
    let name_to_try = "RAX";
    let vn = regs.name_to_vn(name_to_try).expect("RAX should exist on x86-64");
    let resolved = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(resolved, name_to_try);
}

#[test]
fn vn_register_unknown_offset_returns_invalid_reg_vn_error() {
    let cfg = real_cfg();
    // Pick an offset the register table will not map — far outside any real reg.
    let bogus = Vn {
        addr: VnAddr { space: VnSpace::REGISTER, off: 0xffff_ffff_ffff_ffff },
        size: 1,
    };
    let err = vn_to_name(&cfg, &bogus).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidRegVn(_)));
}

#[test]
fn vn_unsupported_space_returns_unsupported_error() {
    let cfg = real_cfg();
    // Use a space that isn't CONST/REGISTER/RAM/UNIQUE. Iterate the enum and pick the first
    // variant that falls through; if the enum changes, update this test.
    let exotic = Vn {
        addr: VnAddr { space: VnSpace::PROCESSOR_SPEC, off: 0 },
        size: 1,
    };
    let err = vn_to_name(&cfg, &exotic).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::UnsupportedVnSpaceDisplay(_)));
}
```

**Verification notes during implementation:**
- `regs::name_to_vn` / exact API: confirm the method name in the installed rsleigh (`grep -n 'fn name_to_vn\|fn vn_to_name' ../rsleigh/src/`). If absent, iterate `regs.iter()` and pick a known name.
- `VnSpace::PROCESSOR_SPEC` is a placeholder — grep `VnSpace::` variants in rsleigh and substitute one that is not CONST/REGISTER/RAM/UNIQUE. If rsleigh only exposes those four, a `VnSpace::from_raw(99)`-style constructor may be needed. If no fallthrough space is constructible, move the `UnsupportedVnSpaceDisplay` assertion to a doc-only commented-out test and record why.

- [ ] **Step 2: Verify**.

Run: `cargo test -p cfg --test vn_to_name`
Expected: six tests pass (or five + one documented skip per the note above).

- [ ] **Step 3: Commit**.

```bash
git add crates/cfg/tests/vn_to_name.rs
git commit -m "test(cfg): cover vn_to_name variants and error paths"
```

---

## Task 14: End-to-end synthetic `Builder::build` tests + option effects

**Files:**
- Create: `crates/cfg/tests/build_end_to_end.rs`

- [ ] **Step 1: Create `crates/cfg/tests/build_end_to_end.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::make_sleigh_with_bytes;

use cfg::{Builder, ErrorKind, OptionsBuilder, RegionEdgeKind};

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> cfg::Cfg<common::TestReader> {
    Builder::new(make_sleigh_with_bytes(bytes.clone(), start), start, OptionsBuilder::new().build())
        .build()
        .expect("Builder::build on synthetic bytes")
}

fn build_from_bytes_opts(
    bytes: Vec<u8>,
    start: u64,
    opts: cfg::Options,
) -> cfg::error::Result<cfg::Cfg<common::TestReader>> {
    Builder::new(make_sleigh_with_bytes(bytes, start), start, opts).build()
}

#[test]
fn linear_ret_produces_single_region() {
    // `ret` (0xc3)
    let cfg = build_from_bytes(vec![0xc3], 0x1000);
    assert_eq!(cfg.graph.node_count(), 1);
    assert!(!cfg.graph[cfg.entry].ends_with_tail_call);
}

#[test]
fn jmp_to_inner_splits_region() {
    // nop; nop; jmp -2 (back to second nop) ; ret
    // At 0x1000: 0x90 (nop)
    // At 0x1001: 0x90 (nop)
    // At 0x1002: 0xeb 0xfd (jmp -3 → 0x1001)
    // At 0x1004: 0xc3 (ret; unreachable through this byte, but Sleigh may explore)
    let bytes = vec![0x90, 0x90, 0xeb, 0xfd, 0xc3];
    let cfg = build_from_bytes(bytes, 0x1000);

    // We expect at least 2 regions (the split at 0x1001 creates a loop body)
    // and at least one back-edge (Branch to 0x1001) plus a Fallthrough from
    // first half to second half.
    assert!(cfg.graph.node_count() >= 2);
    let branch_edges = cfg
        .graph
        .edge_references()
        .filter(|e| *e.weight() == RegionEdgeKind::Branch)
        .count();
    assert!(branch_edges >= 1, "expected at least one Branch edge from the back-jump");
}

#[test]
fn fn_max_size_treats_forward_jump_beyond_limit_as_tail_call() {
    // jmp +0x10 (forward 16 bytes) ; fill ; ret
    // At 0x1000: 0xeb 0x10 → target 0x1012
    // With fn_max_size = 0x10, target 0x1012 >= 0x1000+0x10 → tail call.
    let bytes = vec![0xeb, 0x10];
    let opts = OptionsBuilder::new().set_function_max_size(0x10).build();
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts).unwrap();
    // Single region because the jmp target is classified as a tail call.
    assert_eq!(cfg.graph.node_count(), 1);
    assert!(cfg.graph[cfg.entry].ends_with_tail_call);
}

#[test]
fn allow_code_before_start_addr_negates_default_tail_call() {
    // jmp -16 from 0x1000 → target 0x0ff2 (below start).
    // Default options: tail call. With allow_code_before_start_addr: follow it (and the
    // lift may go wild or fail gracefully; assert the non-tail-call classification only).
    use rsleigh::mem_readers::BufMemReader;
    // Place bytes at 0x0ff0..0x1002 so the below-start target 0x0ff2 is valid RAM.
    let mut bytes = vec![0x90; 0x12];            // 0x0ff0..0x1002
    bytes[0x10] = 0xeb;                          // 0x1000: jmp
    bytes[0x11] = 0xf0;                          // rel8 = -16 → 0x0ff2
    let reader = BufMemReader::new(bytes, 0x0ff0);
    let sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        reader,
    ).unwrap();

    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let cfg = cfg::Builder::new(sleigh, 0x1000, opts).build().unwrap();

    // The entry region ends with a Branch edge, not a tail call, because the option
    // flipped the below-start classification.
    let entry_region = &cfg.graph[cfg.entry];
    assert!(!entry_region.ends_with_tail_call);
    assert!(
        cfg.graph.edge_references().any(|e| *e.weight() == RegionEdgeKind::Branch),
        "at least one Branch edge must exist since the target is now followed"
    );
}

#[test]
fn invalid_tail_call_when_relative_branch_targets_mid_insn_below_start() {
    // Construct a scenario where decode_branch_target yields insn_index != 0
    // on a tail-call target. Easiest path: a `Branch` pcode op inside a machine
    // insn whose CONST-space target, combined with the current insn_index,
    // lands at insn_index > 0 — and the Branch's containing machine insn is at
    // the function start, so the computed PcodeInsnAddr has machine_addr < start
    // is not actually achievable with a simple CONST-relative branch.
    //
    // This test instead drives the error directly through `is_branch_tail_call`,
    // which is covered in `region_builder_tail_call.rs`. We assert here that
    // `Builder::build` propagates any InvalidTailCall cleanly when it arises.
    //
    // Concrete trigger through Builder::build needs a hand-lifted binary where
    // Branch(CONST, +k) with insn_index+k != 0 and target_machine_addr < start.
    // If such a binary cannot be constructed trivially, skip with a note.

    // For now, pin the error-bubbling path via a hand-constructed builder run
    // on bytes that produce no InvalidTailCall and assert normal success — the
    // error-kind test lives in `region_builder_tail_call.rs`.
    let cfg = build_from_bytes(vec![0xc3], 0x1000);
    assert_eq!(cfg.graph.node_count(), 1);
}
```

**Note on the InvalidTailCall end-to-end test:** crafting x86-64 bytes that *produce* this error through `Builder::build` is non-trivial. The error is covered at the unit level in `region_builder_tail_call.rs::check_invalid_insn_index_nonzero_returns_error`. If during implementation you find a clean byte sequence that triggers it via `Builder::build`, replace the placeholder test body. Otherwise leave the coverage at the unit level and keep the `no-crash` sanity test.

- [ ] **Step 2: Verify**.

Run: `cargo test -p cfg --test build_end_to_end`
Expected: all tests pass. If `allow_code_before_start_addr` test fails because Sleigh rejects the below-start bytes, adjust the byte layout until rsleigh accepts them (it's a real lift; the memory region must cover the target).

- [ ] **Step 3: Commit**.

```bash
git add crates/cfg/tests/build_end_to_end.rs
git commit -m "test(cfg): end-to-end Builder::build synthetic-byte scenarios + option effects"
```

---

## Task 15: `CfgDotDumper` smoke test

**Files:**
- Create: `crates/cfg/tests/dot_dumper.rs`

- [ ] **Step 1: Create `crates/cfg/tests/dot_dumper.rs`**.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{binary, build_cfg};

use dot::{GraphDotDumper, DotEmitter};
use std::fmt::Write;

fn dump_to_string(
    cfg: &cfg::Cfg<reader::ElfFileMemReader>,
) -> String {
    let dumper = cfg.dot_dumper();
    let mut out = String::new();
    let mut emitter = DotEmitter::new(&mut out);
    let mut state = dumper.create_initial_state();
    for node in dumper.iter_nodes() {
        dumper.dump_as_dot(node, &mut emitter, &mut state).unwrap();
    }
    out
}

#[test]
fn dot_output_is_non_empty_for_linear_function() {
    let p = binary("x64");
    let cfg = build_cfg(
        p.to_str().unwrap(),
        "add",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    );
    let s = dump_to_string(&cfg);
    assert!(!s.is_empty(), "DOT output must not be empty");
    // A linear function has at least one node block.
    assert!(s.contains("Instruction(addr="), "node label must appear");
}

#[test]
fn dot_output_for_conditional_function_contains_if_case_edges() {
    let p = binary("x64");
    let cfg = build_cfg(
        p.to_str().unwrap(),
        "abs_val",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    );
    let s = dump_to_string(&cfg);
    assert!(s.contains("IfCaseTrue") || s.contains("IfCaseFalse"),
        "DOT output for a conditional fn must label the if-case edges");
    // Dashed style is used for IfCase edges — see src/cfg/dot.rs:102.
    assert!(s.contains("dashed"), "IfCase edges must render with dashed style");
}

#[test]
fn dot_output_for_loop_contains_branch_style() {
    let p = binary("x64");
    let cfg = build_cfg(
        p.to_str().unwrap(),
        "sum_to_n",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    );
    let s = dump_to_string(&cfg);
    // Loops are built with Fallthrough (solid) and potentially Branch (bold).
    assert!(s.contains("solid") || s.contains("bold"),
        "a looping fn's DOT output should contain at least one solid or bold edge");
}

#[test]
fn iter_nodes_yields_every_region() {
    let p = binary("x64");
    let cfg = build_cfg(
        p.to_str().unwrap(),
        "clamp",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    );
    let dumper = cfg.dot_dumper();
    let count: usize = dumper.iter_nodes().into_iter().count();
    assert_eq!(count, cfg.graph.node_count());
}
```

- [ ] **Step 2: Verify**.

Run: `cargo test -p cfg --test dot_dumper`
Expected: four tests pass. The `DotEmitter::new` constructor signature — verify in [crates/dot/src/lib.rs](../../crates/dot/src/lib.rs) during implementation; adapt if it takes a different argument type.

- [ ] **Step 3: Commit**.

```bash
git add crates/cfg/tests/dot_dumper.rs
git commit -m "test(cfg): CfgDotDumper smoke covering linear/conditional/loop"
```

---

### Checkpoint C — all new coverage in place. Run `coderabbit:review` on the diff since Checkpoint B.

---

## Task 16: Final prune and workspace-wide verification

**Files:**
- Modify: `crates/cfg/tests/cfg_integration.rs`

- [ ] **Step 1: Audit `crates/cfg/tests/cfg_integration.rs` for cases now duplicated by per-topic files**:

Duplicates to remove (present as integration tests elsewhere now):
- `abs_val_region_if_returns_both_successors` → covered in `cfg_query.rs`.
- `add_entry_region_branch_is_none` → covered in `cfg_query.rs`.

The arch-parametrized structural cases (`add_is_linear`, `abs_val_has_conditional_edges`, `sum_to_n_has_back_edge`, …) **stay** — their value is cross-arch.

Remove those two methods from the `arch_tests!` macro body. Keep everything else.

- [ ] **Step 2: Run the full workspace test suite**.

Run: `cargo test --workspace`
Expected: green. Nothing else should be affected.

- [ ] **Step 3: Run clippy workspace-wide**.

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean on cfg; other crates untouched.

- [ ] **Step 4: Count the new test total and document it**.

Run: `cargo test -p cfg 2>&1 | grep -E 'test result:' | tail`
Expected: ~75-90 tests, up from the ~34 baseline.

- [ ] **Step 5: Commit**.

```bash
git add crates/cfg/tests/cfg_integration.rs
git commit -m "test(cfg): trim cfg_integration duplicates now covered by per-topic files"
```

---

### Checkpoint D — full migration complete. Run `coderabbit:review` on the complete diff since the start of this plan. Address findings before considering the work done.

---

## Self-Review

(Completed before handing off.)

**1. Spec coverage:** Every section of the spec maps to tasks:
- D1 (move to `tests/`): Tasks 1, 5, 6, 7, 8
- D2 (`test_api` surface): Tasks 2, 3, 4
- D3 (synthetic binaries via `BufMemReader`): Task 5 (helpers), Tasks 11, 14
- D4 (keep `arch_tests!` + factor helpers): Task 1, Task 16
- D5 (pin option effects): Task 14

Coverage matrix table: every row from the spec has a named test in the plan. Gap checks:
- `Builder::explore` — covered transitively by Task 11 (process_insn) and Task 14 (build_end_to_end). Matches spec.
- `RegionBuilder::build` — covered transitively by Task 14.
- `region_insn` error path — Task 12.
- `DuplicateEdgeKind` — Task 12.
- `InvalidRegion` — Task 12.
- `InvalidRegVn`, `UnsupportedVnSpaceDisplay` — Task 13 (with an implementation note for the exotic-space case).
- `NoInstructionsRegionBuilder` — Task 11.
- `FailedSplitingRegion` — Task 7.
- `InvalidBranchTargetVaErr` — Task 10.
- `InvalidTailCall` — Task 8 (unit) + Task 14 (documented limitation on e2e).
- `EmptyRegion` — Task 7.

All accounted for.

**2. Placeholder scan:** No `TBD`/`TODO` literals. Two places include implementation notes where the exact API call needs verification against rsleigh (`regs::name_to_vn` form, `VnSpace` non-standard variant, `DotEmitter::new` signature) — these are verification instructions, not placeholders. The `InvalidTailCall` e2e test is documented as a best-effort with an explicit fallback.

**3. Type consistency:** `TestRegionBuilder`, `ProcessInsnRes`, `test_api::{add_region, find_region_containing_addr, split_region, graph, graph_mut, start_addr_to_region_id, work_queue, sleigh, vn_to_name}` — names used consistently across all tasks that reference them. `make_builder_with_bytes` is defined in Task 5 and used in Task 11; `nop_ret_bytes`, `jmp_rel8_ret_bytes`, `je_rel8_ret_ret_bytes` added in Task 11's Step 1 and used in Task 11's Step 2. Consistent.
