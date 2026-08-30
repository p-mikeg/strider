# Changelog

## 0.2.0

Both the Python and the Rust surfaces changed; the two are listed separately.

### Breaking, Python

- A `CallingConvention.custom(sleigh, ..)` or `CallOtherAbi.custom(sleigh, ..)`
  resolves register names against the `Sleigh` it is given and freezes the
  varnodes, so using one with a `Lifter` of another architecture now raises.
  It used to analyse silently against the wrong varnodes: an x86-64 function
  under a convention built from a 32-bit `Sleigh` simply had no arguments.
- `cc.no_return()` passed as `analyze`'s main `cc` raises. It was
  silently dropped there and only ever meant anything as a `per_address_ccs`
  override.
- `Match[capture]` raises once the function has been compacted, like every other
  capture accessor. It used to succeed and leave the failure to the next read.
- `float_is_nan(p)` and `float_le(a, b)` require the operand each repeats to be
  the SAME value, so they match strictly fewer shapes. `float_is_nan` previously
  matched every lowered `float_ne`.
- `switch().output(n)` binds the arm at slot `n`. It used to bind every arm, one
  match each.
- A `.when()` predicate whose matched root is a control or memory edge, or a
  node with no value output, now fails the match instead of being handed a
  fabricated `I1`.

- `Match.op`, `.value_type`, `.vn`, `.node` and `.float_bits` return a value and
  RAISE when the capture is absent, where v0.1.0 returned `None`. The
  `None`-returning forms keep the old behaviour under `_opt` names (`op_opt`,
  `node_opt`, ...). An `if m.op(c) is None:` check now raises instead of taking
  the branch. `const_uint` / `const_int` / `const_bool` are `uint` / `sint` /
  `boolean`.

- Pattern builders renamed `add` -> `int_add` to follow the convention; const
  readers shortened.
- A bare string is no longer a capture operand; use `Capture(name)`.
- Raw ints coerce to `int_const`, so `int_add(base, 4)` works.
- `call().at()` / `.at_any()` -> `.target()`, which also takes a list of
  candidate targets.
- `one_of` reports every arm that matches, not just the first.
- The any-operator pattern builders take the `any_` prefix the rest of the
  namespace uses, and spell `binary` / `unary` out like their fixed-operator
  sibling `int_binary`: `int_bin_any` -> `any_int_binary`,
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
- `Function.to_dot(style=)` / `Function.to_html(style=)` are gone: `pretty`
  takes a bool or a `DotStyle`. `Cfg` keeps `style=` on both.
- `strider.template.signed_int_const` is gone; `template.int_const` builds the
  same constant. The match side keeps both, where the two differ, as
  `int_const` and `int_const_any_width`.
- `Lifter.neighborhood_dot(function, center, ...)` is gone:
  `Function.neighborhood_dot(center, ..., pretty=True)` renders it.
- The symbol accessors return a `Symbol` record (`name`, `address`, `size`,
  `end`, `is_function`, `region`), so `symbol(name)` is no longer an address:
  `symbol_size` is gone, and
  `functions()` / `iter_symbols()` yield `Symbol`s rather than tuples.
  `size` is `None` when the ELF records no extent, and `functions()` yields
  those symbols rather than dropping them.
- An `ET_REL` symbol's address changes: sections that shared one are rebased
  apart.
- `wide_const_bytes()` returns `bytes`; it returned `list[int]`.
- `Node` equality and hash include the graph generation, so a handle held across
  an `optimize` no longer compares equal to a fresh one.
- The unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, without their `assume_`
  prefix: `assume_distinct_sp_bases_disjoint` ->
  `distinct_sp_bases_disjoint`, joining the new `callee_preserves_stack_args`,
  `noalias_allocators` and
  `escape_analysis`.
- `LifterOptions(calls_clobber=...)` -> `assume_incoming_args_survive_calls`,
  inverted, defaulting to `True`. It reaches which loads count as incoming
  arguments, and nothing else: a memory-clobbering `CallOther` blocks whatever
  it says.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not, so "any integer constant" is now `int_const()`:
  `any_int_const` / `any_float_const` / `any_bool_const` are gone, and
  `int_const` / `float_const` / `bool_const` take a `Capture`, or no argument
  at all, in place of a value. `I1` is an integer type, so `any_int` covers
  booleans too. `bool_value` is gone: `any_bool` is it.
- `signed_int_const` -> `int_const_any_width`, which also takes a list, like
  `int_const`. The axis is the width the value was extended from, not its
  sign: `int_const` already matches a negative.
- `preceded_by` -> `ctrl` on `SwitchPat`, `RetPat`, `IndirectBranchPat` and
  `UnreachablePat`. It was the same slot `CallPat.ctrl` names, under a second
  name; relational vocabulary belongs with `dominates` in
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

- Every claim the analysis cannot check now sits in `AssumptionOptions`.
  `LifterOptions(alias_mode="stack_global_disjoint" | "strict")` is gone: the
  mode was one boolean claim wearing an enum, and it is
  `AssumptionOptions(stack_global_disjoint=...)`, defaulting `True`.
  `LifterOptions(assume_incoming_args_survive_calls=...)` moves there too,
  unchanged and still defaulting `True`. `LifterOptions` loses both
  attributes; `strider.lift.AliasMode` is gone. Clearing all six fields of
  `AssumptionOptions` is the only configuration sound under any input, which
  no single knob promised before.
- `Cfg.is_complete()` answers the four-channel question in one call. The
  `AnalyzeResult` docstring used to say an empty `unresolved` meant the answer
  was complete, which contradicted the Rust contract: a site the CFG consumed
  as a `Return` or `TailCall` is reported only through
  `unverified_seeded_sites`.
- `Function.rewrite` and `Function.rewrite_all` drain and refill the
  memory-decomposition side table, as the optimizer pipeline already did. A
  rule that rewires an address left a stale entry, and a rule that built a
  fresh `Load` left none, so `load().stack_only()` and `store().heap_only()`
  silently matched the wrong nodes or none at all afterwards.

### Breaking, Rust

- The `rsleigh` path dependency moved from a sibling `../rsleigh` checkout to
  the `externals/rsleigh` git submodule: clone with `--recursive`, or
  `git submodule update --init --recursive`.
- The MSRV is 1.91; edition 2024's own floor of 1.85 no longer builds the
  workspace.

- `MatchPat`, `NodePredicate` and `PostMatchFn` carry `+ Send`, so a compiled
  `Pattern` moves between threads with the value that owns it. A `.filter()` or
  `.when_match()` closure capturing an `Rc<Cell<_>>` no longer compiles;
  capture an `Arc<AtomicUsize>`.
- `float_is_nan` / `float_le` pin the operand they repeat to one value;
  `switch().output(n)` pins the slot; and `PostMatchFn` takes
  `Option<ValueType>`, so a guard on a root with no value output fails rather
  than seeing a fabricated `I1`. All three used to match too much.
- Removed with no consumer: `Cfg::raw_neighborhood_dot`, the `ConstValue`
  re-export from `strider-ir`, `dot::Result`, `PostOrder::into_visited`,
  `DenseEntitySet::clear`, `Cfg::switch_arm_region`
  (use `switch_arm_regions`), `OwnedElf::ppc64_abi_level`,
  `elf_get_readonly_regions` and `elf_get_loadable_regions_including_writable`
  (use `OwnedElf::regions` with a `LoadFilter`). `MemRegion::fully_covers` is
  `pub(crate)`.
- `NodeKind` gains `input_head_len` / `output_head_len` /
  `expected_input_kind` / `expected_output_kind`, so a consumer outside
  `strider-ir` can read the slot-layout single source of truth instead of
  hardcoding the shift. `strider-pattern`'s `call().arg(n)`, `ret_val(n)` and
  `phi_input(n)` now do. `NodeKind::is_terminator` likewise replaces three
  hand-written copies of the terminator set.
- A template whose declared output kinds contradict its node signature is
  rejected at instantiation rather than building a malformed node.
- `Builder::with_flow_vars` and `with_function_mode` merge into
  `with_flow_context`: the two were illegal apart, and a `debug_assert`
  existed only to catch the case where a caller set one.
- `ArchPreset::ALL` and `ArchPreset::arch()`. Three hand-written preset
  rosters, all of which had gone stale on `arm_be_kernel`, derive from them.
- `pyo3-stub-gen` is gone: it generated into a gitignored directory nothing
  read, no gate compiled it, and the stubs are hand-written and checked by
  `test_stub_parity.py`.

- `AliasMode` is gone. `OptOptions::alias_mode` and
  `OptOptions::assume_incoming_args_survive_calls` are now
  `AssumptionOptions::stack_global_disjoint` and
  `AssumptionOptions::assume_incoming_args_survive_calls`, both `bool` and both
  defaulting `true`, so `OptOptions` is `{ resolve_indirect_branches,
  assumptions }` and one struct holds every unprovable claim.
  `AssumptionOptions`'s `Default` is hand-written rather than derived, so
  `default()` keeps the two claims on and `AssumptionOptions::none()` is what
  clears all six.
- `graph_algorithms::walk::VisitTracker` and
  `graph_algorithms::dominance::DefSites` are gone, each having had one
  implementation. `PreOrder` / `PostOrder` take one type parameter, the graph,
  and own a `DenseEntitySet`; `phi_placement` takes the `HashMap` directly.
- `AnalyzeResult` gained `unverified_seeded_sites`, `interior_branch_targets`
  and `isa_mode_conflicts`. The struct has no `#[non_exhaustive]`, so a
  struct-literal construction must name them.
- `strider-ir-test-utils`' `proptest_gen` module is behind a `proptest-gen`
  feature, so `proptest` no longer builds for consumers that do not ask for it.
- The ARM processor-mode `CallOther` rows (`setUserMode`, `setStackMode`, ...)
  are scoped to the ARM32 presets. They resolved a register name on no other
  architecture, and `setStackMode` silently claimed aarch64 / MIPS `sp`
  clobbered.

- `LoadPat::mem_in` / `StorePat::mem_in` -> `mem`, matching `CallPat::mem`;
  `PhiPat::input` / `MemPhiPat::input` -> `phi_input`, with `input` now the
  unshifted raw slot.
- `MemRegion::data` / `data_mut` are gone; a region serves bytes through `read`,
  which applies relocation patches.
- `elf_load_with_relocations`, `elf_load_readonly_with_relocations` and the two
  sections-only region loaders are gone; use `OwnedElf::regions`.
  `apply_elf_relocations` takes the `LoadFilter` its regions were built with,
  and `apply_elf_relocations_autoload` is gone.
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
  -> `any_int_cmp`, `float_cmp_any` -> `any_float_cmp`, `function_arg_any` ->
  `any_function_arg`); `any` ->
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
  `stack_slot` -> `memory_class`, `stack_slot_resolved` ->
  `memory_slot_resolved`, `set_stack_slot_not` -> `set_not_memory`,
  `clear_stack_slots` -> `clear_memory_slots`.
- `strider_opt::apply_rules_in_order` is gone; `LoadForward` holds a per-sweep
  memo and is no longer a unit struct, so it needs `LoadForward::default()`.
- `CfgOptions` gains a public `call_other_overrides` field, so a struct literal
  needs `..Default::default()`.
- `strider_pattern::int_const_any_of` is gone: `int_const` takes a collection.
- `MemPat` no longer requires `compile_mem`, and `build_switch` returns the
  `NodeId` it created.
- `StackArgs::index_of` is gone.
- `dominance_frontiers` takes the root. `DomTree::nodes` must yield each node
  once.
- `IndirectBranch` takes an optional fourth input, the ISA mode its instruction
  commits. `Unreachable` takes an optional memory input.
- `ValueType` gains `I24`, `I40`, `I56`, `I72`, `I96`, `I112`, `F16` and `F128`.
- `OptOptions` gains `resolve_indirect_branches` and, in a new
  `AssumptionOptions` group, `escape_analysis` and `noalias_allocators`.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not. `I1` is an integer type, so `any_int` covers booleans
  too. `bool_value` is gone: `any_bool` is it.
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

- A converged CFG reports incompleteness through four channels on
  `AnalyzeResult`, not one: `unresolved_indirect_branches` (a lost successor or
  an unseatable widening), `unverified_seeded_sites` (a dispatch consumed as a
  return or tail call -- a complete answer that cannot be verified, which is
  where an ARM `pop {pc}` epilogue lands), `isa_mode_conflicts` and
  `interior_branch_targets`. The first, third and fourth accumulate across
  rounds, so a later round cannot launder an earlier loss;
  `unverified_seeded_sites` is derived from the final CFG. A consumer asking
  whether a result may be incomplete reads all four.
- `Cfg.isa_mode_conflicts()` (Rust: `AnalyzeResult::isa_mode_conflicts`):
  addresses reached carrying two different ISA modes, where one region owns the
  bytes and the losing path's arm is not the stream it believes.
- `Cfg.unverified_seeded_sites()` (Rust: the
  `AnalyzeResult.unverified_seeded_sites` field): dispatch addresses nothing
  verified, whose answer is exactly the caller's `known_targets` and nothing
  the classifier derived, plus every site the CFG consumed outright as a return
  or a tail call, seeded or derived. A `"return"` or single-out-of-function
  seed is consumed at CFG-build time, leaving no placeholder to report, so it
  is named here too. Seating a seed changes the CFG the classifier reads, so a
  stale seed can stop the selector deriving and take the site's real arms with
  it.
- `BuiltCallingConvention::float_arg_slots`, the positional float / vector
  argument registers; v0.1.0 modelled float RETURNS only.
- `Cfg::interior_branch_targets()` and `AnalyzeResult::interior_branch_targets`
  (Python: `Cfg`): branch targets interior to a region but off every
  instruction boundary, whose edge is therefore not exact.
- `Cfg::undecodable_seeded_targets()`, `Cfg::link_register_seated()` and
  `Cfg::tail_call_seated()`: the CFG-build-time diagnostics the orchestrator
  folds into `unresolved_indirect_branches` and `unverified_seeded_sites`.
- `Cfg::function_isa_bit()`: the entry's ISA mode, which `analyze` feeds back
  as the base a resolved target decodes in when its branch commits no mode.
- `STRIDER_NO_MMAP=1` reads an image instead of mapping it. A paging error
  through a mapping is a SIGBUS no caller can catch, which a network or 9p
  mount can raise on a file nothing is writing.

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
  and every `Match` reader take, alongside the existing `pattern.PatLike`,
  `template.TemplateLike` and `reader.MemLike` aliases.
- `Lifter.reader()` and `.rom()` return the code and read-only sources;
  `ElfLifter.symbol_at(address)` reverse-resolves an address, plus
  `.endianness`, `.iter_symbols()` and `.is_arm_be8`.
- `AnalyzeResult` is now a namedtuple, so `.cfg` / `.function` / `.unresolved`
  also unpack as a 3-tuple.
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
- `input(i, p)` and `any_input(p)` reach every node builder carrying the input
  mixin -- not `entry()`, whose `Entry` is `inputs: []`, nor the four operand
  builders `int_binary` / `float_binary` / `bool_binary` / `function_arg`.
  `input` addresses raw slots, whose numbering is per kind; the IR's
  `expected_signature` is the source of truth.
- `output(slot)` and `any_output()` reach every node builder carrying the
  output mixin -- not the sinks `ret()`, `indirect_branch()` and
  `unreachable()`, all `outputs: []`, nor the same four operand builders. Both
  return `OutputSlotPat`, the terminal `CallPat.output` already returned;
  `CallOutputPat` still names it.
- `indirect_branch().target(p)` and `switch().selector(p)` take a list of
  candidates, like `call().target(p)`; an empty list matches nothing.

- `ElfFileMemReader::from_elf_relocated` serves relocated bytes. Every other
  constructor serves the file-initial bytes, which the type doc now says.
- `StriderError.backtrace` carries the Rust backtrace, captured by default;
  `STRIDER_BACKTRACE=1` folds it into the message too.
- `Lifter.optimize` takes `opts=` and threads the handle's `rom`, so a
  hand-built pipeline sees the same read-only image `analyze` does.
- `CallOtherOverrides::new` rejects a duplicate user-op name rather than
  silently keeping the first, which `get` would have shadowed.
- Fifteen names join seven modules' `__all__`, among them `ElfLifter`,
  `load_elf`, `PatLike`, `ValueTy`, `DotStyle` and
  `OptimizerPass`; `get_type_hints` no longer raises `NameError` on a published
  pattern protocol method.
- The explorer pans by mouse drag and by the arrow keys, and zooms about the
  pointer with ctrl+wheel and about the window centre with `+` / `-`, with `f`
  to fit the graph to the window
  and `0` for 100%. A drag ending over a node pans rather than re-centering.
- `visualize(whole=True)` opens on the entire graph rather than a neighborhood,
  and seeds a toolbar toggle so the page can switch back.
- A mapped file must not change on disk while it is loaded.

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
- Dead-branch elimination reuses one escape set per sweep rather than walking
  the whole CFG once per constant-condition branch. Growth over a chain of
  constant diamonds falls from quadratic to linear (measured exponent 2.16 to
  1.07; 288ms to 2.8ms at 32k nodes).
- The stack argument window widens geometrically as intended. Its gate tested a
  stack offset for positivity, and offsets are normally negative, so an
  ascending run of probes rescanned the whole prefix per load (exponent 1.99 to
  1.15).
- `add_elf` checks region overlap against a sorted prefix-max index rather than
  every existing region, so the per-call cost no longer grows with the regions
  already loaded (2.64s to 0.44s over eight images of 3200 regions).
- Dead-branch elimination shares one backward CFG walk across candidate roots
  instead of walking per root: a chain of 64 constant gates over one spin loop
  went from 64 full walks to 1.
- A call's float argument registers are projected once per function, like its
  return and clobber varnodes; the varnode universe is seeded through a hash
  set; `Sleigh::regs()` is fetched once per CFG dump rather than once per
  region; and the resolver moves its classification map between rounds instead
  of cloning it.
- A call's return and clobber varnodes are computed once per function instead
  of twice per call site (7.8% off `build_ir` over 2183 libc functions).
- Applying relocations builds the section layout once, not twice.

### Fixed

- AArch64 `addv` into a byte destination did not zero the rest of the vector
  register, so `__builtin_popcount` read the surviving `cnt` lanes back and
  returned a value with them in it.
- Sign-extending a constant into `I256` / `I512` folded through `i128` and left
  the upper half zero. It emits a real `Extend` past 128 bits, matching what
  the optimizer's own fold does.
- An ISA mode recorded for a region was lost when a later target split it,
  blinding the mode-conflict check for the first half.
- `popcount` / `lzcount` counted over the OUTPUT width: `clz` on a 32-bit
  operand of a 64-bit register read 32 too high on aarch64 and ppc64.
- ARM32 NEON float user-ops (`FloatVectorAdd`, `FloatCompareGT` and 19 more)
  were unclassified, and an unclassified user-op fails the whole function's
  lift, so any ARM function containing a NEON float instruction was
  unanalysable. `disableDataAbortInterrupts`, `HintPreloadInstruction`,
  `isFIQinterruptsEnabled`, `isIRQinterruptsEnabled` and `ClearExclusiveLocal`
  were missing beside siblings that were present, so `cpsid a` failed where
  `cpsie a` lifted. MIPS `syscall` was unclassified on all four MIPS presets.
- PowerPC `r2` and `r13` are callee-saved on every PowerPC convention, and the
  PPC64 stack-argument base is 112 bytes on ELFv1 and 96 on ELFv2 -- the linkage
  area plus the 64-byte parameter save area, not the linkage area alone. Every
  PPC64 stack argument was read at the wrong offset.
- `software_udf` does not return; it was classified pure, so the lift walked
  past an ARM permanently-undefined instruction into whatever followed.
- MIPS64 `clz` / `clo` counted the whole 64-bit register rather than the word,
  and ARM `vcmp` left FPSCR's C clear when unordered, so a float `<=` was true
  for NaN. Both in the vendored Sleigh specs.
- A masked switch index whose real bound lives on a loop back edge failed the
  whole function. Such a site is now abandoned and reported, as is any resolved
  target that turns out not to be code: `analyze` no longer errors on an
  indirect branch it cannot settle.
- An ISA mode pinned at a region's first address could be clobbered inside that
  region by a change point a sibling region wrote, decoding the rest in the
  wrong mode. It is now re-imposed per instruction, written only when a
  read-back shows it drifted.
- An address two edges reached in different ISA modes was decoded once, in
  whichever won the work queue, with no diagnostic; the clash is now reported.
- A load straddling the bound between this frame and the caller's outgoing
  argument block was treated as private and forwarded across a call
  (`assumptions.escape_analysis` only).
- A query run from inside a `.when()` predicate could make an enclosing
  `first_of` cut on the wrong arm and drop a real match.
- A CONST branch-target varnode of a width other than 1/2/4/8 failed the whole
  function's lift.
- An explorer left serving at interpreter exit aborted the process; the exit
  hook now joins the serving thread.
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
- PowerPC `frin` folded to the wrong value. GHIDRA defines p-code
  `FLOAT_ROUND` as round-half-away-from-zero, and the constant folder used
  ties-to-even, so `frin(2.5)` folded to 2.0 where the hardware gives 3.0. GCC
  lowers ISO C `round()` to a bare `frin`.
- A `LOAD` from the constant space was lifted as an opaque memory read instead
  of the constant its address encodes, so every PowerPC `rlwimi` / `rldimi` /
  `rldic` / `rldcr` mask stayed unknown through the whole pipeline.
- MIPS64 `drotrv` rotated by `32 - shift` on a 64-bit value, degrading to a
  plain logical shift right for counts above 32. In the vendored Sleigh specs.
- x86 `PSLLD` / `PSLLQ` shifted each vector lane by a different per-lane count
  instead of the one count the ISA reads from `SRC[63:0]`, and `PSRAD` took its
  count from the whole 128-bit operand, so a nonzero upper half saturated every
  lane to the sign. In the vendored Sleigh specs.
- Splitting a region re-pointed every incoming edge at the first half,
  including an edge seated for an address the second half owns. The successor
  was dropped and the function's lift then failed outright.
- A `SHF_TLS` section was mapped although the layout deliberately leaves it at
  address 0, so a non-empty `.tdata` in an object file shadowed `.text` and
  reads at code addresses returned thread-local bytes.
- `R_MIPS_16` was read and written as a two-byte field at `r_offset`; its
  storage unit is the four-byte word there, so on big-endian MIPS both the
  addend and the patch hit the wrong halfword.
- `load_elf` never chose `arm_be_kernel` for an ARM BE8 image, so every BE8
  binary failed to lift.
- A `per_address_ccs` override naming an integer argument register outside the
  function's own convention failed the whole lift. Only the float registers
  were seeded into the tracked set.
- Memory SSA memoised a cycle verdict that is only true relative to the walk
  path that produced it, so a sibling branch could narrow a load's memory edge
  onto a phi reached on one arm of its merge.
- `int_const_any_width` treated `I1` as a width a constant could have been
  widened from, so `-1` matched a plain `1` at every width and any odd value
  matched any all-ones constant.
- A value range for a float returned the empty-mask singleton `{0}`, the
  tightest possible interval, rather than top.
- Python's `int_const([-1])` sign-extended only to 64 bits, so above `I64` the
  list form matched a different constant than the scalar `int_const(-1)`.
- A `FloatConst` kept whatever bits it was built with, so an `F32` constant
  could carry garbage above bit 31 and sit in the dedup table as a distinct
  node. Construction and template instantiation both mask now, and the
  validator rejects the rest.
- Removing a node input evicted the dedup entry before checking the index, so
  an out-of-range removal left an unchanged node uncached.
- An out-of-bounds write past the Sleigh parse state: `allocateOperand` checked
  none of the fixed sizes `ParserContext::initialize` hands out, so an
  instruction whose parse descended further stored through the next node's
  `resolve` pointer. Thumb-2 `0xEC8x`..`0xECFx` segfaults a fresh engine on
  one `lift_one`. In the vendored submodule.
- `Cfg.isa_mode_conflicts()` and `Cfg.interior_branch_targets()` in Python
  re-read the final CFG, discarding the accumulation `analyze` performs across
  the resolver's rounds, so a conflict raised in an early round and absent from
  the final CFG went unreported on the Python side while the Rust side had it.
- `isa_mode_conflicts` is accumulated across resolver rounds on the Rust side,
  like `interior_branch_targets` already was, so a later round cannot launder an
  earlier clash. The Python accessors above then read that accumulation.
- Merging a `LinkRegister` outcome with a concrete target set dropped that
  successor with no report. It now lands in `unresolved_indirect_branches`.
- A shift count wider than the shifted value was TRUNCATED to the output width
  rather than saturated, so a count of `0x1_0000_0000` on an `I32` output read
  as 0 and the shift silently did nothing. P-code tests the full count against
  `8 * sizeout`. x86 SIMD shift-by-register (`psrad xmm, xmm` and friends) is
  exactly that shape.
- The big-endian ARM VFP register file did not overlay: reversed register blocks
  line up only if they END together, and the 128-byte `s` block was based with
  the 256-byte `d` block, so `s0` landed inside `d16` and a write through `s0`
  was invisible to a read of `d0`. Affects `ARM7_be` and `ARM8_be`. In the
  vendored Sleigh specs.
- Sub-register reads and writes on `arm_be_kernel` used the image's data
  endianness rather than the register file's, so every sub-register access took
  the wrong half. BE8 is byte-swapped instructions over a little-endian register
  block.
- A write into a tracked container wider than 16 bytes was refused because the
  mask had no `u128` to live in, making any function mixing 256-bit and 128-bit
  VEX forms unanalysable. Masks are built limb-wise at the container's width.
- The Sleigh parse cache was not invalidated on an out-of-band context write,
  and not flushed when a context variable was re-pinned, so an address could
  decode against a stale constructor. Both in the vendored submodule.

## 0.1.0

First release.
