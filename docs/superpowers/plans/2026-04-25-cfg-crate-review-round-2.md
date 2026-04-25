# CFG Crate Review — Round 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Second-round cleanup of the `cfg` crate after round 1's structural fixes — fix one real perf bug (per-vn `Sleigh::regs()` FFI roundtrip during DOT dump), one dead clone on the success path (`node.clone()` inside `ok_or`), DRY a duplicated branch arm, drop redundant struct rebuilds and shadowed bindings, and correct two stale comments. No public API changes except the internal payload of `ErrorKind::EmptyRegion`.

**Architecture:** Each task is a self-contained, independently-committable change. External usage is unchanged: `analyzer` reads `cfg::{Cfg, Builder, OptionsBuilder, RegionEdgeKind, RegionId, Error, ErrorKind}` plus the `Cfg::dot_dumper()` accessor — only one external matcher (`analyzer::error.rs:69`'s `bridge_error!` on `cfg::Error`) ever observes the error variants and it doesn't pattern-match on payloads. The one in-crate matcher on `ErrorKind::EmptyRegion(_)` (in `tests/builder_add_region.rs:34`) uses `_`, so the payload-type change is source-compatible.

**Tech Stack:** Rust, `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Open questions for the reviewer before execution

Each changes your mind about a task below. Pick one per group; defaults are highlighted.

**Q1 — `ErrorKind::EmptyRegion` payload (Task 1):** Currently `EmptyRegion(Region)`. Region is heavy (`Vec<RegionInstruction>` inside) and the only useful diagnostic is the start address.
  - **(A)** `EmptyRegion(PcodeInsnAddr)` — the start_addr the builder was about to publish. **Default choice.** Cheap to clone, useful in error messages, matches the diagnostic intent.
  - **(B)** `EmptyRegion(NodeIndex)` — the region id. Slightly less informative (no machine address) but unambiguous within a CFG.
  - **(C)** Leave the variant as-is, only switch the `dot.rs` site to `ok_or_else`. Keeps the heavy variant but removes the eager clone.

  **Reviewer action:** confirm (A). If (B), ditto with NodeIndex throughout.

**Q2 — Cache `Sleigh::regs()` once per DOT dump (Task 2):** `vn_to_name` calls `self.sleigh.regs()` every time it sees a `REGISTER` varnode. `regs()` is an FFI roundtrip + `BiHashMap` rebuild. In `dump_as_dot` we call `vn_to_name` for every input+output of every instruction in every region, on both the node-label path and (transitively, none — but called once per insn × ~3 vns/insn average). For a 100-region CFG with 5 insns/region, that's ~1500 FFI roundtrips per dump.
  - **(A)** Pre-fetch `SleighRegs` once at the top of `dump_as_dot`, thread it down via a private `vn_to_name_with_regs(&SleighRegs, &Vn)` helper. Public `vn_to_name` keeps its current signature for the test_api forwarder. **Default choice.**
  - **(B)** Add a `LazyCell<SleighRegs>`-style cache field on `Cfg` itself. Wider blast radius (mut access during a `&self` dump dumper, OnceLock or RefCell needed), more invasive.
  - **(C)** Defer the optimization — file an issue, ship round 2 without it, profile first.

  **Reviewer action:** confirm (A). If `regs()` turns out to be cheap (cached internally by Sleigh — verify in step 1 of Task 2 before editing), abandon and move on to (C).

**Q3 — `process_new_insn` Branch arm DRY rewrite (Task 3):** Current code duplicates `finish_current_region` + `Ok(FinishedProcessing)` across the tail-call and non-tail-call branches. The tail-call branch passes `true`, the non-tail-call branch passes `false` and additionally enqueues the target.
  - **(A)** Compute `is_tail_call` once, call `finish_current_region(is_tail_call)?`, conditionally push to the work queue, single `Ok(...)`. **Default choice.**
  - **(B)** Leave alone — keeps the two semantically-distinct cases visually separate.

  **Reviewer action:** confirm (A).

**Q4 — Helper for the "advance past current pcode insn" calculation (Task 7):** Two sites compute this, with related but slightly different semantics:
  - `process_new_insn` CondBranch false case (`region_builder.rs:199-211`): "what's the address of the next pcode op? — could stay in same machine insn or move to the next."
  - `build` end-of-inner-loop step (`region_builder.rs:311-316`): "what's the address right after the current machine insn?" — always advances to the next machine insn.

  Both reduce to the same expression when the current address is the last pcode op of the machine insn. A unifying helper `fn next_pcode_addr(addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> PcodeInsnAddr` covers both.
  - **(A)** Add the helper, use at both sites. **Default choice.**
  - **(B)** Free function in `region_builder.rs`. Slightly looser binding.
  - **(C)** Leave inline; the two sites have similar-but-not-identical code, and DRYing them risks hiding the difference.

  **Reviewer action:** confirm (A).

Assume defaults unless reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Shrink `ErrorKind::EmptyRegion` payload from `Region` to `PcodeInsnAddr`

The variant currently carries a whole `Region`. In `crates/cfg/src/cfg/dot.rs:83` the construction is:

```rust
.ok_or(ErrorKind::EmptyRegion(node.clone()))?
```

`Option::ok_or` evaluates its argument *eagerly*, so `node.clone()` runs on every successful dump iteration even though the option is always `Some` (any region in the graph has at least one instruction — `add_region` enforces it). Each clone is `Vec<RegionInstruction>` worth of allocation.

Fixing the eager evaluation alone would still leave the variant fat. Switching to `PcodeInsnAddr` shrinks the error and keeps the diagnostic information that actually helps the user (where the empty region was about to be added).

**Files:**
- Modify: [crates/cfg/src/error.rs:12-13](crates/cfg/src/error.rs#L12-L13)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:65-68](crates/cfg/src/cfg/builder/mod.rs#L65-L68)
- Modify: [crates/cfg/src/cfg/dot.rs:80-86](crates/cfg/src/cfg/dot.rs#L80-L86)
- Modify: [crates/cfg/tests/builder_add_region.rs:30-35](crates/cfg/tests/builder_add_region.rs#L30-L35)

- [ ] **Step 1: Change the variant**

In `crates/cfg/src/error.rs`, replace:

```rust
#[error("empty region {0:?}")]
EmptyRegion(Region),
```

with:

```rust
#[error("region at {0:?} has no instructions")]
EmptyRegion(crate::cfg::PcodeInsnAddr),
```

Drop the `Region` import if it becomes unused: at the top of the file the import line is `use crate::cfg::{PcodeInsnAddr, Region};` — `Region` should now be removable. Confirm by `grep -n "\bRegion\b" crates/cfg/src/error.rs` after the edit.

- [ ] **Step 2: Update `Builder::add_region`**

In `crates/cfg/src/cfg/builder/mod.rs`, change:

```rust
pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
    if region.insns.is_empty() {
        return Err(ErrorKind::EmptyRegion(region).into());
    }
    ...
```

to:

```rust
pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
    if region.insns.is_empty() {
        return Err(ErrorKind::EmptyRegion(region.start_addr).into());
    }
    ...
```

- [ ] **Step 3: Update `dump_as_dot`**

In `crates/cfg/src/cfg/dot.rs`, replace the `first_insn_index` block (`.ok_or(ErrorKind::EmptyRegion(node.clone()))`) with:

```rust
let first_insn_index = node
    .insns
    .first()
    .ok_or_else(|| ErrorKind::EmptyRegion(node.start_addr))?
    .addr
    .insn_index;
```

Note: also switching to `ok_or_else` is no longer strictly required (the new payload is `Copy`), but keeping it keeps the lazy-construction discipline consistent with the rest of the file.

- [ ] **Step 4: Update the existing matcher in `builder_add_region.rs`**

The match in [tests/builder_add_region.rs:34](crates/cfg/tests/builder_add_region.rs#L34) is `matches!(err.kind(), ErrorKind::EmptyRegion(_))` — it's already `_` and remains valid. Verify by re-reading the test:

Run: `grep -n "EmptyRegion" crates/cfg/tests/builder_add_region.rs`
Expected: `34: ErrorKind::EmptyRegion(_)` (or similar) — no change needed.

If the test happens to pattern-match on the inner Region in some other way (it doesn't today), that match must be updated to expect a `PcodeInsnAddr`.

- [ ] **Step 5: Build + test + workspace check**

Run:
```bash
cargo test -p cfg && cargo check --workspace
```
Expected: PASS — including all dot dumper tests in `tests/dot_dumper.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/src/error.rs crates/cfg/src/cfg/builder/mod.rs crates/cfg/src/cfg/dot.rs
git commit -m "refactor(cfg): shrink ErrorKind::EmptyRegion payload from Region to PcodeInsnAddr; lazy in dot dumper"
```

---

## Task 2: Cache `Sleigh::regs()` once per DOT dump instead of per varnode

`vn_to_name` calls `self.sleigh.regs()` for each `VnSpace::REGISTER` varnode. `Sleigh::regs()` is an FFI roundtrip into the Sleigh C++ bindings: it allocates a regs-info handle, iterates an FFI iterator to populate a `BiHashMap`, then drops the handle (`/home/mike/Desktop/rsleigh/src/lib.rs:240-`). The result is *not* cached internally.

`dump_as_dot` calls `vn_to_name` for every input + output varnode of every instruction in every region. On a 100-region CFG with 5 instructions per region and 3 varnodes per instruction, that's ≈1500 FFI roundtrips per dump. Pre-fetching `SleighRegs` once at the top of `dump_as_dot` collapses that to one.

**First, verify** the optimization is real before changing code:

- [ ] **Step 1: Confirm `regs()` is uncached**

Run: `grep -n "fn regs\b\|cache\|cell" /home/mike/Desktop/rsleigh/src/lib.rs | head -30`

Look at the implementation: it should call `sleigh_bindings_ctx_get_regs_info` and rebuild the `SleighRegs` map every time. If you find a `OnceCell`/`OnceLock`/memoized path, abandon this task and **commit nothing** — note in the round 2 review summary that Q2 fell out via verification.

(Confirmed at plan-write time at `rsleigh/src/lib.rs:240-` — uncached. Confirm again before editing in case rsleigh has changed.)

- [ ] **Step 2: Add a private `vn_to_name_with_regs` helper**

In `crates/cfg/src/cfg/dot.rs`, replace the existing `vn_to_name` implementation:

```rust
impl<R: rsleigh::MemReader> Cfg<R> {
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(ErrorKind::SleighError)?;
                Ok(regs
                    .vn_to_name(*vn)
                    .ok_or(ErrorKind::InvalidRegVn(*vn))?
                    .to_string())
            }
            rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
            rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
            s => Err(ErrorKind::UnsupportedVnSpaceDisplay(s).into()),
        }
    }
    ...
```

with a thin public form that fetches `regs` per call (still used by the test_api forwarder), and a private form that takes a pre-fetched `&SleighRegs`:

```rust
impl<R: rsleigh::MemReader> Cfg<R> {
    /// Public entry — fetches Sleigh registers per call. Suitable for ad-hoc
    /// callers; the DOT dumper uses [`Self::vn_to_name_with_regs`] instead so
    /// it only fetches once per render.
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        match vn.addr.space {
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(ErrorKind::SleighError)?;
                Self::vn_to_name_with_regs(&regs, vn)
            }
            _ => Self::vn_to_name_with_regs_unchecked(vn),
        }
    }

    /// Render `vn` to a name, using a pre-fetched [`rsleigh::SleighRegs`] for
    /// REGISTER lookups. The non-REGISTER spaces don't need `regs`.
    fn vn_to_name_with_regs(
        regs: &rsleigh::SleighRegs,
        vn: &rsleigh::Vn,
    ) -> Result<String> {
        if vn.addr.space == rsleigh::VnSpace::REGISTER {
            return Ok(regs
                .vn_to_name(*vn)
                .ok_or(ErrorKind::InvalidRegVn(*vn))?
                .to_string());
        }
        Self::vn_to_name_with_regs_unchecked(vn)
    }

    fn vn_to_name_with_regs_unchecked(vn: &rsleigh::Vn) -> Result<String> {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
            rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
            rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
            rsleigh::VnSpace::REGISTER => {
                // Caller error: should have routed through with-regs path.
                Err(ErrorKind::InvalidRegVn(*vn).into())
            }
            s => Err(ErrorKind::UnsupportedVnSpaceDisplay(s).into()),
        }
    }
    ...
```

(If `rsleigh::SleighRegs` is the wrong type name, grep `pub struct SleighRegs` in `../rsleigh/src/lib.rs` — confirmed at plan-write time near line 240. The associated `vn_to_name` method on `SleighRegs` is what `vn_to_name` already calls today.)

- [ ] **Step 3: Use the helper in `dump_as_dot`**

In `crates/cfg/src/cfg/dot.rs::dump_as_dot`, fetch `regs` once at the top:

```rust
fn dump_as_dot(
    &self,
    node_id: Self::Node,
    out: &mut dot::DotEmitter,
    _state: &mut Self::State,
) -> Result<()> {
    use std::fmt::Write;

    let regs = self.0.sleigh.regs().map_err(ErrorKind::SleighError)?;

    let dot_id = node_id.index().to_string();
    let node = self
        .0
        .graph
        .node_weight(node_id)
        .ok_or(ErrorKind::InvalidRegion(node_id))?;
    let first_insn_index = node
        .insns
        .first()
        .ok_or_else(|| ErrorKind::EmptyRegion(node.start_addr))?  // from Task 1
        .addr
        .insn_index;
    let start_addr = node.start_addr.machine_addr.addr;

    let mut label = format!("Instruction(addr={start_addr:#x}, idx={first_insn_index})\n");

    for insn in node.insns.iter() {
        let variables: Vec<String> = insn
            .insn
            .output
            .iter()
            .chain(insn.insn.inputs.iter())
            .map(|vn| Cfg::<R>::vn_to_name_with_regs(&regs, vn))
            .collect::<Result<_>>()?;
        let insn_addr = insn.addr.machine_addr.addr;
        write!(&mut label, "\\l{insn_addr:#x}: {:?}", insn.insn.opcode)
            .map_err(ErrorKind::FormatError)?;
        if !variables.is_empty() {
            write!(&mut label, ", {}", variables.join(", "))
                .map_err(ErrorKind::FormatError)?;
        }
    }
    write!(&mut label, "\\l").map_err(ErrorKind::FormatError)?;
    ...
```

This calls `regs()` exactly once per node rendered. (One could fetch even higher up — once per dumper construction — but `dump_as_dot` is the per-node entry point in `GraphDotDumper`; one-per-node is the cleanest place to amortize without adding state to the dumper.)

A further optimization (one fetch per *whole* dump rather than per node) would require either a stateful dumper field or threading via `Self::State`. For now per-node is enough: ≈100 calls instead of ≈1500 on the example workload. If profiling later shows this is still a bottleneck, change `Self::State` from `()` to `SleighRegs` (the trait already wires `state` through; `_state` becomes `state`). Note this in the doc comment above `vn_to_name_with_regs` so the next reader sees the deferred next step.

- [ ] **Step 4: Build + test**

Run:
```bash
cargo test -p cfg && cargo check --workspace
```
Expected: PASS. The tests in `tests/dot_dumper.rs` validate the DOT output text, so any regressions in name resolution will surface.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "perf(cfg): fetch Sleigh::regs() once per dot-dump node instead of per varnode"
```

---

## Task 3: DRY the `Branch` arm of `process_new_insn`

The current `Branch` arm:

```rust
rsleigh::Opcode::Branch => {
    let branch_target_addr = self.decode_branch_target(insn.inputs[0], addr)?;
    if self.is_branch_tail_call(branch_target_addr)? {
        // The tail call marks the end of control flow for this specific path.
        let _region = self.finish_current_region(true)?;
        Ok(ProcessInsnRes::FinishedProcessing)
    } else {
        // We reached the end of the current bb but we know the next address to jump to so enqueue it
        let region = self.finish_current_region(false)?;
        self.builder
            .work_queue
            .push((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
        Ok(ProcessInsnRes::FinishedProcessing)
    }
}
```

duplicates the `finish_current_region` call and the `Ok(...)` return. The two arms differ only in (a) the bool passed to `finish_current_region`, and (b) whether the work queue is updated.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:172-187](crates/cfg/src/cfg/builder/region_builder.rs#L172-L187)

- [ ] **Step 1: Rewrite the arm**

Replace the `Branch` arm with:

```rust
rsleigh::Opcode::Branch => {
    let branch_target_addr = self.decode_branch_target(insn.inputs[0], addr)?;
    let is_tail_call = self.is_branch_tail_call(branch_target_addr)?;
    let region = self.finish_current_region(is_tail_call)?;
    if !is_tail_call {
        // Not a tail call — enqueue the target so the builder explores it next.
        self.builder
            .work_queue
            .push((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
    }
    Ok(ProcessInsnRes::FinishedProcessing)
}
```

- [ ] **Step 2: Run cfg tests**

Run: `cargo test -p cfg`
Expected: PASS — the `region_builder_process` and `region_builder_tail_call` test files exercise both arms.

- [ ] **Step 3: Workspace check**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): unify Branch arm of process_new_insn around a single finish_current_region call"
```

---

## Task 4: Fix stale comments in `split_region`

Two comments inside `split_region` are wrong/confusing. The code itself is correct; only the comments mislead.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/split.rs:36](crates/cfg/src/cfg/builder/split.rs#L36)
- Modify: [crates/cfg/src/cfg/builder/split.rs:76-79](crates/cfg/src/cfg/builder/split.rs#L76-L79)

- [ ] **Step 1: Fix typo at L36**

Change line 36 from:

```rust
// 4. The parent of the popped value from that called the split should also point to the second region
```

to:

```rust
// 4. The parent of the popped work-queue item that triggered the split should also point to the second region
```

- [ ] **Step 2: Fix the parent-edge rewrite block**

The block currently reads:

```rust
// Move the parent edges to be in the first region instead of the first one
for (edge_id, parent_id, edge_data) in parent_edges {
    // re-add edge from second_region to the child
    self.graph.add_edge(parent_id, first_region, edge_data);

    // remove the original edge
    self.graph.remove_edge(edge_id);
}
```

Both inline comments are wrong:
- "in the first region instead of the first one" is a typo for "instead of the second one".
- "re-add edge from second_region to the child" actually adds an edge from `parent_id` to `first_region`.

Change to:

```rust
// Re-target each incoming edge from the original (now second) region onto
// the freshly-created first region, then drop the original edge.
for (edge_id, parent_id, edge_data) in parent_edges {
    self.graph.add_edge(parent_id, first_region, edge_data);
    self.graph.remove_edge(edge_id);
}
```

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg`
Expected: PASS (no behavior change).

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/split.rs
git commit -m "docs(cfg): correct stale comments in split_region"
```

---

## Task 5: Drop redundant `cur_addr` rebuild in `RegionBuilder::build`

Inside the inner per-pcode-insn loop:

```rust
for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
    cur_addr = PcodeInsnAddr {
        machine_addr: cur_addr.machine_addr,
        insn_index: i as u64,
    };
    ...
}
```

`cur_addr` is rebuilt by re-stating the unchanged `machine_addr`. `PcodeInsnAddr` is `Copy`, so `cur_addr.insn_index = i as u64;` is a single-field write that conveys the intent more directly.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:300-304](crates/cfg/src/cfg/builder/region_builder.rs#L300-L304)

- [ ] **Step 1: Replace the struct rebuild**

Change:

```rust
for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
    cur_addr = PcodeInsnAddr {
        machine_addr: cur_addr.machine_addr,
        insn_index: i as u64,
    };
    let res = self.process_insn(insn, cur_addr, &lift_res)?;
    ...
```

to:

```rust
for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
    cur_addr.insn_index = i as u64;
    let res = self.process_insn(insn, cur_addr, &lift_res)?;
    ...
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): assign cur_addr.insn_index in place instead of rebuilding the struct"
```

---

## Task 6: Tidy `process_insn` get-then-deref pattern + drop redundant alias in `split_region`

Two micro-readability cleanups, folded into one task since neither is large enough to stand alone.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:259-269](crates/cfg/src/cfg/builder/region_builder.rs#L259-L269)
- Modify: [crates/cfg/src/cfg/builder/split.rs:53-62](crates/cfg/src/cfg/builder/split.rs#L53-L62)

- [ ] **Step 1: Bind `region_id` by-copy in `process_insn`**

Change:

```rust
let existing_region = self.builder.start_addr_to_region_id.get(&addr);
// If we already processed the instruction - we fell through to an already processed region
if let Some(region_id) = existing_region {
    let region_id = *region_id;
    // The parent region falls through to this region
    let region = self.finish_current_region(false)?;
    self.builder
        .graph
        .add_edge(region, region_id, RegionEdgeKind::Fallthrough);
    return Ok(ProcessInsnRes::FinishedProcessing);
}
self.process_new_insn(insn, addr, lift_res)
```

to:

```rust
// If `addr` is the start of an already-explored region, the current region
// fell through to it: finalise the current region and add a Fallthrough edge.
if let Some(&existing_region_id) = self.builder.start_addr_to_region_id.get(&addr) {
    let region = self.finish_current_region(false)?;
    self.builder
        .graph
        .add_edge(region, existing_region_id, RegionEdgeKind::Fallthrough);
    return Ok(ProcessInsnRes::FinishedProcessing);
}
self.process_new_insn(insn, addr, lift_res)
```

- [ ] **Step 2: Drop the redundant `second_region_id` alias in `split_region`**

In `crates/cfg/src/cfg/builder/split.rs`, the local `let second_region_id = region_id;` introduces an alias that adds no clarity (the function comment already establishes the mental model: `region_id` IS the second region after the swap). Remove it and use `region_id` directly.

Before (lines around 53-62):

```rust
let second_region_insns = second_region.insns.split_off(split_index);
let first_region_insns = std::mem::replace(&mut second_region.insns, second_region_insns);
let second_region_id = region_id;
let first_region_start_addr = second_region.start_addr;
second_region.start_addr = addr;

// We need to update the region location in the mapping to get the correct one when accessed later
self.start_addr_to_region_id
    .insert(second_region.start_addr, second_region_id);
```

After:

```rust
// `split_off(at)` returns elements at-and-after `at`, leaving elements before `at`
// in `second_region.insns`. Swap them so `second_region` keeps its identity but
// owns the second half of the original instruction stream.
let upper = second_region.insns.split_off(split_index);
let first_region_insns = std::mem::replace(&mut second_region.insns, upper);
let first_region_start_addr = second_region.start_addr;
second_region.start_addr = addr;

// Re-index the (now-second) region under its new start address.
self.start_addr_to_region_id.insert(addr, region_id);
```

Then update the rest of the function: every later reference to `second_region_id` becomes `region_id`. Confirm no occurrences remain:

```bash
grep -n "second_region_id" crates/cfg/src/cfg/builder/split.rs
```

Expected: empty output.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS — including `tests/builder_split_region.rs`.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src
git commit -m "refactor(cfg): tidy process_insn binding and drop redundant second_region_id alias"
```

---

## Task 7: Extract `next_pcode_addr` helper

Two sites compute "the pcode address that comes right after `cur_addr`":

1. `process_new_insn` CondBranch false case ([region_builder.rs:199-211](crates/cfg/src/cfg/builder/region_builder.rs#L199-L211)) — branches on whether `cur_addr` is the last pcode insn of the machine insn.
2. `build` end-of-inner-loop ([region_builder.rs:311-316](crates/cfg/src/cfg/builder/region_builder.rs#L311-L316)) — unconditionally advances to the next machine insn.

Site 2's `cur_addr` always satisfies `cur_addr.insn_index == lift_res.insns.len() - 1` (the inner `for` ran to completion), so it's the same expression as site 1's "last-pcode-insn" branch. A single helper covers both.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:199-211](crates/cfg/src/cfg/builder/region_builder.rs#L199-L211)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:311-316](crates/cfg/src/cfg/builder/region_builder.rs#L311-L316)

- [ ] **Step 1: Add the helper**

In `crates/cfg/src/cfg/builder/region_builder.rs`, add a free function near the top of the file (after the imports, before `pub enum ProcessInsnRes`):

```rust
/// Returns the [`PcodeInsnAddr`] that comes immediately after `addr` within
/// the lifted machine instruction `lift_res`.
///
/// - If `addr.insn_index + 1` is still within `lift_res.insns`, returns the
///   same machine address with `insn_index` advanced by one.
/// - Otherwise returns the start (`insn_index = 0`) of the *next* machine
///   instruction.
fn next_pcode_addr(addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> PcodeInsnAddr {
    if (addr.insn_index as usize) + 1 < lift_res.insns.len() {
        PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        }
    } else {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr {
                addr: addr.machine_addr.addr + lift_res.machine_insn_len as u64,
            },
            insn_index: 0,
        }
    }
}
```

(Note the `<` predicate: clearer than `+ 1 == len`, and equivalent.)

- [ ] **Step 2: Use it in `process_new_insn` CondBranch**

Replace the `next_insn_addr = if … { … } else { … };` block in the CondBranch arm (around lines 199-211) with:

```rust
let next_insn_addr = next_pcode_addr(addr, lift_res);
```

Keep the existing comment context — drop the now-stale "The false case requires calculation of the next instruction (is it in the current pcode instr or the next one)" comment since the helper name + doc make it self-evident.

- [ ] **Step 3: Use it in `build` end-of-inner-loop**

Replace the post-loop assignment (around lines 311-316):

```rust
cur_addr = PcodeInsnAddr {
    machine_addr: MachineInsnAddr {
        addr: cur_addr.machine_addr.addr + (lift_res.machine_insn_len as u64),
    },
    insn_index: 0,
};
```

with:

```rust
cur_addr = next_pcode_addr(cur_addr, &lift_res);
```

This works because the inner `for` ran to completion ⇒ `cur_addr.insn_index == lift_res.insns.len() - 1` ⇒ the helper falls into its else-branch and advances to the next machine insn.

- [ ] **Step 4: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS — `tests/region_builder_process.rs` exercises the CondBranch false case and `tests/build_end_to_end.rs` exercises full multi-machine-insn region builds.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): extract next_pcode_addr helper used by CondBranch and inner-loop advance"
```

---

## Task 8: Final sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no new warnings.

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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced, matching pre-refactor behavior. Open `cfg.html` and confirm a few random regions still show their register names (Task 2 regression check).

---

## Out of scope (considered, rejected, or deferred)

- **Caching `Sleigh::regs()` once per *whole* dump** (not just per node): cleaner via `Self::State = SleighRegs`, but adds a state field and a `Default` constructor. Deferred — a 100×–1000× win is in Task 2; pushing further only matters if a profiler says so.
- **`vn_to_name` for `RAM`/`UNIQUE` could include the region/insn context for debugging**: out of scope for a code-quality pass.
- **Overflow guards on `addr.machine_addr.addr + lift_res.machine_insn_len as u64`** (inside the new `next_pcode_addr` helper from Task 7): in practice machine instructions are ≤16 bytes on x86 and ≤4 on aarch64; the only realistic overflow scenario is malformed input that would already trip elsewhere. YAGNI.
- **Overflow guard on `addr.insn_index + 1` (region_builder.rs:209)**: pcode index is bounded by `lift_res.insns.len()` which is small. YAGNI.
- **Removing the `_state: &mut Self::State` parameter from `dump_as_dot`**: required by the `GraphDotDumper` trait; can't drop it.
- **Replacing `dot_id = node_id.index().to_string()` with a stack `itoa::Buffer`**: `dot::DotEmitter::node` takes `&str`. Saving one `String` allocation per node is dwarfed by the per-node `format!` for the label. Skip.
- **Moving `RegionInstruction::insn` behind a `Box`**: same conclusion as round 1 — `rsleigh::Insn` is small and inline `Vec` storage is preferable.
- **Touching `Cfg::sleigh` / `Cfg::graph` / `Cfg::entry` visibility**: same as round 1 — analyzer reads them directly; rejected.
- **Removing the `EmptyRegion` check entirely from `dump_as_dot`** (since `add_region` enforces non-emptiness): the dumper takes a `&Cfg` it didn't necessarily build, so the invariant is enforced "elsewhere" — keeping the runtime check is cheap and produces a clear error rather than a panic if anyone constructs a `Cfg` without going through `Builder::build`. Keep.
