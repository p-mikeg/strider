# Target Crate Review — Round 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the residual test-coverage and doc-consistency gaps left after rounds 1 & 2 of the `target` crate review. The crate's *production code* is in good shape after the previous rounds (clippy clean with `-D warnings`, 12 tests pass); the remaining issues are all concentrated in the test module and one docstring.

**Architecture:** Four small, independently-committable changes. Three harden the existing table-driven tests in `crates/target/src/calling_convention.rs`; one tidies the `x86_cdecl` preset docstring for consistency with the other three.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## What the review found (findings → tasks)

| # | Finding | Evidence | Task |
|---|---------|----------|------|
| F1 | The `aarch64_aapcs64` preset is public API, used by `crates/analyzer/tests/analyze_binary.rs:216`, but is not in the `cases()` table. None of the three invariant tests (`presets_resolve_correct_register_sets`, `presets_resolved_registers_have_expected_size`, `presets_stack_pointer_and_arg_offsets`) exercise it. Changing the preset's registers, `stack_arg_offsets`, or `ret_stack_pop` by mistake is silently accepted. | [calling_convention.rs:224-263](crates/target/src/calling_convention.rs#L224-L263) shows only x86-64 SysV / x86 cdecl / ARM AAPCS; [calling_convention.rs:112-123](crates/target/src/calling_convention.rs#L112-L123) shows the untested preset. | Task 1 |
| F2 | `presets_stack_pointer_and_arg_offsets` asserts `!callee_saved_regs.contains(&stack_ptr_vn)` but not `!arg_passing_regs.contains(&stack_ptr_vn)` or `!ret_val_regs.contains(&stack_ptr_vn)`. The docstring above `BuiltCallingConvention::callee_saved_regs` says SP is "deliberately not listed"; the test only pins that for one of the three lists. | [calling_convention.rs:354-358](crates/target/src/calling_convention.rs#L354-L358) | Task 2 |
| F3 | `build()` has a distinct code path for looking up `stack_ptr_reg_name` (line 182-184, using `ok_or_else`) separate from `regs_to_vns`. Its error behavior is documented (the `# Errors` paragraph explicitly calls out "including the stack pointer"), but no test exercises it — the two existing error tests only poison `arg_passing_regs`. If the SP lookup is ever regressed (e.g., refactor drops the `?`), nothing fails. | [calling_convention.rs:174-193](crates/target/src/calling_convention.rs#L174-L193), [calling_convention.rs:376-412](crates/target/src/calling_convention.rs#L376-L412) | Task 3 |
| F4 | `x86_64_systemv_abi`, `aarch64_aapcs64`, and `arm_aapcs` all have a trailing paragraph explaining why the stack pointer is not in `callee_saved_regs`. `x86_cdecl` does not. This is the same omission rounds 1/2 started fixing for the other presets; `x86_cdecl` slipped through. | [calling_convention.rs:148-165](crates/target/src/calling_convention.rs#L148-L165) | Task 4 |

### Considered and rejected

- **Runtime disjointness check in `build()`** (e.g., refusing a CC where `stack_ptr_reg_name` also appears in `arg_passing_regs`). Round 2's "out of scope" section already rejected this — fields are crate-private and tests pin the invariant on every preset. Task 2 extends that pinning to all three register lists, which is the right layer.
- **Cross-checking `SleighArch::endianness` against the `.sla_spec`.** `rsleigh::Sleigh` does not expose an endianness accessor (verified: `crates/rsleigh/src/lib.rs` has `sla_spec()` and `pspec()` but no `endianness()`). No way to assert consistency without parsing the SLA blob ourselves. Deferred — would require an rsleigh change.
- **Adding a MIPS calling convention preset.** Same as round 1/2 — no MIPS test binary.
- **`Vec<…>` → `Box<[…]>` in `BuiltCallingConvention`.** Same as round 2 — touches 4 consumer sites in `opt`, no measurable win.
- **Renaming `regs_to_vns` to `reg_names_to_vns`.** The current name is shorter and the function is 9 lines; the ambiguity is resolved by reading the signature. Not worth the churn.
- **Adding `#[track_caller]` to `build()`.** The call site already returns a rich `strider_error::Error` with backtrace. No need.
- **Caching `SleighRegs` construction in tests via `OnceLock`.** Each `Sleigh::new` call is ~ms-level. Full `cargo test -p target` runs in ~0.3s cold. Premature.
- **Consolidating the two error-path tests (`build_returns_error_for_unknown_register_name` + `build_returns_error_even_when_some_names_are_valid`) into one parameterized helper.** They already share one `regs_for` call; folding would save ~15 lines but costs clarity of what each test pins. Leave.

---

## Task 1: Add AArch64 AAPCS64 to the `cases()` table

Closes F1. Extends the table-driven invariant tests to cover the one remaining preset.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:224-263](crates/target/src/calling_convention.rs#L224-L263)

- [ ] **Step 1: Extend `cases()` with an AArch64 AAPCS64 entry**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace the existing `cases()` function (lines 224-263):

```rust
    fn cases() -> Vec<Case> {
        vec![
            Case {
                name: "x86-64 SysV",
                cc: CallingConvention::x86_64_systemv_abi,
                arch: crate::arch::SleighArch::x86_64,
                arg_count: 6,
                callee_saved_count: 6,
                ret_count: 2,
                reg_size_bytes: 8,
                stack_ptr_name: "RSP",
                stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
                ret_stack_pop: 8,
            },
            Case {
                name: "x86 cdecl",
                cc: CallingConvention::x86_cdecl,
                arch: crate::arch::SleighArch::x86,
                arg_count: 0,
                callee_saved_count: 4,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "ESP",
                stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
                ret_stack_pop: 4,
            },
            Case {
                name: "ARM AAPCS",
                cc: CallingConvention::arm_aapcs,
                arch: crate::arch::SleighArch::arm,
                arg_count: 4,
                callee_saved_count: 9,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
                ret_stack_pop: 0,
            },
        ]
    }
```

with:

```rust
    fn cases() -> Vec<Case> {
        vec![
            Case {
                name: "x86-64 SysV",
                cc: CallingConvention::x86_64_systemv_abi,
                arch: crate::arch::SleighArch::x86_64,
                arg_count: 6,
                callee_saved_count: 6,
                ret_count: 2,
                reg_size_bytes: 8,
                stack_ptr_name: "RSP",
                stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
                ret_stack_pop: 8,
            },
            Case {
                name: "x86 cdecl",
                cc: CallingConvention::x86_cdecl,
                arch: crate::arch::SleighArch::x86,
                arg_count: 0,
                callee_saved_count: 4,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "ESP",
                stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
                ret_stack_pop: 4,
            },
            Case {
                name: "ARM AAPCS",
                cc: CallingConvention::arm_aapcs,
                arch: crate::arch::SleighArch::arm,
                arg_count: 4,
                callee_saved_count: 9,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
                ret_stack_pop: 0,
            },
            Case {
                name: "AArch64 AAPCS64",
                cc: CallingConvention::aarch64_aapcs64,
                arch: crate::arch::SleighArch::aarch64,
                arg_count: 8,
                callee_saved_count: 12,
                ret_count: 2,
                reg_size_bytes: 8,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[0, 8, 16, 24],
                ret_stack_pop: 0,
            },
        ]
    }
```

Numeric sources (each pulled from the `aarch64_aapcs64` preset at [calling_convention.rs:112-123](crates/target/src/calling_convention.rs#L112-L123)):
- `arg_count: 8` — `["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"]`
- `callee_saved_count: 12` — `["x19" … "x30"]`
- `ret_count: 2` — `["x0", "x1"]`
- `reg_size_bytes: 8` — AArch64 is 64-bit
- `stack_arg_offsets: &[0, 8, 16, 24]` — preset literal
- `ret_stack_pop: 0` — preset literal

- [ ] **Step 2: Run the crate's test suite**

Run: `cargo test -p target`
Expected: 5 unit tests + 7 smoke tests = 12 PASS. All three invariant tests now iterate over 4 cases instead of 3, so the run count stays at 12 but each test internally does more work.

If any assertion fails, the case row numbers above are wrong relative to the preset — fix the row, not the preset (the preset is the source of truth that the integration test in `analyzer/tests/analyze_binary.rs` already exercises end-to-end).

- [ ] **Step 3: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "test(target): cover AArch64 AAPCS64 preset in invariant tests"
```

---

## Task 2: Pin stack-pointer disjointness across all three register lists

Closes F2. The existing assertion only covers `callee_saved_regs`; extend it to `arg_passing_regs` and `ret_val_regs`. The docstring on `BuiltCallingConvention::callee_saved_regs` (lines 56-58) says SP is "deliberately not listed", but the invariant we actually want is stronger: SP does not appear in *any* resolved register list.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:354-358](crates/target/src/calling_convention.rs#L354-L358)

- [ ] **Step 1: Tighten the assertion in `presets_stack_pointer_and_arg_offsets`**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
            assert_eq!(built.stack_ptr_vn, sp, "{}: stack_ptr_vn", c.name);
            assert!(
                !built.callee_saved_regs.contains(&built.stack_ptr_vn),
                "{}: stack pointer must not be listed as callee-saved",
                c.name,
            );
```

with:

```rust
            assert_eq!(built.stack_ptr_vn, sp, "{}: stack_ptr_vn", c.name);
            for (label, set) in [
                ("arg_passing_regs", &built.arg_passing_regs),
                ("callee_saved_regs", &built.callee_saved_regs),
                ("ret_val_regs", &built.ret_val_regs),
            ] {
                assert!(
                    !set.contains(&built.stack_ptr_vn),
                    "{}: stack pointer must not appear in {label}",
                    c.name,
                );
            }
```

- [ ] **Step 2: Run the target tests**

Run: `cargo test -p target presets_stack_pointer_and_arg_offsets -- --nocapture`
Expected: PASS on all 4 cases. If it fails for any preset, a real invariant is broken — investigate before editing the test.

- [ ] **Step 3: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "test(target): assert stack pointer is absent from all resolved reg lists"
```

---

## Task 3: Cover the stack-pointer error path in `build()`

Closes F3. The `ok_or_else` branch at [calling_convention.rs:184](crates/target/src/calling_convention.rs#L184) is documented to return `UnknownRegName` but has no test guarding it.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:376-412](crates/target/src/calling_convention.rs#L376-L412)

- [ ] **Step 1: Add a new test immediately after the existing two error tests**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), after `build_returns_error_even_when_some_names_are_valid` and before the closing `}` of `mod tests` (so the test lands at the end of the module body — around line 412), insert:

```rust
    /// An unknown `stack_ptr_reg_name` must surface as `UnknownRegName`, the
    /// same way an unknown entry in any of the three register lists does.
    /// Guards the open-coded `ok_or_else` in `build()` — the SP name has its
    /// own lookup path separate from `regs_to_vns`.
    #[test]
    fn build_returns_error_for_unknown_stack_pointer_name() {
        let regs = regs_for(crate::arch::SleighArch::x86_64());
        let cc = CallingConvention {
            stack_ptr_reg_name: "NOT_A_SP",
            arg_passing_regs: &[],
            callee_saved_regs: &[],
            ret_val_regs: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        };
        let result = cc.build(&regs);
        assert!(
            matches!(
                result.as_ref().map_err(|e| e.kind()),
                Err(ErrorKind::UnknownRegName(n)) if n == "NOT_A_SP"
            ),
            "expected UnknownRegName(\"NOT_A_SP\"), got {result:?}"
        );
    }
```

- [ ] **Step 2: Run the target tests**

Run: `cargo test -p target`
Expected: 6 unit tests + 7 smoke tests = 13 PASS (one new unit test).

- [ ] **Step 3: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "test(target): cover unknown stack_ptr_reg_name in build()"
```

---

## Task 4: Add the stack-pointer caveat to `x86_cdecl`'s docstring

Closes F4. The other three presets explain why SP isn't in `callee_saved_regs`. Mirror that for `x86_cdecl` so a reader landing on any preset gets the same picture.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:148-165](crates/target/src/calling_convention.rs#L148-L165)

- [ ] **Step 1: Replace the `x86_cdecl` doc block**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// Return value: EAX, EDX
    #[must_use]
    pub fn x86_cdecl() -> CallingConvention {
```

with:

```rust
    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// Callee-saved: EBX, ESI, EDI, EBP
    /// Return value: EAX, EDX
    ///
    /// ESP is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret` pops the 4-byte return address, so the caller
    /// observes SP shifted by `ret_stack_pop` across the call.
    #[must_use]
    pub fn x86_cdecl() -> CallingConvention {
```

- [ ] **Step 2: Build the docs cleanly**

Run: `cargo doc -p target --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links.

- [ ] **Step 3: Run tests**

Run: `cargo test -p target`
Expected: PASS (13 unit+smoke tests).

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): mirror stack-pointer caveat on x86_cdecl preset"
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

All items from rounds 1 and 2's out-of-scope lists remain out of scope. Additionally:

- **Endianness self-consistency with `sla_spec`.** `rsleigh::Sleigh` exposes `sla_spec()` / `pspec()` but no endianness accessor. Without an upstream change we'd have to parse the SLA blob ourselves. Defer; reconsider if an rsleigh change lands.
- **Caching `SleighRegs` construction in tests** (e.g., `OnceLock<SleighRegs>` per arch). `cargo test -p target` completes in ~0.3s cold — no actual perf problem.
- **Consolidating the two error-path tests.** Each pins a distinct behavior (single-bad-name; short-circuit when some names are valid). Folding them loses that granularity.
- **Renaming `regs_to_vns` → `reg_names_to_vns`.** Pure naming churn; the signature already disambiguates.
- **Switching `#[error("... {0:?}")]` to plain `{0}` on `UnknownRegName`.** The debug formatting is load-bearing for empty / whitespace names (renders as `""` vs invisible). Keep.
- **Adding a MIPS calling convention preset.** Same rationale as prior rounds — no MIPS test binary.
