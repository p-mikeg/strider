# Getting started with Strider

Strider answers questions about a compiled function without source or debug
symbols: write a pattern in Python, get back every place it matches. The
[README](../README.md#quickstart) shows this end to end; this page maps the
pipeline.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the function's bytes. Executables, shared libraries and unlinked
   object files load mapped, with relocations applied as bytes are read; an
   object file's undefined symbols get addresses of their own.
   [python-api.md](python-api.md#1-loading-a-binary) covers the loaders,
   symbols for a stripped image (`add_symbol_file`, `add_symbols`) and
   `relocated_got_is_rom`.
2. **Lift** each instruction into p-code, the CPU-independent form of GHIDRA's
   Sleigh. An instruction Sleigh leaves opaque (a syscall, a trap) is
   classified by a built-in ABI table: whether it returns and what it
   clobbers. `user_op_names()` / `call_other_abi(name)` show the table,
   `CfgOptions(call_other_abis=...)` overrides an entry, and
   `CallOtherAbi.custom(sleigh, ...)` names the registers one reads and writes.
   A callee's convention comes from the ELF header unless
   `LifterOptions(per_address_ccs=...)` names one for its address.
3. Build a **CFG**: which regions (straight runs of instructions) jump to
   which. Each address decodes once, in the ISA mode carried by the edge that
   reached it, so ARM and Thumb code in one binary both decode. Literal pools
   marked by `$d` mapping symbols are not decoded;
   `CfgOptions(data_ranges=...)` names ranges the symbols miss.
4. Build the **IR**, a graph with one node per computed value and one edge per
   dependency. Values carry their exact width, from the 1-bit `I1` up to
   `I512`. This is what you query; `function.to_text()` prints one line per
   node and `function.validate()` checks its invariants. A call is modelled by
   its calling convention, never by reading the callee.
5. **Optimize** so equivalent code reaches a query in fewer shapes: constants
   fold, `x + (x << 1)` becomes `x * 3`, a phi carrying one value collapses. A
   pattern written against the source shape may not match;
   [optimizations.md](optimizations.md) lists what each pass reshapes.
   `LifterOptions(assumptions=...)` trades soundness for precision
   ([python-api.md](python-api.md#2-analyzing-a-function));
   `AssumptionOptions.none()` makes no unchecked claim. The lift reads
   `msr_register_keeps_mode` and `fp_rounding_mode_is_nearest`, so changing
   them means analysing again. `function.optimize(assumptions=...)` re-runs
   the pipeline with more claims; `LifterOptions(pipeline=...)` replaces the
   pass list.
6. **Resolve** indirect branches: classify each against the optimized IR,
   feed the targets back, re-lift, repeat until the edges stop changing.
   `CfgOptions(known_targets=..., no_return_targets=...)` seeds your own
   answers; `LifterOptions(resolve_indirect_branches=False)` skips the step.
   `cfg.incomplete` lists what is left, including returns whose target is not
   provably the caller's ([reasons](python-api.md#12-the-cfg-stridercfg)).
   `analyze` returns even when incomplete, so check `cfg.incomplete` before
   trusting an absence ("no store to X").
7. **Query** it, or **rewrite** it. `field`, `code_ptr`, `first_of` and
   `PhiPat.input_from` cover common shapes, `find_unique_value` reads the one
   constant a capture binds, and `JoinPredicate` relates matches by your own
   rule. [python-api.md](python-api.md#4-patterns) is the reference and
   [python-guide.md](python-guide.md) the walkthrough.

## Where the API lives

```python
strider.lift      # load_elf, lifter, LifterOptions, AssumptionOptions,
                  # AnalyzeResult
strider.ir        # Function and Node: the graph you query
strider.cfg       # Cfg (with Cfg.incomplete) and CfgOptions
strider.pattern   # the query DSL, plus .pattern.constraints for joins
strider.template  # the build side of a rewrite
strider.opt       # OptimizerPipeline and its passes
strider.reader    # BufferReader, Symbol, and the memory interfaces
strider.sleigh    # SleighArch, CallingConvention, CallOtherAbi, Vn, VnSpace
strider.StriderError    # the one top-level name
```

`strider.explore.visualize(fn)` serves the graph as a browser explorer
([python-api.md](python-api.md#10-visualizing)).

`analyze`, `build_cfg` and `add_elf` hold a handle exclusively. To analyse in
parallel, give each worker its own handle over the same `arch` / `reader()` /
`rom()` (gathered after every `add_elf`, before any analysis starts) and call
`analyze(addr, cc)`; `analyze` releases the GIL.

An analysis failure raises `strider.StriderError`; its `.backtrace` holds the
error chain and the Rust backtrace, and `STRIDER_BACKTRACE=1` adds that
backtrace to the message.

## Where to go next

- [vocabulary.md](vocabulary.md) defines the terms (region, phi, dominator,
  varnode, ...).
- [python-guide.md](python-guide.md) walks through analyzing many functions,
  patterns, constraints, rewrites, drawing the graph, and debugging a pattern
  that does not match.
- The [README](../README.md) indexes the other guides and the runnable
  examples; its [Upgrading](../README.md#upgrading) section lists the changes
  that alter an answer silently.
