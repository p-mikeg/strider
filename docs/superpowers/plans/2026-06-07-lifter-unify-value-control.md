# Unify value + control lifter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Merge `ValueLifter` into the per-CFG `PerRegionDriver` so value
and control lifting are both `&mut self` methods on one struct. Delete the
`ValueLifter` struct, the `lift/vn_io.rs` wrapper, and the ~87 per-call
reconstructions.

**Sequencing:** the workspace won't compile until T1 finishes; tests won't
compile until T2. Gate per task with `cargo build`/`test` of the named
crate; full gate in T4.

---

### Task 1: Merge value lifting onto `PerRegionDriver`

**Files (move + edit):**
- Move `crates/strider-lift/src/pcode_lift/value/*.rs` →
  `crates/strider-lift/src/lift/value/*.rs` (git mv).
- Move `read_vn`/`write_vn` bodies from
  `crates/strider-lift/src/pcode_lift/vn_io.rs` onto `PerRegionDriver`
  (new `crates/strider-lift/src/lift/vn_io.rs`, replacing the wrapper).
- Keep the free utilities (`vn_sort_key`, `require_output_vn`,
  `first_input_or_err`, `nth_input_or_err`, `decode_space_id`) — relocate
  to `crates/strider-lift/src/lift/vn_util.rs` (or keep a thin
  `pcode_lift` module of just these). Delete the `ValueLifter` struct from
  `pcode_lift/mod.rs`.

- [ ] **Step 1: read_vn/write_vn → PerRegionDriver.** Replace
  `crates/strider-lift/src/lift/vn_io.rs` (the `value_lifter()` wrapper)
  with the real `read_vn`/`write_vn` bodies as
  `impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R>` methods (copied
  from `pcode_lift/vn_io.rs`, `self.builder`/`self.sleigh` unchanged).
  Delete `pcode_lift/vn_io.rs`. Delete the `value_lifter()` method.

- [ ] **Step 2: Retarget value handlers.** `git mv` the `value/` dir into
  `lift/`. In every moved file change
  `impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R>` →
  `impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R>`. Bodies use
  `self.builder` / `self.sleigh` / `self.read_vn` / `self.write_vn`, all of
  which exist on `PerRegionDriver` — no body edits expected. Fix the `use
  crate::pcode_lift::{…}` imports to the new util path.

- [ ] **Step 3: `value::lift` → method.** Convert the dispatch free fn in
  `value/mod.rs` to `impl PerRegionDriver { pub(crate) fn lift_value(&mut
  self, insn) -> Result<bool> }` (it already calls `self.handle_*`). Wire
  `lift/value` as a `mod value;` of `lift`.

- [ ] **Step 4: process_insn.** In `lift/insn/mod.rs`,
  `self.value_lifter().lift(insn)?` → `self.lift_value(insn)?`.

- [ ] **Step 5: Delete `ValueLifter`.** Remove the struct + impl from
  `pcode_lift/mod.rs`. Move the surviving free fns to `lift/vn_util.rs`
  (or retain `pcode_lift` as a utils-only module). Update `lib.rs`
  (`pub mod pcode_lift;` → as appropriate) and all in-crate
  `crate::pcode_lift::*` references.

- [ ] **Step 6: Build.** `cargo build -p strider-lift` (lib). Expected:
  compiles. (`cargo test -p strider-lift` still fails — T2.)

- [ ] **Step 7: Commit.** `refactor(strider-lift): merge ValueLifter into
  the per-CFG lifter`.

---

### Task 2: Move value-opcode tests in-crate

**Files:**
- Delete `crates/strider-lift/tests/value_lifter.rs`.
- Create `crates/strider-lift/src/lift/value/tests.rs` (`#[cfg(test)]`),
  porting all ~40 cases.

- [ ] **Step 1: `with_test_lifter` helper.** A `#[cfg(test)]` helper that
  owns a sleigh, a throwaway 1-region CFG (build `[0xc3]` ret at 0x1000
  via `strider_cfg::Builder::for_arch`), a default `Lifter` config
  (x86 cdecl or a synthetic CC), the `all_vns` (three test reg vns), and
  the merged `PerRegionDriver`, then calls `f(&mut driver)`. Owning the
  locals sidesteps returning a struct that borrows them.

- [ ] **Step 2: Port tests.** Each `ValueLifter::new(&mut b,&s).lift(i)`
  becomes `with_test_lifter(|l| { assert!(l.lift_value(&i)?); … })`.
  Graph-inspecting tests assert on `l`'s function inside the closure.
  Preserve every assertion verbatim — only the construction changes.

- [ ] **Step 3: Test.** `cargo test -p strider-lift`. Expected: same
  count as before (the ~40 value cases + the rest), 0 failures.

- [ ] **Step 4: Commit.** `test(strider-lift): move value-opcode tests
  in-crate onto the merged lifter`.

---

### Task 3: Downstream util path

**Files:** `crates/strider-orchestrator/src/orchestrator/mod.rs` (the
`strider_lift::pcode_lift::vn_sort_key` import).

- [ ] **Step 1:** Update the import to the relocated util path
  (`strider_lift::lift::vn_sort_key` or whatever T1 chose). Grep the
  workspace for any other `pcode_lift::` references and fix.

- [ ] **Step 2: Build.** `cargo build -p strider-orchestrator`.

- [ ] **Step 3: Commit.** `refactor(strider-orchestrator): vn_sort_key
  import follows the merged lift module`.

---

### Task 4: Full gate + review

- [ ] **Step 1: .so rebuild** (`cargo build -p strider-py && cp
  target/debug/libstrider_py.so
  crates/strider-py/strider/strider.abi3.so`).
- [ ] **Step 2: Gate.** `cargo test --workspace` (0 failures),
  `cargo clippy --workspace --all-targets` (clean),
  `cd crates/strider-py && uv run pytest -q` (832).
- [ ] **Step 3: CLAUDE.md.** Update the strider-lift entry: it has one
  lifter (no `ValueLifter`); the "Register Aliasing" section still points
  at `vn_io.rs` (now `lift/vn_io.rs`).
- [ ] **Step 4: Commit + final review subagent.**

---

## Self-review
- Behaviour-preserving: same IR; gate = existing tests pass, no
  assertion weakened.
- Type consistency: every `impl ValueLifter` → `impl PerRegionDriver`;
  `read_vn`/`write_vn`/`lift_value` are the only new method surfaces.
