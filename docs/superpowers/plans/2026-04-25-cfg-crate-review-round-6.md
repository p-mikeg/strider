# CFG Crate Review — Round 6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round 6 cosmetic and small-structural cleanup of the `cfg` crate. After rounds 1–5 the crate is correct, panic-free, lint-clean at the default level, and pedantic-clean at the lib level. Round 6 closes a small backlog of minor readability and simplification items that fell below the threshold of earlier rounds: a duplicated `ProcessInsnRes` enum + `From` shim in `test_api`, two `RegionBuilder` impl blocks that could trivially be one, two overlapping comment blocks on the CONST-space arm of `decode_branch_target`, and three trivial nits (one unused `let _region` binding, one unnecessary `#[inline]`, two doc-comment polishes).

**Architecture:** Each task is a self-contained, independently-committable change. No public-API change. No behaviour change in any execution path. Tests at `crates/cfg/tests/region_builder_process.rs` consume `cfg::test_api::ProcessInsnRes` by name; Task 2 preserves the same name and the same set of variants, so test code does not need to change. The `Hash` derive on the inner enum is already in place; pulling it through is non-breaking.

**Tech Stack:** Rust 1.93+, `petgraph::StableDiGraph`, `rsleigh` (external path crate), `strider-error`, `thiserror`, `dot`.

---

## Up-front honest assessment

A side audit by an exploration sub-agent concluded that "Round 6 is not warranted" — the crate is correct and well-tested, and the remaining items are cosmetic. The reviewer should weigh that take. **You can elect to apply only a subset of these tasks**, or skip the round entirely. Each task here is small enough (2–10 lines changed) that picking-and-choosing is cheap.

The biggest *substantive* item is Task 2 (eliminating `ProcessInsnRes` duplication, ~13 lines + the `From` impl removed). The rest are sub-5-line touch-ups.

---

## Open questions for the reviewer before execution

Each one materially changes a task below. Pick one per group; defaults are noted.

**Q1 — Merge the two `RegionBuilder` impl blocks (Task 1):** [region_builder.rs:72-85](crates/cfg/src/cfg/builder/region_builder.rs#L72-L85) defines `new` in `impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R>`; [region_builder.rs:87](crates/cfg/src/cfg/builder/region_builder.rs#L87) opens a second block `impl<R: rsleigh::MemReader> RegionBuilder<'_, R>` for everything else. The split appears to be historical — both forms are equivalent for these methods (the elided `'_` and the named `'a` are interchangeable in the function bodies, none of which constrain `'a`).
  - **(A)** Merge into a single `impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R> { … }` block. **Default choice.** No semantic change; one fewer impl-block boundary to scan.
  - **(B)** Skip — the split is intentional structural separation between constructor and methods.

  **Reviewer action:** confirm (A).

**Q2 — Replace `test_api::ProcessInsnRes` duplicate with `pub use` of the inner enum (Task 2):** [region_builder.rs:386-400](crates/cfg/src/cfg/builder/region_builder.rs#L386-L400) defines a second `ProcessInsnRes` enum inside `pub mod test_api` with the same two variants as the inner one, plus a `From<InnerProcessInsnRes> for ProcessInsnRes` conversion that all wrapper methods call via `.map(Into::into)`. The duplication exists because the inner `ProcessInsnRes` is `pub(super)`-ish (only re-exported via `region_builder_test_api::ProcessInsnRes`). Removing the duplicate is straightforward: have `test_api` `pub use super::ProcessInsnRes`, drop the wrapper enum + the `From` impl + the `.map(Into::into)` calls.

  Deriva difference: the inner enum derives `Debug, Clone, Copy, PartialEq, Eq, Hash`; the outer drops `Hash`. Pulling the inner enum through gains `Hash`, which is a strict superset and breaks no tests (test code just compares with `==`).
  - **(A)** Replace the duplicate with `pub use super::ProcessInsnRes;`. Drop the `From` impl. Drop the `.map(Into::into)` calls in `process_new_insn` and `process_insn` test wrappers (the inner methods already return `Result<ProcessInsnRes>` of the same type the test caller now expects). **Default choice.** Removes 13 lines and a per-call conversion.
  - **(B)** Keep the duplicate as a deliberate API-surface boundary. Maintains the property that `test_api::*` could in principle have a different `ProcessInsnRes` shape from the inner one.

  **Reviewer action:** confirm (A).

**Q3 — Collapse overlapping comment blocks in `decode_branch_target` CONST-space arm (Task 3):** [region_builder.rs:109-145](crates/cfg/src/cfg/builder/region_builder.rs#L109-L145) has TWO comment blocks describing the same encoding semantics:
  - Lines 109-118 (immediately *before* the `match`): "Relative branch: signed offset … rsleigh encodes CONST-space branch targets as two's-complement … out-of-range index would otherwise be silently skipped …"
  - Lines 119-124 (immediately *inside* the CONST arm): "CONST-space encodes the pcode-target offset as a two's-complement i64 stored in a u64 … `cast_signed` is the bit-pattern-preserving u64→i64 reinterpretation; `checked_add_signed` catches either-direction overflow …"
  - Lines 129-133 (after the cast, before the bounds check): "The resulting index must land within … An out-of-range index would otherwise be silently skipped …"
  
  The pre-match block and inside-arm block both explain the two's-complement-in-u64 encoding. The pre-match block and the after-cast block both explain the silent-skip consequence of an out-of-range target. The actual new information is split across three places.

  - **(A)** Keep ONE comment block, attached to the CONST arm itself (not before the `match`). Single block describes (i) what CONST-space encoding is (two's-complement-in-u64), (ii) what the cast/checked_add_signed combo does, (iii) why the bounds check exists. Drop the pre-match comment entirely, and drop the after-cast comment. **Default choice.**
  - **(B)** Keep all three; they're individually short and locality-friendly.

  **Reviewer action:** confirm (A).

**Q4 — Three trivial nits, individually committable (Task 4 / 5 / 6):** Each one is a 1–2 line change.

  - **Q4a — `Builder::start_pcode_addr` `#[inline]` annotation** [crates/cfg/src/cfg/builder/mod.rs:94](crates/cfg/src/cfg/builder/mod.rs#L94). The function is `pub(super) fn` called from one site (`Builder::build`). `#[inline]` on a private intra-crate function is at best decorative; rustc inlines small private functions at MIR level regardless. Drop it.
    - **(A)** Drop. **Default choice.** **(B)** Keep.
  - **Q4b — Unused `let _region = …` binding in `process_new_insn`'s Return arm** [crates/cfg/src/cfg/builder/region_builder.rs:269](crates/cfg/src/cfg/builder/region_builder.rs#L269). The binding is purely positional — its result is unused — but it adds visual noise. Drop the `let _region = ` and just call the method.
    - **(A)** Drop the `let`. **Default choice.** **(B)** Keep as a marker that the call returns a value.
  - **Q4c — Doc-comment polish on `Cfg::vn_to_name` and `Result<T>`.** [crates/cfg/src/cfg/dot.rs:10-12](crates/cfg/src/cfg/dot.rs#L10-L12) opens with "Public entry — fetches Sleigh registers per call." but the method is `pub(super)`, not `pub`. [crates/cfg/src/error.rs:57](crates/cfg/src/error.rs#L57) reads "the result type using our error." — lowercase, no closing context. Tighten both.
    - **(A)** Update both. **Default choice.** **(B)** Skip.

  **Reviewer action:** confirm (A) for all three.

**Q5 — Bundle the trivial nits into one commit, or three (Tasks 4 / 5 / 6 layout):**
  - **(A)** Three separate commits, one per nit. Matches round-5's style (one commit per discrete change). Easier to revert individually. **Default choice.**
  - **(B)** One commit covering all three. Less log noise.

  **Reviewer action:** confirm (A).

**Reviewer's choices (defaults):** Q1 = A, Q2 = A, Q3 = A, Q4a = A, Q4b = A, Q4c = A, Q5 = A. (Pick `B` on any to drop or modify the corresponding task.)

---

## Task 1: Merge the two `RegionBuilder` impl blocks

**Problem:** `RegionBuilder::new` lives in [region_builder.rs:72-85](crates/cfg/src/cfg/builder/region_builder.rs#L72-L85) under `impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R>`. The other methods (`decode_branch_target`, `is_branch_tail_call_nocheck`, `is_branch_tail_call`, `process_new_insn`, `finish_current_region`, `process_insn`, `build`) live in a separate block at [region_builder.rs:87](crates/cfg/src/cfg/builder/region_builder.rs#L87) under `impl<R: rsleigh::MemReader> RegionBuilder<'_, R>`. The split is structurally arbitrary — `'a` and `'_` are equivalent here since none of the methods bind the lifetime in their signature.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:72-87](crates/cfg/src/cfg/builder/region_builder.rs#L72-L87)

- [ ] **Step 1: Delete the second `impl` opener and merge**

In `crates/cfg/src/cfg/builder/region_builder.rs`, find the block break near line 85-87:

```rust
impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R> {
    pub(super) fn new(
        builder: &'a mut Builder<R>,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<(NodeIndex, RegionEdgeKind)>,
    ) -> Self {
        RegionBuilder {
            builder,
            start_addr,
            insns: Vec::new(),
            parent_edge,
        }
    }
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, R> {
    /// Decodes a pcode branch-target varnode into a [`PcodeInsnAddr`].
    /// …
    fn decode_branch_target(
```

Change to:

```rust
impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R> {
    pub(super) fn new(
        builder: &'a mut Builder<R>,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<(NodeIndex, RegionEdgeKind)>,
    ) -> Self {
        RegionBuilder {
            builder,
            start_addr,
            insns: Vec::new(),
            parent_edge,
        }
    }

    /// Decodes a pcode branch-target varnode into a [`PcodeInsnAddr`].
    /// …
    fn decode_branch_target(
```

(Just delete the closing `}` after `new`, the `impl<R: rsleigh::MemReader> RegionBuilder<'_, R> {` line, and merge — keeping all method bodies as-is. The remaining methods continue under the `'a` lifetime, which they don't constrain explicitly anyway.)

- [ ] **Step 2: Build + lint**

```bash
cargo build -p cfg
cargo clippy -p cfg --all-targets --no-deps -- -D warnings
```
Expected: PASS. No warnings introduced.

- [ ] **Step 3: Run the cfg suite**

Run: `cargo test -p cfg`
Expected: PASS — purely structural change.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): merge RegionBuilder's two impl blocks"
```

---

## Task 2: Eliminate `ProcessInsnRes` duplication in `test_api`

**Problem:** [region_builder.rs:386-400](crates/cfg/src/cfg/builder/region_builder.rs#L386-L400) declares a second `ProcessInsnRes` enum inside `pub mod test_api` that mirrors the inner `ProcessInsnRes` exactly (same two variants), plus a `From<InnerProcessInsnRes> for ProcessInsnRes` conversion. The two `process_*_insn` test-API wrappers each call `.map(Into::into)` to bridge between the inner and outer enum types. The whole structure exists only because the inner enum is private to the module.

The inner enum is already `Debug, Clone, Copy, PartialEq, Eq, Hash`; that's a strict superset of the outer enum's derives. Re-exporting the inner one directly removes the duplication with no test-side change (test code constructs neither, only compares with `==`, which `PartialEq` provides).

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:386-503](crates/cfg/src/cfg/builder/region_builder.rs#L386-L503) (test_api section)

- [ ] **Step 1: Inspect the test consumer to confirm the API contract**

Read `crates/cfg/tests/region_builder_process.rs` lines 1-30 to see how `ProcessInsnRes` is imported and used. Expected: `use cfg::test_api::{self, ProcessInsnRes, RegionInstruction};` and equality comparisons like `assert_eq!(res, ProcessInsnRes::FinishedProcessing);`. No constructor calls beyond `::FinishedProcessing` / `::DidntFinishProcessing`. Confirm.

- [ ] **Step 2: Replace the duplicate enum + `From` impl with a `pub use`**

In [crates/cfg/src/cfg/builder/region_builder.rs:368](crates/cfg/src/cfg/builder/region_builder.rs#L368), the current import line:

```rust
use super::{ProcessInsnRes as InnerProcessInsnRes, RegionBuilder};
```

becomes:

```rust
use super::RegionBuilder;
```

Add (right after the imports, before `pub fn next_pcode_addr`):

```rust
pub use super::ProcessInsnRes;
```

Then delete the entire block at lines 386-400 (the duplicate `pub enum ProcessInsnRes { … }` + the `impl From<InnerProcessInsnRes> for ProcessInsnRes { … }` block).

- [ ] **Step 3: Drop the `.map(Into::into)` calls**

In the `TestRegionBuilder` `process_new_insn` and `process_insn` wrappers (around lines 480 and 491), change:

```rust
        pub fn process_new_insn(
            &mut self,
            insn: &rsleigh::Insn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_new_insn(insn, at, lift).map(Into::into)
        }
```

to:

```rust
        pub fn process_new_insn(
            &mut self,
            insn: &rsleigh::Insn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_new_insn(insn, at, lift)
        }
```

And similarly for `process_insn`.

- [ ] **Step 4: Run the consumers**

```bash
cargo build -p cfg
cargo test -p cfg --test region_builder_process
cargo test -p cfg
```
Expected: PASS — `ProcessInsnRes` resolves to the inner enum, all `assert_eq!`s and pattern-matches in the consuming tests still type-check and pass.

- [ ] **Step 5: Verify clippy is still clean**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "refactor(cfg): collapse test_api ProcessInsnRes duplicate into a pub use"
```

---

## Task 3: Collapse overlapping comments in `decode_branch_target` CONST arm

**Problem:** The CONST-space arm of [region_builder.rs:109-145](crates/cfg/src/cfg/builder/region_builder.rs#L109-L145) carries three comment blocks — one before the `match`, one inside the CONST arm, one before the bounds check — that overlap substantially. The pre-match block (lines 109-118) and the inside-arm block (lines 119-124) both describe the two's-complement-in-u64 encoding; the pre-match block and the after-cast block (lines 129-133) both describe the silent-skip consequence. Three blocks for what's really one explanation.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:109-145](crates/cfg/src/cfg/builder/region_builder.rs#L109-L145)

- [ ] **Step 1: Confirm the current state**

Read [crates/cfg/src/cfg/builder/region_builder.rs:99-145](crates/cfg/src/cfg/builder/region_builder.rs#L99-L145). The CONST arm should look like the description above (three comment blocks).

- [ ] **Step 2: Rewrite the CONST arm with a single consolidated comment**

Replace lines 109-145 (the start of the `match` through the end of the CONST arm) with:

```rust
        match branch_target_var.addr.space {
            // CONST-space: pcode-local relative branch. The "address" is a
            // signed offset on the *pcode index* within the same machine
            // instruction, two's-complement-encoded into the u64 `off` (so
            // `(-n) as u64` for backward branches). `cast_signed` is the
            // bit-pattern-preserving u64→i64 reinterpretation; `checked_add_signed`
            // catches either-direction overflow on the resulting index. The
            // bounds check then ensures the target is in `0..lift_res.insns.len()` —
            // an out-of-range index would otherwise be silently skipped by the
            // build loop, which would advance past the end of the current
            // machine instruction's pcode sequence and produce a wrong CFG with
            // no diagnostic.
            rsleigh::VnSpace::CONST => {
                let off = branch_target_var.addr.off.cast_signed();
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or(
                    ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
                )?;
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
            // Absolute branch: the offset IS the target machine address
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
```

This keeps every behavioural element identical — same arms, same error returns, same `Ok` shape — but consolidates the three comment blocks into one. The "Absolute branch" comment on the default-code-space arm is unchanged.

- [ ] **Step 3: Run the decode tests + full cfg suite**

```bash
cargo test -p cfg --test region_builder_decode
cargo test -p cfg
```
Expected: PASS — every decode test (negative-offset, underflow, past-end, at-end, default-code-space, register, unique, unknown space) continues to pass. The arithmetic and control flow are byte-identical.

- [ ] **Step 4: Verify clippy is still clean**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "docs(cfg): consolidate decode_branch_target CONST-arm comments into one block"
```

---

## Task 4: Drop `#[inline]` on `Builder::start_pcode_addr`

**Problem:** [crates/cfg/src/cfg/builder/mod.rs:94](crates/cfg/src/cfg/builder/mod.rs#L94) carries an `#[inline]` attribute on a `pub(super) fn` with one in-crate call site (`Builder::build`). For private intra-crate functions, `#[inline]` is at best decorative — rustc inlines small private functions at MIR level regardless. The annotation adds noise without informing the reader.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/mod.rs:93-100](crates/cfg/src/cfg/builder/mod.rs#L93-L100)

- [ ] **Step 1: Drop the attribute**

Change:

```rust
    /// Returns the pcode address corresponding to the function entry point.
    #[inline]
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
```

to:

```rust
    /// Returns the pcode address corresponding to the function entry point.
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
```

- [ ] **Step 2: Build + test**

```bash
cargo build -p cfg
cargo test -p cfg
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/mod.rs
git commit -m "style(cfg): drop unnecessary #[inline] on Builder::start_pcode_addr"
```

---

## Task 5: Drop the unused `_region` binding in `process_new_insn`'s Return arm

**Problem:** [crates/cfg/src/cfg/builder/region_builder.rs:269](crates/cfg/src/cfg/builder/region_builder.rs#L269) reads `let _region = self.finish_current_region(false)?;`. The result is unused. The `_region` name conventionally signals "I know this returns something, I'm not using it" — but the same can be expressed by dropping the `let` entirely, which is the more common Rust idiom.

**Files:**
- Modify: [crates/cfg/src/cfg/builder/region_builder.rs:268-271](crates/cfg/src/cfg/builder/region_builder.rs#L268-L271)

- [ ] **Step 1: Remove the binding**

Change:

```rust
            rsleigh::Opcode::Return => {
                let _region = self.finish_current_region(false)?;
                Ok(ProcessInsnRes::FinishedProcessing)
            }
```

to:

```rust
            rsleigh::Opcode::Return => {
                self.finish_current_region(false)?;
                Ok(ProcessInsnRes::FinishedProcessing)
            }
```

- [ ] **Step 2: Build + test**

```bash
cargo build -p cfg
cargo test -p cfg
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs
git commit -m "style(cfg): drop unused _region binding in process_new_insn's Return arm"
```

---

## Task 6: Doc-comment polish on `Cfg::vn_to_name` and `Result<T>`

**Problem:** Two small doc-comment issues:

1. [crates/cfg/src/cfg/dot.rs:10-12](crates/cfg/src/cfg/dot.rs#L10-L12) starts the doc with "Public entry — fetches Sleigh registers per call." But the method is `pub(super)`, not `pub`. The label is misleading.
2. [crates/cfg/src/error.rs:57](crates/cfg/src/error.rs#L57) reads `/// the result type using our error.` — lowercase first letter, no closing context, and unconventional (Rust convention is sentence-case doc comments).

**Files:**
- Modify: [crates/cfg/src/cfg/dot.rs:10-13](crates/cfg/src/cfg/dot.rs#L10-L13)
- Modify: [crates/cfg/src/error.rs:57-58](crates/cfg/src/error.rs#L57-L58)

- [ ] **Step 1: Update `Cfg::vn_to_name` doc**

Change:

```rust
    /// Public entry — fetches Sleigh registers per call. Suitable for ad-hoc
    /// callers; the DOT dumper uses [`vn_to_name_with_regs`] instead so
    /// it only fetches once per render.
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
```

to:

```rust
    /// Renders a varnode to a printable name. Fetches Sleigh registers per
    /// call. Used by the `test_api` forwarder for ad-hoc callers; the DOT
    /// dumper bypasses this and calls [`vn_to_name_with_regs`] directly so
    /// it only fetches the register list once per render.
    pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
```

- [ ] **Step 2: Update `Result<T>` doc**

Change:

```rust
/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
```

to:

```rust
/// Result type alias for fallible cfg operations, parameterised over the
/// success type and using [`Error`] for failures.
pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 3: Verify docs render**

```bash
cargo doc -p cfg --no-deps
```
Expected: PASS, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/cfg/src/cfg/dot.rs crates/cfg/src/error.rs
git commit -m "docs(cfg): polish Cfg::vn_to_name and Result<T> doc comments"
```

---

## Task 7: Final sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Lib-level pedantic must remain empty**

Run: `cargo clippy -p cfg --no-deps -- -W clippy::pedantic 2>&1 | grep -E '^warning:'`
Expected: empty (the lib stays pedantic-clean — none of the round-6 changes touch arithmetic).

- [ ] **Step 2: All-targets pedantic unchanged from round-5 baseline**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -W clippy::pedantic 2>&1 | tail -40`
Expected: any remaining warnings are pre-existing test-side noise carried from round 5. There should be **no new** warnings.

- [ ] **Step 3: cfg-only strict lint**

Run: `cargo clippy -p cfg --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace strict lint (cross-crate sanity)**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS (or pre-existing unrelated lints in other crates, unchanged from baseline).

- [ ] **Step 5: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — same content as before this round (every round-6 change is observationally equivalent to the previous behaviour).

---

## Out of scope (considered, rejected, or deferred)

Carried forward from round 5 (still rejected):
- **Sequential-decode-into-region-interior asymmetry.** Speculative; defer until found in the wild.
- **`vn_to_name_non_register` REGISTER arm is unreachable.** Defence-in-depth; keep.
- **`Cfg::sleigh` / `Cfg::graph` / `Cfg::entry` field privatisation.** `analyzer` reads them directly.
- **Consolidating the three `test_api` re-export tiers.** Round 4 rejected; reaffirmed.
- **Moving `region_builder.rs::test_api` into a sibling file.** Round 3/4 rejected; reaffirmed.
- **Replacing `format!("{e:?}")` in `GenericSleighError` with `Display` propagation.** Round 1 rejected; reaffirmed.
- **`OptionsBuilder::set_function_max_size(0)` rejection.** Degenerate but legal; reaffirmed.
- **Defending against `lift_res.insns.is_empty()` and `lift_res.machine_insn_len == 0`.** Trust rsleigh's contract; reaffirmed.
- **Erroring on duplicate `start_addr_to_region_id` insert in `add_region`.** Round 4 rejected; reaffirmed. (Production path can't trigger it; the invariant violation only matters to direct test_api consumers, who own their inputs.)
- **Renaming `is_branch_tail_call_nocheck` → `branch_target_is_outside_function`.** Round 4 rejected; reaffirmed.
- **Tightening `is_branch_tail_call_nocheck`'s saturating-add comment.** Verbose but accurate; keep.
- **Per-dump `regs` caching in `CfgDotDumper`.** Not hot in profiling; defer.

New for round 6:
- **Aligning the `usize → u64` cast style between `next_pcode_addr` (`as u64`) and `decode_branch_target` (`u64::try_from(...).unwrap_or(u64::MAX)`).** Round 5 chose each idiom deliberately for its local context: the saturating `try_from` matters in `decode_branch_target` because the `>=` bounds check would silently *accept* an in-bounds index on a hypothetical 32-bit target where `lift_res.insns.len()` exceeds `usize`'s range; in `next_pcode_addr` the same scenario would trigger the next-machine-insn branch, which is correct behaviour anyway. Reaffirm both choices; the inconsistency is intentional.
- **`add_region` returning `Err` on duplicate `start_addr` (correctness)**. Considered for round 6 (the production path can't trigger it but `test_api::add_region` could). Rejected on the same grounds as round 4 — it's a contract violation by direct test_api consumers, and per CLAUDE.md "Don't add error handling, fallbacks, or validation for scenarios that can't happen. Trust internal code and framework guarantees."
- **Removing `OptionsBuilder` and exposing `Options` fields directly.** `Options` is `Default + Copy + Clone`; the builder's `set_*`/`build` chain doesn't add value for this use case. Conventional but not free — it's a public-API reshape and an `analyzer`-side ripple. Defer until a concrete use case calls for it.
- **Refactoring `vn_to_name` / `vn_to_name_with_regs` / `vn_to_name_non_register` dispatch.** The current three-function structure is mildly redundant (the REGISTER-vs-not check happens in two places), but the redundancy supports the dot dumper's "regs once" call shape. Net change is a wash; skip.
