# Getting started with Strider

Strider answers questions about a compiled function without source or debug
symbols: write a pattern in Python, get back every place it matches. The
[README](../README.md#quickstart) shows this end to end; this page maps the
pipeline.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the bytes of a function out of the binary. An executable, a shared
   library, or an unlinked object file all load, mapped rather than copied,
   with relocations applied as the bytes are read; an object file's undefined
   symbols get addresses of their own.
   [python-api.md](python-api.md#1-loading-a-binary) has the loaders, the
   symbol sources for a stripped image (`add_symbol_file`, `add_symbols`), and
   the knobs over the mapping, `relocated_got_is_rom` among them.
2. **Lift** each machine instruction into p-code, GHIDRA's Sleigh engine's
   CPU-independent form. An instruction Sleigh leaves opaque, like a syscall or
   a trap, is classified by a built-in ABI table saying whether it returns and
   what it clobbers; `user_op_names()` / `call_other_abi(name)` show the table,
   `CfgOptions(call_other_abis=...)` overrides an entry, and
   `CallOtherAbi.custom(sleigh, ...)` names the registers one reads and writes.
   A float conversion rounds and saturates as the ISA does. A callee's own
   convention comes from the ELF header unless
   `LifterOptions(per_address_ccs=...)` names one for its address.
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which. Each address decodes once, in the ISA mode carried by the
   edge that reached it, so ARM and Thumb code in one binary each decode
   correctly. Bytes that hold no instruction, such as a literal pool an ARM or
   AArch64 ELF's `$d` mapping symbols mark, are not decoded;
   `CfgOptions(data_ranges=...)` names ranges the symbols miss.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. Values carry their exact width, from the
   1-bit `I1` up to `I512`, so the odd widths SIMD and long-double code produce
   get their own types instead of being rounded to a machine word. This is the
   thing you query; `function.to_text()` prints one line per node and
   `function.validate()` checks its invariants. A call is modelled by
   its calling convention, never by reading the callee.
5. **Optimize** the IR so that equivalent code reaches a query in fewer shapes.
   Constants fold, `x + (x << 1)` becomes `x * 3`, and a phi or a cycle of phis
   carrying one value collapses, so a pattern written against the source shape
   often will not match. [optimizations.md](optimizations.md) lists what each
   pass reshapes; [python-api.md](python-api.md#2-analyzing-a-function) says
   what `LifterOptions(assumptions=...)` buys and what it costs, and
   `AssumptionOptions.none()` makes no unchecked claim. Two claims,
   `msr_register_keeps_mode` and `fp_rounding_mode_is_nearest`, are read by
   the lift: change them by analysing again.
   `function.optimize(assumptions=...)` re-runs the pipeline with more claims,
   and `LifterOptions(pipeline=...)` replaces the pass list outright.
6. **Resolve** the indirect branches: classify each one against the optimized
   IR, feed the targets back, re-lift, and repeat until the edge set stops
   changing. `CfgOptions(known_targets=..., no_return_targets=...)` seats your
   own answers; `LifterOptions(resolve_indirect_branches=False)` skips the
   step. `cfg.incomplete` lists what is left over, a return whose target is
   not provably the caller's included;
   [python-api.md](python-api.md#12-the-cfg-stridercfg) describes each reason.
   `analyze` returns even when it cannot vouch for every path, so check
   `cfg.incomplete` before trusting an absence ("no store to X"): code behind
   a listed address may be missing.
7. **Query** it, or **rewrite** it if you want the graph changed rather than
   read. `field`, `code_ptr`, `first_of` and `PhiPat.input_from` cover common
   shapes, `find_unique_value` reads the one constant a capture binds, and
   `JoinPredicate` relates matches by your own rule.
   [python-api.md](python-api.md#4-patterns) is the reference for both sides
   and [python-guide.md](python-guide.md) the walkthrough.

## Where the API lives

Import each name from its home submodule:

```python
strider.lift      # the entry point: load_elf and lifter, plus LifterOptions,
                  # AssumptionOptions and the AnalyzeResult they produce
strider.ir        # Function and Node: the graph you query
strider.cfg       # Cfg (with Cfg.incomplete) and CfgOptions
strider.pattern   # the query DSL, plus .pattern.constraints for joins
strider.template  # the build side of a rewrite
strider.opt       # OptimizerPipeline and the passes it runs
strider.reader    # BufferReader, Symbol, and the memory interfaces
strider.sleigh    # SleighArch, CallingConvention, CallOtherAbi, Vn, VnSpace;
                  # CallingConvention.custom(arch, ...) builds an ABI of your
                  # own out of register names
strider.StriderError    # the one top-level name
```

`strider.explore.visualize(fn)` serves the graph as a browser explorer, the
quickest way to see the shape a pattern must match
([python-api.md](python-api.md#10-visualizing)).

`analyze`, `build_cfg` and `add_elf` hold a handle alone. To analyse in
parallel, each worker builds its own handle over the same `arch` / `reader()`
/ `rom()` (gathered before any analysis is in flight, after every `add_elf`)
and calls `analyze(addr, cc)`; `analyze` releases the GIL.

An analysis failure is a `strider.StriderError`, and its `.backtrace` carries
the error and its causes followed by the Rust backtrace; `STRIDER_BACKTRACE=1`
folds that backtrace into the message too.

## Where to go next

- **[vocabulary.md](vocabulary.md)** defines the terms (region, phi, dominator,
  varnode, ...). Start here if any of them are new.
- **[python-guide.md](python-guide.md)** is the practical walkthrough: analyzing
  many functions, writing patterns, constraints, rewrites, drawing the graph,
  and what to check when a pattern does not match.
- The [quickstart](../README.md#quickstart) is this pipeline end to end in
  Python, and the [README](../README.md) indexes the rest of the guides and the
  runnable examples.

## Upgrading

The README's [Upgrading](../README.md#upgrading) section points to the changes
that alter an answer silently; [CHANGELOG.md](../CHANGELOG.md) has the rest.
