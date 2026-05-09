# Round 10 — `strider` + `target` + `reader`

Reviewing all `.rs` files under `crates/strider/src/`, `crates/strider/tests/`, `crates/strider/benches/`, `crates/target/src/`, `crates/target/tests/`, and `crates/reader/src/`.

---

## CRITICAL

### C-1 — `sysret` classified as `NoReturn` — control-flow truncation in every kernel syscall handler

- **Severity:** HIGH
- **Where:** `crates/target/src/call_other_abi.rs:257`
- **What's wrong:** `"sysret" => NO_RETURN` terminates the region as a trap. Intel SDM Vol.2B: `SYSRET` is the architectural fast-return path from a `SYSCALL`-entered kernel handler — it loads `RIP` from `RCX`, `RFLAGS` from `R11`, and transfers control back to user mode. It is not a trap; the kernel function executes, sets up `RCX`/`R11`, and `SYSRET` leaves. Every Linux kernel function whose fast-path ends with `swapgs; sysretq` (e.g. `do_syscall_64`) will have its CFG artificially truncated. The plan notes "if pcode contains a `Return` opcode after `CallOther(sysret)`, NoReturn is correct AND harmless" — but the diagnostic test `dump_sysret_trap.rs` is unconditionally `#[ignore]` and was never run. **NOTE:** Round 9 verification (`reviews/round9-fix-verification.md`) already determined this is FALSE — sysret correctly terminates the analyzed kernel function for single-function analysis (control transfers to a different address space, different CPL). Re-affirming here for round 10's audit but verdict stands: **deferred / by-design**.
- **Action:** No fix.

---

## IMPORTANT

### I-1 — `step()` stall-guard captures `prev_unresolved_len` AFTER `apply_in_place_edits`

- **Severity:** MED
- **Where:** `crates/strider/src/orchestrator.rs:418-444`
- **What's wrong:** Line 418 calls `apply_in_place_edits`. Line 419 captures `prev_unresolved_len = self.unresolved.len()`. Line 420 calls `recompute_unresolved` which drains `self.unresolved`. Line 444 passes `prev_unresolved_len` to `apply_stall_guard` as `unresolved_before`. The name says "previous" (before this step) but is read AFTER `apply_in_place_edits`. Currently OK because `apply_in_place_edits` doesn't mutate `self.unresolved`, but a future change could prune the list and trigger spurious stall-guard errors.
- **Fix:** Move the capture to before `apply_in_place_edits`.

### I-2 — `apply_elf_relocations_with_extender` may leave `regions` partially extended on `apply_elf_relocations` error

- **Severity:** MED
- **Where:** `crates/reader/src/elf.rs:664-697`
- **What's wrong:** Pass 1 builds the staged region list. `regions.extend(staged)` runs before `apply_elf_relocations(regions, obj)`, which can also `Err`. Callers see a partially-extended `regions` on error. The function's contract is unclear about whether the regions are rolled back.
- **Fix:** Save `regions.len()` before `extend` and `truncate` on error; or document that partial extension is allowed.

### I-3 — `aarch64_linux_syscall` retains `x30` (lr) in `callee_saved_regs` while clearing `link_register_reg_name`

- **Severity:** MED (documentation)
- **Where:** `crates/target/src/calling_convention/mod.rs:892-902`
- **What's wrong:** Inherits `callee_saved_regs` from AAPCS64 including x30, but sets `link_register_reg_name = None`. The `try_from_parts` validator requires `link_register_vn` to be IN `callee_saved_regs` when set, but the inverse (x30 in callee_saved with lr=None) is unchecked. Inconsistent intent; harmless but misleading.
- **Fix:** Remove `x30` from `callee_saved_regs` for the syscall preset, OR document that x30 is intentionally retained because the kernel preserves it.

### I-4 — `GraphRewriter::apply_rule` uses partial-state `RewriteCtx` — undocumented constraint on rule closures

- **Severity:** MED
- **Where:** `crates/strider/src/rewrite.rs:126`
- **What's wrong:** Rule closure receives a `RewriteCtx<'_>` built via `pattern::RewriteCtx::new(&mut *self.graph, self.entry)` — does not go through `for_built`. Doc says `pattern::rewrite_rule` "only touches `graph` and `entry`, verified by inspection" — informal guarantee, not API contract. A future rule that queries `call_clobbered` or `ret_val_regs` on the context would silently see empty data.
- **Fix:** Add an inline comment + debug-assert or document the constraint in `apply_rule`'s public contract.

### I-5 — `apply_elf_relocations` misses MIPS/PPC RELATIVE variants

- **Severity:** MED
- **Where:** `crates/reader/src/elf.rs:824-866`
- **What's wrong:** `image_relative_reloc` handles X86_64/I386/Aarch64/Arm RELATIVE relocations. MIPS (`R_MIPS_REL32` = type 3) and PPC64 (`R_PPC64_RELATIVE` = type 22) have analogous RELATIVE families but fall through to `None`. On a MIPS or PPC64 ET_DYN binary with function-pointer tables, these surface as `RelocationKind::Unknown` or `skipped_unsupported_kind`. The binary's function-pointer dispatch tables read zero, causing the indirect-branch resolver to see `IntConst(0)` targets.
- **Fix:** Extend `image_relative_reloc` (and `got_or_plt_slot_reloc_size`) with MIPS/MIPS64/PowerPC64 RELATIVE arms.

### I-6 — `locate_spliced_call` silently returns `None` for non-standard `ControlState → Call → Return` chains

- **Severity:** LOW
- **Where:** `crates/strider/src/orchestrator.rs:712-720`
- **What's wrong:** Walks one level up from the `Return`'s control input and checks if producer is a `Call`. Misses the `ControlState → Call → Return` chain when there is an intervening `ControlState`. `apply_tail_call` may produce exactly this shape; the per-address CC clobber override would silently not be recorded.
- **Fix:** Walk two levels: check `ctrl_in → producer`; if `ControlState`, also check the ControlState's own ctrl input.

### I-7 — `test_utils.rs` missing MIPS/PPC wrappers

- **Severity:** LOW
- **Where:** `crates/strider/src/test_utils.rs:1-64`
- **What's wrong:** Provides `strider_x86_64`, `strider_x86`, `strider_aarch64`, `strider_arm` — four of nine supported arch families. Unit tests for MIPS/PPC must hand-roll the setup or import from `tests/common/mod.rs`. `orchestrator.rs:999-1003`'s `make_strider_x86_64` duplicates `strider_x86_64` (predates `test_utils.rs`).
- **Fix:** Add `strider_mips_o32` / `strider_mips_n64` / `strider_ppc32` / `strider_ppc64le`. Deduplicate `make_strider_x86_64`.

### I-8 — `GraphRewriter::re_optimize` silently allows destructive passes mid-session

- **Severity:** LOW (documentation)
- **Where:** `crates/strider/src/rewrite.rs:176-178`
- **Fix:** Document constraint and rename example to `Strider::build_destructive_optimizer_pipeline`.

### I-9 — `SortedVns` newtype is `#[allow(dead_code)]` — migration stalled

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:96-143`
- **What's wrong:** Round 9 P3 introduced the newtype. The `#[allow(dead_code)]` will suppress compiler warnings indefinitely, masking the incomplete migration.
- **Fix:** Either complete the migration (change `AnalyzeOptions::all_vns` to `Option<SortedVns>` and update callers) or remove `SortedVns` and add a runtime sort-validation assert.

### I-10 — `eprintln!` in `find_loadable_section_containing` — unstructured library-side stderr output

- **Severity:** LOW
- **Where:** `crates/reader/src/elf.rs:779-782`
- **What's wrong:** Library crate prints to stderr on parse failure during autoload search; calling code merely sees no region returned and counts as `skipped_no_region`, masking the parse error.
- **Fix:** Propagate the parse failure through the extender's `Result<Option<MemRegion>>` return type, or use a tracing/log facade.

### I-11 — `dump_sysret_trap.rs` exploratory tests remain `#[ignore]` with no assertions

- **Severity:** LOW
- **Where:** `crates/strider/tests/dump_sysret_trap.rs:10-54`
- **What's wrong:** `dump_x86_64_sysret_pcode` and `dump_arm_trap_pcode` produce human-readable output but have no assertions. Diagnostic-only; clutter the test suite. The sysret classification controversy persisted because verification was deferred.
- **Fix:** Add pcode-shape assertions, convert to non-ignored tests, or delete.

---

## Coverage

| File | Status |
|---|---|
| `crates/strider/src/orchestrator.rs` | Fully |
| `crates/strider/src/rewrite.rs` | Fully |
| `crates/strider/src/test_utils.rs` | Fully |
| `crates/strider/src/indirect_resolve/{mod,classify,inplace}.rs` | Fully |
| `crates/strider/src/strider/pipeline.rs` | Fully |
| `crates/strider/src/strider/mod.rs` | Fully |
| `crates/strider/src/strider/insn/mod.rs` | Partially |
| `crates/strider/src/strider/insn/control.rs` | Partially |
| `crates/strider/src/strider/vn_io.rs` | Not |
| `crates/strider/src/errors.rs` | Fully |
| `crates/strider/src/lib.rs` | Fully |
| `crates/strider/src/rewrite_tests.rs` | Partially |
| `crates/strider/tests/orchestrator_indirect_branch.rs` | Partially |
| `crates/strider/tests/dump_sysret_trap.rs` | Fully |
| `crates/strider/tests/common/mod.rs` | Partially |
| `crates/target/src/call_other_abi.rs` | Fully |
| `crates/target/src/calling_convention/mod.rs` | Fully |
| `crates/target/src/calling_convention/tests.rs` | Fully |
| `crates/target/src/arch.rs` | Not |
| `crates/target/src/lib.rs` | Not |
| `crates/reader/src/elf.rs` | Fully |
| `crates/reader/src/lib.rs` | Fully |
