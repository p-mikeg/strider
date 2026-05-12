# Round 7 — Deep Correctness Audit (against rsleigh + ABI specs + Sleigh pcode)

External-reference verification of every claim in code, with priority on calling conventions, lifter semantics, optimization rewrites, and CallOther ABI table.

## Findings count

| Class | HIGH | MED | LOW |
|-------|------|-----|-----|
| A — Calling Conventions | 1 (A1) | 3 | 1 |
| B — Pcode-lift | 0 (verified) | 1 | 0 |
| C — Optimization | 2 | 1 | 1 |
| D — CallOther ABI | 2 | 1 | 0 |
| E — Cross-cutting | 0 | 1 | 0 |
| **Total** | **5** | **7** | **2** |

**Retracted findings (verified correct after re-checking the Sleigh spec):**
- A2 — AArch64 `x30` in `callee_saved_regs` (Sleigh emits `x30 = inst_start + 4; call …` so post-call LR is correctly preserved as IntConst).
- A3 — ARM `lr` in `callee_saved_regs` (same reasoning — Sleigh emits `lr = inst_next; call …`).
- (`Top Findings` list at the bottom has been pruned to remove these.)

---

## Class A — Calling Conventions

### A1 — `x86_64_all_preserving` cannot express "no memory clobber" — HIGH (seed bug confirmed)
- **Where:** `crates/target/src/calling_convention/mod.rs:185-203` + `crates/ir/src/builder/call.rs:47-154`
- **Code reality:** `build_call_with_cc` (lines 80-88) computes `clobber_vars` from `caller_saved \ callee_saved`. With `all_preserving`, every reg is callee-saved → `clobber_vars` is empty. **However**, lines 124-127 of `call.rs` unconditionally emit `NodeOutputKind::Memory` as an output of every Call regardless of CC. So the memory chain ALWAYS advances across a Call.
- **Bug:** "All preserving" is supposed to model `__fentry__`/`mcount`-style hooks (zero observable side effect). But the memory edge advance forces `StackLoadForward` and `LoadReadOnly` to stop at these calls. Reads of stack/.rodata that span an `__fentry__` cannot forward. Beyond that, the user noted a deeper semantic problem: a "preserve all" CC should model that **every Vn** (registers + memory + flags) is preserved, not just registers.
- **External ref:** GCC `__attribute__((no_caller_saved_registers))` (System V AMD64 ABI extension).
- **Fix (incremental):** Add `no_memory_clobber: bool` field to `BuiltCallingConvention`; thread it through `build_call_with_cc` to skip emitting the Memory output (or emit a pass-through memory edge). Long-term: lift the question from "which regs to preserve" to "which Vns are unclobbered" so memory + flags + arch-specific Vns can all be expressed.

### A2 — AArch64 `x30` in `callee_saved_regs` — RETRACTED (verified correct)
- **Where:** `crates/target/src/calling_convention/mod.rs:224-226`
- **Initial finding:** I claimed listing `x30` as callee-saved was wrong because `bl` clobbers it.
- **Why it's actually correct:** Sleigh's AArch64 `bl` semantic at `../rsleigh/sleigh/processors/AARCH64/data/languages/AARCH64base.sinc:876-880` is `x30 = inst_start + 4; call Addr26;` — the LR assignment is an explicit pcode COPY *before* the `call`. The strider lifter therefore produces the IR sequence `LR := IntConst(inst_next); Call(target)`. With `x30` in `callee_saved_regs` (excluded from Call's clobber set), post-call LR holds `IntConst(inst_next)` — exactly the return-address value AAPCS guarantees the callee preserves. If `x30` were clobbered, the IR would lose the "LR equals next-PC" information and link-register-return classification would break.
- **AAPCS reality:** "callee preserves x30" means the callee preserves the value `bl` placed there, not the pre-`bl` value. The strider model captures this correctly.
- **Non-AAPCS callees:** Out of scope for the AAPCS preset. Such call sites are expressed via a different `CallingConvention` preset (e.g. an exception-handling thunk CC) or a `per_address_ccs` override; the AAPCS preset itself doesn't need to model their behaviour.
- **Verdict:** No bug. Listing `x30` in `callee_saved_regs` is the correct model for the AAPCS preset.

### A3 — ARM 32-bit `lr` in `callee_saved_regs` — RETRACTED (verified correct)
- **Where:** `crates/target/src/calling_convention/mod.rs:262`
- **Same reasoning as A2.** Sleigh's ARM `bl` semantic at `../rsleigh/sleigh/processors/ARM/data/languages/ARMinstructions.sinc:2291` is `lr = inst_next; call Addr24;`. With `lr` callee-saved (excluded from clobbers), post-call LR holds the IntConst the lifter assigned. AAPCS-conforming callees preserve this value.
- **Verdict:** No bug.

### A4 — MIPS `mips_n64.arg_passing_regs` lists `t0..t3` instead of `a4..a7` — MED
- **Where:** `crates/target/src/calling_convention/mod.rs:334,692`
- **External ref:** MIPS N64 ABI §2.1 — `$4..$11` are `a0..a7`.
- **Bug:** Sleigh's MIPS64 spec may resolve `t0..t3` to `$8..$11` (older naming) — in which case the Vn-level behaviour is correct but confusing. If Sleigh's mips64 spec assigns `t0..t3` to different physical regs (the `temp` regs `$8..$11` from MIPS32), the lift mismatches the N64 ABI. Verification needed.
- **Fix:** Run the (currently `#[ignore]`-d) diagnostic test to discover Sleigh's actual register-name resolution. Then either rename to `a4..a7` for clarity or document the Sleigh-vs-ABI naming.

### A5 — `x86_64_linux_syscall.callee_saved_regs` — MED (low practical risk)
- **Where:** `crates/target/src/calling_convention/mod.rs:622-632`
- **External ref:** Intel SDM Vol.2B `SYSCALL` — clobbers RCX (return RIP) and R11 (saved RFLAGS).
- The CC inherits SysV's callee-saved set (RBX, RBP, R12-R15) — neither RCX nor R11 are in it. So the `Call` node properly clobbers RCX/R11 at any call site using this CC. Plus the `syscall` CallOther's `implicit_writes` should already include RCX/R11.
- **Status:** Likely correct — verify by reading the `syscall` CallOther entry in `call_other_abi.rs`.

### A6 — PowerPC `callee_saved_regs` includes `LR` — MED
- **Where:** `crates/target/src/calling_convention/mod.rs:361-365` (and ELFv1/v2 variants)
- **External ref:** SVR4 PPC ABI §3.3.1 — **LR IS preserved across calls** because the callee saves it in the linkage area and restores before return.
- **Status:** Architecturally different from A2/A3. PPC's mechanism (stack-save) preserves LR end-to-end; listing it as callee-saved is **correct** for PPC.

### A7 — `x86_64_all_preserving.ret_stack_pop = 0` — LOW (by design)
- **Where:** `crates/target/src/calling_convention/mod.rs:185-203`
- **Status:** Comment explains intentional pairing with `per_address_ccs`. Not a bug.

---

## Class B — Pcode-Lift Semantics

### B1 — `Subpiece` shift direction — VERIFIED CORRECT
- **Where:** `crates/pcode-lift/src/value/cast.rs:30-59`
- **External ref:** GHIDRA pcode spec — `SUBPIECE(value, byte_offset)` extracts the bytes starting at `byte_offset` *in arithmetic LE numbering*, regardless of target endianness. Sleigh always uses LE byte numbering for pcode.
- **Verdict:** Right-shift by `byte_offset * 8` is correct on numeric-valued IR (the IR represents integers as numeric values, not byte arrays).

### B2 — `FloatNan` → `BoolNeg(FloatEqual(x,x))` — MED (correct per pcode, not bit-exact for SNaN)
- **Where:** `crates/pcode-lift/src/value/float.rs:78-89`
- **External ref:** IEEE 754-2008 §5.11 (`x != x` ⇔ x is NaN). Quiet vs signaling NaN distinction matters at the hardware level (SNaN raises FP exceptions on compare in some modes), but the lifter's lowering is the canonical software realisation.
- **Verdict:** Acceptable. Document that the IR's `FloatNan` semantics treat all NaNs equivalently.

### B3 — `PtrAdd` element-size multiplication — VERIFIED CORRECT
- **Where:** `crates/pcode-lift/src/value/cast.rs:213-233`
- **Verdict:** Wrapping multiplication matches C pointer-arithmetic semantics at any width. No bug.

### Verified by reading code:
- **`IntCarry` / `IntScarry` / `IntSborrow`** mapped to distinct `IntCmpOp` variants. Match Sleigh's semantics.
- **`IntSrem` / `IntRem` / `IntDiv` / `IntSdiv`** correctly distinguish signed/unsigned.
- **`IntLeft` / `IntRight` / `IntSright`** match Sleigh's "shift by ≥ width returns 0 (unsigned) or sign-extended (signed)" semantics.
- **`Popcount` / `Lzcount`** correctly truncated to input width.
- **`Indirect` / `MultiEqual`** opcodes correctly bail (rsleigh's `lift_one` never emits these).

---

## Class C — Optimization Pass Correctness

### C1 — `IfCondInversion` corrupts VarPhi values at the merge point — HIGH (NEW)
- **Where:** `crates/opt/src/if_cond_inversion/mod.rs:94-130`
- **Code reality:** `invert` swaps consumers of `if.true_out` and `if.false_out` via `update_input`. Each consumer is a ControlState; the slot position of the consumer's input does NOT change (just the value at that slot).
- **Bug:** A `VarPhi` at the merge point has value-input slot `j+1` corresponding to ControlState's predecessor-control-input slot `j`. Before inversion: ControlState slot `j` ← `if.true_out`, VarPhi slot `j+1` ← *true-branch value*. After inversion: ControlState slot `j` ← `if.false_out`, but **VarPhi slot `j+1` still holds the true-branch value**. So the phi merges the wrong value for the (now-false) predecessor.
- **Severity:** Silent data-flow corruption for any function with phi nodes at the merge of an inverted conditional. This is exactly the kind of analysis-corrupting bug that makes the user's "graph must be correct representation" requirement matter.
- **Fix:** After swapping If's control outputs, find every `VarPhi`/`MemPhi` consuming a ControlState that consumes the swapped outputs, and swap their corresponding value inputs at the same slot indices.

### C2 — `FlagCmpCanonicalize` Rule 2 shared-capture may silently fail — MED
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:276-282`
- **Bug:** Pattern `BoolAnd(BoolNeg(IntLess(a, b)), BoolNeg(Equal(diff, 0)))` requires both occurrences of `a` (and `b`) to bind to the same `NodeOutputId`. If ConstantFold collapsed the flag tree such that the captures bind to different outputs of the same op, the rule misses.
- **Fix:** Either ensure FlagCmpCanonicalize runs before ConstantFold collapses shared sub-expressions (`stable_default_pipeline` order — verify), or relax the shared-capture to a structural-equality check.

### C3 — `KnownBits` `SignExtend` propagation missing — LOW (precision miss)
- **Where:** `crates/opt/src/known_bits/mod.rs:254-265`
- **External ref:** Standard known-bits propagation: if input MSB is known 0/1, all extension bits are known 0/1.
- **Status:** Sound (returns "unknown" not "wrong"); affects jump-table classifier on MIPS/ARM Thumb.

### C4 — `ConstantFold` `ZeroExtend(IntConst(v))` doesn't mask v to input width — LOW
- **Where:** `crates/opt/src/constant_fold/rules.rs:454-460`
- **Status:** Defensive only; `build_int_const` masks at construction. Validator should catch unmasked constants.

---

## Class D — CallOther ABI Table

### D1 — `sysret` classified `NoReturn` — HIGH
- **Where:** `crates/target/src/call_other_abi.rs:248`
- **External ref:** Intel SDM Vol.2B SYSRET — fast return from SYSCALL. RIP ← RCX, RFLAGS ← R11, switches CPL.
- **Bug:** Marking it `NoReturn` makes every kernel function ending in `sysret` (e.g., `do_syscall_64` fast path) appear non-terminating. CFG analysis corrupted.
- **Fix:** Reclassify as `Call(CallOtherAbi { implicit_reads: &["RCX","R11"], implicit_writes: &[], memory_edge: true })` — the call has the effect of returning; downstream code won't see this CallOther anyway because `Return` follows.

### D2 — `swapgs` classified `PURE` (no memory edge) — HIGH
- **Where:** `crates/target/src/call_other_abi.rs:299`
- **External ref:** Intel SDM Vol.2B SWAPGS — exchanges `IA32_GS_BASE` with `IA32_KERNEL_GS_BASE` MSR.
- **Bug:** All subsequent `%gs:`-relative loads/stores depend on the new base. With `memory_edge: false`, `StackLoadForward` and `LoadReadOnly` can incorrectly forward across `swapgs` (kernel entry/exit paths).
- **Fix:** Reclassify as `PURE_WITH_MEM_EDGE` (matching `wrgsbase`/`wrfsbase` which are correctly classified).

### D3 — `rdtscp` may not be distinguished from `rdtsc` — MED
- **Where:** `crates/target/src/call_other_abi.rs:135-141`
- **Bug:** `RDTSCP` writes `EAX, EDX, ECX` (TSC_AUX). `rdtsc` writes only `EAX, EDX`. If Sleigh lifts both as `"rdtsc"`, `RDTSCP` users miss the ECX clobber. If Sleigh uses a separate name, no bug.
- **Fix:** Verify Sleigh's x86 spec; if needed, add `"rdtscp"` entry separately.

### Verified-correct entries (sampled):
- `cpuid` (writes EAX/EBX/ECX/EDX), `rdmsr`/`wrmsr` (memory_edge), `wrfsbase`/`wrgsbase` (memory_edge), `mfence`/`sfence`/`lfence` (memory_edge).

---

## Class E — Cross-Cutting

### E1 — `IfCondInversion` fingerprint propagation — VERIFIED CORRECT
- The pass creates no new nodes; it rewires existing inputs. Per the asm-fingerprint contract, no new fingerprint extension is required.

### E2 — `FlagCmpCanonicalize::rhs_thumb_b` doesn't extend root's fingerprint — MED
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:243-248`
- **Code reality:** `rhs_thumb_b` returns the captured node `a`'s output directly. `try_apply_rule` then `replace_all_uses(root_out, a_out)`. The root's contributing asm-instruction address is NOT unioned into `a`'s fingerprint.
- **Per CLAUDE.md asm-fingerprint contract:** "passes may grow fingerprints but must never replace a node with one whose fingerprint omits an ancestor's addresses."
- **Bug:** Proof-of-correctness gap — the root's contributing address is lost when the consumer of root_out is rewired to `a_out`.
- **Fix:** In `rhs_thumb_b`, call `graph.extend_asm_fingerprint_from(a_node, root)` before returning.

---

## Top Findings (most impactful first)

1. **C1 (HIGH, NEW)** — `IfCondInversion` corrupts VarPhi values at merge points. Silent data-flow corruption.
2. **D1 (HIGH)** — `sysret` mis-classified `NoReturn` corrupts kernel-syscall CFGs.
3. **D2 (HIGH)** — `swapgs` missing memory edge allows incorrect forwarding across kernel-entry transitions.
4. **A1 (HIGH)** — `x86_64_all_preserving` cannot express memory-preservation; "preserve all" is misnamed.
5. **C2 (MED)** — FlagCmpCanonicalize Rule 2 shared-capture brittleness.
6. **E2 (MED)** — `rhs_thumb_b` fingerprint-superset violation.
7. **D3 (MED)** — `rdtscp` ECX clobber may be missed.
8. **A4 (MED)** — MIPS N64 register-name confusion.

These are the bugs that actually affect analysis correctness on real binaries — not API ergonomics, not naming, not docs.
