# Changelog

## 0.2.0

Both the Python and the Rust surfaces changed; the two are listed separately.

### Breaking, Python

- Pattern builders renamed `add` -> `int_add` to follow the convention; const
  readers shortened.
- A bare string is no longer a capture operand; use `Capture(name)`.
- Raw ints coerce to `int_const`, so `int_add(base, 4)` works.
- `call().at()` / `.at_any()` -> `.target()`, which also takes a list of
  candidate targets.
- `one_of` reports every arm that matches, not just the first.
- The any-operator pattern builders take the `any_` prefix the rest of the
  namespace uses, and spell `binary` / `unary` out like their fixed-operator
  siblings `int_binary` / `int_unary`: `int_bin_any` -> `any_int_binary`,
  `int_un_any` -> `any_int_unary`, `float_bin_any` -> `any_float_binary`,
  `float_un_any` -> `any_float_unary`, `bool_bin_any` -> `any_bool_binary`,
  `int_cmp_any` -> `any_int_cmp`, `float_cmp_any` -> `any_float_cmp` and
  `function_arg_any` -> `any_function_arg`.
- `switch().address(p)` -> `.selector(p)`: `inputs[1]` is the value dispatched
  on, and the arms' addresses are the control outputs.
- `.cap(name)` is gone: `.capture()` takes a `Capture` or a name, the key type
  `Match` readers and `m[key]` already take.
- `find_all(..., ignore_casts_mask=)` is gone: `ignore_casts` takes a bool or
  a `CastMask`.
- `to_dot(style=)` / `to_html(style=)` are gone: `pretty` takes a bool or a
  `DotStyle`.
- `strider.template.signed_int_const` is gone; `template.int_const` builds the
  same constant. The match side keeps both, where the two differ, as
  `int_const` and `int_const_any_width`.
- `Lifter.neighborhood_dot(function, center, ...)` is gone:
  `Function.neighborhood_dot(center, ..., pretty=True)` renders it.
- The symbol accessors return a `Symbol` record (`name`, `address`, `size`,
  `end`, `is_function`, `region`), so `symbol(name)` is no longer an address:
  `symbol_size`, `symbol_size_opt` and `symbol_region` are gone, and
  `functions()` / `iter_symbols()` yield `Symbol`s rather than tuples.
  `size` is `None` when the ELF records no extent, and `functions()` yields
  those symbols rather than dropping them.
- `analyze` raises when indirect-branch resolution oscillates.
- An `ET_REL` symbol's address changes: sections that shared one are rebased
  apart.
- `wide_const_bytes()` returns `bytes`; it returned `list[int]`.
- `Node` equality and hash include the graph generation, so a handle held across
  an `optimize` no longer compares equal to a fresh one.
- The unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, without their `assume_`
  prefix: `assume_distinct_sp_bases_disjoint` ->
  `distinct_sp_bases_disjoint`, `assume_callee_preserves_stack_args` ->
  `callee_preserves_stack_args`, plus `noalias_allocators` and
  `escape_analysis`.
- `LifterOptions(calls_clobber=...)` -> `assume_incoming_args_survive_calls`,
  inverted, defaulting to `True`. It only ever reached incoming-argument
  detection.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not, so "any integer constant" is now `int_const()`:
  `int_const` / `float_const` / `bool_const` take a `Capture`, or no argument
  at all, in place of a value. `I1` is an integer type, so `any_int` covers
  booleans too. `bool_value` is gone: `any_bool` is it.
- `signed_int_const` -> `int_const_any_width`, which also takes a list, like
  `int_const`. The axis is the width the value was extended from, not its
  sign: `int_const` already matches a negative.
- `preceded_by` -> `ctrl` on `SwitchPat`, `RetPat`, `IndirectBranchPat` and
  `UnreachablePat`. It was the same slot `CallPat.ctrl` names, under a second
  name; relational vocabulary belongs with `dominates` / `reaches` in
  `pattern.constraints`.
- `CfgOptions(call_other_abis=...)` values are `strider.sleigh.CallOtherAbi`
  objects, not the strings `"noop"` / `"pure"` / `"mem_clobber"` /
  `"no_return"`: `{"trap": "no_return"}` becomes
  `{"trap": strider.sleigh.CallOtherAbi.no_return()}`.
- `LoadPat.mem_in` / `StorePat.mem_in` -> `.mem`, the name `call()`,
  `call_other()` and `indirect_branch()` already give that slot. It is the
  node's memory predecessor either way, so `load` and `store` join the
  `MemPat` mixin.
- `PhiPat.input` / `MemPhiPat.input` -> `.phi_input`, which indexes
  predecessors (raw slot `idx + 1`) the way they always did. `.input` is now
  the raw-slot method every other builder's `input` is, so `.input(0, p)`
  reaches the phi token rather than predecessor 0's value.

### Breaking, Rust

- `LoadPat::mem_in` / `StorePat::mem_in` -> `mem`, matching `CallPat::mem`;
  `PhiPat::input` / `MemPhiPat::input` -> `phi_input`, with `input` now the
  unshifted raw slot.
- `MemRegion::data` / `data_mut` are gone; a region serves bytes through `read`,
  which applies relocation patches.
- `elf_load_with_relocations`, `elf_load_readonly_with_relocations` and the three
  sections-only region loaders are gone; use `OwnedElf::regions`.
  `apply_elf_relocations` takes the `LoadFilter` its regions were built with, and
  `apply_elf_relocations_autoload` is gone.
- `Cfg::region_id_at_start` is gone.
- `CallOtherOverrides::new` takes `(String, CallOtherOverride)` entries, where
  `CallOtherOverride` is either a `CallOtherClass` or a caller-resolved
  `BuiltCallOtherAbi`, so an override can carry an implicit register footprint.
  `classify_with` returns a `CallOtherLookup`, whose `built(&regs)` yields the
  resolved footprint (`None` for `NoOp`), borrowed when the caller pre-resolved
  it.
- Every pattern builder spells its name the way `strider.pattern` does, so one
  query reads the same in either language. The 21 integer builders take an
  `int_` prefix (`add` -> `int_add`, `and` -> `int_and`, `bit_not` ->
  `int_not`, `truncate` -> `int_truncate`, ...); the any-operator builders take
  the `any_` prefix and spell `binary` / `unary` out like their fixed-operator
  siblings (`int_binary_any` -> `any_int_binary`, `int_unary_any` ->
  `any_int_unary`, `float_binary_any` -> `any_float_binary`, `float_unary_any`
  -> `any_float_unary`, `bool_bin_any` -> `any_bool_binary`, `int_cmp_any`
  -> `any_int_cmp`, `function_arg_any` -> `any_function_arg`); `any` ->
  `anything`; `if_node` -> `if_else`. The builder types follow their functions:
  `IntBinaryAny` -> `AnyIntBinary`, `IntUnaryAny` -> `AnyIntUnary`, and
  likewise for the cmp, float and boolean ones.
- `CallPat::at_any(addrs)` is gone: `target(int_const(addrs))` takes a
  collection, matching `.target()` on the Python side.
- `SwitchPat::address` -> `selector`: `inputs[1]` is the value dispatched on,
  and the arms' addresses are the control outputs.
- `BuiltCallingConvention::try_new` -> `validate(&self)`, and
  `BuiltCallingConventionParts`, the struct it took, is gone.
- `CallingConvention::x86_64_all_preserving` is gone on both surfaces; build it
  as `CallingConvention::x86_64_systemv().preserves_all()`.
- `strider_cfg::ResolvedTargets` carries `ResolvedTarget { addr, isa_bit }`
  rather than a bare `u64`, so a `CfgOptions::known_targets` map needs
  `ResolvedTarget::from(addr)`.
- `strider_ir` renames the stack-specific side tables for the memory classes
  they now cover: `SpDecomp` -> `MemDecomp`, `StackId` -> `MemoryId`,
  `stack_slot` -> `memory_class`, `stack_slot_resolved` -> `memory_slot_resolved`,
  `set_stack_slot_not` -> `set_not_memory`, `clear_stack_slots` ->
  `clear_memory_slots`.
- `strider_opt::apply_rules_in_order` is gone; `LoadForward` holds a per-sweep
  memo and is no longer a unit struct, so it needs `LoadForward::default()`.
- `CfgOptions` gains a public `call_other_overrides` field, so a struct literal
  needs `..Default::default()`.
- `strider_pattern::int_const_any_of` is gone: `int_const` takes a collection.
- `MemPat` no longer requires `compile_mem`, and `build_switch` returns the
  `NodeId` it created.
- `BuiltCallingConvention::float_arg_vns` -> `float_arg_slots`, which is
  positional. `StackArgs::index_of` is gone.
- `dominance_frontiers` takes the root. `DomTree::nodes` must yield each node
  once.
- `IndirectBranch` takes an optional fourth input, the ISA mode its instruction
  commits. `Unreachable` takes an optional memory input.
- `ValueType` gains `I24`, `I40`, `I56`, `I72`, `I96`, `I112`, `F16` and `F128`.
- `OptOptions` gains `resolve_indirect_branches` and, in a new
  `AssumptionOptions` group, `escape_analysis` and `noalias_allocators`.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not; `any_int_const` / `any_float_const` / `any_bool_const`
  stay the constant-only forms, and `int_const` / `float_const` / `bool_const`
  take a `Capture` in place of a value, so `any_int_const().capture(c)` is
  `int_const(c)`. `I1` is an integer type, so `any_int` covers booleans too.
  `bool_value` is gone: `any_bool` is it.
- `signed_int_const` -> `int_const_any_width`, which also takes a collection,
  like `int_const`. The axis is the width the value was extended from, not its
  sign: `int_const` already matches a negative.
- `MemAliasOptions` is gone. `OptOptions::arg_alias` splits: the two unchecked
  claims move into `OptOptions::assumptions` (`AssumptionOptions`) as
  `distinct_sp_bases_disjoint` and `callee_preserves_stack_args`, joining
  `escape_analysis` and `noalias_allocators`; `calls_clobber` becomes
  `OptOptions::assume_incoming_args_survive_calls`, inverted and defaulting to
  `true`.
- `preceded_by` -> `ctrl` on `SwitchPat`, `RetPat`, `IndirectBranchPat` and
  `UnreachablePat`.
- `WithOutput`'s slot is an `Option<usize>`, `None` being the existential
  `any_output()`.

### Added

- `Cfg.to_dot(style=)`, matching `Cfg.to_html(style=)`. Both default to
  `"dark_cfg"`.
- Object files (`ET_REL`) load.
- Indirect branches resolve with a per-target ISA mode, so ARM/Thumb and MIPS16
  targets decode correctly. `known_targets` seats your own;
  `resolve_indirect_branches=False` turns the classifier off.
- A control cycle that never exits (`while (1)`, a spin loop, a `panic` helper
  ending in a self-jump) is anchored at lift time, so its stores and their
  operands survive into the graph and answer queries.
- `escape_analysis` and `noalias_allocators` tune memory precision.
- `preserves_all()` / `preserves_regs()` calling-convention transforms, and an
  `arm_aapcs_soft` preset for soft-float ARM32.
- An empty alternation is a pattern: `one_of([])`, `first_of([])` and
  `call().target([])` match nothing rather than raising, so a caller assembling
  the arms at runtime needs no empty-list case.
- `one_of` / `first_of` take any pattern as an arm and nest in a value, memory
  or control slot. `JoinPredicate`, `load().non_stack()`,
  `store().heap_only()`, `find_unique_value`, `phi().any_input()`.
- `Pat.ordered()` pins operand order on any binary pattern, `int_add(a, b)` as
  much as `int_binary(op, a, b)`, and a no-op where the operands are ordered
  already, as on `int_le` or `int_shl`.
- `pattern.CaptureKey` names the `Capture`-or-name argument that `.capture()`
  and every `Match` reader take, alongside the existing `PatLike` /
  `TemplateLike` / `MemLike` aliases.
- `Lifter.reader()` and `.rom()` return the code and read-only sources;
  `ElfLifter.symbol_at(address)` reverse-resolves an address, plus
  `.endianness`, `.iter_symbols()` and `.is_arm_be8`.
- `analyze` returns an `AnalyzeResult` with `.cfg` / `.function` /
  `.unresolved`, still unpackable as a 3-tuple.
- `function_arg` supports float; `call().arg(N)` reaches a float argument.
- `AssumptionOptions(callee_preserves_stack_args=True)` forwards a spill at the
  stack top across a call.
- `CfgOptions(call_other_abis={name: CallOtherAbi})` reclassifies a Sleigh
  user-op. `strider.sleigh.CallOtherAbi` carries the footprint-free classes
  `noop()` / `pure()` / `mem_clobber()` / `no_return()`, plus
  `CallOtherAbi.custom(sleigh, implicit_reads=[...], implicit_writes=[...],
  clobbers_memory=, no_return=)` for an implicit register footprint, resolved
  against the `Sleigh` at construction the way `CallingConvention.custom` is.
- `Lifter.user_op_names()` lists every Sleigh user-op name the architecture can
  emit, and `Lifter.call_other_abi(name, opts=None)` reads back the
  classification in force: the `opts` entry when there is one, else the
  built-in table, else `None`.
- The pattern builders share one vocabulary, declared in the stubs as the
  `runtime_checkable` protocols `NodePat` (`capture` / `when` / `into_pat`),
  `InputPat` (`input` / `any_input`), `CtrlPat` (`ctrl`), `MemPat` (`mem`),
  `MemAccessPat` (`addr` / `bit_width` / `space` / `stack_offset` /
  `stack_only` / `non_stack` / `heap_only`, on `load` and `store`),
  `OrderedPat` (`ordered`, on `Pat` and the three binary-op builders) and
  `OutputPat` (`output` / `any_output`). Each builder lists only the mixins it
  genuinely has, so `isinstance(load(), InputPat)` is true while
  `isinstance(entry(), InputPat)` is false.
- `IfPat.ctrl(p)` constrains an `If`'s control predecessor; it had no accessor
  for `inputs[0]` at all.
- `input(i, p)` and `any_input(p)` reach every node builder but `entry()`,
  whose `Entry` is `inputs: []`. `input` addresses raw slots, whose numbering
  is per kind; the IR's `expected_signature` is the source of truth.
- `output(slot)` and `any_output()` reach every node builder but the sinks
  `ret()`, `indirect_branch()` and `unreachable()`, all `outputs: []`. Both
  return `OutputSlotPat`, the terminal `CallPat.output` already returned;
  `CallOutputPat` still names it.
- `indirect_branch().target(p)` and `switch().selector(p)` take a list of
  candidates, like `call().target(p)`; an empty list matches nothing.

### Performance

- Loading an image maps it and patches relocations on read, and a symbol
  resolves through an index, so opening a kernel no longer walks every
  relocation and symbol up front.
- Region ownership is bounded by the longest shadowed span rather than the
  longest span, so a lookup miss over disjoint regions is one probe.
- Re-imposing a region's decode context skips the write, and the Sleigh parse
  cache flush it carries, when the context already matches.
- Stack-argument collection scans a call's memory chain once rather than once
  per slot.
- A stack jump table answers each entry from a slot map that descends control
  merges, instead of re-walking the memory chain per entry.
- A pattern join filters as it builds the product instead of materialising it.

### Fixed

- A resolved interworking table lost its ISA mode, decoding Thumb as ARM.
- A seated switch could drop a re-derived arm and still report full resolution.
  Any site that loses a successor is now reported in `unresolved`.
- A `known_targets` seed was dropped against a mode-bearing answer.
- A p-code temporary of an unmapped width (x86 `adcx` / `adox` / `lsl`, AArch64
  fixed-point `ucvtf`) failed the whole function's lift.
- mips64el relocations were patched using a relocation kind read out of the
  transposed symbol index.
- Float call arguments were compacted, so an argument's index at the call did
  not match its index at the callee.
- A callee-saved `d8`-`d15` was treated as preserving all 128 bits of the `q`
  register containing it.
- A load could forward across a call from an outgoing argument slot when an
  opaque store hid the argument write.
- An exception raised inside a `.when()` predicate could surface from a later
  query.
- A branch into a region's last instruction aborted the build or decoded an
  overlapping region.
- Captures bound inside an `if` branch were invisible to `find_all`'s join
  constraints.
- An `if` branch sub-pattern committed to its first binding, so a match was lost
  outright when a later branch or an outer guard rejected that one.
- An alternation used as a whole pattern reported a match with its capture
  unbound.
- Constant-folding a branch could strand a loop with no path to a terminator.
- `one_of![p]` bound control and memory edges as values.
- `one_of([...]).capture(...)` was rejected in a memory slot.
- A jump-table entry could be enumerated as the table index.
- `Cfg.region_at` missed an address inside a region's last instruction.
- `ElfLifter.analyze(name)` dropped `call_other_abis`.
- Applying an `OptimizerPipeline` emptied it, so a second `optimize` with the
  same object ran no passes.
- Object-file sections sharing an address served each other's bytes.
- `functions()` / `symbols()` were empty for an object file, and for a library
  exporting through `.dynsym`.
- An ARM32 function with a NEON register and a call failed to analyze.
- A `MemReader` callback re-entering `analyze` aborted the process.
- The explorer evaluated its query string with `eval`.
- A MIPS function ending in a `BUG()` trap decoded past its own extent and was
  rejected whole.

## 0.1.0

First release.
