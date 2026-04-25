# CFG Crate Review — Round 4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round 4 cleanup of the `cfg` crate. After rounds 1–3 the crate is structurally sound and lint-clean at default level. Round 4 closes two remaining correctness gaps (machine-address overflow in `next_pcode_addr` and unguarded `insn.inputs[0]` indexing on `Branch`/`CondBranch`) and absorbs four small style/readability nits surfaced by `clippy::pedantic`.

**Architecture:** Each task is a self-contained, independently-committable change. The two correctness fixes add new `ErrorKind` variants and propagate them via the existing `Result` plumbing. No public-API change — `Builder`, `Cfg`, `OptionsBuilder`, `RegionEdgeKind`, `RegionId`, the four `Cfg::*` accessors, and `Region::{start_addr, insns, ends_with_tail_call}` are unchanged.

**Tech Stack:** Rust, `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Open questions for the reviewer before execution

Each one materially changes a task below. Pick one per group; defaults are noted.

**Q1 — `next_pcode_addr` machine-address overflow (Task 1):** `next_pcode_addr` does `addr.machine_addr.addr + lift_res.machine_insn_len as u64` to compute the start of the next machine instruction when the current pcode index is the last in the lifted sequence. At the very top of the address space (e.g. `addr = 0xFFFFFFFFFFFFFFF8`, `machine_insn_len = 16`) this wraps silently to `0x8`, and the next iteration of `RegionBuilder::build` calls `lift_one(0x8)` — producing a garbage CFG with no diagnostic. The function currently returns `PcodeInsnAddr` infallibly; fixing it requires changing its signature.
  - **(A)** Make `next_pcode_addr` return `Result<PcodeInsnAddr>`, use `checked_add` for the machine-address arithmetic, add a new `ErrorKind::MachineAddrOverflow(PcodeInsnAddr)` variant. Three call sites propagate via `?`. **Default choice.** Catches the bug at the precise overflow site with a typed error.
  - **(B)** Saturate to `u64::MAX` and rely on the next `lift_one` call failing. Hides the overflow; couples our correctness to `rsleigh`'s response to address `u64::MAX`.
  - **(C)** Treat overflow as the end of the function (synthesize an `InvalidTailCall` or similar). Re-uses an existing variant but stretches its meaning.

  **Reviewer action:** confirm (A).

**Q2 — Unguarded `insn.inputs[0]` panic (Task 2):** In `process_new_insn`, the `Branch` and `CondBranch` arms read `insn.inputs[0]` with bracket indexing. If `rsleigh` ever produces a `Branch`/`CondBranch` with empty `inputs`, this panics. The CLAUDE.md memory rule (*"errors must propagate via Result; never panic/unwrap/expect in normal error paths"*) makes this a violation regardless of whether the panic is reachable in practice — the `inputs[0]` form is functionally an unwrap.
  - **(A)** Add `ErrorKind::MissingBranchTarget(PcodeInsnAddr)`; replace `insn.inputs[0]` with `*insn.inputs.first().ok_or(ErrorKind::MissingBranchTarget(addr))?`. **Default choice.**
  - **(B)** Document `rsleigh`'s "branch-with-non-empty-inputs" contract via a `// SAFETY:` comment and leave the indexing. Cheaper but doesn't satisfy the no-panic rule.

  **Reviewer action:** confirm (A).

**Q3 — Bundle the four pedantic lints (Tasks 3–6):** Lib-only `clippy::pedantic` reports four warnings, all trivially fixable: `if_not_else` in `Builder::explore`, an elidable `'a` lifetime in `impl GraphDotDumper for CfgDotDumper`, an uninlined `format!("{:?}", e)` in `RegionBuilder::build`, and a missing pair of doc backticks (`insn_index`, `BTreeMap`).
  - **(A)** Fix all four in this plan as separate one-step tasks. Keeps the lint surface clean for next round. **Default choice.**
  - **(B)** Drop them — pure cosmetics.

  **Reviewer action:** confirm (A) (or skip Tasks 3–6).

**Q4 — Test-side pedantic lints (out of scope?):** `cargo clippy -p cfg --all-targets -- -W clippy::pedantic` flags additional warnings in `tests/` (uninlined format args in panic strings, `i8 as u8` and `i64 as u64` casts, doc-markdown). The casts inside `tests/common/synthetic.rs` and `tests/region_builder_decode.rs` are intentional (synthesizing two's-complement byte sequences and signed branch offsets); they get `#[allow(clippy::cast_sign_loss)]` with a one-line justification rather than a rewrite. The format-args / lifetime / doc-markdown lints are mechanical fixes.
  - **(A)** Skip them — out of scope.
  - **(B)** Add a Task 7 covering the test-side lints. **CONFIRMED.**

Reviewer's choices (this round): **Q1 = A, Q2 = A, Q3 = A, Q4 = B.**

---

## Task 1: Reject `next_pcode_addr` machine-address overflow

`next_pcode_addr(addr, lift_res)` advances to either the next pcode index within the same machine instruction or to the start of the next machine instruction. The latter computes `addr.machine_addr.addr + lift_res.machine_insn_len as u64` with no overflow check. At the top of the 64-bit address space this wraps and produces a wildly-wrong successor address — the next `lift_one` call then operates on that wrapped address, generating a malformed CFG with no diagnostic.

The fix: thread overflow back as a typed error. `next_pcode_addr` becomes fallible. Three call sites need `?` propagation.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:9-30](crates/cfg/src/cfg/builder/region_builder.rs#L9-L30) (signature + body)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:230-237](crates/cfg/src/cfg/builder/region_builder.rs#L230-L237) (CondBranch call site — `next_pcode_addr` for the false case)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:323-326](crates/cfg/src/cfg/builder/region_builder.rs#L323-L326) (build-loop call site — advancing past the current machine insn)
- Modify: [crates/cfg/src/error.rs:5-44](crates/cfg/src/error.rs#L5-L44) (new variant)
- Test: `crates/cfg/tests/region_builder_decode.rs` (add a unit test) — uses the existing `fake_lift_res` helper

- [ ] **Step 1: Add the error variant**

In `crates/cfg/src/error.rs`, add this variant (alphabetic order is not preserved in this enum; place it under `InvalidTailCall` to keep similarly-themed errors adjacent):

```rust
#[error("machine-address overflow advancing past pcode addr {0:?}")]
MachineAddrOverflow(PcodeInsnAddr),
```

- [ ] **Step 2: Add a failing test**

Append to `crates/cfg/tests/region_builder_decode.rs`. The existing `fake_lift_res(n)` helper in this file already builds a `rsleigh::LiftRes` with a synthetic `machine_insn_len`; we just need a high-address invocation. If `fake_lift_res` does *not* expose `machine_insn_len` configuration, extend it with a 2-arg overload `fake_lift_res_with_len(insns: usize, machine_insn_len: u8)` first — read the helper's body to confirm.

```rust
/// Pinned contract: advancing past the last pcode op of the machine insn
/// at the very top of the address space returns
/// `ErrorKind::MachineAddrOverflow` rather than silently wrapping.
#[test]
fn next_pcode_addr_machine_address_overflow_errors() {
    use cfg::test_api;

    // 1 pcode op, machine_insn_len = 16. The current addr is the last (and only)
    // pcode index, so `next_pcode_addr` advances by `machine_insn_len`. Place
    // the machine address near the top of the u64 range so the addition wraps.
    let lift = fake_lift_res_with_len(1, 16);
    let addr = test_api::PcodeInsnAddr {
        machine_addr: test_api::MachineInsnAddr {
            addr: u64::MAX - 8,
        },
        insn_index: 0,
    };
    let err = test_api::next_pcode_addr(addr, &lift).unwrap_err();
    assert!(
        matches!(err.kind(), cfg::ErrorKind::MachineAddrOverflow(_)),
        "expected MachineAddrOverflow; got {:?}",
        err.kind()
    );
}
```

If `next_pcode_addr` is *not* yet exposed via `test_api`, add a one-line forwarder. Read `crates/cfg/src/cfg/builder/region_builder.rs::test_api` and `crates/cfg/src/test_api.rs` to confirm. If missing, add to `region_builder.rs::test_api`:

```rust
/// # Errors
/// Returns [`ErrorKind::MachineAddrOverflow`] when advancing past the last
/// pcode op overflows the 64-bit machine-address space.
pub fn next_pcode_addr(
    addr: PcodeInsnAddr,
    lift: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    super::next_pcode_addr(addr, lift)
}
```

And re-export from `crates/cfg/src/test_api.rs` if the umbrella `pub use crate::cfg::region_builder_test_api::*;` doesn't already cover it (it currently selects `{ProcessInsnRes, TestRegionBuilder}`):

```rust
#[doc(hidden)]
pub use crate::cfg::region_builder_test_api::next_pcode_addr;
```

- [ ] **Step 3: Run the new test and confirm it fails**

Run: `cargo test -p cfg --test region_builder_decode next_pcode_addr_machine_address_overflow_errors`
Expected: FAIL — current code silently wraps and returns `Ok(PcodeInsnAddr { machine_addr: 0x7, insn_index: 0 })`.

- [ ] **Step 4: Make `next_pcode_addr` fallible**

In `crates/cfg/src/cfg/builder/region_builder.rs`, replace the function with:

```rust
/// Returns the [`PcodeInsnAddr`] that comes immediately after `addr` within
/// the lifted machine instruction `lift_res`.
///
/// - If `addr.insn_index + 1` is still within `lift_res.insns`, returns the
///   same machine address with `insn_index` advanced by one.
/// - Otherwise returns the start (`insn_index = 0`) of the *next* machine
///   instruction.
///
/// # Errors
/// Returns [`ErrorKind::MachineAddrOverflow`] when the current machine
/// address plus `lift_res.machine_insn_len` overflows `u64`.
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

- [ ] **Step 5: Propagate at the CondBranch call site**

In `process_new_insn`'s `CondBranch` arm, change:

```rust
let next_insn_addr = next_pcode_addr(addr, lift_res);
```

to:

```rust
let next_insn_addr = next_pcode_addr(addr, lift_res)?;
```

- [ ] **Step 6: Propagate at the build-loop call site**

In `RegionBuilder::build`, change:

```rust
// We're done exploring a single machine insn, continue to the next one
cur_addr = next_pcode_addr(cur_addr, &lift_res);
```

to:

```rust
// We're done exploring a single machine insn, continue to the next one
cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
```

- [ ] **Step 7: Run new test + full cfg suite**

```bash
cargo test -p cfg --test region_builder_decode
cargo test -p cfg
cargo check --workspace
```
Expected: PASS — including the new test, and all integration tests under `crates/cfg/tests/` (the existing build / decode / process suites do not exercise the overflow path).

- [ ] **Step 8: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/src/error.rs crates/cfg/tests/region_builder_decode.rs crates/cfg/src/test_api.rs
git commit -m "fix(cfg): reject machine-address overflow in next_pcode_addr"
```

---

## Task 2: Replace unguarded `insn.inputs[0]` with checked access

`process_new_insn`'s `Branch` and `CondBranch` arms read `insn.inputs[0]`. If `rsleigh` produces a malformed `Branch`/`CondBranch` with empty `inputs`, the indexer panics. Per the project's "no panic / no unwrap" rule (CLAUDE.md memory), bracket indexing on caller-supplied data must be replaced with a checked access that propagates a typed error.

**Files:**
- Modify: [crates/cfg/src/error.rs](crates/cfg/src/error.rs) (new variant)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:208-238](crates/cfg/src/cfg/builder/region_builder.rs#L208-L238) (Branch + CondBranch arms)
- Test: `crates/cfg/tests/region_builder_process.rs` (add a unit test driving `process_new_insn` with a synthesized empty-inputs Branch)

- [ ] **Step 1: Add the error variant**

In `crates/cfg/src/error.rs`, add (placed under `InvalidBranchTargetVaErr` for thematic adjacency):

```rust
#[error("branch instruction at {0:?} has no target operand")]
MissingBranchTarget(PcodeInsnAddr),
```

- [ ] **Step 2: Add a failing test**

Append to `crates/cfg/tests/region_builder_process.rs`. The file already has helpers for synthesizing pcode `Insn` values (read the top of the file to find the existing `make_insn(opcode, inputs, output)` or equivalent — if absent, build the `rsleigh::Insn` literal directly):

```rust
/// Pinned contract: a `Branch` pcode instruction with empty inputs is
/// rejected with `MissingBranchTarget` rather than panicking on
/// `insn.inputs[0]`.
#[test]
fn process_new_insn_branch_with_empty_inputs_errors() {
    let mut b = common::make_builder(0x1000);
    let mut rb = common::make_region_builder(&mut b, common::addr(0x1000, 0));

    // Lift the single machine insn at 0x1000 to obtain a real `lift_res`.
    let lift = cfg::test_api::sleigh(&b)
        .lift_one(0x1000)
        .expect("lift");

    let bad_insn = rsleigh::Insn {
        opcode: rsleigh::Opcode::Branch,
        inputs: vec![],
        output: None,
    };

    let err = rb
        .process_new_insn(&bad_insn, common::addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(
        matches!(err.kind(), cfg::ErrorKind::MissingBranchTarget(_)),
        "expected MissingBranchTarget; got {:?}",
        err.kind()
    );
}
```

If the exact `rsleigh::Insn` field names differ from `{ opcode, inputs, output }` (read `../rsleigh/src/lib.rs` to confirm), adjust accordingly. If `rsleigh::Insn` does not have a public default-constructor-style API, mirror the construction style used elsewhere in `tests/region_builder_process.rs`.

If the symmetric test is cheap, also add `process_new_insn_condbranch_with_empty_inputs_errors` with the same shape but `Opcode::CondBranch`.

- [ ] **Step 3: Run new test, confirm it fails (panics or wrong error variant)**

Run: `cargo test -p cfg --test region_builder_process process_new_insn_branch_with_empty_inputs_errors`
Expected: FAIL — current code panics with "index out of bounds: the len is 0 but the index is 0".

- [ ] **Step 4: Replace bracket indexing in both arms**

In `crates/cfg/src/cfg/builder/region_builder.rs::process_new_insn`, change the `Branch` arm:

```rust
rsleigh::Opcode::Branch => {
    let target_var = *insn
        .inputs
        .first()
        .ok_or(ErrorKind::MissingBranchTarget(addr))?;
    let branch_target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
    let is_tail_call = self.is_branch_tail_call(branch_target_addr)?;
    let region = self.finish_current_region(is_tail_call)?;
    if !is_tail_call {
        self.builder
            .work_queue
            .push((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
    }
    Ok(ProcessInsnRes::FinishedProcessing)
}
```

…and the `CondBranch` arm:

```rust
rsleigh::Opcode::CondBranch => {
    let target_var = *insn
        .inputs
        .first()
        .ok_or(ErrorKind::MissingBranchTarget(addr))?;
    let target_addr = self.decode_branch_target(target_var, addr, lift_res)?;

    let region = self.finish_current_region(false)?;

    self.builder
        .work_queue
        .push((Some((region, RegionEdgeKind::IfCaseTrue)), target_addr));
    let next_insn_addr = next_pcode_addr(addr, lift_res)?;

    self.builder
        .work_queue
        .push((Some((region, RegionEdgeKind::IfCaseFalse)), next_insn_addr));
    Ok(ProcessInsnRes::FinishedProcessing)
}
```

(Note: `next_pcode_addr` now returns `Result`, so the `?` here is required by Task 1 even if Task 2 lands first. If Task 1 has not landed yet, drop the `?` and add it in Task 1.)

- [ ] **Step 5: Run new test + full cfg suite**

```bash
cargo test -p cfg
cargo check --workspace
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/src/error.rs crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/tests/region_builder_process.rs
git commit -m "fix(cfg): reject branch insn with empty inputs instead of panicking on insn.inputs[0]"
```

---

## Task 3: Flip `if region.start_addr != addr` to positive form (`clippy::if_not_else`)

In `Builder::explore`, the inner `if region.start_addr != addr { … } else { … }` has the negation on the leading branch. Clippy flags this; the positive form reads more naturally.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/mod.rs:115-128](crates/cfg/src/cfg/builder/mod.rs#L115-L128)

- [ ] **Step 1: Flip the branches**

Change:

```rust
if let Some((region_id, region)) = existing_region {
    let (parent_region_id, edge_kind) =
        parent_region.ok_or(ErrorKind::MissingParentEdge)?;
    if region.start_addr != addr {
        // found a jump to the middle of the region. we need to split it.
        let second_region = self.split_region(region_id, addr)?;
        self.graph
            .add_edge(parent_region_id, second_region, edge_kind);
    } else {
        self.graph.add_edge(parent_region_id, region_id, edge_kind);
    }
} else {
    RegionBuilder::new(self, addr, parent_region).build()?;
}
```

to:

```rust
if let Some((region_id, region)) = existing_region {
    let (parent_region_id, edge_kind) =
        parent_region.ok_or(ErrorKind::MissingParentEdge)?;
    if region.start_addr == addr {
        // The address lands on the start of an existing region — wire an edge.
        self.graph.add_edge(parent_region_id, region_id, edge_kind);
    } else {
        // The address lands inside an existing region — split it and wire
        // the edge to the new "second half".
        let second_region = self.split_region(region_id, addr)?;
        self.graph
            .add_edge(parent_region_id, second_region, edge_kind);
    }
} else {
    RegionBuilder::new(self, addr, parent_region).build()?;
}
```

- [ ] **Step 2: Build + test**

```bash
cargo test -p cfg
cargo clippy -p cfg --no-deps
cargo check --workspace
```
Expected: PASS, no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/mod.rs
git commit -m "style(cfg): flip explore's start_addr branch to positive form"
```

---

## Task 4: Elide redundant `'a` lifetime on `impl GraphDotDumper for CfgDotDumper`

`impl<'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R>` declares `'a` only to use it once. Clippy `clippy::elidable_lifetime_names` flags it; `'_` reads cleaner.

**Files:**
- Modify: [crates/cfg/src/cfg/dot.rs:85](crates/cfg/src/cfg/dot.rs#L85)

- [ ] **Step 1: Elide the lifetime**

Change:

```rust
impl<'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R> {
```

to:

```rust
impl<R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'_, R> {
```

- [ ] **Step 2: Build + test**

```bash
cargo test -p cfg
cargo clippy -p cfg --no-deps
cargo check --workspace
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "style(cfg): elide explicit '_a in CfgDotDumper GraphDotDumper impl"
```

---

## Task 5: Inline format args in the sleigh-error message

`RegionBuilder::build` does `format!("{:?}", e)` to wrap a sleigh error string. Clippy `clippy::uninlined_format_args` (and the rest of the workspace's style) prefers the captured-identifier form.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:307-311](crates/cfg/src/cfg/builder/region_builder.rs#L307-L311)

- [ ] **Step 1: Inline the format arg**

Change:

```rust
.map_err(|e| ErrorKind::GenericSleighError(format!("{:?}", e)))?;
```

to:

```rust
.map_err(|e| ErrorKind::GenericSleighError(format!("{e:?}")))?;
```

- [ ] **Step 2: Build + test**

```bash
cargo test -p cfg
cargo clippy -p cfg --no-deps
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "style(cfg): inline format args in GenericSleighError wrap"
```

---

## Task 6: Add backticks around bare identifiers in doc comments

Two doc comments use bare identifiers that clippy `doc_markdown` flags:
- `crates/cfg/src/cfg/builder/region_builder.rs:139` — `(no insn_index validation)`
- `crates/cfg/src/cfg/builder/mod.rs:78` — `Uses a BTreeMap range query`

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:138-140](crates/cfg/src/cfg/builder/region_builder.rs#L138-L140)
- Modify: [crates/cfg/src/cfg/builder/mod.rs:76-80](crates/cfg/src/cfg/builder/mod.rs#L76-L80)

- [ ] **Step 1: Backtick the identifiers**

In `region_builder.rs`, change:

```rust
/// Checks whether `branch_target_addr` should be treated as a tail call
/// using only address-bounds reasoning (no insn_index validation).
```

to:

```rust
/// Checks whether `branch_target_addr` should be treated as a tail call
/// using only address-bounds reasoning (no `insn_index` validation).
```

In `builder/mod.rs`, change:

```rust
/// Uses a BTreeMap range query to find the last region whose
/// `start_addr <= addr`, then confirms that `addr` also falls within the
/// region's instruction range via [`Region::contains_addr`].
```

to:

```rust
/// Uses a [`BTreeMap`] range query to find the last region whose
/// `start_addr <= addr`, then confirms that `addr` also falls within the
/// region's instruction range via [`Region::contains_addr`].
```

(Linking via `[`BTreeMap`]` requires `BTreeMap` to be in scope; it is — `use std::collections::BTreeMap;` at the top of `builder/mod.rs`.)

- [ ] **Step 2: Build + test + doc**

```bash
cargo doc -p cfg --no-deps
cargo clippy -p cfg --no-deps
cargo test -p cfg
```
Expected: PASS, no doc warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/src/cfg/builder/mod.rs
git commit -m "docs(cfg): backtick insn_index and BTreeMap in doc comments"
```

---

## Task 7: Test-side `clippy::pedantic` cleanup

Round 4 Q4(B): clean up the test-side pedantic lints surfaced by `cargo clippy -p cfg --all-targets -- -W clippy::pedantic`. Mechanical fixes get rewritten; intentional casts get a scoped `#[allow]` with a one-line justification.

**Files:**
- Modify: [crates/cfg/tests/common/real_binary.rs:26](crates/cfg/tests/common/real_binary.rs#L26)
- Modify: [crates/cfg/tests/common/real_binary.rs:46](crates/cfg/tests/common/real_binary.rs#L46)
- Modify: [crates/cfg/tests/common/synthetic.rs:113-116](crates/cfg/tests/common/synthetic.rs#L113-L116) (elidable lifetime)
- Modify: [crates/cfg/tests/common/synthetic.rs:130-145](crates/cfg/tests/common/synthetic.rs#L130-L145) (`i8 as u8` allow)
- Modify: [crates/cfg/tests/cfg_integration.rs:16](crates/cfg/tests/cfg_integration.rs#L16) (doc backticks)
- Modify: [crates/cfg/tests/region_builder_process.rs:25](crates/cfg/tests/region_builder_process.rs#L25) (doc backticks)
- Modify: [crates/cfg/tests/region_builder_decode.rs:120-160](crates/cfg/tests/region_builder_decode.rs#L120-L160) (`i64 as u64` allow + `u64 as usize` fix)

- [ ] **Step 1: Inline format args in `tests/common/real_binary.rs`**

Change line 26 from:

```rust
.unwrap_or_else(|| panic!("symbol '{}' not found in {}", fn_name, binary_path))
```

to:

```rust
.unwrap_or_else(|| panic!("symbol '{fn_name}' not found in {binary_path}"))
```

Change line 46 from:

```rust
.unwrap_or_else(|e| panic!("CFG build failed for '{}': {e:?}", fn_name))
```

to:

```rust
.unwrap_or_else(|e| panic!("CFG build failed for '{fn_name}': {e:?}"))
```

- [ ] **Step 2: Elide lifetime in `tests/common/synthetic.rs::make_region_builder`**

Change:

```rust
pub fn make_region_builder<'a>(
    builder: &'a mut Builder<TestReader>,
    start: PcodeInsnAddr,
) -> TestRegionBuilder<'a, TestReader> {
```

to:

```rust
pub fn make_region_builder(
    builder: &mut Builder<TestReader>,
    start: PcodeInsnAddr,
) -> TestRegionBuilder<'_, TestReader> {
```

- [ ] **Step 3: Allow intentional `i8 as u8` casts in `tests/common/synthetic.rs`**

Read lines around 130-145 to find the two `vec![0xeb, rel as u8, 0xc3]` / `vec![0x74, rel as u8, ...]` helpers. The `rel as u8` cast intentionally reinterprets a signed byte displacement as unsigned (two's-complement byte). Add a function-level allow with a comment for both helpers:

```rust
#[allow(clippy::cast_sign_loss)] // two's-complement byte reinterpretation for x86 short branch displacement
pub fn rel_jmp_then_ret(rel: i8) -> Vec<u8> {
    vec![0xeb, rel as u8, 0xc3]
}

#[allow(clippy::cast_sign_loss)] // two's-complement byte reinterpretation for x86 short branch displacement
pub fn rel_je_then_ret(rel: i8) -> Vec<u8> {
    vec![0x74, rel as u8, 0xc3, 0xc3]
}
```

(Function names and exact bytes may differ; preserve the existing names. Only add the attribute and the comment. If the existing function signatures differ, attach `#[allow]` to each.)

- [ ] **Step 4: Backtick `binary_tests` in `tests/cfg_integration.rs`**

Change line 16 from:

```rust
//!   make -C binary_tests
```

to:

```rust
//!   make -C `binary_tests`
```

- [ ] **Step 5: Backtick `insn_index` in `tests/region_builder_process.rs`**

Change line 25 from:

```rust
/// (insn_index, insn clone).
```

to:

```rust
/// (`insn_index`, insn clone).
```

- [ ] **Step 6: Allow intentional `i64 as u64` and fix `u64 as usize` in `tests/region_builder_decode.rs`**

The two `(-2_i64) as u64` / `(-5_i64) as u64` constructs synthesize CONST-space backward branch offsets — sign-bit reinterpretation is the point. Wrap each occurrence with a localised allow:

For line 127 area, the `Vn` literal contains `off: (-2_i64) as u64`. Replace with:

```rust
#[allow(clippy::cast_sign_loss)] // two's-complement reinterpretation: CONST-space encodes signed pcode-offsets in a u64
let off = (-2_i64) as u64;
let vn = rsleigh::Vn { addr: rsleigh::VnAddr { space: rsleigh::VnSpace::CONST, off }, size: 8 };
```

…or, if you prefer to keep the literal inline, place the allow above the `let vn = …` block. Mirror for line 152's `(-5_i64) as u64`.

For `u64 as usize` at lines 180 and 209 (`fake_lift_res(pcode_count as usize)`): switch to a checked conversion (test code is allowed to `expect` since the test author controls `pcode_count`):

```rust
let lift = fake_lift_res(usize::try_from(pcode_count).expect("pcode_count fits in usize"));
```

- [ ] **Step 7: Verify pedantic-clean on all targets**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -W clippy::pedantic`
Expected: zero warnings (or only pre-existing unrelated lints not touched in this round).

If a remaining warning is reported, list it and either fix mechanically or — if it falls in the "intentional cast" category — add a localised `#[allow]` with a one-line justification.

- [ ] **Step 8: Run the test suite**

```bash
cargo test -p cfg
```
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/cfg/tests/
git commit -m "style(cfg): clean up test-side clippy::pedantic warnings"
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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — same content as before this round (Tasks 1 and 2 only fire on malformed input that the test binary doesn't contain; Tasks 3–6 are no-op refactors).

---

## Out of scope (considered, rejected, or deferred)

- **Test-side `clippy::pedantic` warnings** (uninlined format args in panic messages, intentional `i8 as u8` / `i64 as u64` casts in synthetic helpers, `BTreeMap` doc backticks in test doc comments): test scope only; the casts are intentional for two's-complement byte synthesis. Defer per Q4(A).
- **Defending against `lift_res.insns.is_empty()` and `lift_res.machine_insn_len == 0`:** an empty insn vector or zero-length machine instruction would cause an infinite loop in `RegionBuilder::build`. This is `rsleigh`'s contract to honour; rejected as out-of-scope in round 3 and unchanged here. If `rsleigh` is hardened to allow either, revisit.
- **Erroring on duplicate `start_addr_to_region_id` insert in `add_region`:** in the normal `RegionBuilder` flow `Builder::explore` already checks `find_region_containing_addr` first, so duplicates are unreachable. Defensive check would change behaviour for `test_api` consumers that legitimately call `add_region` twice with the same start. Skip.
- **`vn_to_name_non_register` REGISTER arm is unreachable:** the function is private and all current callers route REGISTER through `vn_to_name_with_regs`. Removing the arm would let a future caller hit the catch-all `UnsupportedVnSpaceDisplay(REGISTER)` instead of `InvalidRegVn(vn)` — a worse error. Leave as defence in depth.
- **Renaming `is_branch_tail_call_nocheck` → `branch_target_is_outside_function`:** more descriptive, but the current name pair (`*_nocheck` / `*` (validated)) follows a reasonable convention and the rename touches `tests/region_builder_tail_call.rs`. Defer until a third caller appears.
- **Consolidating the three `test_api` re-export tiers:** `cfg::test_api`, `cfg::region_builder_test_api`, `cfg::dot_test_api` could collapse into one umbrella. Round 3 normalised the `#[doc(hidden)]` attributes; further consolidation breaks `crates/cfg/src/test_api.rs`'s explicit selective re-exports for unclear gain. Defer.
- **Moving `region_builder.rs::test_api` into a sibling file:** the file is 459 lines now, ~315 of which is impl. Round 3 already considered and deferred this. Same call here.
- **`OptionsBuilder::set_function_max_size(0)` rejection:** `0` is degenerate but legal. Round 3 rejected this; unchanged.
- **`Cfg::sleigh` / `Cfg::graph` / `Cfg::entry` field privatisation:** `analyzer` reads them directly — rejected in rounds 1–3.
- **Replacing `format!("{e:?}")` in `GenericSleighError` with `Display` propagation of `R::Err`:** requires threading `R::Err` through `strider-error::define_error!` — separate cross-cutting effort, rejected in round 1.
