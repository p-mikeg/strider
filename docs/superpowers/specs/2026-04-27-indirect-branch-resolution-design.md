# Indirect Branch Resolution — Design

## Goal

Stop misclassifying every `BranchIndirect` as a return.  When the
target value can be statically resolved within the current basic
block, route the branch to the right shape: an intra-function `Branch`,
an outside-function `Call + Return` (tail call), or a true `Return`
(`bx lr` / `blx lr`).  When the resolver cannot prove the target,
**fail the build** with a typed error — don't silently fall back to
`Return`.

The design must extend cleanly to jump tables (multi-target
`BranchIndirect`) without revisiting any of the data shapes added in
this round.

## Motivation

### Current behaviour

[`crates/strider/src/strider/insn/mod.rs:108`](../../../crates/strider/src/strider/insn/mod.rs#L108)
collapses `Opcode::Return | Opcode::BranchIndirect` into a single
`handle_return` call.  The long block comment above that line
already documents the four real cases that get conflated:

* function return (`bx lr`, `pop {pc}`, `jr ra`) — correctly classified
  as `Return`;
* tail call (`bx <target>` after the target is materialised) — should
  be `Call + Return`, currently misclassified as `Return`;
* jump table (`ldr pc, [tbl + idx*4]`) — should fan out N successors,
  currently collapses to `Return`;
* computed goto (`goto *ptr`) — should be intra-function indirect
  dispatch, currently collapses to `Return`.

[`crates/cfg/src/cfg/builder/region_builder.rs:317-326`](../../../crates/cfg/src/cfg/builder/region_builder.rs#L317-L326)
mirrors this on the CFG side: `BranchIndirect` terminates the region
without any attempt to resolve or follow the target.

### Existing infrastructure to lean on

* [`Options::fn_max_size`](../../../crates/cfg/src/cfg/options.rs#L8) +
  [`allow_code_before_start_addr`](../../../crates/cfg/src/cfg/options.rs#L12)
  classify direct branches as intra-fn vs. tail-call via
  [`is_branch_tail_call`](../../../crates/cfg/src/cfg/builder/region_builder.rs#L216).
* [`Region::ends_with_tail_call: bool`](../../../crates/cfg/src/cfg/types.rs#L81)
  exists but is **never read** anywhere in `crates/strider/` — the IR
  layer has no path to emit `Call + Return` even for direct tail
  calls today.  This work fixes that gap as a side effect.

## Design

### 1. Replace `Region::ends_with_tail_call: bool` with a richer
   terminator enum.

```rust
// crates/cfg/src/cfg/types.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// No terminator opcode; the region ends because control falls
    /// into the next region.  Today this is `ends_with_tail_call=false`
    /// + no branch/return — implicit fall-through.
    Fallthrough,
    /// Direct unconditional branch, intra-function.  Successor is the
    /// `Branch` edge in the graph.
    Branch,
    /// Direct conditional branch.  Successors are the `IfCaseTrue` and
    /// `IfCaseFalse` edges in the graph.
    CondBranch,
    /// `Return` opcode, OR `BranchIndirect` whose target the resolver
    /// proved is the calling convention's link register, OR
    /// `CallIndirect` whose target is the link register.
    Return,
    /// Direct branch to an outside-function address, OR resolved
    /// `BranchIndirect` whose constant target lies outside the
    /// function range.  The IR layer emits `Call(IntConst(target)) +
    /// Return`.
    TailCall { target: u64 },
    /// FUTURE.  Jump table with N statically-known targets.  Reserved
    /// in the API now so a later resolver upgrade is purely additive.
    /// Not constructed by this round.
    Switch { targets: Vec<u64> },
}
```

`Region::ends_with_tail_call: bool` is removed; callers migrate to
matching on `region.terminator`.

### 2. Indirect-target resolver

```rust
// crates/cfg/src/cfg/builder/indirect_resolve.rs (new module)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedTargets {
    /// Target value is the calling convention's link register, with
    /// no intervening write inside the current region.  Equivalent
    /// to `Return`.
    LinkRegister,
    /// Target value is the constant `addr`.
    Single(u64),
    /// FUTURE.  N statically-known targets (jump tables).  Not
    /// produced by this round.
    Multiple(Vec<u64>),
}
```

Note: there is no `None` variant.  An unresolvable indirect branch is
a hard error (see step 4) — the resolver returns
`Result<ResolvedTargets, Error>`.

The same-region resolver recognises:

1. **`LinkRegister`** — `target_vn` is byte-equal to the calling
   convention's link-register varnode AND no instruction earlier in
   this region writes to that varnode.
2. **`Single(K)`** — the most recent write to `target_vn` in this
   region is one of:
   - `IntCopy` from a `CONST` varnode → `K = const_value`;
   - `IntCopy` from another varnode whose own most-recent write is
     `IntCopy` from `CONST` (chase one hop).  Two-hop is the maximum
     this round.

Anything else is an error.  Multi-region chasing (Fallthrough
predecessors, phi merges) is **out of scope** for this round.

### 3. Calling-convention link-register exposure

[`BuiltCallingConvention`](../../../crates/target/src/calling_convention.rs#L72)
gains a new field:

```rust
pub struct BuiltCallingConvention {
    // ... existing fields ...
    /// On link-register ISAs (ARM, AArch64, MIPS, PowerPC), the
    /// varnode that holds the return address across calls.  `None`
    /// on stack-push ISAs (x86, x86_64), where the return address
    /// lives on the stack.
    pub link_register_vn: Option<rsleigh::Vn>,
}
```

`CallingConvention` gains a corresponding `link_register_reg_name:
Option<&'static str>`; presets fill it in with `"lr"` (ARM /
AArch64), `"ra"` (MIPS), `"LR"` (PowerPC), or `None` (x86/x86_64).
The `build` method resolves the name to a varnode the same way the
SP varnode is resolved.

### 4. Strict failure on unresolved indirect branches

```rust
// crates/cfg/src/error.rs
pub enum ErrorKind {
    // ... existing variants ...
    /// `BranchIndirect` whose target the same-region resolver
    /// could not prove.  Carries the p-code address of the
    /// offending instruction.
    UnresolvedIndirectBranch(PcodeInsnAddr),
}
```

No opt-in flag, no migration safety net.  The user explicitly chose
fail-loudly.  `CallIndirect` does **not** error when its target is
unresolved — function-pointer calls are legitimate and lift to
`Call(unknown_value)` (the existing behaviour).  The only special
case for `CallIndirect` is `link_register_vn` detection (`blx lr`
→ `Return`).

### 5. CFG-layer dispatch

[`RegionBuilder::process_new_insn`](../../../crates/cfg/src/cfg/builder/region_builder.rs#L237)
gains:

```rust
rsleigh::Opcode::BranchIndirect => {
    let target_vn = *insn.inputs.first()
        .ok_or(ErrorKind::MissingBranchTarget(addr))?;
    let resolved = self.resolve_indirect_target(target_vn, addr)?;
    let terminator = match resolved {
        ResolvedTargets::LinkRegister => RegionTerminator::Return,
        ResolvedTargets::Single(target) => {
            let target_addr = MachineInsnAddr { addr: target }.into();
            if self.is_branch_tail_call(target_addr)? {
                RegionTerminator::TailCall { target }
            } else {
                self.builder.work_queue.push(
                    (Some((self.current_region(), RegionEdgeKind::Branch)), target_addr),
                );
                RegionTerminator::Branch
            }
        }
        // Multiple is a future variant; resolver doesn't produce
        // it this round, but the match arm exists so adding the
        // case later is a localised change.
        ResolvedTargets::Multiple(_) => {
            unreachable!("resolver does not produce Multiple yet");
        }
    };
    self.finish_current_region(terminator)?;
    Ok(ProcessInsnRes::FinishedProcessing)
}
```

`CallIndirect` similarly gains a `LinkRegister` check that, when it
fires, terminates the region with `RegionTerminator::Return` instead
of letting the call instruction's caller emit a Call.  Otherwise
`CallIndirect` continues into the region (it's not a terminator
opcode) and the analyzer emits a `Call(unknown_value)` as today.

### 6. IR-layer dispatch

[`IrStrider::process_insn`](../../../crates/strider/src/strider/insn/mod.rs#L40)
consults the region's terminator at end-of-region time and routes:

* `Return` (any source) → existing `handle_return`.
* `TailCall { target }` → new
  `handle_tail_call(target)` that emits
  `build_call(IntConst(target)) + build_return(...)`.
  Used by both direct tail calls and resolved-indirect tail calls.
* `Branch` (incl. resolved indirect branches whose target was
  enqueued as a normal Branch edge) → existing `handle_branch`.
* `Fallthrough`, `CondBranch` → existing handlers.
* `Switch` (future) → reserved for jump tables.

The shared `Opcode::Return | Opcode::BranchIndirect` arm at line 108
is split:

```rust
Opcode::Return => self.handle_return(insn)?,
// BranchIndirect's IR action is fully determined by the CFG
// terminator — the per-instruction handler is a no-op.  The
// terminator dispatch happens in process_region's epilogue.
Opcode::BranchIndirect => {}
```

### 7. Failure semantics

Returning `Err(ErrorKind::UnresolvedIndirectBranch(addr))` propagates
up through `Builder::build` → caller (e.g. `Strider::analyze` →
example binary or test).  The error carries the p-code address so a
human can map it back to a machine instruction with `objdump`.

## Files touched

### CFG

* `crates/cfg/src/cfg/types.rs` — `RegionTerminator` enum; remove
  `ends_with_tail_call` from `Region`; add `terminator:
  RegionTerminator`.
* `crates/cfg/src/cfg/builder/region_builder.rs` — `BranchIndirect` /
  `CallIndirect` dispatch updated to call the resolver and produce
  the new terminator.  `finish_current_region` signature changes:
  `(ends_with_tail_call: bool)` → `(terminator: RegionTerminator)`.
* `crates/cfg/src/cfg/builder/indirect_resolve.rs` — new module with
  `ResolvedTargets` and the same-region resolver.
* `crates/cfg/src/cfg/builder/split.rs` — use `RegionTerminator` when
  building the second-half region during a split (always
  `Fallthrough` for the first half, inherited terminator for the
  second).
* `crates/cfg/src/error.rs` — `UnresolvedIndirectBranch(PcodeInsnAddr)`
  variant.

### Target

* `crates/target/src/calling_convention.rs` — add
  `link_register_reg_name: Option<&'static str>` to `CallingConvention`,
  add `link_register_vn: Option<rsleigh::Vn>` to
  `BuiltCallingConvention`, fill it in for every preset, resolve it
  in `build()`.

### Strider (analyzer)

* `crates/strider/src/strider/insn/mod.rs` — split the
  `Return | BranchIndirect` arm; add terminator-driven dispatch in
  the per-region post-loop.
* `crates/strider/src/strider/insn/control.rs` — new
  `handle_tail_call(target)`.

## Test plan

### CFG-level unit tests (new file `crates/cfg/tests/indirect_branch.rs`)

1. Resolver positive: `mov reg, K; jmp *reg` → `Single(K)`.
2. Resolver positive: `mov reg, src; mov src, K; jmp *reg` — the
   resolver walks `reg → src → K` (two hops, the cap) and returns
   `Single(K)`.
3. Resolver positive: `bx lr` (target VN = LR) → `LinkRegister`.
4. Resolver positive: `blx lr` (CallIndirect, target VN = LR) →
   `LinkRegister`.
5. Resolver negative: `mov reg, [mem]; jmp *reg` → `Err(
   UnresolvedIndirectBranch)`.
6. Resolver negative: `mov reg, src1; mov reg, src2; jmp *reg` (most
   recent write is not from CONST) → `Err(...)`.
7. CFG dispatch: indirect branch to in-range constant produces a
   `Branch` edge AND a region with `terminator = Branch`.
8. CFG dispatch: indirect branch to out-of-range constant produces
   `terminator = TailCall { target }` (no successor edge).
9. CFG dispatch: indirect branch via LR produces `terminator = Return`.

### Strider-level integration tests

10. New fixture: `fixtures/cases/indirect_branch.c` — one or more C
    functions that reliably emit `mov reg, K; jmp *reg` when
    compiled `-O0`.  See "Fixture strategy" below.
11. New per-arch test in `crates/strider/tests/indirect_branch.rs`
    (model after `crates/strider/tests/control.rs`): assert each arch
    that exercises the fixture produces an IR with the correct
    `Call + Return` shape (for tail-call cases) or a `Branch` shape
    (for intra-fn cases).  Per-arch ignores follow the existing
    BUG-pattern when an arch's lifter doesn't produce
    `BranchIndirect` for the construct.

### Calling-convention test

12. Update
    [`presets_stack_pointer_and_arg_offsets`](../../../crates/target/src/calling_convention.rs)
    to also assert `link_register_vn` is set for the link-register
    presets (ARM AAPCS, AArch64 AAPCS64, MIPS, PowerPC) and `None`
    for x86/x86_64.

### Regression

13. Full workspace test pass (current baseline 2561/0/18) must hold
    after the change.  If any existing test trips
    `UnresolvedIndirectBranch`, it indicates either (a) a fixture is
    relying on the old fall-through (bug to surface and fix) or
    (b) a resolver pattern we haven't covered (tighten the
    resolver).

## Fixture strategy

Goal: a C source that compiles `-O0` (no optimisation) and reliably
emits `mov <reg>, <const>; jmp *<reg>` (or the arch's equivalent).
Plain `goto *ptr` (computed goto, GNU extension) is the obvious
candidate:

```c
// fixtures/cases/indirect_branch.c
int indirect_branch_resolved(int x) {
    void *targets[] = {&&L0, &&L1};
    goto *targets[(unsigned)x & 1];
L0: return 0;
L1: return 1;
}
```

At `-O0` GCC and clang lower computed-goto to a constant load
followed by an indirect jump.  The label-address constants are baked
into a small array (in `.rodata` or on the stack) — same-region
resolver needs to chase a single `IntCopy` from a constant generated
by `&&L0` / `&&L1` taking-address.

If `-O0` codegen turns out to be too unstable across our 15 arches,
fallback strategies:

* **fallback A:** add an arch-specific `_resolve_inline_asm.S` for
  arches where the C compiler doesn't emit the desired shape;
* **fallback B:** restrict the test fixture to the two or three
  arches where the codegen is stable, with explicit BUG-N entries
  for the others.

The Makefile may need a `-fno-jump-tables` flag added to
`COMMON_CFLAGS` to keep the fixture's emitted shape stable, since
GCC could otherwise turn the `goto *ptr[idx]` into a jump table.
This is the "maybe change makefile a bit" the user flagged.

If a per-arch toolchain rejects `-fno-jump-tables` (clang accepts it,
some older gcc do not), guard the flag in the per-arch `.mk`.

## Migration risk

Fixtures are compiled with `-O0 -fno-optimize-sibling-calls -static
-no-pie`, no fixture currently uses `switch`, and the
`LinkRegister` resolver covers `bx lr` / `pop {pc}` on ARM/Thumb.
**Expected outcome:** no existing test breaks.

If something does break, the failure shape is
`Err(UnresolvedIndirectBranch(addr))` from the CFG build — easy to
locate.  Resolution is per-failure: either the resolver needs to
recognise a new pattern (tighten and add a unit test) or the fixture
itself is constructed in a way that genuinely cannot be statically
resolved (rare; tracker entry).

## Future work (not part of this round)

* **Cross-region resolution.**  Chase the target VN backward through
  Fallthrough predecessors until the const is found.  Adds one
  variant to the resolver implementation, no API change to
  `ResolvedTargets`.
* **`ResolvedTargets::Multiple` for jump tables.**  Detect the shape
  `mov reg, [base + idx*N]; jmp *reg` where `base` is a `.rodata`
  table and `idx` has a bounded range from a prior conditional.
  Reads the table contents via the existing `MemReader`.  Outputs N
  resolved addresses → `RegionTerminator::Switch { targets }`.
* **IR-side resolution pass.**  An `IndirectBranchResolve` opt pass
  that runs after `ConstantFold` and `LoadReadOnly`, picking up the
  cases the CFG-side resolver couldn't handle (constants merged at
  phis, function pointers loaded from globals).  Requires a CFG-IR
  re-wiring story we're deferring.
* **`Switch` IR node.**  When `RegionTerminator::Switch` ships, the
  IR can either lower as a ladder of `If` nodes (no new node kind,
  linear time per dispatch) or introduce
  `NodeKind::Switch { cases: Vec<u64> }`.  Pick the ladder for the
  first jump-table PR; promote to a dedicated node kind only if
  pattern-matcher ergonomics require it.

## Out-of-scope confirmations

* Resolution scope is **same-region only**.  Chasing across
  Fallthrough preds is future work.
* `fn_max_size` has **no default**.  Callers that don't set it
  classify everything `target >= start_addr` as intra-fn and
  everything below as a tail call.  Same as today.
* Test fixtures are **C** (with a possible Makefile flag tweak).
  No hand-crafted assembly.
* The migration is **strict from day one**.  No
  `allow_unresolved_indirect_branches` opt-in.
