# CFG Crate Review — Round 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round 3 cleanup of the `cfg` crate. After rounds 1 and 2 the crate is structurally sound and lint-clean. Round 3 closes one latent correctness gap (out-of-range CONST-space pcode branch targets are silently dropped instead of erroring), removes a redundant lookup in the DOT dumper, fixes a `&Vec<…>` test-api accessor, normalises a `#[doc(hidden)]` re-export, and converts two utility methods that don't use `Self` into free functions. No public-API change *except* tightening `decode_branch_target`'s contract and converting `test_api::work_queue`'s return type from `&Vec<…>` to `&[…]` (slice deref makes existing callers source-compatible).

**Architecture:** Each task is a self-contained, independently-committable change. External callers (`analyzer`, `examples/analyzer.rs`) only see `Cfg`, `Builder`, `OptionsBuilder`, `RegionEdgeKind`, `RegionId`, `Error`, `ErrorKind`, the four `Cfg::*` accessors and `Region::{start_addr, insns, ends_with_tail_call}` — none of which change. The two test-only signature touches in Tasks 4 and 5 only affect tests inside this crate; the upstream test-API consumer is integration tests under `crates/cfg/tests/`.

**Tech Stack:** Rust, `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Open questions for the reviewer before execution

Each one materially changes a task below. Pick one per group; defaults are noted.

**Q1 — Out-of-range CONST-space pcode index (Task 1):** A `CondBranch`/`Branch` with a CONST-space target whose computed pcode index is `>= lift_res.insns.len()` is currently not flagged anywhere. `decode_branch_target` only checks `target < 0`. The work-queue entry that results has `insn_index` past the actual pcode op count for that machine insn; when the builder later pops it, `for (i, insn) in lift_res.insns.iter().enumerate().skip(insn_index)` iterates zero times and `next_pcode_addr` silently advances to the next machine instruction at index 0. The wrong code path is followed without any error, producing a subtly-wrong CFG.
  - **(A)** Validate at the *decode* site: thread `&rsleigh::LiftRes` into `decode_branch_target` and reject `target >= lift_res.insns.len()` with `ErrorKind::InvalidBranchTargetVaErr`. Catches the bug with the right context and the existing variant. **Default choice.**
  - **(B)** Validate at the *consume* site: at the top of `RegionBuilder::build`'s outer loop, after `lift_one`, check `cur_addr.insn_index < lift_res.insns.len()` and error if not. Simpler diff but flags the bug one step removed from where it was introduced.
  - **(C)** Document the silent-skip as intentional. Risky — there is no caller code that would prefer "skip rather than error" for an invalid backward CONST target.

  **Reviewer action:** confirm (A). If you'd rather stay decoupled from `LiftRes`, pick (B).

**Q2 — `vn_to_name_with_regs(_unchecked)` as free functions or methods (Task 6):** Both currently sit inside `impl<R: rsleigh::MemReader> Cfg<R>` even though neither references `self` or `R`. The DOT dumper calls them as `Cfg::<R>::vn_to_name_with_regs(&regs, vn)` — pointless turbofish. The public `vn_to_name` *does* depend on `self.sleigh` and stays as a method.
  - **(A)** Lift both helpers to free functions in `dot.rs` (private module-scope). Keep `vn_to_name` as the only method. **Default choice.**
  - **(B)** Leave as-is — methods read uniformly even when they don't use `Self`.

  **Reviewer action:** confirm (A).

**Q3 — `NoInstructionsRegionBuilder` error variant (Task 7):** `RegionBuilder::finish_current_region` does `if self.insns.is_empty() { return Err(ErrorKind::NoInstructionsRegionBuilder.into()); }`. The very next call is `self.builder.add_region(Region { ..., insns: std::mem::take(&mut self.insns), ... })`, which itself rejects empty regions with `ErrorKind::EmptyRegion(start_addr)`. The two checks fire on the same condition; the only behavioural difference is the error variant. `NoInstructionsRegionBuilder` is dead in practice — `process_new_insn` always pushes an instruction before the first opcode-driven `finish_current_region` call.
  - **(A)** Drop the early check in `finish_current_region` and the variant from `ErrorKind`. The `EmptyRegion(start_addr)` from `add_region` is the canonical error, and it carries strictly more diagnostic info (the region's start address). **Default choice.**
  - **(B)** Keep both — defence in depth. Costs an unused variant + a never-fired check.

  **Reviewer action:** confirm (A). The shared cfg test helpers don't construct `NoInstructionsRegionBuilder` (verified via `grep -rn "NoInstructionsRegionBuilder" crates/cfg`).

**Q4 — `process_new_insn` cloning style (Task 8):** Current code uses `insn.to_owned()`; `.clone()` is the more direct call for an already-owned `rsleigh::Insn`. Pure stylistic micro-cleanup with no behavioural change.
  - **(A)** Switch to `.clone()`. **Default choice.**
  - **(B)** Leave as `.to_owned()`.

  **Reviewer action:** confirm (A) (or skip Task 8 entirely).

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Validate CONST-space pcode-index branch targets against `lift_res.insns.len()`

The CONST arm of `decode_branch_target` lets `target = base + off (signed)` produce an index that is past the end of the current machine instruction's pcode sequence. The builder later silently skips such entries. We push the validation up to `decode_branch_target` so the bug is caught at the same place as the other malformed-target checks.

`decode_branch_target` currently has no access to `lift_res`. We thread it through both call sites (Branch and CondBranch arms in `process_new_insn`).

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:83-123](crates/cfg/src/cfg/builder/region_builder.rs#L83-L123)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:184-225](crates/cfg/src/cfg/builder/region_builder.rs#L184-L225)
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:404-410](crates/cfg/src/cfg/builder/region_builder.rs#L404-L410) (test_api forwarder)
- Modify: [crates/cfg/tests/region_builder_decode.rs](crates/cfg/tests/region_builder_decode.rs)

- [ ] **Step 1: Add a failing test**

Append to `crates/cfg/tests/region_builder_decode.rs`:

```rust
/// Pinned contract: a CONST-space relative branch target whose computed pcode
/// index lands past the last pcode op of the current machine instruction is
/// rejected at decode time. Without this check, the BUILD loop silently skips
/// the entry on its next pop and advances to the wrong machine instruction.
#[test]
fn decode_branch_target_const_space_index_past_end_errors() {
    use cfg::test_api::Options;

    // Lift a real machine instruction so we have a true lift_res to pass.
    let opts = cfg::OptionsBuilder::new().build();
    let mut b = common::make_builder_opts(0x1000, opts);
    let mut rb = common::make_region_builder(&mut b, common::addr(0x1000, 0));

    // The first machine insn at 0x1000 lifts to N pcode ops. We synthesize a
    // CONST-space target whose offset jumps past N.
    let lift = b_lift_at(&b, 0x1000);
    let pcode_count = lift.insns.len() as u64;
    assert!(pcode_count > 0, "synthetic harness must emit ≥1 pcode op");

    let vn = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::CONST,
            // Forward jump to (current_idx + pcode_count + 1) — definitely past end.
            off: (pcode_count + 1),
        },
        size: 8,
    };

    let err = rb
        .decode_branch_target(vn, common::addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(
        matches!(err.kind(), cfg::ErrorKind::InvalidBranchTargetVaErr(_, _)),
        "expected InvalidBranchTargetVaErr; got {:?}",
        err.kind()
    );
}

/// Helper: lift a single machine insn from the builder's sleigh context.
/// Inlined here to avoid widening tests/common's surface area.
fn b_lift_at<R: rsleigh::MemReader>(b: &cfg::Builder<R>, at: u64) -> rsleigh::LiftRes {
    cfg::test_api::sleigh(b).lift_one(at).expect("lift")
}
```

If `make_builder_opts` is not currently re-exported through `tests/common/mod.rs`, mirror the pattern used by Round 1 Task 1 (`fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call`) — that test imports the same helper from `common::synthetic` so the wiring is identical.

If `cfg::test_api::sleigh` (the Round 1 forwarder added in `crates/cfg/src/cfg/builder/mod.rs::test_api`) is not exposed at `cfg::test_api`, add a one-line re-export through `crates/cfg/src/test_api.rs`:

```rust
#[doc(hidden)]
pub use crate::cfg::test_api::sleigh;
```

(Read `crates/cfg/src/test_api.rs` first to confirm; the Round 1 forwarder is already there so no edit may be needed.)

- [ ] **Step 2: Run the new test and confirm it fails**

Run: `cargo test -p cfg --test region_builder_decode decode_branch_target_const_space_index_past_end_errors`
Expected: FAIL — current code returns an `Ok(PcodeInsnAddr { insn_index: pcode_count + 1, .. })`. The test expects `Err(InvalidBranchTargetVaErr)`.

- [ ] **Step 3: Tighten `decode_branch_target`**

In `crates/cfg/src/cfg/builder/region_builder.rs`, change the signature to accept `lift_res: &rsleigh::LiftRes` and validate the resulting index in the CONST arm:

```rust
fn decode_branch_target(
    &self,
    branch_target_var: rsleigh::Vn,
    branch_insn_addr: PcodeInsnAddr,
    lift_res: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    let default_code_space = self.builder.sleigh.default_code_space();

    match branch_target_var.addr.space {
        // Relative branch: signed offset from the current pcode insn index.
        // rsleigh encodes CONST-space branch targets as two's-complement in a
        // u64, so a backward pcode-local branch comes in as `(-n) as u64`.
        rsleigh::VnSpace::CONST => {
            let base = branch_insn_addr.insn_index as i64;
            let off = branch_target_var.addr.off as i64;
            let target = base.checked_add(off).ok_or(
                ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
            )?;
            if target < 0 || (target as u64) >= lift_res.insns.len() as u64 {
                return Err(ErrorKind::InvalidBranchTargetVaErr(
                    branch_target_var,
                    branch_insn_addr,
                )
                .into());
            }
            Ok(PcodeInsnAddr {
                machine_addr: branch_insn_addr.machine_addr,
                insn_index: target as u64,
            })
        }
        // Absolute branch: the offset IS the target machine address.
        space if space == default_code_space => Ok(PcodeInsnAddr {
            machine_addr: MachineInsnAddr {
                addr: branch_target_var.addr.off,
            },
            insn_index: 0,
        }),
        _ => {
            Err(ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr).into())
        }
    }
}
```

- [ ] **Step 4: Update both call sites in `process_new_insn`**

In the same file, update the two `decode_branch_target` invocations to pass `lift_res`:

```rust
rsleigh::Opcode::Branch => {
    let branch_target_addr = self.decode_branch_target(insn.inputs[0], addr, lift_res)?;
    ...
}
rsleigh::Opcode::CondBranch => {
    let target_addr = self.decode_branch_target(insn.inputs[0], addr, lift_res)?;
    ...
}
```

- [ ] **Step 5: Update the `test_api` forwarder**

Change the `TestRegionBuilder::decode_branch_target` forwarder at the bottom of the file:

```rust
/// # Errors
/// Propagates errors from the underlying branch-target decode, including
/// out-of-range CONST-space pcode indices.
pub fn decode_branch_target(
    &self,
    vn: rsleigh::Vn,
    at: PcodeInsnAddr,
    lift: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    self.inner.decode_branch_target(vn, at, lift)
}
```

- [ ] **Step 6: Update existing decode tests to pass a `LiftRes`**

Read `crates/cfg/tests/region_builder_decode.rs` end-to-end. Every existing call site of `decode_branch_target` (currently 3 — backward-offset, underflow-errors, default-code-space) must now pass a real `lift_res`. The `b_lift_at` helper added in Step 1 is the right vehicle. For each existing test, lift once at the test's start machine address and pass it through. Example update for the `decode_branch_target_const_space_negative_offset_does_not_wrap` test:

```rust
let lift = b_lift_at(&b, 0x1000);
let got = rb.decode_branch_target(vn, common::addr(0x1000, 5), &lift).unwrap();
```

For tests that exercise the underflow-errors path, the same `lift_res` works — the CONST-arm validation rejects negative `target` *before* the new bounds check.

For tests that exercise the default-code-space arm: any non-empty `lift_res` works; the CONST validation never executes.

If lifting a real instruction is awkward in some test, construct a minimal `LiftRes` directly (peek at `../rsleigh/src/lib.rs` for the public field shape — it has `insns: Vec<Insn>` and `machine_insn_len: u8` per `crates/cfg/src/cfg/builder/region_builder.rs:25`'s usage). One line:

```rust
let dummy_lift = rsleigh::LiftRes {
    insns: vec![/* minimal Insn or count-of-N */],
    machine_insn_len: 4,
};
```

Use whichever is simpler per test.

- [ ] **Step 7: Run the new test and the full cfg suite**

```bash
cargo test -p cfg --test region_builder_decode
cargo test -p cfg
cargo check --workspace
```
Expected: PASS — including the new test, the prior backward-offset / underflow / default-code-space tests, and `region_builder_process` / `build_end_to_end` (which exercise the full Branch/CondBranch path through `decode_branch_target`).

- [ ] **Step 8: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/tests/region_builder_decode.rs crates/cfg/src/test_api.rs
git commit -m "fix(cfg): reject CONST-space branch targets whose pcode index is past the lifted insn count"
```

---

## Task 2: Drop redundant `node.insns.first()` lookup in DOT dumper

`dump_as_dot` does:

```rust
let first_insn_index = node
    .insns
    .first()
    .ok_or(ErrorKind::EmptyRegion(node.start_addr))?
    .addr
    .insn_index;
```

By invariant (`Builder::add_region` rejects empty regions and `RegionBuilder::process_new_insn` always pushes the instruction whose `addr` equals the region's `start_addr` before any `finish_current_region` call), `node.insns.first().unwrap().addr == node.start_addr`. So `first_insn_index == node.start_addr.insn_index`. The lookup, the `EmptyRegion` error, and the `.first()` call are all redundant — we can read the field directly.

This does not delete the `EmptyRegion` variant: `Builder::add_region` still uses it as the canonical guard. Round 2 already shrunk its payload to `PcodeInsnAddr`, so there is no perf or size benefit beyond removing the dead lookup itself. The clarity win comes from one fewer error path on a hot rendering loop.

**Files:**
- Modify: [crates/cfg/src/cfg/dot.rs:96-138](crates/cfg/src/cfg/dot.rs#L96-L138)

- [ ] **Step 1: Replace the lookup**

In `crates/cfg/src/cfg/dot.rs::dump_as_dot`, change:

```rust
let first_insn_index = node
    .insns
    .first()
    .ok_or(ErrorKind::EmptyRegion(node.start_addr))?
    .addr
    .insn_index;
let start_addr = node.start_addr.machine_addr.addr;
```

to:

```rust
let first_insn_index = node.start_addr.insn_index;
let start_addr = node.start_addr.machine_addr.addr;
```

- [ ] **Step 2: Run dot tests**

Run: `cargo test -p cfg --test dot_dumper`
Expected: PASS — the rendered label string for every region is bit-identical to the prior code path because the value of `first_insn_index` is unchanged.

- [ ] **Step 3: Run full cfg suite**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "refactor(cfg): use Region::start_addr.insn_index directly in dot dumper instead of inspecting insns.first()"
```

---

## Task 3: Replace `node.insns.iter()` with `&node.insns` in DOT dumper loop

A `clippy::pedantic` style nit, but it's the only loop in `dump_as_dot` that uses explicit `.iter()` and the file is otherwise consistent on `&container`-style iteration. Cosmetic only.

**Files:**
- Modify: [crates/cfg/src/cfg/dot.rs:123](crates/cfg/src/cfg/dot.rs#L123)

- [ ] **Step 1: Change the loop**

Change:

```rust
for insn in node.insns.iter() {
```

to:

```rust
for insn in &node.insns {
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "refactor(cfg): iterate &node.insns directly in dot dumper"
```

---

## Task 4: Return `&[…]` from `test_api::work_queue` instead of `&Vec<…>`

`builder::test_api::work_queue` currently signs as:

```rust
pub fn work_queue<R: rsleigh::MemReader>(
    b: &Builder<R>,
) -> &Vec<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)> {
    &b.work_queue
}
```

`&Vec<T>` is a clippy `ptr_arg`/style smell — `&[T]` is the strict superset. All current callers in `tests/region_builder_process.rs` and `tests/region_builder_tail_call.rs` only use `[index]`, `.len()`, `.iter()`, and `.is_empty()`, all of which work on a slice. The `&Vec<…>` wrapper adds nothing.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/mod.rs:213-218](crates/cfg/src/cfg/builder/mod.rs#L213-L218)

- [ ] **Step 1: Change the return type**

```rust
#[must_use]
pub fn work_queue<R: rsleigh::MemReader>(
    b: &Builder<R>,
) -> &[(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)] {
    &b.work_queue
}
```

(`&Vec<T>` deref-coerces to `&[T]`, so `&b.work_queue` is fine without `.as_slice()`.)

- [ ] **Step 2: Verify no caller relied on `Vec`-specific methods**

Run: `grep -rn "work_queue" crates/cfg/tests/`
Expected: only `[N]`, `.len()`, `.iter()`, or `.is_empty()` accesses. If any test calls `.capacity()`, `.push()`, or `.to_vec()`, list it and adjust.

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/mod.rs
git commit -m "refactor(cfg): expose work_queue test_api accessor as &[…] instead of &Vec<…>"
```

---

## Task 5: Mark `pub use builder::test_api;` as `#[doc(hidden)]` for consistency

In `crates/cfg/src/cfg/mod.rs` two test-API re-exports are spelled inconsistently:

```rust
pub use builder::test_api;          // L8 — NOT marked #[doc(hidden)]

#[doc(hidden)]
pub use dot::test_api as dot_test_api;   // L11-12

#[doc(hidden)]
pub use builder::region_builder_test_api;  // L14-15
```

The upstream `pub mod test_api { … }` in `builder/mod.rs` already carries `#[doc(hidden)]`, but rustdoc's hidden-ness does *not* always propagate through re-exports. The wrapping `pub mod test_api;` in `lib.rs` *is* marked `#[doc(hidden)]`, so the externally-visible path is hidden — but the in-crate path `cfg::test_api` is currently visible to rustdoc. Cosmetic, but worth normalising for the next reader.

**Files:**
- Modify: [crates/cfg/src/cfg/mod.rs:8](crates/cfg/src/cfg/mod.rs#L8)

- [ ] **Step 1: Add the attribute**

Change:

```rust
pub use builder::test_api;
```

to:

```rust
#[doc(hidden)]
pub use builder::test_api;
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg && cargo check --workspace && cargo doc -p cfg --no-deps`
Expected: PASS, no doc warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/mod.rs
git commit -m "docs(cfg): mark builder::test_api re-export #[doc(hidden)] for consistency with dot_test_api"
```

---

## Task 6: Convert `vn_to_name_with_regs` and `vn_to_name_with_regs_unchecked` to free functions

Both currently sit inside `impl<R: rsleigh::MemReader> Cfg<R>` but neither references `self`, `R`, or any `Cfg`-bound state. The DOT dumper calls them as `Cfg::<R>::vn_to_name_with_regs(&regs, vn)` — a turbofish for a function that doesn't need it. Lifting them to module-level free functions removes the noise.

**Files:**
- Modify: [crates/cfg/src/cfg/dot.rs:9-56](crates/cfg/src/cfg/dot.rs#L9-L56)
- Modify: [crates/cfg/src/cfg/dot.rs:120-130](crates/cfg/src/cfg/dot.rs#L120-L130)

- [ ] **Step 1: Move both helpers out of `impl Cfg`**

In `crates/cfg/src/cfg/dot.rs`, replace the `vn_to_name_with_regs` and `vn_to_name_with_regs_unchecked` associated functions with free functions, and rewrite `vn_to_name` to call them directly:

```rust
impl<R: rsleigh::MemReader> Cfg<R> {
    /// Public entry — fetches Sleigh registers per call. Suitable for ad-hoc
    /// callers; the DOT dumper uses [`vn_to_name_with_regs`] instead so it
    /// only fetches once per render.
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        match vn.addr.space {
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(ErrorKind::SleighError)?;
                vn_to_name_with_regs(&regs, vn)
            }
            _ => vn_to_name_non_register(vn),
        }
    }

    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    #[must_use]
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
    }
}

/// Render `vn` to a name, using a pre-fetched [`rsleigh::SleighRegs`] for
/// REGISTER lookups. Non-REGISTER spaces don't need `regs`.
///
/// `regs` is fetched once per node rendered by the DOT dumper. If profiling
/// later flags this as hot, lift the fetch into per-dump state by changing
/// `CfgDotDumper`'s `GraphDotDumper::State` from `()` to `SleighRegs`.
fn vn_to_name_with_regs(regs: &rsleigh::SleighRegs, vn: &rsleigh::Vn) -> Result<String> {
    if vn.addr.space == rsleigh::VnSpace::REGISTER {
        return Ok(regs
            .vn_to_name(*vn)
            .ok_or(ErrorKind::InvalidRegVn(*vn))?
            .to_string());
    }
    vn_to_name_non_register(vn)
}

/// Render `vn` for non-REGISTER spaces. REGISTER input is a caller-routing
/// bug (the caller should have gone through [`vn_to_name_with_regs`]) and
/// yields [`ErrorKind::InvalidRegVn`].
fn vn_to_name_non_register(vn: &rsleigh::Vn) -> Result<String> {
    let offset = vn.addr.off;
    let size = vn.size;
    match vn.addr.space {
        rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
        rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
        rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
        rsleigh::VnSpace::REGISTER => Err(ErrorKind::InvalidRegVn(*vn).into()),
        s => Err(ErrorKind::UnsupportedVnSpaceDisplay(s).into()),
    }
}
```

(I'm renaming `vn_to_name_with_regs_unchecked` → `vn_to_name_non_register`. The new name describes *what* the function handles, not *what it doesn't check* — the rounds-2 name was awkward.)

- [ ] **Step 2: Update the call site in `dump_as_dot`**

Change:

```rust
.map(|vn| Cfg::<R>::vn_to_name_with_regs(&regs, vn))
```

to:

```rust
.map(|vn| vn_to_name_with_regs(&regs, vn))
```

- [ ] **Step 3: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS — the `tests/dot_dumper.rs` and `tests/vn_to_name.rs` suites exercise every branch.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs
git commit -m "refactor(cfg): lift vn_to_name_with_regs / non_register out of impl Cfg into free functions"
```

---

## Task 7: Drop `ErrorKind::NoInstructionsRegionBuilder` and the redundant emptiness check

`RegionBuilder::finish_current_region` checks `self.insns.is_empty()` and returns `NoInstructionsRegionBuilder`. Immediately after, it calls `Builder::add_region(Region { …, insns: std::mem::take(&mut self.insns), … })`, which itself returns `EmptyRegion(start_addr)` for the same condition. The first check fires only on a contradiction (the only possible caller — `process_new_insn` — pushes an instruction *before* triggering `finish_current_region`).

Drop the early check and the now-orphaned variant.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:238-251](crates/cfg/src/cfg/builder/region_builder.rs#L238-L251)
- Modify: [crates/cfg/src/error.rs:27-28](crates/cfg/src/error.rs#L27-L28)

- [ ] **Step 1: Remove the early check**

Change:

```rust
fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
    if self.insns.is_empty() {
        return Err(ErrorKind::NoInstructionsRegionBuilder.into());
    }
    let region = self.builder.add_region(Region {
        start_addr: self.start_addr,
        insns: std::mem::take(&mut self.insns),
        ends_with_tail_call,
    })?;
    if let Some((parent_id, edge_kind)) = self.parent_edge {
        self.builder.graph.add_edge(parent_id, region, edge_kind);
    }
    Ok(region)
}
```

to:

```rust
/// Finalises the region that has been accumulating instructions.
///
/// Calls `Builder::add_region` (which enforces non-emptiness via
/// [`ErrorKind::EmptyRegion`]) and, if there is a parent edge, adds that
/// edge to the graph. Returns the new region's [`NodeIndex`].
fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
    let region = self.builder.add_region(Region {
        start_addr: self.start_addr,
        insns: std::mem::take(&mut self.insns),
        ends_with_tail_call,
    })?;
    if let Some((parent_id, edge_kind)) = self.parent_edge {
        self.builder.graph.add_edge(parent_id, region, edge_kind);
    }
    Ok(region)
}
```

- [ ] **Step 2: Remove the variant**

In `crates/cfg/src/error.rs`, delete:

```rust
#[error("builder about to build an empty instruction region")]
NoInstructionsRegionBuilder,
```

- [ ] **Step 3: Verify no remaining references**

Run: `grep -rn "NoInstructionsRegionBuilder" crates/`
Expected: empty output (workspace-wide).

- [ ] **Step 4: Build + test + workspace check**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS. `tests/builder_add_region.rs` covers the `EmptyRegion` path; if any test specifically pattern-matched on `NoInstructionsRegionBuilder` (`grep -rn "NoInstructionsRegionBuilder" crates/cfg/tests`), retarget it to the `EmptyRegion(_)` arm.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/src/error.rs
git commit -m "refactor(cfg): drop NoInstructionsRegionBuilder; rely on add_region's EmptyRegion check"
```

---

## Task 8: Replace `insn.to_owned()` with `insn.clone()` in `process_new_insn`

Pure stylistic micro-cleanup. `to_owned()` for an already-owned `rsleigh::Insn` is `<Insn as Clone>::clone`; `.clone()` is the more direct call.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:190-193](crates/cfg/src/cfg/builder/region_builder.rs#L190-L193)

- [ ] **Step 1: Change the call**

```rust
self.insns.push(RegionInstruction {
    addr,
    insn: insn.clone(),
});
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p cfg && cargo check --workspace`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "style(cfg): use Insn::clone() directly in process_new_insn"
```

---

## Task 9: Final sanity sweep

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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced, matching pre-Round-3 behavior. Open `cfg.html` and confirm a few random regions still show their register names (Task 6 regression check).

---

## Out of scope (considered, rejected, or deferred)

- **Tightening `Region::contains_addr` to exact-match (currently range-based):** the range semantics is documented (`tests/region.rs::contains_addr_in_interior` covers `addr(0x1008, 0)` between `(0x1000, 0)` and `(0x1010, 0)` and asserts `true`). External callers (`Builder::find_region_containing_addr` → `Builder::explore` → `split_region`) cope: the `position(|insn| insn.addr == addr)?` in `split_region` rejects ranges that don't hit a real instruction with `FailedSplitingRegion`. Tightening would change observable behaviour for synthetic-test consumers via `test_api`. Out of scope.
- **`Cfg::sleigh` / `Cfg::graph` / `Cfg::entry` field privatisation:** rejected in rounds 1 and 2 — `analyzer` reads them directly. No reason to revisit.
- **Replacing `format!("{:?}", e)` in `GenericSleighError`:** pragmatically rejected via Round 1 Q3; threading `R::Err` through `strider-error::define_error!` is a separate cross-cutting effort.
- **Validating `branch_target_var.size` in the absolute-branch arm of `decode_branch_target`:** the size is informational; sleigh does not constrain it for branch targets, and the eventual `lift_one(target_addr)` either succeeds or returns a typed sleigh error. YAGNI.
- **`process_new_insn` taking `Insn` by value to skip the clone:** caller `RegionBuilder::build` borrows from `lift_res.insns`; ownership transfer would require draining the vector and breaking iteration. The clone cost is dominated by Sleigh lifting itself.
- **Renaming `RegionId` ⇄ `NodeIndex`:** type alias is intentional and externally-used (`analyzer`, `pattern`).
- **Adding a `Default` to `Builder`:** the start address is a required parameter — there is no sensible default.
- **Splitting `region_builder.rs` (444 lines):** ~315 lines of impl + ~130 lines of `test_api` block. The `test_api` mod could be moved to a sibling file (`region_builder_test.rs` re-exported from the parent), but the layering benefit is marginal. Defer until either the impl grows past ~500 lines or a concrete test_api bug surfaces.
- **`set_function_max_size(0)` rejection:** `0` is a degenerate but legal value; `OptionsBuilder` should not error on it. A doc note could be added but isn't worth its own task.
- **Validating `lift_res.insns.is_empty() == false` and `lift_res.machine_insn_len > 0` in the build loop:** a Sleigh empty-insn or zero-len machine instruction would cause an infinite loop. This is upstream's contract to honour and `rsleigh` does not document a path that produces either; would be a separate bug-find investigation against `rsleigh`, not a `cfg` review item.
