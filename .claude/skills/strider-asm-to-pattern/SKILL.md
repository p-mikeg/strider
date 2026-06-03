---
name: strider-asm-to-pattern
description: Use when the user pastes an assembly snippet (objdump, godbolt output, or a fixture's .s) and asks for a pattern that matches the lifted IR shape — covers the asm → pcode → canonical-IR-shape mental translation, common per-arch idioms (x86 cmp/jcc, AArch64 NZCV flag chains, MIPS branch-delay slots, PPC CR bits, ARM IT blocks), the CFG/IR dump workflow via the strider-orchestrator examples, and the lift-time canonicalisations that decide between source-level and IR-level pattern shapes.
---

# strider-asm-to-pattern

Bridge from raw assembly to a strider pattern.  The user pastes
asm; you (the agent) emit a Pat (Rust or Python).

**Use when** the user says "here's the asm from objdump, write me
a pattern for it" / "match this AArch64 cbz idiom" / "what IR does
`cmp; jne` produce on x86_64?" / "give me a pattern for the MIPS
delay-slot pair `beq + nop`".

**Do NOT use** for:
- IR-shape-first authoring (the user already knows the IR shape) →
  `strider-py-pattern`.
- Writing the inverse rewrite (pattern → replacement) →
  `strider-rewrite-rule-author`.

## How to use this skill

1. **Identify the architecture** (x86 / x86_64 / AArch64 / ARM /
   ARM-Thumb / MIPS32 / MIPS64 / PowerPC32 / PowerPC64 — see
   `crates/strider-target/src/arch.rs` for the closed set).
   The same asm idiom lifts to different IR shapes across arches.
2. **Lift the asm to IR** if you can — drop the asm into a fixture,
   run the orchestrator, dump `graph.html`.  Two workflows:
   - **Quick:** the user already has a fixture binary in `fixtures/out/<arch>/…`.
     Run `cargo run -p strider-orchestrator --example orchestrator_demo`
     (defaults to `fixtures/out/x86/arithmetic.elf::add`).
   - **Custom:** add a one-off fixture under `fixtures/<arch>/` and
     extend `fixtures/Makefile`; rebuild fixtures; lift; dump.
3. **Read the graph dump** for the lifted shape.  Identify
   (a) the value-flow root (load? add? cmp?), (b) the operand kinds
   (`InitialVar` / `IntConst` / nested op), (c) any phi join points.
4. **Apply lift-time canonicalisations** — the lifter rewrites
   `sub` to `Add(_, Neg(_))`, swaps `LessEqual` to `BoolNeg(Less)`,
   etc.  Source-level patterns won't match.  See the
   `strider-py-pattern` cheat sheet or the equivalent Rust
   pattern surface for the full list.
5. **Pick the right pattern root** — `add` is commutative
   (`add(x, y)` matches both orderings), `sub(x, y)` produces
   `Add(x, Neg(y))` (not commutative), `int_le` produces
   `BoolNeg(IntLess(b, a))` (NOT `int_cmp("LessEqual", a, b)`).
6. **Emit the pattern** with captures for the values the user
   wants back.

## Per-arch idioms

### x86 / x86_64

`cmp r1, r2 ; jne L` → `If(BoolNeg(IntEqual(r1, r2)))`.  The `cmp`
sets flags (`ZF` etc.) that the lifter encodes as separate IR
nodes; `IfCondInversion` and `FlagCmpCanonicalize` then fold the
flag-tree into the single `IntCmpOp(Equal)`.

`test r, r ; jz L` → `If(IntEqual(r, IntConst(0)))` (after
canonicalisation).  `test x, x` is `And(x, x)` semantically; the
lifter and constant-folder collapse it to a self-AND and then to
a zero-check via the flag-tree decomposition.

`mov eax, [rsp+8]` → `Load(Add(InitialVar(rsp), IntConst(8)))`.
After `StackOffsetDetect` (which annotates stack-relative `Store` /
`Load` in `Function::stack_offsets`) and `LoadForward`, a
same-offset preceding store forwards through.

`call <imm>` → `Call(at=<imm>)` (resolved jump-target).  Indirect
`call rax` lifts to `IndirectBranch` that the orchestrator's
fixed-point loop resolves.

`xor eax, eax` → `Xor("v", "v")` zero-idiom.  The string back-
reference (or the Rust equivalent `xor(var(c), var(c))`) enforces
must-be-same.

### AArch64

NZCV flag chains: `cmp x0, x1 ; b.gt L` decomposes through Sleigh
into a tree of flag-bit reads.  `FlagCmpCanonicalize` rewrites the
tree to a single `IntCmpOp` — the pattern should match the
post-canonicalisation form.

`ldr w0, [sp, #16]` → `Load(Add(InitialVar(sp), IntConst(16)))`.
Same shape as x86 (the lifter is arch-agnostic at this layer).

`cbz w0, L` → `If(IntEqual(w0, IntConst(0)))` directly (no flag
intermediate — Sleigh emits the comparison inline).

LR-as-callee-saved: AArch64's `x30` is captured by the
`LinkRegister` indirect-branch resolver; the pattern surface
exposes `function_arg_reg` for first-arg `x0`, etc.

`csel x0, x1, x2, cc` → `If(cc){x1}{x2}` (a branch-free select
encoded as If/Phi after orchestrator drives the indirect-branch
loop to fixed point).  Pattern: `phi().input(0, "x1").input(1, "x2")`
inside an `If(cond=cc)` parent (use captures on both branches).

### ARM (incl. Thumb)

ARM IT blocks (`itte eq ; moveq … ; movne …`) lift to standard
If/Phi.  Thumb mode is a context-register state — the lifter
preserves it across same-region instructions.

`bx lr` → `Return` (LR-as-callee-saved); the indirect-branch
resolver classifies via `apply_link_register`.

### MIPS (32 / 64, BE / LE)

Branch-delay slots: `beq rs, rt, L ; nop` lifts to an `If` with the
delay-slot insn lifted into both successor regions.  Most patterns
should match the If condition, not the delay-slot fill.

MIPS32 `mul a, b` lifts to `IntMul` on a 64-bit unique varnode
followed by a 32-bit Truncate.  ConstantFold's
`narrow_mul_through_sext` rule strips the Truncate/SignExt round-
trip; pattern authors querying optimised IR will see the
post-rewrite shape (`mul(a, b)` at the natural width).

### PowerPC (32 / 64, BE / LE)

CR (condition register) bits: `cmpw cr0, r3, r4 ; bne cr0, L`
decomposes through Sleigh into CR-bit reads.
`FlagCmpCanonicalize` folds the CR chain to a single `IntCmpOp`.

LR-as-callee-saved: PowerPC's `LR` register; same as AArch64.

PPC64 ELF V1 vs V2 ABI affects function-arg register lookup but
not the IR shape; pattern authors don't usually care.

## Walking from asm → IR step by step

For each asm instruction, the lift trajectory is:

```
asm → Sleigh pcode → ValueLifter::lift → IR nodes (+ side-tables)
                                         ↓
                                       lift-time canonicalisation
                                         (Sub→Add(_, Neg(_)), etc.)
                                         ↓
                                       optimizer pipeline
                                         (FlagCmpCanonicalize,
                                          ConstantFold, KnownBits,
                                          StackOffsetDetect, …)
                                         ↓
                                       canonical IR shape (Pat targets this)
```

For pattern authoring, decide which layer you're querying:

- **Unoptimised IR** (lifter output, before opt pipeline) — see
  the source-level shape (with lift-time canonicalisations applied).
  Useful for debugging the lifter itself.
- **Stable-opt-output IR** (after `build_stable_optimizer_pipeline`)
  — most patterns target this.  Constant fold, dead branch, flag-
  cmp canonicalisation, redundant phi removal, alias-split have all
  run.
- **Destructive-opt-output IR** (after the full pipeline) —
  `FunctionArgDetect`, `CallStackArgCollect`, `LoadForward`
  have all run as post-passes.  Function args are canonicalised via
  `Function::arg_index_to_nodes`; stack offsets are visible via
  `Function::stack_offsets`.

The `strider.run(...)` Python API and the `orchestrator::run()` Rust
API both yield the post-destructive-pipeline IR.  This is what
`Graph.find_all` queries by default.

## Dumping for visual confirmation

- `cargo run -p strider-orchestrator --example orchestrator_demo` —
  defaults to `fixtures/out/x86/arithmetic.elf::add`, writes
  `cfg.html` (control flow), `graph.html` (pre-opt IR),
  `graph-opt.html` (post-opt IR) to the workspace root.
- `cargo run -p strider-orchestrator --example dump_arch_cmps` — per-
  arch IR cmp shapes for the FlagCmpCanonicalize spec.
- Python: `Graph.to_dot()` / `Graph.to_html()`.
- For new fixtures, add a source file under `fixtures/<arch>/` and
  extend `fixtures/Makefile`; rebuild fixtures with `make` from the
  fixtures dir.

**Dump APIs (v12+):**

- `strider_orchestrator::dump_per_region(graph, exit_controls, lift_gen, sleigh, dir)`
  — emits one `region_<N>.html` per region into `dir`.  Useful when
  the full-graph dump is too dense to read; each file scopes the
  view to a single region's subgraph.  `lift_gen` must match
  `outcome.function.generation()` — `Function::compact` between lift
  and dump invalidates the captured ids and the helper surfaces a
  typed error rather than silently rendering the wrong region.
- `strider_orchestrator::dump_neighborhood(graph, anchor, depth, sleigh, path)`
  — emits a single HTML file scoped to the BFS frontier within
  `depth` hops of `anchor` (via
  `strider_ir::walk::collect_neighborhood`).  Use when you have a
  candidate match anchor and need to inspect just its local
  context.  Rejects foreign / stale anchors with a typed error.
- URL-fragment deep links: append `#n=42` to a dumped `graph.html` URL
  to auto-focus node 42 in the viewer.  Useful for pasting "look at
  node N in this dump" links into bug reports / chat without
  walking the renderer's pan-and-zoom by hand.

## Worked examples (sketch list — flesh out as users hit each case)

1. x86_64 `add eax, 8 ; cmp eax, 0 ; je L` → pattern for the if-cmp
   chain.  Match shape: `if_node().cond(int_eq(add(any(), int_const(8)), int_const(0)))`.
2. AArch64 `ldr x0, [sp, #16] ; cbz x0, L` → pattern for the
   load-then-zero-check.  Match shape: `if_node().cond(int_eq(load().addr(add(initial_var(sp_vn), int_const(16))), int_const(0)))`.
3. MIPS32 `lw $a0, 16($sp) ; mul $v0, $a0, $a1 ; jr $ra` → pattern
   for the mul-of-loaded-arg (use `function_arg(1)` to capture the
   second positional arg after `CallStackArgCollect` runs).
4. PowerPC64 ELF V2 leaf function epilogue → match the `Return` with
   its `ret_val_regs` slots; cross-check against the `cc_metadata`'s
   ret-val list.

For an end-to-end walk-through of an arbitrary new arch idiom: dump
the IR (`Graph.to_html`), look at the lifted shape, then pick the
pattern root by following lift-time canonicalisations.
