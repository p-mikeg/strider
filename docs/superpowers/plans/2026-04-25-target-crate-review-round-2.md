# Target Crate Review — Round 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the residual correctness and readability gaps left after round 1 of the `target` crate review: two mismatched preset docstrings, one orphaned field (`SleighArch::stack_ptr_reg_name`), two micro-cleanups in `CallingConvention::build`, and the missing per-field doc coverage on `BuiltCallingConvention`.

**Architecture:** Each task is a self-contained, independently-committable change. No external callers of the `target` crate read `SleighArch::stack_ptr_reg_name` — verified by grep across `crates/` (only `crates/target/tests/arch_smoke.rs` references it). Every other external API (`SleighArch::sla_spec` / `.pspec` / `.endianness`, the full `BuiltCallingConvention` field set, every preset constructor) is used and stays untouched.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## Decisions locked in by the reviewer

- **Q-A (SleighArch::stack_ptr_reg_name):** Remove the field. Single source of truth becomes `CallingConvention::stack_ptr_reg_name`.

---

## Task 1: Fix incorrect preset docstrings

`x86_64_systemv_abi` and `aarch64_aapcs64` both list registers in their doc-comments that the code deliberately does not include (stack pointers are excluded from `callee_saved_regs` — the `presets_stack_pointer_and_arg_offsets` test explicitly pins this).

**Files:**
- Modify: [crates/target/src/calling_convention.rs:60-64](crates/target/src/calling_convention.rs#L60-L64)
- Modify: [crates/target/src/calling_convention.rs:80-84](crates/target/src/calling_convention.rs#L80-L84)

- [ ] **Step 1: Fix the `x86_64_systemv_abi` docstring**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace the doc block above `pub fn x86_64_systemv_abi()`:

```rust
    /// Returns the x86-64 System V ABI calling convention.
    ///
    /// Argument registers: RDI, RSI, RDX, RCX, R8, R9
    /// Callee-saved: RBX, RSP, RBP, R12–R15
    /// Return value: RAX, RDX
    #[must_use]
    pub fn x86_64_systemv_abi() -> CallingConvention {
```

with:

```rust
    /// Returns the x86-64 System V ABI calling convention.
    ///
    /// Argument registers: RDI, RSI, RDX, RCX, R8, R9
    /// Callee-saved: RBX, RBP, R12–R15
    /// Return value: RAX, RDX
    ///
    /// RSP is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret` pops the return address, so the caller observes
    /// SP shifted by `ret_stack_pop` across the call.
    #[must_use]
    pub fn x86_64_systemv_abi() -> CallingConvention {
```

- [ ] **Step 2: Fix the `aarch64_aapcs64` docstring**

In the same file, replace the doc block above `pub fn aarch64_aapcs64()`:

```rust
    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register), sp
    /// Return value: x0, x1
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
```

with:

```rust
    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    /// Return value: x0, x1
    ///
    /// `sp` is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret_stack_pop` is `0` on AAPCS64 because `bl` writes
    /// the return address to `lr` rather than pushing it.
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
```

- [ ] **Step 3: Confirm the tests still pin the stated invariant**

Run: `cargo test -p target presets_stack_pointer_and_arg_offsets -- --nocapture`
Expected: PASS. This test already asserts `!built.callee_saved_regs.contains(&built.stack_ptr_vn)` for every case, which is what the corrected docstrings now say.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): correct callee-saved register lists in preset docstrings"
```

---

## Task 2: Remove `SleighArch::stack_ptr_reg_name` and simplify `arch_smoke.rs`

The field became orphaned after round 1's Task 3 moved stack-pointer resolution onto `CallingConvention`. Only `tests/arch_smoke.rs` touches it today; leaving it in place creates two independent "what is the SP for arch X?" facts that can drift.

**Files:**
- Modify: [crates/target/src/arch.rs](crates/target/src/arch.rs)
- Modify: [crates/target/tests/arch_smoke.rs](crates/target/tests/arch_smoke.rs)

- [ ] **Step 1: Verify the field has no production consumer**

Run: `grep -rn "stack_ptr_reg_name" --include='*.rs' crates/`
Expected: hits only in `crates/target/src/arch.rs` (field decl + 7 preset literals), `crates/target/src/calling_convention.rs` (the field on `CallingConvention` — different field, same name), and `crates/target/tests/arch_smoke.rs`. If any other file appears, STOP and report — the plan assumption is wrong.

- [ ] **Step 2: Remove the field from `SleighArch`**

In [crates/target/src/arch.rs](crates/target/src/arch.rs), replace the struct definition:

```rust
/// A collection of Sleigh configuration items that together describe a
/// specific target architecture.
///
/// Pass a `SleighArch` to [`crate::Analyzer::new`] along with the calling
/// convention to build an analyser for that target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    /// The `.sla` specification for the architecture's instruction set.
    pub sla_spec: rsleigh::sla_spec::SlaSpec,
    /// The `.pspec` processor specification (register and space definitions).
    pub pspec: rsleigh::pspec::PSpec,
    /// The byte order of this architecture.
    pub endianness: Endianness,
    /// The Sleigh register name of the hardware stack pointer.
    pub stack_ptr_reg_name: &'static str,
}
```

with:

```rust
/// A collection of Sleigh configuration items that together describe a
/// specific target architecture.
///
/// Pass a `SleighArch` to [`crate::Analyzer::new`] along with the calling
/// convention to build an analyser for that target.  The calling convention
/// owns the stack-pointer register name (see
/// [`crate::CallingConvention::build`]) rather than the arch, so that
/// `CallingConvention::build` is self-contained and different ABIs on the
/// same arch can in principle declare different SP registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    /// The `.sla` specification for the architecture's instruction set.
    pub sla_spec: rsleigh::sla_spec::SlaSpec,
    /// The `.pspec` processor specification (register and space definitions).
    pub pspec: rsleigh::pspec::PSpec,
    /// The byte order of this architecture.
    pub endianness: Endianness,
}
```

- [ ] **Step 3: Drop the field from every preset body**

In the same file, delete the `stack_ptr_reg_name: "…",` line from each of the seven `SleighArch { … }` literals (`x86_64`, `x86`, `mipsbe32`, `mipsle32`, `arm`, `aarch64`, `aarch64be`). Example for `x86_64`:

```rust
    /// Returns the x86-64 (64-bit Intel/AMD) architecture descriptor.
    #[must_use]
    pub fn x86_64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64,
            pspec: rsleigh::pspec::PSPEC_X86_64,
            endianness: Endianness::Little,
        }
    }
```

Repeat for the other six presets — only the `stack_ptr_reg_name` line goes away; leave `sla_spec`, `pspec`, and `endianness` alone.

- [ ] **Step 4: Simplify `arch_smoke.rs`**

Replace the full contents of [crates/target/tests/arch_smoke.rs](crates/target/tests/arch_smoke.rs) with:

```rust
//! Smoke tests: every [`target::SleighArch`] preset must successfully feed
//! into `rsleigh::Sleigh::new` and produce a usable register table.  Without
//! this, presets that nothing else exercises (e.g. `mipsbe32`, `mipsle32`,
//! `aarch64be`) could silently rot if an upstream constant were renamed.
//!
//! Stack-pointer resolution is covered by the `calling_convention` tests in
//! the crate's unit-test module — this file intentionally does not assert it,
//! because the SP name lives on `CallingConvention`, not `SleighArch`.

#![allow(clippy::panic, clippy::unwrap_used)]

use target::SleighArch;

fn assert_preset_resolves(label: &str, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap_or_else(|e| panic!("{label}: Sleigh::new failed: {e:?}"));
    sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{label}: Sleigh::regs failed: {e:?}"));
}

#[test]
fn x86_64_preset_resolves() {
    assert_preset_resolves("x86_64", SleighArch::x86_64());
}

#[test]
fn x86_preset_resolves() {
    assert_preset_resolves("x86", SleighArch::x86());
}

#[test]
fn mipsbe32_preset_resolves() {
    assert_preset_resolves("mipsbe32", SleighArch::mipsbe32());
}

#[test]
fn mipsle32_preset_resolves() {
    assert_preset_resolves("mipsle32", SleighArch::mipsle32());
}

#[test]
fn arm_preset_resolves() {
    assert_preset_resolves("arm", SleighArch::arm());
}

#[test]
fn aarch64_preset_resolves() {
    assert_preset_resolves("aarch64", SleighArch::aarch64());
}

#[test]
fn aarch64be_preset_resolves() {
    assert_preset_resolves("aarch64be", SleighArch::aarch64be());
}
```

- [ ] **Step 5: Run the target-crate tests**

Run: `cargo test -p target`
Expected: 5 unit tests + 7 smoke tests = 12 PASS.

- [ ] **Step 6: Confirm the workspace still builds and tests pass**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. No crate outside `target` referenced the removed field — verified in Step 1.

- [ ] **Step 7: Commit**

```bash
git add crates/target/src/arch.rs crates/target/tests/arch_smoke.rs
git commit -m "refactor(target): drop orphaned SleighArch::stack_ptr_reg_name"
```

---

## Task 3: Tidy `build()`'s stack-pointer lookup

Two small cleanups:
1. The `let stack_ptr_name = self.stack_ptr_reg_name;` rebinding is a one-use alias — inline it.
2. `.ok_or(ErrorKind::UnknownRegName(stack_ptr_name.to_string()))` unconditionally allocates a `String`. `.ok_or_else(|| …)` only pays the allocation on the error path. This also matches what `regs_to_vns` does one function above in the same file — the inconsistency is the whole tell.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:150-166](crates/target/src/calling_convention.rs#L150-L166)

- [ ] **Step 1: Rewrite `build()`**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace the body of `pub fn build(...)`:

```rust
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
        let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
        let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
        let stack_ptr_name = self.stack_ptr_reg_name;
        let stack_ptr_vn = sleigh_regs
            .name_to_vn(stack_ptr_name)
            .ok_or(ErrorKind::UnknownRegName(stack_ptr_name.to_string()))?;
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
            ret_stack_pop: self.ret_stack_pop,
        })
    }
```

with:

```rust
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
        let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
        let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
        let stack_ptr_vn = sleigh_regs
            .name_to_vn(self.stack_ptr_reg_name)
            .ok_or_else(|| ErrorKind::UnknownRegName(self.stack_ptr_reg_name.to_string()))?;
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
            ret_stack_pop: self.ret_stack_pop,
        })
    }
```

- [ ] **Step 2: Run the target tests**

Run: `cargo test -p target`
Expected: PASS. The existing `build_returns_error_for_unknown_register_name` and `build_returns_error_even_when_some_names_are_valid` tests already pin the error behavior we just kept.

- [ ] **Step 3: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "refactor(target): simplify build() stack-pointer lookup"
```

---

## Task 4: Add per-field docs on `BuiltCallingConvention`

`CallingConvention` has per-field docs for the non-obvious fields (`stack_arg_offsets`, `ret_stack_pop`). `BuiltCallingConvention` has the *same* fields in resolved form but no docs — a reader who lands on the "built" type has to hop back to the source type. Fix by mirroring the docs and adding a type-level link.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:47-57](crates/target/src/calling_convention.rs#L47-L57)

- [ ] **Step 1: Replace the `BuiltCallingConvention` definition**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
    pub stack_ptr_vn: rsleigh::Vn,
    pub stack_arg_offsets: Vec<i64>,
    pub ret_stack_pop: i64,
}
```

with:

```rust
/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
///
/// Produced by [`CallingConvention::build`].  The field semantics mirror
/// [`CallingConvention`]; see that type's field docs for details.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// Varnodes for the ABI's argument-passing registers, in positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Varnodes the callee must preserve across the call.  Excludes the
    /// stack pointer; SP's callee-side preservation is expressed through
    /// [`Self::ret_stack_pop`] instead.
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// Varnodes used to return a value to the caller, in positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// The hardware stack-pointer varnode (e.g. `RSP` on x86-64, `sp` on
    /// AArch64).  Deliberately not listed in [`Self::callee_saved_regs`].
    pub stack_ptr_vn: rsleigh::Vn,
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    pub stack_arg_offsets: Vec<i64>,
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    pub ret_stack_pop: i64,
}
```

- [ ] **Step 2: Verify docs build cleanly**

Run: `cargo doc -p target --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links. `[`CallingConvention::build`]` and `[`CallingConvention`]` should resolve within the same file.

- [ ] **Step 3: Run tests**

Run: `cargo test -p target`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): add per-field docs to BuiltCallingConvention"
```

---

## Task 5: Final workspace sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Strict lint on the target crate**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 2: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 3: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced without error.

- [ ] **Step 5: Workspace lint (informational)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS if no other crate has a latent lint; otherwise the remaining warnings are outside this review's scope — flag to the reviewer, do not fix here.

---

## Out of scope (considered, rejected, or deferred)

- **Route the stack-pointer resolution through `regs_to_vns`.** Round 1's deferred Task 7. Declined: consolidating into `let [vn] = regs_to_vns(...)?` requires `.expect()` on a structural "one-in → one-out" invariant, which violates the user's memory-recorded rule *"no panic/unwrap in normal error paths"*. The two-line open-coded lookup is clearer *and* panic-free.
- **Runtime invariant checks in `build()`** (e.g., no duplicates within a list; `stack_ptr_reg_name` disjoint from `arg_passing_regs` / `callee_saved_regs` / `ret_val_regs`). `CallingConvention`'s fields are crate-private, so external callers can only construct one via a preset. The three existing invariant tests (`presets_resolve_correct_register_sets`, `presets_resolved_registers_have_expected_size`, `presets_stack_pointer_and_arg_offsets`) already pin these properties for every bundled preset; nothing outside the crate can ship a malformed CC through `build()`.
- **`Vec<…>` → `Box<[…]>` in `BuiltCallingConvention`.** Saves 8 bytes per slice field, requires touching 4 `from_convention` consumer sites in `opt` (`stack_store.rs`, `function_args.rs`, `stack_load_forward.rs`), no measurable runtime win.
- **`ErrorKind::UnknownRegName(String)` → `&'static str`.** All names are statically known today, so the allocation is pointless on paper — but adding a lifetime parameter to `ErrorKind` would cascade through `analyzer::Error` and the `strider_error` bridge machinery. Not worth it until a different error needs the same treatment.
- **Privatizing `BuiltCallingConvention` fields.** Already covered in round 1's "out of scope" section — all six fields are read externally by `ir` and `opt` pipelines. Privatizing would mean one accessor per field with zero behavioral change.
- **Adding a MIPS calling convention preset.** Round 1 already deferred this — no MIPS CC preset pairs with `SleighArch::mipsbe32` / `mipsle32`, but we don't have a MIPS test binary to validate the choice against.
