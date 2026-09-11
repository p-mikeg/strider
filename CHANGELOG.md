# Changelog

## 0.2.0

Both the Python and the Rust surfaces changed; the two are listed separately.
The shape each API settled into is in
[docs/python-api.md](docs/python-api.md).

### Breaking, Python

- `add_elf` applies relocations by default (`apply_relocations=True`), as
  `load_elf` already did. The flag also selects what is mapped -- `True` every
  allocatable section, `False` code and read-only data only -- so the two
  defaults disagreeing served a different image from the same file. Pass
  `apply_relocations=False` for the old behaviour.

- A pattern over 256 nodes, or a `one_of` nested more than 256 levels deep, is
  refused with a catchable `StriderError` when the query runs. The matcher is
  continuation-passing -- a node's later operands match inside the deepest
  frame of its earlier operands' subtrees -- so stack depth tracks pattern
  NODES, not pattern depth, and nothing bounded it: measured in debug, a
  551-node chain and a 1023-node balanced tree both overflowed the stack and
  aborted the interpreter, and nested `one_of` overflowed at lowering time.
  Split a larger pattern and match the parts separately.

- The operand-index setters reject an index past 1,048,576: `CallPat.arg`,
  `CallOtherPat.arg`, `RetPat.ret_val`, `PhiPat.phi_input`,
  `MemPhiPat.phi_input`, and the generic `.input()`. They passed a `usize`
  straight into `head_len + idx`. In debug that panicked as a `PanicException`;
  the shipped wheel had no `[profile.release]`, so overflow checks were off and
  the index wrapped into the `sp` slot with no error at all. `.input()` also
  rejects the value that aliased the any-input sentinel.

- A `Lifter` decodes only on the thread that built it. Calling `analyze`,
  `build_cfg`, `optimize`, `pcode_at`, `reg`, `reg_name`, `call_other_abi`,
  `user_op_names`, or a renderer taking `lifter=`, from another thread raises a
  catchable `StriderError` instead of corrupting Sleigh's decoder state. The
  handle itself moves and drops anywhere; build a second one over the same
  `arch` / `reader()` / `rom()` to work off-thread. `BufferReader` and a loaded
  ELF became `Arc<Mutex<_>>` to make that safe.

- `CallingConvention.custom(sleigh, ..)` resolves register names against the
  `Sleigh` it is given and freezes the varnodes, so using one with a `Lifter`
  of another architecture now raises. It used to analyse silently against the
  wrong varnodes: an x86-64 function under a convention built from a 32-bit
  `Sleigh` simply had no arguments.
- `cc.no_return()` passed as `analyze`'s main `cc` raises. It was
  silently dropped there and only ever meant anything as a `per_address_ccs`
  override.
- `capture in match` raises once the function has been compacted, like every
  other capture accessor. `__contains__` answered out of the stale arena, where
  every reader beside it already checked the graph generation.
- `Match[capture]` returns a `BoundCapture` carrying every reader (`.uint`,
  `.node`, `.op`, ... and their `_opt` forms), where v0.1.0 returned the value
  itself: a bool, else an int, else raw float bits, else `None`. Numeric use is
  unchanged (`m[off] == 4`, `int(m[off])`), but `m[c] is None` no longer tests
  for an unbound capture: a `BoundCapture` is never `None`, so the check
  silently inverts. Ask `c in m` instead, or read `m[c].uint_opt` for a value
  that may not be a constant.
- `float_is_nan(p)` and `float_le(a, b)` require the operand each repeats to be
  the SAME value, so they match strictly fewer shapes. `float_is_nan` previously
  matched every lowered `float_ne`.
- A `.when()` predicate whose matched root is a control or memory edge, or a
  node with no value output, is handed the real matched node as `Match.root`,
  where it used to be handed a fabricated `I1`. The match itself still stands
  or falls on what the predicate returns.

- `Match.op`, `.value_type`, `.vn`, `.node` and `.float_bits` return a value and
  RAISE when the capture is absent, where v0.1.0 returned `None`. The
  `None`-returning forms keep the old behaviour under `_opt` names (`op_opt`,
  `node_opt`, ...). An `if m.op(c) is None:` check now raises instead of taking
  the branch. `const_uint` / `const_int` / `const_bool` are `uint` / `sint` /
  `boolean`.

- Pattern builders renamed `add` -> `int_add` to follow the convention; const
  readers shortened. The settled vocabulary is in
  [docs/python-api.md](docs/python-api.md#4-patterns).
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
  `end`, `is_function`, `region`), so `symbol(name)` is no longer an address
  and `symbol_size` is gone. `functions()` yields `Symbol`s where v0.1.0
  yielded name strings, and yields function symbols only, one per address: a
  data symbol, or an alias of an address already listed, is dropped, keeping
  the one whose size the ELF records. `size` is `None` when the ELF records no
  extent, and such a symbol is still yielded.
- An `ET_REL` symbol's address changes: sections that shared one are rebased
  apart.
- `wide_const_bytes()` returns `bytes`; it returned `list[int]`.
- `Node` equality and hash include the graph generation, so a handle held across
  an `optimize` no longer compares equal to a fresh one.
- The three unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, and
  `strider.lift.AliasMode` is gone with them.
  `alias_mode="stack_global_disjoint" | "strict"` was one boolean claim wearing
  an enum and is `stack_global_disjoint`, defaulting `True`; `calls_clobber` is
  `assume_incoming_args_survive_calls`, inverted and defaulting `True`, which
  reaches which loads count as incoming arguments and nothing else (a
  memory-clobbering `CallOther` blocks whatever it says); and
  `assume_distinct_sp_bases_disjoint` is `distinct_sp_bases_disjoint`, the one
  name that sheds its `assume_` prefix, which
  `assume_incoming_args_survive_calls` keeps. They join the new
  `callee_preserves_stack_args`, `noalias_allocators` and `escape_analysis`,
  and `AssumptionOptions.none()` clears all six, which no single knob promised
  before. [docs/python-api.md](docs/python-api.md#2-analyzing-a-function) says
  what each one buys.
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
- `LoadPat.mem_in` / `StorePat.mem_in` -> `.mem`, the name `call()`,
  `call_other()` and `indirect_branch()` already give that slot. It is the
  node's memory predecessor either way, so `load` and `store` join the
  `MemPat` mixin.
- `PhiPat.input` / `MemPhiPat.input` -> `.phi_input`, which indexes
  predecessors (raw slot `idx + 1`) the way they always did. `.input` is now
  the raw-slot method every other builder's `input` is, so `.input(0, p)`
  reaches the phi token rather than predecessor 0's value.

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

- An `ET_REL` object's sections seat at a synthetic image base rather than at
  address 0, so every symbol address in an unlinked `.o` moves and address 0 is
  unmapped. A `.text` seated at 0 was served as read-only memory: a null
  dereference, and every relocation site left at its file-initial zero, folded
  to instruction bytes instead of failing to fold.
- `Cfg.is_complete()` answers `False` for a `build_cfg` CFG that holds an
  unresolved indirect branch. It read three of the four channels, and the
  fourth reached only `AnalyzeResult`, so the method both guides present as the
  completeness test asserted the opposite of the truth on the one CFG that
  cannot consult it.
- `Lifter.optimize(function)` raises when the function was lifted by another
  handle. It folded that function's constant-address loads against THIS
  handle's rom, so a foreign binary's bytes arrived as constants; every
  renderer already compared architectures.
- A root-level pattern matches a `Call` that produces no value output --
  `one_of([...])`, `first_of([...])` and a bare `call()` alike. The root
  lowering anchored on a value, so a call built from a no-clobber convention
  was invisible to every one of them.
- `float_is_nan`'s sibling `int_not` is refused when the output is wider than
  128 bits, where it used to match an `Xor` against a saturated mask that is
  not a complement, and miss the real one.
- A `.when_match()` guard in a `ctrl()` slot is refused at build time, as the
  same guard already was elsewhere. It was accepted and could never fire, so
  the query answered nothing with no error.
- A `Template` declaring a gapped output slot is refused, as the input side
  already refused the same gap; it used to densify silently.
- `CallOtherAbi.__eq__` and `__hash__` include the architecture the ABI was
  frozen against, so two `custom(...)` ABIs from different arches no longer
  compare equal or collide in a dict while being mutually unusable.

### Breaking, Rust

- `TemplatePat` is implemented for `Captured<Var>` alone, so `.capture()` on a
  composite template is a compile error. The blanket impl let
  `int_add(var(a), var(b)).capture(c)` compile and silently discard the `Add`
  and both operands, leaving an RHS that replaced the match with `c`'s binding.
- `Graph::retain_reachable` is `retain_reachable_stale_cache`: rebuilding the
  node cache is the caller's, which the old name did not say.

- `OwnedElf::file` is gone and `is_arm_be8` returns `Result<bool>`. Both parse
  the mapping, and a file rebuilt under a live handle makes that a SIGBUS no
  caller can catch, which neither a `File` return nor a `bool` had any way to
  report. `checked_file` is the way in.
- The `rsleigh` path dependency moved from a sibling `../rsleigh` checkout to
  the `externals/rsleigh` git submodule: clone with `--recursive`, or
  `git submodule update --init --recursive`.
- The MSRV is 1.91; edition 2024's own floor of 1.85 no longer builds the
  workspace.
- The fixture binaries are Git LFS objects (`fixtures/out/**/*.o` and
  `fixtures/out/**/*.elf`), so a fresh clone needs
  `git lfs install && git lfs pull`. Without them the `ET_REL` tests read
  pointer text where an ELF should be, and panic.

- `MatchPat`, `NodePredicate` and `PostMatchFn` carry `+ Send`, so a compiled
  `Pattern` moves between threads with the value that owns it. A `.filter()` or
  `.when_match()` closure capturing an `Rc<Cell<_>>` no longer compiles;
  capture an `Arc<AtomicUsize>`.
- `float_is_nan` / `float_le` pin the operand they repeat to one value, so they
  match strictly fewer shapes, and `PostMatchFn` takes `Option<ValueType>`, so
  a guard on a root with no value output fails rather than seeing a fabricated
  `I1`.
- Removed with no consumer: `Cfg::raw_neighborhood_dot`, the `ConstValue`
  re-export from `strider-ir`, `dot::Result`, `PostOrder::into_visited`,
  `DenseEntitySet::clear` and `MemRegion::fully_covers`.
- `elf_get_loadable_regions_including_writable` is gone; use `OwnedElf::regions`
  with a `LoadFilter`.
- `NodeKind` gains `input_head_len` and `expected_output_kind`, so a consumer
  outside `strider-ir` can read the slot-layout single source of truth instead
  of hardcoding the shift. `strider-pattern`'s `call().arg(n)`, `ret_val(n)` and
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

- `MemRegion::data` / `data_mut` are gone; a region serves bytes through `read`,
  which applies relocation patches.
- `elf_load_with_relocations`, `elf_load_readonly_with_relocations` and the two
  sections-only region loaders are gone; use `OwnedElf::regions`.
  `apply_elf_relocations` and `apply_elf_relocations_autoload` are gone with
  them, relocations being a `regions(.., relocate)` argument.
- `ElfFileMemReader::from_bytes`, `::from_path` and `::from_elf_relocated` are
  gone, and so is `elf::apply_elf_relocations` (`elf::relocations` is no longer
  a public module). `from_bytes(b)` is `object::File::parse(b)` then
  `::from_object(&obj)`; `from_path(p)` is `load_elf(p)` then
  `::from_elf(&owned)`; both `from_elf_relocated` and a load-then-apply pair are
  `OwnedElf::regions(source, filter, /* relocate */ true)`, the path that
  windows into the ELF's own bytes instead of copying them.
- `graph_algorithms::walk::entity_preorder` is gone; call `PreOrder::new`, which
  it only forwarded to. `entity_postorder` stays.
- A direct branch to an address the image has no bytes for no longer fails the
  whole function. The edge is seated as an empty tail-call stub and reported on
  the new `Cfg::unmapped_branch_targets` / `AnalyzeResult::unmapped_branch_targets`,
  bound in Python as `cfg.unmapped_branch_targets()`. That is a FIFTH
  incompleteness channel, and `is_complete()` now folds five. An unmapped ENTRY
  is still an error.
- `Cfg::region_id_at_start` is gone.
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
- The pattern renames under Breaking, Python land on the Rust builders under
  the same names: `CallPat::at_any` -> `target` (taking a collection),
  `SwitchPat::address` -> `selector`, `signed_int_const` ->
  `int_const_any_width`, `preceded_by` -> `ctrl`, `LoadPat` / `StorePat::mem_in`
  -> `mem`, and `PhiPat` / `MemPhiPat::input` -> `phi_input`, with `input` now
  the unshifted raw slot. `bool_value` goes the same way: `any_bool` is it, and
  `I1` being an integer type, `any_int` covers booleans too.
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
- `AliasMode` and `MemAliasOptions` are gone, and every claim the analysis
  cannot check gathers in `OptOptions::assumptions`, an `AssumptionOptions`:
  `OptOptions::alias_mode` becomes `stack_global_disjoint`,
  `MemAliasOptions::calls_clobber` becomes
  `assume_incoming_args_survive_calls` (inverted, and defaulting `true` where
  the derived `Default` had it `false`), and
  `MemAliasOptions::assume_distinct_sp_bases_disjoint` becomes
  `distinct_sp_bases_disjoint`, joining the new `callee_preserves_stack_args`,
  `noalias_allocators` and `escape_analysis`. `OptOptions::arg_alias` is gone
  with them, leaving `OptOptions` as
  `{ resolve_indirect_branches, assumptions }`, `resolve_indirect_branches`
  being new. `AssumptionOptions`'s `Default` is hand-written rather than
  derived, so `default()` keeps two on and `AssumptionOptions::none()` clears
  all six.
- `WithOutput`'s slot is an `Option<usize>`, `None` being the existential
  `any_output()`.

- `FlowVars::reset_at` and `FlowVars::restore_at` are one `pin_at(..)`
  returning `Result<()>`. They had identical bodies, and the `bool` both
  production callers discarded is gone.
- `FunctionBuilder::build_call_other_abi` is removed. It re-implemented
  `strider-lift`'s `build_abi_call_other`; that is the only copy now.
- `FunctionBuilder::build_branch` takes the `test-util` gate its siblings
  carry, and `Function::retain_reachable` is private -- neither had a
  production caller. `Graph` grows a stale-cache entry point for the one
  caller that re-keys the dedup cache afterwards.
- Dot labelling is infallible: resolving the register table once per render
  rather than once per varnode removed the failure it fed, so `pretty_label`,
  `call_clobbered_name`, `return_ret_name`, `try_declare_node` and
  `emit_input_edge` no longer return `io::Result`.
- `MemOptions::call_blocking` takes the `noalias_allocators` set as a required
  parameter and `with_noalias_allocators` is gone. The decomposition memo is
  keyed by `ValueId` alone, so two analyzers over one `Function` must decompose
  against the same set or one answers the other's question.
- `Region`'s variable map holds `PackedOption<ValueId>` and an unrenamed read
  errors. `ValueId` derives `Default`, so a slot never written read back as
  `Entry`'s control edge: a real value dressed as an SSA variable.
- A `SegmentOp` or `Indirect` opcode fails the lift by name. Neither can come
  from `lift_one` -- `SegmentOp`'s only producer is a decompiler action whose
  first input is a host pointer. The dispatch is exhaustive now, so a new
  rsleigh opcode is a compile error rather than a runtime message.
- `Builder::enqueue_resolved` takes the dispatch's `PcodeInsnAddr` rather than
  a bare address, and `Cfg` carries the flowing ISA bit each indirect site was
  sampled at.
- An ARM processor-mode switch clobbers `r8`-`r12` as well as `sp` and `lr`.
  FIQ banks R8-R14 and the sla models no banking, so this table is the whole
  model; it widens a clobber set, so ARM32 results where a mode switch appears
  are less precise, which is the sound direction.
- `SleighArch::transient_decode_vars` returns `PAIR_INSTRUCTION_FLAG` on the
  four MIPS presets. It is `noflow`, selects a constructor, and is
  forward-painted, so an unset one decoded the next function's entry against
  the paired form on a reused engine.

### Added

- The root `Cargo.toml` declares `[profile.release]` with
  `overflow-checks = true`, so an arithmetic slip in the shipped wheel traps
  instead of wrapping into a silently wrong slot or address.
  `debug-assertions` stays off: the debug and `cargo test --release` runs
  already gate on those.

- A converged CFG reports incompleteness through four channels on
  `AnalyzeResult`, not one: `unresolved_indirect_branches`,
  `unverified_seeded_sites`, `isa_mode_conflicts` and
  `interior_branch_targets`. A consumer asking whether a result may be
  incomplete reads all four;
  [docs/python-api.md](docs/python-api.md#12-the-cfg-stridercfg) says what each
  one carries.
- `Cfg.isa_mode_conflicts()` (Rust: `AnalyzeResult::isa_mode_conflicts`):
  addresses reached carrying two different ISA modes, where one region owns the
  bytes and the losing path's arm is not the stream it believes.
- `Cfg.unverified_seeded_sites()` (Rust: the
  `AnalyzeResult.unverified_seeded_sites` field): dispatch addresses nothing
  verified, whose answer is exactly the caller's `known_targets` and nothing
  the classifier derived, plus every site the CFG consumed outright as a return
  or a tail call, seeded or derived. A `"return"` or single-out-of-function
  seed is consumed at CFG-build time, leaving no placeholder to report, so it
  is named here too.
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
  mixin, but not `entry()`, whose `Entry` is `inputs: []`, nor the four operand
  builders `int_binary` / `float_binary` / `bool_binary` / `function_arg`.
  `input` addresses raw slots, whose numbering is per kind; the IR's
  `expected_signature` is the source of truth.
- `output(slot)` and `any_output()` reach every node builder carrying the
  output mixin, but not the sinks `ret()`, `indirect_branch()` and
  `unreachable()`, all `outputs: []`, nor the same four operand builders. Both
  return `OutputSlotPat`, the terminal `CallPat.output` already returned;
  `CallOutputPat` still names it.
- `indirect_branch().target(p)` and `switch().selector(p)` take a list of
  candidates, like `call().target(p)`; an empty list matches nothing.

- `ElfFileMemReader` serves the file-initial bytes whichever constructor built
  it, which the type doc now says.
- `StriderError.backtrace` carries the Rust backtrace, captured by default;
  `STRIDER_BACKTRACE=1` folds it into the message too.
- `Lifter.optimize` takes `opts=` and threads the handle's `rom`, so a
  hand-built pipeline sees the same read-only image `analyze` does.
- `CallOtherOverrides::new` takes `(String, CallOtherOverride)` entries, where
  `CallOtherOverride` is either a `CallOtherClass` or a caller-resolved
  `BuiltCallOtherAbi`, so an override can carry an implicit register footprint,
  and it rejects a duplicate user-op name rather than silently keeping the
  first, which `get` would have shadowed. `classify_with` returns a
  `CallOtherLookup`, whose `built(&regs)` yields the resolved footprint (`None`
  for `NoOp`), borrowed when the caller pre-resolved it.
- Twenty-six names join seven modules' `__all__`, fifteen of them in
  `strider.pattern`, among them `ElfLifter`, `load_elf`, `PatLike`, `ValueTy`,
  `DotStyle` and `OptimizerPass`; `get_type_hints` no longer raises `NameError`
  on a published pattern protocol method.
- The explorer pans by mouse drag and by the arrow keys, and zooms about the
  pointer with ctrl+wheel and about the window centre with `+` / `-`, with `f`
  to fit the graph to the window
  and `0` for 100%. A drag ending over a node pans rather than re-centering.
- A sum of scaled copies of one value folds to a single multiply for every
  mul/shift pairing, not just two multiplies: `x*3 + (x << 2)`, `(x << 2) +
  (x << 3)`, `x*3 - x` and the rest all canonicalise to `x*C`. A shift by C is
  a multiply by 2^C, so a compiler emits these interchangeably, and a pattern
  now has one shape to match instead of thirteen.

- `visualize` opens on the entire graph; `whole=False` opens on a neighborhood
  instead, and the toolbar toggle switches between them either way.
- `visualize(background=True)` serves on its own thread and returns straight
  away, so the calling thread keeps querying while the page is open; stop it
  with `strider.explore.shutdown(port)`. `visualize` now returns the bound port
  either way, where it used to return `None`.
- The explorer's neighborhood knobs open uncapped: `0` means no limit on depth,
  hub cap and max nodes, and their ceilings are raised.
- `ElfLifter.add_symbol_file(path)` attaches a debug or symbol companion's
  symbols without its bytes; `add_elf` refuses such a file, since it is linked
  at the same addresses as the image it describes.
- `ElfLifter.add_symbols({name: addr | (addr, size)}, is_function=)` adds
  symbols that live in no ELF, such as a `System.map`. A size is what lets
  `symbol_at` resolve an address inside the symbol.
- `Lifter.arch` and `Cfg.lifter`, together enough to build a second handle over
  the same memory.
- `lifter=` on `Function.to_dot` / `neighborhood_dot` and `Cfg.to_dot` /
  `neighborhood_dot`, to render through a handle other than the one that built
  the graph. A handle for a different arch is rejected.
- A mapped file must not change on disk while it is loaded.
- `AssumptionOptions.none()` clears all six claims in one call, the only
  configuration sound under any input. `AssumptionOptions()` is not that: two
  of the six default `True`.

- `Cfg.flowing_isa_bit_at_site` reports the ISA-mode bit flowing into each
  indirect-branch site, sampled by the builder at the moment it decides it.
- `strider_ir::node::low_bits_mask_u128` gives the low-*n*-bits mask one owner;
  four crates restated it with four different `>= 128` guards.
- `IntBinaryOpName`, `BoolBinaryOpName` and `FloatBinaryOpName` join the
  existing `Literal` aliases, so `bool_binary("Add")` no longer type-checks
  clean and raises at runtime.
- `int_const` accepts a value in `[2^127, 2^128)`, which the equivalent raw-int
  operand already did.
- `strider-cfg`, `strider-lift` and `strider-orchestrator` ship READMEs, as
  `strider-py`, `strider-reader` and `strider-target` already did.
- The seventeen example scripts run in CI. They were type-checked and never
  executed, so a script that imported cleanly and then raised passed the gate.

### Performance

- Reading `CfgOptions.known_targets` or `.call_other_abis` no longer copies the
  table. Both are cached `mappingproxy` views over one `Arc`-shared map, where a
  `#[pyo3(get)]` deep-copied the whole table on every read and `_api` read them
  on every call: `analyze` with 20,000 known targets cost 32.0 ms against
  1.6 ms, plus 6.8 ms to seat the seeds. The views are read-only -- rebinding
  the attribute raises `AttributeError`, item assignment `TypeError` -- where
  the copy silently absorbed a write.
- Dead-branch elimination memoizes what escapes as well as what does not. It
  kept only the walk's false verdicts, and the positive memo it did keep proves
  escape only along routes avoiding every constant branch's dead arm, so a
  shape where each gate's live arm reaches `Return` only by crossing another
  gate's dead arm paid a whole-CFG walk per gate: 2n+1 walks at n = 8, 16, 32
  and 64. The walk now attributes a verdict to every node it covers and drops
  only the memos whose route crossed a branch that has since folded. One walk
  at every n.
- Applying relocations reuses one scratch buffer instead of allocating per
  relocation site.

- A partial read no longer walks every region with a lower start. One region
  spanning the image held the prefix maximum above every interior address, so
  the walk never terminated early: 2.8 ms for a single read at 262,144 regions.
  Both halves of the lookup are index descents now.
- `UnionDag::for_each` cost scaled with the largest id in the arena rather than
  with the answer, so sweeping every key was quadratic and reading
  `asm_fingerprint` over a large function paid for it. Per-call cost is flat in
  arena size. Its repeat guard also caught only consecutive repeats, so an
  alternating pattern grew the link list without bound.
- `dedup_overlapping_largest` was quadratic on byte-identical varnodes, since
  exact duplicates never subsume one another. Two scalars replace the open list.
- `Sleigh::set_context_at` scanned its replay log on every call, which a sweep
  over many pinned addresses made quadratic, and the log grew without bound. It
  appends, and stops recording past a cap; `Sleigh::clone_would_replay_context`
  reports when a clone would start from the pspec defaults instead.
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
- A read one region fully covers, which is every instruction fetch, resolves
  through a max-end segment tree over the region starts in O(log n) instead of
  a downward walk over candidates. The walk's early-out is a prefix maximum, so
  one region spanning the image held it above every interior address and it
  never stopped early. Region count is attacker-chosen, sections being one
  region each and `SHN_XINDEX` lifting the 65535 header cap. Over 1,000 to
  64,000 eight-byte regions nested under one spanning region: 0.048us to
  0.093us per read, where the walk was 1.40us to 142us.

- `Function.to_dot(pretty=True)` resolves the Sleigh register table once per
  render instead of once per varnode name. rsleigh documents that call as
  expensive; measured at 331us against the 1440 registers x86-64 declares, and
  it was reached once per `Return` input slot and once per `Call` clobber edge.
- `pcode_at` caches one sweep engine on the handle rather than cloning the
  whole `Sleigh` per call. Decoding a single instruction cost 30.6ms against
  0.8ms to decode the entire function; the next five calls now cost nothing,
  and the linear sweep no longer makes repeated calls quadratic.
- A guard lookup walks the query point's dominator chain instead of scanning
  every guard recorded for that value. Guards on one value cost the square of
  their count in entries scanned and grew cubically -- 4096 entries and 9.4ms
  at 64 guards, against 3 probes now.
- Stamping the same lift address on a node the dedup cache returned adds one
  leaf rather than one per call, so an asm fingerprint grows with distinct
  addresses instead of with node creations.

### Fixed

- A `Function` or `Cfg` that crossed to a worker thread leaked the `Lifter` it
  transitively owned, 21 MB a drop, with an unraisable error to stderr: PyO3
  refused to run an `unsendable` destructor off-thread and leaked the value
  instead. Rendering off-thread was worse, raising `PanicException`, a
  `BaseException` that `except Exception` never catches, so the thread died
  silently. The handle holds its lifter in a `ThreadPinned`, which pins USE to
  the creating thread and leaves the value free to move and to be dropped
  anywhere, so an off-thread decode is a catchable `StriderError` and an
  off-thread drop is just a drop.

- A register-space `STORE` whose address names no register gave two different
  registers the same SSA value. The clobbering re-read built each register's
  slot address at the REGISTER's own width instead of the space's, so
  `build_int_const` masked it: on PowerPC every 1-byte register at an offset of
  0x100 or more collapsed onto `addr_off & 0xff`, and `cr0` and `xer_so`, `cr1`
  and `xer_ov`, `cr3` and `xer_ca` each came back as one value. Reachable from
  any register-derived `mtsrin`. 256 colliding groups on PowerPC, 72 on x86-64,
  8 on AArch64; ARM and MIPS have none, which is why the ARM `vld1.N` shapes the
  path exists for never showed it.

- Applying relocations allocated one patch record per covering region per site,
  so an image with many overlapping `PT_LOAD`s aborted the process: 8192
  segments over one extent and 41,000 relocations is a 1.5 MB file, and it
  exhausted 8 GB and died with `SIGABRT`, uncatchable from Python. Enumeration
  now descends the region index per region yielded rather than walking every
  lower-start entry, and a patch budget refuses an image past 4x amplification
  instead of truncating it. 1170 real images relocate at amplification 1.

- A relocation site is still patched in every region that fully covers it, so a
  read wide enough to fall through to an outer region gets the same bytes.

- `OwnedElf::regions` and `ElfFileMemReader::from_elf_maybe_relocated` panicked
  when the mapped file had been rewritten to the same size within one second:
  the freshness check passed and the unguarded re-parse hit its `expect`. Both
  go through the checked parse, and the unguarded accessor is gone.

- A `Not` over `phi_input_from_edge` admitted the rows it had to drop. The
  constraint answered `false` where it could not answer at all, so negating it
  turned "unanswerable" into "true" for a phi capture bound to a non-phi, or an
  edge capture bound to a non-control value. Every other join constraint was
  already three-valued.

- Stacking a second `.capture()` on one vertex silently dropped the first, and
  the lost capture did not appear in the pattern's declared captures, so
  `var(x).capture(y)` left `x` bound to nothing. Both bind now.

- A `Match` used after `Function.compact` indexed a compacted arena: an
  out-of-range id panicked and an in-range one read a different node. A `Match`
  carries the graph generation it was made at and reports stale instead.

- `IfPat::with_true` / `with_false` panicked on a multi-sink branch pattern,
  where every other path returns an error. Building a guard on a control-rooted
  pattern, and `.ordered()` on an alternation, both silently matched nothing;
  all three are refused when the pattern is built.

- A MIPS o32 `double` return carried only its low half. The convention named the
  4-byte `f0`, so the `Return` sliced I32 out of the tracked 8-byte `f0_1` pair
  every double-format instruction writes. It names the pair registers now; n64,
  whose FPRs are already 8 bytes, is unchanged.

- ARM32 `smc` and `hvc` reported no register write, so a read of `r0` after a
  secure-monitor call resolved to the value that flowed in. They carry the
  SMCCC footprint, as the AArch64 rows already did.

- `Function.rewrite` with a float-typed root and an integer literal raised
  `PanicException`, which `except Exception` does not catch. The rewrite is
  declined.

- The background explorer could be held for five minutes by one client that
  stopped reading, and `shutdown()` then reported it had stopped nothing while
  the server kept running and the interpreter hung at exit. The serving loop
  marks itself started on entry, `shutdown` signals every target before waiting
  on any, and a body is sent in slices that re-check for shutdown. A client that
  drains slowly still receives the whole body.

- Dropping a pattern built by a loop killed the interpreter with `SIGSEGV` past
  about 41,000 links, with no catchable exception, because the drop recursed
  through the whole chain. Wrapper drops queue their operands instead. Five
  builder shapes reached it, not only the free constructors.

- `Cfg.neighborhood_dot` accepted an out-of-range region id and rendered an
  empty graph, and took `u32::MAX` as a real id. It raises, as the IR renderer
  beside it already did.

- `Function.neighborhood_dot` ignored a supplied `lifter` unless `pretty=True`,
  so the architecture and thread checks it carries never ran.

- ppc64 ELFv1 symbols resolve through their `.opd` descriptors, so `analyze` by
  name works on an ELFv1 image rather than decoding the descriptor as code. The
  entry point resolves the same way. A followed symbol reports no size, since
  `st_size` measures the 24-byte descriptor.

- `LoadForward` narrowed a load's memory edge using the relaxed analyzer, so a
  function optimised once with `escape_analysis` kept edges only that assumption
  justified, and re-optimising with `AssumptionOptions.none()` inherited them.
  The relaxation decides whether to forward; the rewire uses the structural
  answer, as `FunctionArgDetect` already did. The structural answer is now the
  whole of it: the narrowing analyzer had kept `stack_global_disjoint`, whose
  one reader makes a stack address and a global disjoint, and
  `noalias_allocators`, which steps the walk past a listed allocator `Call` on
  frame privacy alone, so the IR handed back still asserted a disjointness
  `AssumptionOptions.none()` refuses and `FunctionArgDetect` then read the
  shortened chain as clean. It runs with every claim off, and discards a heap
  address class when the allocator set is empty, since the decompose memo it
  shares with the read-only analyzer would otherwise answer disjoint for two
  distinct heap bases.

- A call's collected stack arguments reported a wide store's value for a slot a
  nearer store had partly overwritten, and dropped the overwriting store. The
  slot ends the prefix instead.

- An ARM `VLD2/3/4` or `VST2/3/4` multiple-structures instruction no longer
  fails the whole function. The register the p-code addresses through the
  REGISTER space is picked by a loop-carried pointer, so it does not fold to a
  constant, and refusing to lift it cost every de-interleaving NEON function
  v0.1.0 had handled, 24 forms across every element size. The access is now
  opaque: it stays in the REGISTER space, so a later read of the same slot sees
  the write, and every tracked register is re-read afterwards, so none keeps a
  value the write may have replaced.
- A resolved REGISTER-space address is gated on a declared register enclosing
  it. ARM's `VLD4`/`VST4` single-lane forms omit the element-size scale their
  siblings apply, so the address could straddle two registers; seeding that
  slot broke aliasing silently, and a `vld4.32` lane write followed by
  `vmov r0,s18` returned s18's entry value.
- `int_const_any_width(v)` no longer matches a constant that merely shares
  `v`'s low bits. A hunt for `0x1234` matched a stored `0x34`, and `0` answered
  a search for `256`.
- An indirect site that has proved an ISA mode now reports itself when a later
  round seats an arm on that mode without evaluating it. The arm is still
  seated, since dropping it costs every same-mode dispatch its widening, but
  `is_complete()` no longer calls the answer settled.
- An object file's relocations resolve against `.symtab`, the table its
  `sh_link` names. `object` reports the first `SHT_DYNSYM` section as the
  dynamic table whatever the `e_type`, so an `ET_REL` carrying one patched in
  an unrelated symbol's `st_value`, silently.
- A `CallOther` whose p-code output is a memory operand (x86 `sgdt [mem]`) no
  longer fails the function; `save_processor_state` was unliftable on every
  x86-64 kernel.
- A `call_other_abis` override outranks the built-in trap rule, in the CFG and
  in the lifter alike.
- The allocator stack relaxation is gated on frame privacy.
- An alignment mask's 1-run must reach the top of the width it is applied at.
  `is_alignment_mask` accepted any contiguous 1-run over a low zero run without
  reference to the address width, so `sp & 0x10` (which is 0 or 16) and a
  32-bit truncation mask both anchored a stack base. Under
  `stack_global_disjoint`, which defaults ON, such a base was then called
  disjoint from a constant address it may equal. Inert under
  `AssumptionOptions::none()`.
- The seeded-arm cross-check runs for every dispatch shape. `seated_arm_losses`
  looked `known_targets` up by exact p-code key while `CfgOptions::seated`,
  which did the seating, falls back to the machine-start key, so it never ran
  for a `BRANCHIND` that is not its instruction's first p-code op (ARM `bx`,
  MIPS `jr`, x86 `jmp [mem]`), nor for round one of any seeded site.
- The explorer's read deadline no longer truncates a slow client's response
  body, and `explore.shutdown` returns only the ports it actually stopped.
- A connection that sends nothing no longer wedges the explorer, and then the
  interpreter, at exit.
- The explorer refuses a request whose `Host` header does not name loopback.
  `do_GET` dispatched on the path alone, so a page that resolved its own
  hostname to 127.0.0.1 could read `/dot`, the whole rendered graph of the
  binary under analysis. No browser lets a page forge `Host`.
- A client trickling one byte at a time no longer holds the explorer's
  single-threaded loop. Its `timeout` was a per-`recv` deadline, which every
  byte renewed; `rfile` now runs under a whole-request budget, re-armed with
  what is LEFT before each read. This is not the connection that sends nothing:
  a drip refused twelve consecutive normal requests with no recovery, and the
  untimed `shutdown` then ran before the non-daemon join, so the interpreter
  did not exit either.
- `load_elf` no longer aborts on a guarded parse, nor hangs on a FIFO.

- Three inputs took the whole process down from a plain `build_cfg`, with no
  options set, and now raise a catchable `StriderError`. A malformed
  instruction in a MIPS branch delay slot wrote tens of kilobytes past a
  destroyed stack frame, because the handler that renders the error message
  disassembled through a walker the unwind had already invalidated. An AArch64
  operand the parse allocated but never built answered with a null address
  space that the p-code builder dereferenced. And a region seated at the top of
  the address space handed `BTreeMap::range` two equal excluded bounds.
- A PowerPC `tw` / `twi` / `td` / `tdi` seals its region as no-return whenever
  its TO mask covers either trichotomy, signed `LT|GT|EQ` or unsigned
  `LTU|GTU|EQ`, which is seven of the 32 masks. Only all-bits-set counted
  before, so the other six left dead fall-through code reachable. Correctness
  only: across 21 PowerPC kernels, 321,048 trap instructions in 125,166,009
  words, none of the six new masks occurs.
- `OptimizerPipeline` is no longer thread-pinned. Touching one from another
  thread raised `pyo3_runtime.PanicException`, which derives from
  `BaseException` and so escapes `except Exception`.
- `analyze` releases the GIL on the custom-pipeline path too. It used to hold
  it for the whole analysis, stalling every other Python thread.
- `Cfg.unverified_seeded_sites()` on a `build_cfg` result holds every site you
  seeded; its docstring, its `.pyi` stub and both copies of `is_complete`'s
  all claimed it was always empty there.
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
- A p-code `LOAD` / `STORE` addressing the REGISTER space lifted as opaque
  memory, so the register it names was never read or written and no phi was
  placed for it: the value simply vanished, with `is_complete()` still true and
  nothing reported. A sla addresses a register this way when an instruction
  field picks it rather than naming it outright, which is how ARM's
  `vld1.N {dX[i]}, [addr]` writes one lane: `str r0,[sp]; vld1.32 {d0[0]},[sp];
  vmov.32 r0,d0[0]` returned 0 instead of `r0`. The address is built from
  constants the decoder substituted, so it folds to the register it names, and
  the lift now writes that register. It fails the function when the address does
  NOT fold, rather than naming the wrong register or falling back to memory.
  Def-site collection resolves it through the same code over the same op
  sequence, so a register written is a register a phi was placed for, and the
  register joins the tracked varnode set, which is otherwise built from pcode
  operands alone and so cannot see a computed address. The multi-register form
  (`vst1.32 {d0-d1}, [r0]`), whose sla body loops with the register offset
  advancing per iteration, has no single answer and fails: 14 functions in
  4,918,502, each of which previously lifted a wrong memory store silently.
- PowerPC `CR2` / `CR3` / `CR4`, the non-volatile condition fields, were absent
  from every PowerPC convention's callee-saved set, so a `Call` clobbered them
  and a compare held across a call read as opaque afterwards, a silently wrong
  branch reported through no channel. `CR0` / `CR1` and `CR5`-`CR7` really are
  volatile and stay out. Named individually, which is how `mfcr` / `mtcrf` name
  them, never as the 8-byte `crall` container that spans the volatile fields
  too. PowerPC libc alone carries 878 `mfcr`, 712 `mtcrf` and 1,541 branches
  reading `cr2`-`cr4`.
- PowerPC `r2` and `r13` are callee-saved on every PowerPC convention, and the
  PPC64 stack-argument base is 112 bytes on ELFv1 and 96 on ELFv2, the linkage
  area plus the 64-byte parameter save area, not the linkage area alone. Every
  PPC64 stack argument was read at the wrong offset.
- `software_udf` does not return; it was classified pure, so the lift walked
  past an ARM permanently-undefined instruction into whatever followed.
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
- Sub-register reads and writes on `arm_be_kernel` used the image's data
  endianness rather than the register file's, so every sub-register access took
  the wrong half. BE8 is byte-swapped instructions over a little-endian register
  block.
- A write into a tracked container wider than 16 bytes was refused because the
  mask had no `u128` to live in, making any function mixing 256-bit and 128-bit
  VEX forms unanalysable. Masks are built limb-wise at the container's width.
- One client sending a byte at a time, never finishing its request line, held
  the explorer's single-threaded loop indefinitely: `shutdown()` did not return
  and the interpreter could not exit, so a REPL in that state needed `SIGKILL`.
  The read deadline armed once per `readline`, and the buffered reader loops
  `recv` inside one of those, so each read renewed the whole budget. It arms
  per `recv` now.
- A callee owns the whole argument slot, not the bytes the caller wrote into
  it. The outgoing-argument window recorded each range as the store's extent
  while advancing by whole ABI slots, so the tail of a sub-slot argument -- a
  4-byte seventh integer argument on x86-64 SysV -- fell outside the window and
  a load of those bytes forwarded across a call that may write them. Reachable
  only under `escape_analysis` or a non-empty `noalias_allocators`.
- An ISA-mode clash costs the arm that names the clashing address, not the
  whole jump table. Every producer of a mode clash is direct flow, so a table
  lost all its arms because two unrelated direct branches disagreed about one
  target.
- The flowing ISA bit is sampled where it is decided. The builder read it at a
  branch's seal and the orchestrator read it again after the build, and a
  region explored later can paint its own mode across the branch's address in
  between, inverting the interworking test.
- Abandoning a site removes both keys that can name it. A fold is keyed by the
  anchor's p-code address and a caller seed by machine address, so a site the
  loop had given up on kept being re-seated.
- Deep pattern nesting is bounded by measured stack as well as by level count.
  The count is calibrated for the main thread's 8 MiB; an unoptimised build on
  a 2 MiB thread stack died at 255 nested operands, inside the documented 512
  levels. A release build was never affected.
- An `__index__` that re-enters the pattern builder raises instead of
  recursing without bound. The depth guard fired, and the integer extraction
  swallowed its error before retrying at another width -- two recursions per
  level.
- A failed multi-pattern query no longer consumes the one-shot patterns it had
  already taken, and a failed `add_symbols` no longer commits the entries it
  had processed, where they surfaced after some later mutation and a corrected
  retry duplicated them.
- `PhiCollapse` reports the node it killed. It returned "no change" when the
  phi had no uses, so the fixed-point loop could exit having just removed a
  live-set member.
- `sp & 0x8000_0000` is not an alignment mask, so it no longer roots a stack
  base; a value range at a width past the `u128` carrier stays top rather than
  being narrowed by a later scaling; and the pipeline clears the frame-escape
  bit beside the decomposition memo.
- Reading a relocation's kind before charging the patch budget stops a table of
  GOT relocations inflating the amplification allowance, and a mapped-file
  freshness check dedups by set rather than by linear scan.
- Replacing an `IfPat` branch drops the discarded branch's captures, which
  otherwise stayed in the declared set and failed a rewrite at instantiation.
- Six example scripts reported binding rows as sites. Commutative matching
  answers once per operand order, so every count whose noun was "sites" or
  "pairs" was doubled, and one demo printed a number that disproved its own
  comment.

- A commutative arity-2 node with only ONE pinned operand tried a single
  ordering, so pinning slot 0 and pinning slot 1 answered differently for the
  same query. Both orderings are tried whenever the node commutes. The swapped
  one is skipped only where it provably yields the same bindings: the two
  operand vertices are the same vertex, or structurally equal with no capture,
  identity pin or predicate anywhere in either cone.

- A `.when()` predicate nested into another pattern fell out of the GC graph
  while the closures still owned it, so a reference cycle through one built
  that way was never collected. A cached pattern replays its predicate handles
  into the open scope.

- A query that raises before doing any work -- a bad `constraints` argument, a
  `.when()` on a rewrite LHS -- restores its one-shot pattern instead of
  burning it, so the next `find_all` no longer reports a query that never ran.

- `Cfg.fingerprint_pcode` rejects a `Node` whose function another `Cfg` lifted,
  which it used to answer for with this binary's p-code. A fingerprint is
  machine addresses, which collide across images; every neighbouring accessor
  already refused it.

- `explore.shutdown()` called from a worker no longer joins the caller's own
  still-running thread, which cost the whole five-second join deadline.

- An ET_REL `SHT_NOBITS` section whose base plus `sh_size` passes 2^64 left the
  watermark alone, so later sections stayed seatable while it had already
  claimed its aligned base: the next allocatable section got that same base and
  its symbols resolved into the other section's bytes. A crafted object put
  `.text` and a `.bss` symbol on one address, where reading the `.bss` symbol
  returned `.text` opcodes as foldable ROM. The overflow branch advances the
  watermark.

- Equal-start `PT_LOAD`s dedup widest-first, so the region table and the
  relocation walk no longer disagree. The section walk deduped by base and the
  segment walk did not, so the table kept the last and the walk patched the
  first that covered: two loads at one vaddr lost 28 of 32 fetchable bytes.

- A relocation against an out-of-range `st_shndx` is skipped, as
  `resolve_symbol_target`'s doc already said, rather than patching the bare
  `st_value` as an address.

- A folded shift follows the p-code rule: a count at or past `8 * sizeout`
  gives 0. `IntLeft` clamped at 127 instead, which the trailing mask covered
  below 128 but not at or above it with a 16-byte output, and the wrong folded
  constant names the wrong register in `register_store_target`. `IntRight` was
  wrong independently of the count: it masked the input to the output width
  AFTER shifting rather than before.

- MIPS16e `ext_delay` is a forward-painted context var, so a value committed by
  one MIPS16 `jal` no longer changes the constant address the next cold entry
  computes. `mips.sinc` declares it `noflow`, `mips16.sinc` paints it at five
  `globalset(inst_next, ..)` sites, and one of those reads it into PC-relative
  address arithmetic.

- `CallingConvention::validate`'s disjointness rules compare byte ranges, so a
  callee-saved `d8` against an argument `q8` is rejected. Exact varnode
  equality let that pass while the doc said disjoint and the runtime check was
  already byte-accurate. The membership and duplicate rules stay exact: they
  state identity, not disjointness.

- A seed spelled at the machine start keeps its ISA mode. `apply_resolutions`
  read `known_targets` by exact p-code key, where every other seed-aware read
  goes through the machine-start fallback that exists because a caller can only
  spell the machine address. At any dispatch whose `BRANCHIND` is not the
  instruction's first p-code op -- ARM `bx`, MIPS `jr`, x86 `jmp [mem]` -- the
  lookup missed, so the arm was seated mode-less and decoded in the flowing
  mode, which at an interworking `bx` is the mode being switched away from.
  Neither mode report fired, so `is_complete` answered true and the next round
  stayed silent.

- A single out-of-range or interior arm no longer costs the whole seeded table.
  Bad arms drop individually and land on the channel that reports them -- an
  interior arm on `interior_branch_targets`, a short seat on
  `unresolved_indirect_branches` -- where before one tail-calling switch case
  re-deferred the dispatch every round and the interior case recorded nothing.

- An undecodable seeded target freezes only the site that named it, rather than
  every site naming that address: `undecodable_seeded_targets` carries the
  site.

- Overlapping code is reported rather than assumed away. The fall-through check
  is an exact-key lookup, so a decode could step over a region start interior to
  an instruction it decodes and leave two regions owning one address with
  different instruction streams, on none of the five channels a caller reads.
  Region starts stepped over that way are reported as `interior_branch_targets`.
  No measurable cost: 1.30 s against 1.38 s over 20 builds and ~60k
  instructions.

- A backslash in a symbol name is escaped rather than read as a line break.
  `escape_dot_label` passed `\n`, `\l` and `\r` through un-doubled, unable to
  tell a caller's escape from one arriving inside the symbol names and lifted
  disassembly it documents as its content: through real Graphviz the label
  `C:\lib\name` rendered as three lines, losing characters silently. The one
  caller that hand-emits `\l` per instruction takes `node_raw_label`.

- `validate` rejects a non-phi node that is its own transitive data producer.
  The walk terminates on it, so nothing noticed, and reverse post-order then
  yields the node before its own producer. `dominance_frontiers` asserts the
  root precondition its sibling already asserted: both need `idom(root) ==
  None` for the climb to pass through the root, and a tree encoding
  root-as-own-idom silently loses `DF(root)` containing root, which
  `phi_placement` then reports no error for.

### Fixed - decoder and sla

The vendored Sleigh engine and specs under `externals/rsleigh`, one line per
mnemonic group. A caller sees these only in the lift of the named instructions.

- AArch64 `usdot` / `sudot` / `bfdot` **by element**: `usdot` and `bfdot` lifted
  with whichever register the previous instruction left in the lane operand's
  slot, and read the wrong lane; `sudot` did not decode at all. Both
  endiannesses, 170 newly decoding instructions.
- AArch64 `FRINT{A,I,M,N,P,X,Z}` lifted as a float-to-INTEGER conversion, so
  `ceil(2.5)` answered the integer 2 where the hardware gives 3.0. p-code
  expresses three of the seven rounding modes, so the family lifts opaquely
  through a pure `NEON_frint` user-op: unknown rather than wrong.
- AArch64 `frint32x` / `frint32z` / `frint64x` / `frint64z` did not zero the
  destination's upper lanes, in all twenty constructors.
- AArch64 `addv` into a byte destination did not zero the rest of the vector
  register, so `__builtin_popcount` read the surviving `cnt` lanes back.
- The big-endian ARM VFP register file did not overlay: the 128-byte `s` block
  was based with the 256-byte `d` block, so `s0` landed inside `d16` and a write
  through `s0` was invisible to a read of `d0`. `ARM7_be` and `ARM8_be`.
- ARM `vcmp` left FPSCR's C clear when unordered, so a float `<=` was true for
  NaN.
- Thumb `sev` / `sev.w` emitted nothing where the A32 form raises `SendEvent`,
  so a wait/signal pair read as a no-op on one encoding and not the other.
- A32 `csdb` fell through to an `msr` with an empty field mask, making `cpsr` a
  tracked varnode and minting three temporaries per site.
- MIPS64 `clz` / `clo` counted the whole 64-bit register rather than the word.
- MIPS64 `drotrv` rotated by `32 - shift` on a 64-bit value, degrading to a
  plain logical shift right for counts above 32.
- x86 `PSLLD` / `PSLLQ` shifted each vector lane by its own count instead of the
  one count the ISA reads from `SRC[63:0]`, and `PSRAD` took its count from the
  whole 128-bit operand, so a nonzero upper half saturated every lane.
- x86-64 `rdtsc` / `rdpmc` / `xgetbv` kept the caller's upper 32 bits in RAX and
  RDX and carried a data dependency the machine does not have; in 64-bit mode
  these clear the high half. `rdtscp` stays opaque: its constructor writes no
  register, so its whole result comes from the CallOther ABI table.
- Nine x86-64 constructors named their ModRM.rm operand with the raw register
  field, which ignores `REX.B`, so `rdfsbase r9d` read and wrote `ECX`:
  `rdfsbase`, `rdgsbase`, `rdpid`, `wrfsbase`, `wrgsbase`, `incsspd` /
  `incsspq`, `rdsspd` / `rdsspq`.
- The x86-64 32-bit destinations still missing their zero-extension have it:
  `RDPKRU`, `LOOP` / `LOOPcc` / `JECXZ` under an address-size override, the
  `fsgsbase` and `rdpid` reads, `RDRAND`, `RDSEED`, `CMPXCHG8B`, and 30 VEX and
  EVEX constructors including `VCVTS{S,D,H}2{,U}SI`, `VMOVD`, `VMOVMSKP{S,D}`
  and the `KMOV` family.
- `allocateOperand` checked none of the fixed sizes `ParserContext::initialize`
  hands out, so an instruction whose parse descended further wrote out of
  bounds. Thumb-2 `0xEC8x`..`0xECFx` segfaults a fresh engine on one `lift_one`.
- `ParserWalker::setOutOfBandState` checked none of the bounds every other
  walked operand checks. A memory reader that overstates its read is an error
  rather than a panic across the FFI boundary.
- The parse cache was not invalidated on an out-of-band context write, nor
  flushed when a context variable was re-pinned, so an address could decode
  against a stale constructor.
- `BufMemReader::end_off` summed base and buffer length in `u64`, so a region at
  the top of the space overflowed inside the read callback, where a panic cannot
  unwind: it aborted the process. The arithmetic is u128. The pin also moves
  back onto rsleigh's `master`, off a feature branch where a force-push would
  make every commit in this release unbuildable.

## 0.1.0

First release.
