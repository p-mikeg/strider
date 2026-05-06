# Round 7 — strider + target + reader Audit

Independent review (code-only) of `crates/strider`, `crates/target`, `crates/reader` cross-checked against `../rsleigh/sleigh/src/**`.

---

## CRITICAL

### C1 — `GraphRewriter::apply_rule` builds a `BuiltFunctionGraph` with empty `variables` / `call_clobbered` / `ret_val_regs` — HIGH (conf 80)
- **Where:** `crates/strider/src/rewrite.rs:130-139`
- **Evidence:** Comment claims `pattern::rewrite_rule` only touches `graph` and `entry` ("verified by inspection"). No type-level guard. If a future pattern rule reads `tmp.variables` / `tmp.call_clobbered` / `tmp.ret_val_regs`, it sees empty Vecs silently. Match accessors that consult function-level metadata may also misbehave.
- **Fix:** Either change `BuiltFunctionGraph::from_graph_and_entry` to require these fields explicitly, or wrap in a `RewriteCtx` newtype that exposes only `graph`/`entry`, eliminating the contract leak.

### C2 — `handle_call_other` indexes `node_outputs(node)[1]` (magic memory slot) — MED (conf 80)
- **Where:** `crates/strider/src/strider/insn/mod.rs:204`
- **Evidence:** `build_call_other_modeled` (`ir/src/builder/call.rs:266-272`) puts Control at slot 0 and Memory at slot 1. Today this is correct. Magic-number indexing breaks silently if the layout ever changes.
- **Fix:** Add a named accessor on the IR builder (`call_other_modeled_memory_output(node)`) or a debug assertion that slot 1 is `NodeOutputKind::Memory`.

---

## HIGH

### H1 — `swapgs` classified `PURE` (no memory edge) — HIGH (conf 85)
- **Where:** `crates/target/src/call_other_abi.rs:299`
- **Evidence:** Intel SDM Vol.2B: SWAPGS exchanges `GS.base` with `IA32_KERNEL_GS_BASE`. All subsequent `%gs:`-relative loads/stores depend on the new base. Without `memory_edge: true`, `StackLoadForward` (and other forwarding passes) can reorder/eliminate `%gs:`-relative accesses across `swapgs`.
- **Fix:** Reclassify `swapgs` as `PURE_WITH_MEM_EDGE` (matching `writefsbase`/`writegsbase`).

### H2 — `sysret` classified `NoReturn` — HIGH (conf 87)
- **Where:** `crates/target/src/call_other_abi.rs:248`
- **Evidence:** `SYSRET` returns from a `SYSCALL`-entered kernel handler to user mode. It IS the architectural return path of syscalls. Marking it `NoReturn` corrupts every kernel function ending in `sysret` (e.g. `do_syscall_64` fast path).
- **Fix:** Reclassify as `Call(CallOtherAbi { implicit_reads: &["RCX","R11"], implicit_writes: &[], memory_edge: false })`. Or: add a `Return`-class for SYSRET-shaped instructions.

### H3 — MIPS N64 `arg_passing_regs` lists `t0..t3` — needs Sleigh-table verification — MED (conf 50)
- **Where:** `crates/target/src/calling_convention/mod.rs:334`
- **Evidence:** ABI standard names `$8..$11` as `a4..a7` for N64. Code uses `t0..t3` (older MIPS naming). If Sleigh's mips64 spec resolves `t0..t3` to `$8..$11`, behavior is correct at the `Vn` level — but doc-mismatch is a footgun.
- **Fix:** Verify against `../rsleigh/sleigh` register table. Document the naming choice or rename to `a4..a7` if the Sleigh table allows.

### H4 — `aarch64_aapcs64.callee_saved_regs` includes `x30` (LR) — MED (conf 82)
- **Where:** `crates/target/src/calling_convention/mod.rs:224-226`
- **Evidence:** AAPCS64 §6.1.1: x30 must be preserved across calls *only* in the sense that the callee's prologue saves it. `bl` itself overwrites x30. Listing x30 as callee-saved excludes it from `Call` clobber outputs. Patterns reading post-call `x30` get the pre-call value via `InitialVar(x30)` instead of a fresh clobber output.
- **Fix:** Remove `x30` from `callee_saved_regs`. The link-register save/restore protocol is already modelled by the indirect-resolve "link-register-return" classifier path; Call clobbering should be conservative.

### H5 — Stall-budget not reset across `Rebuild` transitions — LOW (conf 50)
- **Where:** `crates/strider/src/orchestrator.rs:349-353,396-403`
- **Evidence:** Budget initialised once at `build_iter_0`. After a `Rebuild` (graph reset, indirect-branch placeholders may re-appear), continuing to deplete the same budget across `StableOnly` iterations could `bail!` prematurely on a legitimate slow-resolving function.
- **Fix:** Reset `stall_budget` on every `Rebuild` decision.

### H6 — `apply_elf_relocations_autoload` calls `obj.dynamic_relocations()` twice — LOW (conf 20)
- **Where:** `crates/reader/src/elf.rs:690-710`
- **Evidence:** Today `object` returns a fresh iterator on each call; safe in practice. Brittle assumption.

---

## MEDIUM

### M3 — `PyMemoryMap::ReadOnlyMemory::read` always little-endian — HIGH (conf 88, severity HIGH for BE targets)
- **Where:** `crates/strider-py/src/reader.rs:567-579`
- **Evidence:** `from_le_bytes(buf)` regardless of architecture. `ElfFileMemReader` correctly uses `is_little_endian` (`reader/src/elf.rs:328-342`). Big-endian Python pipeline (MIPS BE, AArch64 BE, ARM BE) silently byte-swaps `LoadReadOnly` constants.
- **Fix:** Carry endianness on `PyMemoryMap` (or its underlying region table) and switch `from_le_bytes` / `from_be_bytes` accordingly.

### M4 — Tier-1/2 terminology in production (`orchestrator.rs:5,32,213`; `indirect_resolve/{mod,inplace,classify}.rs`; `strider/{pipeline,mod,insn/mod,insn/control}.rs`)
- **Fix:** Rename to lift-time vs IR-level resolution. Concrete proposal:
  - `IrStrider::unresolved_branches` → `pending_indirect_branches`
  - `SpecialTerm::Unresolved` (cfg) → `SpecialTerm::PendingIndirect`
  - "tier-2 fixed-point loop" → "indirect-resolution fixed-point loop"
  - "tier 1 mini-graph" → "lift-time indirect-resolver" or "cfg-time placeholder resolver"

### M6 — `add_region_from_elf(apply_relocations=True)` vs `apply_elf_relocations` autoload mismatch — MED (conf 82)
- **Where:** `crates/strider-py/src/reader.rs:213-215` vs `:270`
- **Evidence:** One-step path uses `elf_load_with_relocations` (no autoload); two-step uses `apply_elf_relocations_autoload`. Same binary, different output sections covered.
- **Fix:** Make `add_region_from_elf` accept an `autoload: bool = True` flag and route through autoload by default, matching CLAUDE.md's stated default.

---

## LOW

### L1 — `orchestrator.rs:901,903` `expect`s — TEST ONLY (`#[cfg(test)] mod tests`). No issue.

### L2 — `MemRegionsLookupTable::read` O(n) worst case on overlapping regions — accepted design.

### L3 — `arm_aapcs.callee_saved_regs` includes `lr` — same structural concern as H4 but lower impact since AAPCS strongly preserves `lr` via prologue/epilogue.

### M2 — `cpuid` `PURE`+no-mem-edge — accepted; cpuid does not write RAM and register writes use `write_vn` chain.

### M5 — `re_optimize` has no type-level prevention against passing the destructive pipeline. Doc-only contract.

---

## Verified-Correct

### Orchestrator
- Fixed-point loop bound `cap = 2*pending_at_iter_0 + 4` polynomial.
- `Decision { FixedPoint, StableOnly, Rebuild }` predicate logic correct.
- `RegionIndex` rebuilt on `rebuild()`.
- `AnalyzeOutcome` correctly captures handles before `build()` consumes builder.

### indirect_resolve
- Thin shim delegating to `opt::classify_anchor_with_rom_and_sp` and `opt::{apply_link_register, apply_tail_call}`.
- `UnresolvedIndirectBranchError { addr: PcodeInsnAddr }` is inspectable.

### IrStrider
- `set_lift_addr(Some(machine_addr)) … set_lift_addr(None)` funnel correctly wraps `process_insn_inner` (`strider/insn/mod.rs:35-39`).
- Terminator handlers reuse last-insn addr.
- `vn_io.rs` is correctly thin (delegates to `pcode_lift::ValueLifter`).

### target presets
- `SleighArch` endianness correctly assigned.
- All `ArchPreset` variants mapped.
- CC presets: x86_cdecl / x86_64_systemv / aarch64_aapcs64 / arm_aapcs / mips_o32 / mips_n64 plus Linux kernel + syscall variants — structurally correct except for the `x30` / `lr` callee-saved inclusion (H4 / L3).

### reader
- ELF section loading correct.
- `apply_elf_relocations` and `_autoload` correct (modulo H6 cosmetic concern).
- `MemReader` trait shape matches rsleigh's.
- `MemRegion::read` bounds-checked.
- No production panics in reader.

### Python parity
- All 15 SleighArch presets exposed (incl. ppc).
- All CC presets exposed (incl. Linux kernel + syscall).
- `apply_elf_relocations` Python uses autoload variant.
- `strider.run` exposed.
- `UnresolvedIndirectBranchError` typed.

---

## Top Findings

1. **C1 (HIGH)** `GraphRewriter` empty-fields contract leak.
2. **H2 (HIGH)** `sysret` misclassified as NoReturn — corrupts kernel-syscall function graphs.
3. **H1 (HIGH)** `swapgs` missing memory edge — allows incorrect forwarding across base swap.
4. **M3 (MED→HIGH)** Python `MemoryMap` always little-endian — silently corrupts big-endian analyses.
5. **H4 (MED)** AArch64 `x30` listed callee-saved — pollutes Call clobber set.
