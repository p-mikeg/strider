# Getting started with Strider

Strider answers questions about a compiled function without source or debug
symbols: write a pattern in Python, get back every place it matches. The
[README](../README.md#quickstart) shows this end to end; this page maps the
pipeline. New since 0.1.0: [README](../README.md#whats-new-since-010).

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the function's bytes from an ELF (executable, shared library or
   `.o`) or a raw buffer ([loading](python-api.md#1-loading-a-binary)).
2. **Lift** each instruction into p-code, the CPU-independent form of GHIDRA's
   Sleigh. An instruction Sleigh leaves opaque (a syscall, a trap) is
   classified by a built-in ABI table; a call is modelled by its calling
   convention, never by reading the callee
   ([options](python-api.md#liftlifteroptions-per-call-tuning)).
3. Build a **CFG**: which regions (straight runs of instructions) jump to
   which. ARM/Thumb and MIPS16 code decodes in the mode its edge carries.
   From an ELF, GCC `.cold` fragments are followed as part of their function.
4. Build the **IR**, a graph with one node per computed value and one edge per
   dependency. Values carry their exact width, from the 1-bit `I1` up to
   `I512`. This is what you query; `function.to_text()` prints one line per
   node and `function.validate()` checks its invariants.
5. **Optimize** so equivalent code reaches a query in fewer shapes: constants
   fold, `x + (x << 1)` becomes `x * 3`, a phi carrying one value collapses
   ([optimizations.md](optimizations.md)). `function.optimizer_converged` is
   false when a run stopped at its iteration cap.
6. **Resolve** indirect branches: classify each against the optimized IR, feed
   the targets back, re-lift, repeat until the edges stop changing.
   `cfg.incomplete` lists what is left
   ([reasons](python-api.md#12-the-cfg-stridercfg)); `analyze` reports it
   rather than raising, so check it before trusting an absence. The answer
   rests on `AssumptionOptions`, claims the analysis cannot check;
   `function.assumptions` holds those in force.
7. **Query** it with patterns, or **rewrite** it with `strider.template`.
   [python-api.md](python-api.md#4-patterns) is the reference.

Next: [python-guide.md](python-guide.md), the walkthrough;
[vocabulary.md](vocabulary.md) defines the terms.
