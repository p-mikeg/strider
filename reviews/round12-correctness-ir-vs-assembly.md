# Round 12 — Emphasis A axis 3: lifted IR vs real assembly

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

## Verdict

**No HIGH findings.** 1 MED + 2 LOW findings. 20 spot-checks verified correct against Intel SDM, SMCCC, Linux ARM EABI, SysV AMD64 / AAPCS / AAPCS64 / MIPS O32 / PPC ELFv1/v2 ABI specs.

## Findings

### IRA-1 — `cmpxchg16b`, `xsetbv`, `xgetbv`, `monitor`, `mwait` absent from CallOther table
- **Severity:** MED (confidence 100)
- **Binary:** any x86_64 binary using these instructions (glibc, UEFI, hypervisors, crypto)
- **Where:** `crates/target/src/call_other_abi.rs` — five missing entries in `classify_arch_specific` for `(X86 | X86_64, name)`
- **objdump trace:** `0f c7 /1` (CMPXCHG16B), `0f 01 d1` (XSETBV), `0f 01 d0` (XGETBV), `0f 01 c8` (MONITOR), `0f 01 c9` (MWAIT)
- **Expected IR:** `CallOther` nodes with correct implicit register channels per ABI
- **Actual IR / lifter path:** `classify()` returns `None`; strict-on-emission raises `UnknownCallOtherError` at IR lift time, aborting the entire lift
- **ABI ref:** Intel SDM Vol. 2A/2B/2C:
  - `CMPXCHG16B` §3-179: reads RDX:RAX (compare) + RCX:RBX (swap); writes RDX:RAX on mismatch; atomic RMW → `memory_edge: true`
  - `XSETBV` §4-690: reads ECX (XCR#) + EDX:EAX (value); no GPR writes; `memory_edge: true`
  - `XGETBV` §4-689: reads ECX; writes EDX:EAX (value); `memory_edge: false`
  - `MONITOR` §4-39: reads RAX + ECX (extensions) + EDX (hints); `memory_edge: true`
  - `MWAIT` §4-44: reads EAX (hints) + ECX (extensions); `memory_edge: true`
- **Fix:** Add five `("name") => Some(CallOtherClass::Call(CallOtherAbi { ... }))` entries per the spec channels.
- **Regression test:** Unit tests mirroring `rdtsc_writes_edx_eax_no_memory_edge` for each new entry; propose `fixtures/cases/advanced_x86.asm` with inline-asm exercising each opcode + smoke test asserting `classify(X86_64, name).is_some()`.

### IRA-2 — Stale `#[ignore]` on `indirect_branch_resolved_aarch64be`
- **Severity:** LOW (confidence 85)
- **Binary:** `fixtures/out/aarch64be/indirect_branch.elf::indirect_branch_resolved`
- **Where:** `crates/strider/tests/indirect_branch.rs:215` — `#[ignore]` reads: "aarch64-be: stack-array dispatch unresolved — lifter emits Or(SP,K) instead of Add(SP,K) and wraps stored labels in Truncate; resolver matches Add(SP,K)+raw-IntConst only"
- **Expected lifter path:** `classify_stack_array` resolves the dispatch via `flatten_add_tree(Or)` + `peel_to_u64_const(Truncate)`
- **Actual lifter path (current):** Both blockers are now fixed. `flatten_add_tree` at `crates/opt/src/indirect_branch_resolve/stack_array.rs:443` handles `IntBinaryOp::Or` as an add-equivalent. `peel_to_u64_const` (lines 155-199) handles `Truncate(IntConst)`. The ignore reason no longer describes the code.
- **Fix:** Run `cargo test -p strider indirect_branch_resolved_aarch64be` with the `#[ignore]` removed. If green, commit. If red, file fresh characterisation — the old reason is demonstrably stale.
- **Regression test:** Re-enabling the test is the regression guard.

### IRA-3 — `sysret` classified `NoReturn` — defensible but undocumented limitation
- **Severity:** LOW (confidence 82)
- **Binary:** any x86_64 kernel binary using SYSRET as normal function exit (e.g. `arch/x86/entry/entry_64.S::syscall_return_via_sysret`)
- **Where:** `crates/target/src/call_other_abi.rs:258` — `"sysret" => NO_RETURN`
- **objdump trace:** `0f 07` — SYSRET; resumes at RCX (user RIP) with RFLAGS from R11
- **Expected IR:** For kernel entry wrappers, `NoReturn` is pragmatically correct (the kernel function's execution context ends). But `NoReturn` silently drops any epilogue code between SYSRET and the function's architectural end, making pattern queries over that epilogue impossible.
- **ABI ref:** Intel SDM Vol. 2B §4-565: SYSRET resumes user-mode. Not a fault; not a trap handler.
- **Fix:** Add a comment at line 258 explaining the choice is correct for kernel-internal analysis (kernel function's control does not return to its kernel-context caller) and that a future `ReturnToUserMode` classification would be needed for user-mode trampoline analysis. No code change strictly required.
- **Regression test:** Propose `fixtures/cases/sysret_stub.S` kernel entry wrapper + test asserting `RegionTerminator::NoReturn`.

## 20 verified-correct spot-checks

| # | Item | ABI ref | Status |
|---|------|---------|--------|
| 1 | x86_64 arg regs `[RDI,RSI,RDX,RCX,R8,R9]` | SysV AMD64 §3.2.3 | ✓ |
| 2 | x86_64 callee-saved `[RBX,RBP,R12-R15]` | SysV AMD64 §3.2.1 | ✓ |
| 3 | x86_64 ret `[RAX,RDX]` / `[XMM0,XMM1]` | SysV AMD64 §3.2.3 | ✓ |
| 4 | x86_64 stack arg offsets `[8,16,...]` | SysV AMD64 §3.2.2 | ✓ |
| 5 | x86 cdecl callee-saved `[EBX,ESI,EDI,EBP]` | SysV i386 §3.2.1 | ✓ |
| 6 | AArch64 arg regs `[x0-x7]` | AAPCS64 §6.8.2 | ✓ |
| 7 | AArch64 callee-saved `[x19-x28,x29,x30]` | AAPCS64 §6.1 + CLAUDE.md LR tradeoff | ✓ |
| 8 | AArch64 ret `[x0,x1]` / `[q0,q1]` | AAPCS64 §6.5 | ✓ |
| 9 | ARM arg regs `[r0-r3]` | AAPCS §5.4 | ✓ |
| 10 | ARM callee-saved `[r4-r11,lr]` | AAPCS §5.1.1 + CLAUDE.md LR tradeoff | ✓ |
| 11 | `syscall` CallOther reads+writes | Linux ABI + Intel SDM SYSCALL §4-574 | ✓ |
| 12 | `rdtsc` writes `[EAX,EDX]`, no mem edge | Intel SDM §4-540 | ✓ |
| 13 | `rdtscp` adds ECX clobber | Intel SDM §4-544 | ✓ |
| 14 | `rdpkru_u32` reads `[ECX]`, writes `[EAX,EDX]` | Intel SDM §4-471 | ✓ |
| 15 | ARM `swi` reads `[r7,r0-r6]`, writes `[r0]` | Linux ARM EABI entry-common.S | ✓ |
| 16 | SMCCC `CallHyperVisor`/`CallSecureMonitor` | SMCCC §6.3 | ✓ |
| 17 | `swapgs` PURE_WITH_MEM_EDGE | Intel SDM §4-591 | ✓ |
| 18 | `mfence`/`sfence`/`lfence` PURE_WITH_MEM_EDGE | Intel SDM §4-22,4-567,4-334 | ✓ |
| 19 | `IntSub → Add(a,Neg(b))` canonicalisation | two's-complement arithmetic | ✓ |
| 20 | `FloatLessEqual → Or(Less,Equal)` + `FloatSub → FloatAdd(a,Neg(b))` | IEEE 754 §6.3 + NaN | ✓ |

## Fixtures status

All arch directories enumerated under `fixtures/out/` are present: `x86`, `x64`, `x86_kernel`, `aarch64`, `aarch64be`, `arm`, `arm_be`, `arm_thumb` (via common test), mips32be/le, mips64be/le, ppc32be/le, ppc64be/le. Cross-arch `objdump` traces not executed in this audit; findings are code-shape derived and ABI-spec verified directly against published specifications.

## Files reviewed

- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/{call_other_abi.rs,calling_convention/mod.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/{vn_io.rs,value/*.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/{stack_array.rs,classify.rs,mod.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/constant_fold/{eval_int.rs,eval_float.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_branch.rs`

Cross-referenced against Intel SDM Vol. 2A/2B/2C, SysV AMD64 ABI, AAPCS64, AAPCS, SMCCC, Linux kernel `arch/x86/entry/calling.h` and `arch/arm/kernel/entry-common.S`.
