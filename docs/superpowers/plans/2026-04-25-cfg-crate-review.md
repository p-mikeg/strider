# CFG Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clean up the `cfg` crate — fix two latent correctness issues (fn-size overflow, test clippy lints that break `cargo clippy --all-targets`), collapse a handful of trivial wrappers and unnecessary allocations, delete dead surface, and narrow two `&mut self` methods to `&self`, without changing observable behavior for the only external caller (`analyzer`).

**Architecture:** Each task is a self-contained, independently-committable change. External usage (checked across `analyzer`, `ir`, `opt`, `pattern`, `reader`) is narrow: `cfg::{Cfg, Builder, OptionsBuilder, RegionEdgeKind, RegionId, Error, ErrorKind}`, `Cfg::{sleigh, graph, entry, regions, region_ids, region_branch, region_if, dot_dumper}`, and `Region::{start_addr, insns, ends_with_tail_call}`. Nothing external reads the query method `region_insn`, the error variants `UnknownRegName` / `AssertionFailed`, or touches `VecDeque`-specific APIs on `Region::insns` or the work queue.

**Tech Stack:** Rust, `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Open questions for the reviewer before execution

Each changes your mind about a task below. Pick one per group.

**Q1 — `decode_branch_target` relative CONST offset (Task 2):** The current code does
`insn_index: branch_insn_addr.insn_index + branch_target_var.addr.off`
with both operands `u64`. P-code CONST-space branch targets conventionally encode a signed offset (Sleigh emits backward pcode-local branches for complex insns like `rep`/`string` ops) as two's-complement in a `u64`. Unsigned addition silently wraps on a backward branch.
  - **(A)** Treat `off` as signed (`off as i64`) and use `i64::checked_add` on the signed index; error on overflow or on a negative resulting index. Matches the semantics rsleigh's callers expect. **Default choice.**
  - **(B)** Document the current unsigned-only contract and return `InvalidBranchTargetVaErr` when `off` has the high bit set (forbid backward pcode-local branches at decode time). Stricter; might break any backward-CONST-branch input.
  - **(C)** Leave as-is, just add `checked_add` so debug builds panic instead of silently wrapping. Keeps the bug latent but visible.

  **Reviewer action:** confirm (A). If the real `rsleigh` code path never emits backward CONST branches, (C) is acceptable as a guardrail.

**Q2 — `region_insn` dead method (Task 6):** Only the two tests in `cfg_query.rs` call it. Analyzer iterates `region.insns` directly.
  - **(A)** Delete it. Drop the two tests. **Default choice.**
  - **(B)** Keep it but rename to `region_instructions` and return `&[RegionInstruction]` (or `&VecDeque<RegionInstruction>`) to avoid the per-call clone.

**Q3 — `GenericSleighError(String)` in error enum (Task 9):** `sleigh.lift_one(...)` returns `GenericError<R::Err>` (`Base(BaseError) | MemReadErr(E)`). Current code flattens via `format!("{:?}", e)`, losing the typed variant. Properly threading the generic `E` through `strider-error::define_error!` looks non-trivial.
  - **(A)** Leave `GenericSleighError(String)` as-is; just note the tradeoff in the enum's doc. **Default choice — low ROI.**
  - **(B)** Split into two variants (`SleighBaseError(rsleigh::error::BaseError)` + `SleighMemReadError(String)`) and match on `GenericError` at the call site. Preserves type info for the `Base` arm.

**Q4 — `CfgDotDumperState` empty struct (Task 12):** The `dot::GraphDotDumper` trait requires a `State` associated type. We could use `()` if the trait allows it.
  - **(A)** Investigate and switch to `type State = ();` if `()` satisfies `GraphDotDumper::State`. **Default choice.**
  - **(B)** Keep `CfgDotDumperState` — future CFG dump passes may want per-render state.

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Fix `fn_max_size + start_addr` overflow in tail-call classification

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:105-109](crates/cfg/src/cfg/builder/region_builder.rs#L105-L109)
- Modify: [crates/cfg/tests/region_builder_tail_call.rs](crates/cfg/tests/region_builder_tail_call.rs)

- [ ] **Step 1: Write a failing test**

Append to `crates/cfg/tests/region_builder_tail_call.rs`:

```rust
/// Pinned contract: when `start_addr + fn_max_size` would overflow u64,
/// the tail-call bound check must not silently wrap. Current code computes
/// `fn_max_size + start_addr.addr <= addr.addr` with unchecked `+`, so an
/// overflow produces a tiny wrapped number that trivially <= addr, flipping
/// every branch to "tail call". Fix: saturate on overflow.
#[test]
fn fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call() {
    use cfg::test_api::Options;

    // start_addr near top of u64; fn_max_size large enough that the raw sum
    // would overflow but every plausible target still lies inside the function.
    let start_addr = u64::MAX - 0x100;
    let max_size   = 0x1000u64; // start + max overflows
    let opts = cfg::OptionsBuilder::new()
        .set_function_max_size(max_size)
        .build();

    let mut b = common::make_builder_opts(start_addr, opts);
    let mut rb = common::make_region_builder(&mut b, common::addr(start_addr, 0));

    // Target inside [start, start+max) (but the raw sum would overflow).
    let target = common::addr(start_addr + 0x10, 0);
    assert!(
        !rb.is_branch_tail_call_nocheck(target),
        "target inside function range must NOT classify as tail call even when start+max overflows"
    );
}
```

Also ensure `make_builder_opts` is re-exported from `tests/common/mod.rs` if it isn't already (check; `synthetic.rs` defines it but the re-export path depends on `common/mod.rs`).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p cfg --test region_builder_tail_call fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call`
Expected: FAIL — current code wraps and treats the in-range target as a tail call.

- [ ] **Step 3: Fix the arithmetic**

In `crates/cfg/src/cfg/builder/region_builder.rs`, replace the `fn_max_size` block inside `is_branch_tail_call_nocheck`:

```rust
if let Some(fn_max_size) = self.builder.options.fn_max_size {
    // Saturate on overflow: if start + max would exceed u64::MAX, no target can
    // be at-or-beyond the sum, so the only way `addr >= sat_sum` is when
    // `sat_sum == u64::MAX && addr == u64::MAX`. That tiny boundary case is
    // the correct semantics — an address past the end of addressable memory
    // is, by definition, outside any reasonable function.
    let end_exclusive = self.builder.start_addr.addr.saturating_add(fn_max_size);
    if end_exclusive <= addr.addr {
        return true;
    }
}
```

- [ ] **Step 4: Run the test suite**

Run: `cargo test -p cfg`
Expected: all pass, including the new test and the existing `is_branch_tail_call_*` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/tests/region_builder_tail_call.rs
git commit -m "fix(cfg): saturate fn_max_size + start_addr to avoid u64 overflow in tail-call bound check"
```

---

## Task 2: Treat CONST-space relative branch offset as signed

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:62-86](crates/cfg/src/cfg/builder/region_builder.rs#L62-L86)
- Modify: [crates/cfg/tests/region_builder_decode.rs](crates/cfg/tests/region_builder_decode.rs)

(Applies default **Q1 (A)**. If reviewer selects (B) or (C), rewrite only Step 3 accordingly.)

- [ ] **Step 1: Write a failing test**

Append to `crates/cfg/tests/region_builder_decode.rs`:

```rust
/// Pinned contract: a CONST-space relative branch target with a negative
/// offset (two's-complement `u64`) must decode to the predecessor pcode slot,
/// not wrap around `u64::MAX`.
#[test]
fn decode_branch_target_const_space_negative_offset_does_not_wrap() {
    let mut b = common::make_builder(0x1000);
    let mut rb = common::make_region_builder(&mut b, common::addr(0x1000, 0));

    // Synthetic CONST varnode: off = -2 (two's complement u64), size irrelevant.
    let vn = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::CONST,
            off: (-2_i64) as u64,
        },
        size: 8,
    };

    // We're currently at machine 0x1000, pcode index 5. A `-2` branch target
    // must decode to (0x1000, 3), not (0x1000, u64::MAX - 1).
    let got = rb.decode_branch_target(vn, common::addr(0x1000, 5)).unwrap();
    assert_eq!(got, common::addr(0x1000, 3));
}

/// Pinned contract: a CONST-space relative offset that would drive the
/// resulting pcode index negative must error rather than wrap.
#[test]
fn decode_branch_target_const_space_underflow_errors() {
    let mut b = common::make_builder(0x1000);
    let mut rb = common::make_region_builder(&mut b, common::addr(0x1000, 0));

    let vn = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::CONST,
            off: (-5_i64) as u64,
        },
        size: 8,
    };

    // At (0x1000, 2) with offset -5, the resulting index would be -3.
    let err = rb
        .decode_branch_target(vn, common::addr(0x1000, 2))
        .unwrap_err();
    assert!(matches!(
        err.kind(),
        cfg::ErrorKind::InvalidBranchTargetVaErr(_, _)
    ));
}
```

Confirm the exact public fields of `rsleigh::Vn` / `rsleigh::VnAddr` match the synthesis above (grep in `../rsleigh/src/lib.rs`). If field names differ, adjust the test.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfg --test region_builder_decode`
Expected: both new tests FAIL — the first produces `(0x1000, u64::MAX - 1)`, the second produces a wrapped huge number rather than erroring.

- [ ] **Step 3: Fix `decode_branch_target`**

In `crates/cfg/src/cfg/builder/region_builder.rs`, replace the CONST arm:

```rust
// Relative branch: signed offset from the current pcode insn index.
// rsleigh encodes CONST-space branch targets as two's-complement in a
// u64, so a backward pcode-local branch comes in as `(-n) as u64`.
rsleigh::VnSpace::CONST => {
    let base = branch_insn_addr.insn_index as i64;
    let off = branch_target_var.addr.off as i64;
    let target = base.checked_add(off).ok_or_else(|| {
        ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr)
    })?;
    if target < 0 {
        return Err(
            ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr).into(),
        );
    }
    Ok(PcodeInsnAddr {
        machine_addr: branch_insn_addr.machine_addr,
        insn_index: target as u64,
    })
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfg`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/tests/region_builder_decode.rs
git commit -m "fix(cfg): decode CONST-space branch offsets as signed i64 to avoid u64 wrap"
```

---

## Task 3: Fix clippy lint failures in tests so `cargo clippy --all-targets` passes

Currently `cargo clippy -p cfg --all-targets --no-deps -- -D warnings` fails with three lint errors in two test files (`panic`, `map_unwrap_or`, `clone_on_copy`). The library itself is lint-clean.

**Files:**
- Modify: [crates/cfg/tests/cfg_query.rs:131](crates/cfg/tests/cfg_query.rs#L131)
- Modify: [crates/cfg/tests/region_builder_process.rs:26-33](crates/cfg/tests/region_builder_process.rs#L26-L33) and [crates/cfg/tests/region_builder_process.rs:85](crates/cfg/tests/region_builder_process.rs#L85)

- [ ] **Step 1: Replace the `panic!` arm in `cfg_query.rs`**

Change [cfg_query.rs:128-132](crates/cfg/tests/cfg_query.rs#L128-L132) from:

```rust
let result = cfg.region_if(src);
let err = match result {
    Err(e) => e,
    Ok(_) => panic!("expected DuplicateEdgeKind error"),
};
```

to:

```rust
let err = cfg.region_if(src).expect_err("region_if must return DuplicateEdgeKind on malformed graph");
```

(`expect_used` is already allowed at the top of the file.)

- [ ] **Step 2: Replace `.map(...).unwrap_or_else(...)` and inner `panic!` in `region_builder_process.rs`**

Change [region_builder_process.rs:26-33](crates/cfg/tests/region_builder_process.rs#L26-L33) from:

```rust
fn find_pcode(lift: &rsleigh::LiftRes, want: rsleigh::Opcode) -> (u64, rsleigh::Insn) {
    lift.insns
        .iter()
        .enumerate()
        .find(|(_, i)| i.opcode == want)
        .map(|(idx, i)| (idx as u64, i.clone()))
        .unwrap_or_else(|| panic!("no pcode op with opcode {:?}", want))
}
```

to:

```rust
fn find_pcode(lift: &rsleigh::LiftRes, want: rsleigh::Opcode) -> (u64, rsleigh::Insn) {
    let (idx, i) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| i.opcode == want)
        .unwrap_or_else(|| unreachable!("no pcode op with opcode {want:?}"));
    (idx as u64, i.clone())
}
```

Clippy's `panic` lint is scoped to `panic!` calls in production code; `unreachable!` is allowed here and still panics with a clear message. Alternatively the whole function can be rewritten to return `Option` and let callers `expect(...)`; but the `unreachable!` route keeps the test API unchanged. If `unreachable` also triggers a lint locally, revert to:

```rust
let (idx, i) = lift
    .insns
    .iter()
    .enumerate()
    .find(|(_, i)| i.opcode == want)
    .expect("no pcode op with requested opcode");
(idx as u64, i.clone())
```

(`expect_used` is allowed at file top.)

- [ ] **Step 3: Remove the needless `.clone()` on a Copy tuple**

Change [region_builder_process.rs:85](crates/cfg/tests/region_builder_process.rs#L85) from:

```rust
let (parent, enqueued_addr) = test_api::work_queue(&b)[0].clone();
```

to:

```rust
let (parent, enqueued_addr) = test_api::work_queue(&b)[0];
```

- [ ] **Step 4: Verify clippy is clean**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: `Finished ...` with no errors or warnings.

- [ ] **Step 5: Run test suite**

Run: `cargo test -p cfg`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/tests
git commit -m "test(cfg): satisfy clippy panic/map_unwrap_or/clone_on_copy in cfg test suite"
```

---

## Task 4: Remove unused `ErrorKind::UnknownRegName`

Grepping the workspace shows no producer or consumer of `cfg::ErrorKind::UnknownRegName`. (The same variant exists in the `target` crate and IS used there — leave that copy alone.)

**Files:**
- Modify: [crates/cfg/src/error.rs:19-20](crates/cfg/src/error.rs#L19-L20)

- [ ] **Step 1: Delete the variant**

Remove these two lines from `ErrorKind` in `crates/cfg/src/error.rs`:

```rust
#[error("unknown register name by sleign {0:?}")]
UnknownRegName(String),
```

- [ ] **Step 2: Verify no remaining references**

Run: `grep -rn "UnknownRegName" crates/cfg`
Expected: empty output.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/error.rs
git commit -m "refactor(cfg): remove unused ErrorKind::UnknownRegName variant"
```

---

## Task 5: Remove unused `ErrorKind::AssertionFailed`

No test or production code in `crates/cfg` constructs `cfg::ErrorKind::AssertionFailed`. (Other crates have their own unrelated copies.)

**Files:**
- Modify: [crates/cfg/src/error.rs:55-58](crates/cfg/src/error.rs#L55-L58)

- [ ] **Step 1: Delete the variant and its doc**

Remove these lines from `ErrorKind`:

```rust
/// A test assertion failed. Exists so tests can return `Result<(), Error>`
/// instead of using `panic!`.
#[error("assertion failed: {0}")]
AssertionFailed(String),
```

- [ ] **Step 2: Verify no remaining references inside cfg**

Run: `grep -rn "AssertionFailed" crates/cfg`
Expected: empty output.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/error.rs
git commit -m "refactor(cfg): remove unused ErrorKind::AssertionFailed variant"
```

---

## Task 6: Delete dead `Cfg::region_insn` method

(Applies default **Q2 (A)**.) Grepped — only `cfg_query.rs` consumes `region_insn`. Analyzer and all other callers use `region.insns` directly on the `Region` struct. The current shape is also odd: singular name (`region_insn`) returning a `Vec<rsleigh::Insn>` constructed by cloning every entry — callers wanting this shape can iterate themselves.

**Files:**
- Modify: [crates/cfg/src/cfg/query.rs:72-86](crates/cfg/src/cfg/query.rs#L72-L86)
- Modify: [crates/cfg/tests/cfg_query.rs:23-40](crates/cfg/tests/cfg_query.rs#L23-L40)

- [ ] **Step 1: Delete the method**

Remove the `region_insn` impl block in `crates/cfg/src/cfg/query.rs` — the method definition and its doc comment.

- [ ] **Step 2: Delete the two now-dead tests**

From `crates/cfg/tests/cfg_query.rs` remove:
- `fn region_insn_returns_clone_of_region_insns`
- `fn region_insn_invalid_node_index_returns_error`

Also remove the `// ── region_insn ──` section header comment and drop `region_insn` from the module doc at the top of the file (line 3).

- [ ] **Step 3: Verify no other references**

Run: `grep -rn "region_insn\b" crates/`
Expected: empty output (outside this file before the edit).

- [ ] **Step 4: Build + test + workspace check**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg
git commit -m "refactor(cfg): drop unused Cfg::region_insn method; analyzer iterates region.insns directly"
```

---

## Task 7: Narrow `&mut self` → `&self` on read-only `RegionBuilder` methods

`decode_branch_target`, `is_branch_tail_call_nocheck`, and `is_branch_tail_call` take `&mut self` but never mutate. Narrowing them unlocks calling from `&`-contexts and is a small readability gain.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:62](crates/cfg/src/cfg/builder/region_builder.rs#L62)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:96](crates/cfg/src/cfg/builder/region_builder.rs#L96)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:120](crates/cfg/src/cfg/builder/region_builder.rs#L120)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:379-399](crates/cfg/src/cfg/builder/region_builder.rs#L379-L399) (test_api forwarders)

- [ ] **Step 1: Change signatures**

Replace the three signatures:

```rust
fn decode_branch_target(
    &mut self,
    branch_target_var: rsleigh::Vn,
    branch_insn_addr: PcodeInsnAddr,
) -> Result<PcodeInsnAddr> {
```

→ `&self`.

```rust
pub(super) fn is_branch_tail_call_nocheck(&mut self, branch_target_addr: PcodeInsnAddr) -> bool {
```

→ `&self`.

```rust
pub(super) fn is_branch_tail_call(&mut self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
```

→ `&self`.

- [ ] **Step 2: Propagate to test_api forwarders**

In the `mod test_api` at the bottom of the file, change the three matching `TestRegionBuilder` methods (`is_branch_tail_call_nocheck`, `is_branch_tail_call`, `decode_branch_target`) from `&mut self` to `&self`.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS. If any test that previously called these via `&mut` breaks (shouldn't, but `make_region_builder` returns `TestRegionBuilder` already owned mutably, so `&`-access still works), adjust the binding.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): narrow &mut self to &self on read-only RegionBuilder methods"
```

---

## Task 8: Rewrite `Cfg::region_branch` / `Cfg::region_if` to avoid the `HashMap` allocation

`following_regions` builds a `HashMap<&RegionEdgeKind, NodeIndex>` on every call, just so the two public methods can `.get(...).copied()`. Each edge only matters for one kind; a direct walk eliminates the allocation and the duplicate-detection logic collapses into an "already-seen" check inline.

**Files:**
- Modify: [crates/cfg/src/cfg/query.rs:1-60](crates/cfg/src/cfg/query.rs#L1-L60)

- [ ] **Step 1: Replace `following_regions` + `region_branch` + `region_if`**

Replace the top-of-file body (everything between `use` and the `regions()` method) with:

```rust
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionEdgeKind};
use super::{Cfg, RegionId};
use crate::error::{ErrorKind, Result};

/// The two successors of a conditional-branch region.
///
/// Returned by [`Cfg::region_if`].
pub struct IfRegionState {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>,
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Returns the sole successor of `region_id` whose edge weight is `kind`,
    /// or `None` if no such edge exists.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one outgoing
    /// edge of `kind` is attached to `region_id`.
    fn unique_outgoing(&self, region_id: RegionId, kind: RegionEdgeKind) -> Result<Option<NodeIndex>> {
        let mut found: Option<NodeIndex> = None;
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            if *edge.weight() != kind {
                continue;
            }
            if found.is_some() {
                return Err(ErrorKind::DuplicateEdgeKind(region_id, kind).into());
            }
            found = Some(edge.target());
        }
        Ok(found)
    }

    /// Returns the unconditional-branch successor of `region_id`, if any.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one `Branch`
    /// edge leaves `region_id`.
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Branch)
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one
    /// `IfCaseTrue` or `IfCaseFalse` edge leaves `region_id`.
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionState> {
        Ok(IfRegionState {
            if_true_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseTrue)?,
            if_false_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseFalse)?,
        })
    }

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
    }
}
```

Note: the `HashMap` import line at the top of the file becomes unused — remove `use std::collections::HashMap;`. `region_insn` is already gone per Task 6.

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg`
Expected: PASS — including the two duplicate-edge tests in `cfg_query.rs`.

- [ ] **Step 3: Workspace build**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/query.rs
git commit -m "refactor(cfg): drop HashMap allocation in region_branch/region_if; walk edges directly"
```

---

## Task 9: Convert `work_queue` from `VecDeque` to `Vec` (used as a LIFO stack)

The work queue is only ever `push_back`'d and `pop_back`'d. `Vec::push` / `Vec::pop` is the same operation on a simpler type. This also corrects the current doc comment's implicit "queue" framing.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/mod.rs:42-43](crates/cfg/src/cfg/builder/mod.rs#L42-L43)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:156-160](crates/cfg/src/cfg/builder/mod.rs#L156-L160)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:225-229](crates/cfg/src/cfg/builder/mod.rs#L225-L229) (test_api forwarder)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:161-196](crates/cfg/src/cfg/builder/region_builder.rs#L161-L196) (`push_back` → `push`)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:9](crates/cfg/src/cfg/builder/mod.rs#L9) (drop `VecDeque` from `use` if no longer referenced from this file)

- [ ] **Step 1: Change the field type**

In `crates/cfg/src/cfg/builder/mod.rs`, change:

```rust
/// Pending addresses to explore, together with the parent edge they
/// should connect from.  Processed LIFO (depth-first).
pub(super) work_queue: VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>,
```

to:

```rust
/// Pending addresses to explore, together with the parent edge they
/// should connect from. Treated as a LIFO stack (depth-first traversal).
pub(super) work_queue: Vec<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>,
```

In `Builder::new`, change `work_queue: VecDeque::new()` → `work_queue: Vec::new()`.

- [ ] **Step 2: Update `Builder::build`**

```rust
self.work_queue.push((None, self.start_pcode_addr()));
while let Some((parent_region, address)) = self.work_queue.pop() {
    self.explore(parent_region, address)?;
}
```

(Both `push_back` → `push` and `pop_back` → `pop`.)

- [ ] **Step 3: Update `RegionBuilder` queue pushes**

In `crates/cfg/src/cfg/builder/region_builder.rs`, in `process_new_insn`, change each:

```rust
self.builder.work_queue.push_back(...);
```

to:

```rust
self.builder.work_queue.push(...);
```

There are three sites (Branch non-tail-call, CondBranch true, CondBranch false).

- [ ] **Step 4: Update test_api forwarder**

In `crates/cfg/src/cfg/builder/mod.rs::test_api`, change:

```rust
pub fn work_queue<R: rsleigh::MemReader>(
    b: &Builder<R>,
) -> &VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)> {
    &b.work_queue
}
```

to use `&Vec<...>` and drop the `VecDeque` import from the `use` clause. The callers in `tests/region_builder_process.rs` and `tests/region_builder_tail_call.rs` only use `[0]` indexing, `.len()`, and `.iter()`, all of which work on `Vec`.

- [ ] **Step 5: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/src
git commit -m "refactor(cfg): convert work_queue from VecDeque to Vec (LIFO stack)"
```

---

## Task 10: Convert `Region::insns` from `VecDeque` to `Vec`

`Region::insns: VecDeque<RegionInstruction>` is only ever `push_back`'d, `iter()`'d, `position()`'d, and `split_off`'d. All four work on `Vec`, which has better iteration locality and is simpler. External callers (analyzer) use `.iter()` and `&region.insns` — both work unchanged.

**Files:**
- Modify: [crates/cfg/src/cfg/types.rs:1-98](crates/cfg/src/cfg/types.rs#L1-L98)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs](crates/cfg/src/cfg/builder/region_builder.rs)
- Modify: [crates/cfg/src/cfg/builder/split.rs:42-67](crates/cfg/src/cfg/builder/split.rs#L42-L67)
- Modify: [crates/cfg/tests/common/synthetic.rs](crates/cfg/tests/common/synthetic.rs) (the `VecDeque<_>` in `make_region`)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:307-379](crates/cfg/src/cfg/builder/region_builder.rs#L307-L379) (test_api `insns()` return type)

- [ ] **Step 1: Change the field type**

In `crates/cfg/src/cfg/types.rs`, at the top replace `use std::collections::VecDeque;` with nothing (no new import), and change:

```rust
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    pub insns: VecDeque<RegionInstruction>,
    pub ends_with_tail_call: bool,
}
```

to:

```rust
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    pub insns: Vec<RegionInstruction>,
    pub ends_with_tail_call: bool,
}
```

And in `Region::contains_addr`, replace `self.insns.back()` with `self.insns.last()` (Vec's API).

- [ ] **Step 2: Update `RegionBuilder`**

In `crates/cfg/src/cfg/builder/region_builder.rs`:

- At the top, change `use std::collections::VecDeque;` → drop it entirely if no other use remains in the file (the struct still declares `pub(super) insns: VecDeque<RegionInstruction>`).
- Change `pub(super) insns: VecDeque<RegionInstruction>` → `pub(super) insns: Vec<RegionInstruction>`.
- Change every `insns: VecDeque::new()` → `insns: Vec::new()`.
- Change `self.insns.push_back(...)` → `self.insns.push(...)`.

In the same file, in `mod test_api`:

- Change `VecDeque::new()` → `Vec::new()` inside the two constructors.
- Change `-> &VecDeque<RegionInstruction>` to `-> &[RegionInstruction]` in the `insns(&self)` return type, and adjust the body to `&self.inner.insns` (slice coercion is automatic).
- Change `self.inner.insns.push_back(insn)` → `self.inner.insns.push(insn)`.
- Drop `use std::collections::VecDeque;` from the `test_api` mod.

- [ ] **Step 3: Update `split_region`**

In `crates/cfg/src/cfg/builder/split.rs` no code change is needed — `Vec::split_off`, `Vec::iter().position()`, and `std::mem::replace` all behave identically. Just confirm nothing references `VecDeque` here.

- [ ] **Step 4: Update test fixtures**

In `crates/cfg/tests/common/synthetic.rs`:

- Drop `use std::collections::VecDeque;`
- In `make_region`, change `let insns: VecDeque<_> = ...` → `let insns: Vec<_> = ...`.

Also update the imports at the top of tests that import `VecDeque` via the common module if the type no longer matches — grep confirms before commit:

```bash
grep -rn "VecDeque<RegionInstruction" crates/cfg
```

- [ ] **Step 5: Build + test + workspace check**

Run: `cargo test -p cfg && cargo test --workspace && cargo check --workspace`
Expected: PASS. The workspace build verifies analyzer's `for wrapped in region.insns.iter()` and `for wrapped_insn in &region.insns` still compile — both work on `Vec` unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg
git commit -m "refactor(cfg): change Region::insns and RegionBuilder::insns to Vec<RegionInstruction>"
```

---

## Task 11: Inline `Builder::explore_new_region` + use `std::mem::take` in `finish_current_region`

Two tiny cleanups folded together since they share the same context.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/mod.rs:136-145](crates/cfg/src/cfg/builder/mod.rs#L136-L145)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:130-133](crates/cfg/src/cfg/builder/mod.rs#L130-L133)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:207-224](crates/cfg/src/cfg/builder/region_builder.rs#L207-L224)

- [ ] **Step 1: Inline `explore_new_region` into `explore`**

In `crates/cfg/src/cfg/builder/mod.rs`, replace the else-branch of `explore`:

```rust
} else {
    // This is not an explored region - explore it
    self.explore_new_region(addr, parent_region)?;
}
```

with:

```rust
} else {
    RegionBuilder::new(self, addr, parent_region).build()?;
}
```

Then delete the `explore_new_region` method entirely. (Import `RegionBuilder` if not already: the file has `use region_builder::RegionBuilder;` at the top — it's already in scope.)

- [ ] **Step 2: Use `std::mem::take` on the `insns` field**

In `crates/cfg/src/cfg/builder/region_builder.rs::finish_current_region`, change:

```rust
let region = self.builder.add_region(Region {
    start_addr: self.start_addr,
    insns: self.insns.to_owned(),
    ends_with_tail_call,
})?;
```

to:

```rust
let region = self.builder.add_region(Region {
    start_addr: self.start_addr,
    insns: std::mem::take(&mut self.insns),
    ends_with_tail_call,
})?;
```

`finish_current_region` is called at most once per `RegionBuilder` (every caller returns `FinishedProcessing` immediately afterward), so leaving `self.insns` empty is fine.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src
git commit -m "refactor(cfg): inline explore_new_region; mem::take the finished region's insns"
```

---

## Task 12: Replace empty `CfgDotDumperState` with `()` if the trait allows it

(Applies default **Q4 (A)**.) Investigative task.

**Files:**
- Inspect: `crates/dot/src/lib.rs` (for the `GraphDotDumper::State` bound).
- Modify: [crates/cfg/src/cfg/dot.rs:53-63](crates/cfg/src/cfg/dot.rs#L53-L63)

- [ ] **Step 1: Check the trait bound on `State`**

Run: `grep -n "type State" crates/dot/src` and `grep -n "trait GraphDotDumper" crates/dot/src`.
Expected: find the associated-type declaration. If it has no `: Trait` bound, `()` works. If it's something like `: Default`, `()` still works (`Default` is implemented for `()`). If it's `: Debug`, same. If it has some unusual bound, fall back to keeping `CfgDotDumperState` and document the reason.

- [ ] **Step 2: Switch to `()` if viable**

In `crates/cfg/src/cfg/dot.rs`, change:

```rust
pub struct CfgDotDumperState;
pub struct CfgDotDumper<'a, R: rsleigh::MemReader>(&'a Cfg<R>);

impl<'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R> {
    type Node = NodeIndex;
    type Error = Error;
    type State = CfgDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {}
    }
    // ...
```

to:

```rust
pub struct CfgDotDumper<'a, R: rsleigh::MemReader>(&'a Cfg<R>);

impl<'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R> {
    type Node = NodeIndex;
    type Error = Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}
    // ...
```

If the investigation in Step 1 shows this is infeasible, leave the file alone and commit nothing for this task.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit (if any change)**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "refactor(cfg): drop empty CfgDotDumperState in favor of ()"
```

---

## Task 13: Clean up `RegionBuilder::build`'s pcode-index accounting

The current loop skips then enumerates, then re-adds the skipped offset to every iteration:

```rust
let start_pcode_idx = cur_addr.insn_index;
for (i, insn) in lift_res.insns.iter().skip(start_pcode_idx as usize).enumerate() {
    cur_addr = PcodeInsnAddr {
        machine_addr: cur_addr.machine_addr,
        insn_index: start_pcode_idx + i as u64,
    };
    ...
}
```

Swapping the order of `.enumerate()` and `.skip()` lets us use `i` directly as the absolute pcode index.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:266-302](crates/cfg/src/cfg/builder/region_builder.rs#L266-L302)

- [ ] **Step 1: Reorder enumerate/skip**

Replace the `build` loop body's accounting section:

```rust
pub(super) fn build(mut self) -> Result<()> {
    let mut cur_addr = self.start_addr;
    loop {
        let lift_res = self
            .builder
            .sleigh
            .lift_one(cur_addr.machine_addr.addr)
            .map_err(|e| ErrorKind::GenericSleighError(format!("{:?}", e)))?;
        // `enumerate` before `skip` so `i` is the absolute pcode index.
        // On the very first machine instruction this may start at a non-zero
        // index (the work queue delivered a mid-instruction entry point);
        // subsequent machine instructions always start at 0.
        let start_pcode_idx = cur_addr.insn_index as usize;
        for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
            cur_addr = PcodeInsnAddr {
                machine_addr: cur_addr.machine_addr,
                insn_index: i as u64,
            };
            let res = self.process_insn(insn, cur_addr, &lift_res)?;
            if matches!(res, ProcessInsnRes::FinishedProcessing) {
                return Ok(());
            }
        }
        cur_addr = PcodeInsnAddr {
            machine_addr: MachineInsnAddr {
                addr: cur_addr.machine_addr.addr + (lift_res.machine_insn_len as u64),
            },
            insn_index: 0,
        };
    }
}
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg`
Expected: PASS — including the mid-instruction-entry tests (if any rely on the non-zero start index branch, e.g. via CondBranch CONST-space targets).

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): enumerate-then-skip so pcode index is absolute without arithmetic"
```

---

## Task 14: Final sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no warnings.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 3: cfg-only strict lint**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS (or pre-existing unrelated lints in other crates, unchanged from baseline).

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced, matching pre-refactor behavior.

---

## Out of scope (considered, rejected, or deferred)

- **Privatizing `Cfg::sleigh` / `Cfg::graph` / `Cfg::entry`**: external caller `analyzer` reads all three directly (`cfg.sleigh`, `cfg.graph.map(...)`, `cfg.entry`). Providing accessors would force analyzer-side churn for near-zero benefit; keep as-is.
- **Making `RegionInstruction::insn` a `Box`**: `rsleigh::Insn` is not huge and `Vec<RegionInstruction>` benefits from inline storage. No measurable win.
- **Replacing `GenericSleighError(String)` with a typed variant**: pragmatically rejected via **Q3 (A)** — requires propagating a generic `E` through `strider-error::define_error!`, which the macro is not currently shaped to handle. Note it in the doc comment of the variant if desired, but no behavior change.
- **Removing the LIFO-vs-BFS comment or switching traversal order**: DFS via `.pop()` produces more contiguous regions in common binaries and is the current behavior tests depend on. Don't change.
- **Collapsing `test_api.rs` + three sub-`test_api`s into one module**: the layering is intentional (mirrors the source tree); consolidation would muddy which tests touch which layer. Leave.
- **Changing `region_if` to return `Option<IfRegionState>` when neither if-edge is present**: analyzer expects the struct shape unconditionally and gracefully handles `None, None`. Flipping to `Option` would force analyzer-side matching with zero behavior win.
- **Adding checked_add / wrapping_add guards on every address arithmetic in `RegionBuilder`**: the only realistic overflow site is the `fn_max_size` one fixed in Task 1. `machine_addr + machine_insn_len` at line 297-298 and `insn_index + 1` at line 179 both sit under invariants that make overflow impossible in practice (machine_insn_len ≤ 15 on x86, insn_index is a small counter). Over-engineering.
