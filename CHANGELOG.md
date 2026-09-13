# Changelog

## 0.2.0

Both the Python and the Rust surfaces changed; the two are listed separately.
The shape each API settled into is in
[docs/python-api.md](docs/python-api.md).

### Breaking, Python

Loading and symbols:

- The symbol accessors return a `Symbol` record (`name`, `address`, `size`,
  `end`, `is_function`, `region`, `is_thumb`), so `symbol(name)` is no longer
  an address and `symbol_size` is gone. `functions()` yields `Symbol`s, not
  names, one per address and function symbols only, keeping the alias whose
  size the ELF records. A Thumb function's `address` keeps its ISA bit; `end`,
  `region` and `symbol_at` measure from `address & ~1`. A name resolves to a
  definition, in load order, before an undefined symbol that carries an
  address.
- `add_elf` applies relocations by default (`apply_relocations=True`), as
  `load_elf` does. The flag also selects what is mapped: `True` every
  allocatable section, `False` code and read-only data only. Pass
  `apply_relocations=False` for the old behaviour.
- An `ET_REL` object's sections are laid out from a synthetic image base, not
  address 0, and sections that shared an address are rebased apart, so every
  symbol address in a `.o` moves and address 0 is unmapped.
- Images are memory-mapped. A file changed on disk while it is loaded raises;
  set `STRIDER_NO_MMAP=1` to read the image instead.
- `wide_const_bytes()` returns `bytes`; it returned `list[int]`.

Analysis:

- The three unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, and
  `strider.lift.AliasMode` is gone; `LifterOptions(alias_mode=)`,
  `calls_clobber=` and `assume_distinct_sp_bases_disjoint=` raise `TypeError`.
  `alias_mode` is `stack_global_disjoint` (default `True`), `calls_clobber` is
  `assume_incoming_args_survive_calls` (inverted, default `True`) and
  `assume_distinct_sp_bases_disjoint` is `distinct_sp_bases_disjoint`, beside
  the new `callee_preserves_stack_args`, `noalias_allocators` and
  `escape_analysis`. `AssumptionOptions.none()` clears all six.
- A `Lifter` decodes only on the thread that built it. `analyze`, `build_cfg`,
  `optimize`, `pcode_at`, `reg`, `reg_name`, `call_other_abi`,
  `user_op_names` or a renderer given `lifter=` raise `StriderError` on any
  other thread. The handle moves and drops anywhere; build a second one over
  the same `arch` / `reader()` / `rom()` to work off-thread.
- `CallingConvention.custom(sleigh, ...)` resolves register names against the
  `Sleigh` it is given, so using the result with a `Lifter` of another
  architecture raises instead of analysing against the wrong registers.
- `cc.no_return()` passed as `analyze`'s main `cc` raises; it only means
  something as a `per_address_ccs` override.
- Building a `Cfg` raises when the architecture reports no user-op names,
  where every `CallOther` then classified as returning.
- `Lifter.optimize(function)` raises for a function another handle lifted.
- `Function.rewrite` / `rewrite_all` raise where a replacement reads the value
  it replaces, which installed an uncomputable `x = f(x)`.
- `Node.op()` returns the `ExtendOp` for `Extend`, where it returned `None`.
- `Node` equality and hash include the graph generation, so a handle held
  across an `optimize` no longer equals a fresh one.
- `Node.const_uint` / `const_int` / `const_bool` are `uint` / `sint` /
  `boolean`.

Patterns:

- Pattern and template builders take the `int_` prefix: `add`, `sub`, `mul`,
  `div`, `sdiv`, `rem`, `srem`, `neg`, `shl`, `shr`, `sshr`, `popcount`,
  `lzcount`, `extend`, `truncate`, `sign_extend` and `zero_extend` become
  `int_add` ... `int_zero_extend`.
- The any-operator builders take the `any_` prefix: `int_bin_any` ->
  `any_int_binary`, `int_un_any` -> `any_int_unary`, `float_bin_any` ->
  `any_float_binary`, `float_un_any` -> `any_float_unary`, `bool_bin_any` ->
  `any_bool_binary`, `int_cmp_any` -> `any_int_cmp`, `float_cmp_any` ->
  `any_float_cmp`, `function_arg_any` -> `any_function_arg`.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not. `any_int_const` / `any_float_const` /
  `any_bool_const` and `bool_value` are gone: `int_const()` / `float_const()`
  / `bool_const()` take a `Capture` or no argument, and `any_bool` replaces
  `bool_value`. `I1` is an integer type, so `any_int` covers booleans.
- `int_const_any_of(values)` is gone: `int_const` takes a list.
- `signed_int_const` -> `int_const_any_width`, which also takes a list. It
  matches a constant when the value is a zero or sign extension of the
  constant's bits at the constant's width, so a stored `0x34` no longer
  answers a search for `0x1234`. `strider.template.signed_int_const` is gone;
  `template.int_const` builds the same constant.
- A bare string is no longer a capture operand; use `Capture(name)`.
- `.cap(name)` is gone: `.capture()` takes a `Capture` or a name.
- `call(at=)`, `call().at()` and `.at_any()` -> `call().target()`, which also
  takes a list.
- `switch().address(p)` -> `.selector(p)`: `inputs[1]` is the dispatched value.
- `preceded_by` -> `ctrl` on `SwitchPat`, `RetPat`, `IndirectBranchPat` and
  `UnreachablePat`.
- `LoadPat.mem_in` / `StorePat.mem_in` -> `.mem`.
- `PhiPat.input` / `MemPhiPat.input` -> `.phi_input`, which indexes
  predecessors. `.input` is now the raw-slot method every builder has, so
  `.input(0, p)` reaches the phi token.
- `one_of` reports every arm that matches, not just the first; `first_of` is
  the first-match form.
- `float_is_nan(p)` and `float_le(a, b)` require the operand each repeats to be
  the same value, so they match fewer shapes.
- `find_all(..., ignore_casts_mask=)` is gone: `ignore_casts` takes a bool or a
  `CastMask`.
- A pattern over 256 nodes, counting the nodes of `if_else` branch patterns, or
  nested deeper than the thread's stack allows, raises
  `StriderError` instead of crashing the interpreter.
- The operand-index setters (`CallPat.arg`, `CallOtherPat.arg`,
  `RetPat.ret_val`, `PhiPat.phi_input`, `MemPhiPat.phi_input`, `.input()`)
  reject an index past 1,048,576.
- A `.when()` predicate on a root with no value output is handed the real
  matched node as `Match.root`, not a fabricated `I1`.

Match readers:

- `Match[capture]` returns a `BoundCapture` carrying every reader (`.uint`,
  `.node`, `.op`, ... and their `_opt` forms), where v0.1.0 returned the value.
  `m[off] == 4` and `int(m[off])` still work, but `m[c] is None` is always
  false; ask `c in m`.
- `Match.op`, `.value_type`, `.vn`, `.node` and `.float_bits` raise when the
  capture is absent; the `None`-returning forms are `op_opt`, `node_opt`, ....
  `const_uint` / `const_int` / `const_bool` are `uint` / `sint` / `boolean`.
- `capture in match` raises once the function has been compacted, like every
  other capture accessor.

Rendering:

- `Function.to_dot(style=)` / `to_html(style=)` are gone: `pretty` takes a bool
  or a `DotStyle`. `Cfg` keeps `style=`.
- `Lifter.neighborhood_dot(function, center, ...)` is gone; use
  `Function.neighborhood_dot(center, ..., pretty=True)`.
- `Function.to_dot` / `to_html` quote every attribute value (`shape="box"`).
- `Function.validate()` appends each node's kind and lowest instruction
  address to its error, as in `[node12 Store(..) @ 0x401000]`.

### Breaking, Rust

- The `rsleigh` path dependency is the `externals/rsleigh` git submodule: clone
  with `--recursive`, or run `git submodule update --init --recursive`.
- The MSRV is 1.91.
- The fixture binaries are Git LFS objects; a fresh clone needs
  `git lfs install && git lfs pull`.
- Every pattern builder takes the Python name: the `int_` and `any_` renames
  above (`bit_not` -> `int_not`, `truncate` -> `int_truncate`, ...), `any` ->
  `anything`, `if_node` -> `if_else`, `CallPat::at_any` -> `target` (taking a
  collection), `SwitchPat::address` -> `selector`, `signed_int_const` ->
  `int_const_any_width`, `preceded_by` -> `ctrl`, `mem_in` -> `mem`,
  `PhiPat` / `MemPhiPat::input` -> `phi_input`. Builder types follow
  (`IntBinaryAny` -> `AnyIntBinary`, ...). `bool_value` and `int_const_any_of`
  are gone.
- `MatchPat`, `NodePredicate` and `PostMatchFn` carry `+ Send`; a closure
  capturing an `Rc<Cell<_>>` no longer compiles. `PostMatchFn` takes
  `Option<ValueType>`.
- `TemplatePat` is implemented for `Captured<Var>` alone, so `.capture()` on a
  composite template is a compile error rather than a dropped operand.
- `MemPat` no longer requires `compile_mem`. `WithOutput`'s slot is an
  `Option<usize>`, `None` being `any_output()`.
- The `int`, `bool`, `int_binary_op`, `int_cmp_op` and `bool_binary_op`
  capture kinds of `int_const_with!` / `bool_const_with!` /
  `float_const_with!` are gone.
- `NodeKind::Switch(SwitchTableId)` and `NodeKind::Call { cc: Option<CcId> }`
  carry their case table and override convention. `SideTables::{switch_targets,
  set_switch_targets, set_call_cc, call_other_name, set_call_other_name}` moved
  to `Function`; `set_call_other_name` takes the `user_op_id`.
- `NodeKind::SegmentOp`, `IRBuilderExt::build_segment_op` and the `Seg` / `Off`
  slot roles are gone; a `SegmentOp` or `Indirect` opcode fails the lift by
  name.
- `IndirectBranch` and `Switch` take an optional trailing input, the ISA mode
  their instruction commits (`build_switch_with_mode`,
  `IRViewer::switch_isa_mode`). `Unreachable` takes an optional memory input.
  `build_switch` returns the `NodeId` it created.
- `ValueType` gains `I24`, `I40`, `I56`, `I72`, `I96`, `I112`, `F16` and `F128`.
- `ValidationError` gains `FloatConstUnrepresentableType`, `MemoryRewind`,
  `DanglingInitialVnId`, `InitialVarTypeMismatch` and `LostStore`.
  `FunctionBuilder::build` fails with a `ValidationReport` whose `errors` field
  holds them.
- `strider_ir::dominates` and `control_edge_dominators` are gone;
  `control_dominator_tree` / `control_edge_dominator_tree` return a
  `DominatorTree`, whose `dominates` is O(1).
- `strider_ir` renames the stack side tables for the memory classes they cover:
  `SpDecomp` -> `MemDecomp`, `StackId` -> `MemoryId`, `stack_slot` ->
  `memory_class`, `stack_slot_resolved` -> `memory_slot_resolved`,
  `set_stack_slot_not` -> `set_not_memory`, `clear_stack_slots` ->
  `clear_memory_slots`.
- `Graph::retain_reachable` is `retain_reachable_stale_cache`;
  `Function::retain_reachable` is private. `Graph::update_input` panics on a
  `UseId` its node no longer holds. `Graph::remove_node_input` needs the
  `test-injectors` feature and `FunctionBuilder::build_branch` the `test-util`
  feature.
- `FunctionBuilder::build_call_other_abi` is gone; `strider-lift`'s
  `build_abi_call_other` is the one copy.
- Removed: `Cfg::raw_neighborhood_dot`, `Cfg::region_id_at_start`, the
  `ConstValue` re-export from `strider-ir`, `strider_ir::graph::InputCursor`
  (`strider_graph::InputCursor` remains), `dot::Result`,
  `PostOrder::into_visited`, `MemRegion::fully_covers`, `StackArgs::index_of`,
  `graph_algorithms::walk::entity_preorder` (call `PreOrder::new`),
  `graph_algorithms::walk::VisitTracker` and
  `graph_algorithms::dominance::DefSites`.
- `PreOrder` / `PostOrder` take one type parameter, the graph.
  `PreOrderContext` / `PostOrderContext` and `DenseEntitySet::with_capacity`
  are private. `phi_placement` takes the `HashMap` directly.
  `dominance_frontiers` takes the root, and `DomTree::nodes` must yield each
  node once. `Worklist<E>` requires `E: EntityRef`.
- `dot::DotEmitter::node` / `edge` escape and quote every `extra` value: pass
  `#ffcc00`, not `"\"#ffcc00\""`. `DotStyle`'s fields and
  `DEFAULT_SFDP_NODE_THRESHOLD` are private; build a style with
  `DotStyle::dark()`, `dark_cfg()` or `empty()`. Dot labelling is infallible:
  `pretty_label`, `call_clobbered_name`, `return_ret_name`, `try_declare_node`
  and `emit_input_edge` no longer return `io::Result`.
- `strider_cfg::Builder::for_arch` takes `options: &'a CfgOptions`.
- `CfgOptions` gains public `call_other_overrides` and `data_ranges` fields,
  so a struct literal needs `..Default::default()`.
- `strider_cfg::ResolvedTargets` carries `ResolvedTarget { addr, isa_bit }`, so
  a `known_targets` map needs `ResolvedTarget::from(addr)`.
- `AnalyzeResult` gains `unverified_seeded_sites`, `interior_branch_targets`,
  `isa_mode_conflicts`, `unmapped_branch_targets` and
  `undecodable_branch_targets`; a struct literal must name them.
- `AliasMode`, `MemAliasOptions` and `OptOptions::arg_alias` are gone.
  `OptOptions` is `{ resolve_indirect_branches, assumptions }`, with the
  claims in `AssumptionOptions` as listed under Breaking, Python;
  `AssumptionOptions::default()` keeps two on and `::none()` clears all six.
- `strider_opt::apply_rules_in_order` is gone. `LoadForward` is no longer a
  unit struct: use `LoadForward::default()`.
- `PostOptimizer` has an `Any` supertrait, so an implementor must be
  `'static`.
- `strider_opt::value_range::compute_value_ranges` takes a
  `&DominatorTree<NodeId>` (from `control_dominator_tree`) rather than
  petgraph's `Dominators`.
- `BuiltCallingConvention::try_new` -> `validate(&self)`;
  `BuiltCallingConventionParts` is gone. `validate` compares argument and
  callee-saved registers by byte range, so a callee-saved `d8` against an
  argument `q8` is rejected.
- `CallingConvention::x86_64_all_preserving` is gone on both surfaces; use
  `CallingConvention::x86_64_systemv().preserves_all()`.
- `SleighArch::probe_regs` needs the `test-util` feature.
- The ARM processor-mode `CallOther` rows (`setUserMode`, `setStackMode`, ...)
  apply to the ARM32 presets only, and a mode switch clobbers `r8`-`r12` as
  well as `sp` and `lr`.
- `OwnedElf::file` is gone; use `checked_file`.
  `elf_get_loadable_regions_including_writable`, `elf_load_with_relocations`,
  `elf_load_readonly_with_relocations`, the two sections-only region loaders,
  `apply_elf_relocations` and `apply_elf_relocations_autoload` are gone; use
  `OwnedElf::regions(source, filter, relocate)`. `MemRegion::data` /
  `data_mut` are gone; a region serves bytes through `read`.
- `ElfFileMemReader::from_bytes`, `::from_path` and `::from_elf_relocated` are
  gone, and `elf::relocations` is private. `from_bytes(b)` is
  `object::File::parse(b)` then `::from_object(&obj)`; `from_path(p)` is
  `load_elf(p)` then `::from_elf(&owned)`.

### Added, Python

- `LifterOptions(assumptions=AssumptionOptions(...))` tunes memory precision;
  [docs/python-api.md](docs/python-api.md#2-analyzing-a-function) says what
  each claim buys. `AssumptionOptions.none()` makes none of them.
- `cfg.is_complete()` and six incompleteness channels:
  `unresolved`, `cfg.unverified_seeded_sites()` (answers exactly the caller's
  `known_targets`, and sites consumed as a return or tail call),
  `cfg.isa_mode_conflicts()` (addresses reached in two ISA modes),
  `cfg.interior_branch_targets()` (targets off every instruction boundary,
  including overlapping decodes), `cfg.unmapped_branch_targets()` (direct
  branches to addresses no byte backs) and `cfg.undecodable_branch_targets()`
  (bytes that hold no instruction, reached by a branch or by a fall-through
  past a call). [docs/python-api.md](docs/python-api.md#12-the-cfg-stridercfg)
  describes each.
- A `Return` whose target does not fold to the entry return address
  (`push x; ret`, `mov lr, r1; bx lr`, an overwritten saved link) is reported
  in `unresolved`.
- `CfgOptions(data_ranges=[(start, end), ...])` marks bytes the CFG builder
  never decodes; `ElfLifter.analyze` fills it from ARM and AArch64 `$d`
  mapping symbols unless the caller sets it.
- Indirect branches resolve with a per-target ISA mode, so ARM/Thumb and MIPS16
  targets decode in the right mode. `CfgOptions(known_targets=...)` seats your
  own answers; `LifterOptions(resolve_indirect_branches=False)` turns the
  classifier off.
- Object files (`ET_REL`) analyse: code relocations are applied for x86,
  x86-64, AArch64, ARM and Thumb, PowerPC and MIPS; an undefined or common
  symbol resolves to its own address in an unmapped range past the image; a
  GOT reference loads from a synthetic slot holding the symbol's address. A
  relocation strider does not compute is a hole outside writable mappings:
  reads stop there and the decode error names the relocation type.
- ppc64 ELFv1 function symbols and the entry point resolve through their `.opd`
  descriptors.
- `ElfLifter.add_symbol_file(path)` takes the symbols of a debug companion;
  `ElfLifter.add_symbols({name: addr | (addr, size)}, is_function=)` takes
  symbols from no ELF, such as a `System.map`. `symbol_at(address)`,
  `symbol_opt`, `iter_symbols()`, `endianness` and `is_arm_be8`.
- `Lifter.arch`, `Lifter.reader()` and `Lifter.rom()` (on `ElfLifter`, `arch`
  and `reader()` already existed), and `Cfg.lifter`.
- `BufferReader` accepts `bytes`, `bytearray` or a sequence of ints.
- `Lifter.optimize` takes `opts=` and uses the handle's `rom`.
- `cc.preserves_all()` / `cc.preserves_regs()` calling-convention transforms
  and an `arm_aapcs_soft` preset. A callee-cleanup convention (`ret $imm16`)
  or an i386 PC thunk needs its own `per_address_ccs` entry, built with
  `CallingConvention.custom(..., ret_stack_pop=4 + imm16)` or with the thunk's
  register left out of `callee_saved_regs`.
- `CfgOptions(call_other_abis={name: CallOtherAbi})` reclassifies a Sleigh
  user-op: `CallOtherAbi.noop()` / `pure()` / `mem_clobber()` / `no_return()`,
  or `CallOtherAbi.custom(sleigh, implicit_reads=, implicit_writes=,
  clobbers_memory=, no_return=)`. `Lifter.user_op_names()` lists the user-ops
  an architecture can emit; `Lifter.call_other_abi(name, opts=None)` reads back
  the classification in force.
- `function_arg_float(n)` and `call().arg(n)` reach float arguments, which
  follow the integer ones at a call site.
- A control cycle that never exits (`while (1)`, a spin loop) is anchored at
  lift time, so its stores and their operands stay in the graph.
- Patterns: `first_of`; `one_of` / `first_of` take any pattern as an arm and
  nest in a value, memory or control slot, and an empty list matches nothing.
  `field(base, offset=None)` and `FieldPat`; `code_ptr(x)` (`x` or `x & -2`);
  `PhiPat.input_from(edge, value)`; `IfPat.ctrl(p)`; `load().non_stack()`,
  `store().heap_only()`; `.ordered()` on every binary pattern;
  `find_unique_value`; `JoinPredicate` for relational constraints. A raw int
  stands in for `int_const`, and `int_const` accepts values up to 2^128 - 1.
- `.input(i, p)` / `.any_input(p)` and `.output(slot)` / `.any_output()` on
  every node builder with those slots, declared in the stubs as the
  `runtime_checkable` protocols `NodePat`, `InputPat`, `CtrlPat`, `MemPat`,
  `MemAccessPat`, `OrderedPat` and `OutputPat`. `pattern.CaptureKey` and the
  `IntBinaryOpName`, `BoolBinaryOpName` and `FloatBinaryOpName` aliases.
- `indirect_branch().target(p)` and `switch().selector(p)` take a list.
- `Function.to_text()`: the IR as canonical text, one line per node, stable
  across runs.
- `Function.validate()` also reports a memory read that skips a memory effect
  on its path, an `InitialVar` naming a varnode the function never minted or
  typed against it wrongly, and a `Store` only a `Load` takes (`LostStore`).
- `visualize` opens on the whole graph (`whole=False` for a neighborhood) and
  returns the bound port; `visualize(background=True)` serves on its own
  thread until `strider.explore.shutdown(port)`. The explorer pans by drag and
  arrow keys, zooms with ctrl+wheel or `+` / `-`, fits with `f` and resets with
  `0`; `0` means no limit on its depth, hub-cap and node-count knobs.
- `Cfg.to_dot(style=)`, matching `Cfg.to_html(style=)`, and `lifter=` on
  `Function.to_dot` / `neighborhood_dot` and `Cfg.to_dot` / `neighborhood_dot`.
- `StriderError.backtrace` carries the Rust backtrace; `STRIDER_BACKTRACE=1`
  also puts it in the message.
- `ElfLifter`, `load_elf`, `PatLike`, `ValueTy`, `DotStyle`, `OptimizerPass`
  and other names join their modules' `__all__`.

### Added, Rust

- `Function::to_text(fingerprints)`.
- `Cfg::unverified_seeded_sites` / `isa_mode_conflicts` /
  `interior_branch_targets` / `unmapped_branch_targets` /
  `undecodable_branch_targets` / `undecodable_seeded_targets` /
  `link_register_seated` / `tail_call_seated` / `function_isa_bit` /
  `flowing_isa_bit_at_site` / `space_ids`, and `AnalyzeResult::is_complete`.
- `CfgOptions::data_ranges` (`DataRanges`) and
  `strider_reader::elf::mapping_symbol_data_ranges`.
- `strider_cfg::Builder::with_user_op_names` and `with_transient_defaults`;
  `SleighArch::has_delay_slots` and `internal_registers`;
  `FunctionBuilder::set_internal_registers`.
- `LiftOutcome::return_sites` and `strider_opt::ReturnTargets`.
- `OptimizerPipeline::has_post_pass`.
- `ElfSectionLayout::extern_address`, `OwnedElf::writable_ranges`,
  `elf::without_ranges`, `OwnedElf::checked_file`.
- `graph_algorithms::dominance::dominators` (Lengauer-Tarjan),
  `DominatorTree` and `DominatorTree::span`.
- `NodeKind::input_head_len`, `expected_output_kind` and `is_terminator`.
- `ArchPreset::ALL` and `ArchPreset::arch()`.
- `BuiltCallingConvention::float_arg_slots`.
- `strider_ir::node::low_bits_mask_u128`.
- `[profile.release]` sets `overflow-checks = true`, so the shipped wheel
  traps on arithmetic overflow.

### Changed

- Asm fingerprints no longer include instructions whose only contribution was
  dead code, such as a flag computation a rewrite discarded.
- `ConstantFold` factors a sum of scaled copies of one value into one multiply
  for every mul / shift pairing: `x*3 + (x << 2)`, `(x << 2) + (x << 3)` and
  `x*3 - x` all reach a query as `x * C`.
- `PhiCollapse` folds a cycle of phis whose inputs from outside the cycle are
  one value.
- The optimizer skips a pass whose last run changed nothing until the graph
  changes.
- `validate` rejects IR it used to accept: a non-phi node that is its own
  transitive producer, and the memory, `InitialVar` and `LostStore` checks
  above.

### Performance

- A pattern query no longer doubles per nested failing branch pattern (a
  40-`If` graph with no match: 14.6 s to 0.001 s), and a commutative node whose
  two inputs are one value tries one operand order.
- `LoadForward`, `FunctionArgDetect` and `CallStackArgCollect` answer
  nearest-clobber queries by climbing a memory dominator tree instead of
  walking the memory graph per query (debug build: `LoadForward` over 1600
  call diamonds 7.9 s to 0.10 s).
- Dominators come from Lengauer-Tarjan rather than petgraph's quadratic
  iterative scheme; join `dominates` constraints, `phi_input_from_edge` and
  value-range guard lookups answer from dominator-tree intervals.
- Jump tables that are only reachable through each other's arms are seated from
  the code in front of them between full resolution rounds (debug build,
  x86-64 `f_fullswitch_64`: 17.7 s to about 4 s).
- `Function::validate` is linear (192k nodes: 2.1 s to 232 ms), and so is
  draining a `Worklist` and dead-branch elimination over chained constant
  branches.
- Region lookup is O(log n) for full and partial reads.
- Loading maps the image and patches relocations on read, and a symbol resolves
  through an index, so opening a kernel no longer walks every relocation and
  symbol. `symbol_at` is one index query (40k labels: 8.3 ms to 0.002 ms).
- `add_elf` checks overlap in O(n log n).
- `BufferReader` copies `bytes` and `bytearray` in one piece (64 MiB: 3.2 s to
  41 ms).
- `pcode_at` decodes through one cached engine instead of cloning `Sleigh` per
  call.
- Reading `CfgOptions.known_targets` or `.call_other_abis` returns a cached
  read-only `mappingproxy` instead of copying the table (`analyze` with 20,000
  known targets: 32.0 ms to 1.6 ms).
- A CFG build reuses the `Lifter`'s Sleigh user-op table instead of fetching it
  over FFI, and `Function.to_dot(pretty=True)` resolves the register table once
  per render.
- `UnionDag::for_each` costs the size of the answer, not of the arena, so
  reading `asm_fingerprint` over a large function is linear.

### Fixed

Lifting and memory:

- A p-code `LOAD` / `STORE` in the REGISTER space reads or writes the register
  its folded address names, with a phi placed for it; it was lifted as opaque
  memory and the value vanished (ARM `vld1.N {dX[i]}`). One that does not fold
  (`VLD2/3/4`, `VST2/3/4`) is an opaque access that mirrors every tracked
  register into its slot before and re-reads each after, where it failed the
  function.
- A `LOAD` from the constant space lifts as the constant its address encodes,
  so PowerPC `rlwimi` / `rldimi` / `rldic` / `rldcr` masks fold.
- `CBRANCH` lowers as `cond != 0`, not the condition's low bit.
- A shift count at or past the output width gives 0 (the sign fill for an
  arithmetic shift right), in the lift and in `ConstantFold`, where the lift
  truncated the count (x86 `psrad xmm, xmm`) and the fold clamped `IntLeft` at
  127 and masked `IntRight`'s input after shifting.
- `popcount` / `lzcount` count over the operand width, where `clz` on a 32-bit
  operand of a 64-bit register read 32 too high on AArch64 and PPC64.
- Sign-extending a constant into `I256` / `I512` fills the upper half.
- A write into a tracked container wider than 16 bytes lifts; a p-code
  temporary of a width the IR did not type (x86 `adcx` / `adox` / `lsl`,
  AArch64 fixed-point `ucvtf`) and a CONST branch-target varnode of width other
  than 1, 2, 4 or 8 no longer fail the function.
- A `Copy` whose input and output widths disagree fails the lift instead of
  truncating or zero-extending silently.
- Sub-register reads and writes on `arm_be_kernel` use the register file's
  little-endian layout, not the image's data endianness, and `load_elf` picks
  `arm_be_kernel` for an ARM BE8 image.
- A call through an x86 trap vector (`int imm8`, `into`) pops no return address,
  where SP drifted by 4 or 8 after every syscall.
- A `CallOther` whose output is a memory operand (x86 `sgdt [mem]`) lifts.
- Unclassified user-ops that failed the lift are classified: AArch64 `svc`,
  ARM32 NEON float user-ops, `cpsid a` and its siblings, MIPS `syscall`. ARM32
  `smc` / `hvc` carry the SMCCC register footprint; `software_udf` does not
  return.
- A user-op is opaque to the memory walk whatever the function's convention
  says, where `preserves_memory` let a store forward across a `syscall`.
- A PowerPC `tw` / `twi` / `td` / `tdi` whose mask covers a whole signed or
  unsigned trichotomy ends its region as no-return; a MIPS `BUG()` trap ends
  its function's decode.
- A MIPS o32 `double` return is the 8-byte `f0_1` pair, not its low half.
- PowerPC `cr2`-`cr4`, `r2`, `r13` and `v20`-`v31` are callee-saved, and the
  PPC64 stack-argument base is 112 bytes on ELFv1 and 96 on ELFv2, where every
  PPC64 stack argument was read at the wrong offset.
- A callee-saved `d8`-`d15` preserves 64 bits, not the whole `q` register.
- A `per_address_ccs` override naming an integer argument register outside the
  function's own convention lifts.
- An ARM32 function with a NEON register and a call analyses.
- PowerPC `frin` folds half away from zero, as p-code's `FLOAT_ROUND` is
  defined.
- A float value range is top, not the singleton `{0}`.
- A `FloatConst` is masked to its width on construction and instantiation, and
  `validate` rejects stray upper bits.
- An SSA read of a never-written variable errors instead of returning `Entry`'s
  control edge.

Optimizer and analysis:

- A value tested for non-zero is no longer read as bit 0 of itself, which
  deleted packed comparisons above the tested bit.
- Memory SSA no longer memoises a path-relative cycle verdict, which let a
  sibling branch narrow a load onto a phi of one arm.
- `LoadForward` and `FunctionArgDetect` rewire load memory edges only on
  structural facts, so re-optimising under `AssumptionOptions.none()` inherits
  no assumption. `LoadForward` reports a change when it narrows without
  forwarding, where the final drain could leave a `Phi[x, x]`.
- An alignment mask anchors a stack base only when its 1-run reaches the top of
  the address width, so `sp & 0x10` or `sp & 0x8000_0000` no longer roots a
  stack base that `stack_global_disjoint` calls disjoint from a global.
- A call's collected stack arguments stop at a slot a nearer store partly
  overwrote.
- `CallStackArgCollect` and `FunctionArgDetect` replace what an earlier run
  wrote, where re-optimising a function grew each call's argument list.
- `PhiCollapse` reports the node it killed.
- The optimizer's iteration cap drains and validates before reporting.
- Constant-folding a branch no longer strands a loop with no path to a
  terminator.
- `Function.rewrite` / `rewrite_all` refill the memory-decomposition side
  table, so `load().stack_only()` answers correctly after a rewrite.
- `Lifter::build_ir` refuses a `Cfg` another `Lifter` built: its p-code named
  address spaces by pointer into the engine that decoded it, a use-after-free
  once that engine was dropped.
- `rewrite_all` and `apply_rules_count` stop at the first rule that fires at a
  node.
- Under `escape_analysis`, a frame address held in any register a call
  clobbers escapes the frame (i386 `regparm`, clang fastcc, the GNU C static
  chain, AArch64 `x8`); a spill is no longer forwarded across a callee that
  writes it. Sleigh's internal registers (`SleighArch::internal_registers`)
  are not callee-visible and stay out.

Indirect branches and the CFG:

- A conditional's arms are told apart by where its successors start, where an
  `fn_max_size` bound cutting an instruction could swap them.
- A guard on a sub-register bounds the index scaled out of it, and a guard
  bounds a table index through a mask of bits known to be zero (PowerPC
  `slwi` / `sldi`), so tables stop seating slots past their end.
- A table index under a constant offset that wraps below zero defers the site
  instead of seating dead slots.
- A masked switch index bounded on a loop back edge resolves on x86-64.
- A union of two single-element ranges keeps their gap as its stride.
- A jump-table entry is no longer enumerated as the table index.
- A site that loses a successor, including a `LinkRegister` answer merged with
  concrete targets, is reported in `unresolved`, and a resolved target that is
  not code is abandoned and reported rather than failing `analyze`.
- A direct branch to an address with no bytes, or to bytes that hold no
  instruction, costs the edge, not the function; so does a fall-through past a
  call into such bytes, and bytes that do not decode behind a jump-table arm
  drop that arm.
- A decode that runs off the end of the image mid-instruction ends the region.
- A branch into a region's last instruction, and a region split, keep every
  edge: the build aborted, decoded an overlapping region or dropped a
  successor.
- `Cfg.region_at` finds an address inside a region's last instruction.
- A reused engine decodes a function the same as a fresh one: ARM's
  `mov lr, pc` state and MIPS's `PAIR_INSTRUCTION_FLAG` and MIPS16e
  `ext_delay` no longer carry into the next decode.
- An `analyze` given a post-pass named `IndirectBranchClassify` still runs the
  real classifier.
- Three inputs that crashed `build_cfg` raise `StriderError`: a malformed
  instruction in a MIPS delay slot, an AArch64 operand with a null address
  space, and a region at the top of the address space.

Patterns:

- A commutative node with one pinned operand tries both orders, and a capture
  on an operand's sibling output keeps its binding.
- An `if_else` branch sub-pattern backtracks into its later bindings, and its
  captures are visible to join constraints; replacing a branch drops the old
  branch's captures.
- An alternation used as a whole pattern binds its capture, a root-level
  `one_of` or bare `call()` matches a `Call` with no value output, and
  `one_of![p]` no longer binds control and memory edges as values.
- `Not` over `phi_input_from_edge` drops the rows the constraint cannot answer.
- A second `.capture()` on one vertex binds both names.
- A `Match` used after `Function.compact` reports stale instead of reading
  another node.
- `int_not` refuses an output wider than 128 bits, where it matched `Xor`
  against a saturated mask.
- A guard in a `ctrl()` slot, a guard on a control-rooted pattern,
  `.ordered()` on an alternation, a multi-sink branch pattern and a `Template`
  with a gapped output slot are refused at build time.
- A template refuses an ill-typed operand and output kinds that contradict the
  node signature, and a comparison operand built fresh takes its operand's
  width, not `I1`.
- Deep patterns, `JoinConstraint` trees and `if_else` towers drop without
  recursing, where dropping one killed the interpreter.
- An exception inside a `.when()` predicate surfaces from its own query, not a
  later one; a `.when()` predicate nested into another pattern is
  garbage-collected correctly, where it leaked a reference cycle or segfaulted;
  an `__index__` that re-enters the builder raises.
- A query that raises before doing any work, or fails partway through a
  multi-pattern call, leaves its one-shot patterns usable.
- `Function.rewrite` with a float-typed root and an integer literal declines
  rather than raising `PanicException`.
- `Cfg.fingerprint_pcode` rejects a `Node` from another binary's function.

Loading:

- The read-only view leaves out every address a writable mapping covers, in one
  image and across `add_elf`, so `LoadReadOnly` no longer folds a stale file
  byte.
- A relocation field straddling a region end is patched in every region
  covering it, and equal-start `PT_LOAD`s dedup widest-first.
- Object files: relocations resolve against `.symtab`; sections sharing an
  address, `SHF_TLS` sections and sections whose extent overflows the address
  space no longer serve each other's bytes; `functions()` / `symbols()` list an
  object's symbols, and a library's `.dynsym` exports.
- `R_MIPS_16` patches its four-byte storage unit, mips64el relocation kinds are
  read correctly, and a relocation against an out-of-range `st_shndx` is
  skipped.
- An image with many overlapping `PT_LOAD`s is refused past 4x relocation
  amplification instead of exhausting memory.
- A mapping ending at 2^64 loses its last byte instead of failing the load.
- `load_elf` no longer aborts on a guarded parse or hangs on a FIFO.

Python and the explorer:

- `analyze` releases the GIL on a custom pipeline too, and `OptimizerPipeline`
  is usable from any thread and survives being applied.
- A `Function` or `Cfg` moved to another thread no longer leaks its `Lifter`,
  and an off-thread render raises `StriderError` rather than
  `PanicException`.
- A `MemReader` callback that panics raises `StriderError`, and one that
  re-enters `analyze` no longer aborts the process.
- `Cfg.neighborhood_dot` raises on an out-of-range region id.
- DOT, HTML and p-code text are deterministic, and a backslash in a symbol name
  is escaped.
- `visualize(depth=-1)` raises `ValueError`.
- The explorer no longer `eval`s its query string, refuses a request whose
  `Host` is not loopback, and a client that stalls, trickles or stops reading
  neither holds it nor blocks interpreter exit; `explore.shutdown` waits under a
  deadline and returns the ports it stopped.
- The vendored `svg-pan-zoom` bundle carries its BSD-2-Clause notice.

### Fixed, decoder and sla

The vendored Sleigh engine and specs under `externals/rsleigh`, one line per
mnemonic group. A caller sees these only in the lift of the named instructions.

- AArch64 `ldrsb` / `ldrsh` into a W register (every addressing form,
  `ldursb` and `ldtrsb` included) sign-extend to 32 bits and clear the upper
  half.
- AArch64 scalar SIMD writes (`fmadd`, `uaddlv`, `umaxv`, `fmaxv` and about 170
  more) zero the rest of the vector register, as do `addv` into a byte
  destination and `frint32x` / `frint32z` / `frint64x` / `frint64z`.
- AArch64 `usdot` / `sudot` / `bfdot` by element read the right lane, and
  `sudot` decodes.
- AArch64 `FRINT{A,I,M,N,P,X,Z}` lift through an opaque `NEON_frint` user-op
  instead of as a float-to-integer conversion.
- Thumb-2 `ands`, `bics`, `movs` and `mvns` set C from the shifter or the
  expanded immediate.
- ARM NEON `vmov.<dt> Dn[x], Rt` writes the element into its lane.
- The big-endian ARM VFP register file overlays `s` and `d` registers
  (`ARM7_be`, `ARM8_be`).
- ARM `vcmp` sets FPSCR's C when unordered.
- Thumb `sev` / `sev.w` raise `SendEvent`; A32 `csdb` no longer decodes as an
  `msr`.
- A Thumb-2 `0xEC8x`..`0xECFx` instruction no longer writes out of bounds in
  the parser.
- PowerPC `lhbrx` / `lwbrx` reverse the byte order on little-endian too, and
  32-bit PowerPC decodes `popcntw`.
- MIPS64 `clz` / `clo` count the word, `drotrv` rotates by the right amount,
  and `dsllv` / `dsrlv` / `dsrav` take six count bits.
- x86 `PSLLD` / `PSLLQ` / `PSRAD` read one count from `SRC[63:0]`, and
  `psrlw` / `psrld` read a memory count once.
- x86-64 32-bit destinations clear the upper half: `rdtsc`, `rdpmc`,
  `xgetbv`, `RDPKRU`, `LOOP` / `LOOPcc` / `JECXZ` under an address-size
  override, the `fsgsbase` and `rdpid` reads, `RDRAND`, `RDSEED`, `CMPXCHG8B`,
  30 VEX and EVEX constructors, and string instructions under a 0x67 prefix
  (RSI, RDI and the `rep` count).
- x86-64 `rdfsbase`, `rdgsbase`, `rdpid`, `wrfsbase`, `wrgsbase`, `incssp` and
  `rdssp` honour `REX.B`; `rdsspd` decodes, and `incssp` no longer truncates
  its count to a byte.
- x86-64 `VMREAD` / `VMWRITE` take 64-bit operands in long mode, and `GETSEC`
  clears the upper halves of its results.
- A decode whose own context commit changes a value, an out-of-band context
  write, or a re-pinned context variable flushes the parse cache.
- An out-of-bounds operand in `setOutOfBandState`, and a memory reader
  overstating its read, are errors rather than a panic across the FFI
  boundary; a region at the top of the address space no longer overflows the
  read callback.

## 0.1.0

First release.
