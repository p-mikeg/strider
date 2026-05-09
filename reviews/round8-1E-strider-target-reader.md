# Round 8 / 1E — `strider` + `target` + `reader` audit

**Branch:** `review/ai2`.  Independent audit; round-7 reviews not consulted.

## Coverage

All source files in scope plus `crates/target/tests/*` and `crates/reader/tests/*` inspected fully.  `crates/strider/tests/**` (38 files) fully read.  ABI specs (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PowerPC ELF v1/v2, Intel SDM, ARM ARM) consulted as references.

## Findings

### MED: `apply_elf_relocations` generic symbol path mislabels malformed-ELF failures as `skipped_unresolved_target`

- **Severity:** MED (callers diagnosing relocation outcomes see a misleading bucket).
- **Where:** `crates/reader/src/elf.rs:554-574` (generic path) vs `:519-524` (GOT/PLT path).
- **What's wrong:** When `RelocationTarget::Symbol(idx)` in the generic (non-GOT/PLT) path has its `symbol_by_index` call return `Err` (out-of-range index = malformed ELF), the code increments `stats.skipped_unresolved_target` rather than `stats.skipped_malformed_target`.

  Doc-strings (`crates/reader/src/elf.rs::RelocationStats`):
  - `skipped_unresolved_target`: "legitimately unresolvable at static-analysis time — typically undefined externs, weak symbols."
  - `skipped_malformed_target`: "ELF is malformed (the relocation references an index `symbol_by_index` rejects)."

  The GOT/PLT-slot path uses `skipped_malformed_target` for the same condition (Err from `symbol_by_index`).  The generic path is inconsistent.
- **Verified against:** `object::File::symbol_by_index` doc — returns `Err` for out-of-range indices.
- **Fix:**
  ```rust
  let Some((addr, undef)) = resolved else {
      stats.skipped_malformed_target += 1;  // not skipped_unresolved_target
      continue;
  };
  ```

### MED: `locate_and_write` uses unchecked `site_addr + size_bytes as u64` in region-coverage check

- **Severity:** MED (theoretically can wrap on a malformed ELF mapping near `u64::MAX`; in practice unreachable).
- **Where:** `crates/reader/src/elf.rs:930`.
- **What's wrong:** `find(|r| r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())` — the addition is not guarded.  Debug build would panic on overflow; release would silently wrap, allowing the predicate to spuriously succeed.  `MemRegion::new` correctly uses `checked_add` for region construction; the inconsistency is notable.
- **Verified against:** `MemRegion::new` constructor uses `addr.checked_add(data.len() as u64)`.
- **Fix:**
  ```rust
  .find(|r| {
      r.contains(site_addr)
          && site_addr
              .checked_add(size_bytes as u64)
              .is_some_and(|end| end <= r.end_addr())
  })
  ```

## Minor / informational (LOW; not action items)

- **`stall_budget` reset to 0 on Rebuild with empty unresolved.**  `crates/strider/src/orchestrator.rs:441` sets `self.stall_budget = self.unresolved.len()`.  If a Rebuild empties unresolved, budget becomes 0.  Reachable only if the next `step()` runs before `no_unresolved()` short-circuits — verified that `run()` (lines 173-175) AND each `step()` exit on `FixedPoint` before the stall guard fires.  No observable bug.

- **`x86_64_linux_syscall::callee_saved_regs` inherits SystemV unchanged.**  Linux kernel preserves `RBX/RBP/R12-R15` across a syscall; the inheritance is correct.  RCX/R11 (clobbered by the `syscall` instruction) handled in the CallOther ABI table, not the CC.  A clarifying comment would help readers but the code is correct.

- **`GraphRewriter::apply_rule` doc-string drift.**  Says "wraps the wrapped graph into a short-lived `BuiltFunctionGraph` per call (via `mem::take`)" — the implementation now creates a `pattern::RewriteCtx` per node iteration (post-round-7 RewriteCtx newtype work).  Doc-only.

## Areas verified correct

- **Orchestrator fixed-point**: `LoopState::step` semantics; `Decision::{FixedPoint, StableOnly, Rebuild}`; `cap = 2 * pending_at_iter_0 + 4` is a sound over-approximation; `edge_set_of` uses `BTreeSet` for stable comparison.
- **`apply_in_place_edits`**: dispatch for `LinkRegister` / `Single(K)` (tail-call); `Multiple` rejected with explicit error.
- **`build_anchor_calling_context` / `override_clobber_vars`**: clobber projection consistent across call sites; `initial_var_index` hoist is correctly per-iteration.
- **`Strider: Clone`**: shallow clone over `BuiltCallingConvention`/`SleighArch`/`SleighRegs` — all `Clone`/`Copy` value types; no shared mutable state.
- **`build_switch_if_ladder`**: control-chain correct including N=1 degenerate case.
- **CC presets**: every register name verified against canonical ABI specs.  x86_64 SysV `RDI/RSI/RDX/RCX/R8/R9`, AArch64 `x0-x7, x19-x28, x29, x30`, ARM `r0-r3, r4-r11, lr`, MIPS o32 `a0-a3, s0-s8, gp, ra`, MIPS n64 `a0-a3 + t0-t3` (Sleigh names), PPC SysV32 `r3-r10, r14-r31, LR`, PPC ELFv1 stack args at +48 with r2 as TOC, PPC ELFv2 stack args at +32.  `ret_stack_pop` correct: 8 on x86_64, 4 on x86, 0 on link-register ISAs.
- **CallOther ABI**: every entry in `classify_arch_specific` and `classify_arch_independent` cross-verified against ISA references.  `swi` ARM/x86 split correct.  `syscall` x86_64 reads `RAX/RDI/RSI/RDX/R10/R8/R9`, writes `RAX/RCX/R11` per Intel SDM.  `sysret` `NoReturn` correct for kernel analysis.  `swapgs` `PURE_WITH_MEM_EDGE` correct.
- **`ElfFileMemReader::read`**: endianness handling at `:327-342` correct for both LE (bytes at low end) and BE (bytes at high end) with `from_le_bytes`/`from_be_bytes`.
- **`apply_elf_relocations_autoload` / `_with_extender`**: two-pass approach correct (collect missing → extend → apply).  Staged-region dedup at `:671-673` covers pre-existing + newly-staged.  SHT_NOBITS exclusion correct.
- **`MemRegion::read`**: `available == 0` guard never fires from a `contains(addr)`-true input.
- **`RegionIndex::region_for_placeholder`**: control-input lookup correct.

## Summary

- **2 MED** — relocation classify mislabel; `locate_and_write` overflow.
- **3 LOW informational** — stall budget Rebuild reset (no bug; reasoning verified); kernel-syscall CC comment gap; `apply_rule` doc-string drift.
- 11+ areas verified correct.
