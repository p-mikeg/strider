# Target Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clean up the `target` crate — fix a public-API naming typo, rename one inconsistent preset, narrow an over-coupled private field, satisfy the clippy lints already firing, and close an under-tested area (arch presets that never construct a Sleigh instance).

**Architecture:** Each task is a self-contained, independently-committable change. The crate's external API surface is the types re-exported through [analyzer/src/lib.rs:39](crates/analyzer/src/lib.rs#L39) (`BuiltCallingConvention`, `CallingConvention`, `Endianess`, `SleighArch`). All six `BuiltCallingConvention` fields are read externally (`ir`, `opt`) and `SleighArch.sla_spec` / `.pspec` / `.endianess` are all read by `analyzer` — so those stay `pub` and are not touched. The only private field in the crate is `CallingConvention`'s internals, which have room to shrink.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## Open questions for the reviewer before execution

Each of these changes a task below. Pick one per group.

**Q1 — `Endianess` → `Endianness` rename (Task 1):** The enum and its `endianess` field are both misspelled (English spelling is "Endianness"; `object::Endianness` in `reader` also uses the correct form).
  - **(A)** Rename both the type and the field. Touches 3 files: `crates/target/src/arch.rs`, `crates/analyzer/src/lib.rs` (re-export), `crates/analyzer/src/analyzer/register_aliasing.rs` (two match arms + one field access). Breaking for any yet-to-be-written Python consumer, but `strider-py` is still planned-only (see CLAUDE.md). **Default choice.**
  - **(B)** Leave the typo. Pin it with a comment so a future reviewer doesn't "fix" it and break callers.

**Q2 — `aarchbe64` → `aarch64be` rename (Task 2):** `mipsbe32`/`mipsle32` put the endianness before the bit width; `aarch64` (LE) uses the canonical name; `aarchbe64` awkwardly splits it.
  - **(A)** Rename to `aarch64be` (matches `aarch64` LE + conventional `<arch><endian>` suffix). Zero external callers use the current name (verified via grep), so this is effectively internal. **Default choice.**
  - **(B)** Leave as-is.

**Q3 — `CallingConvention::arch` field (Task 3):** Currently `CallingConvention` stores `arch: SleighArch` privately but `build()` only ever consults `self.arch.stack_ptr_reg_name`. The field is private, so this change is internal-only.
  - **(A)** Replace with `stack_ptr_reg_name: &'static str`. Removes 3 unused fields of coupling; each preset now encodes the one ABI-level fact it actually uses. **Default choice.**
  - **(B)** Keep `arch` — rationale: "a CC is arch-specific, so naming the arch documents intent." Counter: the preset function name (`aarch64_aapcs64`) already documents intent, and `build()` takes `&SleighRegs` which already ties it to an arch at resolve time.

**Q4 — SleighArch smoke test scope (Task 6):** `mipsbe32`, `mipsle32`, `aarch64be` (post-Task 2 name) never actually feed into `rsleigh::Sleigh::new` anywhere. If a `SLA_SPEC_*` / `PSPEC_*` constant were renamed upstream, these three presets would silently rot.
  - **(A)** Add one smoke test per preset that constructs a `Sleigh` and resolves the `stack_ptr_reg_name`. Seven tiny tests, same shape. **Default choice.**
  - **(B)** Only add the three currently-untested ones (mipsbe32, mipsle32, aarch64be). More surgical but leaves a pattern gap — future presets will be checked iff someone remembers to add them.

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Rename `Endianess` → `Endianness` (and `.endianess` → `.endianness`)

Public-API typo fix. Touches the type, the struct field, one re-export, and one consumer.

**Files:**
- Modify: [crates/target/src/arch.rs](crates/target/src/arch.rs)
- Modify: [crates/analyzer/src/lib.rs:39](crates/analyzer/src/lib.rs#L39)
- Modify: [crates/analyzer/src/analyzer/register_aliasing.rs:65-67](crates/analyzer/src/analyzer/register_aliasing.rs#L65-L67)

- [ ] **Step 1: Rename the type and field in `crates/target/src/arch.rs`**

Replace the top of the file with:

```rust
/// The byte order used by an architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endianness {
    /// Least-significant byte at the lowest address (x86, AArch64 LE, …).
    Little,
    /// Most-significant byte at the lowest address (MIPS BE, AArch64 BE, …).
    Big,
}

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

Then in every preset (`x86_64`, `x86`, `mipsbe32`, `mipsle32`, `arm`, `aarch64`, `aarchbe64`), rewrite each body to use the renamed type and field, e.g.:

```rust
pub fn x86_64() -> SleighArch {
    SleighArch {
        sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64,
        pspec: rsleigh::pspec::PSPEC_X86_64,
        endianness: Endianness::Little,
        stack_ptr_reg_name: "RSP",
    }
}
```

Repeat the same `endianness:` / `Endianness::Little` or `Endianness::Big` substitution in the other six presets.

- [ ] **Step 2: Update `crates/target/src/lib.rs` re-export**

Change [crates/target/src/lib.rs:21](crates/target/src/lib.rs#L21) from:

```rust
pub use arch::{Endianess, SleighArch};
```

to:

```rust
pub use arch::{Endianness, SleighArch};
```

- [ ] **Step 3: Update the `analyzer` re-export**

In [crates/analyzer/src/lib.rs:39](crates/analyzer/src/lib.rs#L39), change:

```rust
pub use target::{BuiltCallingConvention, CallingConvention, Endianess, SleighArch};
```

to:

```rust
pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
```

- [ ] **Step 4: Update the one consumer in `analyzer/register_aliasing.rs`**

In [crates/analyzer/src/analyzer/register_aliasing.rs:65-67](crates/analyzer/src/analyzer/register_aliasing.rs#L65-L67), change:

```rust
match self.analyzer.arch.endianess {
    crate::Endianess::Little => 8 * (reg.addr.off - container_reg.addr.off),
    crate::Endianess::Big => {
```

to:

```rust
match self.analyzer.arch.endianness {
    crate::Endianness::Little => 8 * (reg.addr.off - container_reg.addr.off),
    crate::Endianness::Big => {
```

- [ ] **Step 5: Run workspace-wide grep to catch any stragglers**

Run: `grep -rn "Endianess\|endianess" --include='*.rs' crates/`
Expected: empty output.

- [ ] **Step 6: Build and test the workspace**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/target/src/arch.rs crates/target/src/lib.rs crates/analyzer/src/lib.rs crates/analyzer/src/analyzer/register_aliasing.rs
git commit -m "refactor(target): fix Endianess → Endianness typo in public API"
```

---

## Task 2: Rename `aarchbe64` → `aarch64be`

Makes the big-endian AArch64 preset match the `<arch><endian>` suffix pattern that `mipsbe32`/`mipsle32` use (and that `aarch64` LE establishes).

**Files:**
- Modify: [crates/target/src/arch.rs:93-100](crates/target/src/arch.rs#L93-L100)

- [ ] **Step 1: Verify no external callers**

Run: `grep -rn "aarchbe64" --include='*.rs' crates/`
Expected: one hit only — the definition itself in `crates/target/src/arch.rs`.

- [ ] **Step 2: Rename the function**

In [crates/target/src/arch.rs](crates/target/src/arch.rs), change:

```rust
/// Returns the big-endian AArch64 architecture descriptor.
pub fn aarchbe64() -> SleighArch {
    SleighArch {
        sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
        pspec: rsleigh::pspec::PSPEC_AARCH64,
        endianness: Endianness::Little,
        stack_ptr_reg_name: "sp",
    }
}
```

to:

```rust
/// Returns the big-endian AArch64 architecture descriptor.
pub fn aarch64be() -> SleighArch {
    SleighArch {
        sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
        pspec: rsleigh::pspec::PSPEC_AARCH64,
        endianness: Endianness::Big,
        stack_ptr_reg_name: "sp",
    }
}
```

(Post-Task 1 field names; if Task 1 is deferred, substitute `endianess: Endianess::Big`. The existing preset already sets `Big` correctly — only the function name changes.)

- [ ] **Step 3: Build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/arch.rs
git commit -m "refactor(target): rename aarchbe64 → aarch64be for naming consistency"
```

---

## Task 3: Narrow `CallingConvention::arch` to `stack_ptr_reg_name`

`build()` currently reads nothing from `self.arch` except `stack_ptr_reg_name`. The `arch` field is private — no external code reads it.

**Files:**
- Modify: [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs)

- [ ] **Step 1: Change the struct shape**

In [crates/target/src/calling_convention.rs:25-41](crates/target/src/calling_convention.rs#L25-L41), replace:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    arch: crate::arch::SleighArch,
    arg_passing_regs: &'static [&'static str],
    callee_saved_regs: &'static [&'static str],
    ret_val_regs: &'static [&'static str],
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    stack_arg_offsets: &'static [i64],
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    ret_stack_pop: i64,
}
```

with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    /// The Sleigh register name of the hardware stack pointer.  Stored on
    /// the convention because every built convention needs the stack
    /// pointer's `Vn` resolved, and the `SleighArch` that would otherwise
    /// own this fact is already passed separately to `Analyzer::new`.
    stack_ptr_reg_name: &'static str,
    arg_passing_regs: &'static [&'static str],
    callee_saved_regs: &'static [&'static str],
    ret_val_regs: &'static [&'static str],
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    stack_arg_offsets: &'static [i64],
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    ret_stack_pop: i64,
}
```

- [ ] **Step 2: Update each preset constructor body**

In every preset (`x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `x86_cdecl`), replace the `arch: crate::arch::SleighArch::...()` line with the equivalent `stack_ptr_reg_name` literal:

- `x86_64_systemv_abi` → `stack_ptr_reg_name: "RSP",`
- `aarch64_aapcs64` → `stack_ptr_reg_name: "sp",`
- `arm_aapcs` → `stack_ptr_reg_name: "sp",`
- `x86_cdecl` → `stack_ptr_reg_name: "ESP",`

Example after edit for `x86_64_systemv_abi`:

```rust
pub fn x86_64_systemv_abi() -> CallingConvention {
    CallingConvention {
        stack_ptr_reg_name: "RSP",
        arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
        callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
        ret_val_regs: &["RAX", "RDX"],
        stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
        ret_stack_pop: 8,
    }
}
```

- [ ] **Step 3: Update `build()` to read the new field**

In [crates/target/src/calling_convention.rs:141](crates/target/src/calling_convention.rs#L141), replace:

```rust
let stack_ptr_name = self.arch.stack_ptr_reg_name;
```

with:

```rust
let stack_ptr_name = self.stack_ptr_reg_name;
```

- [ ] **Step 4: Update the four error-path tests**

In [crates/target/src/calling_convention.rs:336-372](crates/target/src/calling_convention.rs#L336-L372), both error-path tests build `CallingConvention` literals with `arch: ...`. Replace each such `arch: crate::arch::SleighArch::x86_64(),` line with `stack_ptr_reg_name: "RSP",` (matching the arch those tests were probing).

- [ ] **Step 5: Run the target-crate tests**

Run: `cargo test -p target`
Expected: PASS (5 tests; all were passing before, and we haven't changed any behavior externally observable).

- [ ] **Step 6: Confirm the whole workspace still builds**

Run: `cargo build --workspace`
Expected: PASS (no external code referenced the private `arch` field).

- [ ] **Step 7: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "refactor(target): narrow CallingConvention::arch to stack_ptr_reg_name"
```

---

## Task 4: Add `#[must_use]` to the 11 preset constructors

Satisfies 11 of the 12 currently-firing clippy errors on the crate.

**Files:**
- Modify: [crates/target/src/arch.rs](crates/target/src/arch.rs)
- Modify: [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs)

- [ ] **Step 1: Annotate every `SleighArch` preset**

In [crates/target/src/arch.rs](crates/target/src/arch.rs), add `#[must_use]` directly above each of the seven `pub fn ...() -> SleighArch` lines: `x86_64`, `x86`, `mipsbe32`, `mipsle32`, `arm`, `aarch64`, and (post-Task 2) `aarch64be`.

Pattern:

```rust
#[must_use]
pub fn x86_64() -> SleighArch {
    ...
}
```

- [ ] **Step 2: Annotate every `CallingConvention` preset**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), add `#[must_use]` above each of the four preset functions: `x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `x86_cdecl`.

- [ ] **Step 3: Verify clippy's `must_use_candidate` lint is now clean**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings -A clippy::missing_errors_doc`
Expected: PASS (we've masked the remaining `missing_errors_doc` error, which Task 5 will fix).

- [ ] **Step 4: Run tests**

Run: `cargo test -p target`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/target/src/arch.rs crates/target/src/calling_convention.rs
git commit -m "refactor(target): add #[must_use] to preset constructors"
```

---

## Task 5: Add `# Errors` doc to `CallingConvention::build`

Satisfies the last remaining clippy error.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:131-137](crates/target/src/calling_convention.rs#L131-L137)

- [ ] **Step 1: Append an `# Errors` paragraph**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace the existing doc block above `pub fn build(...)` with:

```rust
/// Resolves all register name strings in this calling convention to their
/// concrete [`rsleigh::Vn`] varnodes using `sleigh_regs`.
///
/// The number of varnodes in each resulting list equals the length of the
/// corresponding name list.
///
/// # Errors
///
/// Returns [`ErrorKind::UnknownRegName`] if any register name listed in
/// this convention (including the stack pointer) does not resolve against
/// `sleigh_regs`.  The resolution short-circuits on the first failure.
pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
```

- [ ] **Step 2: Verify the full strict lint now passes**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS (no errors, no warnings).

- [ ] **Step 3: Run tests**

Run: `cargo test -p target`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): document CallingConvention::build error conditions"
```

---

## Task 6: Smoke-test every `SleighArch` preset

Currently only `x86_64`, `x86`, `arm`, and `aarch64` are exercised (through the `CallingConvention` table-driven tests). `mipsbe32`, `mipsle32`, and `aarch64be` (post-Task 2 name) are never fed to `rsleigh::Sleigh::new`, so a typo in any of their `SLA_SPEC_*` / `PSPEC_*` / `stack_ptr_reg_name` constants would silently rot.

**Files:**
- Create: [crates/target/tests/arch_smoke.rs](crates/target/tests/arch_smoke.rs)

- [ ] **Step 1: Write the new test file**

Create `crates/target/tests/arch_smoke.rs` with:

```rust
//! Smoke tests: every [`target::SleighArch`] preset must successfully feed
//! into `rsleigh::Sleigh::new` and resolve its documented stack pointer
//! register.  Without this, presets that nothing else exercises (e.g.
//! `mipsbe32`, `mipsle32`, `aarch64be`) could silently rot if an upstream
//! constant were renamed.

use target::SleighArch;

fn assert_preset_resolves(label: &str, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap_or_else(|e| panic!("{label}: Sleigh::new failed: {e:?}"));
    let regs = sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{label}: Sleigh::regs failed: {e:?}"));
    assert!(
        regs.name_to_vn(arch.stack_ptr_reg_name).is_some(),
        "{label}: stack_ptr_reg_name {:?} must resolve",
        arch.stack_ptr_reg_name,
    );
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

Note: if Task 2 has not been merged, use `SleighArch::aarchbe64()` for the last test instead.

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p target --test arch_smoke`
Expected: 7 tests PASS. If any fails, it points directly at a rotted preset — fix the offending constant in `crates/target/src/arch.rs` before proceeding.

- [ ] **Step 3: Run the full crate test suite**

Run: `cargo test -p target`
Expected: 5 unit tests + 7 smoke tests = 12 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/target/tests/arch_smoke.rs
git commit -m "test(target): smoke-test every SleighArch preset against rsleigh::Sleigh::new"
```

---

## Task 7: Consolidate stack-pointer lookup into `regs_to_vns`

The stack-pointer resolution in `build()` is a one-off open-coded version of what `regs_to_vns` already does. Small readability win, and it means the error-path behavior for stack-pointer-name failures goes through the same code path as every other name lookup (same error wording, same short-circuit semantics).

**Files:**
- Modify: [crates/target/src/calling_convention.rs:137-153](crates/target/src/calling_convention.rs#L137-L153)

- [ ] **Step 1: Replace the body of `build()`**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace the existing `build()` body (post-Tasks 3 and 5) with:

```rust
pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
    let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
    let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
    let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
    let [stack_ptr_vn] =
        regs_to_vns(&[self.stack_ptr_reg_name], sleigh_regs)?
            .try_into()
            .expect("regs_to_vns returns exactly one Vn for a one-name slice");
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

Note on the `.expect()`: per the user's "no panic or unwrap in normal error paths" rule, this is acceptable because the invariant ("a one-element input to `regs_to_vns` returns a one-element `Vec` on success") is a pure structural fact about the helper, not a runtime assumption about the input. If the reviewer prefers to be strict, swap the destructure for:

```rust
let mut sp = regs_to_vns(&[self.stack_ptr_reg_name], sleigh_regs)?;
let stack_ptr_vn = sp.pop().expect("regs_to_vns returns one Vn for a one-name slice");
```

or (zero-panic variant) keep the old open-coded lookup — in which case skip this task entirely.

**Q5 for the reviewer:** is the consolidation worth the `.expect()` on a structural invariant?
  - **(A)** Yes — one path for all name lookups, clearer failure messages. **Default choice.**
  - **(B)** No — keep the open-coded two-line lookup; structural assertions are still runtime panics.

Assume (A) unless the reviewer picks (B).

- [ ] **Step 2: Run tests**

Run: `cargo test -p target`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "refactor(target): route stack-pointer lookup through regs_to_vns"
```

---

## Task 8: Final workspace sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Strict lint on the target crate**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings (Tasks 4 and 5 clear the 12 baseline errors; Tasks 1–3, 6, 7 should not have introduced new ones).

- [ ] **Step 2: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 3: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Workspace lint (informational)**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS if no other crate has a latent lint; otherwise the remaining warnings are outside this review's scope — flag to the reviewer, do not fix here.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced.

---

## Out of scope (considered, rejected or deferred)

- **Privatizing `BuiltCallingConvention` fields.** Verified that all six fields (`arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`, `stack_ptr_vn`, `stack_arg_offsets`, `ret_stack_pop`) are read externally by `ir/src/builder/mod.rs`, `ir/src/builder/call.rs`, `ir/src/function.rs`, `ir/src/dot/label.rs`, `opt/src/function_args.rs`, `opt/src/stack_store.rs`, `opt/src/stack_load_forward.rs`. Privatizing would require one accessor per field with no behavioral change. Cost > benefit.
- **Replacing `Vec<i64>` / `Vec<Vn>` in `BuiltCallingConvention` with `&'static` slices / `Cow`.** The cloning in `opt/src/stack_store.rs:422` and `opt/src/function_args.rs:78-80` is per-pipeline-construction (not per-match), so the allocation is cold. Adding lifetime parameters to `BuiltCallingConvention` would propagate through every consumer. Not worth it.
- **Adding `Endianness ↔ object::Endianness` bridge.** `reader` uses `object::Endianness` directly; `analyzer` uses `target::Endianness` directly; nothing today needs to convert. Add the bridge when the first consumer needs it.
- **Adding a MIPS calling convention preset.** `SleighArch::mipsbe32` / `mipsle32` exist but no CC preset pairs with them. Out of scope — no user has asked; adding one without a test binary to validate against would be guesswork.
- **Validating CC–arch coherence at `build()` time** (e.g., refusing `x86_64_systemv_abi().build(&arm_regs)`). Already caught by `name_to_vn` returning `None` for mismatched register names. A dedicated check adds no signal.
- **Pruning `arm()` docstring reference to `binary_tests/arch/arm.mk`.** Borderline — it's implementation detail leaking into doc. Left alone because it's load-bearing context for anyone debugging why `ARM8_le` + `ARM_v45` specifically (rather than some other plausible pair).
