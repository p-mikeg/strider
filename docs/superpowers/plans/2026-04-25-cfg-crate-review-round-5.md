# CFG Crate Review — Round 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round 5 cleanup of the `cfg` crate. After rounds 1–4 the crate is structurally sound, has no panics in normal error paths, and is lint-clean at the default level. Round 5 closes the six lib-level `clippy::pedantic` warnings that round 4's `next_pcode_addr` and pre-existing `decode_branch_target` arithmetic exposed, elevates two `Cfg::regions()`-returned types from "reachable via inference only" to first-class re-exports, and tightens one redundant inline comment block in `split_region`.

**Architecture:** Each task is a self-contained, independently-committable change. The arithmetic refactor (Task 1) replaces ad-hoc `u64 ↔ i64` casting with `u64::checked_add_signed` / `u64::cast_signed`, narrowing the cast surface to a single intentional reinterpretation. No public-API change beyond two new pub-uses (`Region`, `RegionInstruction`) — `Builder`, `Cfg`, `OptionsBuilder`, `RegionEdgeKind`, `RegionId`, the four `Cfg::*` accessors, and `Region::{start_addr, insns, ends_with_tail_call}` are unchanged. No behaviour change in any execution path.

**Tech Stack:** Rust 1.93+ (`u64::cast_signed` stabilised 1.87), `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Open questions for the reviewer before execution

Each one materially changes a task below. Pick one per group; defaults are noted.

**Q1 — `decode_branch_target` arithmetic refactor (Task 1):** Today's code uses raw `as` casts: `let base = branch_insn_addr.insn_index as i64; let off = branch_target_var.addr.off as i64; … let target = base.checked_add(off)?; … if target < 0 || (target as u64) >= …; … insn_index: target as u64`. That's two `u64 → i64` casts, one `i64 → u64` cast, and one fallible signed addition — clippy `pedantic` flags four of them (`cast_possible_wrap` × 2, `cast_sign_loss` × 2). The arithmetic also juggles signedness more than necessary: only the *offset* is genuinely signed (CONST-space encodes pcode offsets as two's-complement in `u64`); the *base index* is naturally `u64`.
  - **(A)** Refactor to: `let off = branch_target_var.addr.off.cast_signed(); let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or(…)?; if target >= u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX) { return Err(…); }`. One intentional reinterpretation (`cast_signed`), one safe `checked_add_signed`, no `as` casts in the result. **Default choice.** Matches the underlying semantic shape (offset is signed, index is unsigned) and clears all four lints in one move.
  - **(B)** Keep the current shape, add a function-level `#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]` with a justification comment. Cheaper but doesn't improve the arithmetic intent.
  - **(C)** Skip — accept the `pedantic` warnings as background noise.

  **Reviewer action:** confirm (A).

**Q2 — `next_pcode_addr` index-comparison flip (Task 2):** The current code does `if (addr.insn_index as usize) + 1 < lift_res.insns.len()`, which clippy flags as `cast_possible_truncation` (truncates on hypothetical 32-bit hosts where `usize == u32`). Strider isn't expected to run on 32-bit, but the cleaner phrasing is to compare in `u64` space: `if addr.insn_index + 1 < lift_res.insns.len() as u64`. The `usize → u64` cast is widening on every supported target, so it doesn't trip any pedantic lint.
  - **(A)** Flip to `lift_res.insns.len() as u64`. **Default choice.**
  - **(B)** `#[allow(clippy::cast_possible_truncation)]` on the existing line with a justification. Equivalent end state, less idiomatic.

  **Reviewer action:** confirm (A).

**Q3 — `RegionBuilder::build` `start_pcode_idx` cast (Task 3):** `Iterator::skip` requires `usize`, so `let start_pcode_idx = cur_addr.insn_index as usize;` is unavoidable here. Same `cast_possible_truncation` lint. Two options:
  - **(A)** Add a localised `#[allow(clippy::cast_possible_truncation)]` with a one-line comment ("pcode counts within a single machine instruction are bounded by Sleigh's per-insn output and always fit in `usize` on supported 64-bit targets"). **Default choice.** Honest about the platform assumption; doesn't introduce a panic or a new error variant for an unreachable case.
  - **(B)** `usize::try_from(cur_addr.insn_index).expect(…)` — violates the no-panic rule.
  - **(C)** Add a new `ErrorKind::PcodeIndexTooLarge(PcodeInsnAddr)` variant and propagate. Defensive; pays the cost of a new error variant for an impossible case (YAGNI per CLAUDE.md).

  **Reviewer action:** confirm (A).

**Q4 — `Region` / `RegionInstruction` re-exports (Task 4):** `Cfg::regions()` returns `impl Iterator<Item = &Region>` and `&Region.insns` is `Vec<RegionInstruction>` (both `pub` fields). Today the downstream `analyzer` crate consumes them via inference (`for region in cfg.regions() { for wrapped in region.insns.iter() { … } }` in `crates/analyzer/src/analyzer/pipeline.rs:77-83`). The types are reachable but unnameable from outside `cfg`. They can only be named via `cfg::test_api::{Region, RegionInstruction}`, which is `#[doc(hidden)]` and "not covered by semver" per its own module doc.
  - **(A)** Add `pub use cfg::types::{Region, RegionInstruction};` to `crates/cfg/src/lib.rs` so downstream callers can name them in struct fields, function signatures, and trait bounds. Aligns the public name surface with the actual returned types. Costs nothing (the field/return shape is already part of the public API). **Default choice.**
  - **(B)** Skip — keep them nameable only via `test_api`. Maintains the (already-leaky) encapsulation.

  Note: `MachineInsnAddr` has the same shape (it's the type of the `pub machine_addr: MachineInsnAddr` field of `PcodeInsnAddr`, which IS `pub use`'d). Adding it too is a one-line addition. Default choice: also re-export `MachineInsnAddr` for symmetry.

  **Reviewer action:** confirm (A) including `MachineInsnAddr`.

**Q5 — `split_region` redundant inline-comment block (Task 5):** Lines 31–36 of `crates/cfg/src/cfg/builder/split.rs` repeat, in less-precise prose, what the doc comment above already says about why `region_id` stays attached to the second half. The doc comment lists the three manual fixups; the inline block lists "4 things that break." Item 4 ("parent of the popped work-queue item") is interesting context that doesn't appear in the doc comment.
  - **(A)** Delete the redundant inline block; absorb the unique fact (item 4) into the doc comment as a one-liner. **Default choice.**
  - **(B)** Leave as-is.

  **Reviewer action:** confirm (A).

**Q6 — `split_region` no-op contract test (Task 6):** `split_region(region_id, addr)` returns `Ok(region_id)` unchanged when `addr == region.start_addr` (the `if split_index == 0` guard at `split.rs:48`). This contract is unreachable from `Builder::explore` (which checks `region.start_addr == addr` first), but is reachable via the `test_api` direct call. It is currently untested.
  - **(A)** Add a single integration test in `crates/cfg/tests/builder_split_region.rs` that pins the no-op contract: `split_region(rid, region.start_addr) → Ok(rid)`, graph node count and edge count unchanged. **Default choice.**
  - **(B)** Skip — defensive guard is obvious from inspection.

  **Reviewer action:** confirm (A).

**Reviewer's choices (defaults):** Q1 = A, Q2 = A, Q3 = A, Q4 = A (incl. `MachineInsnAddr`), Q5 = A, Q6 = A.

---

## Task 1: Refactor `decode_branch_target` arithmetic to `checked_add_signed`

**Problem:** Lines 117, 118, 122, 131 of [crates/cfg/src/cfg/builder/region_builder.rs](crates/cfg/src/cfg/builder/region_builder.rs#L99-L145) implement signed-offset addition on the pcode index using four `as` casts (`u64 → i64` × 2, `i64 → u64` × 2). All four are flagged by `clippy::pedantic` (`cast_possible_wrap` × 2, `cast_sign_loss` × 2). The arithmetic is fundamentally `u64.checked_add_signed(i64) -> Option<u64>` — Rust has had a one-call primitive for this since 1.66 — and only the offset reinterpretation needs an explicit `as`-equivalent.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:99-145](crates/cfg/src/cfg/builder/region_builder.rs#L99-L145) (the `decode_branch_target` CONST-space arm)

- [ ] **Step 1: Inspect the current code**

Read [crates/cfg/src/cfg/builder/region_builder.rs:99-145](crates/cfg/src/cfg/builder/region_builder.rs#L99-L145) — confirm the function shape matches the plan's expected starting state. The CONST-space arm (lines 116-133) is the only block that changes.

- [ ] **Step 2: Rewrite the CONST-space arm**

In `decode_branch_target`, replace the CONST-space arm body (lines 116-133) with:

```rust
            rsleigh::VnSpace::CONST => {
                // CONST-space encodes the pcode-target offset as a
                // two's-complement i64 stored in a u64 (so a backward branch
                // arrives as `(-n) as u64`). `cast_signed` is the bit-pattern-
                // preserving u64→i64 reinterpretation; `checked_add_signed`
                // catches either-direction overflow on the index addition.
                let off = branch_target_var.addr.off.cast_signed();
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or(
                    ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
                )?;
                // The resulting index must land within the current machine
                // instruction's pcode sequence: `target < lift_res.insns.len()`.
                // An out-of-range index would otherwise be silently skipped by
                // the build loop, producing a wrong CFG with no diagnostic.
                let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
                if target >= pcode_count {
                    return Err(ErrorKind::InvalidBranchTargetVaErr(
                        branch_target_var,
                        branch_insn_addr,
                    )
                    .into());
                }
                Ok(PcodeInsnAddr {
                    machine_addr: branch_insn_addr.machine_addr,
                    insn_index: target,
                })
            }
```

Notes for the executor:
- `u64::cast_signed` is stable since Rust 1.87.0; the workspace targets ≥ 1.93.0 (`rustc --version` confirms).
- `u64::checked_add_signed(i64) -> Option<u64>` is stable since Rust 1.66.0.
- `u64::try_from(usize)` always succeeds on any platform where `usize ≤ u64` (every supported target). `unwrap_or(u64::MAX)` is a defensive saturation — if a hypothetical >64-bit-pointer target ever runs this, the condition `target >= u64::MAX` correctly rejects all but the single boundary case, which we accept.
- The code keeps the same external behaviour: same error variant on any invalid input, same `Ok` shape on success.

- [ ] **Step 3: Build and run the existing decode tests**

Run: `cargo test -p cfg --test region_builder_decode`
Expected: PASS — every existing test (negative-offset, underflow, past-end, at-end, default-code-space, register, unique, unknown space) continues to pass. The arithmetic change is observationally identical for all in-range and out-of-range inputs.

- [ ] **Step 4: Verify the four warnings are gone**

Run: `cargo clippy -p cfg --no-deps -- -W clippy::pedantic 2>&1 | grep -E 'cast_possible_wrap|cast_sign_loss'`
Expected: empty (no `cast_possible_wrap` or `cast_sign_loss` warnings on `region_builder.rs`).

- [ ] **Step 5: Run the full cfg suite**

Run: `cargo test -p cfg`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): use checked_add_signed for CONST-space pcode-index arithmetic"
```

---

## Task 2: Flip `next_pcode_addr` index comparison into `u64` space

**Problem:** Line 24 of [crates/cfg/src/cfg/builder/region_builder.rs](crates/cfg/src/cfg/builder/region_builder.rs#L20-L39) does `(addr.insn_index as usize) + 1 < lift_res.insns.len()`. The `u64 → usize` cast is potentially-truncating on 32-bit targets and clippy flags it (`cast_possible_truncation`). Comparing in the other direction — promoting `lift_res.insns.len()` to `u64` — is a *widening* cast on every supported target, which clippy doesn't warn about.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:20-39](crates/cfg/src/cfg/builder/region_builder.rs#L20-L39)

- [ ] **Step 1: Rewrite the within-machine-insn branch**

Change:

```rust
fn next_pcode_addr(
    addr: PcodeInsnAddr,
    lift_res: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    if (addr.insn_index as usize) + 1 < lift_res.insns.len() {
        return Ok(PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        });
    }
    let next_machine = addr
        .machine_addr
        .addr
        .checked_add(lift_res.machine_insn_len as u64)
        .ok_or(ErrorKind::MachineAddrOverflow(addr))?;
    Ok(PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: next_machine },
        insn_index: 0,
    })
}
```

to:

```rust
fn next_pcode_addr(
    addr: PcodeInsnAddr,
    lift_res: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    // Compare in u64 space: usize → u64 is widening on every supported
    // target and avoids a potentially-truncating u64 → usize cast.
    let pcode_count = lift_res.insns.len() as u64;
    if addr.insn_index + 1 < pcode_count {
        return Ok(PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        });
    }
    let next_machine = addr
        .machine_addr
        .addr
        .checked_add(lift_res.machine_insn_len as u64)
        .ok_or(ErrorKind::MachineAddrOverflow(addr))?;
    Ok(PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: next_machine },
        insn_index: 0,
    })
}
```

Notes:
- `addr.insn_index + 1` cannot overflow in the comparison: insn_index is bounded by Sleigh's per-machine-insn pcode count (a small constant).
- The new `lift_res.insns.len() as u64` cast is widening (`usize ≤ u64` on every supported platform); clippy doesn't flag it.
- Behaviour is identical: same predicate, same branch.

- [ ] **Step 2: Run the decode suite + full cfg suite**

```bash
cargo test -p cfg --test region_builder_decode
cargo test -p cfg
```
Expected: PASS — existing pinned tests for both branches of `next_pcode_addr` (`next_pcode_addr_within_machine_insn_advances_pcode_index`, `next_pcode_addr_non_overflowing_advance_succeeds`, `next_pcode_addr_machine_address_overflow_errors`) continue to pass.

- [ ] **Step 3: Verify the warning is gone**

Run: `cargo clippy -p cfg --no-deps -- -W clippy::pedantic 2>&1 | grep -E 'region_builder.rs:24'`
Expected: empty (no warning on the `next_pcode_addr` index comparison).

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "style(cfg): compare next_pcode_addr's pcode index in u64 space"
```

---

## Task 3: `#[allow]` the unavoidable `start_pcode_idx` cast

**Problem:** `RegionBuilder::build` at [crates/cfg/src/cfg/builder/region_builder.rs:332](crates/cfg/src/cfg/builder/region_builder.rs#L332) does `let start_pcode_idx = cur_addr.insn_index as usize;` to feed `Iterator::skip(usize)`. There is no in-the-language way to compare a `u64` to a `usize`-shaped iterator-position on 32-bit targets; the cast is intrinsic to the API. We accept the (impossible-in-practice) truncation and document it.

The justification is honest: pcode counts per machine instruction are bounded by Sleigh's per-instruction output (typically ≤ 64; Sleigh's hard cap is 256), and `usize ≥ 32 bits` on every supported target.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:328-340](crates/cfg/src/cfg/builder/region_builder.rs#L328-L340) (just the one line, with a comment)

- [ ] **Step 1: Wrap the cast with `#[allow]` and justify**

In `RegionBuilder::build`'s top-of-loop region, change:

```rust
            // `enumerate` before `skip` so `i` is the absolute pcode index.
            // On the very first machine instruction this may start at a non-zero
            // index (the work queue delivered a mid-instruction entry point);
            // subsequent machine instructions always start at 0.
            let start_pcode_idx = cur_addr.insn_index as usize;
```

to:

```rust
            // `enumerate` before `skip` so `i` is the absolute pcode index.
            // On the very first machine instruction this may start at a non-zero
            // index (the work queue delivered a mid-instruction entry point);
            // subsequent machine instructions always start at 0.
            //
            // `Iterator::skip` requires `usize`. Pcode counts per machine
            // instruction are bounded by Sleigh's per-insn output (≤ 256); on
            // every supported target `usize ≥ u32`, so the cast cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let start_pcode_idx = cur_addr.insn_index as usize;
```

- [ ] **Step 2: Build + clippy + test**

```bash
cargo clippy -p cfg --no-deps -- -W clippy::pedantic 2>&1 | grep -E 'region_builder.rs'
cargo test -p cfg
```
Expected: clippy output empty for `region_builder.rs`; `cargo test` PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "docs(cfg): justify the start_pcode_idx u64 -> usize cast in build loop"
```

---

## Task 4: Re-export `Region`, `RegionInstruction`, and `MachineInsnAddr`

**Problem:** Three types are part of the cfg crate's effective public surface (returned by `Cfg::regions()`, exposed as a `pub` field of `PcodeInsnAddr`, or named in `Region.insns: Vec<RegionInstruction>`) but are not in the production `pub use` list. Downstream consumers can use them via inference (`for region in cfg.regions() { … }` works without naming `Region`), but cannot name them in their own `struct` fields, function signatures, or `where` clauses without reaching into `cfg::test_api::*`, which is `#[doc(hidden)]` and explicitly "not covered by semver."

`crates/analyzer/src/analyzer/pipeline.rs:77-83` is the live demonstration of this papering-over: it loops over `cfg.regions()` and `region.insns.iter()` purely via type inference.

**Files:**
- Modify: [crates/cfg/src/cfg/mod.rs:18](crates/cfg/src/cfg/mod.rs#L18) (add `Region`, `RegionInstruction`, `MachineInsnAddr` to the re-export line)

- [ ] **Step 1: Confirm current state**

Read [crates/cfg/src/cfg/mod.rs:18](crates/cfg/src/cfg/mod.rs#L18). Current line:

```rust
pub use types::{PcodeInsnAddr, RegionEdgeKind};
```

- [ ] **Step 2: Extend the re-export**

Change to:

```rust
pub use types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction};
```

Also confirm [crates/cfg/src/lib.rs:18](crates/cfg/src/lib.rs#L18) — the lib.rs re-export uses `pub use cfg::{Builder, Cfg, IfRegionState, OptionsBuilder, RegionEdgeKind, RegionId};` — needs to add the three new names:

```rust
pub use cfg::{
    Builder, Cfg, IfRegionState, MachineInsnAddr, OptionsBuilder, Region, RegionEdgeKind,
    RegionId, RegionInstruction,
};
```

- [ ] **Step 3: Build the workspace + downstream crate to confirm no name collisions**

```bash
cargo build --workspace
cargo test -p cfg
cargo test -p analyzer
```
Expected: PASS. The new `pub use` items add names but don't shadow anything in the cfg or analyzer crates (verified by grepping `MachineInsnAddr`, `Region`, `RegionInstruction` in `crates/analyzer/src/`).

- [ ] **Step 4: Verify the names are nameable from outside**

Run: `cargo doc -p cfg --no-deps`
Expected: PASS, no warnings; the rendered doc lists `Region`, `RegionInstruction`, `MachineInsnAddr` at the crate root.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/mod.rs crates/cfg/src/lib.rs
git commit -m "refactor(cfg): re-export Region, RegionInstruction, and MachineInsnAddr at lib root"
```

---

## Task 5: Tighten redundant inline comment in `split_region`

**Problem:** Lines 31–36 of [crates/cfg/src/cfg/builder/split.rs](crates/cfg/src/cfg/builder/split.rs#L26-L86) repeat in less-precise prose what the doc comment (lines 9–25) already establishes. The block lists "4 things that break" when changing the region id; the doc comment lists the three manual fixups. The unique fact that the inline block carries — "items in the queue that use `region_id` as parent are also auto-handled by the swap" — belongs in the doc comment, not duplicated as inline prose.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/split.rs:8-37](crates/cfg/src/cfg/builder/split.rs#L8-L37)

- [ ] **Step 1: Update the doc comment**

Replace lines 8–25 (the existing doc comment on `split_region`) with:

```rust
    /// Splits the region identified by `region_id` at `addr`, creating two
    /// regions:
    ///
    /// - **first**: instructions *before* `addr` — gets a new [`NodeIndex`].
    /// - **second**: instructions *from* `addr` onwards — **retains** `region_id`.
    ///
    /// Retaining `region_id` for the second half avoids having to update:
    /// - outgoing edges (children) of the original region, and
    /// - any work-queue entries that still reference `region_id` as a parent
    ///   (including the popped item that triggered this split — its parent
    ///   pointer carries forward to the second half automatically).
    ///
    /// The following fixups ARE performed manually:
    /// 1. Incoming edges (parents) are rewired to the first region.
    /// 2. A [`RegionEdgeKind::Fallthrough`] edge is added from first → second.
    /// 3. The `start_addr_to_region_id` map is updated for both halves.
    ///
    /// Returns `region_id` (the second region) on success, or `region_id`
    /// unchanged when `addr` is already the region start (no-op split).
```

- [ ] **Step 2: Drop the redundant inline block**

Delete lines 31–36 (the `// The idea here is to swap … // 4. The parent of the popped …` block) entirely. The function body should start at the `let second_region = self.graph.node_weight_mut(region_id)…` line directly under the function signature.

After the change, the function body should be:

```rust
    pub(super) fn split_region(
        &mut self,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        let second_region = self
            .graph
            .node_weight_mut(region_id)
            .ok_or(ErrorKind::InvalidRegion(region_id))?;
        let split_index = second_region
            .insns
            .iter()
            .position(|insn| insn.addr == addr)
            .ok_or(ErrorKind::FailedSplitingRegion(region_id, addr))?;

        if split_index == 0 {
            return Ok(region_id);
        }
        // …rest of the function unchanged…
```

- [ ] **Step 3: Build + test**

```bash
cargo test -p cfg
cargo doc -p cfg --no-deps
```
Expected: PASS — no behavioural change. Doc render still describes the swap-second-region trick.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/split.rs
git commit -m "docs(cfg): merge split_region's redundant inline comment into the doc comment"
```

---

## Task 6: Pin `split_region` no-op contract via test

**Problem:** `split_region(region_id, addr)` short-circuits to `Ok(region_id)` (no graph mutation, no map mutation) when `addr == region.start_addr` — the `if split_index == 0` branch. This is unreachable from the production `Builder::explore` path (which checks `region.start_addr == addr` first and skips `split_region` entirely), but is reachable via the `test_api` direct call. The contract is currently undocumented in the test suite.

**Files:**
- Modify: [crates/cfg/tests/builder_split_region.rs](crates/cfg/tests/builder_split_region.rs) — append one test

- [ ] **Step 1: Inspect the existing test file for conventions**

Read [crates/cfg/tests/builder_split_region.rs](crates/cfg/tests/builder_split_region.rs). Identify:
- The exact import set at the top (`mod common;`, `use common::{…};`, `use cfg::test_api::*;`).
- The test_api functions used to construct a builder, add a region, and split it.
- The pattern for asserting node/edge counts (likely via `petgraph::visit::IntoNodeReferences` / `IntoEdgeReferences` on the underlying graph from `test_api::graph(b)`).

- [ ] **Step 2: Append the no-op test**

```rust
/// Pinned contract: `split_region(region_id, region.start_addr)` is a no-op —
/// returns `region_id` unchanged, mutates neither the region graph nor the
/// `start_addr_to_region_id` map.
#[test]
fn split_region_at_existing_start_is_a_no_op() {
    use petgraph::visit::IntoEdgeReferences;

    let mut b = common::make_builder(0x1000);
    let region = common::make_region(&[(0x1000, 0), (0x1000, 1), (0x1004, 0)]);
    let region_start = region.start_addr;
    let rid = cfg::test_api::add_region(&mut b, region).unwrap();

    let nodes_before = cfg::test_api::graph(&b).node_count();
    let edges_before = cfg::test_api::graph(&b).edge_references().count();
    let map_len_before = cfg::test_api::start_addr_to_region_id(&b).len();

    let returned = cfg::test_api::split_region(&mut b, rid, region_start).unwrap();

    assert_eq!(returned, rid, "no-op split must return the input region id");
    assert_eq!(
        cfg::test_api::graph(&b).node_count(),
        nodes_before,
        "no-op split must not add a region"
    );
    assert_eq!(
        cfg::test_api::graph(&b).edge_references().count(),
        edges_before,
        "no-op split must not add an edge"
    );
    assert_eq!(
        cfg::test_api::start_addr_to_region_id(&b).len(),
        map_len_before,
        "no-op split must not insert into start_addr_to_region_id"
    );
}
```

If the existing file doesn't already `use petgraph::visit::IntoEdgeReferences;` at the module level, the local `use` inside the test is sufficient.

If the existing file doesn't already import `common::make_region`, confirm `make_region` is exported (it is — see `crates/cfg/tests/common/synthetic.rs:102`).

- [ ] **Step 3: Run the new test**

Run: `cargo test -p cfg --test builder_split_region split_region_at_existing_start_is_a_no_op`
Expected: PASS.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p cfg`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/tests/builder_split_region.rs
git commit -m "test(cfg): pin split_region(region, region.start_addr) no-op contract"
```

---

## Task 7: Final sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Lib-level pedantic must be empty**

Run: `cargo clippy -p cfg --no-deps -- -W clippy::pedantic 2>&1 | grep -E 'warning:'`
Expected: empty (or, at most, warnings on test code that fall outside `--lib`; the lib must be clean).

- [ ] **Step 2: All-targets pedantic must be unchanged from round-4 baseline**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -W clippy::pedantic 2>&1 | tail -40`
Expected: any remaining warnings are pre-existing test-side noise that round 4 deemed out of scope. Compare against the baseline (round-4 final state) — there should be **no new** warnings.

- [ ] **Step 3: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no new warnings.

- [ ] **Step 4: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: cfg-only strict lint**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Workspace strict lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS (or pre-existing unrelated lints in other crates, unchanged from baseline).

- [ ] **Step 7: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — same content as before this round (Task 1's arithmetic refactor and Tasks 2/3's cast cleanups are all observationally equivalent to the previous behaviour).

---

## Out of scope (considered, rejected, or deferred)

- **Sequential-decode-into-region-interior asymmetry.** `RegionBuilder::process_insn` checks `start_addr_to_region_id.get(&addr)` (start-only lookup), not `find_region_containing_addr(addr)` (interior-aware lookup). If sequential decoding ever lands inside the *interior* of an already-built region (rather than at its start), the build loop would create overlapping regions. In practice this requires variable-length-instruction overlap on x86 (e.g. RegionA's 4-byte insn at 0x1018 lifts to pcode at 0x1018, then a later RegionB sequentially decodes 0x1014→0x1018 and bypasses 0x1018's start because it didn't go through the byte boundary). Speculative; no test binary in `binary_tests/` has triggered it. Defer to a dedicated round if/when an instance is found in the wild.
- **`vn_to_name_non_register` REGISTER arm is unreachable.** Round 4 already considered and rejected: defence in depth.
- **`Cfg::sleigh` / `Cfg::graph` / `Cfg::entry` field privatisation.** Rejected in rounds 1–4: `analyzer` reads them directly.
- **Consolidating the three `test_api` re-export tiers.** Round 4 rejected; same call.
- **Moving `region_builder.rs::test_api` into a sibling file.** Round 3 considered and rejected; round 4 reaffirmed; same call.
- **Replacing `format!("{e:?}")` in `GenericSleighError` with `Display` propagation of `R::Err`.** Round 1 rejected; same call.
- **`OptionsBuilder::set_function_max_size(0)` rejection.** Round 3 rejected: `0` is degenerate but legal.
- **Defending against `lift_res.insns.is_empty()` and `lift_res.machine_insn_len == 0`.** Round 3 rejected: rsleigh's contract.
- **Erroring on duplicate `start_addr_to_region_id` insert in `add_region`.** Round 4 rejected: changes test_api consumer behaviour for unclear gain.
- **Renaming `is_branch_tail_call_nocheck` → `branch_target_is_outside_function`.** Round 4 rejected: rename touches existing tests for unclear gain.
- **Tightening `is_branch_tail_call_nocheck`'s saturating-add comment.** Considered for round 5; the comment is verbose but accurate, and shortening risks losing the boundary-case explanation. Skip.
- **Per-dump `regs` caching in `CfgDotDumper`.** Round 4 considered; not hot in profiling. Skip until/unless dot rendering becomes a bottleneck.
