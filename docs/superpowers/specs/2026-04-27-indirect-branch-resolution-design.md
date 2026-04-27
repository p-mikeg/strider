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

### 2. Indirect-target resolver — mini IR graph approach

**Lazily invoked.**  The resolver is called **only** from the
`BranchIndirect` opcode arm in `RegionBuilder::process_new_insn`.
Regions without a `BranchIndirect` never trigger the mini-graph
build — the cfg layer's normal path is unaffected and incurs zero
overhead.  `CallIndirect` is out of scope (see step 4).

Per invocation, the resolver builds a **single-block IR graph** for
the current region's value-producing instructions, runs a
stripped-down opt pipeline (`ConstantFold` + `KnownBits` +
`RedundantPhis` + optionally `LoadReadOnly`), and inspects the
producer of `target_vn` in the resolved graph.  Cost is
O(region_size) per encountered indirect branch; indirect branches
are rare (typically a few per binary), so the workspace-wide cost
is negligible.

This subsumes the side-table tracker and chase-one-hop sketches from
earlier drafts.  The benefits:

* **Full constant-fold + KnownBits power** — resolves arithmetic
  chains and bit-mask sequences (`mov eax, 0x100; add eax, 0x33;
  jmp *rax`).
* **Sub-register aliasing handled correctly** — IR encodes overlap
  via `Piece`/`Insert`/`Extract`; constant-fold simplifies them.
  No "largest containing register" logic needed at the resolver
  layer.
* **`LoadReadOnly` participates** — `mov rax, [rodata_addr]; jmp
  *rax` resolves when the binary's `.rodata` is in scope.
* **`InitialVar(LR)` is the natural LinkRegister signal** — no
  ad-hoc VN comparison; just match on the producer's `NodeKind`.
* **Jump tables become pattern queries against the resolved graph**
  in a future round — same machinery.

Resolver return type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// Target value is the function-entry value of the calling
    /// convention's link register varnode (i.e. an `InitialVar(lr)`
    /// in the resolved graph).  Equivalent to `Return`.
    LinkRegister,
    /// Target value is the constant `addr`.
    Single(u64),
    /// FUTURE.  N statically-known targets (jump tables).  Not
    /// produced by this round.
    Multiple(Vec<u64>),
}
```

There is no `None` variant.  An unresolvable indirect branch is a
hard error — see step 4.

#### Mini-graph construction

```rust
// new crate `pcode-lift` (see step 5):
pub fn lift_value_block<R: MemReader>(
    builder: &mut FunctionBuilder,
    insns: &[RegionInstruction],
    sleigh: &Sleigh<R>,
) -> Result<HashMap<Vn, NodeOutputId>, Error>;

// crates/cfg/src/cfg/builder/indirect_resolve.rs (new module):
pub(super) fn resolve_indirect_target<R: MemReader>(
    region_insns: &[RegionInstruction],
    target_vn: rsleigh::Vn,
    sleigh: &rsleigh::Sleigh<R>,
    cc_link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    insn_addr: PcodeInsnAddr,
) -> Result<ResolvedTargets, Error> {
    let mut graph = Graph::new();
    let mut builder = FunctionBuilder::new_for_value_resolution(
        &mut graph, sleigh.regs()?,
    )?;
    let vn_to_value = pcode_lift::lift_value_block(
        &mut builder, region_insns, sleigh,
    )?;
    let target_value = pcode_lift::read_vn(
        &mut builder, &vn_to_value, &target_vn,
    )?;
    builder.build_return(Some(target_value), &[])?;

    let mut fg = builder.build()?;
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(KnownBits);
    pipeline.add(RedundantPhis);
    if let Some(rom) = rom { pipeline.add(LoadReadOnly::new(rom)); }
    pipeline.run(&mut fg)?;

    classify(&fg, target_value, cc_link_register_vn)
        .ok_or_else(|| ErrorKind::UnresolvedIndirectBranch(insn_addr).into())
}

fn classify(
    fg: &BuiltFunctionGraph,
    target: NodeOutputId,
    lr: Option<Vn>,
) -> Option<ResolvedTargets> {
    let producer = fg.graph.output_producer(target);
    match fg.graph.node_kind(producer) {
        NodeKind::IntConst(k) => Some(ResolvedTargets::Single(*k as u64)),
        NodeKind::InitialVar(vn) if Some(*vn) == lr => Some(ResolvedTargets::LinkRegister),
        _ => None,
    }
}
```

#### Optimizer passes deliberately omitted

* `StackStoreDetect` / `CallStackArgCollect` / `StackLoadForward` —
  no calls or stack frames in a stripped value-only mini-graph.
* `CallOtherElide` — no callother nodes (control-flow is filtered
  out of the mini-graph).
* `function_args::FunctionArgDetect` — no calls.
* `DeadBranchElimination` — no branches.

Multi-region chasing (Fallthrough predecessors, phi merges) is
**out of scope** for this round.  Future work documented at the end
of this spec.

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
fail-loudly.  `CallIndirect` is **not touched** by this work —
function-pointer calls (including `blx lr`, where the target is the
caller's link-register value) lift unchanged to `Call(unknown_value)`.
The resolver and the new error apply only to `BranchIndirect`.

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

`CallIndirect` is **not** touched by this work.  `blx lr` and other
indirect calls continue to lift via the analyzer's existing
`handle_call_indirect` to a `Call(unknown_value)` — function-pointer
call semantics are correct without any LinkRegister special-casing.

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

### 8. New crate `pcode-lift`

Per-opcode pcode → IR translation lives today on
`IrStrider<R>` methods in `crates/strider/src/strider/insn/`.  The
**value-producing subset** (no Branch / Return / Call / CallIndirect
/ CallOther / Store / Branch / CondBranch / BranchIndirect) is a
clean, stateless layer — it depends only on `FunctionBuilder` and a
`HashMap<Vn, NodeOutputId>`.

This subset moves into a new crate `pcode-lift` with the API:

```rust
// crates/pcode-lift/src/lib.rs
pub struct ValueLifter<'a, 'b, R: rsleigh::MemReader> {
    pub builder: &'a mut FunctionBuilder<'b>,
    pub vn_to_value: &'a mut HashMap<rsleigh::Vn, NodeOutputId>,
    pub sleigh: &'a rsleigh::Sleigh<R>,
}

impl<'a, 'b, R: rsleigh::MemReader> ValueLifter<'a, 'b, R> {
    /// Lifts a single pcode insn whose opcode is value-producing.
    /// Returns `Ok(true)` when the insn was lifted, `Ok(false)`
    /// when the opcode is a control-flow / call / store op that
    /// the caller is responsible for handling.
    pub fn lift(&mut self, insn: &rsleigh::Insn) -> Result<bool>;

    pub fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<NodeOutputId>;
    pub fn write_vn(&mut self, vn: &rsleigh::Vn, value: NodeOutputId) -> Result<()>;
}
```

Internal organisation mirrors the strider layout (one file per opcode
family: `arithmetic`, `integer`, `float`, `boolean`, `cast`, `mem_io`,
`misc_value`).  Strider's per-opcode handlers for value ops move
verbatim, with `&mut self` rebound to `&mut self.value_lifter`.

Strider keeps:
* control-flow handlers (`branch`, `cond_branch`, `return`, `call`,
  `call_indirect`, `call_other`) — these are strider-specific (they
  use `region_lookup`, `arg_passing_vars`, calling convention, etc.).
* `Store` handler — needs strider's memory-chain advancement, not
  pure value-flow.
* `IrStrider::process_insn` dispatch loop — first delegates to
  `ValueLifter::lift`, then handles control-flow ops directly when
  `lift` returns `false`.

Dependency graph after the refactor:

```
reader, ir-macros (proc-macro)
       ↓
       ir  ←  pcode-lift  ←  cfg  ←  strider
                   ↑                    ↓
               (cfg uses for resolver)  ↑
                                      uses ir, opt, pattern directly
       ↑
       opt → pattern (consumers via strider)
```

No cycles.  `pcode-lift` is below `cfg` and `strider`; both depend
on it.

### 9. Files touched

#### New crate

* `crates/pcode-lift/Cargo.toml`
* `crates/pcode-lift/src/lib.rs` — `ValueLifter` definition,
  module declarations, public API.
* `crates/pcode-lift/src/value/{arithmetic,integer,float,boolean,cast,mem_io,misc_value}.rs`
  — moved from strider with minor adaptations.

#### CFG

* `crates/cfg/Cargo.toml` — add `ir`, `opt`, `pcode-lift`,
  `target` (for the calling-convention link-register varnode) as
  dependencies.
* `crates/cfg/src/cfg/types.rs` — `RegionTerminator` enum; remove
  `ends_with_tail_call` from `Region`; add `terminator:
  RegionTerminator`.
* `crates/cfg/src/cfg/builder/region_builder.rs` —
  `BranchIndirect` / `CallIndirect` dispatch updated to call the
  resolver and produce the new terminator.
  `finish_current_region` signature: `(ends_with_tail_call: bool)`
  → `(terminator: RegionTerminator)`.
* `crates/cfg/src/cfg/builder/indirect_resolve.rs` — new module
  with `ResolvedTargets` and the mini-graph resolver.
* `crates/cfg/src/cfg/builder/split.rs` — use `RegionTerminator`
  when splitting (first half → `Fallthrough`, second half inherits).
* `crates/cfg/src/error.rs` —
  `UnresolvedIndirectBranch(PcodeInsnAddr)` variant.

#### Target

* `crates/target/src/calling_convention.rs` — add
  `link_register_reg_name: Option<&'static str>` to
  `CallingConvention`, add `link_register_vn: Option<rsleigh::Vn>`
  to `BuiltCallingConvention`, fill it in for every preset,
  resolve it in `build()`.

#### Strider

* `crates/strider/Cargo.toml` — add `pcode-lift` dependency.
* `crates/strider/src/strider/insn/mod.rs` — replace per-opcode
  arms for value ops with a `ValueLifter::lift` delegate; split
  the `Return | BranchIndirect` arm; add terminator-driven dispatch
  in the per-region post-loop.
* `crates/strider/src/strider/insn/control.rs` — new
  `handle_tail_call(target)`.
* Remove (now in pcode-lift):
  `crates/strider/src/strider/insn/{arithmetic,integer,float,boolean,cast,mem_io,...value}.rs`.

## Test plan

### `pcode-lift`-level unit tests (new file `crates/pcode-lift/tests/value_lifter.rs`)

1. `ValueLifter` lifts an `IntCopy` of a `CONST` into an
   `IntConst` IR node.
2. `ValueLifter` lifts `IntAdd` of two `CONST`s into an
   `IntBinaryOp(Add)` and `ConstantFold` reduces it.
3. `ValueLifter::lift` returns `Ok(false)` for control-flow opcodes
   (`Branch`, `Return`, `Call`, `BranchIndirect`).
4. Round-trip: a known value-only sequence lifts to the same IR as
   strider produces.

### CFG-level resolver tests (new file `crates/cfg/tests/indirect_branch.rs`)

5. **Resolver positive — direct const:** `mov reg, K; jmp *reg` →
   `ResolvedTargets::Single(K)`.
6. **Resolver positive — arithmetic chain:** `mov reg, K1; add reg,
   K2; jmp *reg` → `Single(K1 + K2)`.  Exercises the `ConstantFold`
   step that the side-table tracker couldn't handle.
7. **Resolver positive — sub-register:** `mov eax, K; jmp *rax`
   (where `eax` is a sub-register of `rax`) → `Single(K)`.
   Exercises the `Piece`/`Insert` aliasing path.
8. **Resolver positive — `bx lr`:** target VN is the calling
   convention's link register, no prior write → `LinkRegister`.
9. **Sanity — `blx lr` is unaffected:** assert that `blx lr` lifts
   via the analyzer's existing `handle_call_indirect` to a `Call`
   node (NOT a `Return`).  This pins the contract that the resolver
   does not bleed into `CallIndirect`.
10. **Resolver positive — rodata load:** `mov rax, [rodata_addr];
    jmp *rax` with a `ReadOnlyMemory` containing the entry value →
    `Single(K)`.  Exercises `LoadReadOnly` participation.
11. **Resolver negative — unknown memory:** `mov rax, [mem]; jmp
    *rax` with no `ReadOnlyMemory` covering the address →
    `Err(UnresolvedIndirectBranch)`.
12. **Resolver negative — runtime input:** `jmp *<arg_reg>` with no
    constant write to `arg_reg` → `Err(...)`.

### CFG-level dispatch tests (same file)

13. Indirect branch to in-range constant produces a `Branch` edge
    AND a region with `terminator = Branch`.
14. Indirect branch to out-of-range constant produces `terminator
    = TailCall { target }` (no successor edge).
15. Indirect branch via LR produces `terminator = Return`.

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

  **Bounding `idx`.**  Two complementary mechanisms cover real-world
  `switch` lowerings:

  1. **`KnownBits`-derived bound.**  For `idx & MASK` patterns the
     existing `KnownBits` pass already proves the upper bits are
     zero.  Read the resolved bit-mask: `N = MASK + 1`.
  2. **Predecessor `If` walk.**  For `if (idx < N) ...` patterns the
     bounding `If` is upstream of the `BranchIndirect`'s region.
     Walk backward through CFG edges (`Fallthrough` /
     `IfCaseTrue` / `IfCaseFalse`) along the predecessor chain.  At
     each `If` we cross, the condition expression bounds some VN
     and the edge label tells us which side of the bound we're on.

  **CFG-build ordering caveat (Issue A).**  The CFG builder is
  depth-first via a LIFO work queue.  When the resolver runs at a
  `BranchIndirect` in region X, the bounding `If`'s OTHER successor
  (the default / out-of-bounds path) may still be on the work
  queue, NOT yet a CFG node.  This is fine for the predecessor-`If`
  walk: we only need (i) the `If`'s condition expression, which
  lives in the parent region's pcode and is therefore available as
  soon as the parent region exists, and (ii) the edge label for the
  edge we entered through, which is in the graph metadata.  We do
  NOT need the not-taken sibling region's contents.

  **Multi-predecessor join regions (Issue B).**  If the predecessor
  chain forks at a join region (multiple paths reach `X`, each with
  its own bound on `idx`), the round-1 jump-table resolver should
  fail closed with `UnresolvedIndirectBranch`.  Cross-predecessor
  bound merging (intersection / union) is a later round.  The
  failure mode stays consistent with this round's strict-failure
  semantics — no silent fallback to an unbounded `Multiple`.

  **Newly-resolved targets (Issue C).**  Each `Multiple(targets)`
  result enqueues N new addresses on the cfg builder's work queue
  via the existing `RegionEdgeKind::Branch` path.  Subsequent
  exploration creates the target regions; their own `BranchIndirect`s
  (if any) get an independent resolution pass when they're processed.
  No special handling required.
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
